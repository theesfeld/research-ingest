# research-ingest

Local-first research ingest and knowledge routing for [Obsidian](https://obsidian.md), driven by **Grok SuperGrok session** (Grok Build CLI login).

<!-- agents:status:begin -->
> **Status:** Phase 1 in progress · [Issue #1](https://github.com/theesfeld/research-ingest/issues/1) · Version `0.1.0-dev.1` · License MIT  
> **AI:** `grok-session` only (no default xAI API key / pay-per-token path)
<!-- agents:status:end -->

## What it does

1. You send a page, selection, image URL, PDF, or file into `raw/incoming/`.
2. `research-ingest` watches the folder, extracts text, and queues a job.
3. Language work runs through your logged-in **`grok`** CLI (SuperGrok OAuth in `~/.grok/auth.json`).
4. Notes land under `wiki/projects/<slug>/`. Indexes update.

## Billing rule (important)

| Path | Default |
|------|---------|
| Grok Build / SuperGrok session (`grok login`) | **Yes — this is the AI path** |
| `XAI_API_KEY` / console.x.ai token billing | **No — not used by default** |

The ingest process **removes `XAI_API_KEY`** from the environment when it starts `grok`, so session auth wins.

Set `ai_backend = "queue-only"` in config if you want extract-only notes until you process the queue by hand or through MCP.

## Install

Needs: Rust toolchain, Grok Build CLI (`grok`), optional `tesseract` and `ffmpeg`.

```sh
git clone https://github.com/theesfeld/research-ingest.git
cd research-ingest
cargo install --path crates/research-ingest
cargo install --path crates/research-send
cargo install --path crates/research-mcp
cargo install --path crates/research-zotero
research-ingest init
```

Default vault path: `~/Documents/Obsidian Vault`.

Config file (created on `init`): `~/.config/research-ingest/config.toml`.

## Run the watcher

```sh
# Log in to SuperGrok once (browser OAuth)
grok login

# Process existing incoming files, then watch
research-ingest watch

# One pass only
research-ingest watch --once
```

systemd user unit:

```sh
cp packaging/systemd/research-ingest.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now research-ingest.service
```

See [packaging/nix-notes.md](packaging/nix-notes.md) for Nix/Gentoo/CachyOS/Void notes.

## Browser: Send to Grok Research

1. Build and install the native host:

```sh
cargo install --path crates/research-send
research-send install-host
```

2. Load `browser-extension/` as an unpacked extension in Brave (`brave://extensions` → Developer mode).
3. Copy the extension ID. Re-run:

```sh
research-send install-host --extension-id <YOUR_EXTENSION_ID>
```

4. Use the context menu, toolbar popup, or hotkey **Ctrl+Shift+Y** (macOS: **Cmd+Shift+Y**).

CLI without the extension:

```sh
research-send text --text "quote" --url "https://example.com" --title "Example"
research-send file ~/Downloads/paper.pdf
```

## MCP (Grok Build)

Expose the vault to Grok Build:

```toml
# ~/.grok/config.toml
[mcp_servers.research]
command = "research-mcp"
args = []
enabled = true
```

Tools include: `list_projects`, `list_pending_jobs`, `search_notes`, `read_note`, `write_note`, `list_incoming`, `vault_info`, `read_extract`.

## Zotero

Point Better BibTeX auto-export (or a folder of PDFs) at a directory, then:

```sh
research-zotero watch --export-dir ~/Zotero/export
```

Files copy into `raw/incoming/` for the main watcher.

## Workspace crates

| Crate | Binary | Role |
|-------|--------|------|
| `research-core` | library | Config, queue, vault layout |
| `research-extract` | library | PDF/text/image/media extract |
| `research-ingest` | `research-ingest` | Watch + process + Grok session |
| `research-send` | `research-send` | CLI + native messaging host |
| `research-mcp` | `research-mcp` | MCP stdio server |
| `research-zotero` | `research-zotero` | Zotero export watch |

## Vault layout

```text
~/Documents/Obsidian Vault/
  raw/incoming/       # drop target
  raw/processed/      # after success
  raw/extracts/       # plain extracts per job
  wiki/projects/<slug>/
  wiki/inbox.md
  index.md
  map-of-content.md
```

## Configuration

`~/.config/research-ingest/config.toml` (example):

```toml
vault_path = "/home/you/Documents/Obsidian Vault"
ai_backend = "grok-session"   # or "queue-only"
max_extract_chars = 48000

[grok]
binary = "grok"
yolo = true
timeout_secs = 600
effort = "high"
# model = "grok-build"   # optional
```

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## License

MIT. See [LICENSE](LICENSE).

## Roadmap

Tracked on [Issue #1](https://github.com/theesfeld/research-ingest/issues/1). Pre-1.0: **0.x minor versions may include breaking changes**.
