//! Tunes speaker separation against a recording with known speakers: a WAV (16 kHz mono) and a
//! caption file with lines like "[01:06] Kari: …" (each line lasts until the next).
//!
//!   cargo run --release --example diarize_tune -- <file.wav> <captions.md> <threshold>:<merge>:<small>:<small_secs>...
//!
//! For each threshold: how many speakers were found, and what share of the speech time lands on
//! the right person (each found speaker counts as whoever they overlap most).
use std::collections::BTreeMap;

use heylisten::audio;
use heylisten::diarize::{Clustering, diarize_with};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let samples = audio::read(a[1].as_ref()).unwrap();
    let total_ms = samples.len() as u64 * 1000 / 16000;
    let captions: Vec<(u64, String)> = std::fs::read_to_string(&a[2])
        .unwrap()
        .lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix('[')?;
            let (time, rest) = l.split_once(']')?;
            let (m, s) = time.split_once(':')?;
            let who = rest.trim().split_once(':')?.0.trim().to_string();
            Some((m.parse::<u64>().ok()? * 60_000 + s.parse::<u64>().ok()? * 1000, who))
        })
        .collect();
    let truth = |t: u64| captions.iter().rev().find(|(start, _)| *start <= t).map(|(_, w)| w.clone());
    let people: std::collections::BTreeSet<_> = captions.iter().map(|(_, w)| w.clone()).collect();
    println!("{} caption lines, {} people: {people:?}", captions.len(), people.len());

    for spec in &a[3..] {
        let p: Vec<f32> = spec.split(':').map(|x| x.parse().unwrap()).collect();
        let clustering = Clustering { threshold: p[0], merge_similarity: p[1], small_similarity: p[2], small_secs: p[3], ..Clustering::default() };
        let threshold = spec;
        let turns = diarize_with(&samples, clustering, false).unwrap();
        // overlap[found speaker][true speaker] in 100 ms steps
        let mut overlap: BTreeMap<i32, BTreeMap<String, u64>> = BTreeMap::new();
        for t in (0..total_ms).step_by(100) {
            let (Some(found), Some(who)) = (turns.iter().find(|x| x.start_ms <= t && t < x.end_ms), truth(t)) else { continue };
            *overlap.entry(found.speaker).or_default().entry(who).or_default() += 100;
        }
        let total: u64 = overlap.values().flat_map(|m| m.values()).sum();
        let right: u64 = overlap.values().map(|m| m.values().max().copied().unwrap_or(0)).sum();
        let mut sizes: Vec<u64> = overlap.values().map(|m| m.values().sum()).collect();
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        let big = sizes.iter().filter(|s| **s * 20 >= total).count();
        let mapping: Vec<String> = overlap
            .iter()
            .map(|(sp, m)| {
                let (who, ms) = m.iter().max_by_key(|(_, v)| **v).unwrap();
                format!("{sp}→{who} ({:.0}s)", *ms as f64 / 1000.0)
            })
            .collect();
        println!(
            "{threshold:>18}: {} speakers ({big} with ≥5 % of the speech), {:.0} % of speech on the right person | {}",
            overlap.len(),
            right as f64 * 100.0 / total.max(1) as f64,
            mapping.join(", ")
        );
    }
}
