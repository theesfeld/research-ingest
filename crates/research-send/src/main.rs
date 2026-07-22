//! research-send — drop research into vault / daemon (CLI, clipboard, native host).

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use research_core::config::{self, Config};
use research_core::vault::VaultPaths;
use research_extract::BrowserPayload;
use serde::{Deserialize, Serialize};
use tracing::info;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const NATIVE_HOST_NAME: &str = "dev.theesfeld.research_send";

#[derive(Parser, Debug)]
#[command(
    name = "research-send",
    version,
    about = "Send research items into the vault (bookmarklet daemon, clipboard, files)"
)]
struct Cli {
    #[arg(long, global = true, env = "RESEARCH_VAULT")]
    vault: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Write a browser-style payload JSON into incoming/ (or POST to daemon).
    Text {
        #[arg(long)]
        text: String,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        title: Option<String>,
    },
    /// Send clipboard text (for global hotkeys). Uses wl-paste / xclip / xsel.
    Clip {
        /// Title for the note.
        #[arg(long, default_value = "Clipboard")]
        title: String,
    },
    /// Send primary selection (X11) or clipboard fallback.
    Selection {
        #[arg(long, default_value = "Selection")]
        title: String,
    },
    /// Copy a local file into incoming/.
    File { path: PathBuf },
    /// Write raw stdin as a note/file into incoming/.
    Stdin {
        #[arg(long, default_value = "clip")]
        name: String,
        #[arg(long, default_value = "md")]
        ext: String,
    },
    /// Install permanent send helpers (desktop entry + bookmarklet HTML). No extension.
    Install {
        #[arg(long, default_value = "127.0.0.1:18765")]
        listen_addr: String,
    },
    /// Install Chrome/Brave native messaging host (optional extension fallback).
    InstallHost {
        #[arg(long)]
        binary: Option<PathBuf>,
        #[arg(long, default_value = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")]
        extension_id: String,
    },
    /// Native messaging host mode.
    Host,
}

#[derive(Debug, Deserialize)]
struct NativeRequest {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(flatten)]
    payload: BrowserPayload,
}

#[derive(Debug, Serialize)]
struct NativeResponse {
    ok: bool,
    path: Option<String>,
    error: Option<String>,
}

fn main() -> Result<()> {
    let is_host = std::env::args().any(|a| a == "host" || a == "Host");
    if !is_host {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .init();
    }

    let cli = Cli::parse();
    let mut cfg = Config::load_or_default();
    if let Some(v) = &cli.vault {
        cfg.vault_path = config::expand_tilde(v.clone());
    }
    let vault = VaultPaths::new(&cfg.vault_path);

    match cli.cmd.unwrap_or(Commands::Host) {
        Commands::Text { text, url, title } => {
            let payload = BrowserPayload {
                title,
                url,
                selection: Some(text),
                page_markdown: None,
                page_text: None,
                image_url: None,
                content_type: Some("selection".into()),
                captured_at: Some(Utc::now().to_rfc3339()),
                extra: None,
            };
            let path = send_payload(&cfg, &vault, &payload)?;
            println!("{path}");
        }
        Commands::Clip { title } => {
            let text = read_clipboard(false)?;
            if text.trim().is_empty() {
                bail!("clipboard is empty");
            }
            let payload = BrowserPayload {
                title: Some(title),
                url: None,
                selection: Some(text),
                page_markdown: None,
                page_text: None,
                image_url: None,
                content_type: Some("clipboard".into()),
                captured_at: Some(Utc::now().to_rfc3339()),
                extra: None,
            };
            let path = send_payload(&cfg, &vault, &payload)?;
            println!("{path}");
            notify_send("Grok Research", "Clipboard sent.");
        }
        Commands::Selection { title } => {
            let text = read_clipboard(true)?;
            if text.trim().is_empty() {
                bail!("selection is empty");
            }
            let payload = BrowserPayload {
                title: Some(title),
                url: None,
                selection: Some(text),
                page_markdown: None,
                page_text: None,
                image_url: None,
                content_type: Some("selection".into()),
                captured_at: Some(Utc::now().to_rfc3339()),
                extra: None,
            };
            let path = send_payload(&cfg, &vault, &payload)?;
            println!("{path}");
            notify_send("Grok Research", "Selection sent.");
        }
        Commands::File { path } => {
            vault.ensure_layout()?;
            let path = config::expand_tilde(path);
            let dest = copy_into_incoming(&vault, &path)?;
            println!("{}", dest.display());
        }
        Commands::Stdin { name, ext } => {
            vault.ensure_layout()?;
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf)?;
            let dest = vault.incoming().join(format!(
                "{}-{}-{}.{}",
                Utc::now().format("%Y%m%dT%H%M%SZ"),
                sanitize(&name),
                &Uuid::new_v4().to_string()[..8],
                ext
            ));
            fs::write(&dest, buf)?;
            println!("{}", dest.display());
        }
        Commands::Install { listen_addr } => {
            install_persistent(&listen_addr)?;
        }
        Commands::InstallHost {
            binary,
            extension_id,
        } => {
            install_native_host(binary, &extension_id)?;
        }
        Commands::Host => {
            run_native_host(&vault)?;
        }
    }
    Ok(())
}

