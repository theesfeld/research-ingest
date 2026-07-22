//! Process single files and drain the job queue.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use research_core::config::{AiBackend, Config};
use research_core::job::{IngestJob, JobStatus};
use research_core::queue::JobQueue;
use research_core::vault::{self, VaultPaths};
use research_extract::{self, truncate_chars, Extracted};
use tracing::{info, warn};

use crate::grok::{self, AiIngestResult};

pub async fn process_path(cfg: &Config, path: &Path) -> Result<()> {
    let queue = JobQueue::open_default()?;
    let mut job = IngestJob::new(path.to_path_buf());
    queue.put(&job)?;
    run_job(cfg, &queue, &mut job).await?;
    Ok(())
}

pub async fn drain_queue(cfg: &Config, limit: Option<usize>) -> Result<()> {
    let queue = JobQueue::open_default()?;
    let mut n = 0usize;
    while let Some(mut job) = queue.next_pending()? {
        if let Some(lim) = limit {
            if n >= lim {
                break;
            }
        }
        if let Err(e) = run_job(cfg, &queue, &mut job).await {
            warn!("job {} failed: {e:#}", job.id);
            job.fail(format!("{e:#}"));
            let _ = queue.put(&job);
        }
        n += 1;
    }
    if n > 0 {
        info!("processed {n} job(s)");
    }
    Ok(())
}

async fn run_job(cfg: &Config, queue: &JobQueue, job: &mut IngestJob) -> Result<()> {
    let vault = VaultPaths::new(&cfg.vault_path);
    vault.ensure_layout()?;

    let path = job.source_path.clone();
    if !path.exists() {
        job.set_status(JobStatus::Skipped);
        job.error = Some("source missing".into());
        queue.put(job)?;
        return Ok(());
    }

    // Dedupe by content hash.
    let hash = vault::file_sha256(&path)?;
    job.content_sha256 = Some(hash.clone());
    if queue.hash_seen(&hash)? {
        info!("skip duplicate {}", path.display());
        job.set_status(JobStatus::Skipped);
        job.error = Some("duplicate content hash".into());
        queue.put(job)?;
        // Still move out of incoming if it lives there.
        if path.starts_with(vault.incoming()) {
            let _ = vault.move_to_processed(&path, &job.id.to_string());
        }
        return Ok(());
    }

    job.set_status(JobStatus::Extracting);
    queue.put(job)?;

    research_extract::require_readable(&path)?;
    let extracted = research_extract::extract_file(&path)
        .with_context(|| format!("extract {}", path.display()))?;

    job.title = extracted.title.clone();
    job.source_url = extracted.source_url.clone();
    job.kind = extracted.kind;
    job.metadata_json = if extracted.notes.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&extracted.notes)?)
    };

    // Persist extract for audit / MCP.
    let extract_path = vault.extracts().join(format!("{}.md", job.id));
    write_extract_file(&extract_path, job, &extracted)?;
    job.extracted_text_path = Some(extract_path.clone());
    queue.put(job)?;

    let existing_projects = vault.list_project_slugs().unwrap_or_default();

    let ai = match cfg.ai_backend {
        AiBackend::GrokSession => {
            job.set_status(JobStatus::AwaitingAi);
            queue.put(job)?;
            match call_ai(cfg, job, &extracted, &existing_projects).await {
                Ok(r) => Some(r),
                Err(e) => {
                    warn!("Grok session failed ({e:#}); using heuristic note");
                    None
                }
            }
        }
        AiBackend::QueueOnly => None,
    };

    job.set_status(JobStatus::Writing);
    queue.put(job)?;

    let (slug, title, note_title, body, summary) = if let Some(ai) = ai {
        (
            vault::slugify(&ai.project_slug),
            ai.project_title.clone(),
            ai.note_title.clone(),
            build_note_from_ai(job, &extracted, &ai),
            ai.summary.clone(),
        )
    } else {
        let (slug, title) = research_extract::heuristic_project(
            extracted.title.as_deref(),
            extracted.source_url.as_deref(),
            &extracted.text,
        );
        let note_title = extracted
            .title
            .clone()
            .unwrap_or_else(|| "Research note".into());
        let body = build_heuristic_note(job, &extracted, &summary_heuristic(&extracted));
        let summary = summary_heuristic(&extracted);
        (slug, title, note_title, body, summary)
    };

    let project_dir = vault.ensure_project(&slug, &title)?;
    let note_filename = format!(
        "{}-{}.md",
        Utc::now().format("%Y%m%d"),
        vault::slugify(&note_title)
    );
    let note_path = project_dir.join(&note_filename);
    fs::write(&note_path, &body).with_context(|| format!("write note {}", note_path.display()))?;

    // Append to project root sources list.
    append_source_link(
        &project_dir.join("_project.md"),
        &note_filename,
        &note_title,
        &summary,
    )?;

    job.project_slug = Some(slug);
    job.note_path = Some(note_path.clone());
    job.title = Some(note_title);

    if path.starts_with(vault.incoming()) {
        let _ = vault.move_to_processed(&path, &job.id.to_string());
    }

    queue.mark_hash_seen(&hash)?;
    job.set_status(JobStatus::Done);
    queue.put(job)?;
    info!(
        "done {} → {}",
        job.id,
        note_path
            .strip_prefix(&vault.root)
            .unwrap_or(&note_path)
            .display()
    );
    Ok(())
}

