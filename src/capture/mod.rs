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

