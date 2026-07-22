//! Invoke Grok via CLI session (SuperGrok OAuth). Never uses XAI_API_KEY.

use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use research_core::config::GrokSessionConfig;
use research_core::vault;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{info, warn};

/// Structured result we ask Grok to return as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiIngestResult {
    pub project_slug: String,
    pub project_title: String,
    pub note_title: String,
    pub summary: String,
    #[serde(default)]
    pub entities: Vec<String>,
    /// Obsidian-ready markdown body (with footnotes).
    pub markdown: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl AiIngestResult {
    /// Validate and normalize fields. Returns Err if critically incomplete.
    pub fn validate_and_normalize(mut self) -> Result<Self> {
        self.project_slug = vault::slugify(&self.project_slug);
        if self.project_slug.is_empty() {
            self.project_slug = "inbox".into();
        }
        self.project_title = self.project_title.trim().to_string();
        self.note_title = self.note_title.trim().to_string();
        self.summary = self.summary.trim().to_string();
        self.markdown = self.markdown.trim().to_string();

        if self.project_title.is_empty() {
            self.project_title = self.project_slug.replace('-', " ");
        }
        if self.note_title.is_empty() {
            bail!("note_title is empty");
        }
        if self.summary.is_empty() {
            bail!("summary is empty");
        }
        if self.markdown.chars().count() < 40 {
            bail!("markdown body too short");
        }
        // Dedupe tags/entities, trim.
        self.entities = normalize_list(self.entities);
        self.tags = normalize_list(self.tags);
        if self.tags.is_empty() {
            self.tags.push("ingest".into());
        }
        Ok(self)
    }
}

fn normalize_list(items: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for i in items {
        let t = i.trim().to_string();
        if t.is_empty() {
            continue;
        }
        if !out.iter().any(|x: &String| x.eq_ignore_ascii_case(&t)) {
            out.push(t);
        }
    }
    out
}

pub async fn run_ingest_ai(
    grok: &GrokSessionConfig,
    system_prompt: &str,
    user_payload: &str,
) -> Result<AiIngestResult> {
    let schema = OUTPUT_SCHEMA;
    let mut last_err = None;
    let attempts = grok.max_retries.saturating_add(1);

    for attempt in 1..=attempts {
        let repair = if let Some(err) = &last_err {
            format!(
                "\n\n# Repair instruction\n\
                 Your previous reply failed validation: {err}\n\
                 Return ONLY one valid JSON object. No markdown fences. No prose.\n"
            )
        } else {
            String::new()
        };

        let prompt = format!(
            "{system_prompt}\n\n\
             ---\n\n\
             # Output contract (mandatory)\n\
             {schema}\n\n\
             ---\n\n\
             # Input to process\n\n\
             {user_payload}\n\
             {repair}\n\
             # Final instruction\n\
             Reply with a single JSON object only. Do not wrap in markdown fences.\n\
             Do not call tools. Do not ask questions.\n"
        );

        info!("Grok ingest attempt {attempt}/{attempts}");
        let raw = match run_grok_prompt(grok, &prompt).await {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(format!("spawn/run: {e:#}"));
                warn!("Grok run failed attempt {attempt}: {e:#}");
                continue;
            }
        };

        match parse_ai_result(&raw).and_then(|r| r.validate_and_normalize()) {
            Ok(ok) => return Ok(ok),
            Err(e) => {
                last_err = Some(e.to_string());
                warn!("Grok parse/validate failed attempt {attempt}: {e:#}");
            }
        }
    }

    bail!(
        "Grok session failed after {attempts} attempt(s): {}",
        last_err.unwrap_or_else(|| "unknown".into())
    )
}

const OUTPUT_SCHEMA: &str = r#"Return exactly one JSON object with these keys:
{
  "project_slug": "kebab-case-string",
  "project_title": "Human title for the project",
  "note_title": "Title for this note",
  "summary": "1-3 sentence factual summary",
  "entities": ["Name", "Org", "Product"],
  "markdown": "Full Obsidian note body with ## headings, key points, and footnotes like [^1]\n\n[^1]: source",
  "tags": ["topic", "source-type"]
}

Rules:
- project_slug: lowercase a-z 0-9 hyphens only; reuse existing project when listed and it fits.
- Prefer project_slug "inbox" only when no topic is clear.
- markdown: use clear headings; include footnotes that cite URL or file name; no marketing language.
- entities: people, orgs, products, standards, places that matter; empty array if none.
- Do not invent facts that are not in the input.
- Do not include keys other than those listed.
"#;

