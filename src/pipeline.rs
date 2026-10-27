//! Everything that happens after recording: transcribe tracks, merge, write the note.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::audio;
use crate::config::{Config, SummaryEngine};
use crate::diarize;
use crate::live::{self, Chunker};
use crate::meeting::{Meeting, Track};
use crate::note::{self, Summary};
use crate::names;
use crate::summarize::{self, Writer};
use crate::transcribe::Transcriber;
use crate::transcript::{Transcript, Who};
use crate::vad;

pub enum Event {
    LoadingModel,
    Diarizing(Track),
    /// That track keeps its "Meg"/"Andre" labels.
    DiarizationFailed(Track, String),
    Transcribing { track: Track, percent: i32 },
    SkippedSilentTrack(Track),
    /// `ollama_url` isn't on this machine: the transcript is about to leave it.
    SendingOffMachine(String),
    /// Looking for speakers' names in the transcript.
    FindingNames,
    NamesFound(Vec<String>),
    /// The note keeps "Taler N".
    NamesFailed(String),
    Summarizing { part: usize, parts: usize },
    /// How far the summary engine has got with the current request.
    SummaryProgress(summarize::Progress),
    /// The note is written without a summary, and the audio is kept so `process` can retry.
    SummaryFailed(String),
    /// Too few words to summarize without making things up: the note has the transcript only.
    TooShortToSummarize(usize),
    NoteWritten(PathBuf),
    AudioDeleted,
    /// How long a step took: "transcription", "speakers" or "names and summary".
    Took(&'static str, Duration),
}

pub type OnEvent = Arc<dyn Fn(Event) + Send + Sync>;

/// A track with less speech than this is left out: a cough or a click.
const MIN_SPEECH_SECONDS: f32 = 0.3;

/// Fewer words than this get a note with the transcript only: a summary of a few sentences
/// would be mostly made up.
const MIN_WORDS_TO_SUMMARIZE: usize = 30;

/// How much of the note to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Transcript, speakers, names and summary.
    Full,
    /// Only the transcript, with Me and Others: done in seconds once the live transcript is
    /// complete. The audio goes as after a full note (kept only with keep_audio or debug).
    TranscriptOnly,
}

