//! Transcribes a stretch of a kept meeting several ways, side by side, to choose settings on
//! real speech: the main model greedy, the main model with beam search, and the verbatim model.
//!
//!   cargo run --release --example compare -- <meeting-id> <start-minute> <minutes> [mic|system]
use std::path::PathBuf;
use std::time::Instant;

use heylisten::config::{Config, models_dir};
use heylisten::live::Chunker;
use heylisten::meeting::{Meeting, Track};
use heylisten::transcribe::Transcriber;
use heylisten::{audio, transcript::Who};

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let (id, from, minutes) = (&a[1], a[2].parse::<f32>()?, a[3].parse::<f32>()?);
    let track = if a.get(4).map(String::as_str) == Some("mic") { Track::Mic } else { Track::System };
    let meeting = Meeting::load(id)?;
    let samples = audio::read(&meeting.track_path(track))?;
    let (s, e) = ((from * 60.0 * 16000.0) as usize, ((from + minutes) * 60.0 * 16000.0) as usize);
    let samples = &samples[s.min(samples.len())..e.min(samples.len())];
    let mut chunker = Chunker::new(track);
    let mut chunks = Vec::new();
    chunker.push(samples, &mut chunks);
    chunker.finish(&mut chunks);

    let config = Config::load()?;
    let verbatim = models_dir().join("nb-whisper-large-verbatim-q5_0.bin");
    let runs: [(&str, PathBuf, u32); 3] =
        [("main, greedy", config.whisper_model.clone(), 1), ("main, beam 5", config.whisper_model.clone(), 5), ("verbatim, beam 5", verbatim, 5)];
    let mut results = Vec::new();
    for (name, model, beam) in runs {
        let mut t = Transcriber::load(&model)?;
        t.set_beam_size(beam);
        let started = Instant::now();
        let lines: Vec<String> = chunks
            .iter()
            .map(|c| t.transcribe(&c.samples, 0, Who::Others).map(|segs| segs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ")))
            .collect::<anyhow::Result<_>>()?;
        eprintln!("{name}: {:.1} s for {minutes} min", started.elapsed().as_secs_f32());
        results.push((name, lines));
    }
    for (i, c) in chunks.iter().enumerate() {
        let at = (s as u64 + c.start_sample) / 16000;
        println!("\n[{:02}:{:02}:{:02}]", at / 3600, at / 60 % 60, at % 60);
        for (name, lines) in &results {
            println!("  {name:>16}: {}", lines[i]);
        }
    }
    Ok(())
}
