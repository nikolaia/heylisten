//! Diarization: tells the Others apart. Runs on the system track after the meeting.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::config::models_dir;
use crate::transcript::{Segment, Who};

const SEGMENTATION: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/segmentation.onnx"));
const EMBEDDING: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/embedding.onnx"));

/// One stretch of time where a single speaker talks.
pub struct Turn {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: i32,
}

/// Threads for speaker recognition. It runs after the meeting, on the CPU.
const THREADS: i32 = 4;

/// Finds who speaks when in 16 kHz mono audio. The number of speakers is detected.
pub fn diarize(samples: &[f32], debug: bool) -> Result<Vec<Turn>> {
    use sherpa_rs::sherpa_rs_sys as sys;
    use std::ffi::CString;

    let path = |p: PathBuf| CString::new(p.to_string_lossy().as_bytes()).context("bad model path");
    let segmentation = path(model_file("segmentation.onnx", SEGMENTATION)?)?;
    let embedding = path(model_file("embedding.onnx", EMBEDDING)?)?;
    let provider = CString::new("cpu")?;
    // sherpa-rs's own wrapper always uses one thread; this is the same setup with more.
    let config = sys::SherpaOnnxOfflineSpeakerDiarizationConfig {
        segmentation: sys::SherpaOnnxOfflineSpeakerSegmentationModelConfig {
            pyannote: sys::SherpaOnnxOfflineSpeakerSegmentationPyannoteModelConfig { model: segmentation.as_ptr() },
            num_threads: THREADS,
            debug: 0,
            provider: provider.as_ptr(),
        },
        embedding: sys::SherpaOnnxSpeakerEmbeddingExtractorConfig {
            model: embedding.as_ptr(),
            num_threads: THREADS,
            debug: 0,
            provider: provider.as_ptr(),
        },
        clustering: sys::SherpaOnnxFastClusteringConfig { num_clusters: -1, threshold: 0.5 }, // detect the speaker count
        min_duration_on: 0.3,
        min_duration_off: 0.5,
    };

    let mut turns = Vec::new();
    unsafe {
        let sd = sys::SherpaOnnxCreateOfflineSpeakerDiarization(&config);
        if sd.is_null() {
            bail!("couldn't load the speaker models");
        }
        let result = sys::SherpaOnnxOfflineSpeakerDiarizationProcess(sd, samples.as_ptr(), samples.len() as i32);
        if !result.is_null() {
            let n = sys::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments(result);
            let segments = sys::SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime(result);
            if !segments.is_null() {
                for t in std::slice::from_raw_parts(segments, n.max(0) as usize) {
                    turns.push(Turn { start_ms: (t.start * 1000.0) as u64, end_ms: (t.end * 1000.0) as u64, speaker: t.speaker });
                }
                sys::SherpaOnnxOfflineSpeakerDiarizationDestroySegment(segments);
            }
            sys::SherpaOnnxOfflineSpeakerDiarizationDestroyResult(result);
        }
        sys::SherpaOnnxDestroyOfflineSpeakerDiarization(sd);
        if result.is_null() {
            bail!("speaker recognition failed");
        }
    }
    if debug {
        for t in &turns {
            crate::recorder::log(&format!("turn {:.2}–{:.2} s: speaker {}", t.start_ms as f32 / 1000.0, t.end_ms as f32 / 1000.0, t.speaker));
        }
    }
    Ok(turns)
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
    let path = models_dir().join(name);
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
