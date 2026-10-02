use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::{Sink, downmix_active};

/// The system default microphone.
pub struct Mic {
    _stream: cpal::Stream,
    pub device: String,
    pub rate: u32,
    pub channels: usize,
}

impl Mic {
    /// `debug` logs each input channel's peak level every 5 s.
    pub fn start(mut sink: Sink, debug: bool) -> Result<Mic> {
        let device = cpal::default_host().default_input_device().context("no microphone found")?;
        let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "microphone".into());
        let config = device.default_input_config().context("can't read the microphone's format")?;
        if config.sample_format() != cpal::SampleFormat::F32 {
            bail!("microphone uses {} samples; only f32 is supported", config.sample_format());
        }
        let channels = config.channels() as usize;
        let rate = config.sample_rate();
        let mut mono = Vec::new();
        let (mut peaks, mut frames) = (vec![0f32; channels], 0usize);
        let stream = device.build_input_stream(
            config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                downmix_active(data, channels, &mut mono);
                sink(&mono, rate);
                if debug {
                    for (i, s) in data.iter().enumerate() {
                        peaks[i % channels] = peaks[i % channels].max(s.abs());
                    }
                    frames += data.len() / channels;
                    if frames >= rate as usize * 5 {
                        let db: Vec<String> = peaks.iter().map(|p| format!("{:.0}", 20.0 * p.max(1e-6).log10())).collect();
                        crate::recorder::log(&format!("mic: peak per input over 5 s (dBFS): [{}]", db.join(", ")));
                        peaks.fill(0.0);
                        frames = 0;
                    }
                }
            },
            |err| crate::recorder::log(&format!("microphone error: {err}")),
            None,
        )?;
        stream.play().context("can't start the microphone (check microphone permission)")?;
        Ok(Mic { _stream: stream, device: name, rate, channels })
    }
}