/// Turns a recorded meeting into a note. With `reuse_live`, a complete live transcript is used
/// as is; otherwise everything is transcribed again from the saved audio. Returns the note's path.
pub fn process(meeting: &Meeting, config: &Config, reuse_live: bool, mode: Mode, on_event: OnEvent) -> Result<PathBuf> {
    let mut tracks = Vec::new();
    let mut found = false;
    for track in Track::ALL {
        let path = meeting.track_path(track);
        if !path.exists() {
            continue;
        }
        found = true;
        let samples = audio::read(&path)?;
        if vad::speech_seconds(&samples)? < MIN_SPEECH_SECONDS {
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

    let transcript = if reuse_live && meeting.live_complete {
        Transcript::merge(vec![live::read(&meeting.dir())?])
    } else {
        if found && tracks.is_empty() {
            bail!("no speech in meeting {} ('{}'): nothing to transcribe", meeting.id, meeting.title);
        }
        if tracks.is_empty() {
            bail!(
                "meeting {} has no audio in {} (audio is deleted after the note is written, or after 7 days with keep_audio or debug)",
                meeting.id,
                meeting.dir().display()
            );
        }
        on_event(Event::LoadingModel);
        let started = Instant::now();
        let mut transcriber = Transcriber::load(&config.whisper_model)?;
        // Same as live: chunks cut at pauses keep timestamps (and so speakers) accurate.
        let mut segments = Vec::new();
        for (track, samples) in &tracks {
            let mut chunker = Chunker::new(*track)?;
            let mut chunks = Vec::new();
            chunker.push(samples, &mut chunks);
            chunker.finish(&mut chunks);
            for (i, chunk) in chunks.iter().enumerate() {
                on_event(Event::Transcribing { track: *track, percent: (i * 100 / chunks.len()) as i32 });
                let offset_ms = chunk.start_sample * 1000 / audio::SAMPLE_RATE as u64;
                segments.push(transcriber.transcribe(&chunk.samples, offset_ms, track.who())?);
            }
            on_event(Event::Transcribing { track: *track, percent: 100 });
        }
        on_event(Event::Took("transcription", started.elapsed()));
        Transcript::merge(segments)
    };
    // Given next to nothing, the summary model invents a meeting. Nothing at all: no note.
    let words: usize = transcript.segments.iter().map(|s| s.text.split_whitespace().count()).sum();
    if words == 0 {
        bail!("nothing was said in '{}', so no note was written", meeting.title);
    }
    if mode == Mode::Full && words < MIN_WORDS_TO_SUMMARIZE {
        on_event(Event::TooShortToSummarize(words));
    }
    if mode == Mode::TranscriptOnly || words < MIN_WORDS_TO_SUMMARIZE {
        return write_note(&meeting, &transcript, None, true, config, &on_event);
    }

    // Tell speakers apart: the Others first, then any room full of people on the mic.
    let mut transcript = transcript;
    let mut others = 0;
    let started = Instant::now();
    for track in [Track::System, Track::Mic] {
        let Some((_, samples)) = tracks.iter().find(|(t, _)| *t == track) else { continue };
        on_event(Event::Diarizing(track));
        match diarize::diarize(samples, config.debug) {
            Ok(turns) if track == Track::System => others = diarize::label_others(&mut transcript.segments, &turns),
            Ok(turns) => diarize::label_mic(&mut transcript.segments, &turns, others + 1),
            Err(e) => on_event(Event::DiarizationFailed(track, format!("{e:#}"))),
        }
    }
    diarize::renumber(&mut transcript.segments);
    on_event(Event::Took("speakers", started.elapsed()));

    if config.summary_engine == SummaryEngine::Ollama && !config.ollama_is_local() {
        on_event(Event::SendingOffMachine(config.ollama_url.clone()));
    }
    // One language model for both: names first, so the summary can use them.
    let started = Instant::now();
    let summary = (|| -> Result<String> {
        let writer = Writer::open(config, &transcript)?;
        if transcript.people().iter().any(|w| matches!(w, Who::Speaker(_) | Who::Me)) {
            on_event(Event::FindingNames);
            match names::find(&writer, &transcript, config.debug) {
                Ok(found) => {
                    on_event(Event::NamesFound(found.values().map(|n| n.name.clone()).collect()));
                    transcript.set_names(found);
                }
                Err(e) => on_event(Event::NamesFailed(format!("{e:#}"))),
            }
        }
        on_event(Event::Summarizing { part: 1, parts: 1 });
        writer.summarize(&transcript, |part, parts| on_event(Event::Summarizing { part, parts }), &mut |p| on_event(Event::SummaryProgress(p)))
    })()
    .map(|mut text| {
        // The summary's title names the note, unless the meeting was given one when it started.
        if let Some(title) = note::take_title(&mut text)
            && note::has_default_title(&meeting)
        {
            meeting.title = title;
        }
        Summary { model: summarize::model_name(config).to_string(), text }
    })
    .inspect_err(|e| on_event(Event::SummaryFailed(format!("{e:#}"))))
    .ok();
    on_event(Event::Took("names and summary", started.elapsed()));
    // A failed summary keeps the audio, so `heylisten process` can retry it.
    let complete = summary.is_some();
    write_note(&meeting, &transcript, summary.as_ref(), complete, config, &on_event)
}

/// Saves the transcript, writes the note and records it on the meeting. Then deletes the audio
/// if the note is `complete`, unless it's kept anyway (keep_audio, debug), for at most
/// AUDIO_KEPT_DAYS.
fn write_note(meeting: &Meeting, transcript: &Transcript, summary: Option<&Summary>, complete: bool, config: &Config, on_event: &OnEvent) -> Result<PathBuf> {
    fs::write(meeting.dir().join("transcript.json"), serde_json::to_string_pretty(transcript)?)?;

    let model_name = config.whisper_model.file_stem().and_then(|s| s.to_str()).unwrap_or("whisper");
    let contents = note::render(meeting, transcript, model_name, summary);
    let note_path = note::write(&config.notes_dir, meeting, &contents)?;
    let mut done = meeting.clone();
    done.note = Some(note_path.clone());
    done.save()?;
    on_event(Event::NoteWritten(note_path.clone()));

    Meeting::prune_old_audio();
    if complete && !config.keep_audio && !config.debug {
        for file in crate::meeting::AUDIO_FILES {
            let _ = fs::remove_file(meeting.dir().join(file));
        }
        on_event(Event::AudioDeleted);
    }
    Ok(note_path)
}
