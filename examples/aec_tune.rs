//! Replays a kept debug meeting's echo cancellation offline, to compare settings on real audio:
//! mic-raw.wav (the mic before cancellation) against system.wav (the exact reference).
//!
//!   cargo run --release --example aec_tune -- <meeting-dir> [model-dir units] [shift-ms...]
//!
//! Prints the echo reduction (ERLE) over the stretches where the speakers play, for each
//! reference shift (positive takes the reference later). Without a model dir, the embedded
//! 128-unit model is used.
use std::time::Instant;

use heylisten::aec::Aec;
use heylisten::audio;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = std::path::Path::new(&a[1]);
    let mic = audio::read(&dir.join("mic-raw.wav")).unwrap();
    let reference = audio::read(&dir.join("system.wav")).unwrap();
    let (models, rest) = match a.get(2) {
        Some(m) if std::path::Path::new(m).is_dir() => (Some((m.clone(), a[3].parse::<usize>().unwrap())), &a[4..]),
        _ => (None, &a[2..]),
    };
    let shifts: Vec<i64> = if rest.is_empty() { vec![0] } else { rest.iter().map(|s| s.parse().unwrap()).collect() };
    for shift_ms in shifts {
        let mut aec = match &models {
            Some((dir, units)) => {
                let read = |n: usize| std::fs::read(format!("{dir}/model_{units}_{n}.onnx")).unwrap();
                Aec::with_models(&read(1), &read(2), *units).unwrap()
            }
            None => Aec::new().unwrap(),
        };
        let shift = shift_ms * 16;
        let shifted: Vec<f32> = (0..mic.len() as i64)
            .map(|i| reference.get((i + shift).max(0) as usize).copied().filter(|_| i + shift >= 0).unwrap_or(0.0))
            .collect();
        let started = Instant::now();
        let mut out = Vec::new();
        for (m, r) in mic.chunks(480).zip(shifted.chunks(480)) {
            aec.process(m, r, &mut out).unwrap();
        }
        aec.finish(&mut out).unwrap();
        let speed = mic.len() as f64 / 16000.0 / started.elapsed().as_secs_f64();

        // ERLE over 100 ms windows where the reference is playing (above -45 dBFS RMS).
        let (mut before, mut after) = (0f64, 0f64);
        for i in (0..mic.len().min(out.len())).step_by(1600) {
            let end = (i + 1600).min(mic.len()).min(out.len());
            let power = |s: &[f32]| s.iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
            let rms = (power(&shifted[i..end]) / (end - i) as f64).sqrt();
            if 20.0 * rms.max(1e-9).log10() > -45.0 {
                before += power(&mic[i..end]);
                after += power(&out[i..end]);
            }
        }
        let units = models.as_ref().map_or(128, |(_, u)| *u);
        println!("{units:>3} units, shift {shift_ms:>4} ms: echo reduced {:5.1} dB, {:.0}x real time", 10.0 * (before / after.max(1e-12)).log10(), speed);
    }
}
