//! Localhost HTTP drop for always-on send (bookmarklet, extension, CLI).

use std::collections::HashMap;
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
    pub listen_addr: String,
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
    let state = DropState {
        vault,
        listen_addr: cfg.listen_addr.clone(),
    };

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {}", addr))?;
    info!("HTTP drop listening on http://{addr}");
    info!("  POST /send       JSON (extension / curl)");
    info!("  POST /send-form  form (bookmarklet — works on HTTPS pages)");
    info!("  GET  /send       install + bookmarklet page");

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
                write_html(
                    socket,
                    413,
                    "<html><body><h1>Payload too large</h1></body></html>",
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
        write_html(
            socket,
            400,
            "<html><body><h1>Incomplete request</h1></body></html>",
        )
        .await?;
        return Ok(());
    };

    let headers = std::str::from_utf8(&buf[..header_end]).unwrap_or("");
    let req_line = headers.lines().next().unwrap_or("");
    let method_path = parse_request_line(req_line);
    let content_type = parse_header(headers, "content-type").unwrap_or_default();

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
        ("GET", "/health") => {
            write_json(
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
        ("GET", "/") | ("GET", "/send") | ("GET", "/install") => {
            write_html(socket, 200, &install_page(&state.listen_addr)).await?;
        }
        ("GET", "/bookmarklet.js") => {
            let js = bookmarklet_js(&state.listen_addr);
            write_bytes(
                socket,
                200,
                "application/javascript; charset=utf-8",
                js.as_bytes(),
            )
            .await?;
        }
        ("POST", "/send") | ("POST", "/drop") => {
            let resp = match accept_bytes(state, &body, content_type) {
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
            write_json(socket, code, &resp).await?;
        }
        ("POST", "/send-form") => {
            // Bookmarklet path: form navigation works from HTTPS pages.
            match accept_form(state, &body) {
                Ok(path) => {
                    write_html(socket, 200, &success_page(&path)).await?;
                }
                Err(e) => {
                    write_html(
                        socket,
                        400,
                        &format!(
                            "<!DOCTYPE html><html><body style='font-family:system-ui;padding:2rem'>\
                             <h1>Send failed</h1><pre>{}</pre>\
                             <p><a href='javascript:window.close()'>Close</a></p></body></html>",
                            html_escape(&format!("{e:#}"))
                        ),
                    )
                    .await?;
                }
            }
        }
        _ => {
            write_html(
                socket,
                404,
                "<html><body><h1>Not found</h1><p>See <a href='/send'>/send</a></p></body></html>",
            )
            .await?;
        }
    }
    Ok(())
}

fn accept_bytes(state: &DropState, body: &[u8], content_type: &str) -> Result<String> {
    if content_type.contains("application/x-www-form-urlencoded") {
        return accept_form(state, body);
    }
    state.vault.ensure_layout()?;
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
    write_payload(state, &payload)
}

fn accept_form(state: &DropState, body: &[u8]) -> Result<String> {
    let map = parse_form(body);
    let payload = BrowserPayload {
        title: map.get("title").cloned().filter(|s| !s.is_empty()),
        url: map.get("url").cloned().filter(|s| !s.is_empty()),
        selection: map.get("selection").cloned().filter(|s| !s.is_empty()),
        page_markdown: map.get("page_markdown").cloned().filter(|s| !s.is_empty()),
        page_text: map.get("page_text").cloned().filter(|s| !s.is_empty()),
        image_url: map.get("image_url").cloned().filter(|s| !s.is_empty()),
        content_type: Some(
            map.get("content_type")
                .cloned()
                .unwrap_or_else(|| "bookmarklet".into()),
        ),
        captured_at: Some(
            map.get("captured_at")
                .cloned()
                .unwrap_or_else(|| Utc::now().to_rfc3339()),
        ),
        extra: None,
    };
    write_payload(state, &payload)
}

fn write_payload(state: &DropState, payload: &BrowserPayload) -> Result<String> {
    state.vault.ensure_layout()?;
    let id = &Uuid::new_v4().to_string()[..8];
    let slug = sanitize(payload.title.as_deref().unwrap_or("clip"));
    let path = state.vault.incoming().join(format!(
        "{}-{}-{}.json",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        slug,
        id
    ));
    let text = serde_json::to_string_pretty(payload)?;
    std::fs::write(&path, text)?;
    info!("drop wrote {}", path.display());
    Ok(path.display().to_string())
}

fn parse_form(body: &[u8]) -> HashMap<String, String> {
    let s = String::from_utf8_lossy(body);
    let mut map = HashMap::new();
    for pair in s.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        map.insert(url_decode(k), url_decode(v));
    }
    map
}

