use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Local};
use serde::{Deserialize, Serialize};

use crate::audio;
use crate::config::meetings_dir;
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

    pub fn create(title: String, start: DateTime<Local>) -> Result<Meeting> {
        let base = start.format("%Y-%m-%d-%H%M%S").to_string();
        for n in 1..100 {
            let id = if n == 1 { base.clone() } else { format!("{base}-{n}") };
            let meeting = Meeting { id, title: title.clone(), start, end: start, live_complete: false };
            if !meeting.dir().exists() {
                fs::create_dir_all(meeting.dir())?;
                meeting.save()?;
                return Ok(meeting);
            }
        }
        bail!("too many meetings starting at {base}")
    }
}
