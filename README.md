# research-ingest

Local-first research ingest and knowledge routing for [Obsidian](https://obsidian.md), driven by **Grok SuperGrok session** (Grok Build CLI login).

<!-- agents:status:begin -->
> **Status:** Always-on · [Issue #6](https://github.com/theesfeld/research-ingest/issues/6) · Version `0.1.0-dev.4` · License MIT  
> **AI:** `grok-session` only (no default xAI API key / pay-per-token path)  
> **Media:** OCR (tesseract) · auto transcript (ffmpeg + whisper-cli)  
> **Daily use:** permanent **bookmark** or **clipboard hotkey** — no extension reload
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

Needs: Rust toolchain, Grok Build CLI (`grok`). For OCR and transcripts:

```sh
# Nix (example)
nix profile add nixpkgs#tesseract nixpkgs#ffmpeg nixpkgs#whisper-cpp

# Whisper model (base.en is a good default)
mkdir -p ~/.local/share/research-ingest/models
cd ~/.local/share/research-ingest/models
whisper-cpp-download-ggml-model base.en
```

```sh
git clone https://github.com/theesfeld/research-ingest.git
cd research-ingest
cargo install --path crates/research-ingest
cargo install --path crates/research-send
cargo install --path crates/research-mcp
cargo install --path crates/research-zotero
export PATH="$HOME/.cargo/bin:$HOME/.nix-profile/bin:$PATH"
# SuperGrok session once (subscription login, not API key)
grok login

research-ingest enable
research-ingest doctor
```

That starts the **always-on** user service: vault watch + HTTP drop on `127.0.0.1:18765`.

Default vault path: `~/Documents/Obsidian Vault`.  
Config: `~/.config/research-ingest/config.toml`.

### Daily use (no extension reload)

Unpacked Brave extensions are optional and annoying. **Prefer the permanent bookmark.**

**One-time:**

1. `research-ingest enable`
2. Open <http://127.0.0.1:18765/send>
3. Drag **Send to Grok Research** onto the bookmarks bar

**Every day:** select text (optional) → click the bookmark.

**Or global hotkey (any app):**

```sh
research-send clip          # clipboard
research-send selection     # primary selection (X11)
# Bind Super+Shift+Y → research-send clip
```

```sh
research-ingest service-status
research-ingest status
research-ingest disable
```

See [packaging/nix-notes.md](packaging/nix-notes.md) for Nix/Gentoo/CachyOS/Void notes.

## OCR and auto transcript

| Feature | Tools | Config |
|---------|--------|--------|
| Image OCR | `tesseract` | `[tools] enable_ocr`, `ocr_lang` |
| Media metadata | `ffprobe` | auto on PATH |
| Auto transcript | `ffmpeg` + `whisper-cli` + ggml model | `[tools] enable_transcript`, `whisper_model`, `whisper_lang` |

The watcher extracts text first (including OCR and transcript), then sends that text to the Grok session for project routing and note writing.

```sh
research-ingest doctor
research-ingest process ~/path/to/scan.png
research-ingest process ~/path/to/talk.mp4
```

## Send paths (persistent)

| Path | Persistence | Notes |
|------|-------------|--------|
| **Bookmarklet** (recommended) | Permanent after one drag | Form POST to daemon; works on HTTPS pages |
| **`research-send clip`** | Hotkey / desktop entry | Clipboard from any app |
| Optional MV3 extension | Unpacked can break on Brave updates | Not required |

```sh
research-send install                 # desktop entries + open install page
research-send text --text "quote" --url "https://example.com"
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
max_retries = 2
# model = "grok-build"   # optional

[tools]
enable_ocr = true
enable_transcript = true
ocr_lang = "eng"
whisper_lang = "en"
# optional absolute paths if not on PATH:
# tesseract = "/home/you/.nix-profile/bin/tesseract"
# ffmpeg = "ffmpeg"
# whisper = "whisper-cli"
whisper_model = "/home/you/.local/share/research-ingest/models/ggml-base.en.bin"
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