fn url_decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Bookmarklet source. Form POST avoids mixed-content blocks on HTTPS pages.
pub fn bookmarklet_js(listen_addr: &str) -> String {
    // Keep compact; no template literals in the outer string.
    format!(
        r#"(function(){{
var A='http://{listen_addr}/send-form';
var f=document.createElement('form');
f.method='POST';f.action=A;f.target='_blank';f.acceptCharset='utf-8';
function add(n,v){{if(v==null)v='';var i=document.createElement('input');i.type='hidden';i.name=n;i.value=String(v);f.appendChild(i);}}
var sel='';try{{sel=String(window.getSelection&&window.getSelection()||'');}}catch(e){{}}
var body='';try{{body=(document.body&&document.body.innerText)||'';}}catch(e){{}}
if(body.length>180000)body=body.slice(0,180000);
add('title',document.title||'');
add('url',location.href||'');
add('selection',sel);
add('page_text',body);
add('content_type',sel.trim()?'selection':'page');
add('captured_at',new Date().toISOString());
document.documentElement.appendChild(f);
f.submit();
setTimeout(function(){{try{{f.remove();}}catch(e){{}}}},500);
}})();"#
    )
}

pub fn bookmarklet_href(listen_addr: &str) -> String {
    let js = bookmarklet_js(listen_addr);
    // javascript: URL — encode carefully
    format!("javascript:{}", url_encode_component(&js))
}

fn url_encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'(' | b')' => {
                out.push(b as char);
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn install_page(listen_addr: &str) -> String {
    let href = bookmarklet_href(listen_addr);
    let raw_js = bookmarklet_js(listen_addr);
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>Send to Grok Research — install</title>
<style>
 body {{ font-family: system-ui, sans-serif; max-width: 40rem; margin: 2rem auto; padding: 0 1rem; line-height: 1.5; }}
 .bm {{ display: inline-block; background: #1e40af; color: #fff; padding: 0.75rem 1.1rem;
        border-radius: 0.5rem; text-decoration: none; font-weight: 600; }}
 .bm:hover {{ background: #1d4ed8; }}
 code, pre {{ background: #f1f5f9; padding: 0.15rem 0.35rem; border-radius: 0.25rem; }}
 pre {{ padding: 0.75rem; overflow-x: auto; white-space: pre-wrap; word-break: break-all; font-size: 0.75rem; }}
 .box {{ border: 1px solid #cbd5e1; border-radius: 0.5rem; padding: 1rem; margin: 1rem 0; }}
 h1 {{ font-size: 1.4rem; }}
</style>
</head>
<body>
<h1>Send to Grok Research</h1>
<p>Daemon is listening on <code>http://{listen_addr}</code>. You do <strong>not</strong> need a Brave extension for daily use.</p>

<div class="box">
  <h2>1. Bookmark (one time, permanent)</h2>
  <p>Drag this button to your bookmarks bar:</p>
  <p><a class="bm" href="{href}">Send to Grok Research</a></p>
  <p>Or bookmark any page, then edit the URL to the javascript code below.</p>
  <p>Daily use: open a page, optionally select text, click the bookmark. A tab opens and closes after send.</p>
</div>

<div class="box">
  <h2>2. Global hotkey (optional)</h2>
  <p>Bind a key to clipboard send (no browser extension):</p>
  <pre>research-send clip</pre>
  <p>Example Hyprland:</p>
  <pre>bind = SUPER SHIFT, Y, exec, research-send clip</pre>
</div>

<div class="box">
  <h2>Bookmarklet source</h2>
  <pre id="src"></pre>
</div>

<script>
document.getElementById('src').textContent = {raw_js_json};
</script>
</body>
</html>
"#,
        href = html_escape(&href),
        raw_js_json = serde_json::to_string(&raw_js).unwrap_or_else(|_| "''".into()),
    )
}

fn success_page(path: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>Sent</title>
<style>
 body {{ font-family: system-ui, sans-serif; padding: 2rem; max-width: 32rem; margin: auto; }}
</style>
<script>setTimeout(function(){{ try {{ window.close(); }} catch(e){{}} }}, 900);</script>
</head>
<body>
<h1>Sent to Grok Research</h1>
<p>Queued for background processing. You can close this tab.</p>
<p style="font-size:0.85rem;color:#64748b"><code>{}</code></p>
</body></html>
"#,
        html_escape(path)
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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

fn parse_header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    let want = name.to_ascii_lowercase();
    for line in headers.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case(&want) {
                return Some(v.trim());
            }
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

async fn write_json<T: Serialize>(
    socket: &mut tokio::net::TcpStream,
    code: u16,
    body: &T,
) -> Result<()> {
    let json = serde_json::to_vec(body)?;
    write_bytes(socket, code, "application/json; charset=utf-8", &json).await
}

async fn write_html(socket: &mut tokio::net::TcpStream, code: u16, html: &str) -> Result<()> {
    write_bytes(socket, code, "text/html; charset=utf-8", html.as_bytes()).await
}

async fn write_bytes(
    socket: &mut tokio::net::TcpStream,
    code: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
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
         Content-Type: {content_type}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.flush().await?;
    Ok(())
}

async fn write_raw(socket: &mut tokio::net::TcpStream, raw: &str) -> Result<()> {
    socket.write_all(raw.as_bytes()).await?;
    socket.flush().await?;
    Ok(())
}
