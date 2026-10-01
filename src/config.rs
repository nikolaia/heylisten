use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

const DEFAULT_CONFIG: &str = include_str!("default-config.toml");
const DEFAULT_PROMPT: &str = include_str!("summary-prompt.md");

/// Every key is optional; the defaults suit a fresh install.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where notes are written. `vault` is its old name.
    #[serde(alias = "vault")]
    pub notes_dir: PathBuf,
    pub whisper_model: PathBuf,
    /// Which engine writes the summary: the built-in one, or Ollama with `ollama_model`.
    pub summary_engine: SummaryEngine,
    pub ollama_model: String,
    pub ollama_url: String,
    pub keep_audio: bool,
    /// For testing: keeps every recording and logs details to each meeting's recorder.log.
    /// HEYLISTEN_DEBUG=1 turns it on too.
    pub debug: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            notes_dir: "~/Desktop".into(),
            whisper_model: models_dir().join(WHISPER_MODEL_FILE),
            summary_engine: SummaryEngine::Builtin,
            ollama_model: "hf.co/NbAiLab/borealis-12b-gguf".into(),
            ollama_url: "http://localhost:11434".into(),
            keep_audio: false,
            debug: false,
        }
    }
}

pub const WHISPER_MODEL_FILE: &str = "nb-whisper-large-q5_0.bin";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SummaryEngine {
    /// llama.cpp with Borealis, started by heyListen itself.
    Builtin,
    /// A model in a local Ollama.
    Ollama,
}

impl Config {
    /// Loads the config file, creating it and the summary prompt with defaults on first run.
    pub fn load() -> Result<Config> {
        let path = config_path();
        fs::create_dir_all(path.parent().unwrap())?;
        for (file, default) in [(&path, DEFAULT_CONFIG), (&prompt_path(), DEFAULT_PROMPT)] {
            if !file.exists() {
                fs::write(file, default)?;
            }
        }
        let text = fs::read_to_string(&path)?;
        let mut config: Config =
            toml::from_str(&text).with_context(|| format!("invalid config in {}", path.display()))?;
        config.notes_dir = expand_home(&config.notes_dir);
        config.whisper_model = expand_home(&config.whisper_model);
        config.debug |= std::env::var_os("HEYLISTEN_DEBUG").is_some();
        Ok(config)
    }

    /// Saves a new notes folder to the config file, keeping its comments.
    pub fn set_notes_dir(&mut self, dir: &Path) -> Result<()> {
        edit(|doc| {
            doc.remove("vault");
            doc["notes_dir"] = toml_edit::value(dir.to_string_lossy().as_ref());
        })?;
        self.notes_dir = dir.to_path_buf();
        Ok(())
    }

    /// Saves debug mode to the config file.
    pub fn set_debug(&mut self, on: bool) -> Result<()> {
        edit(|doc| doc["debug"] = toml_edit::value(on))?;
        self.debug = on;
        Ok(())
    }

    /// Saves the summary engine (and, for Ollama, its model) to the config file.
    pub fn set_summary(&mut self, engine: SummaryEngine, ollama_model: Option<&str>) -> Result<()> {
        edit(|doc| {
            doc["summary_engine"] = toml_edit::value(if engine == SummaryEngine::Builtin { "builtin" } else { "ollama" });
            if let Some(model) = ollama_model {
                doc["ollama_model"] = toml_edit::value(model);
            }
        })?;
        self.summary_engine = engine;
        if let Some(model) = ollama_model {
            self.ollama_model = model.to_string();
        }
        Ok(())
    }

    /// True if `ollama_url` points at this machine.
    pub fn ollama_is_local(&self) -> bool {
        let rest = self.ollama_url.split_once("://").map_or(&*self.ollama_url, |(_, r)| r);
        let authority = rest.split('/').next().unwrap_or("");
        let host = if let Some(v6) = authority.strip_prefix('[') {
            v6.split(']').next().unwrap_or("")
        } else {
            authority.split(':').next().unwrap_or("")
        };
        host == "localhost" || host == "::1" || host.starts_with("127.")
    }
}

/// Changes the config file in place, keeping its comments.
fn edit(change: impl FnOnce(&mut toml_edit::DocumentMut)) -> Result<()> {
    let path = config_path();
    let mut doc: toml_edit::DocumentMut = fs::read_to_string(&path)?.parse()?;
    change(&mut doc);
    fs::write(&path, doc.to_string())?;
    Ok(())
}

/// `$XDG_CONFIG_HOME/heylisten/config.toml`, defaulting to `~/.config` on every platform.
pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    base.join("heylisten").join("config.toml")
}

/// The editable summary prompt, next to the config file.
pub fn prompt_path() -> PathBuf {
    config_path().with_file_name("summary-prompt.md")
}

/// heyListen's own folder in the platform data dir.
pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(home).join("heylisten")
}

/// Where meeting folders live.
pub fn meetings_dir() -> PathBuf {
    data_dir().join("meetings")
}

/// Where `setup` puts models.
pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

fn home() -> PathBuf {
    dirs::home_dir().expect("no home directory")
}

fn expand_home(path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => home().join(rest),
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_url(url: &str) -> Config {
        let mut c: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        c.ollama_url = url.into();
        c
    }

    #[test]
    fn default_config_parses() {
        toml::from_str::<Config>(DEFAULT_CONFIG).unwrap();
    }

    #[test]
    fn old_vault_key_still_works() {
        let c: Config = toml::from_str("vault = \"~/Notes\"").unwrap();
        assert_eq!(c.notes_dir, PathBuf::from("~/Notes"));
        assert_eq!(c.ollama_url, "http://localhost:11434");
    }

    #[test]
    fn detects_local_ollama() {
        for url in ["http://localhost:11434", "http://127.0.0.1:11434/", "http://[::1]:11434"] {
            assert!(with_url(url).ollama_is_local(), "{url}");
        }
        for url in ["http://192.168.1.10:11434", "https://ollama.example.com", "http://localhost.evil.com"] {
            assert!(!with_url(url).ollama_is_local(), "{url}");
        }
    }
}
