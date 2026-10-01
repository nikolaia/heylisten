use std::fs;
use std::path::Path;

use crate::config::{Config, meetings_dir};
use crate::ollama;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Warn,
    Fail,
}

pub struct Check {
    pub status: Status,
    pub what: String,
    /// What to do about it, copy-pasteable where possible.
    pub fix: Option<String>,
}

fn pass(what: impl Into<String>) -> Check {
    Check { status: Status::Pass, what: what.into(), fix: None }
}

fn fail(what: impl Into<String>, fix: impl Into<String>) -> Check {
    Check { status: Status::Fail, what: what.into(), fix: Some(fix.into()) }
}

pub fn run(config: &Config) -> Vec<Check> {
    let mut checks = Vec::new();

    let model = &config.whisper_model;
    checks.push(if model.is_file() {
        pass(format!("Whisper model {}", model.display()))
    } else {
        fail(
            format!("Whisper model missing: {}", model.display()),
            format!(
                "mkdir -p {dir} && curl -L -o {path} https://huggingface.co/NbAiLab/nb-whisper-large/resolve/main/ggml-model-q5_0.bin",
                dir = shell_quote(model.parent().unwrap_or(Path::new("."))),
                path = shell_quote(model),
            ),
        )
    });

    checks.push(match writable(&config.vault) {
        true => pass(format!("Vault folder {}", config.vault.display())),
        false => fail(
            format!("Vault folder missing or not writable: {}", config.vault.display()),
            format!("mkdir -p {}  (or set `vault` in the config)", shell_quote(&config.vault)),
        ),
    });

    let meetings = meetings_dir();
    checks.push(match fs::create_dir_all(&meetings).is_ok() && writable(&meetings) {
        true => pass(format!("Meetings folder {}", meetings.display())),
        false => fail(format!("Meetings folder not writable: {}", meetings.display()), "Check the folder's permissions"),
    });

    if !config.ollama_is_local() {
        checks.push(Check {
            status: Status::Warn,
            what: format!("ollama_url {} is not on this machine: transcripts will leave it", config.ollama_url),
            fix: Some("Set ollama_url = \"http://localhost:11434\" to keep everything local".into()),
        });
    }

    match ollama::list_models(&config.ollama_url) {
        Err(_) => checks.push(fail(
            format!("Ollama not reachable at {}", config.ollama_url),
            "Start it: `ollama serve` (or open the Ollama app)",
        )),
        Ok(models) => {
            checks.push(pass(format!("Ollama running at {}", config.ollama_url)));
            checks.push(if ollama::has_model(&models, &config.ollama_model) {
                pass(format!("Ollama model {}", config.ollama_model))
            } else {
                fail(format!("Ollama model not pulled: {}", config.ollama_model), format!("ollama pull {}", config.ollama_model))
            });
        }
    }

    checks
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(".heylisten-write-test");
    let ok = dir.is_dir() && fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(probe);
    ok
}

fn shell_quote(path: &Path) -> String {
    let s = path.display().to_string();
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-~".contains(c)) { s } else { format!("'{}'", s.replace('\'', r"'\''")) }
}