async fn call_ai(
    cfg: &Config,
    job: &IngestJob,
    extracted: &Extracted,
    existing_projects: &[String],
) -> Result<AiIngestResult> {
    let system = grok::load_system_prompt(&cfg.ingest_prompt_path);
    let text = truncate_chars(&extracted.text, cfg.max_extract_chars);
    let payload = format!(
        "job_id: {}\n\
         source_path: {}\n\
         kind: {:?}\n\
         title: {}\n\
         source_url: {}\n\
         existing_projects: {}\n\
         extract_notes: {}\n\n\
         --- BEGIN CONTENT ---\n\
         {text}\n\
         --- END CONTENT ---\n",
        job.id,
        job.source_path.display(),
        extracted.kind,
        extracted.title.as_deref().unwrap_or(""),
        extracted.source_url.as_deref().unwrap_or(""),
        existing_projects.join(", "),
        extracted.notes.join("; "),
    );
    grok::run_ingest_ai(&cfg.grok, &system, &payload).await
}

fn write_extract_file(path: &Path, job: &IngestJob, ex: &Extracted) -> Result<()> {
    let mut md = String::new();
    md.push_str("---\n");
    md.push_str(&format!("job_id: \"{}\"\n", job.id));
    if let Some(t) = &ex.title {
        md.push_str(&format!("title: \"{}\"\n", escape_yaml(t)));
    }
    if let Some(u) = &ex.source_url {
        md.push_str(&format!("source_url: \"{}\"\n", escape_yaml(u)));
    }
    md.push_str(&format!("kind: \"{:?}\"\n", ex.kind));
    md.push_str("---\n\n");
    md.push_str(&ex.text);
    md.push('\n');
    fs::write(path, md)?;
    Ok(())
}

fn escape_yaml(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn summary_heuristic(ex: &Extracted) -> String {
    ex.text
        .lines()
        .map(str::trim)
        .find(|l| l.len() > 20)
        .unwrap_or("Extracted source material.")
        .chars()
        .take(240)
        .collect()
}

fn build_note_from_ai(job: &IngestJob, ex: &Extracted, ai: &AiIngestResult) -> String {
    let mut md = String::new();
    md.push_str("---\n");
    md.push_str(&format!("title: \"{}\"\n", escape_yaml(&ai.note_title)));
    md.push_str(&format!("job_id: \"{}\"\n", job.id));
    if let Some(u) = &ex.source_url {
        md.push_str(&format!("source: \"{}\"\n", escape_yaml(u)));
    }
    if !ai.tags.is_empty() {
        md.push_str(&format!(
            "tags: [{}]\n",
            ai.tags
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    md.push_str(&format!("created: {}\n", Utc::now().format("%Y-%m-%d")));
    md.push_str("---\n\n");
    md.push_str(&format!("# {}\n\n", ai.note_title));
    md.push_str(&format!("> {}\n\n", ai.summary));
    if !ai.entities.is_empty() {
        md.push_str("## Entities\n\n");
        for e in &ai.entities {
            md.push_str(&format!("- {e}\n"));
        }
        md.push('\n');
    }
    md.push_str(&ai.markdown);
    if !ai.markdown.ends_with('\n') {
        md.push('\n');
    }
    md.push_str("\n## Source\n\n");
    md.push_str(&format!("- File: `{}`\n", job.source_path.display()));
    if let Some(u) = &ex.source_url {
        md.push_str(&format!("- URL: {u}\n"));
    }
    md.push_str(&format!("- Job: `{}`\n", job.id));
    md
}

fn build_heuristic_note(job: &IngestJob, ex: &Extracted, summary: &str) -> String {
    let title = ex.title.clone().unwrap_or_else(|| "Research note".into());
    let mut md = String::new();
    md.push_str("---\n");
    md.push_str(&format!("title: \"{}\"\n", escape_yaml(&title)));
    md.push_str(&format!("job_id: \"{}\"\n", job.id));
    md.push_str("tags: [\"ingest\", \"heuristic\"]\n");
    md.push_str(&format!("created: {}\n", Utc::now().format("%Y-%m-%d")));
    md.push_str("---\n\n");
    md.push_str(&format!("# {title}\n\n"));
    md.push_str(&format!("> {summary}\n\n"));
    md.push_str("## Extract\n\n");
    md.push_str(&truncate_chars(&ex.text, 12_000));
    md.push_str("\n\n## Source\n\n");
    md.push_str(&format!("- File: `{}`\n", job.source_path.display()));
    if let Some(u) = &ex.source_url {
        md.push_str(&format!("- URL: {u}\n"));
    }
    md.push_str("\n> Note: Grok session was not used for this note (queue-only, or session error). Re-run with `research-ingest drain` after `grok login` if needed.\n");
    md
}

fn append_source_link(
    project_md: &Path,
    note_file: &str,
    title: &str,
    summary: &str,
) -> Result<()> {
    let mut body = if project_md.exists() {
        fs::read_to_string(project_md)?
    } else {
        String::new()
    };
    let line = format!(
        "- [[{}|{title}]] — {}\n",
        note_file.trim_end_matches(".md"),
        summary.chars().take(120).collect::<String>()
    );
    if body.contains(&line) {
        return Ok(());
    }
    if let Some(idx) = body.find("## Sources") {
        let insert_at = body[idx..]
            .find('\n')
            .map(|i| idx + i + 1)
            .unwrap_or(body.len());
        body.insert_str(insert_at, &line);
    } else {
        body.push_str(&format!("\n## Sources\n\n{line}"));
    }
    fs::write(project_md, body)?;
    Ok(())
}
