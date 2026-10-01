//! First-run setup: fetches the speech model, and the summary engine and model (or has Ollama
//! pull it). This is the only time heyListen downloads anything, and only when asked
//! (see docs/adr/0004 and 0005).

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::config::{Config, SummaryEngine};
use crate::{engine, ollama};

/// NB-Whisper large, q5_0 GGML, from the National Library of Norway.
pub const WHISPER_URL: &str = "https://huggingface.co/NbAiLab/nb-whisper-large/resolve/main/ggml-model-q5_0.bin";
const WHISPER_SHA256: &str = "feb5951ae694a62cfeb81fb501f6cfa8cc50d96bcddb1e4e8215f7006bac23a2";
pub const WHISPER_BYTES: u64 = 1_081_140_203;
pub const OLLAMA_DOWNLOAD: &str = "https://ollama.com/download";

/// What's still missing for the configured engines.
pub struct Needs {
    pub whisper_model: bool,
    /// Built-in engine: llama.cpp isn't downloaded yet.
    pub engine: bool,
    /// Built-in: Borealis isn't on disk (ours or Ollama's). Ollama: the model isn't pulled.
    pub summary_model: bool,
    /// Ollama engine only: Ollama isn't running.
    pub ollama_missing: bool,
    /// Models Ollama has, if it's running (to choose from).
    pub ollama_models: Vec<String>,
}

impl Needs {
    pub fn check(config: &Config) -> Needs {
        let ollama = ollama::list_models(&config.ollama_url).ok();
        let builtin = config.summary_engine == SummaryEngine::Builtin;
        Needs {
            whisper_model: !config.whisper_model.is_file(),
            engine: builtin && !engine::installed(),
            summary_model: match &ollama {
                _ if builtin => engine::borealis_path().is_none(),
                Some(models) => !ollama::has_model(models, &config.ollama_model),
                None => true,
            },
            ollama_missing: !builtin && ollama.is_none(),
            ollama_models: ollama.unwrap_or_default(),
        }
    }

    pub fn anything(&self) -> bool {
        self.whisper_model || self.engine || self.summary_model || self.ollama_missing
    }

    /// Whether "Download models" can do something now (Ollama models need Ollama running).
    pub fn downloadable(&self) -> bool {
        self.whisper_model || self.engine || (self.summary_model && !self.ollama_missing)
    }

    /// Bytes still to download, for "downloads ~8 GB".
    pub fn download_bytes(&self) -> u64 {
        (self.whisper_model as u64 * WHISPER_BYTES) + (self.summary_model as u64 * engine::BOREALIS_BYTES)
    }
}

pub enum Progress {
    /// Bytes of the speech model so far.
    SpeechModel { done: u64, total: u64 },
    /// Bytes of llama.cpp so far.
    Engine { done: u64, total: u64 },
    /// Bytes of the summary model so far (`status` is Ollama's, when it pulls).
    SummaryModel { status: String, done: u64, total: u64 },
}

/// Downloads whatever is missing. With summary_engine = "ollama", needs Ollama running.
pub fn run(config: &Config, mut progress: impl FnMut(Progress)) -> Result<()> {
    download_speech_model(config, &mut progress)?;
    get_summary_model(config, &mut progress)
}

/// The NB-Whisper model, if it isn't there yet.
pub fn download_speech_model(config: &Config, mut progress: impl FnMut(Progress)) -> Result<()> {
    if config.whisper_model.is_file() {
        return Ok(());
    }
    download(WHISPER_URL, WHISPER_SHA256, WHISPER_BYTES, &config.whisper_model, |done, total| {
        progress(Progress::SpeechModel { done, total })
    })
}

/// The summary engine and model: llama.cpp + Borealis, or an Ollama pull.
pub fn get_summary_model(config: &Config, mut progress: impl FnMut(Progress)) -> Result<()> {
    if config.summary_engine == SummaryEngine::Builtin {
        if !engine::installed() {
            engine::install(|done, total| progress(Progress::Engine { done, total }))?;
        }
        if engine::borealis_path().is_none() {
            engine::download_borealis(|done, total| progress(Progress::SummaryModel { status: String::new(), done, total }))?;
        }
        return Ok(());
    }
    let models = ollama::list_models(&config.ollama_url)
        .with_context(|| format!("Ollama isn't running. Install it from {OLLAMA_DOWNLOAD} and open it, or use summary_engine = \"builtin\""))?;
    if ollama::has_model(&models, &config.ollama_model) {
        return Ok(());
    }
    ollama::pull(&config.ollama_url, &config.ollama_model, |status, done, total| {
        progress(Progress::SummaryModel { status: status.to_string(), done, total })
    })
}

/// Downloads to `<dest>.part`, resuming an earlier attempt, checks the SHA-256, then moves it in place.
pub(crate) fn download(url: &str, sha256: &str, bytes: u64, dest: &Path, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    fs::create_dir_all(dest.parent().context("bad model path")?)?;
    let part = dest.with_extension("part");
    let mut hasher = Sha256::new();
    let mut have = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    if let Ok(mut f) = fs::File::open(&part) {
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            have += n as u64;
        }
    }

    if have < bytes {
        fetch(url, &part, &mut have, &mut hasher, &mut buf, &mut progress)?;
    }

    let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got != sha256 {
        let _ = fs::remove_file(&part);
        bail!("{url} downloaded with the wrong checksum ({got}); it was deleted, try again");
    }
    fs::rename(&part, dest)?;
    Ok(())
}

/// Appends the rest of `url` (from byte `have`) to `part`, hashing as it goes.
fn fetch(url: &str, part: &Path, have: &mut u64, hasher: &mut Sha256, buf: &mut [u8], progress: &mut impl FnMut(u64, u64)) -> Result<()> {
    // HTTPS all the way, redirects included. The SHA-256 check is what makes the file
    // trustworthy, but there's no reason to let anyone downgrade the connection.
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(None).https_only(true).build().into();
    let mut request = agent.get(url);
    if *have > 0 {
        request = request.header("Range", format!("bytes={have}-"));
    }
    let mut response = request.call().with_context(|| format!("can't download {url}"))?;
    if *have > 0 && response.status() != 206 {
        // The server ignored the range: start over.
        *have = 0;
        *hasher = Sha256::new();
    }
    let total = *have + response.body().content_length().unwrap_or(0);
    let mut file = OpenOptions::new().create(true).write(true).append(*have > 0).truncate(*have == 0).open(part)?;
    let mut body = response.body_mut().with_config().limit(u64::MAX).reader();
    loop {
        let n = body.read(buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        *have += n as u64;
        progress(*have, total);
    }
    file.flush()?;
    Ok(())
}
