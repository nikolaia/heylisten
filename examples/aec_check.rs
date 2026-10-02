//! Runs heyListen's echo cancellation on Anarlog's test recordings (mic + loopback) and prints
//! the output's loudness, to compare with Anarlog's expected snapshots.
//!   cargo run --release --example aec_check -- <mic.wav> <loopback.wav> [out.wav]
use heylisten::aec::Aec;
fn read(path: &str) -> Vec<f32> {
    hound::WavReader::open(path).unwrap().into_samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect()
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (mic, lpb) = (read(&a[1]), read(&a[2]));
    let n = mic.len().min(lpb.len());
    let mut aec = Aec::new().unwrap();
    let mut out = Vec::new();
    let t = std::time::Instant::now();
    for (m, l) in mic[..n].chunks(480).zip(lpb[..n].chunks(480)) {
        aec.process(m, l, &mut out).unwrap();
    }
    aec.finish(&mut out).unwrap();
    let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    let peak = |s: &[f32]| s.iter().fold(0f32, |m, x| m.max(x.abs()));
    println!("in: rms {:.6} peak {:.4} | out: rms {:.6} peak {:.4} | {} samples, {:.0}x real time",
        rms(&mic[..n]), peak(&mic[..n]), rms(&out), peak(&out), out.len(), n as f64 / 16000.0 / t.elapsed().as_secs_f64());
    if let Some(path) = a.get(3) {
        let spec = hound::WavSpec { channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for s in &out { w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16).unwrap(); }
        w.finalize().unwrap();
    }
}
