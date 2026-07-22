//! Localhost HTTP drop for Brave extension (always-on send path).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use research_core::config::Config;
use research_core::vault::VaultPaths;
use research_extract::BrowserPayload;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{error, info, warn};
use uuid::Uuid;

#[derive(Clone)]
pub struct DropState {
    pub vault: VaultPaths,
}

#[derive(Serialize)]
struct DropResponse {
    ok: bool,
    path: Option<String>,
    error: Option<String>,
}

/// Accept browser payloads on 127.0.0.1 and write JSON into raw/incoming/.
pub async fn run_drop_server(cfg: Arc<Config>) -> Result<()> {
    let addr: SocketAddr = cfg
        .listen_addr
        .parse()
        .with_context(|| format!("parse listen_addr {}", cfg.listen_addr))?;
    if !addr.ip().is_loopback() {
        anyhow::bail!(
            "listen_addr must be loopback for safety (got {})",
            cfg.listen_addr
        );
    }

    let vault = VaultPaths::new(&cfg.vault_path);
    vault.ensure_layout()?;
    let state = DropState { vault };

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {}", addr))?;
    info!("HTTP drop listening on http://{addr}  (POST /send)");

    loop {
        match listener.accept().await {
            Ok((mut socket, peer)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(&mut socket, &state).await {
                        warn!("drop conn from {peer}: {e:#}");
                    }
                });
            }
            Err(e) => {
                error!("accept error: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

async fn handle_conn(socket: &mut tokio::net::TcpStream, state: &DropState) -> Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    // Read until headers end or buffer full; then body by Content-Length.
    let n = socket.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let total = n;
    let header_end = find_header_end(&buf[..total]);
    let (header_end, body) = if let Some(he) = header_end {
        let body_start = he;
        let headers = std::str::from_utf8(&buf[..he]).unwrap_or("");
        let content_len = parse_content_length(headers).unwrap_or(0);
        let mut body = buf[body_start..total].to_vec();
        while body.len() < content_len {
            let mut chunk = vec![0u8; 64 * 1024];
            let n = socket.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
            if body.len() > 32 * 1024 * 1024 {
                write_response(
                    socket,
                    413,
                    &DropResponse {
                        ok: false,
                        path: None,
                        error: Some("payload too large".into()),
                    },
                )
                .await?;
                return Ok(());
            }
        }
        if body.len() > content_len {
            body.truncate(content_len);
        }
        (he, body)
    } else {
        // Incomplete headers — fail soft.
        write_response(
            socket,
            400,
            &DropResponse {
                ok: false,
                path: None,
                error: Some("incomplete HTTP request".into()),
            },
        )
        .await?;
        return Ok(());
    };

    let headers = std::str::from_utf8(&buf[..header_end]).unwrap_or("");
    let req_line = headers.lines().next().unwrap_or("");
    let method_path = parse_request_line(req_line);

    // CORS preflight for extension / page callers.
    if method_path.0 == "OPTIONS" {
        write_raw(
            socket,
            "HTTP/1.1 204 No Content\r\n\
             Access-Control-Allow-Origin: *\r\n\
             Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
             Access-Control-Allow-Headers: content-type\r\n\
             Content-Length: 0\r\n\
             Connection: close\r\n\r\n",
        )
        .await?;
        return Ok(());
    }

    match (method_path.0.as_str(), method_path.1.as_str()) {
        ("GET", "/health") | ("GET", "/") => {
            write_response(
                socket,
                200,
                &serde_json::json!({
                    "ok": true,
                    "service": "research-ingest",
                    "version": env!("CARGO_PKG_VERSION"),
                    "incoming": state.vault.incoming(),
                }),
            )
            .await?;
        }
        ("POST", "/send") | ("POST", "/drop") => {
            let resp = match accept_payload(state, &body) {
                Ok(path) => DropResponse {
                    ok: true,
                    path: Some(path),
                    error: None,
                },
                Err(e) => DropResponse {
                    ok: false,
                    path: None,
                    error: Some(format!("{e:#}")),
                },
            };
            let code = if resp.ok { 200 } else { 400 };
            write_response(socket, code, &resp).await?;
        }
        _ => {
            write_response(
                socket,
                404,
                &DropResponse {
                    ok: false,
                    path: None,
                    error: Some("use POST /send or GET /health".into()),
                },
            )
            .await?;
        }
    }
    Ok(())
}

fn accept_payload(state: &DropState, body: &[u8]) -> Result<String> {
    state.vault.ensure_layout()?;
    // Accept BrowserPayload JSON, or wrap plain text.
    let payload: BrowserPayload = if let Ok(p) = serde_json::from_slice(body) {
        p
    } else {
        let text = String::from_utf8_lossy(body).into_owned();
        BrowserPayload {
            title: Some("clip".into()),
            url: None,
            selection: Some(text),
            page_markdown: None,
            page_text: None,
            image_url: None,
            content_type: Some("raw".into()),
            captured_at: Some(Utc::now().to_rfc3339()),
            extra: None,
        }
    };

    let id = &Uuid::new_v4().to_string()[..8];
    let slug = sanitize(payload.title.as_deref().unwrap_or("clip"));
    let path = state.vault.incoming().join(format!(
        "{}-{}-{}.json",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        slug,
        id
    ));
    let text = serde_json::to_string_pretty(&payload)?;
    std::fs::write(&path, text)?;
    info!("drop wrote {}", path.display());
    Ok(path.display().to_string())
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

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn parse_content_length(headers: &str) -> Option<usize> {
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn parse_request_line(line: &str) -> (String, String) {
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    (method, path)
}

async fn write_response<T: Serialize>(
    socket: &mut tokio::net::TcpStream,
    code: u16,
    body: &T,
) -> Result<()> {
    let json = serde_json::to_vec(body)?;
    let reason = match code {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: application/json\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        json.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(&json).await?;
    socket.flush().await?;
    Ok(())
}

async fn write_raw(socket: &mut tokio::net::TcpStream, raw: &str) -> Result<()> {
    socket.write_all(raw.as_bytes()).await?;
    socket.flush().await?;
    Ok(())
}
