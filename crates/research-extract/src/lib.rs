//! Content extraction for research ingest.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use research_core::ContentKind;
use serde::{Deserialize, Serialize};

/// Result of extraction from one source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extracted {
    pub title: Option<String>,
    pub source_url: Option<String>,
    pub text: String,
    pub kind: ContentKind,
    pub notes: Vec<String>,
}

/// Browser / native-messaging envelope written as JSON into incoming/.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserPayload {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub selection: Option<String>,
    #[serde(default)]
    pub page_markdown: Option<String>,
    #[serde(default)]
    pub page_text: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub captured_at: Option<String>,
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

pub fn extract_file(path: &Path) -> Result<Extracted> {
    let kind = ContentKind::from_path(path);
    match kind {
        ContentKind::Pdf => extract_pdf(path),
        ContentKind::Image => extract_image(path),
        ContentKind::Video | ContentKind::Audio => extract_media(path, kind),
        ContentKind::Html => extract_html(path),
        ContentKind::UrlClip | ContentKind::Unknown => {
            // Try JSON browser payload first.
            if let Ok(text) = fs::read_to_string(path) {
                if let Ok(payload) = serde_json::from_str::<BrowserPayload>(&text) {
                    return Ok(extracted_from_payload(payload));
                }
                if looks_like_text(&text) {
                    return Ok(Extracted {
                        title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
                        source_url: None,
                        text,
                        kind: ContentKind::Text,
                        notes: vec![],
                    });
                }
            }
            extract_plain(path, kind)
        }
        ContentKind::Text | ContentKind::Markdown => extract_plain(path, kind),
    }
}

fn extracted_from_payload(p: BrowserPayload) -> Extracted {
    let mut parts = Vec::new();
    if let Some(sel) = &p.selection {
        if !sel.trim().is_empty() {
            parts.push(format!("## Selection\n\n{sel}"));
        }
    }
    if let Some(md) = &p.page_markdown {
        if !md.trim().is_empty() {
            parts.push(format!("## Page (Markdown)\n\n{md}"));
        }
    } else if let Some(t) = &p.page_text {
        if !t.trim().is_empty() {
            parts.push(format!("## Page text\n\n{t}"));
        }
    }
    if let Some(img) = &p.image_url {
        parts.push(format!("## Image URL\n\n{img}"));
    }
    let mut notes = Vec::new();
    if let Some(ct) = &p.content_type {
        notes.push(format!("content_type={ct}"));
    }
    Extracted {
        title: p.title,
        source_url: p.url,
        text: parts.join("\n\n"),
        kind: ContentKind::UrlClip,
        notes,
    }
}

fn extract_plain(path: &Path, kind: ContentKind) -> Result<Extracted> {
    let text = fs::read_to_string(path).with_context(|| format!("read text {}", path.display()))?;
    Ok(Extracted {
        title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
        source_url: None,
        text,
        kind,
        notes: vec![],
    })
}

fn extract_html(path: &Path) -> Result<Extracted> {
    let raw = fs::read_to_string(path)?;
    // Minimal tag strip (good enough for clips; not a full HTML parser).
    let text = strip_tags(&raw);
    Ok(Extracted {
        title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
        source_url: None,
        text,
        kind: ContentKind::Html,
        notes: vec!["html tags stripped with a simple filter".into()],
    })
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_pdf(path: &Path) -> Result<Extracted> {
    let bytes = fs::read(path).with_context(|| format!("read pdf {}", path.display()))?;
    let text = pdf_extract::extract_text_from_mem(&bytes)
        .with_context(|| format!("pdf extract {}", path.display()))?;
    Ok(Extracted {
        title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
        source_url: None,
        text,
        kind: ContentKind::Pdf,
        notes: vec!["pdf-extract".into()],
    })
}

fn extract_image(path: &Path) -> Result<Extracted> {
    let mut notes = Vec::new();
    // Optional tesseract if present on PATH.
    if command_exists("tesseract") {
        let output = Command::new("tesseract")
            .arg(path)
            .arg("stdout")
            .arg("-l")
            .arg("eng")
            .output()
            .context("run tesseract")?;
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            notes.push("ocr=tesseract".into());
            return Ok(Extracted {
                title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
                source_url: None,
                text,
                kind: ContentKind::Image,
                notes,
            });
        }
        notes.push(format!(
            "tesseract failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    } else {
        notes.push("tesseract not installed; image text not extracted".into());
    }
    Ok(Extracted {
        title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
        source_url: None,
        text: format!(
            "(Image file: {}. Install tesseract for OCR.)",
            path.display()
        ),
        kind: ContentKind::Image,
        notes,
    })
}

fn extract_media(path: &Path, kind: ContentKind) -> Result<Extracted> {
    let mut notes = Vec::new();
    let mut text = String::new();

    if command_exists("ffprobe") {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path)
            .output()
            .context("run ffprobe")?;
        if output.status.success() {
            text.push_str("## Media metadata (ffprobe)\n\n```json\n");
            text.push_str(&String::from_utf8_lossy(&output.stdout));
            text.push_str("\n```\n");
            notes.push("ffprobe".into());
        }
    } else {
        notes.push("ffprobe not installed".into());
    }

    // Optional whisper.cpp binary if user installs it as `whisper-cli` or `whisper`.
    for bin in ["whisper-cli", "whisper"] {
        if command_exists(bin) {
            notes.push(format!(
                "found {bin}; auto transcript not wired in 0.1.0-dev — run by hand or enable later"
            ));
            break;
        }
    }

    if text.is_empty() {
        text = format!(
            "(Media file: {}. Install ffmpeg/ffprobe for metadata. Optional whisper binary for transcripts.)",
            path.display()
        );
    }

    Ok(Extracted {
        title: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
        source_url: None,
        text,
        kind,
        notes,
    })
}

fn command_exists(name: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn looks_like_text(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let sample: String = s.chars().take(512).collect();
    let bad = sample
        .bytes()
        .filter(|b| *b < 9 || (*b > 13 && *b < 32))
        .count();
    bad < 8
}

/// Truncate text for AI prompts.
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let t: String = text.chars().take(max).collect();
    format!("{t}\n\n…[truncated for length]\n")
}

/// Heuristic project slug from title/url/text when Grok is offline.
pub fn heuristic_project(title: Option<&str>, url: Option<&str>, text: &str) -> (String, String) {
    if let Some(t) = title {
        if t.trim().len() >= 3 {
            let slug = research_core::vault::slugify(t);
            return (slug, t.trim().to_string());
        }
    }
    if let Some(u) = url {
        if let Some(host) = url_host(u) {
            let slug = research_core::vault::slugify(&host);
            return (slug, host);
        }
    }
    // First non-empty line of text.
    for line in text.lines() {
        let line = line.trim().trim_start_matches('#').trim();
        if line.len() >= 8 && line.len() <= 80 {
            let slug = research_core::vault::slugify(line);
            return (slug, line.to_string());
        }
    }
    ("inbox".into(), "Inbox".into())
}

fn url_host(url: &str) -> Option<String> {
    let u = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = u.split('/').next()?;
    Some(host.trim_start_matches("www.").to_string())
}

pub fn require_readable(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("missing file {}", path.display());
    }
    if !path.is_file() {
        bail!("not a file {}", path.display());
    }
    Ok(())
}
