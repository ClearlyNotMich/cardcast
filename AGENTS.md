# AGENTS.md

Cardcast mirrors a screen from the sender onto the card display, so a second PC can capture it, and keeps other windows off that display. Runs on Windows and Linux (ADR 0004), written in Rust.

## Read first

- `SPEC.md`: what v1 does, the tech stack (section 8) and the acceptance checks (section 9).
- `GLOSSARY.md`: use its terms in code, issues and commits (say "sender", not "gaming PC").
- `docs/adr/`: decisions already made. Flag it if your work contradicts one.
- `CODING_STANDARDS.md`: rules every change must follow.
- `docs/win32-pitfalls.md`: Win32 code (window guard, pointer guard, mirror, audio): sourced facts on blocking calls, coordinates, hooks and focus. Read it before guessing Win32 behaviour.
- `docs/win32-mirror-audio.md`: mirror and audio code (capture, swap chain, HDR, WASAPI): sourced facts. Read it before writing either.
- `docs/win32-ddc.md`: monitor input switching over DDC/CI (dxva2, VCP 0x60, command mode exit codes): sourced facts. Read it before writing it.
- `docs/research/linux-support.md`: the Linux parts (portal and PipeWire capture, layer-shell, PipeWire audio, per-desktop guard helpers, i2c-dev): sourced facts and the support matrix. Read it before writing Linux code.

## Build and test

- Code is written on Linux. The Windows mirror, guards and audio relay only run on Windows, so they can't be run here.
- `cargo build --release` on Linux builds the real Linux cardcast (`target/release/cardcast`; `cargo run` starts it). Parts not yet built for Linux (#36) say "does not work on Linux yet" in the status line; never make one fake success. The stand-in mirror is `cfg(test)` only, for the snapshots.
- Linux code goes in `cfg(target_os = "linux")` modules beside the Windows ones, behind the same traits. `guard::desktop::detect` says which desktop runs (Plasma, GNOME, sway, Hyprland, other Wayland, X11); `match` on it to pick a per-desktop helper.
- A pre-push hook in `.githooks/` runs these checks (one-time: `git config core.hooksPath .githooks`); its `cargo test` renders the snapshots, so it needs `mesa-vulkan-drivers` too. Never skip it with `--no-verify`.
- Before every push: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- Check the Windows-only code on Linux before pushing: `cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings` (one-time: `rustup target add x86_64-pc-windows-msvc`).
- GitHub Actions builds the Windows `.exe` (artifact `cardcast-exe`) and the Linux tarball (artifact `cardcast-linux-x86_64.tar.gz`: the binary, `LICENSE`, `README.md`) on every push. The owner tests both on the sender.
- See the settings window: `cargo test snapshot` renders it to `tests/snapshots/*.png` (Linux, lavapipe: `mesa-vulkan-drivers`); `UPDATE_SNAPSHOTS=1 cargo test snapshot` rewrites them after a UI change.
- Keep decisions in plain functions with no platform calls (for example "where does this window go", "which desktop is this"), so they can be unit-tested on Linux.

## Agent skills

### Issue tracker

GitHub Issues on `ClearlyNotMich/cardcast`; writes need the `ClearlyNotMich` token. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default labels (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
