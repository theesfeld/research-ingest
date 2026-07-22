//! research-ingest — watch, extract, and process research into Obsidian via Grok session.

mod grok;
mod process;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use notify::{EventKind, RecursiveMode, Watcher};
use research_core::config::{self, AiBackend, Config};
use research_core::queue::JobQueue;
use research_core::vault::VaultPaths;
use research_core::{IngestJob, JobStatus};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "research-ingest",
    version,
    about = "Ingest research files into an Obsidian vault with Grok session processing"
)]
struct Cli {
    /// Path to the Obsidian vault root.
    #[arg(long, global = true, env = "RESEARCH_VAULT")]
    vault: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create config, vault folders, and print paths.
    Init,
    /// Watch raw/incoming and process new files.
    Watch {
        /// Drain the queue once then exit (no long watch).
        #[arg(long)]
        once: bool,
    },
    /// Process one file path into the vault.
    Process {
        /// File under incoming/ or any readable path.
        path: PathBuf,
    },
    /// Drain pending jobs without starting the watcher.
    Drain {
        /// Max jobs to process (default: all pending).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Print queue status.
    Status,
    /// Print effective config and paths.
    Paths,
    /// Check OCR, ffmpeg, whisper, model, and Grok binary.
    Doctor,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let mut cfg = Config::load_or_default();
    if let Some(v) = &cli.vault {
        cfg.vault_path = config::expand_tilde(v.clone());
    }

    match cli.cmd {
        Commands::Init => cmd_init(&cfg)?,
        Commands::Watch { once } => cmd_watch(cfg, once).await?,
        Commands::Process { path } => {
            let path = config::expand_tilde(path);
            process::process_path(&cfg, &path).await?;
        }
        Commands::Drain { limit } => process::drain_queue(&cfg, limit).await?,
        Commands::Status => cmd_status(&cfg)?,
        Commands::Paths => cmd_paths(&cfg)?,
        Commands::Doctor => cmd_doctor(&cfg)?,
    }
    Ok(())
}

fn cmd_doctor(cfg: &Config) -> Result<()> {
    let tools = cfg.tools.resolve();
    println!("vault={}", cfg.vault_path.display());
    println!("ai_backend={:?}", cfg.ai_backend);
    println!("grok_binary={}", cfg.grok.binary);
    if research_core::tools::which(&cfg.grok.binary).is_some() {
        println!("grok_binary_found=yes");
    } else {
        println!("grok_binary_found=no");
    }
    println!("grok_max_retries={}", cfg.grok.max_retries);
    for line in tools.doctor_lines() {
        println!("{line}");
    }
    let model_dir = research_core::config::state_dir().join("models");
    println!("models_dir={}", model_dir.display());
    Ok(())
}

fn cmd_init(cfg: &Config) -> Result<()> {
    cfg.ensure_written()?;
    let vault = VaultPaths::new(&cfg.vault_path);
    vault.ensure_layout()?;
    let q = JobQueue::open_default()?;
    // Seed / refresh hardened system prompt next to config.
    let prompt_dest = research_core::config::config_dir().join("ingest_system.md");
    if let Err(e) = std::fs::write(&prompt_dest, grok::DEFAULT_INGEST_PROMPT) {
        warn!("could not write {}: {e}", prompt_dest.display());
    } else {
        println!("Prompt:  {}", prompt_dest.display());
    }
    // Ensure models dir exists for whisper weights.
    let models = research_core::config::state_dir().join("models");
    let _ = std::fs::create_dir_all(&models);
    println!("Models:  {}", models.display());
    println!("Config:  {}", Config::config_file_path().display());
    println!("Vault:   {}", vault.root.display());
    println!("Incoming:{}", vault.incoming().display());
    println!("Queue:   {}", q.root().display());
    println!("AI:      {:?}", cfg.ai_backend);
    println!("Grok bin:{}", cfg.grok.binary);
    if cfg.ai_backend == AiBackend::GrokSession {
        println!(
            "Note: language work uses your Grok CLI session (SuperGrok). No xAI API key is used."
        );
    }
    Ok(())
}

fn cmd_paths(cfg: &Config) -> Result<()> {
    let vault = VaultPaths::new(&cfg.vault_path);
    println!("vault_path={}", vault.root.display());
    println!("incoming={}", vault.incoming().display());
    println!("processed={}", vault.processed().display());
    println!("projects={}", vault.projects().display());
    println!("config={}", Config::config_file_path().display());
    println!("ai_backend={:?}", cfg.ai_backend);
    Ok(())
}

fn cmd_status(cfg: &Config) -> Result<()> {
    let q = JobQueue::open_default()?;
    let jobs = q.list()?;
    let mut counts = std::collections::BTreeMap::new();
    for j in &jobs {
        *counts.entry(format!("{:?}", j.status)).or_insert(0usize) += 1;
    }
    println!("vault={}", cfg.vault_path.display());
    println!("jobs_total={}", jobs.len());
    for (k, v) in counts {
        println!("  {k}={v}");
    }
    for j in jobs.iter().rev().take(10) {
        println!(
            "- {} {:?} {} {}",
            j.id,
            j.status,
            j.source_path.display(),
            j.project_slug.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

async fn cmd_watch(cfg: Config, once: bool) -> Result<()> {
    cfg.ensure_written()?;
    let vault = VaultPaths::new(&cfg.vault_path);
    vault.ensure_layout()?;
    let queue = JobQueue::open_default()?;

    if cfg.process_existing_on_start || once {
        enqueue_existing(&vault, &queue)?;
        process::drain_queue(&cfg, None).await?;
        if once {
            return Ok(());
        }
    }

    let incoming = vault.incoming();
    info!("watching {}", incoming.display());

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .context("create filesystem watcher")?;
    watcher
        .watch(&incoming, RecursiveMode::NonRecursive)
        .with_context(|| format!("watch {}", incoming.display()))?;

    // Keep watcher alive.
    let _watcher = watcher;

    let debounce = Duration::from_millis(cfg.debounce_ms);
    loop {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(event)) => {
                if !is_create_or_modify(&event.kind) {
                    continue;
                }
                // Debounce burst.
                std::thread::sleep(debounce);
                // Drain any further events.
                while rx.try_recv().is_ok() {}

                for path in event.paths {
                    if !path.is_file() {
                        continue;
                    }
                    if path.file_name().and_then(|n| n.to_str()) == Some("README.md") {
                        continue;
                    }
                    match queue.find_by_source(&path) {
                        Ok(Some(_)) => continue,
                        Ok(None) => {}
                        Err(e) => warn!("queue lookup: {e}"),
                    }
                    let job = IngestJob::new(path.clone());
                    if let Err(e) = queue.put(&job) {
                        error!("enqueue {}: {e}", path.display());
                        continue;
                    }
                    info!("enqueued {} ({})", path.display(), job.id);
                }
                if let Err(e) = process::drain_queue(&cfg, None).await {
                    error!("drain: {e:#}");
                }
            }
            Ok(Err(e)) => error!("watch event error: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Opportunistic drain of any leftover pending.
                if let Ok(Some(_)) = queue.next_pending() {
                    if let Err(e) = process::drain_queue(&cfg, Some(1)).await {
                        error!("drain: {e:#}");
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail_watch();
            }
        }
    }
}

fn bail_watch() -> ! {
    panic!("watcher channel closed");
}

fn is_create_or_modify(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any
    )
}

fn enqueue_existing(vault: &VaultPaths, queue: &JobQueue) -> Result<()> {
    let dir = vault.incoming();
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("README.md") {
            continue;
        }
        if queue.find_by_source(&path)?.is_some() {
            continue;
        }
        // Skip already-done by hash if we can.
        if let Ok(hash) = research_core::vault::file_sha256(&path) {
            if queue.hash_seen(&hash)? {
                info!("skip seen hash {}", path.display());
                continue;
            }
        }
        let mut job = IngestJob::new(path);
        job.status = JobStatus::Pending;
        queue.put(&job)?;
        info!("enqueued existing {}", job.source_path.display());
    }
    Ok(())
}
