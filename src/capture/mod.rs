//! Audio sources. Each one calls its sink with mono f32 samples and their sample rate,
//! on an audio thread, until it's dropped.

mod mic;
#[cfg(target_os = "macos")]
mod system_macos;

pub use mic::Mic;

pub type Sink = Box<dyn FnMut(&[f32], u32) + Send>;

/// Everything this machine plays, except heyListen itself.
#[cfg(target_os = "macos")]
pub use system_macos::System;

#[cfg(not(target_os = "macos"))]
pub struct System;

#[cfg(not(target_os = "macos"))]
impl System {
    pub fn start(_sink: Sink) -> anyhow::Result<System> {
        anyhow::bail!("system audio capture isn't supported on this platform yet")
    }
}

/// Downmixes interleaved frames to mono.
pub fn downmix(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    let channels = channels.max(1);
    out.extend(interleaved.chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32));
}

/// Downmixes interleaved frames to mono, averaging only the channels that carry sound.
/// An audio interface often has one mic among several silent inputs; a plain average would
/// make that mic 6 times quieter on a 6-input interface.
pub fn downmix_active(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    let channels = channels.max(1);
    let active: Vec<usize> = (0..channels)
        .filter(|&c| interleaved.iter().skip(c).step_by(channels).any(|s| s.abs() > SILENT))
        .collect();
    if active.is_empty() || active.len() == channels {
        return downmix(interleaved, channels, out);
    }
    out.clear();
    out.extend(interleaved.chunks(channels).map(|f| active.iter().map(|&c| f[c]).sum::<f32>() / active.len() as f32));
}

/// Below this, a channel counts as not connected (about −80 dBFS).
const SILENT: f32 = 1e-4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_downmix_ignores_silent_inputs() {
        // Six inputs, a mic on input 1 only.
        let frames: Vec<f32> = (0..100).flat_map(|i| [if i % 2 == 0 { 0.5 } else { -0.5 }, 0.0, 0.0, 0.0, 0.0, 0.0]).collect();
        let mut out = Vec::new();
        downmix_active(&frames, 6, &mut out);
        assert_eq!(out.len(), 100);
        assert!((out[0] - 0.5).abs() < 1e-6, "full level, not a sixth: {}", out[0]);
        downmix(&frames, 6, &mut out);
        assert!((out[0] - 0.5 / 6.0).abs() < 1e-6);
    }
}

