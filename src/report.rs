//! A problem report: a draft GitHub issue with what helps to debug heyListen, and nothing from
//! the meetings themselves. heyListen never sends it. It opens in the browser, where the person
//! reads it, edits it, and decides whether to submit it.

use std::fs;
use std::process::Command;

use crate::config::{Config, data_dir, meetings_dir};

const NEW_ISSUE: &str = "https://github.com/nikolaia/heylisten/issues/new";
/// Browsers and GitHub handle URLs up to about 8 KB; the logs are shortened to fit.
const MAX_URL: usize = 7_500;
/// What the report contains, for the consent prompt.
pub const CONTENTS: &str = "your heyListen and macOS versions, your Mac model, a few settings, and the last \
    lines of heyListen's logs. Names, meeting titles and your home folder are removed from the logs; \
    no audio, transcripts or notes are included";

pub struct Report {
    pub title: String,
    pub body: String,
}

/// Gathers the report, with the logs redacted and trimmed to fit.
pub fn draft(config: &Config) -> Report {
    let system = system_info();
    let settings = format!(
        "summary engine: {:?}, debug: {}, keep audio: {}",
        config.summary_engine, config.debug, config.keep_audio
    );
    let recorder = latest_recorder_log().unwrap_or_default();
    let tray = fs::read_to_string(data_dir().join("tray.log")).unwrap_or_default();
    fit(&system, &settings, lines(&redact(&recorder)), lines(&redact(&tray)))
}

/// The report with as many of the newest log lines as fit in the issue URL.
fn fit(system: &str, settings: &str, mut recorder: Vec<String>, mut tray: Vec<String>) -> Report {
    loop {
        let report = Report { title: "Problem: ".into(), body: body(system, settings, &recorder, &tray) };
        if issue_url(&report).len() <= MAX_URL || (recorder.is_empty() && tray.is_empty()) {
            return report;
        }
        // Drop the oldest line of the longer log.
        if recorder.len() >= tray.len() { recorder.remove(0) } else { tray.remove(0) };
    }
}

/// The draft as a "new issue" link, with the title and body filled in.
pub fn issue_url(report: &Report) -> String {
    format!("{NEW_ISSUE}?title={}&body={}", encode(&report.title), encode(&report.body))
}

fn body(system: &str, settings: &str, recorder: &[String], tray: &[String]) -> String {
    let log = |lines: &[String]| if lines.is_empty() { "(empty)".to_string() } else { lines.join("\n") };
    format!(
        "> [!WARNING]\n\
         > **Review this before you submit it: it will be public.** Check that it contains no personal or \
         sensitive information: no names, meeting titles, customers, file paths or anything from a meeting. \
         heyListen removed what it recognizes, but it can miss things. Delete anything you're unsure of.\n\n\
         **What happened?**\n\n<!-- What did you do, what did you expect, and what happened instead? -->\n\n\
         **System:** {system}\n**Settings:** {settings}\n\n\
         <details><summary>Recorder log (newest meeting)</summary>\n\n```\n{}\n```\n</details>\n\n\
         <details><summary>Menu bar app log</summary>\n\n```\n{}\n```\n</details>\n\n\
         _Drafted by `heylisten report`._\n",
        log(recorder),
        log(tray)
    )
}

fn system_info() -> String {
    let run = |cmd: &str, args: &[&str]| {
        Command::new(cmd).args(args).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())
    };
    let os = if cfg!(target_os = "macos") {
        run("sw_vers", &["-productVersion"]).map(|v| format!("macOS {v}"))
    } else {
        run("uname", &["-sr"])
    };
    let model = run("sysctl", &["-n", "hw.model"]);
    let chip = run("sysctl", &["-n", "machdep.cpu.brand_string"]);
    [Some(format!("heyListen {}", env!("CARGO_PKG_VERSION"))), os, model, chip].into_iter().flatten().collect::<Vec<_>>().join(", ")
}