/// Prefer daemon HTTP so the watcher picks up immediately; fall back to vault write.
fn send_payload(cfg: &Config, vault: &VaultPaths, payload: &BrowserPayload) -> Result<String> {
    if cfg.listen_http {
        if let Ok(path) = post_to_daemon(&cfg.listen_addr, payload) {
            return Ok(path);
        }
    }
    vault.ensure_layout()?;
    let path = write_payload(vault, payload)?;
    Ok(path.display().to_string())
}

fn post_to_daemon(listen_addr: &str, payload: &BrowserPayload) -> Result<String> {
    let url = format!("http://{listen_addr}/send");
    let body = serde_json::to_vec(payload)?;
    // curl is always available on this host; avoids reqwest dep in send crate.
    let tmp = std::env::temp_dir().join(format!("ri-send-{}.json", Uuid::new_v4()));
    fs::write(&tmp, &body)?;
    let out = Command::new("curl")
        .args([
            "-fsS",
            "--max-time",
            "5",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            &format!("@{}", tmp.display()),
            &url,
        ])
        .output()
        .context("curl POST to daemon")?;
    let _ = fs::remove_file(&tmp);
    if !out.status.success() {
        bail!(
            "daemon POST failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
        bail!("daemon error: {}", v);
    }
    Ok(v.get("path")
        .and_then(|p| p.as_str())
        .unwrap_or("(ok)")
        .to_string())
}

fn read_clipboard(prefer_primary: bool) -> Result<String> {
    // Wayland clipboard
    if !prefer_primary {
        if let Ok(t) = run_capture(&["wl-paste", "-n"]) {
            if !t.is_empty() {
                return Ok(t);
            }
        }
    } else if let Ok(t) = run_capture(&["wl-paste", "-n", "--primary"]) {
        if !t.is_empty() {
            return Ok(t);
        }
    }
    // X11
    if prefer_primary {
        if let Ok(t) = run_capture(&["xclip", "-o", "-selection", "primary"]) {
            return Ok(t);
        }
        if let Ok(t) = run_capture(&["xsel", "-o"]) {
            return Ok(t);
        }
    }
    if let Ok(t) = run_capture(&["xclip", "-o", "-selection", "clipboard"]) {
        return Ok(t);
    }
    if let Ok(t) = run_capture(&["xsel", "-b"]) {
        return Ok(t);
    }
    // wl-paste fallback after primary miss
    if let Ok(t) = run_capture(&["wl-paste", "-n"]) {
        return Ok(t);
    }
    bail!("no clipboard tool found (install wl-clipboard or xclip)");
}

fn run_capture(args: &[&str]) -> Result<String> {
    let out = Command::new(args[0])
        .args(&args[1..])
        .output()
        .with_context(|| format!("run {}", args[0]))?;
    if !out.status.success() {
        bail!("{} failed", args[0]);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn notify_send(title: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["-a", "research-ingest", title, body])
        .status();
}

fn install_persistent(listen_addr: &str) -> Result<()> {
    let share = research_core::config::state_dir();
    fs::create_dir_all(&share)?;

    // Open install page via daemon if up; also write a local HTML copy.
    let install_url = format!("http://{listen_addr}/send");
    let html_path = share.join("Send-to-Grok-Research.html");
    // Minimal offline page that points at live daemon install page.
    let offline = format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"/><title>Send to Grok Research</title>
<meta http-equiv="refresh" content="0;url={install_url}"/>
</head><body>
<p>Open <a href="{install_url}">{install_url}</a> while the daemon is running
(<code>research-ingest enable</code>) and drag the bookmark button to your bookmarks bar.</p>
</body></html>
"#
    );
    fs::write(&html_path, offline)?;
    println!("install_page: {install_url}");
    println!("local_html:   {}", html_path.display());

    // Desktop entry: open install page + clip action.
    let apps = directories_home()?.join(".local/share/applications");
    fs::create_dir_all(&apps)?;
    let send_bin = which_or("research-send", "%h/.cargo/bin/research-send");
    let desktop = format!(
        r#"[Desktop Entry]
Type=Application
Name=Send clipboard to Grok Research
Comment=Send clipboard to research-ingest daemon
Exec={send_bin} clip
Icon=edit-copy
Terminal=false
Categories=Utility;Office;
"#
    );
    let desk_path = apps.join("research-send-clip.desktop");
    fs::write(&desk_path, desktop)?;
    println!("desktop:      {}", desk_path.display());

    let install_desk = format!(
        r#"[Desktop Entry]
Type=Application
Name=Install Grok Research bookmark
Comment=Open bookmarklet install page
Exec=xdg-open {install_url}
Icon=web-browser
Terminal=false
Categories=Network;
"#
    );
    let install_desk_path = apps.join("research-ingest-install-bookmark.desktop");
    fs::write(&install_desk_path, install_desk)?;
    println!("desktop:      {}", install_desk_path.display());

    let _ = Command::new("update-desktop-database").arg(&apps).status();

    // Open install page if possible.
    let _ = Command::new("xdg-open").arg(&install_url).status();

    println!();
    println!("PRIMARY (permanent, no extension):");
    println!("  1. Keep research-ingest enable running (already always-on).");
    println!("  2. Drag 'Send to Grok Research' from {install_url} to the bookmarks bar.");
    println!("  3. Click the bookmark on any page (select text first if you want a highlight).");
    println!();
    println!("Hotkey (clipboard, any app):");
    println!("  research-send clip");
    println!("  e.g. Super+Shift+Y → research-send clip");
    Ok(())
}

fn which_or(name: &str, fallback: &str) -> String {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name}"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn write_payload(vault: &VaultPaths, payload: &BrowserPayload) -> Result<PathBuf> {
    let id = &Uuid::new_v4().to_string()[..8];
    let slug = sanitize(payload.title.as_deref().unwrap_or("clip"));
    let path = vault.incoming().join(format!(
        "{}-{}-{}.json",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        slug,
        id
    ));
    let text = serde_json::to_string_pretty(payload)?;
    fs::write(&path, text)?;
    info!("wrote {}", path.display());
    Ok(path)
}

