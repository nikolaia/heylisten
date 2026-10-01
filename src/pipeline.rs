//! Everything that happens after recording: transcribe tracks, merge, write the note.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};

use crate::audio;
use crate::config::Config;
use crate::diarize;
use crate::live::{self, Chunker};
use crate::meeting::{Meeting, Track};
use crate::note::{self, Summary};
use crate::summarize;
use crate::transcribe::Transcriber;
use crate::transcript::Transcript;

pub enum Event {
    LoadingModel,
    Diarizing(Track),
    /// That track keeps its "Meg"/"Andre" labels.
    DiarizationFailed(Track, String),
    Transcribing { track: Track, percent: i32 },
    SkippedSilentTrack(Track),
    /// `ollama_url` isn't on this machine: the transcript is about to leave it.
    SendingOffMachine(String),
    Summarizing { part: usize, parts: usize },
    /// The note is written without a summary, and the audio is kept so `process` can retry.
    SummaryFailed(String),
    NoteWritten(PathBuf),
    AudioDeleted,
}

pub type OnEvent = Arc<dyn Fn(Event) + Send + Sync>;

/// Turns a recorded meeting into a note. With `reuse_live`, a complete live transcript is used
/// as is; otherwise everything is transcribed again from the saved audio. Returns the note's path.
pub fn process(meeting: &Meeting, config: &Config, reuse_live: bool, on_event: OnEvent) -> Result<PathBuf> {
    let mut tracks = Vec::new();
    for track in Track::ALL {
        let path = meeting.track_path(track);
        if !path.exists() {
            continue;
        }
        let samples = audio::read(&path)?;
        if !audio::has_speech(&samples) {
            on_event(Event::SkippedSilentTrack(track));
            continue;
        }
        tracks.push((track, samples));
    }
    // A recorder that died never wrote its end time: take it from the audio.
    let mut meeting = meeting.clone();
    if meeting.end <= meeting.start {
        let longest = tracks.iter().map(|(_, s)| s.len()).max().unwrap_or(0);
        meeting.end = meeting.start + chrono::Duration::milliseconds((longest as i64 * 1000) / audio::SAMPLE_RATE as i64);
        meeting.save()?;
    }
    let meeting = &meeting;

    let transcript = if reuse_live && meeting.live_complete {
        Transcript::merge(vec![live::read(&meeting.dir())?])
    } else {
        if tracks.is_empty() {
            bail!(
                "meeting {} has no audio in {} (audio is deleted after the note is written unless keep_audio = true)",
                meeting.id,
                meeting.dir().display()
            );
        }
        on_event(Event::LoadingModel);
        let mut transcriber = Transcriber::load(&config.whisper_model)?;
        // Same as live: chunks cut at pauses keep timestamps (and so speakers) accurate.
        let mut segments = Vec::new();
        for (track, samples) in &tracks {
            let mut chunker = Chunker::new(*track);
            let mut chunks = Vec::new();
            chunker.push(samples, &mut chunks);
            chunker.finish(&mut chunks);
            for (i, chunk) in chunks.iter().enumerate() {
                on_event(Event::Transcribing { track: *track, percent: (i * 100 / chunks.len()) as i32 });
                let offset_ms = chunk.start_sample * 1000 / audio::SAMPLE_RATE as u64;
                segments.push(transcriber.transcribe(&chunk.samples, offset_ms, track.who(), |_| {})?);
            }
            on_event(Event::Transcribing { track: *track, percent: 100 });
        }
        Transcript::merge(segments)
    };
    // Tell speakers apart: the Others first, then any room full of people on the mic.
    let mut transcript = transcript;
    let mut others = 0;
    for track in [Track::System, Track::Mic] {
        let Some((_, samples)) = tracks.iter().find(|(t, _)| *t == track) else { continue };
        on_event(Event::Diarizing(track));
        match diarize::diarize(samples) {
            Ok(turns) if track == Track::System => others = diarize::label_others(&mut transcript.segments, &turns),
            Ok(turns) => diarize::label_mic(&mut transcript.segments, &turns, others + 1),
            Err(e) => on_event(Event::DiarizationFailed(track, format!("{e:#}"))),
        }
    }
    diarize::renumber(&mut transcript.segments);
    fs::write(meeting.dir().join("transcript.json"), serde_json::to_string_pretty(&transcript)?)?;

    if !config.ollama_is_local() {
        on_event(Event::SendingOffMachine(config.ollama_url.clone()));
    }
    on_event(Event::Summarizing { part: 1, parts: 1 });
    let summary = summarize::summarize(config, &transcript, |part, parts| on_event(Event::Summarizing { part, parts }))
        .map(|text| Summary { model: summarize::short_name(&config.ollama_model).to_string(), text })
        .inspect_err(|e| on_event(Event::SummaryFailed(format!("{e:#}"))))
        .ok();

    let model_name = config.whisper_model.file_stem().and_then(|s| s.to_str()).unwrap_or("whisper");
    let contents = note::render(meeting, &transcript, model_name, summary.as_ref());
    let note_path = note::write(&config.notes_dir, meeting, &contents)?;
    let mut done = meeting.clone();
    done.note = Some(note_path.clone());
    done.save()?;
    on_event(Event::NoteWritten(note_path.clone()));

    if !config.keep_audio && summary.is_some() {
        for track in Track::ALL {
            let _ = fs::remove_file(meeting.track_path(track));
        }
        on_event(Event::AudioDeleted);
    }
    Ok(note_path)
}
