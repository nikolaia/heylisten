//! Acoustic echo cancellation: removes what the speakers played (the system track) from the
//! mic, so the others' voices and music don't end up as your own lines.
//!
//! The model is DTLN-aec (Nils L. Westhausen and Bernd T. Meyer, MIT,
//! <https://github.com/breizhn/DTLN-aec>), 128 units. The processing follows Anarlog's
//! `crates/aec` (Fastrepl, MIT, <https://github.com/fastrepl/anarlog>), run here with the
//! pure-Rust `tract` instead of onnxruntime, which sherpa-onnx already links.

use std::sync::Arc;

use anyhow::{Context, Result};
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use tract_onnx::prelude::*;

const MODEL_1: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/aec_model_128_1.onnx"));
const MODEL_2: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/aec_model_128_2.onnx"));
/// The model was trained on 512-sample blocks moving 128 samples at a time, at 16 kHz.
const BLOCK: usize = 512;
const SHIFT: usize = 128;
const BINS: usize = BLOCK / 2 + 1;
const STATES: usize = 2 * 128 * 2;

type Plan = Arc<TypedRunnableModel>;

/// Streaming echo canceller. Feed it mic and loopback (what the speakers played) sample-aligned;
/// it returns exactly as many samples as it's fed, minus what's still in its buffers until
/// `finish`.
pub struct Aec {
    model_1: Plan,
    model_2: Plan,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    states_1: Vec<f32>,
    states_2: Vec<f32>,
    mic: Vec<f32>,
    lpb: Vec<f32>,
    out: Vec<f32>,
    pending_mic: Vec<f32>,
    pending_lpb: Vec<f32>,
    /// The model's latency, dropped from the start so the output lines up with the input.
    skip: usize,
    fed: u64,
    produced: u64,
}

impl Aec {
    pub fn new() -> Result<Aec> {
        let load = |bytes: &[u8]| -> Result<Plan> {
            tract_onnx::onnx().model_for_read(&mut &bytes[..])?.into_optimized()?.into_runnable()
        };
        let mut planner = RealFftPlanner::<f32>::new();
        Ok(Aec {
            model_1: load(MODEL_1).context("can't load the echo cancellation model")?,
            model_2: load(MODEL_2).context("can't load the echo cancellation model")?,
            fft: planner.plan_fft_forward(BLOCK),
            ifft: planner.plan_fft_inverse(BLOCK),
            states_1: vec![0.0; STATES],
            states_2: vec![0.0; STATES],
            mic: vec![0.0; BLOCK],
            lpb: vec![0.0; BLOCK],
            out: vec![0.0; BLOCK],
            pending_mic: Vec::new(),
            pending_lpb: Vec::new(),
            skip: BLOCK - SHIFT,
            fed: 0,
            produced: 0,
        })
    }

    /// Cancels `lpb` out of `mic` (same length, sample-aligned) and appends the result to `out`.
    pub fn process(&mut self, mic: &[f32], lpb: &[f32], out: &mut Vec<f32>) -> Result<()> {
        let n = mic.len().min(lpb.len());
        self.pending_mic.extend_from_slice(&mic[..n]);
        self.pending_lpb.extend_from_slice(&lpb[..n]);
        self.fed += n as u64;
        let mut used = 0;
        while self.pending_mic.len() - used >= SHIFT {
            self.block(used)?;
            used += SHIFT;
            self.emit(out);
        }
        self.pending_mic.drain(..used);
        self.pending_lpb.drain(..used);
        Ok(())
    }

    /// Flushes what's still buffered, so the output is exactly as long as the input.
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<()> {
        while self.produced < self.fed {
            self.pending_mic.resize(SHIFT.max(self.pending_mic.len()), 0.0);
            self.pending_lpb.resize(SHIFT.max(self.pending_lpb.len()), 0.0);
            self.block(0)?;
            self.pending_mic.drain(..SHIFT);
            self.pending_lpb.drain(..SHIFT);
            self.emit(out);
        }
        let extra = (self.produced - self.fed) as usize;
        out.truncate(out.len().saturating_sub(extra));
        self.produced = self.fed;
        Ok(())
    }

    /// The next `SHIFT` output samples go out of `self.out[..SHIFT]`, minus the start-up skip.
    fn emit(&mut self, out: &mut Vec<f32>) {
        let skip = self.skip.min(SHIFT);
        self.skip -= skip;
        out.extend_from_slice(&self.out[skip..SHIFT]);
        self.produced += (SHIFT - skip) as u64;
    }

