use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

const DEFAULT_CONFIG: &str = include_str!("default-config.toml");
const DEFAULT_PROMPT: &str = include_str!("summary-prompt.md");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub vault: PathBuf,
    pub whisper_model: PathBuf,
    pub ollama_model: String,
    pub ollama_url: String,
    pub keep_audio: bool,
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
        config.vault = expand_home(&config.vault);
        config.whisper_model = expand_home(&config.whisper_model);
        Ok(config)
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

/// Where meeting folders live: the platform data dir.
pub fn meetings_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(home).join("heylisten").join("meetings")
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
    fn detects_local_ollama() {
        for url in ["http://localhost:11434", "http://127.0.0.1:11434/", "http://[::1]:11434"] {
            assert!(with_url(url).ollama_is_local(), "{url}");
        }
        for url in ["http://192.168.1.10:11434", "https://ollama.example.com", "http://localhost.evil.com"] {
            assert!(!with_url(url).ollama_is_local(), "{url}");
        }
    }
}