/// The newest meeting's recorder.log. Meeting ids are timestamps, so the newest sorts last.
fn latest_recorder_log() -> Option<String> {
    let mut ids: Vec<_> = fs::read_dir(meetings_dir()).ok()?.flatten().map(|e| e.path()).filter(|p| p.join("recorder.log").is_file()).collect();
    ids.sort();
    fs::read_to_string(ids.last()?.join("recorder.log")).ok()
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

/// Removes what could identify people or meetings: lines about names, a name before "'s"
/// (device names like "Kari's AirPods"), anything in single quotes (meeting titles in errors),
/// and the home folder.
pub fn redact(text: &str) -> String {
    let home = dirs::home_dir().map(|h| h.display().to_string()).filter(|h| h.len() > 1);
    text.lines()
        .filter(|line| !line.contains("names:") && !line.contains("candidates"))
        .map(|line| {
            let line = home.as_deref().map_or_else(|| line.to_string(), |h| line.replace(h, "~"));
            unquote(&line.split(' ').map(possessive).collect::<Vec<_>>().join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// "Kari's" → "‹name›'s".
fn possessive(word: &str) -> String {
    for mark in ["’s", "'s"] {
        if let Some(i) = word.find(mark)
            && i > 0
            && word[..i].chars().all(char::is_alphabetic)
        {
            return format!("‹name›{}", &word[i..]);
        }
    }
    word.to_string()
}

/// "already recording 'Kundemøte' (id)" → "already recording '‹title›' (id)". A quote opens
/// after a space or at the start, and closes before a space, punctuation or the end.
fn unquote(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let opens = chars[i] == '\'' && (i == 0 || chars[i - 1].is_whitespace());
        let close = opens.then(|| (i + 1..chars.len()).find(|&j| chars[j] == '\'' && chars.get(j + 1).is_none_or(|c| !c.is_alphanumeric()))).flatten();
        match close {
            Some(j) => {
                out.push_str("'‹title›'");
                i = j + 1;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// Percent-encodes everything but unreserved characters, for a URL query value.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Opens the draft in the default browser.
pub fn open(report: &Report) -> std::io::Result<()> {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    Command::new(opener).arg(issue_url(report)).status().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_names_titles_and_name_lines() {
        let log = "[08:46:19] mic: Nikolai’s AirPods Pro (24000 Hz, 1 ch)\n\
                   start failed: already recording 'Kundemøte med Kari' (2026-10-23-121534). Stop it first\n\
                   names: candidates [Kari]\n\
                   System: 1.0s in, received 0.42s of audio at 48000 Hz";
        assert_eq!(
            redact(log),
            "[08:46:19] mic: ‹name›’s AirPods Pro (24000 Hz, 1 ch)\n\
             start failed: already recording '‹title›' (2026-10-23-121534). Stop it first\n\
             System: 1.0s in, received 0.42s of audio at 48000 Hz"
        );
    }

    #[test]
    fn a_long_log_is_shortened_to_its_newest_lines() {
        let long: Vec<String> = (0..2000).map(|i| format!("Mic: {i}.0s in, received audio")).collect();
        let report = fit("macOS", "debug: true", long, vec!["start failed".into()]);
        assert!(issue_url(&report).len() <= MAX_URL);
        assert!(report.body.contains("Mic: 1999.0s in"), "the newest line stays");
        assert!(!report.body.contains("Mic: 0.0s in"), "the oldest goes");
        assert!(report.body.contains("start failed"));
    }

    #[test]
    fn encodes_for_a_url() {
        assert_eq!(encode("a b/å"), "a%20b%2F%C3%A5");
    }

    #[test]
    fn starts_with_the_review_warning() {
        let report = fit("macOS", "debug: true", vec![], vec![]);
        assert!(report.body.starts_with("> [!WARNING]\n> **Review this before you submit it"));
        assert!(report.body.contains("no personal or sensitive information"));
    }
}
