//! The live transcript: while recording, each track is cut into chunks of speech at pauses,
//! and one transcriber thread appends their segments to `live.jsonl` in the meeting folder.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use anyhow::Result;

use crate::audio::{self, FRAME, SAMPLE_RATE, SPEECH_RMS};
use crate::meeting::Track;
use crate::transcribe::Transcriber;
use crate::transcript::Segment;

const SECOND: usize = SAMPLE_RATE as usize;
/// A chunk ends at a pause once it's at least this long. Short, so a chunk rarely holds two
/// speakers.
const MIN_CHUNK: usize = SECOND;
/// Cut by here even without a pause. NB-Whisper skips whole sentences in longer chunks.
const MAX_CHUNK: usize = SECOND * 12;
/// How far back from MAX_CHUNK to look for the quietest moment to cut at.
const CUT_WINDOW: usize = SECOND * 4;

/// The pause that ends a chunk shrinks as the chunk grows: half a second while it's short
/// (a real pause in conversation), down to a breath once it's long. Natural speech has those
/// often, so the forced cut at MAX_CHUNK is rare.
fn pause_needed(chunk_len: usize) -> usize {
    match chunk_len {
        n if n < SECOND * 5 => SECOND / 2,
        n if n < SECOND * 9 => SECOND * 3 / 10,
        _ => SECOND * 3 / 20,
    }
}

pub struct Chunk {
    pub track: Track,
    pub start_sample: u64,
    pub samples: Vec<f32>,
}

/// Cuts one track's 16 kHz audio into chunks of speech, dropping long silences.
pub struct Chunker {
    track: Track,
    buf: Vec<f32>,
    /// Absolute sample index of `buf[0]`.
    start: u64,
    /// How much of `buf` has been looked at, in whole frames.
    analyzed: usize,
    /// Trailing quiet samples.
    quiet: usize,
}

impl Chunker {
    pub fn new(track: Track) -> Chunker {
        Chunker { track, buf: Vec::new(), start: 0, analyzed: 0, quiet: 0 }
    }

    /// `samples` must follow on directly from the previous push.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<Chunk>) {
        self.buf.extend_from_slice(samples);
        while self.analyzed + FRAME <= self.buf.len() {
            let loud = audio::rms(&self.buf[self.analyzed..self.analyzed + FRAME]) > SPEECH_RMS / 2.0;
            self.analyzed += FRAME;
            self.quiet = if loud { 0 } else { self.quiet + FRAME };

            if self.quiet == self.analyzed && self.quiet >= SECOND / 2 {
                // Nothing but silence so far: throw it away.
                self.cut(self.analyzed, false, out);
            } else if self.analyzed >= MIN_CHUNK && self.quiet >= pause_needed(self.analyzed) {
                self.cut(self.analyzed, true, out);
            } else if self.analyzed >= MAX_CHUNK {
                // No pause at all: cut at the quietest moment near the end, usually between words.
                let at = audio::quietest_frame(&self.buf, self.analyzed - CUT_WINDOW, self.analyzed) + FRAME;
                self.cut(at, true, out);
            }
        }
    }

    pub fn finish(&mut self, out: &mut Vec<Chunk>) {
        self.cut(self.buf.len(), true, out);
    }

    fn cut(&mut self, at: usize, emit: bool, out: &mut Vec<Chunk>) {
        let mut samples: Vec<f32> = self.buf.drain(..at).collect();
        if emit && audio::has_speech(&samples) {
            // Whisper fills leading silence with made-up words: start just before the speech.
            let first_loud = samples.chunks(FRAME).position(|f| audio::rms(f) > SPEECH_RMS / 2.0).unwrap_or(0) * FRAME;
            let skip = first_loud.saturating_sub(SECOND / 10);
            samples.drain(..skip);
            out.push(Chunk { track: self.track, start_sample: self.start + skip as u64, samples });
        }
        self.start += at as u64;
        self.analyzed -= at.min(self.analyzed);
        // Whatever's left after the cut point is still unread speech or silence; count it again.
        self.quiet = 0;
    }
}

pub fn live_path(meeting_dir: &Path) -> PathBuf {
    meeting_dir.join("live.jsonl")
}