pub async fn run_grok_prompt(grok: &GrokSessionConfig, prompt: &str) -> Result<String> {
    let dir = research_core::config::state_dir().join("prompts");
    fs::create_dir_all(&dir)?;
    let prompt_path = dir.join(format!("prompt-{}.md", uuid::Uuid::new_v4()));
    fs::write(&prompt_path, prompt)?;

    let mut cmd = Command::new(&grok.binary);
    // Session auth only: strip pay-per-token API key so SuperGrok OAuth wins.
    cmd.arg("--prompt-file")
        .arg(&prompt_path)
        .arg("--output-format")
        .arg("plain")
        .arg("--verbatim")
        .arg("--max-turns")
        .arg("2")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("XAI_API_KEY")
        .env("RESEARCH_INGEST_AI", "1");

    if grok.yolo {
        cmd.arg("--yolo");
    }
    if let Some(model) = &grok.model {
        cmd.arg("-m").arg(model);
    }
    if let Some(effort) = &grok.effort {
        cmd.arg("--effort").arg(effort);
    }
    for a in &grok.extra_args {
        cmd.arg(a);
    }

    info!("running Grok session: {} …", grok.binary);
    let child = cmd.spawn().with_context(|| {
        format!(
            "spawn `{}` (install Grok Build CLI and run `grok login` for SuperGrok session)",
            grok.binary
        )
    })?;

    let output = timeout(
        Duration::from_secs(grok.timeout_secs),
        child.wait_with_output(),
    )
    .await
    .context("grok timed out")?
    .context("wait for grok")?;

    let _ = fs::remove_file(&prompt_path);

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !output.status.success() {
        // Retry without --tools "" if CLI rejects empty tools.
        if stderr.contains("tools") || output.status.code() == Some(2) {
            warn!("grok failed (will note): {}", truncate(&stderr, 400));
        }
        warn!("grok stderr: {stderr}");
        // Still accept stdout if present (some CLIs non-zero with partial output).
        if stdout.trim().is_empty() {
            bail!(
                "grok exited with status {:?}: {}",
                output.status.code(),
                truncate(&stderr, 800)
            );
        }
    }

    if stdout.trim().is_empty() {
        bail!(
            "grok returned empty stdout; stderr={}",
            truncate(&stderr, 400)
        );
    }
    Ok(stdout)
}

fn parse_ai_result(raw: &str) -> Result<AiIngestResult> {
    if let Some(json) = extract_json_block(raw) {
        return serde_json::from_str(json)
            .with_context(|| format!("parse AI JSON: {}", truncate(json, 200)));
    }
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).context("parse AI JSON root");
    }
    bail!("response is not JSON: {}", truncate(trimmed, 180));
}

fn extract_json_block(raw: &str) -> Option<&str> {
    if let Some(start) = raw.find("```json") {
        let after = &raw[start + 7..];
        let after = after.strip_prefix('\n').unwrap_or(after);
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim());
        }
    }
    if let Some(start) = raw.find("```") {
        let after = &raw[start + 3..];
        let after = after
            .strip_prefix('\n')
            .or_else(|| after.find('\n').map(|i| &after[i + 1..]))
            .unwrap_or(after);
        if let Some(end) = after.find("```") {
            let block = after[..end].trim();
            if block.starts_with('{') {
                return Some(block);
            }
        }
    }
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end > start {
        Some(&raw[start..=end])
    } else {
        None
    }
}

fn truncate(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

/// Load ingest system prompt from config path or embedded default.
pub fn load_system_prompt(override_path: &Option<PathBuf>) -> String {
    if let Some(p) = override_path {
        if let Ok(t) = fs::read_to_string(p) {
            return t;
        }
    }
    for candidate in [
        PathBuf::from("prompts/ingest_system.md"),
        research_core::config::config_dir().join("ingest_system.md"),
    ] {
        if let Ok(t) = fs::read_to_string(&candidate) {
            return t;
        }
    }
    DEFAULT_INGEST_PROMPT.to_string()
}

pub const DEFAULT_INGEST_PROMPT: &str = r#"You are the research ingest assistant for a local Obsidian vault.
You run on a Grok SuperGrok subscription session (Grok Build login).
You do not use a pay-per-token console API.

## Role

Convert raw research captures (web clips, PDFs, OCR text, media transcripts) into durable project notes.

## Rules

1. Read only the provided input. Do not invent sources, quotes, or facts.
2. Choose project_slug (kebab-case) and project_title.
   Reuse an existing project from the list when the topic fits.
3. Write markdown that a human can scan: short summary, key points, entities, footnotes.
4. Footnotes must cite the source URL and/or source file name when present.
5. Use precise technical language. Do not use marketing tone, slang, or filler.
6. If the input is thin, still write a short honest note. Use project_slug "inbox" when no project fits.
7. When the input includes a transcript section, treat it as primary evidence and quote carefully.
8. When the input is OCR, note that text may contain recognition errors; do not silently "fix" uncertain words into false facts.

## Output

Return one JSON object only, matching the output contract in the user message.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fenced_json() {
        let raw = "Here you go:\n```json\n{\n  \"project_slug\": \"elevator-modernization\",\n  \"project_title\": \"Elevator Modernization\",\n  \"note_title\": \"Code notes\",\n  \"summary\": \"Summary of elevator code updates for modernization projects.\",\n  \"entities\": [\"ASME A17.1\"],\n  \"markdown\": \"## Key points\\n\\n- Point one about modernization.\\n- Point two about safety.\\n\\n[^1]: source\\n\",\n  \"tags\": [\"elevators\"]\n}\n```\n";
        let r = parse_ai_result(raw)
            .unwrap()
            .validate_and_normalize()
            .unwrap();
        assert_eq!(r.project_slug, "elevator-modernization");
        assert!(!r.markdown.is_empty());
    }

    #[test]
    fn reject_short_markdown() {
        let r = AiIngestResult {
            project_slug: "x".into(),
            project_title: "X".into(),
            note_title: "N".into(),
            summary: "S".into(),
            entities: vec![],
            markdown: "too short".into(),
            tags: vec![],
        };
        assert!(r.validate_and_normalize().is_err());
    }
}
