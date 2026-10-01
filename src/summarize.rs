//! The summary: transcript → Ollama → Norwegian Markdown.

use std::fs;

use anyhow::{Context, Result};

use crate::config::{Config, prompt_path};
use crate::ollama;
use crate::transcript::{Segment, Transcript};

/// Bigger windows cost RAM and time without helping a meeting summary much.
const MAX_CTX: u64 = 32_768;
/// Room left for the model's answer.
const ANSWER_TOKENS: u64 = 2_048;
/// Rough Norwegian token estimate. Errs on the safe side.
const CHARS_PER_TOKEN: u64 = 3;

/// Returns the summary Markdown. `on_part(i, n)` is called before each part when the transcript is split.
pub fn summarize(config: &Config, transcript: &Transcript, mut on_part: impl FnMut(usize, usize)) -> Result<String> {
    let path = prompt_path();
    let prompt = fs::read_to_string(&path).with_context(|| format!("can't read {}", path.display()))?;
    let (url, model) = (&config.ollama_url, &config.ollama_model);
    let num_ctx = ollama::context_length(url, model)
        .with_context(|| format!("Ollama not reachable at {url}, or model {model} not pulled"))?
        .unwrap_or(8_192)
        .min(MAX_CTX);
    let budget_chars = num_ctx.saturating_sub(prompt.len() as u64 / CHARS_PER_TOKEN + ANSWER_TOKENS) * CHARS_PER_TOKEN;

    let parts = split(&transcript.segments, budget_chars as usize);
    if parts.len() == 1 {
        return ollama::chat(url, model, num_ctx, &prompt, &format!("Transkripsjon:\n\n{}", parts[0]));
    }
    let n = parts.len();
    let mut summaries = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        on_part(i + 1, n);
        let user = format!(
            "Dette er del {} av {n} av et langt møte. Lag referat bare for denne delen.\n\nTranskripsjon:\n\n{part}",
            i + 1
        );
        summaries.push(format!("# Del {}\n\n{}", i + 1, ollama::chat(url, model, num_ctx, &prompt, &user)?));
    }
    let user = format!(
        "Her er referater av hver del av et langt møte, i rekkefølge. Slå dem sammen til ett referat for hele møtet.\n\n{}",
        summaries.join("\n\n")
    );
    ollama::chat(url, model, num_ctx, &prompt, &user)
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
