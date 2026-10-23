//! The live transcript: while recording, the voice detector cuts each track into chunks of speech,
//! and one transcriber thread appends their segments to `live.jsonl` in the meeting folder.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use anyhow::Result;

use crate::audio::{self, SAMPLE_RATE};
use crate::meeting::Track;
use crate::transcribe::Transcriber;
use crate::transcript::Segment;
use crate::vad::Vad;

const SECOND: u64 = SAMPLE_RATE as u64;
/// Audio around each stretch of speech, so its first and last sounds aren't clipped.
const PAD: u64 = SECOND / 4;
/// Stretches of speech closer than this are joined into one chunk, up to JOIN_MAX: whisper
/// does better with a few sentences than with the pieces between pauses, but skips speech in
/// long chunks. These two gave the most words on real meetings.
const JOIN_GAP: u64 = SECOND * 6 / 10;
const JOIN_MAX: u64 = SECOND * 5;
/// Longer speech is cut, at its quietest moment in the last few seconds (usually a breath).
/// NB-Whisper skips whole sentences in longer chunks.
const MAX_CHUNK: u64 = SECOND * 12;
const CUT_WINDOW: u64 = SECOND * 4;
/// Recent audio kept for cutting chunks from: more than the longest stretch of speech the
/// detector reports, plus the pause it waits for before reporting it.
const KEEP: usize = SECOND as usize * 60;

pub struct Chunk {
    pub track: Track,
    pub start_sample: u64,
    pub samples: Vec<f32>,
}

/// Cuts one track's 16 kHz audio into chunks of speech, as the voice detector finds them:
/// stretches of speech, joined when close together, at most MAX_CHUNK long. Silence, music and
/// other noise are left out.
pub struct Chunker {
    track: Track,
    vad: Vad,
    /// Recent audio; `buf[0]` is sample `start` of the track.
    buf: Vec<f32>,
    start: u64,
    /// Where the last chunk ended, so padding never repeats audio in the next one.
    emitted: u64,
    /// Speech found but not sent yet: the next stretch may follow closely enough to join it.
    pending: Option<std::ops::Range<u64>>,
}

impl Chunker {
    pub fn new(track: Track) -> anyhow::Result<Chunker> {
        Ok(Chunker { track, vad: Vad::new()?, buf: Vec::new(), start: 0, emitted: 0, pending: None })
    }

    /// `samples` must follow on directly from the previous push.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<Chunk>) {
        self.buf.extend_from_slice(samples);
        for speech in self.vad.push(samples) {
            self.join(speech, out);
        }
        // Nothing followed within the gap: send what's waiting.
        let end = self.start + self.buf.len() as u64;
        if let Some(p) = &self.pending
            && !self.vad.in_speech()
            && end > p.end + JOIN_GAP
        {
            let p = self.pending.take().unwrap();
            self.emit(p, out);
        }
        if self.buf.len() > 2 * KEEP {
            let drop = self.buf.len() - KEEP;
            self.buf.drain(..drop);
            self.start += drop as u64;
        }
    }

    pub fn finish(&mut self, out: &mut Vec<Chunk>) {
        for speech in self.vad.finish() {
            self.join(speech, out);
        }
        if let Some(p) = self.pending.take() {
            self.emit(p, out);
        }
    }

    /// Joins a stretch of speech to the one waiting if it follows within JOIN_GAP and the two
    /// fit in JOIN_MAX; otherwise sends the waiting one.
    fn join(&mut self, speech: std::ops::Range<u64>, out: &mut Vec<Chunk>) {
        match self.pending.take() {
            Some(p) if speech.start <= p.end + JOIN_GAP && speech.end - p.start <= JOIN_MAX => self.pending = Some(p.start..speech.end),
            Some(p) => {
                self.emit(p, out);
                self.pending = Some(speech);
            }
            None => self.pending = Some(speech),
        }
    }

    fn emit(&mut self, speech: std::ops::Range<u64>, out: &mut Vec<Chunk>) {
        let end = self.start + self.buf.len() as u64;
        let from = speech.start.saturating_sub(PAD).max(self.emitted).max(self.start);
        let to = (speech.end + PAD).min(end);
        if from >= to {
            return;
        }
        let mut from = from;
        while from < to {
            let at = |i: u64| (i - self.start) as usize;
            let cut = if to - from <= MAX_CHUNK {
                to
            } else {
                let window = from + MAX_CHUNK - CUT_WINDOW;
                self.start + (audio::quietest_frame(&self.buf, at(window), at(from + MAX_CHUNK)) + audio::FRAME) as u64
            };
            out.push(Chunk { track: self.track, start_sample: from, samples: self.buf[at(from)..at(cut)].to_vec() });
            from = cut;
        }
        self.emitted = to;
    }
}

pub fn live_path(meeting_dir: &Path) -> PathBuf {
    meeting_dir.join("live.jsonl")
}

