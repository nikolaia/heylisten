//! The summary: transcript → Borealis (built in, or through Ollama) → Norwegian Markdown.

use std::fs;

use anyhow::{Context, Result};

use crate::config::{Config, SummaryEngine, prompt_path};
use crate::{engine, ollama};
use crate::transcript::{Segment, Transcript};

/// Bigger windows cost RAM and time without helping a meeting summary much. Longer
/// transcripts are summarized in parts.
const MAX_CTX: u64 = 32_768;
const MIN_CTX: u64 = 8_192;
/// Room left for the model's answer.
const ANSWER_TOKENS: u64 = 2_048;
/// Room for the prompt, when sizing the context.
const PROMPT_ALLOWANCE: u64 = 4_000;
/// Rough Norwegian token estimate. Errs on the safe side.
const CHARS_PER_TOKEN: u64 = 3;

/// The language model for this meeting: started once, then used to find speakers' names and
/// to write the summary.
pub struct Writer<'a> {
    engine: Engine<'a>,
    num_ctx: u64,
}

enum Engine<'a> {
    Builtin(engine::Server),
    Ollama { url: &'a str, model: &'a str },
}

impl<'a> Writer<'a> {
    /// Starts the configured engine with as much context as this transcript needs: memory for
    /// it is reserved up front, and a 32k window costs gigabytes a short meeting doesn't need.
    pub fn open(config: &'a Config, transcript: &Transcript) -> Result<Writer<'a>> {
        let transcript_chars: usize = transcript.segments.iter().map(|s| s.text.len() + 16).sum();
        let needed = (PROMPT_ALLOWANCE + transcript_chars as u64) / CHARS_PER_TOKEN + ANSWER_TOKENS;
        let wanted = needed.div_ceil(4_096) * 4_096;
        Ok(match config.summary_engine {
            SummaryEngine::Builtin => {
                let model = engine::borealis_path().context("the summary model isn't downloaded yet (run heylisten setup)")?;
                let num_ctx = wanted.clamp(MIN_CTX, MAX_CTX);
                Writer { engine: Engine::Builtin(engine::Server::start(&model, num_ctx)?), num_ctx }
            }
            SummaryEngine::Ollama => {
                let (url, model) = (config.ollama_url.as_str(), config.ollama_model.as_str());
                let num_ctx = ollama::context_length(url, model)
                    .with_context(|| format!("Ollama not reachable at {url}, or model {model} not pulled"))?
                    .unwrap_or(8_192)
                    .min(wanted.clamp(MIN_CTX, MAX_CTX));
                Writer { engine: Engine::Ollama { url, model }, num_ctx }
            }
        })
    }

    /// One chat turn. With `json`, the reply is constrained to a JSON object.
    pub fn chat(&self, system: &str, user: &str, json: bool) -> Result<String> {
        match &self.engine {
            Engine::Builtin(server) => server.chat(system, user, json),
            Engine::Ollama { url, model } => ollama::chat(url, model, self.num_ctx, system, user, json),
        }
    }

    /// How much transcript fits in one request, after `prompt` and room for the answer.
    pub fn budget_chars(&self, prompt: &str) -> usize {
        (self.num_ctx.saturating_sub(prompt.len() as u64 / CHARS_PER_TOKEN + ANSWER_TOKENS) * CHARS_PER_TOKEN) as usize
    }

    /// Returns the summary Markdown. `on_part(i, n)` is called before each part when the
    /// transcript is split.
    pub fn summarize(&self, transcript: &Transcript, mut on_part: impl FnMut(usize, usize)) -> Result<String> {
        let path = prompt_path();
        let prompt = fs::read_to_string(&path).with_context(|| format!("can't read {}", path.display()))?;
        let parts = split(transcript, self.budget_chars(&prompt));
        if parts.len() == 1 {
            return self.chat(&prompt, &format!("Transkripsjon:\n\n{}", parts[0]), false);
        }
        let n = parts.len();
        let mut summaries = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            on_part(i + 1, n);
            let user = format!(
                "Dette er del {} av {n} av et langt møte. Lag referat bare for denne delen.\n\nTranskripsjon:\n\n{part}",
                i + 1
            );
            summaries.push(format!("# Del {}\n\n{}", i + 1, self.chat(&prompt, &user, false)?));
        }
        let user = format!(
            "Her er referater av hver del av et langt møte, i rekkefølge. Slå dem sammen til ett referat for hele møtet.\n\n{}",
            summaries.join("\n\n")
        );
        self.chat(&prompt, &user, false)
    }
}

/// The summary model's name for the note's frontmatter.
pub fn model_name(config: &Config) -> &str {
    match config.summary_engine {
        SummaryEngine::Builtin => "borealis-12b",
        SummaryEngine::Ollama => short_name(&config.ollama_model),
    }
}

/// "hf.co/NbAiLab/borealis-12b-gguf:latest" → "borealis-12b".
pub fn short_name(model: &str) -> &str {
    let name = model.rsplit('/').next().unwrap_or(model);
    let name = name.split(':').next().unwrap_or(name);
    name.strip_suffix("-gguf").unwrap_or(name)
}

/// Splits the transcript into texts that each fit in `budget_chars`.
pub fn split(transcript: &Transcript, budget_chars: usize) -> Vec<String> {
    let render = |segments: Vec<Segment>| Transcript { segments, names: transcript.names.clone() }.text();
    let mut parts = Vec::new();
    let mut current: Vec<Segment> = Vec::new();
    let mut len = 0;
    for s in &transcript.segments {
        let cost = s.text.len() + 16; // + speaker label and newlines
        if len + cost > budget_chars && !current.is_empty() {
            parts.push(render(std::mem::take(&mut current)));
            len = 0;
        }
        current.push(s.clone());
        len += cost;
    }
    parts.push(render(current));
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::Who;

    #[test]
    fn short_model_names() {
        assert_eq!(short_name("hf.co/NbAiLab/borealis-12b-gguf"), "borealis-12b");
        assert_eq!(short_name("gemma3:12b"), "gemma3");
    }

    #[test]
    fn splits_long_monologues() {
        let segments: Vec<Segment> = (0..10)
            .map(|i| Segment { start_ms: i * 1000, end_ms: i * 1000 + 900, who: Who::Others, text: "x".repeat(84), words: Vec::new() })
            .collect();
        let transcript = Transcript { segments, ..Default::default() };
        let parts = split(&transcript, 300);
        assert_eq!(parts.len(), 4);
        assert!(parts.iter().all(|p| p.starts_with("Andre: ")));
        assert_eq!(split(&transcript, 10_000).len(), 1);
    }
}
