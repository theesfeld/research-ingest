//! Invoke Grok via CLI session (SuperGrok OAuth). Never uses XAI_API_KEY.

use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use research_core::config::GrokSessionConfig;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{info, warn};

/// Structured result we ask Grok to return as JSON in a fenced block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiIngestResult {
    pub project_slug: String,
    pub project_title: String,
    pub note_title: String,
    pub summary: String,
    pub entities: Vec<String>,
    /// Obsidian-ready markdown body (with footnotes).
    pub markdown: String,
    pub tags: Vec<String>,
}

pub async fn run_ingest_ai(
    grok: &GrokSessionConfig,
    system_prompt: &str,
    user_payload: &str,
) -> Result<AiIngestResult> {
    let prompt = format!(
        "{system_prompt}\n\n---\n\n# Input to process\n\n{user_payload}\n\n---\n\n\
         Reply with a single JSON object only (no prose outside JSON) with keys:\n\
         project_slug, project_title, note_title, summary, entities (array of strings),\n\
         markdown (Obsidian note body with footnotes), tags (array of strings).\n\
         project_slug must be lowercase kebab-case.\n"
    );

    let raw = run_grok_prompt(grok, &prompt).await?;
    parse_ai_result(&raw)
}

pub async fn run_grok_prompt(grok: &GrokSessionConfig, prompt: &str) -> Result<String> {
    // Prefer prompt file to avoid argv length limits.
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
        warn!("grok stderr: {stderr}");
        bail!(
            "grok exited with status {:?}: {}",
            output.status.code(),
            truncate(&stderr, 800)
        );
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
    // Prefer fenced ```json block.
    if let Some(json) = extract_json_block(raw) {
        return serde_json::from_str(json)
            .with_context(|| format!("parse AI JSON: {}", truncate(json, 200)));
    }
    // Whole response as JSON.
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).context("parse AI JSON root");
    }
    // Fallback: wrap freeform markdown.
    warn!("AI response was not JSON; wrapping as note body");
    Ok(AiIngestResult {
        project_slug: "inbox".into(),
        project_title: "Inbox".into(),
        note_title: "Ingest note".into(),
        summary: first_line(raw),
        entities: vec![],
        markdown: raw.to_string(),
        tags: vec!["ingest".into()],
    })
}

fn extract_json_block(raw: &str) -> Option<&str> {
    if let Some(start) = raw.find("```json") {
        let after = &raw[start + 7..];
        let after = after.strip_prefix('\n').unwrap_or(after);
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim());
        }
    }
    // First { … last }
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end > start {
        Some(&raw[start..=end])
    } else {
        None
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("summary")
        .chars()
        .take(200)
        .collect()
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
    // Optional prompts next to the binary install or current directory.
    for candidate in [
        PathBuf::from("prompts/ingest_system.md"),
        research_core::config::config_dir().join("ingest_system.md"),
    ] {
        if let Ok(t) = fs::read_to_string(candidate) {
            return t;
        }
    }
    DEFAULT_INGEST_PROMPT.to_string()
}

pub const DEFAULT_INGEST_PROMPT: &str = r#"You are the research ingest assistant for a local Obsidian vault.
You work from a Grok subscription session (not a pay-per-token API key path).

Tasks:
1. Read the source material and metadata.
2. Choose a project_slug (kebab-case) and project_title. Reuse an existing project when the list is provided and it fits.
3. Write a clear Obsidian markdown note with:
   - short summary
   - key points
   - entities (people, orgs, products, standards)
   - footnotes / citations that point at the source URL or file name
   - wikilinks when you reference related project concepts
4. Prefer precise technical language. Do not use marketing tone.
5. If content is thin, still produce a short honest note; put it in project_slug "inbox" when no project fits.

Output: one JSON object only as specified by the user message.
"#;
