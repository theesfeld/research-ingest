//! External tool discovery for OCR and transcripts.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::config::{expand_tilde, state_dir};

/// Paths and toggles for OCR / media tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    #[serde(default = "default_true")]
    pub enable_ocr: bool,
    #[serde(default = "default_true")]
    pub enable_transcript: bool,
    /// Tesseract binary (name or absolute path).
    #[serde(default)]
    pub tesseract: Option<String>,
    /// OCR language(s), e.g. `eng` or `eng+deu`.
    #[serde(default = "default_ocr_lang")]
    pub ocr_lang: String,
    #[serde(default)]
    pub ffmpeg: Option<String>,
    #[serde(default)]
    pub ffprobe: Option<String>,
    /// whisper.cpp CLI (`whisper-cli`).
    #[serde(default)]
    pub whisper: Option<String>,
    /// Path to ggml model (e.g. `ggml-base.en.bin`).
    #[serde(default)]
    pub whisper_model: Option<PathBuf>,
    /// Whisper language (`en`, `auto`, …).
    #[serde(default = "default_whisper_lang")]
    pub whisper_lang: String,
    /// Whisper threads (0 = leave default).
    #[serde(default)]
    pub whisper_threads: u32,
    /// Max seconds of media to transcribe (0 = full file).
    #[serde(default)]
    pub transcript_max_secs: u64,
}

fn default_true() -> bool {
    true
}
fn default_ocr_lang() -> String {
    "eng".into()
}
fn default_whisper_lang() -> String {
    "en".into()
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enable_ocr: true,
            enable_transcript: true,
            tesseract: None,
            ocr_lang: default_ocr_lang(),
            ffmpeg: None,
            ffprobe: None,
            whisper: None,
            whisper_model: None,
            whisper_lang: default_whisper_lang(),
            whisper_threads: 0,
            transcript_max_secs: 0,
        }
    }
}

impl ToolsConfig {
    /// Resolve binaries and model paths (PATH + defaults).
    pub fn resolve(&self) -> ResolvedTools {
        let tesseract = resolve_bin(self.tesseract.as_deref(), &["tesseract"]);
        let ffmpeg = resolve_bin(self.ffmpeg.as_deref(), &["ffmpeg"]);
        let ffprobe = resolve_bin(self.ffprobe.as_deref(), &["ffprobe"]);
        let whisper = resolve_bin(self.whisper.as_deref(), &["whisper-cli", "whisper"]);
        let whisper_model = resolve_model(self.whisper_model.as_ref());

        ResolvedTools {
            enable_ocr: self.enable_ocr,
            enable_transcript: self.enable_transcript,
            tesseract,
            ocr_lang: self.ocr_lang.clone(),
            ffmpeg,
            ffprobe,
            whisper,
            whisper_model,
            whisper_lang: self.whisper_lang.clone(),
            whisper_threads: self.whisper_threads,
            transcript_max_secs: self.transcript_max_secs,
        }
    }
}

/// Concrete tool paths after discovery.
#[derive(Debug, Clone)]
pub struct ResolvedTools {
    pub enable_ocr: bool,
    pub enable_transcript: bool,
    pub tesseract: Option<PathBuf>,
    pub ocr_lang: String,
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
    pub whisper: Option<PathBuf>,
    pub whisper_model: Option<PathBuf>,
    pub whisper_lang: String,
    pub whisper_threads: u32,
    pub transcript_max_secs: u64,
}

impl ResolvedTools {
    pub fn doctor_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!(
            "ocr: {} tesseract={}",
            on_off(self.enable_ocr),
            disp(&self.tesseract)
        ));
        lines.push(format!("ocr_lang: {}", self.ocr_lang));
        lines.push(format!("ffmpeg: {}", disp(&self.ffmpeg)));
        lines.push(format!("ffprobe: {}", disp(&self.ffprobe)));
        lines.push(format!(
            "transcript: {} whisper={}",
            on_off(self.enable_transcript),
            disp(&self.whisper)
        ));
        lines.push(format!("whisper_model: {}", disp(&self.whisper_model)));
        lines.push(format!("whisper_lang: {}", self.whisper_lang));
        if self.enable_ocr && self.tesseract.is_none() {
            lines.push(
                "WARN: OCR enabled but tesseract not found (nix profile install nixpkgs#tesseract)"
                    .into(),
            );
        }
        if self.enable_transcript {
            if self.whisper.is_none() {
                lines.push(
                    "WARN: transcript enabled but whisper-cli not found (nixpkgs#whisper-cpp)"
                        .into(),
                );
            }
            if self.whisper_model.is_none() {
                lines.push(format!(
                    "WARN: no whisper model; place ggml-*.bin in {} or set tools.whisper_model",
                    state_dir().join("models").display()
                ));
            }
            if self.ffmpeg.is_none() {
                lines.push(
                    "WARN: ffmpeg missing; video→audio extract needs ffmpeg (nixpkgs#ffmpeg)"
                        .into(),
                );
            }
        }
        lines
    }
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

fn disp(p: &Option<PathBuf>) -> String {
    p.as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(missing)".into())
}

fn resolve_bin(configured: Option<&str>, names: &[&str]) -> Option<PathBuf> {
    if let Some(c) = configured {
        let p = expand_tilde(PathBuf::from(c));
        if p.is_file() {
            return Some(p);
        }
        if let Some(found) = which(c) {
            return Some(found);
        }
    }
    for name in names {
        if let Some(found) = which(name) {
            return Some(found);
        }
    }
    None
}

fn resolve_model(configured: Option<&PathBuf>) -> Option<PathBuf> {
    if let Some(p) = configured {
        let p = expand_tilde(p.clone());
        if p.is_file() {
            return Some(p);
        }
    }
    let dir = state_dir().join("models");
    // Prefer common names.
    for name in [
        "ggml-base.en.bin",
        "ggml-small.en.bin",
        "ggml-tiny.en.bin",
        "ggml-base.bin",
        "ggml-small.bin",
        "ggml-tiny.bin",
        "ggml-medium.en.bin",
    ] {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    // Any ggml-*.bin
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|e| e.to_str()) == Some("bin")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("ggml-"))
                    .unwrap_or(false)
            {
                return Some(p);
            }
        }
    }
    None
}

/// Locate an executable on PATH (or absolute path).
pub fn which(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() && p.is_file() {
        return Some(p.to_path_buf());
    }
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {}", shell_escape(name)))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

fn shell_escape(s: &str) -> String {
    // Minimal safe quote for command -v.
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.' | '+'))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_defaults() {
        let t = ToolsConfig::default().resolve();
        assert!(t.enable_ocr);
        assert!(t.enable_transcript);
    }
}
