//! research-send — drop research into vault incoming/ (CLI + native messaging).

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

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
    about = "Send research items into the vault incoming folder"
)]
struct Cli {
    #[arg(long, global = true, env = "RESEARCH_VAULT")]
    vault: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Write a browser-style payload JSON into incoming/.
    Text {
        /// Selected or body text.
        #[arg(long)]
        text: String,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        title: Option<String>,
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
    /// Install Chrome/Brave native messaging host manifest.
    InstallHost {
        /// Absolute path to this binary (default: current exe).
        #[arg(long)]
        binary: Option<PathBuf>,
        /// Extension ID (Chrome/Brave). Use `*` only for unpackaged dev if allowed.
        #[arg(long, default_value = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")]
        extension_id: String,
    },
    /// Native messaging host mode (stdin length-prefixed JSON). Called by the browser.
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
    // Native host must keep stdout clean for length-prefixed protocol.
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
            vault.ensure_layout()?;
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
            let path = write_payload(&vault, &payload)?;
            println!("{}", path.display());
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
        // type may be "send" | "ping"
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
    // Brave + Chrome user-level native messaging hosts (Linux).
    let dirs = [
        home.join(".config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
        home.join(".config/chromium/NativeMessagingHosts"),
        home.join(".config/google-chrome/NativeMessagingHosts"),
    ];

    let allowed = format!("chrome-extension://{extension_id}/");
    let manifest = serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "research-send native host for Grok research ingest",
        "path": exe.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [allowed]
    });

    for dir in dirs {
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

    // Wrapper script args: browser invokes path with no args — manifest path is the binary.
    // We need the host subcommand. Install a small shim next to config.
    let shim_dir = research_core::config::config_dir();
    fs::create_dir_all(&shim_dir)?;
    let shim = shim_dir.join("native-host.sh");
    let shim_body = format!("#!/bin/sh\nexec \"{}\" host\n", exe.display());
    fs::write(&shim, shim_body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&shim)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&shim, perms)?;
    }

    // Rewrite manifests to use shim so `host` subcommand is applied.
    let manifest = serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "research-send native host for Grok research ingest",
        "path": shim.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{extension_id}/")]
    });
    for dir in [
        home.join(".config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
        home.join(".config/chromium/NativeMessagingHosts"),
        home.join(".config/google-chrome/NativeMessagingHosts"),
    ] {
        if dir.is_dir() {
            let path = dir.join(format!("{NATIVE_HOST_NAME}.json"));
            fs::write(&path, serde_json::to_string_pretty(&manifest)?)?;
            println!("updated {}", path.display());
        }
    }

    println!("Native host name: {NATIVE_HOST_NAME}");
    println!("Set the extension ID with: research-send install-host --extension-id <id>");
    Ok(())
}

fn directories_home() -> Result<PathBuf> {
    directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("home directory")
}