fn copy_into_incoming(vault: &VaultPaths, src: &Path) -> Result<PathBuf> {
    if !src.is_file() {
        bail!("not a file: {}", src.display());
    }
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file.bin".into());
    let dest = vault
        .incoming()
        .join(format!("{}-{}", Utc::now().format("%Y%m%dT%H%M%SZ"), name));
    fs::copy(src, &dest)?;
    Ok(dest)
}

fn sanitize(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-');
    if s.is_empty() {
        "clip".into()
    } else {
        s.chars().take(40).collect()
    }
}

fn run_native_host(vault: &VaultPaths) -> Result<()> {
    vault.ensure_layout()?;
    loop {
        let msg = match read_native_message() {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) => {
                let _ = write_native_message(&NativeResponse {
                    ok: false,
                    path: None,
                    error: Some(e.to_string()),
                });
                break;
            }
        };
        let req: NativeRequest = match serde_json::from_value(msg) {
            Ok(r) => r,
            Err(e) => {
                write_native_message(&NativeResponse {
                    ok: false,
                    path: None,
                    error: Some(format!("bad request: {e}")),
                })?;
                continue;
            }
        };
        if req.r#type.as_deref() == Some("ping") {
            write_native_message(&NativeResponse {
                ok: true,
                path: None,
                error: None,
            })?;
            continue;
        }
        match write_payload(vault, &req.payload) {
            Ok(path) => write_native_message(&NativeResponse {
                ok: true,
                path: Some(path.display().to_string()),
                error: None,
            })?,
            Err(e) => write_native_message(&NativeResponse {
                ok: false,
                path: None,
                error: Some(e.to_string()),
            })?,
        }
    }
    Ok(())
}

fn read_native_message() -> Result<Option<serde_json::Value>> {
    let mut stdin = io::stdin().lock();
    let mut len_buf = [0u8; 4];
    match stdin.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > 64 * 1024 * 1024 {
        bail!("invalid native message length {len}");
    }
    let mut buf = vec![0u8; len];
    stdin.read_exact(&mut buf)?;
    let v = serde_json::from_slice(&buf)?;
    Ok(Some(v))
}

fn write_native_message<T: Serialize>(msg: &T) -> Result<()> {
    let bytes = serde_json::to_vec(msg)?;
    let len = (bytes.len() as u32).to_le_bytes();
    let mut stdout = io::stdout().lock();
    stdout.write_all(&len)?;
    stdout.write_all(&bytes)?;
    stdout.flush()?;
    Ok(())
}

fn install_native_host(binary: Option<PathBuf>, extension_id: &str) -> Result<()> {
    let exe = match binary {
        Some(p) => config::expand_tilde(p),
        None => std::env::current_exe().context("current_exe")?,
    };
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    let home = directories_home()?;
    let shim_dir = research_core::config::config_dir();
    fs::create_dir_all(&shim_dir)?;
    let shim = shim_dir.join("native-host.sh");
    fs::write(
        &shim,
        format!("#!/bin/sh\nexec \"{}\" host\n", exe.display()),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&shim)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&shim, perms)?;
    }
    let manifest = serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "research-send native host",
        "path": shim.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{extension_id}/")]
    });
    for dir in [
        home.join(".config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
        home.join(".config/chromium/NativeMessagingHosts"),
        home.join(".config/google-chrome/NativeMessagingHosts"),
    ] {
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{NATIVE_HOST_NAME}.json"));
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        writeln!(f, "{}", serde_json::to_string_pretty(&manifest)?)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn directories_home() -> Result<PathBuf> {
    directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("home directory")
}
