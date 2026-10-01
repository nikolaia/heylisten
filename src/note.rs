//! The Obsidian note: frontmatter + transcript. User-facing text is Norwegian.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::meeting::Meeting;
use crate::transcript::Transcript;

/// A finished summary and the short name of the model that wrote it.
pub struct Summary {
    pub model: String,
    pub text: String,
}

pub fn render(meeting: &Meeting, transcript: &Transcript, transcription_model: &str, summary: Option<&Summary>) -> String {
    let duration_min = ((meeting.end - meeting.start).num_seconds() as f64 / 60.0).round() as i64;
    let people: Vec<String> = transcript.people().into_iter().map(|w| w.label()).collect();

    let mut out = String::new();
    // Titles go through JSON quoting, which is valid YAML and survives colons and quotes.
    let _ = writeln!(out, "---");
    let _ = writeln!(out, "title: {}", serde_json::to_string(&meeting.title).unwrap());
    let _ = writeln!(out, "date: {}", meeting.start.format("%Y-%m-%d"));
    let _ = writeln!(out, "start: \"{}\"", meeting.start.format("%H:%M"));
    let _ = writeln!(out, "end: \"{}\"", meeting.end.format("%H:%M"));
    let _ = writeln!(out, "duration_min: {duration_min}");
    let _ = writeln!(out, "speakers: [{}]", people.join(", "));
    let _ = writeln!(out, "type: meeting");
    let _ = writeln!(out, "transcription_model: {transcription_model}");
    if let Some(summary) = summary {
        let _ = writeln!(out, "summary_model: {}", summary.model);
    }
    let _ = writeln!(out, "tags: [møte]");
    let _ = writeln!(out, "---\n");

    if let Some(summary) = summary {
        let _ = writeln!(out, "{}\n", summary.text.trim());
    }

    let _ = writeln!(out, "## Transkripsjon\n");
    for p in transcript.paragraphs() {
        let _ = writeln!(out, "**{}** [{}]\n{}\n", p.who.label(), timestamp(p.start_ms), p.text);
    }
    out
}

/// Writes `<vault>/<date> <title>.md`, never overwriting: adds ` (2)`, ` (3)`…
pub fn write(vault: &Path, meeting: &Meeting, contents: &str) -> Result<PathBuf> {
    fs::create_dir_all(vault)?;
    let title: String = meeting.title.chars().filter(|c| !r#"/\:*?"<>|"#.contains(*c)).collect();
    let base = format!("{} {}", meeting.start.format("%Y-%m-%d"), title.trim());
    let mut path = vault.join(format!("{base}.md"));
    let mut n = 2;
    while path.exists() {
        path = vault.join(format!("{base} ({n}).md"));
        n += 1;
    }
    fs::write(&path, contents)?;
    Ok(path)
}

fn timestamp(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{Segment, Who};
    use chrono::{Local, TimeZone};

    #[test]
    fn renders_frontmatter_and_transcript() {
        let meeting = Meeting {
            id: "x".into(),
            title: "Kundemøte: X".into(),
            start: Local.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap(),
            end: Local.with_ymd_and_hms(2026, 10, 1, 9, 47, 0).unwrap(),
            live_complete: false,
        };
        let transcript = Transcript::merge(vec![vec![
            Segment { start_ms: 4_000, end_ms: 6_000, who: Who::Me, text: "Hei.".into() },
            Segment { start_ms: 3_725_000, end_ms: 3_726_000, who: Who::Speaker(1), text: "Hallo.".into() },
        ]]);
        let summary = Summary { model: "borealis-12b".into(), text: "## Sammendrag\nKort.\n".into() };
        let note = render(&meeting, &transcript, "nb-whisper-large-q5_0", Some(&summary));
        assert!(note.contains("title: \"Kundemøte: X\"\n"));
        assert!(note.contains("duration_min: 47\n"));
        assert!(note.contains("speakers: [Meg, Taler 1]\n"));
        assert!(note.contains("summary_model: borealis-12b\n"));
        assert!(note.contains("---\n\n## Sammendrag\nKort.\n\n## Transkripsjon\n"));
        assert!(note.contains("**Meg** [00:00:04]\nHei.\n"));
        assert!(note.contains("**Taler 1** [01:02:05]\nHallo.\n"));
    }
}
