//! Finds speakers' names in one meeting's transcript ("dette er Kari", "Kari, kan du…?",
//! "Takk, Kari"). Only for that meeting: nothing about anyone is kept.
//!
//! The work is split by what each part is good at. Plain code finds the candidates and works out
//! which speaker each one refers to; that's bookkeeping, and a 12B model got it wrong every time
//! we tried. The language model only answers which candidate words are people's names.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::Deserialize;

use crate::summarize::Writer;
use crate::transcript::{Naming, Paragraph, Transcript, Who};

const PROMPT: &str = r#"Du får noen ord fra en transkripsjon av et norsk møte. Hvilke av dem er navn på personer (fornavn eller etternavn)? Ord som «Bra», «Ok», «Hei» eller «Alle» er ikke navn.

Svar bare med JSON på formen {"navn": ["Kari", "Ola"]}. Bruk en tom liste hvis ingen er navn."#;

/// Words that start sentences or follow "takk" without being names.
const NOT_NAMES: &[&str] = &[
    "alle", "alt", "bare", "bra", "da", "dere", "det", "du", "ellers", "en", "enig", "fint", "flott", "god", "godt", "greit", "ha",
    "hallo", "hei", "her", "ingen", "ja", "jaha", "jeg", "jo", "men", "mange", "morn", "nei", "nok", "nå", "og", "ok", "okei",
    "også", "så", "supert", "takk", "uansett", "vel", "vi", "yes", "ålreit",
];

/// A possible name for a speaker, and the sentence it comes from.
#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    speaker: u32,
    name: String,
    evidence: String,
}

/// Names for the transcript's speakers, where the transcript makes them clear.
/// `debug` logs the candidates and the model's answer.
pub fn find(writer: &Writer, transcript: &Transcript, debug: bool) -> Result<BTreeMap<u32, Naming>> {
    let candidates = candidates(&transcript.paragraphs());
    if candidates.is_empty() {
        return Ok(BTreeMap::new());
    }
    let words: BTreeSet<&str> = candidates.iter().map(|c| c.name.as_str()).collect();
    let words: Vec<&str> = words.into_iter().collect();
    let reply = writer.chat(PROMPT, &format!("Ordene: {}", words.join(", ")), true)?;
    if debug {
        eprintln!("names: candidates {candidates:?}; the model said {reply}");
    }
    Ok(assign(&candidates, &people(&reply)))
}

/// The names the model confirmed, lowercased.
fn people(reply: &str) -> BTreeSet<String> {
    #[derive(Deserialize)]
    struct Answer {
        #[serde(default)]
        navn: Vec<String>,
    }
    serde_json::from_str::<Answer>(reply).map(|a| a.navn.into_iter().map(|n| n.trim().to_lowercase()).collect()).unwrap_or_default()
}

/// Every place the transcript shows a speaker's name:
/// - an introduction in their own words: "dette er Kari", "jeg heter Kari", "Kari her"
/// - being addressed: a sentence starting "Kari, …" names the next speaker
/// - being thanked: "Takk, Kari" names the previous speaker
fn candidates(paragraphs: &[Paragraph]) -> Vec<Candidate> {
    let speaker = |who: Option<Who>| match who {
        Some(Who::Speaker(n)) => Some(n),
        _ => None,
    };
    let mut out = Vec::new();
    for (i, p) in paragraphs.iter().enumerate() {
        let own = speaker(Some(p.who));
        let next = speaker(paragraphs.get(i + 1).map(|q| q.who)).filter(|n| Some(*n) != own);
        let previous = speaker(i.checked_sub(1).map(|j| paragraphs[j].who)).filter(|n| Some(*n) != own);
        for sentence in p.text.split(['.', '?', '!']).map(str::trim).filter(|s| !s.is_empty()) {
            let tokens: Vec<&str> = sentence.split_whitespace().collect();
            let words: Vec<String> = tokens.iter().map(|t| t.trim_matches(|c: char| !c.is_alphabetic()).to_string()).collect();
            let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
            let mut found = |speaker: Option<u32>, word: &str| {
                if let Some(speaker) = speaker
                    && could_be_a_name(word)
                {
                    out.push(Candidate { speaker, name: word.to_string(), evidence: sentence.to_string() });
                }
            };
            // Introductions: "dette er X", "jeg heter X", "jeg er X", "mitt navn er X", "X her".
            for k in 0..lower.len() {
                let after = |phrase: &[&str]| {
                    lower[k..].starts_with(&phrase.iter().map(|w| w.to_string()).collect::<Vec<_>>()).then(|| words.get(k + phrase.len()))
                };
                for phrase in [&["dette", "er"][..], &["jeg", "heter"], &["jeg", "er"], &["mitt", "navn", "er"]] {
                    if let Some(Some(word)) = after(phrase) {
                        found(own, word);
                    }
                }
            }
            if lower.len() >= 2 && lower[1] == "her" {
                found(own, &words[0]);
            }
            // Addressed: "X, …" at the start of the sentence.
            if tokens.first().is_some_and(|t| t.ends_with(',')) {
                found(next, &words[0]);
            }
            // Thanked: "takk, X" / "takk X".
            for k in 0..lower.len().saturating_sub(1) {
                if lower[k] == "takk" {
                    found(previous, &words[k + 1]);
                }
            }
        }
    }
    out
}

