//! Transcribes each speaker turn of a 16 kHz WAV file on its own, to check who said what.
//!
//!   cargo run --release --example turn_text -- <file.wav>
use heylisten::config::Config;
use heylisten::transcribe::Transcriber;
use heylisten::{audio, diarize, transcript::Who};

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: turn_text <file.wav>");
    let samples = audio::read(path.as_ref())?;
    let mut t = Transcriber::load(&Config::load()?.whisper_model)?;
    for turn in diarize::diarize(&samples, false)? {
        let (s, e) = (turn.start_ms as usize * 16, (turn.end_ms as usize * 16).min(samples.len()));
        let text: Vec<String> = t.transcribe(&samples[s..e], 0, Who::Others)?.into_iter().map(|s| s.text).collect();
        println!("{:6.1}-{:6.1} speaker {:>2}: {}", turn.start_ms as f64 / 1000.0, turn.end_ms as f64 / 1000.0, turn.speaker, text.join(" "));
    }
    Ok(())
}
