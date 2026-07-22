//! research-zotero — copy Zotero export / storage drops into vault incoming/.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use notify::{EventKind, RecursiveMode, Watcher};
use research_core::config::{self, Config};
use research_core::vault::VaultPaths;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(
    name = "research-zotero",
    version,
    about = "Watch Zotero Better BibTeX export or a drop folder and feed research-ingest"
)]
struct Cli {
    #[arg(long, global = true, env = "RESEARCH_VAULT")]
    vault: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Watch a directory (Better BibTeX auto-export or Zotero storage drop).
    Watch {
        /// Directory to watch (for example ~/Zotero/export or a BBT auto-export path).
        #[arg(long, env = "ZOTERO_EXPORT_DIR")]
        export_dir: PathBuf,
        /// Also copy PDFs found under this tree into incoming.
        #[arg(long, default_value_t = true)]
        copy_pdfs: bool,
    },
    /// One-shot scan of export_dir into incoming.
    Sync {
        #[arg(long, env = "ZOTERO_EXPORT_DIR")]
        export_dir: PathBuf,
        #[arg(long, default_value_t = true)]
        copy_pdfs: bool,
    },
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
    if let Some(v) = cli.vault {
        cfg.vault_path = config::expand_tilde(v);
    }
    let vault = VaultPaths::new(&cfg.vault_path);
    vault.ensure_layout()?;

    match cli.cmd {
        Commands::Sync {
            export_dir,
            copy_pdfs,
        } => {
            let export_dir = config::expand_tilde(export_dir);
            sync_once(&vault, &export_dir, copy_pdfs)?;
        }
        Commands::Watch {
            export_dir,
            copy_pdfs,
        } => {
            let export_dir = config::expand_tilde(export_dir);
            sync_once(&vault, &export_dir, copy_pdfs)?;
            watch_loop(&vault, &export_dir, copy_pdfs)?;
        }
    }
    Ok(())
}

fn sync_once(vault: &VaultPaths, export_dir: &Path, copy_pdfs: bool) -> Result<()> {
    if !export_dir.exists() {
        warn!("export dir missing: {}", export_dir.display());
        return Ok(());
    }
    for entry in WalkDir::new(export_dir).max_depth(4) {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let take = matches!(ext.as_str(), "bib" | "json" | "csv") || (copy_pdfs && ext == "pdf");
        if !take {
            continue;
        }
        if let Err(e) = copy_to_incoming(vault, path) {
            warn!("copy {}: {e}", path.display());
        }
    }
    Ok(())
}

fn copy_to_incoming(vault: &VaultPaths, src: &Path) -> Result<PathBuf> {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "zotero.bin".into());
    // Dedupe by size+name marker file.
    let dest = vault.incoming().join(format!(
        "zotero-{}-{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        name
    ));
    // Skip if identical basename already pending with same length.
    if let Ok(meta) = fs::metadata(src) {
        for e in fs::read_dir(vault.incoming())? {
            let e = e?;
            let p = e.path();
            if p.file_name()
                .map(|n| n.to_string_lossy().ends_with(&name))
                .unwrap_or(false)
            {
                if let Ok(m2) = fs::metadata(&p) {
                    if m2.len() == meta.len() {
                        info!("skip existing pending {}", name);
                        return Ok(p);
                    }
                }
            }
        }
    }
    fs::copy(src, &dest).with_context(|| format!("copy {} → {}", src.display(), dest.display()))?;
    info!("queued {}", dest.display());
    Ok(dest)
}

fn watch_loop(vault: &VaultPaths, export_dir: &Path, copy_pdfs: bool) -> Result<()> {
    info!("watching Zotero export {}", export_dir.display());
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(export_dir, RecursiveMode::Recursive)?;
    let _w = watcher;

    loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(event)) => {
                if !matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any
                ) {
                    continue;
                }
                std::thread::sleep(Duration::from_millis(500));
                while rx.try_recv().is_ok() {}
                for path in event.paths {
                    if !path.is_file() {
                        continue;
                    }
                    let ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    let take = matches!(ext.as_str(), "bib" | "json" | "csv")
                        || (copy_pdfs && ext == "pdf");
                    if take {
                        let _ = copy_to_incoming(vault, &path);
                    }
                }
            }
            Ok(Err(e)) => warn!("watch error: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}
