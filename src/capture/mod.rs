//! Audio sources. Each one calls its sink with mono f32 samples, their sample rate, and when
//! the first of them was captured (see `now_ns`), on an audio thread, until it's dropped.

mod mic;
#[cfg(target_os = "macos")]
mod system_macos;

pub use mic::Mic;

pub type Sink = Box<dyn FnMut(&[f32], u32, u64) + Send>;

/// Now, on the clock capture times use, in nanoseconds. On macOS that's the host clock both the
/// mic and the system-audio tap stamp their buffers with, so the two tracks can be lined up
/// exactly, whenever each buffer happens to arrive.
#[cfg(target_os = "macos")]
pub fn now_ns() -> u64 {
    host_ticks_to_ns(unsafe { mach2::mach_time::mach_absolute_time() })
}

#[cfg(target_os = "macos")]
pub fn host_ticks_to_ns(ticks: u64) -> u64 {
    use std::sync::OnceLock;
    static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut info = mach2::mach_time::mach_timebase_info::default();
        unsafe { mach2::mach_time::mach_timebase_info(&mut info) };
        (info.numer as u64, info.denom.max(1) as u64)
    });
    (ticks as u128 * numer as u128 / denom as u128) as u64
}

/// Elsewhere there's no shared capture clock yet: buffers are stamped when they arrive.
#[cfg(not(target_os = "macos"))]
pub fn now_ns() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

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

/// Checks a device's reported sample rate against how fast samples actually arrive.
/// AirPods and other Bluetooth headsets drop to a call-quality rate when a mic is in use,
/// and macOS doesn't always say so; samples taken at the wrong rate sound like a duck.
#[derive(Default)]
pub struct RateCheck {
    start_ns: u64,
    last_ns: u64,
    frames: u64,
}

impl RateCheck {
    /// Over this long, the arrival rate is measured.
    const WINDOW_NS: u64 = 2_000_000_000;
    /// A pause this long (sleep, a stalled device) starts the measurement again.
    const STALL_NS: u64 = 200_000_000;
    /// The rates devices run at.
    const RATES: [u32; 9] = [8_000, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000, 96_000];

    /// Counts `frames` captured at `captured_ns`, and returns the rate to use: `rate`, unless
    /// samples have been arriving at least 15 % faster or slower, then the nearest usual rate.
    pub fn update(&mut self, rate: u32, frames: usize, captured_ns: u64) -> u32 {
        let stalled = captured_ns.saturating_sub(self.last_ns) > Self::STALL_NS;
        self.last_ns = captured_ns;
        if self.start_ns == 0 || stalled {
            (self.start_ns, self.frames) = (captured_ns, frames as u64);
            return rate;
        }
        let elapsed = captured_ns - self.start_ns;
        let mut rate = rate;
        if elapsed >= Self::WINDOW_NS {
            let measured = self.frames as f64 * 1e9 / elapsed as f64;
            if (measured - rate as f64).abs() > rate as f64 * 0.15 {
                rate = *Self::RATES.iter().min_by_key(|r| (**r as f64 - measured).abs() as u64).unwrap();
            }
            (self.start_ns, self.frames) = (captured_ns, 0);
        }
        self.frames += frames as u64;
        rate
    }
}

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

    /// Feeds 10 ms buffers of `true_rate` audio, timed by the clock, for `secs` seconds.
    fn feed(check: &mut RateCheck, mut rate: u32, true_rate: u32, start_ns: u64, secs: u64) -> u32 {
        let frames = true_rate as usize / 100;
        for i in 0..secs * 100 {
            rate = check.update(rate, frames, start_ns + i * 10_000_000);
        }
        rate
    }

    #[test]
    fn rate_check_follows_what_actually_arrives() {
        let mut check = RateCheck::default();
        // Reported 48 kHz, true: kept.
        assert_eq!(feed(&mut check, 48_000, 48_000, 1_000_000_000, 5), 48_000);
        // A headset switches to call mode: 24 kHz arrives, still reported as 48 kHz.
        assert_eq!(feed(&mut check, 48_000, 24_000, 6_000_000_000, 5), 24_000);
        // And back.
        assert_eq!(feed(&mut check, 24_000, 48_000, 11_000_000_000, 5), 48_000);
    }

    #[test]
    fn rate_check_ignores_a_pause() {
        let mut check = RateCheck::default();
        assert_eq!(feed(&mut check, 48_000, 48_000, 1_000_000_000, 1), 48_000);
        // Ten seconds with no buffers (the Mac slept), then normal again.
        assert_eq!(feed(&mut check, 48_000, 48_000, 12_000_000_000, 5), 48_000);
    }
}