/// The transcriber thread. Returns the sender to feed chunks to; dropping every sender
/// lets it finish the queue and exit. The thread returns Ok(()) if every chunk was transcribed.
pub fn spawn(model: PathBuf, meeting_dir: PathBuf) -> Result<(mpsc::Sender<Chunk>, JoinHandle<Result<()>>)> {
    let (tx, rx) = mpsc::channel::<Chunk>();
    let mut file = OpenOptions::new().create(true).append(true).open(live_path(&meeting_dir))?;
    let handle = thread::spawn(move || {
        background_priority();
        let mut transcriber = Transcriber::load(&model)?;
        for chunk in rx {
            let offset_ms = chunk.start_sample * 1000 / SECOND;
            for segment in transcriber.transcribe(&chunk.samples, offset_ms, chunk.track.who())? {
                writeln!(file, "{}", serde_json::to_string(&segment)?)?;
            }
            file.flush()?;
        }
        Ok(())
    });
    Ok((tx, handle))
}

/// Live transcription runs during video calls: give it a lower priority than the call. Audio
/// capture runs on its own real-time threads and isn't affected.
fn background_priority() {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
    #[cfg(not(target_os = "macos"))]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10); // on Linux this affects only this thread
    }
}

/// Reads the live transcript's segments, in the order they were written.
pub fn read(meeting_dir: &Path) -> Result<Vec<Segment>> {
    let file = File::open(live_path(meeting_dir))?;
    BufReader::new(file)
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}

/// Reads segments appended since `pos` bytes; returns them and the new position.
/// A half-written last line is left for next time.
pub fn read_from(meeting_dir: &Path, pos: u64) -> Result<(Vec<Segment>, u64)> {
    let text = match fs::read(live_path(meeting_dir)) {
        Ok(bytes) => bytes,
        Err(_) => return Ok((Vec::new(), pos)),
    };
    let new = &text[pos.min(text.len() as u64) as usize..];
    let complete = new.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let segments = std::str::from_utf8(&new[..complete])?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Segment>, _>>()?;
    Ok((segments, pos + complete as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 5.5 s of synthetic Norwegian speech (macOS's Nora voice), at -46 dBFS: as quiet as a
    /// low-gain mic, where a volume threshold heard almost nothing.
    fn speech() -> Vec<f32> {
        crate::audio::read(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/speech-nb.wav"))).unwrap()
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (seconds * SAMPLE_RATE as f32) as usize]
    }

    fn tone(seconds: f32) -> Vec<f32> {
        (0..(seconds * SAMPLE_RATE as f32) as usize).map(|i| (i as f32 * 0.05).sin() * 0.3).collect()
    }

    /// Feeds the audio in 20 ms pieces, as the recorder does, or all at once, as the pipeline
    /// does when it transcribes a meeting again.
    fn chunks_fed(parts: &[Vec<f32>], piece: usize) -> Vec<Chunk> {
        let mut c = Chunker::new(Track::Mic).unwrap();
        let mut out = Vec::new();
        for part in parts {
            for piece in part.chunks(piece) {
                c.push(piece, &mut out);
            }
        }
        c.finish(&mut out);
        out
    }

    fn chunks(parts: &[Vec<f32>]) -> Vec<Chunk> {
        chunks_fed(parts, 320)
    }

    fn secs(samples: u64) -> f32 {
        samples as f32 / SAMPLE_RATE as f32
    }

    #[test]
    fn finds_quiet_speech_and_leaves_out_silence() {
        let out = chunks(&[silence(3.0), speech(), silence(2.0), speech(), silence(3.0)]);
        assert!(!out.is_empty());
        let first = secs(out[0].start_sample);
        assert!((2.8..=3.6).contains(&first), "first chunk at {first} s");
        // Both stretches of speech (2 × 5.5 s, plus padding), not the 8 s of silence around them.
        let heard: f32 = out.iter().map(|c| secs(c.samples.len() as u64)).sum();
        assert!((8.0..=13.5).contains(&heard), "{heard} s in chunks");
        let in_gap = out.iter().any(|c| (9.0..10.0).contains(&secs(c.start_sample)));
        assert!(!in_gap, "a chunk started in the pause between the two");
    }

    #[test]
    fn leaves_out_tones_and_silence() {
        assert!(chunks(&[silence(2.0), tone(5.0), silence(2.0)]).is_empty());
    }

    #[test]
    fn splits_long_speech_without_overlap() {
        let long: Vec<Vec<f32>> = (0..8).map(|_| speech()).collect();
        let out = chunks(&long);
        assert!(out.len() >= 4, "{} chunks", out.len());
        assert!(out.iter().all(|c| c.samples.len() as u64 <= MAX_CHUNK));
        for pair in out.windows(2) {
            assert!(pair[0].start_sample + pair[0].samples.len() as u64 <= pair[1].start_sample, "overlapping chunks");
        }
    }

    #[test]
    fn finds_the_same_speech_however_the_audio_arrives() {
        let parts = [silence(3.0), speech(), silence(2.0), speech(), silence(3.0)];
        let live: Vec<(u64, usize)> = chunks_fed(&parts, 320).iter().map(|c| (c.start_sample, c.samples.len())).collect();
        let at_once: Vec<(u64, usize)> = chunks_fed(&[parts.concat()], usize::MAX).iter().map(|c| (c.start_sample, c.samples.len())).collect();
        assert_eq!(live, at_once);
    }
}