/// Capitalized, a word, and not one of the usual sentence starters.
fn could_be_a_name(word: &str) -> bool {
    word.chars().count() >= 2
        && word.chars().next().is_some_and(char::is_uppercase)
        && word.chars().all(char::is_alphabetic)
        && !NOT_NAMES.contains(&word.to_lowercase().as_str())
}

/// Gives a speaker a name when every confirmed candidate for them agrees, and the name isn't
/// claimed by another speaker too.
fn assign(candidates: &[Candidate], people: &BTreeSet<String>) -> BTreeMap<u32, Naming> {
    let mut by_speaker: BTreeMap<u32, Vec<&Candidate>> = BTreeMap::new();
    for c in candidates.iter().filter(|c| people.contains(&c.name.to_lowercase())) {
        by_speaker.entry(c.speaker).or_default().push(c);
    }
    let mut names: BTreeMap<u32, Naming> = BTreeMap::new();
    for (speaker, found) in by_speaker {
        let distinct: BTreeSet<String> = found.iter().map(|c| c.name.to_lowercase()).collect();
        if distinct.len() == 1 {
            names.insert(speaker, Naming { name: found[0].name.clone(), evidence: found[0].evidence.clone() });
        }
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for naming in names.values() {
        *seen.entry(naming.name.to_lowercase()).or_default() += 1;
    }
    names.retain(|_, naming| seen[&naming.name.to_lowercase()] == 1);
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::Segment;

    /// Taler 1 hosts; Taler 2 is Kari (addressed, introduces herself, thanked); Taler 3 is Ola
    /// (addressed, then answers).
    fn meeting() -> Vec<Paragraph> {
        let lines = [
            (1, "Hei alle sammen, og velkommen til statusmøtet. Kari, kan du starte med testingen?"),
            (2, "Ja, dette er Kari, testingen er nesten ferdig."),
            (1, "Takk, Kari. Ola, hvordan går det med prisene?"),
            (3, "Prisene er oppdatert."),
            (1, "Flott, takk Ola. Bra, da ses vi neste uke."),
        ];
        let segments = lines
            .iter()
            .enumerate()
            .map(|(i, (n, text))| Segment { start_ms: i as u64 * 1000, end_ms: i as u64 * 1000 + 900, who: Who::Speaker(*n), text: text.to_string() })
            .collect();
        Transcript { segments, ..Default::default() }.paragraphs()
    }

    fn confirmed(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_lowercase()).collect()
    }

    #[test]
    fn finds_who_is_who_from_the_patterns() {
        let names = assign(&candidates(&meeting()), &confirmed(&["Kari", "Ola"]));
        assert_eq!(names[&2].name, "Kari");
        assert_eq!(names[&3].name, "Ola");
        assert!(!names.contains_key(&1), "the host never says her own name");
    }

    #[test]
    fn candidates_skip_sentence_starters() {
        let words: BTreeSet<String> = candidates(&meeting()).into_iter().map(|c| c.name).collect();
        assert_eq!(words, ["Kari".to_string(), "Ola".to_string()].into());
    }

    #[test]
    fn only_names_the_model_confirms() {
        assert!(assign(&candidates(&meeting()), &confirmed(&[])).is_empty());
        let names = assign(&candidates(&meeting()), &confirmed(&["Ola"]));
        assert_eq!(names.len(), 1);
        assert_eq!(names[&3].name, "Ola");
    }

    #[test]
    fn conflicting_names_leave_a_speaker_unnamed() {
        let mut found = candidates(&meeting());
        found.push(Candidate { speaker: 2, name: "Per".into(), evidence: "Per, vil du si noe".into() });
        let names = assign(&found, &confirmed(&["Kari", "Ola", "Per"]));
        assert!(!names.contains_key(&2));
        assert_eq!(names[&3].name, "Ola");
    }

    #[test]
    fn reads_the_models_answer() {
        assert_eq!(people(r#"{"navn": ["Kari", " Ola "]}"#), confirmed(&["Kari", "Ola"]));
        assert!(people("Kari og Ola").is_empty());
    }
}
