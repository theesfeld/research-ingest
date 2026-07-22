# Send to Grok Research (browser extension)

Minimal Brave/Chrome MV3 extension. It only collects page/selection/image metadata and sends it to the **Rust** native host `research-send`.

## Why this is not pure Rust

Chromium MV3 requires a JavaScript service worker (`background.js`) to use `chrome.*` APIs (context menus, native messaging, hotkeys). A pure-Rust extension is not possible for Brave/Chrome.

Rust owns:

- native messaging host (`research-send host`)
- vault write path
- all processing

Optional later: compile shared logic to WASM and call it from JS. That still needs a JS shell.

## Install

1. `cargo install --path crates/research-send`
2. Load this folder as an unpacked extension in Brave.
3. `research-send install-host --extension-id <id-from-brave>`
