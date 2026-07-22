# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-dev.2] - 2026-07-22

### Added

- Hardened Grok ingest prompts with JSON schema, validation, and retries
- OCR via tesseract with config + PATH discovery (`tools.*`)
- Auto transcript via ffmpeg audio extract + whisper.cpp (`whisper-cli`)
- `research-ingest doctor` for tool health
- Default model path under `~/.local/share/research-ingest/models/`

### Changed

- Extract pipeline takes resolved tool paths (no silent “found but not wired” transcript)

### Notes

- Browser extensions cannot be pure Rust on Chromium/Brave MV3 (service worker must be JS); native host remains Rust

## [0.1.0-dev.1] - 2026-07-22

### Added

- Cargo workspace: `research-core`, `research-extract`, `research-ingest`, `research-send`, `research-mcp`, `research-zotero`
- Vault layout under Obsidian: `raw/incoming`, `raw/processed`, `raw/extracts`, `wiki/projects`, `index.md`, `map-of-content.md`
- Watcher CLI with job queue and PDF/text extraction
- **Grok session** AI backend only (headless `grok` + OAuth). No default xAI API key path
- Heuristic note fallback when Grok is offline
- `research-send` CLI and Chrome/Brave native messaging host
- Browser extension (MV3) with context menu and hotkey
- `research-mcp` stdio MCP server for Grok Build
- `research-zotero` export directory watcher
- systemd user unit templates and packaging notes

### Notes

- 0.x releases may include breaking changes
