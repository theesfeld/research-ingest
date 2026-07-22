//! User and system configuration.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::tools::ToolsConfig;

/// How language work runs. Default is subscription session only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AiBackend {
    /// Headless `grok` CLI with OAuth session (SuperGrok). No API key.
    #[default]
    GrokSession,
    /// Extract and queue only. You run Grok by hand or through MCP.
    QueueOnly,
}

/// Settings for headless Grok Build / Grok CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrokSessionConfig {
    /// Binary name or absolute path. Default: `grok`.
    #[serde(default = "default_grok_bin")]
    pub binary: String,
    /// Model flag value. Default: empty (CLI default).
    #[serde(default)]
    pub model: Option<String>,
    /// Extra CLI args after the prompt flags.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Pass `--yolo` so tools can write without prompts in headless mode.
    #[serde(default = "default_true")]
    pub yolo: bool,
    /// Max seconds to wait for one headless run.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Reasoning effort if supported (`high`, `xhigh`, …).
    #[serde(default)]
    pub effort: Option<String>,
    /// Retries when Grok output fails JSON validation.
    #[serde(default = "default_ai_retries")]
    pub max_retries: u32,
}

fn default_ai_retries() -> u32 {
    2
}

fn default_grok_bin() -> String {
    "grok".into()
}
fn default_true() -> bool {
    true
}
fn default_timeout_secs() -> u64 {
    600
}

impl Default for GrokSessionConfig {
    fn default() -> Self {
        Self {
            binary: default_grok_bin(),
            model: None,
            extra_args: Vec::new(),
            yolo: true,
            timeout_secs: default_timeout_secs(),
            effort: Some("high".into()),
            max_retries: default_ai_retries(),
        }
    }
}

/// Root application config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    /// Absolute path to the Obsidian vault root.
    pub vault_path: PathBuf,
    /// AI backend. Default: grok-session (no xAI API keys).
    #[serde(default)]
    pub ai_backend: AiBackend,
    #[serde(default)]
    pub grok: GrokSessionConfig,
    /// Process existing files in `raw/incoming` on start.
    #[serde(default = "default_true")]
    pub process_existing_on_start: bool,
    /// Debounce milliseconds for filesystem events.
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    /// Path to the system prompt file for ingestion (optional override).
    #[serde(default)]
    pub ingest_prompt_path: Option<PathBuf>,
    /// Max characters of extracted text to send to Grok per item.
    #[serde(default = "default_max_extract_chars")]
    pub max_extract_chars: usize,
    /// OCR / ffmpeg / whisper tools.
    #[serde(default)]
    pub tools: ToolsConfig,
    /// Local HTTP drop for the browser extension (always-on path).
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
    /// Enable the local HTTP drop server while watching.
    #[serde(default = "default_true")]
    pub listen_http: bool,
}

fn default_listen_addr() -> String {
    "127.0.0.1:18765".into()
}

fn default_debounce_ms() -> u64 {
    400
}
fn default_max_extract_chars() -> usize {
    48_000
}

impl Default for Config {
    fn default() -> Self {
        Self {
            vault_path: default_vault_path(),
            ai_backend: AiBackend::GrokSession,
            grok: GrokSessionConfig::default(),
            process_existing_on_start: true,
            debounce_ms: default_debounce_ms(),
            ingest_prompt_path: None,
            max_extract_chars: default_max_extract_chars(),
            tools: ToolsConfig::default(),
            listen_addr: default_listen_addr(),
            listen_http: true,
        }
    }
}

/// Default vault: `~/Documents/Obsidian Vault`.
pub fn default_vault_path() -> PathBuf {
    directories::UserDirs::new()
        .map(|u| u.home_dir().join("Documents").join("Obsidian Vault"))
        .unwrap_or_else(|| PathBuf::from("Obsidian Vault"))
}

/// Config directory: `~/.config/research-ingest/`.
pub fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("dev", "theesfeld", "research-ingest")
        .map(|p| p.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".config/research-ingest"))
}

/// State directory for the job queue: `~/.local/share/research-ingest/`.
pub fn state_dir() -> PathBuf {
    directories::ProjectDirs::from("dev", "theesfeld", "research-ingest")
        .map(|p| p.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".local/share/research-ingest"))
}

impl Config {
    pub fn config_file_path() -> PathBuf {
        config_dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_file_path();
        if !path.exists() {
            let cfg = Self::default();
            cfg.ensure_written()?;
            return Ok(cfg);
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("read config {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).context("parse config.toml")?;
        Ok(cfg)
    }

    pub fn load_or_default() -> Self {
        Self::load().unwrap_or_default()
    }

    /// Write default config if missing. Does not overwrite.
    pub fn ensure_written(&self) -> Result<()> {
        let dir = config_dir();
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let path = Self::config_file_path();
        if path.exists() {
            return Ok(());
        }
        let text = toml::to_string_pretty(self).context("serialize config")?;
        fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
        tracing::info!("wrote default config {}", path.display());
        Ok(())
    }

    pub fn with_vault(mut self, vault: impl Into<PathBuf>) -> Self {
        self.vault_path = self.expand_home(vault.into());
        self
    }

    fn expand_home(&self, path: PathBuf) -> PathBuf {
        expand_tilde(path)
    }
}

/// Expand a leading `~` to the home directory.
pub fn expand_tilde(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = directories::UserDirs::new() {
            return home.home_dir().join(rest);
        }
    }
    if s == "~" {
        if let Some(home) = directories::UserDirs::new() {
            return home.home_dir().to_path_buf();
        }
    }
    path
}

/// Resolve vault from CLI override or config.
pub fn resolve_vault(cli_vault: Option<&Path>) -> PathBuf {
    if let Some(p) = cli_vault {
        return expand_tilde(p.to_path_buf());
    }
    Config::load_or_default().vault_path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backend_is_session() {
        assert_eq!(Config::default().ai_backend, AiBackend::GrokSession);
    }

    #[test]
    fn expand_tilde_home() {
        let p = expand_tilde(PathBuf::from("~/Documents/x"));
        assert!(!p.to_string_lossy().starts_with('~'));
        assert!(p.ends_with("Documents/x"));
    }
}
