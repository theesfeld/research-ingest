//! Content extraction for research ingest (PDF, OCR, media transcript).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use research_core::tools::ResolvedTools;
use research_core::ContentKind;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

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

pub fn extract_file(path: &Path, tools: &ResolvedTools) -> Result<Extracted> {
    let kind = ContentKind::from_path(path);
    match kind {
        ContentKind::Pdf => extract_pdf(path),
        ContentKind::Image => extract_image(path, tools),
        ContentKind::Video | ContentKind::Audio => extract_media(path, kind, tools),
        ContentKind::Html => extract_html(path),
        ContentKind::UrlClip | ContentKind::Unknown => {
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

fn extract_image(path: &Path, tools: &ResolvedTools) -> Result<Extracted> {
    let mut notes = Vec::new();
    let title = path.file_stem().map(|s| s.to_string_lossy().into_owned());

    if !tools.enable_ocr {
        notes.push("ocr disabled in config".into());
        return Ok(Extracted {
            title,
            source_url: None,
            text: format!("(Image file: {}. OCR disabled.)", path.display()),
            kind: ContentKind::Image,
            notes,
        });
    }

    let Some(tess) = tools.tesseract.as_ref() else {
        notes.push("tesseract not found".into());
        return Ok(Extracted {
            title,
            source_url: None,
            text: format!(
                "(Image file: {}. Install tesseract for OCR, e.g. `nix profile add nixpkgs#tesseract`.)",
                path.display()
            ),
            kind: ContentKind::Image,
            notes,
        });
    };

    info!("OCR {} with {}", path.display(), tess.display());
    let output = Command::new(tess)
        .arg(path)
        .arg("stdout")
        .arg("-l")
        .arg(&tools.ocr_lang)
        .arg("--psm")
        .arg("3")
        .output()
        .with_context(|| format!("run tesseract {}", tess.display()))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        notes.push(format!("tesseract failed: {}", err.trim()));
        warn!("tesseract failed: {}", err.trim());
        return Ok(Extracted {
            title,
            source_url: None,
            text: format!("(OCR failed for {}.)\n{}", path.display(), err),
            kind: ContentKind::Image,
            notes,
        });
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    notes.push(format!(
        "ocr=tesseract lang={} bin={}",
        tools.ocr_lang,
        tess.display()
    ));
    Ok(Extracted {
        title,
        source_url: None,
        text: if text.is_empty() {
            "(OCR returned empty text.)".into()
        } else {
            text
        },
        kind: ContentKind::Image,
        notes,
    })
}

fn extract_media(path: &Path, kind: ContentKind, tools: &ResolvedTools) -> Result<Extracted> {
    let mut notes = Vec::new();
    let mut parts = Vec::new();
    let title = path.file_stem().map(|s| s.to_string_lossy().into_owned());

    // Metadata via ffprobe when available.
    if let Some(ffprobe) = tools.ffprobe.as_ref() {
        let output = Command::new(ffprobe)
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
            .with_context(|| format!("run ffprobe {}", ffprobe.display()))?;
        if output.status.success() {
            let meta = String::from_utf8_lossy(&output.stdout);
            parts.push(format!(
                "## Media metadata (ffprobe)\n\n```json\n{}\n```",
                meta.trim()
            ));
            notes.push(format!("ffprobe={}", ffprobe.display()));
        } else {
            notes.push(format!(
                "ffprobe failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    } else {
        notes.push("ffprobe not found".into());
    }

    // Auto transcript.
    match run_transcript(path, kind, tools) {
        Ok(Some((transcript, tnotes))) => {
            parts.push(format!("## Transcript\n\n{}", transcript.trim()));
            notes.extend(tnotes);
        }
        Ok(None) => {
            notes.push("transcript skipped".into());
        }
        Err(e) => {
            warn!("transcript error: {e:#}");
            notes.push(format!("transcript error: {e:#}"));
            parts.push(format!("## Transcript\n\n(Transcript failed: {e:#})"));
        }
    }

    let text = if parts.is_empty() {
        format!(
            "(Media file: {}. Install ffmpeg + whisper-cli + model for transcripts.)",
            path.display()
        )
    } else {
        parts.join("\n\n")
    };

    Ok(Extracted {
        title,
        source_url: None,
        text,
        kind,
        notes,
    })
}

/// Run whisper.cpp on audio or extracted video audio.
fn run_transcript(
    path: &Path,
    kind: ContentKind,
    tools: &ResolvedTools,
) -> Result<Option<(String, Vec<String>)>> {
    if !tools.enable_transcript {
        return Ok(None);
    }
    let Some(whisper) = tools.whisper.as_ref() else {
        return Ok(None);
    };
    let Some(model) = tools.whisper_model.as_ref() else {
        return Ok(None);
    };

    let mut notes = vec![
        format!("whisper={}", whisper.display()),
        format!("model={}", model.display()),
    ];

    let work = research_core::config::state_dir().join("tmp");
    fs::create_dir_all(&work)?;
    let id = uuid_simple();
    let wav_path = work.join(format!("{id}.wav"));
    let out_base = work.join(format!("{id}-out"));

    // Prepare 16 kHz mono wav.
    let audio_in = match kind {
        ContentKind::Audio if is_direct_whisper_input(path) => {
            // Still normalize with ffmpeg when available for best results.
            if let Some(ffmpeg) = tools.ffmpeg.as_ref() {
                ffmpeg_to_wav(ffmpeg, path, &wav_path, tools.transcript_max_secs)?;
                notes.push("ffmpeg=normalize".into());
                wav_path.clone()
            } else {
                path.to_path_buf()
            }
        }
        ContentKind::Audio | ContentKind::Video => {
            let Some(ffmpeg) = tools.ffmpeg.as_ref() else {
                bail!("ffmpeg required to extract audio from {}", path.display());
            };
            ffmpeg_to_wav(ffmpeg, path, &wav_path, tools.transcript_max_secs)?;
            notes.push("ffmpeg=extract-audio".into());
            wav_path.clone()
        }
        _ => bail!("not media"),
    };

    info!(
        "transcribe {} with {} model {}",
        audio_in.display(),
        whisper.display(),
        model.display()
    );

    let mut cmd = Command::new(whisper);
    cmd.arg("-m")
        .arg(model)
        .arg("-f")
        .arg(&audio_in)
        .arg("-l")
        .arg(&tools.whisper_lang)
        .arg("-otxt")
        .arg("-of")
        .arg(&out_base);

    if tools.whisper_threads > 0 {
        cmd.arg("-t").arg(tools.whisper_threads.to_string());
    }

    let output = cmd
        .output()
        .with_context(|| format!("run whisper {}", whisper.display()))?;

    // whisper writes to stdout and/or .txt
    let txt_path = PathBuf::from(format!("{}.txt", out_base.display()));
    let mut transcript = if txt_path.is_file() {
        fs::read_to_string(&txt_path).unwrap_or_default()
    } else {
        String::new()
    };

    if transcript.trim().is_empty() {
        // Fall back to stdout (timestamped lines).
        transcript = String::from_utf8_lossy(&output.stdout).into_owned();
        // Keep only segment lines if present.
        if transcript.contains("-->") {
            let lines: Vec<&str> = transcript
                .lines()
                .filter(|l| {
                    l.contains("-->")
                        || (!l.starts_with("whisper_")
                            && !l.starts_with("system_info")
                            && !l.starts_with("main:"))
                })
                .collect();
            // Prefer pure text from bracket lines: [00:00:00.000 --> 00:00:03.720]  text
            let mut cleaned = String::new();
            for l in transcript.lines() {
                if let Some(idx) = l.find("] ") {
                    if l.contains("-->") {
                        cleaned.push_str(l[idx + 2..].trim());
                        cleaned.push(' ');
                    }
                }
            }
            if !cleaned.trim().is_empty() {
                transcript = cleaned.trim().to_string();
            } else if !lines.is_empty() {
                transcript = lines.join("\n");
            }
        }
    }

    if !output.status.success() && transcript.trim().is_empty() {
        let err = String::from_utf8_lossy(&output.stderr);
        bail!("whisper failed: {}", err.trim());
    }

    notes.push(format!("transcript_chars={}", transcript.chars().count()));

    // Cleanup temps.
    let _ = fs::remove_file(&wav_path);
    let _ = fs::remove_file(&txt_path);

    if transcript.trim().is_empty() {
        notes.push("transcript empty".into());
        return Ok(Some(("(empty transcript)".into(), notes)));
    }

    Ok(Some((transcript.trim().to_string(), notes)))
}

fn ffmpeg_to_wav(ffmpeg: &Path, input: &Path, out_wav: &Path, max_secs: u64) -> Result<()> {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-y").arg("-i").arg(input);
    if max_secs > 0 {
        cmd.arg("-t").arg(max_secs.to_string());
    }
    cmd.args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"])
        .arg(out_wav);
    let output = cmd
        .output()
        .with_context(|| format!("run ffmpeg {}", ffmpeg.display()))?;
    if !output.status.success() {
        bail!(
            "ffmpeg extract failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if !out_wav.is_file() {
        bail!("ffmpeg did not write {}", out_wav.display());
    }
    Ok(())
}

fn is_direct_whisper_input(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "wav" | "mp3" | "flac" | "ogg"
    )
}

fn uuid_simple() -> String {
    // Avoid extra dep: use timestamp + random from file hash-ish.
    format!(
        "{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        std::process::id()
    )
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
    // Prefer a substantial transcript line for media project naming.
    if let Some(idx) = text.find("## Transcript") {
        let after = &text[idx + "## Transcript".len()..];
        for line in after.lines() {
            let line = line.trim();
            if line.len() >= 24 && line.len() <= 120 && !line.starts_with('(') {
                let slug = research_core::vault::slugify(line);
                if slug != "inbox" {
                    return (slug, line.to_string());
                }
            }
        }
    }
    if let Some(t) = title {
        let t = t.trim();
        if t.len() >= 3 && !matches!(t, "image" | "video" | "audio" | "clip") {
            let slug = research_core::vault::slugify(t);
            return (slug, t.to_string());
        }
    }
    if let Some(u) = url {
        if let Some(host) = url_host(u) {
            let slug = research_core::vault::slugify(&host);
            return (slug, host);
        }
    }
    for line in text.lines() {
        let line = line.trim().trim_start_matches('#').trim();
        if line.len() >= 8
            && line.len() <= 80
            && !line.starts_with('{')
            && !line.starts_with("Media metadata")
        {
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

#[cfg(test)]
mod tests {
    use super::*;
    use research_core::ToolsConfig;

    #[test]
    fn truncate_works() {
        let s = truncate_chars("abcdef", 3);
        assert!(s.starts_with("abc"));
        assert!(s.contains("truncated"));
    }

    #[test]
    fn extract_plain_md() {
        let dir = std::env::temp_dir().join(format!("ri-ex-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let p = dir.join("a.md");
        fs::write(&p, "# Hello\n\nworld").unwrap();
        let tools = ToolsConfig::default().resolve();
        let ex = extract_file(&p, &tools).unwrap();
        assert!(ex.text.contains("Hello"));
        let _ = fs::remove_dir_all(dir);
    }
}