/// The transcriber thread. Returns the sender to feed chunks to; dropping every sender
/// lets it finish the queue and exit. The thread returns Ok(()) if every chunk was transcribed.
pub fn spawn(model: PathBuf, meeting_dir: PathBuf) -> Result<(mpsc::Sender<Chunk>, JoinHandle<Result<()>>)> {
    let (tx, rx) = mpsc::channel::<Chunk>();
    let mut file = OpenOptions::new().create(true).append(true).open(live_path(&meeting_dir))?;
    let handle = thread::spawn(move || {
        background_priority();
        let mut transcriber = Transcriber::load(&model)?;
        for chunk in rx {
            let offset_ms = chunk.start_sample * 1000 / SAMPLE_RATE as u64;
            for segment in transcriber.transcribe(&chunk.samples, offset_ms, chunk.track.who())? {
                writeln!(file, "{}", serde_json::to_string(&segment)?)?;
            }
            file.flush()?;
        }
        Ok(())
    });
    Ok((tx, handle))
}

/// Live transcription runs during video calls: give it a lower priority than the call. Audio
/// capture runs on its own real-time threads and isn't affected.
fn background_priority() {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
    #[cfg(not(target_os = "macos"))]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10); // on Linux this affects only this thread
    }
}

/// Reads the live transcript's segments, in the order they were written.
pub fn read(meeting_dir: &Path) -> Result<Vec<Segment>> {
    let file = File::open(live_path(meeting_dir))?;
    BufReader::new(file)
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}

/// Reads segments appended since `pos` bytes; returns them and the new position.
/// A half-written last line is left for next time.
pub fn read_from(meeting_dir: &Path, pos: u64) -> Result<(Vec<Segment>, u64)> {
    let text = match fs::read(live_path(meeting_dir)) {
        Ok(bytes) => bytes,
        Err(_) => return Ok((Vec::new(), pos)),
    };
    let new = &text[pos.min(text.len() as u64) as usize..];
    let complete = new.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let segments = std::str::from_utf8(&new[..complete])?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Segment>, _>>()?;
    Ok((segments, pos + complete as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f32) -> Vec<f32> {
        (0..(seconds * SAMPLE_RATE as f32) as usize).map(|i| (i as f32 * 0.05).sin() * 0.3).collect()
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (seconds * SAMPLE_RATE as f32) as usize]
    }

    #[test]
    fn cuts_at_pauses_and_drops_silence() {
        let mut c = Chunker::new(Track::Mic);
        let mut out = Vec::new();
        for part in [silence(5.0), tone(4.0), silence(1.0), tone(2.0), silence(0.2), tone(2.0)] {
            for piece in part.chunks(1000) {
                c.push(piece, &mut out);
            }
        }
        c.finish(&mut out);
        assert_eq!(out.len(), 2);
        // The leading silence is dropped, so the first chunk starts near 5 s.
        let first_start = out[0].start_sample as f32 / SAMPLE_RATE as f32;
        assert!((4.0..=5.0).contains(&first_start), "{first_start}");
        // The short pause doesn't split the second chunk.
        assert!(out[1].samples.len() >= 4 * SAMPLE_RATE as usize);
    }

    #[test]
    fn cuts_long_monologues() {
        let mut c = Chunker::new(Track::System);
        let mut out = Vec::new();
        c.push(&tone(60.0), &mut out);
        c.finish(&mut out);
        assert!(out.len() >= 5, "{} chunks", out.len());
        assert!(out.iter().all(|ch| ch.samples.len() <= MAX_CHUNK));
        // Nothing lost: the chunks cover all 60 s.
        assert_eq!(out.iter().map(|ch| ch.samples.len()).sum::<usize>(), 60 * SECOND);
    }

    #[test]
    fn cuts_monologues_at_the_quietest_moment() {
        // 11 s of speech, a 0.2 s dip (too short to count as a pause), then more speech.
        let mut audio = tone(10.0);
        audio.extend(silence(0.2));
        audio.extend(tone(8.0));
        let mut c = Chunker::new(Track::System);
        let mut out = Vec::new();
        c.push(&audio, &mut out);
        c.finish(&mut out);
        let first = out[0].samples.len() as f32 / SAMPLE_RATE as f32;
        assert!((10.0..=10.25).contains(&first), "cut at {first} s");
    }
}
