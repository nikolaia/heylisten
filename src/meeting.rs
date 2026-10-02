use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Local};
use serde::{Deserialize, Serialize};

use crate::audio;
use crate::config::{AUDIO_KEPT_DAYS, meetings_dir};
use crate::transcript::Who;

/// One meeting and its folder: `<meetings_dir>/<id>/` holding `meeting.json`, track WAVs and `transcript.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    /// The recorder transcribed every chunk live, so the transcript doesn't need redoing.
    #[serde(default)]
    pub live_complete: bool,
    /// The note written for this meeting, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    Mic,
    System,
}

impl Track {
    pub const ALL: [Track; 2] = [Track::Mic, Track::System];

    pub fn file_name(self) -> &'static str {
        match self {
            Track::Mic => "mic.wav",
            Track::System => "system.wav",
        }
    }

    pub fn who(self) -> Who {
        match self {
            Track::Mic => Who::Me,
            Track::System => Who::Others,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Track::Mic => "mic",
            Track::System => "system",
        }
    }
}

impl Meeting {
    pub fn dir(&self) -> PathBuf {
        meetings_dir().join(&self.id)
    }

    pub fn track_path(&self, track: Track) -> PathBuf {
        self.dir().join(track.file_name())
    }

    pub fn load(id: &str) -> Result<Meeting> {
        let path = meetings_dir().join(id).join("meeting.json");
        let text = fs::read_to_string(&path)
            .with_context(|| format!("no meeting or file named '{id}' (meetings live in {})", meetings_dir().display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self) -> Result<()> {
        fs::write(self.dir().join("meeting.json"), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Creates a meeting from an existing audio file. Everyone in it counts as Others.
    pub fn import(file: &Path) -> Result<Meeting> {
        let title = file.file_stem().and_then(|s| s.to_str()).unwrap_or("Opptak").to_string();
        // A file's modification time is roughly when the recording ended.
        let end: DateTime<Local> = fs::metadata(file)?.modified()?.into();
        let mut meeting = Meeting::create(title, end)?;
        let seconds = audio::import(file, &meeting.track_path(Track::System)).inspect_err(|_| {
            let _ = fs::remove_dir_all(meeting.dir());
        })?;
        meeting.start = end - Duration::milliseconds((seconds * 1000.0) as i64);
        meeting.save()?;
        Ok(meeting)
    }

    /// The newest notes that still exist, newest first.
    pub fn recent_notes(limit: usize) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(meetings_dir()) else { return Vec::new() };
        let mut meetings: Vec<Meeting> = entries
            .flatten()
            .filter_map(|e| fs::read_to_string(e.path().join("meeting.json")).ok())
            .filter_map(|text| serde_json::from_str(&text).ok())
            .collect();
        meetings.sort_by_key(|m| std::cmp::Reverse(m.start));
        meetings.into_iter().filter_map(|m| m.note).filter(|n| n.is_file()).take(limit).collect()
    }

    /// Deletes the audio of meetings that ended more than `AUDIO_KEPT_DAYS` ago. Transcripts
    /// and notes stay. Returns how many meetings had audio deleted.
    pub fn prune_old_audio() -> usize {
        prune_audio_in(&meetings_dir(), Local::now() - Duration::days(AUDIO_KEPT_DAYS))
    }

    pub fn create(title: String, start: DateTime<Local>) -> Result<Meeting> {
        let base = start.format("%Y-%m-%d-%H%M%S").to_string();
        for n in 1..100 {
            let id = if n == 1 { base.clone() } else { format!("{base}-{n}") };
            let meeting = Meeting { id, title: title.clone(), start, end: start, live_complete: false, note: None };
            if !meeting.dir().exists() {
                fs::create_dir_all(meeting.dir())?;
                meeting.save()?;
                return Ok(meeting);
            }
        }
        bail!("too many meetings starting at {base}")
    }
}

/// Every audio file a meeting folder can hold: the tracks, and in debug mode the mic from before
/// echo cancellation.
pub const AUDIO_FILES: [&str; 3] = ["mic.wav", "system.wav", "mic-raw.wav"];

/// Deletes the audio of every meeting in `dir` that ended before `cutoff`.
fn prune_audio_in(dir: &Path, cutoff: DateTime<Local>) -> usize {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    let mut pruned = 0;
    for entry in entries.flatten() {
        let Ok(text) = fs::read_to_string(entry.path().join("meeting.json")) else { continue };
        let Ok(meeting) = serde_json::from_str::<Meeting>(&text) else { continue };
        // A meeting that never got an end time (the recorder died) counts from its start.
        if meeting.end.max(meeting.start) >= cutoff {
            continue;
        }
        let mut deleted = false;
        for file in AUDIO_FILES {
            deleted |= fs::remove_file(entry.path().join(file)).is_ok();
        }
        pruned += deleted as usize;
    }
    pruned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prunes_only_audio_older_than_the_cutoff() {
        let dir = std::env::temp_dir().join(format!("heylisten-prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let now = Local::now();
        for (id, ended) in [("old", now - Duration::days(8)), ("new", now - Duration::days(2))] {
            let m = dir.join(id);
            fs::create_dir_all(&m).unwrap();
            let meeting = Meeting { id: id.into(), title: id.into(), start: ended, end: ended, live_complete: true, note: None };
            fs::write(m.join("meeting.json"), serde_json::to_string(&meeting).unwrap()).unwrap();
            fs::write(m.join("mic.wav"), b"x").unwrap();
            fs::write(m.join("mic-raw.wav"), b"x").unwrap();
            fs::write(m.join("transcript.json"), b"{}").unwrap();
        }
        assert_eq!(prune_audio_in(&dir, now - Duration::days(AUDIO_KEPT_DAYS)), 1);
        assert!(!dir.join("old/mic.wav").exists());
        assert!(!dir.join("old/mic-raw.wav").exists());
        assert!(dir.join("old/transcript.json").exists(), "transcripts stay");
        assert!(dir.join("new/mic.wav").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
