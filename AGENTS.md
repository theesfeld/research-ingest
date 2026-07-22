# research-ingest — project facts

Global process law: `~/.config/agents/AGENTS.md` (wins on conflict).

## Class

Product (CLI/TUI + local tooling). MIT. Version from workspace `0.1.0-dev.1`.

## Layout

- Workspace root: this repo
- Default vault: `~/Documents/Obsidian Vault`
- Config: `~/.config/research-ingest/config.toml`
- Queue: `~/.local/share/research-ingest/queue/`

## AI policy

- **Default:** `ai_backend = grok-session` (headless `grok` + OAuth SuperGrok)
- **Forbidden by default:** wiring console `XAI_API_KEY` token billing
- Child `grok` processes must strip `XAI_API_KEY`

## Commands

```sh
cargo build --workspace
cargo test --workspace
cargo run -p research-ingest -- init
cargo run -p research-ingest -- watch --once
```

## Browser exception

`browser-extension/` is minimal JS (browser platform requirement). Native host is Rust.
