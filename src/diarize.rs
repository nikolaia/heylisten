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
    diarize_with(samples, Clustering::default(), debug)
}

/// How voices are grouped into speakers.
#[derive(Debug, Clone, Copy)]
pub struct Clustering {
    /// sherpa-onnx's clustering: voices closer than this (cosine distance) count as the same
    /// speaker. On its own it splits real people into many small speakers.
    pub threshold: f32,
    pub min_duration_on: f32,
    pub min_duration_off: f32,
    /// Then heyListen merges speakers whose averaged voices are this similar (cosine)...
    pub merge_similarity: f32,
    /// ...or, for a speaker with less than `small_secs` of speech, this similar.
    pub small_similarity: f32,
    pub small_secs: f32,
}

impl Default for Clustering {
    fn default() -> Clustering {
        Clustering {
            threshold: 0.5,
            min_duration_on: 0.3,
            min_duration_off: 0.5,
            // The most alike different people seen in testing were 0.65 similar, so 0.7 stays
            // above that.
            merge_similarity: 0.7,
            small_similarity: 0.5,
            small_secs: 10.0,
        }
    }
}

/// `diarize` with explicit clustering settings, for tuning.
pub fn diarize_with(samples: &[f32], clustering: Clustering, debug: bool) -> Result<Vec<Turn>> {
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
        clustering: sys::SherpaOnnxFastClusteringConfig { num_clusters: -1, threshold: clustering.threshold }, // detect the count
        min_duration_on: clustering.min_duration_on,
        min_duration_off: clustering.min_duration_off,
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
    merge_speakers(samples, &mut turns, clustering, &embedding)?;
    if debug {
        for t in &turns {
            crate::recorder::log(&format!("turn {:.2}–{:.2} s: speaker {}", t.start_ms as f32 / 1000.0, t.end_ms as f32 / 1000.0, t.speaker));
        }
    }
    Ok(turns)
}

/// The second step: an averaged voice per speaker, then merging, most similar pair first:
/// two speakers if their voices are clearly the same, a small one into a bigger one at a lower
/// bar. A speaker too short to measure goes to whoever spoke right next to it. The voices exist
/// only here, in memory.
fn merge_speakers(samples: &[f32], turns: &mut [Turn], clustering: Clustering, model: &std::ffi::CString) -> Result<()> {
    use sherpa_rs::sherpa_rs_sys as sys;
    use std::collections::BTreeMap;

    if clustering.merge_similarity > 1.0 && clustering.small_similarity > 1.0 {
        return Ok(());
    }
    let mut speech: BTreeMap<i32, f32> = BTreeMap::new();
    for t in turns.iter() {
        *speech.entry(t.speaker).or_default() += (t.end_ms - t.start_ms) as f32 / 1000.0;
    }
    let provider = std::ffi::CString::new("cpu")?;
    let config = sys::SherpaOnnxSpeakerEmbeddingExtractorConfig { model: model.as_ptr(), num_threads: THREADS, debug: 0, provider: provider.as_ptr() };
    let mut voices: BTreeMap<i32, Vec<f32>> = BTreeMap::new();
    unsafe {
        let extractor = sys::SherpaOnnxCreateSpeakerEmbeddingExtractor(&config);
        if extractor.is_null() {
            bail!("couldn't load the speaker embedding model");
        }
        let dim = sys::SherpaOnnxSpeakerEmbeddingExtractorDim(extractor) as usize;
        for &speaker in speech.keys() {
            // Up to 60 s of the speaker's longest turns.
            let mut own: Vec<&Turn> = turns.iter().filter(|t| t.speaker == speaker).collect();
            own.sort_by_key(|t| std::cmp::Reverse(t.end_ms - t.start_ms));
            let mut audio = Vec::new();
            for t in own {
                let (s, e) = ((t.start_ms * 16) as usize, ((t.end_ms * 16) as usize).min(samples.len()));
                audio.extend_from_slice(&samples[s.min(e)..e]);
                if audio.len() >= 16_000 * 60 {
                    break;
                }
            }
            if audio.len() < 16_000 {
                continue; // too short to measure
            }
            let stream = sys::SherpaOnnxSpeakerEmbeddingExtractorCreateStream(extractor);
            sys::SherpaOnnxOnlineStreamAcceptWaveform(stream, 16_000, audio.as_ptr(), audio.len() as i32);
            sys::SherpaOnnxOnlineStreamInputFinished(stream);
            if sys::SherpaOnnxSpeakerEmbeddingExtractorIsReady(extractor, stream) != 0 {
                let v = sys::SherpaOnnxSpeakerEmbeddingExtractorComputeEmbedding(extractor, stream);
                if !v.is_null() {
                    voices.insert(speaker, normalized(std::slice::from_raw_parts(v, dim)));
                    sys::SherpaOnnxSpeakerEmbeddingExtractorDestroyEmbedding(v);
                }
            }
            sys::SherpaOnnxDestroyOnlineStream(stream);
        }
        sys::SherpaOnnxDestroySpeakerEmbeddingExtractor(extractor);
    }

    let mut into: BTreeMap<i32, i32> = BTreeMap::new();
    loop {
        let ids: Vec<i32> = voices.keys().copied().collect();
        let mut best: Option<(f32, i32, i32)> = None;
        for (i, &a) in ids.iter().enumerate() {
            for &b in &ids[i + 1..] {
                let small = speech[&a].min(speech[&b]) < clustering.small_secs;
                let bar = if small { clustering.small_similarity } else { clustering.merge_similarity };
                let sim = dot(&voices[&a], &voices[&b]);
                if sim >= bar && best.is_none_or(|(s, _, _)| sim > s) {
                    best = Some((sim, a, b));
                }
            }
        }
        let Some((_, a, b)) = best else { break };
        // Merge the smaller into the bigger, weighting the voices by speech.
        let (keep, gone) = if speech[&a] >= speech[&b] { (a, b) } else { (b, a) };
        let (wk, wg) = (speech[&keep], speech[&gone]);
        let merged: Vec<f32> = voices[&keep].iter().zip(&voices[&gone]).map(|(k, g)| k * wk + g * wg).collect();
        voices.insert(keep, normalized(&merged));
        voices.remove(&gone);
        *speech.get_mut(&keep).unwrap() += wg;
        speech.remove(&gone);
        into.insert(gone, keep);
    }
    let resolve = |mut s: i32| {
        while let Some(&n) = into.get(&s) {
            s = n;
        }
        s
    };
    for t in turns.iter_mut() {
        t.speaker = resolve(t.speaker);
    }
    // Speakers too short to measure: whoever spoke right before (or after) them.
    let measured: std::collections::BTreeSet<i32> = voices.keys().copied().collect();
    for i in 0..turns.len() {
        if !measured.contains(&turns[i].speaker) {
            let neighbour = turns[..i].iter().rev().chain(turns[i + 1..].iter()).find(|t| measured.contains(&t.speaker)).map(|t| t.speaker);
            if let Some(n) = neighbour {
                turns[i].speaker = n;
            }
        }
    }
    Ok(())
}

fn normalized(v: &[f32]) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.iter().map(|x| x / n).collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
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
