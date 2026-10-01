use std::path::Path;

use anyhow::{Context, Result, bail};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

use crate::audio;
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
        // Flash attention: about 25 % faster on Metal, same text.
        let mut params = WhisperContextParameters::default();
        params.flash_attn(true);
        let ctx = WhisperContext::new_with_params(path, params).with_context(|| format!("can't load whisper model {}", model.display()))?;
        Ok(Transcriber { state: ctx.create_state()? })
    }

    /// Transcribes a chunk of 16 kHz mono speech in Norwegian. `offset_ms` is where it starts
    /// in the meeting.
    ///
    /// Whisper sometimes silently skips a stretch of speech. If the text is far too short for
    /// the amount of speech, the chunk is split at its quietest moment and each half is
    /// transcribed again; whichever version has more words wins.
    pub fn transcribe(&mut self, samples: &[f32], offset_ms: u64, who: Who) -> Result<Vec<Segment>> {
        self.transcribe_checked(samples, offset_ms, who, 2)
    }

    fn transcribe_checked(&mut self, samples: &[f32], offset_ms: u64, who: Who, splits_left: u32) -> Result<Vec<Segment>> {
        let segments = self.run(samples, offset_ms, who)?;
        let speech = audio::speech_seconds(samples);
        let words = word_count(&segments);
        if splits_left == 0 || speech < 2.0 || words as f32 >= speech * MIN_WORDS_PER_SPEECH_SECOND {
            return Ok(segments);
        }
        let n = samples.len();
        let at = audio::quietest_frame(samples, n / 3, 2 * n / 3) + audio::FRAME;
        let mut split = self.transcribe_checked(&samples[..at], offset_ms, who, splits_left - 1)?;
        let second_ms = offset_ms + at as u64 * 1000 / audio::SAMPLE_RATE as u64;
        split.extend(self.transcribe_checked(&samples[at..], second_ms, who, splits_left - 1)?);
        Ok(if word_count(&split) > words { split } else { segments })
    }

    /// One whisper pass over the samples.
    fn run(&mut self, samples: &[f32], offset_ms: u64, who: Who) -> Result<Vec<Segment>> {
        let state = &mut self.state;
        // Greedy decoding with flash attention: twice as fast and 500 MB lighter than beam
        // search, with the same text on our Norwegian benchmark. It runs during video calls.
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("no"));
        params.set_n_threads(4);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Short segments, so diarization can give each its own speaker.
        params.set_token_timestamps(true);
        params.set_max_len(80);
        params.set_split_on_word(true);
        // Chunks are independent; carrying text over just leaks words between them.
        params.set_no_context(true);
        state.full(params, samples)?;

        // Whisper pads to 30 s and may report times in the padding.
        let len_ms = samples.len() as u64 * 1000 / audio::SAMPLE_RATE as u64;
        let mut segments = Vec::new();
        for s in state.as_iter() {
            let text = strip_special_tokens(&s.to_str_lossy()?);
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

/// Below this rate, whisper has probably skipped some of the speech. People speak about 2–3
/// words a second; this leaves plenty of room for slow speakers.
const MIN_WORDS_PER_SPEECH_SECOND: f32 = 0.8;

fn word_count(segments: &[Segment]) -> usize {
    segments.iter().map(|s| s.text.split_whitespace().count()).sum()
}

/// Removes whisper's own markers, like NB-Whisper's `<|nocaptions|>` for "no speech here".
fn strip_special_tokens(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<|") {
        out.push_str(&rest[..start]);
        match rest[start..].find("|>") {
            Some(end) => rest = &rest[start + end + 2..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Subtitle credits whisper learned from TV subtitles and produces on silence or music.
fn is_hallucination(text: &str) -> bool {
    let t = text.to_lowercase();
    ["teksting av", "tekstet av", "undertekster av", "takk for at du så"].iter().any(|p| t.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_special_tokens() {
        assert_eq!(strip_special_tokens(" <|nocaptions|>"), "");
        assert_eq!(strip_special_tokens("Hei <|no|> på deg"), "Hei på deg");
        assert_eq!(strip_special_tokens("Helt vanlig tekst."), "Helt vanlig tekst.");
    }
}
