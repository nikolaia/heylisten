//! Audio files: everything heyListen stores is 16 kHz mono 16-bit WAV, which is what whisper wants.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::audio::sample::Sample;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

pub const SAMPLE_RATE: u32 = 16_000;

/// Decodes any supported audio file (wav, m4a, mp3, flac, ogg…) into a 16 kHz mono WAV.
/// Returns the duration in seconds.
pub fn import(src: &Path, dest: &Path) -> Result<f64> {
    let file = File::open(src).with_context(|| format!("can't open {}", src.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = src.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .with_context(|| format!("{} is not an audio file heyListen can read", src.display()))?;
    let track = format.default_track(TrackType::Audio).context("no audio track")?;
    let track_id = track.id;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).context("no audio track")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())?;

    let mut writer = WavWriter::create(dest)?;
    let mut resampler: Option<To16k> = None;
    let mut interleaved: Vec<f32> = Vec::new();
    let mut mono = Vec::new();
    let mut out = Vec::new();

    while let Some(packet) = format.next_packet()? {
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let channels = buf.spec().channels().count().max(1);
        let rate = buf.spec().rate();
        interleaved.resize(buf.samples_interleaved(), f32::MID);
        buf.copy_to_slice_interleaved(&mut interleaved);

        mono.clear();
        mono.extend(interleaved.chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32));

        let rs = match &mut resampler {
            Some(rs) => rs,
            None => resampler.insert(To16k::new(rate)?),
        };
        out.clear();
        rs.push(&mono, &mut out)?;
        writer.write(&out)?;
    }
    let Some(mut rs) = resampler else { bail!("{} contains no audio", src.display()) };
    out.clear();
    rs.finish(&mut out)?;
    writer.write(&out)?;
    writer.finish()
}

/// Reads a 16 kHz mono WAV written by heyListen into f32 samples.
pub fn read(path: &Path) -> Result<Vec<f32>> {
    let reader = hound::WavReader::open(path).with_context(|| format!("can't open {}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE || spec.channels != 1 {
        bail!("{} is not 16 kHz mono", path.display());
    }
    reader
        .into_samples::<i16>()
        .map(|s| Ok(s? as f32 / i16::MAX as f32))
        .collect()
}

/// 20 ms analysis frames.
pub const FRAME: usize = SAMPLE_RATE as usize / 50;
/// A frame louder than this probably contains speech.
pub const SPEECH_RMS: f32 = 0.01;

pub fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

/// Where the quietest whole frame in `samples[from..to]` starts.
pub fn quietest_frame(samples: &[f32], from: usize, to: usize) -> usize {
    (from..to.saturating_sub(FRAME).max(from))
        .step_by(FRAME)
        .min_by(|a, b| rms(&samples[*a..*a + FRAME]).total_cmp(&rms(&samples[*b..*b + FRAME])))
        .unwrap_or(from)
}

/// Seconds of the audio that are loud enough to be speech.
pub fn speech_seconds(samples: &[f32]) -> f32 {
    samples.chunks(FRAME).filter(|f| rms(f) > SPEECH_RMS).count() as f32 * FRAME as f32 / SAMPLE_RATE as f32
}

/// True if at least 0.3 s of the audio is loud enough to be speech. Whisper invents text
/// from near-silence, so anything else isn't worth transcribing.
pub fn has_speech(samples: &[f32]) -> bool {
    samples.chunks(FRAME).filter(|f| rms(f) > SPEECH_RMS).count() >= 15
}

/// Streaming 16 kHz mono WAV writer.
pub struct WavWriter {
    inner: hound::WavWriter<std::io::BufWriter<File>>,
    samples: u64,
}

impl WavWriter {
    pub fn create(path: &Path) -> Result<WavWriter> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        Ok(WavWriter { inner: hound::WavWriter::create(path, spec)?, samples: 0 })
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        for s in samples {
            self.inner.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
        }
        self.samples += samples.len() as u64;
        Ok(())
    }

    /// Updates the header so the file is valid up to here, even if we crash later.
    pub fn flush(&mut self) -> Result<()> {
        self.inner.flush()?;
        Ok(())
    }

    /// Finalizes the header. Returns the duration in seconds.
    pub fn finish(self) -> Result<f64> {
        self.inner.finalize()?;
        Ok(self.samples as f64 / SAMPLE_RATE as f64)
    }
}

/// Streaming mono resampler from any rate to 16 kHz.
pub struct To16k {
    fft: Option<Fft<f32>>,
    pending: Vec<f32>,
    delay_left: usize,
    total_in: u64,
    total_out: u64,
}

impl To16k {
    pub fn new(rate: u32) -> Result<To16k> {
        let fft = if rate == SAMPLE_RATE {
            None
        } else {
            Some(Fft::new(rate as usize, SAMPLE_RATE as usize, 1024, 1, FixedSync::Input)?)
        };
        let delay_left = fft.as_ref().map_or(0, |f| f.output_delay());
        Ok(To16k { fft, pending: Vec::new(), delay_left, total_in: 0, total_out: 0 })
    }

    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        self.total_in += input.len() as u64;
        if self.fft.is_none() {
            out.extend_from_slice(input);
            return Ok(());
        }
        self.pending.extend_from_slice(input);
        let n = self.fft.as_ref().unwrap().input_frames_next();
        let mut used = 0;
        while self.pending.len() - used >= n {
            let resampled = self.process(used, n)?;
            used += n;
            self.emit(&resampled, out);
        }
        self.pending.drain(..used);
        Ok(())
    }

    /// Flushes what's left so the output is exactly as long as the input.
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<()> {
        let Some(fft) = &self.fft else { return Ok(()) };
        let target = (self.total_in as f64 * fft.resample_ratio()).round() as u64;
        let n = fft.input_frames_next();
        while self.total_out < target {
            self.pending.resize(n, 0.0);
            let resampled = self.process(0, n)?;
            self.pending.clear();
            self.emit(&resampled, out);
        }
        out.truncate(out.len().saturating_sub((self.total_out - target) as usize));
        self.total_out = target;
        Ok(())
    }

    fn process(&mut self, from: usize, n: usize) -> Result<Vec<f32>> {
        let chunk = InterleavedSlice::new(&self.pending[from..from + n], 1, n)?;
        Ok(self.fft.as_mut().unwrap().process(&chunk, None)?.take_data())
    }

    /// Appends output, dropping the resampler's startup delay.
    fn emit(&mut self, resampled: &[f32], out: &mut Vec<f32>) {
        let skip = self.delay_left.min(resampled.len());
        self.delay_left -= skip;
        out.extend_from_slice(&resampled[skip..]);
        self.total_out += (resampled.len() - skip) as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_48k_to_16k_with_same_duration() {
        let input: Vec<f32> = (0..48_000 * 3).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let mut rs = To16k::new(48_000).unwrap();
        let mut out = Vec::new();
        for chunk in input.chunks(4410) {
            rs.push(chunk, &mut out).unwrap();
        }
        rs.finish(&mut out).unwrap();
        let expected = 16_000 * 3;
        assert_eq!(out.len(), expected);
    }
}
