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
///
/// A recorder that was killed (a crash, Force Quit) leaves its files unfinished: empty, or with
/// a header that counts fewer samples than were written after it. heyListen only ever appends,
/// so in its own files everything after the header is audio, whatever the header says.
pub fn read(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("can't open {}", path.display()))?;
    if bytes.len() <= OWN_HEADER {
        return Ok(Vec::new()); // created, but killed before anything was written
    }
    if is_own_wav(&bytes) {
        return Ok(bytes[OWN_HEADER..].as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f32 / i16::MAX as f32).collect());
    }
    let reader = hound::WavReader::new(std::io::Cursor::new(bytes)).with_context(|| format!("can't open {}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE || spec.channels != 1 {
        bail!("{} is not 16 kHz mono", path.display());
    }
    reader
        .into_samples::<i16>()
        .map(|s| Ok(s? as f32 / i16::MAX as f32))
        .collect()
}

/// heyListen's own WAV files: a 44-byte header, then 16 kHz mono 16-bit samples.
const OWN_HEADER: usize = 44;

/// Whether the file has exactly the layout `WavWriter` writes. Other WAVs (from ffmpeg, say)
/// may have more chunks before the audio, so they go through the full parser.
fn is_own_wav(bytes: &[u8]) -> bool {
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    bytes.len() > OWN_HEADER
        && &bytes[0..4] == b"RIFF"
        && &bytes[8..16] == b"WAVEfmt "
        && u32_at(16) == 16
        && u16_at(20) == 1 // PCM
        && u16_at(22) == 1 // mono
        && u32_at(24) == SAMPLE_RATE
        && u16_at(34) == 16
        && &bytes[36..40] == b"data"
}

/// 20 ms analysis frames.
pub const FRAME: usize = SAMPLE_RATE as usize / 50;

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

    /// A WAV as `WavWriter` writes it, with the header's sizes as given (a killed recorder
    /// leaves them at 0 or out of date).
    fn own_wav(samples: &[i16], header_samples: u32) -> Vec<u8> {
        let data = header_samples * 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        b.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data.to_le_bytes());
        for s in samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        b
    }

    #[test]
    fn reads_what_a_killed_recorder_left() {
        let dir = std::env::temp_dir().join(format!("heylisten-audio-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            read(&path).unwrap().len()
        };
        assert_eq!(file("empty.wav", b""), 0, "created, nothing written");
        assert_eq!(file("header-only.wav", &own_wav(&[], 0)[..44]), 0);
        assert_eq!(file("stale-header.wav", &own_wav(&[100, -100, 200, -200], 0)), 4, "the header says 0, four were written");
        assert_eq!(file("older-header.wav", &own_wav(&[1, 2, 3, 4, 5, 6], 2)), 6);

        // A file WavWriter finished properly reads the same as before.
        let path = dir.join("finished.wav");
        let mut w = WavWriter::create(&path).unwrap();
        w.write(&[0.5, -0.5, 0.25]).unwrap();
        w.finish().unwrap();
        let samples = read(&path).unwrap();
        assert_eq!(samples.len(), 3);
        assert!((samples[0] - 0.5).abs() < 1e-3 && (samples[1] + 0.5).abs() < 1e-3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
