use std::path::Path;

use anyhow::{Context, Result, bail};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

use crate::transcript::{Segment, Who};

pub struct Transcriber {
    state: WhisperState,
}

impl Transcriber {
    pub fn load(model: &Path) -> Result<Transcriber> {
        if !model.is_file() {
            bail!("whisper model not found at {}. Run `heylisten doctor` for the download command", model.display());
        }
        whisper_rs::install_logging_hooks(); // silences whisper.cpp's stderr chatter
        let path = model.to_str().context("model path is not valid UTF-8")?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .with_context(|| format!("can't load whisper model {}", model.display()))?;
        Ok(Transcriber { state: ctx.create_state()? })
    }

    /// Transcribes 16 kHz mono samples in Norwegian. `offset_ms` is where they start in the meeting.
    /// `progress` gets 0–100.
    pub fn transcribe(&mut self, samples: &[f32], offset_ms: u64, who: Who, progress: impl FnMut(i32) + 'static) -> Result<Vec<Segment>> {
        let state = &mut self.state;
        let mut params = FullParams::new(SamplingStrategy::BeamSearch { beam_size: 5, patience: -1.0 });
        params.set_language(Some("no"));
        params.set_n_threads(std::thread::available_parallelism().map_or(4, |n| n.get().min(8)) as _);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_progress_callback_safe(progress);
        // Short segments, so diarization can give each its own speaker.
        params.set_token_timestamps(true);
        params.set_max_len(80);
        params.set_split_on_word(true);
        // Chunks are independent; carrying text over just leaks words between them.
        params.set_no_context(true);
        state.full(params, samples)?;

        // Whisper pads to 30 s and may report times in the padding.
        let len_ms = samples.len() as u64 * 1000 / crate::audio::SAMPLE_RATE as u64;
        let mut segments = Vec::new();
        for s in state.as_iter() {
            let text = s.to_str_lossy()?.trim().to_string();
            if text.is_empty() || s.no_speech_probability() > 0.6 || is_hallucination(&text) {
                continue;
            }
            // whisper timestamps are in centiseconds.
            segments.push(Segment {
                start_ms: offset_ms + (s.start_timestamp().max(0) as u64 * 10).min(len_ms),
                end_ms: offset_ms + (s.end_timestamp().max(0) as u64 * 10).min(len_ms),
                who,
                text,
            });
        }
        Ok(segments)
    }
}

/// Subtitle credits whisper learned from TV subtitles and produces on silence or music.
fn is_hallucination(text: &str) -> bool {
    let t = text.to_lowercase();
    ["teksting av", "tekstet av", "undertekster av", "takk for at du så"].iter().any(|p| t.contains(p))
}
