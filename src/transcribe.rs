use std::path::Path;

use anyhow::{Context, Result, bail};
use whisper_rs::{DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState, WhisperTokenId};

use crate::audio;
use crate::transcript::{Segment, Who, Word};

pub struct Transcriber {
    state: WhisperState,
    beam_size: u32,
    /// Tokens from here up are whisper's own markers, not text.
    first_special: WhisperTokenId,
}

impl Transcriber {
    pub fn load(model: &Path) -> Result<Transcriber> {
        if !model.is_file() {
            bail!("whisper model not found at {}. Run `heylisten doctor` for the download command", model.display());
        }
        whisper_rs::install_logging_hooks(); // silences whisper.cpp's stderr chatter
        let path = model.to_str().context("model path is not valid UTF-8")?;
        // DTW word times: whisper's own segment times run up to a few seconds late, which put
        // words on the wrong speaker. NB-Whisper large is fine-tuned from large-v3, so its
        // alignment heads fit. Costs about a third more time, as whisper.cpp can't combine
        // it with flash attention.
        let mut params = WhisperContextParameters::default();
        params.flash_attn(false);
        params.dtw_parameters(DtwParameters { mode: DtwMode::ModelPreset { model_preset: DtwModelPreset::LargeV3 }, ..Default::default() });
        let ctx = WhisperContext::new_with_params(path, params).with_context(|| format!("can't load whisper model {}", model.display()))?;
        Ok(Transcriber { first_special: ctx.token_eot(), state: ctx.create_state()?, beam_size: 5 })
    }

    /// 1 means greedy decoding. Only for comparing; heyListen always uses 5.
    pub fn set_beam_size(&mut self, beam_size: u32) {
        self.beam_size = beam_size.max(1);
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
        let first_special = self.first_special;
        let state = &mut self.state;
        // Beam search, as NB-Whisper's authors recommend ("greatly increases the accuracy").
        // Greedy was twice as fast and identical on clean synthetic speech, but on real
        // meetings it changed and invented words.
        let strategy = match self.beam_size {
            1 => SamplingStrategy::Greedy { best_of: 1 },
            n => SamplingStrategy::BeamSearch { beam_size: n as i32, patience: -1.0 },
        };
        let mut params = FullParams::new(strategy);
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
            // whisper times are in centiseconds. A word starts at a token with a leading space;
            // its bytes may be split over tokens (å, ø).
            let at = |cs: i64| offset_ms + (cs.max(0) as u64 * 10).min(len_ms);
            let mut words: Vec<(u64, Vec<u8>)> = Vec::new();
            for i in 0..s.n_tokens() {
                let Some(token) = s.get_token(i) else { continue };
                if token.token_id() >= first_special {
                    continue;
                }
                let bytes = token.to_bytes()?;
                match words.last_mut() {
                    Some((_, word)) if !bytes.starts_with(b" ") => word.extend_from_slice(bytes),
                    _ => words.push((at(token.token_data().t_dtw), bytes.to_vec())),
                }
            }
            let words = words
                .into_iter()
                .map(|(at_ms, bytes)| Word { at_ms, text: String::from_utf8_lossy(&bytes).trim().to_string() })
                .filter(|w| !w.text.is_empty())
                .collect();
            segments.push(Segment { start_ms: at(s.start_timestamp()), end_ms: at(s.end_timestamp()), who, text, words });
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
