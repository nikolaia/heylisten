//! Diarization: tells the Others apart. Runs on the system track after the meeting.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use sherpa_rs::diarize::{Diarize, DiarizeConfig};

use crate::config::meetings_dir;
use crate::transcript::{Segment, Who};

const SEGMENTATION: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/segmentation.onnx"));
const EMBEDDING: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/embedding.onnx"));

/// One stretch of time where a single speaker talks.
pub struct Turn {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: i32,
}

/// Finds who speaks when in 16 kHz mono audio. The number of speakers is detected.
pub fn diarize(samples: &[f32]) -> Result<Vec<Turn>> {
    let config = DiarizeConfig {
        num_clusters: Some(-1), // detect the number of speakers
        threshold: Some(0.5),
        min_duration_on: Some(0.3),
        min_duration_off: Some(0.5),
        ..Default::default()
    };
    let (segmentation, embedding) = (model_file("segmentation.onnx", SEGMENTATION)?, model_file("embedding.onnx", EMBEDDING)?);
    // sherpa-rs reports errors as eyre::Report.
    let mut d = Diarize::new(segmentation, embedding, config).map_err(|e| anyhow!("{e}"))?;
    let turns = d.compute(samples.to_vec(), None).map_err(|e| anyhow!("{e}"))?;
    if std::env::var_os("HEYLISTEN_DEBUG").is_some() {
        for t in &turns {
            eprintln!("turn {:6.2}–{:6.2} speaker {}", t.start, t.end, t.speaker);
        }
    }
    Ok(turns
        .into_iter()
        .map(|t| Turn { start_ms: (t.start * 1000.0) as u64, end_ms: (t.end * 1000.0) as u64, speaker: t.speaker })
        .collect())
}

/// Labels the Others (system track) as Speaker 1, 2, 3… in order of appearance.
/// Returns how many speakers were found.
pub fn label_others(segments: &mut [Segment], turns: &[Turn]) -> u32 {
    let clusters = clusters(segments, turns, Who::Others);
    label(segments, &clusters, 1)
}

/// Labels the mic track. One voice stays Me. Several voices (a meeting room) can't tell which
/// one is Me, so they become Speakers numbered from `first`, after the Others.
pub fn label_mic(segments: &mut [Segment], turns: &[Turn], first: u32) {
    let mut clusters = clusters(segments, turns, Who::Me);
    // A cluster with under 5 % of the mic's speech is usually one voice split in two, not a person.
    let mut time: HashMap<i32, u64> = HashMap::new();
    for (s, c) in segments.iter().zip(&clusters) {
        if let Some(c) = c {
            *time.entry(*c).or_default() += s.end_ms - s.start_ms;
        }
    }
    let total: u64 = time.values().sum();
    let Some(main) = time.iter().max_by_key(|(_, t)| **t).map(|(c, _)| *c) else { return };
    for c in clusters.iter_mut().flatten() {
        if time[c] * 20 < total {
            *c = main;
        }
    }
    let voices: HashSet<i32> = clusters.iter().flatten().copied().collect();
    if voices.len() > 1 {
        label(segments, &clusters, first);
    }
}

/// Renumbers Speakers 1, 2, 3… in order of first appearance in the whole meeting,
/// whichever track they're on.
pub fn renumber(segments: &mut [Segment]) {
    let mut order: Vec<u32> = Vec::new();
    for s in segments.iter_mut() {
        if let Who::Speaker(n) = s.who {
            let i = order.iter().position(|o| *o == n).unwrap_or_else(|| {
                order.push(n);
                order.len() - 1
            });
            s.who = Who::Speaker(i as u32 + 1);
        }
    }
}

/// The diarization speaker of each segment said by `who` (None for everyone else): the one it
/// overlaps most, or the nearest if none overlap.
fn clusters(segments: &[Segment], turns: &[Turn], who: Who) -> Vec<Option<i32>> {
    segments
        .iter()
        .map(|s| {
            if s.who != who {
                return None;
            }
            // Positive: overlap in ms. Negative: distance in ms.
            let score = |t: &Turn| s.end_ms.min(t.end_ms) as i64 - s.start_ms.max(t.start_ms) as i64;
            turns.iter().max_by_key(|t| score(t)).map(|t| t.speaker)
        })
        .collect()
}

/// Gives each cluster a Speaker number from `first`, in order of first appearance.
fn label(segments: &mut [Segment], clusters: &[Option<i32>], first: u32) -> u32 {
    let mut order: Vec<i32> = Vec::new();
    for (s, c) in segments.iter_mut().zip(clusters) {
        let Some(c) = c else { continue };
        let n = order.iter().position(|o| o == c).unwrap_or_else(|| {
            order.push(*c);
            order.len() - 1
        });
        s.who = Who::Speaker(first + n as u32);
    }
    order.len() as u32
}

/// sherpa-onnx wants model files: unpack the embedded ones next to the meetings.
fn model_file(name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let path = meetings_dir().with_file_name("models").join(name);
    if fs::metadata(&path).map(|m| m.len()).ok() != Some(bytes.len() as u64) {
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, bytes)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: u64, end_ms: u64, who: Who) -> Segment {
        Segment { start_ms, end_ms, who, text: String::new() }
    }

    fn turn(start_ms: u64, end_ms: u64, speaker: i32) -> Turn {
        Turn { start_ms, end_ms, speaker }
    }

    #[test]
    fn labels_others_by_overlap_in_order_of_appearance() {
        let turns = [turn(0, 5_000, 7), turn(5_000, 9_000, 3)];
        let mut segments = [seg(500, 4_000, Who::Others), seg(4_500, 8_500, Who::Others), seg(1_000, 2_000, Who::Me), seg(20_000, 21_000, Who::Others)];
        assert_eq!(label_others(&mut segments, &turns), 2);
        let who: Vec<Who> = segments.iter().map(|s| s.who).collect();
        assert_eq!(who, [Who::Speaker(1), Who::Speaker(2), Who::Me, Who::Speaker(2)]);
    }

    #[test]
    fn one_voice_on_the_mic_stays_me() {
        // The second cluster has under 5 % of the speech: the same voice, split.
        let turns = [turn(0, 30_000, 0), turn(30_000, 31_000, 1)];
        let mut segments = [seg(0, 29_000, Who::Me), seg(30_000, 31_000, Who::Me)];
        label_mic(&mut segments, &turns, 3);
        assert!(segments.iter().all(|s| s.who == Who::Me));
    }

    #[test]
    fn several_voices_on_the_mic_become_speakers_after_the_others() {
        let turns = [turn(0, 5_000, 4), turn(5_000, 10_000, 2), turn(10_000, 15_000, 4)];
        let mut segments = [seg(0, 5_000, Who::Me), seg(5_000, 10_000, Who::Me), seg(6_000, 7_000, Who::Speaker(1)), seg(10_000, 15_000, Who::Me)];
        label_mic(&mut segments, &turns, 2);
        let who: Vec<Who> = segments.iter().map(|s| s.who).collect();
        assert_eq!(who, [Who::Speaker(2), Who::Speaker(3), Who::Speaker(1), Who::Speaker(2)]);
        renumber(&mut segments);
        let who: Vec<Who> = segments.iter().map(|s| s.who).collect();
        assert_eq!(who, [Who::Speaker(1), Who::Speaker(2), Who::Speaker(3), Who::Speaker(1)]);
    }
}
