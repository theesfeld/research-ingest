# Packaging notes (Nix / Gentoo / CachyOS / Void)

## Rust toolchain

- Stable or recent nightly works. CI targets stable 1.80+.
- Build: `cargo build --release --workspace`
- Install binaries to `~/.local/bin` or `~/.cargo/bin`:

```sh
cargo install --path crates/research-ingest
cargo install --path crates/research-send
cargo install --path crates/research-mcp
cargo install --path crates/research-zotero
```

## Optional system tools

| Tool | Purpose | Required? |
|------|---------|-----------|
| Grok Build CLI (`grok`) | SuperGrok session AI | Yes for AI notes |
| `tesseract` | Image OCR | Optional |
| `ffmpeg` / `ffprobe` | Media metadata | Optional |
| `whisper` / `whisper-cli` | Transcripts | Optional (not auto-wired yet) |

### NixOS / Home Manager

```nix
home.packages = with pkgs; [
  tesseract
  ffmpeg
];
# Install research-ingest from this repo with cargo or a flake overlay you maintain.
```

### Gentoo

```sh
emerge -av app-text/tesseract media-video/ffmpeg
```

### CachyOS / Arch

```sh
sudo pacman -S tesseract tesseract-data-eng ffmpeg
```

### Void

```sh
sudo xbps-install -S tesseract ffmpeg
```

## systemd --user

```sh
mkdir -p ~/.config/systemd/user
cp packaging/systemd/research-ingest.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now research-ingest.service
```

Do **not** set `XAI_API_KEY` in the unit. The tool uses `grok` OAuth session auth.
