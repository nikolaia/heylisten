use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::{Sink, downmix};

/// The system default microphone.
pub struct Mic {
    _stream: cpal::Stream,
    pub device: String,
    pub rate: u32,
    pub channels: usize,
}

impl Mic {
    pub fn start(mut sink: Sink) -> Result<Mic> {
        let device = cpal::default_host().default_input_device().context("no microphone found")?;
        let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|_| "microphone".into());
        let config = device.default_input_config().context("can't read the microphone's format")?;
        if config.sample_format() != cpal::SampleFormat::F32 {
            bail!("microphone uses {} samples; only f32 is supported", config.sample_format());
        }
        let channels = config.channels() as usize;
        let rate = config.sample_rate();
        let mut mono = Vec::new();
        let stream = device.build_input_stream(
            config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                downmix(data, channels, &mut mono);
                sink(&mono, rate);
            },
            |err| crate::recorder::log(&format!("microphone error: {err}")),
            None,
        )?;
        stream.play().context("can't start the microphone (check microphone permission)")?;
        Ok(Mic { _stream: stream, device: name, rate, channels })
    }
}