    /// One 128-sample step: mask the mic's spectrum with model 1, then clean up with model 2.
    fn block(&mut self, from: usize) -> Result<()> {
        shift_in(&mut self.mic, &self.pending_mic[from..from + SHIFT]);
        shift_in(&mut self.lpb, &self.pending_lpb[from..from + SHIFT]);

        let (mut mic_spec, mic_mag) = self.spectrum(&self.mic)?;
        let (_, lpb_mag) = self.spectrum(&self.lpb)?;
        let mask = run(&self.model_1, [(&[1, 1, BINS], &mic_mag), (&[1, 2, 128, 2], &self.states_1), (&[1, 1, BINS], &lpb_mag)])?;
        self.states_1 = mask.1;
        for (c, m) in mic_spec.iter_mut().zip(&mask.0) {
            *c *= *m;
        }
        let mut estimate = vec![0.0; BLOCK];
        let mut scratch = vec![Complex::default(); self.ifft.get_scratch_len()];
        self.ifft.process_with_scratch(&mut mic_spec, &mut estimate, &mut scratch).context("echo cancellation: inverse FFT")?;
        estimate.iter_mut().for_each(|s| *s /= BLOCK as f32);

        let block = run(&self.model_2, [(&[1, 1, BLOCK], &estimate), (&[1, 2, 128, 2], &self.states_2), (&[1, 1, BLOCK], &self.lpb)])?;
        self.states_2 = block.1;
        // Overlap-add.
        self.out.copy_within(SHIFT.., 0);
        self.out[BLOCK - SHIFT..].fill(0.0);
        for (o, b) in self.out.iter_mut().zip(&block.0) {
            *o += *b;
        }
        Ok(())
    }

    fn spectrum(&self, block: &[f32]) -> Result<(Vec<Complex<f32>>, Vec<f32>)> {
        let mut input = block.to_vec();
        let mut spec = vec![Complex::default(); BINS];
        let mut scratch = vec![Complex::default(); self.fft.get_scratch_len()];
        self.fft.process_with_scratch(&mut input, &mut spec, &mut scratch).context("echo cancellation: FFT")?;
        let mag = spec.iter().map(|c| c.norm()).collect();
        Ok((spec, mag))
    }
}

/// Shifts `buffer` left by `SHIFT` samples and puts `chunk` at the end.
fn shift_in(buffer: &mut [f32], chunk: &[f32]) {
    buffer.copy_within(SHIFT.., 0);
    buffer[BLOCK - SHIFT..].copy_from_slice(chunk);
}

/// Runs a model on (shape, data) inputs; returns its two outputs (result, new states).
fn run(plan: &Plan, inputs: [(&[usize], &[f32]); 3]) -> Result<(Vec<f32>, Vec<f32>)> {
    let inputs: TVec<TValue> = inputs
        .into_iter()
        .map(|(shape, data)| Ok(Tensor::from_shape(shape, data)?.into()))
        .collect::<Result<_>>()?;
    let outputs = plan.run(inputs)?;
    let first = outputs[0].try_as_plain_ram()?.as_slice::<f32>()?.to_vec();
    let second = outputs[1].try_as_plain_ram()?.as_slice::<f32>()?.to_vec();
    Ok((first, second))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(len: usize, freq: f32) -> Vec<f32> {
        (0..len).map(|i| (i as f32 * freq * std::f32::consts::TAU / 16000.0).sin() * 0.3).collect()
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
    }

    #[test]
    fn output_is_exactly_as_long_as_the_input() {
        let mut aec = Aec::new().unwrap();
        let (mic, lpb) = (tone(16000 + 77, 440.0), vec![0.0; 16000 + 77]);
        let mut out = Vec::new();
        for (m, l) in mic.chunks(1000).zip(lpb.chunks(1000)) {
            aec.process(m, l, &mut out).unwrap();
        }
        aec.finish(&mut out).unwrap();
        assert_eq!(out.len(), mic.len());
    }

    #[test]
    fn removes_echo() {
        // The mic hears only the speakers: a quieter, slightly delayed copy of the loopback.
        // (Keeping your voice is checked against Anarlog's real recordings in
        // examples/aec_check.rs; synthetic tones get suppressed like noise.)
        let n = 16000 * 3;
        let speakers = tone(n, 300.0);
        let mut mic = vec![0.0; n];
        for i in 80..n {
            mic[i] = speakers[i - 80] * 0.5;
        }
        let mut aec = Aec::new().unwrap();
        let mut out = Vec::new();
        aec.process(&mic, &speakers, &mut out).unwrap();
        aec.finish(&mut out).unwrap();
        let (before, after) = (rms(&mic[8000..]), rms(&out[8000..]));
        assert!(after < before * 0.3, "echo {before} → {after}");
    }
}
