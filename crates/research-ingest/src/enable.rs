//! Install and start the always-on user systemd service.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use research_core::config::{self, Config};
use research_core::vault::VaultPaths;
use tracing::info;

use crate::grok;

const UNIT_NAME: &str = "research-ingest.service";

pub fn enable_daemon(cfg: &Config) -> Result<()> {
    cfg.ensure_written()?;
    let vault = VaultPaths::new(&cfg.vault_path);
    vault.ensure_layout()?;

    // Seed prompt.
    let prompt_dest = config::config_dir().join("ingest_system.md");
    let _ = fs::write(&prompt_dest, grok::DEFAULT_INGEST_PROMPT);
    let _ = fs::create_dir_all(config::state_dir().join("models"));

    let unit_dir = user_systemd_dir()?;
    fs::create_dir_all(&unit_dir)?;
    let unit_path = unit_dir.join(UNIT_NAME);
    let unit = render_unit();
    fs::write(&unit_path, unit).with_context(|| format!("write {}", unit_path.display()))?;
    info!("wrote {}", unit_path.display());

    // Prefer linger so the watcher survives logout on this machine.
    let _ = Command::new("loginctl")
        .args(["enable-linger", &whoami()])
        .status();

    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", UNIT_NAME])?;

    // Brief settle, then status.
    std::thread::sleep(std::time::Duration::from_millis(400));
    let status = Command::new("systemctl")
        .args(["--user", "is-active", UNIT_NAME])
        .output()?;
    let active = String::from_utf8_lossy(&status.stdout).trim().to_string();
    if active != "active" {
        let _ = Command::new("systemctl")
            .args(["--user", "status", UNIT_NAME, "--no-pager"])
            .status();
        bail!("service is not active (got '{active}'). Check: systemctl --user status {UNIT_NAME}");
    }

    // Health check HTTP drop if configured.
    if cfg.listen_http {
        let url = format!("http://{}/health", cfg.listen_addr);
        match ureq_get_health(&url) {
            Ok(body) => println!("health: {body}"),
            Err(e) => println!("health: not ready yet ({e}) — wait a second and retry curl {url}"),
        }
    }

    println!("service: active ({UNIT_NAME})");
    println!("unit:    {}", unit_path.display());
    println!("vault:   {}", vault.root.display());
    println!("incoming:{}", vault.incoming().display());
    if cfg.listen_http {
        println!("drop:    http://{}/send", cfg.listen_addr);
        println!(
            "install: http://{}/send   (drag bookmark — permanent, no extension)",
            cfg.listen_addr
        );
    }
    println!();

    // Permanent send helpers (bookmarklet install page + desktop entries).
    let _ = Command::new("research-send")
        .args(["install", "--listen-addr", &cfg.listen_addr])
        .status();

    println!();
    println!("Daily send (pick one — none require reloading an extension):");
    println!(
        "  • Bookmark: open http://{}/send once, drag button to bookmarks bar",
        cfg.listen_addr
    );
    println!("  • Hotkey:   bind Super+Shift+Y → research-send clip");
    println!("  • Desktop:  “Send clipboard to Grok Research” app menu entry");
    println!();
    println!("Optional: browser extension is NOT required for daily use.");
    Ok(())
}

pub fn disable_daemon() -> Result<()> {
    let _ = run_systemctl(&["disable", "--now", UNIT_NAME]);
    println!("service disabled");
    Ok(())
}

pub fn daemon_status() -> Result<()> {
    let _ = Command::new("systemctl")
        .args(["--user", "status", UNIT_NAME, "--no-pager"])
        .status();
    Ok(())
}

fn render_unit() -> String {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().display().to_string())
        .unwrap_or_else(|| "%h".into());
    // PATH must include cargo, nix profile, and system grok.
    let path = format!(
        "{home}/.cargo/bin:{home}/.nix-profile/bin:{home}/.local/bin:/etc/profiles/per-user/{user}/bin:/run/current-system/sw/bin:/usr/local/bin:/usr/bin:/bin",
        user = whoami()
    );
    format!(
        r#"[Unit]
Description=research-ingest always-on vault watcher + Brave drop
After=default.target network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={home}/.cargo/bin/research-ingest watch
Restart=always
RestartSec=3
# Full tool PATH for OCR / whisper / grok session
Environment=PATH={path}
Environment=HOME={home}
Environment=RUST_LOG=info
# SuperGrok session only — never bill console API tokens from the daemon
Environment=XAI_API_KEY=
# Ensure no empty key confuses some CLIs
PassEnvironment=HOME

[Install]
WantedBy=default.target
"#
    )
}

fn user_systemd_dir() -> Result<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("home directory")?;
    Ok(home.join(".config/systemd/user"))
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "glenda".into())
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let mut full = vec!["--user"];
    full.extend_from_slice(args);
    let st = Command::new("systemctl")
        .args(&full)
        .status()
        .with_context(|| format!("systemctl {}", args.join(" ")))?;
    if !st.success() {
        bail!("systemctl {} failed", args.join(" "));
    }
    Ok(())
}

/// Tiny HTTP GET without extra deps (for enable health print).
fn ureq_get_health(url: &str) -> Result<String> {
    // Prefer curl if present — always on NixOS.
    let out = Command::new("curl")
        .args(["-fsS", "--max-time", "2", url])
        .output()
        .context("curl health")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
