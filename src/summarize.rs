//! The summary: transcript → Borealis (built in, or through Ollama) → Norwegian Markdown.

use std::fs;

use anyhow::{Context, Result};

use crate::config::{Config, SummaryEngine, prompt_path};
use crate::{engine, ollama};
use crate::transcript::{Segment, Transcript};

/// Bigger windows cost RAM and time without helping a meeting summary much.
const MAX_CTX: u64 = 32_768;
/// Room left for the model's answer.
const ANSWER_TOKENS: u64 = 2_048;
/// Rough Norwegian token estimate. Errs on the safe side.
const CHARS_PER_TOKEN: u64 = 3;

/// Whoever writes the summary.
enum Writer<'a> {
    Builtin(engine::Server),
    Ollama { url: &'a str, model: &'a str },
}

impl Writer<'_> {
    fn chat(&self, num_ctx: u64, system: &str, user: &str) -> Result<String> {
        match self {
            Writer::Builtin(server) => server.chat(system, user),
            Writer::Ollama { url, model } => ollama::chat(url, model, num_ctx, system, user),
        }
    }
}

/// Returns the summary Markdown. `on_part(i, n)` is called before each part when the transcript is split.
pub fn summarize(config: &Config, transcript: &Transcript, mut on_part: impl FnMut(usize, usize)) -> Result<String> {
    let path = prompt_path();
    let prompt = fs::read_to_string(&path).with_context(|| format!("can't read {}", path.display()))?;
    let (writer, num_ctx) = match config.summary_engine {
        SummaryEngine::Builtin => {
            let model = engine::borealis_path().context("the summary model isn't downloaded yet (run heylisten setup)")?;
            (Writer::Builtin(engine::Server::start(&model, MAX_CTX)?), MAX_CTX)
        }
        SummaryEngine::Ollama => {
            let (url, model) = (config.ollama_url.as_str(), config.ollama_model.as_str());
            let num_ctx = ollama::context_length(url, model)
                .with_context(|| format!("Ollama not reachable at {url}, or model {model} not pulled"))?
                .unwrap_or(8_192)
                .min(MAX_CTX);
            (Writer::Ollama { url, model }, num_ctx)
        }
    };
    let budget_chars = num_ctx.saturating_sub(prompt.len() as u64 / CHARS_PER_TOKEN + ANSWER_TOKENS) * CHARS_PER_TOKEN;

    let parts = split(&transcript.segments, budget_chars as usize);
    if parts.len() == 1 {
        return writer.chat(num_ctx, &prompt, &format!("Transkripsjon:\n\n{}", parts[0]));
    }
    let n = parts.len();
    let mut summaries = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        on_part(i + 1, n);
        let user = format!(
            "Dette er del {} av {n} av et langt møte. Lag referat bare for denne delen.\n\nTranskripsjon:\n\n{part}",
            i + 1
        );
        summaries.push(format!("# Del {}\n\n{}", i + 1, writer.chat(num_ctx, &prompt, &user)?));
    }
    let user = format!(
        "Her er referater av hver del av et langt møte, i rekkefølge. Slå dem sammen til ett referat for hele møtet.\n\n{}",
        summaries.join("\n\n")
    );
    writer.chat(num_ctx, &prompt, &user)
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

/// Splits segments into transcript texts that each fit in `budget_chars`.
fn split(segments: &[Segment], budget_chars: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current: Vec<Segment> = Vec::new();
    let mut len = 0;
    for s in segments {
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

fn render(segments: Vec<Segment>) -> String {
    let paragraphs = Transcript { segments }.paragraphs();
    paragraphs.iter().map(|p| format!("{}: {}", p.who.label(), p.text)).collect::<Vec<_>>().join("\n\n")
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
            .map(|i| Segment { start_ms: i * 1000, end_ms: i * 1000 + 900, who: Who::Others, text: "x".repeat(84) })
            .collect();
        let parts = split(&segments, 300);
        assert_eq!(parts.len(), 4);
        assert!(parts.iter().all(|p| p.starts_with("Andre: ")));
        assert_eq!(split(&segments, 10_000).len(), 1);
    }
}
