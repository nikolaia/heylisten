//! Voice activity detection: where someone is speaking, judged by how it sounds rather than how
//! loud it is (Silero VAD v5, run by sherpa-onnx). Quiet speech on a low-gain mic still counts;
//! music, typing and fans mostly don't, so whisper isn't handed noise to invent words from.

use std::ffi::CString;
use std::ops::Range;

use anyhow::{Context, Result, bail};
use sherpa_rs::sherpa_rs_sys as sys;

use crate::audio::SAMPLE_RATE;

const MODEL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/vad.onnx"));

/// How sure Silero must be that a window is speech. Below its usual 0.5, for compressed call
/// audio and far-off mics; measured on real meetings, 0.35 kept more real speech.
const THRESHOLD: f32 = 0.35;
/// Silero's window at 16 kHz: 32 ms.
const WINDOW: usize = 512;
/// A pause this long ends a stretch of speech: a pause in conversation, not a breath.
const MIN_SILENCE: f32 = 0.4;
/// Shorter blips (a click, a cough) aren't speech.
const MIN_SPEECH: f32 = 0.25;
/// Past this, the detector looks harder for a pause to end the speech at. It's a soft limit:
/// the chunker cuts what's still too long.
const MAX_SPEECH: f32 = 10.0;

/// A streaming detector for one track. Feed it audio in order; it reports each stretch of speech
/// once it has ended (or run long, see MAX_SPEECH).
pub struct Vad {
    vad: *const sys::SherpaOnnxVoiceActivityDetector,
    /// Audio not yet given to the model: less than a window.
    pending: Vec<f32>,
}

// The detector is only ever used from one thread at a time.
unsafe impl Send for Vad {}

impl Vad {
    pub fn new() -> Result<Vad> {
        let path = crate::diarize::model_file("vad.onnx", MODEL)?;
        let model = CString::new(path.to_string_lossy().as_bytes()).context("bad model path")?;
        let provider = CString::new("cpu")?;
        let config = sys::SherpaOnnxVadModelConfig {
            silero_vad: sys::SherpaOnnxSileroVadModelConfig {
                model: model.as_ptr(),
                threshold: THRESHOLD,
                min_silence_duration: MIN_SILENCE,
                min_speech_duration: MIN_SPEECH,
                window_size: WINDOW as i32,
                max_speech_duration: MAX_SPEECH,
            },
            sample_rate: SAMPLE_RATE as i32,
            num_threads: 1,
            provider: provider.as_ptr(),
            debug: 0,
            // Unused: an empty TEN VAD config.
            ten_vad: unsafe { std::mem::zeroed() },
        };
        // The buffer holds the speech still in progress, so it needs more than MAX_SPEECH.
        let vad = unsafe { sys::SherpaOnnxCreateVoiceActivityDetector(&config, MAX_SPEECH * 3.0) };
        if vad.is_null() {
            bail!("couldn't load the voice activity model");
        }
        Ok(Vad { vad, pending: Vec::new() })
    }

    /// Feeds the audio that follows on from the last call. Returns the stretches of speech that
    /// have ended, as sample ranges counted from the first sample ever fed.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Range<u64>> {
        self.pending.extend_from_slice(samples);
        // One window per call, collecting as it goes: given more at once, sherpa-onnx loses
        // the speech it finds (measured: a 7.7 s file in one call reported none of its 3.8 s).
        let mut found = Vec::new();
        let whole = self.pending.len() / WINDOW * WINDOW;
        for window in self.pending[..whole].chunks(WINDOW) {
            unsafe { sys::SherpaOnnxVoiceActivityDetectorAcceptWaveform(self.vad, window.as_ptr(), WINDOW as i32) };
            found.extend(pop(self.vad));
        }
        self.pending.drain(..whole);
        found
    }

    /// Whether speech has started that hasn't been reported yet.
    pub fn in_speech(&self) -> bool {
        unsafe { sys::SherpaOnnxVoiceActivityDetectorDetected(self.vad) != 0 }
    }

    /// The end of the audio: reports the speech still in progress.
    pub fn finish(&mut self) -> Vec<Range<u64>> {
        // A pause after the end lets the detector close the speech itself: sherpa-onnx's Flush
        // miscounts speech that runs right up to the end ("circular-buffer … Invalid n").
        let pause = (MIN_SILENCE * SAMPLE_RATE as f32) as usize + 2 * WINDOW;
        let mut ended = self.push(&vec![0.0; pause]);
        unsafe { sys::SherpaOnnxVoiceActivityDetectorFlush(self.vad) };
        ended.extend(pop(self.vad));
        ended
    }

}

/// Takes the finished stretches of speech out of the detector.
fn pop(vad: *const sys::SherpaOnnxVoiceActivityDetector) -> Vec<Range<u64>> {
    let mut found = Vec::new();
    unsafe {
        while sys::SherpaOnnxVoiceActivityDetectorEmpty(vad) == 0 {
            let segment = sys::SherpaOnnxVoiceActivityDetectorFront(vad);
            if !segment.is_null() {
                let (start, n) = ((*segment).start.max(0) as u64, (*segment).n.max(0) as u64);
                found.push(start..start + n);
                sys::SherpaOnnxDestroySpeechSegment(segment);
            }
            sys::SherpaOnnxVoiceActivityDetectorPop(vad);
        }
    }
    found
}

impl Drop for Vad {
    fn drop(&mut self) {
        unsafe { sys::SherpaOnnxDestroyVoiceActivityDetector(self.vad) };
    }
}

/// Where the speech is in a whole recording, as sample ranges.
pub fn speech(samples: &[f32]) -> Result<Vec<Range<usize>>> {
    let mut vad = Vad::new()?;
    let mut ranges = vad.push(samples);
    ranges.extend(vad.finish());
    Ok(ranges.into_iter().map(|r| (r.start as usize).min(samples.len())..(r.end as usize).min(samples.len())).collect())
}

/// Seconds of speech in a whole recording.
pub fn speech_seconds(samples: &[f32]) -> Result<f32> {
    Ok(speech(samples)?.iter().map(|r| r.len() as f32).sum::<f32>() / SAMPLE_RATE as f32)
}
