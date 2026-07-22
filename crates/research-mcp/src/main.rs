//! research-mcp — stdio MCP server for the research vault.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use research_core::config::{self, Config};
use research_core::queue::JobQueue;
use research_core::vault::VaultPaths;
use serde_json::{json, Value};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "research-mcp",
    version,
    about = "MCP server for research-ingest vault tools"
)]
struct Cli {
    #[arg(long, env = "RESEARCH_VAULT")]
    vault: Option<PathBuf>,
}

struct State {
    vault: VaultPaths,
    queue: JobQueue,
}

fn main() -> Result<()> {
    // Logs to stderr only — stdout is MCP JSON-RPC.
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
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
    let queue = JobQueue::open_default()?;
    let state = Arc::new(State { vault, queue });

    info!("research-mcp ready vault={}", state.vault.root.display());

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_msg(&json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                }))?;
                continue;
            }
        };
        if let Some(resp) = handle_message(&state, msg)? {
            write_msg(&resp)?;
        }
    }
    Ok(())
}

fn write_msg(v: &Value) -> Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, v)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn handle_message(state: &State, msg: Value) -> Result<Option<Value>> {
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = msg.get("id").cloned();
    let params = msg.get("params").cloned().unwrap_or(json!({}));

    // Notifications (no id) — ignore after handling if needed.
    let is_notification = id.is_none() || id.as_ref().is_some_and(|i| i.is_null());

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "research-mcp",
                "version": env!("CARGO_PKG_VERSION")
            }
        })),
        "notifications/initialized" | "initialized" => {
            return Ok(None);
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_defs() })),
        "tools/call" => tools_call(state, &params),
        _ => {
            if is_notification {
                return Ok(None);
            }
            return Ok(Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {method}") }
            })));
        }
    };

    if is_notification {
        return Ok(None);
    }

    match result {
        Ok(r) => Ok(Some(json!({ "jsonrpc": "2.0", "id": id, "result": r }))),
        Err(e) => Ok(Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32000, "message": format!("{e:#}") }
        }))),
    }
}

fn tool_defs() -> Vec<Value> {
    vec![
        tool(
            "list_projects",
            "List research project slugs under wiki/projects.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "list_pending_jobs",
            "List ingest jobs that are not done.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "search_notes",
            "Search vault Markdown notes for a query string.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "default": 20 }
                },
                "required": ["query"]
            }),
        ),
        tool(
            "read_note",
            "Read a note path relative to the vault root.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path under the vault" }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "write_note",
            "Write or overwrite a Markdown note relative to the vault root.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        ),
        tool(
            "list_incoming",
            "List files waiting in raw/incoming.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "vault_info",
            "Return vault paths and counts.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "read_extract",
            "Read the extract for a job id (raw/extracts/<id>.md).",
            json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string" }
                },
                "required": ["job_id"]
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

fn tools_call(state: &State, params: &Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .context("missing tool name")?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let text = match name {
        "list_projects" => {
            let slugs = state.vault.list_project_slugs()?;
            serde_json::to_string_pretty(&slugs)?
        }
        "list_pending_jobs" => {
            let jobs: Vec<_> = state
                .queue
                .list()?
                .into_iter()
                .filter(|j| {
                    !matches!(
                        j.status,
                        research_core::JobStatus::Done | research_core::JobStatus::Skipped
                    )
                })
                .map(|j| {
                    json!({
                        "id": j.id,
                        "status": format!("{:?}", j.status),
                        "source": j.source_path,
                        "project": j.project_slug,
                        "title": j.title,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&jobs)?
        }
        "search_notes" => {
            let q = args
                .get("query")
                .and_then(|v| v.as_str())
                .context("query required")?;
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            let hits = state.vault.search_notes(q, limit)?;
            let rows: Vec<_> = hits
                .into_iter()
                .map(|(p, snip)| json!({ "path": p, "snippet": snip }))
                .collect();
            serde_json::to_string_pretty(&rows)?
        }
        "read_note" => {
            let rel = args
                .get("path")
                .and_then(|v| v.as_str())
                .context("path required")?;
            let path = safe_join(&state.vault.root, rel)?;
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?
        }
        "write_note" => {
            let rel = args
                .get("path")
                .and_then(|v| v.as_str())
                .context("path required")?;
            let content = args
                .get("content")
                .and_then(|v| v.as_str())
                .context("content required")?;
            let path = safe_join(&state.vault.root, rel)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, content)?;
            format!("wrote {}", path.display())
        }
        "list_incoming" => {
            let dir = state.vault.incoming();
            let mut names = Vec::new();
            if dir.is_dir() {
                for e in fs::read_dir(dir)? {
                    let e = e?;
                    if e.file_type()?.is_file() {
                        names.push(e.file_name().to_string_lossy().into_owned());
                    }
                }
            }
            names.sort();
            serde_json::to_string_pretty(&names)?
        }
        "vault_info" => {
            let projects = state.vault.list_project_slugs()?.len();
            let jobs = state.queue.list()?.len();
            serde_json::to_string_pretty(&json!({
                "vault": state.vault.root,
                "incoming": state.vault.incoming(),
                "projects_count": projects,
                "jobs_total": jobs,
            }))?
        }
        "read_extract" => {
            let id = args
                .get("job_id")
                .and_then(|v| v.as_str())
                .context("job_id required")?;
            let path = state.vault.extracts().join(format!("{id}.md"));
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?
        }
        other => anyhow::bail!("unknown tool: {other}"),
    };

    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    }))
}

fn safe_join(root: &std::path::Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.trim_start_matches('/');
    if rel.contains("..") {
        anyhow::bail!("path must not contain ..");
    }
    let path = root.join(rel);
    let root_c = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if let Ok(c) = fs::canonicalize(&path) {
        if !c.starts_with(&root_c) {
            anyhow::bail!("path escapes vault");
        }
        return Ok(c);
    }
    // New file: ensure parent is under root.
    if let Some(parent) = path.parent() {
        if parent.exists() {
            let pc = fs::canonicalize(parent)?;
            if !pc.starts_with(&root_c) {
                anyhow::bail!("path escapes vault");
            }
        }
    }
    Ok(path)
}
