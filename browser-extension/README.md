# Send to Grok Research (browser extension)

Minimal Brave/Chrome MV3 extension. Daily path: **HTTP drop** to the always-on daemon.

## How send works

1. Preferred: `POST http://127.0.0.1:18765/send` (daemon from `research-ingest enable`)
2. Fallback: native messaging host `research-send host`

## Why not pure Rust

Chromium MV3 requires a JavaScript service worker for `chrome.*` APIs. The native host and all processing stay Rust.

## One-time setup

1. `research-ingest enable` (always-on daemon)
2. Brave → Extensions → Developer mode → Load unpacked → this folder
3. Use right-click **Send to Grok Research**, toolbar, or **Ctrl+Shift+Y**

No further steps. The daemon processes OCR, transcripts, and Grok notes in the background.
