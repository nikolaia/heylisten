//! The built-in summary engine: llama.cpp's `llama-server`, started on localhost for the
//! duration of a summary (see docs/adr/0005). No Ollama needed.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config::{data_dir, models_dir};

/// llama.cpp release, pinned.
const LLAMA_RELEASE: &str = "b11321";

/// The prebuilt llama.cpp for this platform: (download URL, SHA-256, bytes).
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const LLAMA_BUILD: Option<(&str, &str, u64)> = Some((
    "https://github.com/ggml-org/llama.cpp/releases/download/b11321/llama-b11321-bin-macos-arm64.tar.gz",
    "5f47ffa4de936853004e7403a09d87616022e5af16651d71fc66a96b261886fb",
    11_827_849,
));
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
const LLAMA_BUILD: Option<(&str, &str, u64)> = None;

/// Borealis 12B, Q4_K_M: the same file Ollama pulls for `hf.co/NbAiLab/borealis-12b-gguf`.
pub const BOREALIS_URL: &str = "https://huggingface.co/NbAiLab/borealis-12b-gguf/resolve/main/borealis-12b-Q4_K_M.gguf";
pub const BOREALIS_SHA256: &str = "4b1483c7c65dc9fe888877a09484de14e497f30cc6fd12df772e2549942aee62";
pub const BOREALIS_BYTES: u64 = 7_300_778_912;
const BOREALIS_FILE: &str = "borealis-12b-Q4_K_M.gguf";

pub fn available() -> bool {
    LLAMA_BUILD.is_some()
}

fn engine_dir() -> PathBuf {
    data_dir().join("engine")
}

fn server_path() -> PathBuf {
    engine_dir().join(format!("llama-{LLAMA_RELEASE}")).join("llama-server")
}

pub fn installed() -> bool {
    server_path().is_file()
}

/// Where Borealis is on disk: our own copy, or the identical file Ollama already has.
pub fn borealis_path() -> Option<PathBuf> {
    let ours = models_dir().join(BOREALIS_FILE);
    if ours.is_file() {
        return Some(ours);
    }
    let ollama = std::env::var_os("OLLAMA_MODELS")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".ollama/models")))?;
    let blob = ollama.join("blobs").join(format!("sha256-{BOREALIS_SHA256}"));
    blob.is_file().then_some(blob)
}

/// Downloads and unpacks llama.cpp. `progress(done, total)` in bytes.
pub fn install(progress: impl FnMut(u64, u64)) -> Result<()> {
    let Some((url, sha256, bytes)) = LLAMA_BUILD else { bail!("the built-in summary engine isn't available on this platform yet; use summary_engine = \"ollama\"") };
    let dir = engine_dir();
    let tarball = dir.join(format!("llama-{LLAMA_RELEASE}.tar.gz"));
    crate::setup::download(url, sha256, bytes, &tarball, progress)?;
    let status = Command::new("tar").arg("xzf").arg(&tarball).arg("-C").arg(&dir).status()?;
    if !status.success() || !installed() {
        bail!("couldn't unpack {}", tarball.display());
    }
    std::fs::remove_file(&tarball)?;
    Ok(())
}

/// Downloads Borealis into heyListen's models folder.
pub fn download_borealis(progress: impl FnMut(u64, u64)) -> Result<()> {
    crate::setup::download(BOREALIS_URL, BOREALIS_SHA256, BOREALIS_BYTES, &models_dir().join(BOREALIS_FILE), progress)
}

/// A running `llama-server`, stopped when dropped. It only listens on 127.0.0.1, and needs a
/// key made fresh for each run, so other programs on this machine can't use it meanwhile.
pub struct Server {
    child: Child,
    port: u16,
    key: String,
}

impl Server {
    pub fn start(model: &Path, num_ctx: u64) -> Result<Server> {
        // Let the OS pick a free port, then hand it to llama-server.
        let port = TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let key = random_key()?;
        let mut command = Command::new(server_path());
        // A summary may still be running when the next call starts: let the call go first.
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                libc::setpriority(libc::PRIO_PROCESS, 0, 10);
                Ok(())
            });
        }
        let child = command
            .arg("-m")
            .arg(model)
            .args(["--host", "127.0.0.1", "--port", &port.to_string(), "-c", &num_ctx.to_string(), "-ngl", "99", "--jinja"])
            // Passed in the environment rather than as an argument, so `ps` doesn't show it.
            .env("LLAMA_API_KEY", &key)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("can't start the built-in summary engine (run heylisten setup)")?;
        let mut server = Server { child, port, key };

        // Loading 7 GB onto the GPU takes a while, the first time especially.
        let agent = agent(Duration::from_secs(2));
        let deadline = Instant::now() + Duration::from_secs(180);
        while agent.get(format!("http://127.0.0.1:{port}/health")).call().is_err() {
            if server.child.try_wait()?.is_some() {
                bail!("the built-in summary engine exited while loading {}", model.display());
            }
            if Instant::now() > deadline {
                bail!("the built-in summary engine didn't start within 3 minutes");
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Ok(server)
    }

    /// One chat turn: system prompt + user message → reply.
    pub fn chat(&self, system: &str, user: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Reply {
            choices: Vec<Choice>,
        }
        #[derive(Deserialize)]
        struct Choice {
            message: Message,
        }
        #[derive(Deserialize)]
        struct Message {
            content: String,
        }
        let body = serde_json::json!({
            "temperature": 0.2,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
        });
        let reply: Reply = agent(Duration::from_secs(30 * 60))
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", self.port))
            .header("Authorization", format!("Bearer {}", self.key))
            .send_json(body)?
            .body_mut()
            .read_json()?;
        let content = reply.choices.into_iter().next().context("empty reply from the summary engine")?.message.content;
        Ok(content.trim().to_string())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 128 random bits, hex.
fn random_key() -> Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into()
}
