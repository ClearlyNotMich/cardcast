# AGENTS.md

Cardcast mirrors a screen from the sender onto the card display, so a second PC can capture it, and keeps other windows off that display. Windows-only app, written in Rust.

## Read first

- `SPEC.md`: what v1 does, the tech stack (section 8) and the acceptance checks (section 9).
- `GLOSSARY.md`: use its terms in code, issues and commits (say "sender", not "gaming PC").
- `docs/adr/`: decisions already made. Flag it if your work contradicts one.
- `CODING_STANDARDS.md`: rules every change must follow.
- `docs/win32-pitfalls.md`: Win32 code (window guard, pointer guard, mirror, audio): sourced facts on blocking calls, coordinates, hooks and focus. Read it before guessing Win32 behaviour.
- `docs/win32-mirror-audio.md`: mirror and audio code (capture, swap chain, HDR, WASAPI): sourced facts. Read it before writing either.

## Build and test

- Code is written on Linux. Mirror and window guard only run on Windows, so they can't be run here.
- A pre-push hook in `.githooks/` runs these checks (one-time: `git config core.hooksPath .githooks`); its `cargo test` renders the snapshots, so it needs `mesa-vulkan-drivers` too. Never skip it with `--no-verify`.
- Before every push: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- Check the Windows-only code on Linux before pushing: `cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings` (one-time: `rustup target add x86_64-pc-windows-msvc`).
- GitHub Actions builds the Windows `.exe` on every push. The owner tests it on the sender.
- See the settings window: `cargo test snapshot` renders it to `tests/snapshots/*.png` (Linux, lavapipe: `mesa-vulkan-drivers`); `UPDATE_SNAPSHOTS=1 cargo test snapshot` rewrites them after a UI change.
- Keep decisions in plain functions with no Windows calls (for example "where does this window go"), so they can be unit-tested on Linux.

## Agent skills

### Issue tracker

GitHub Issues on `ClearlyNotMich/cardcast`; writes need the `ClearlyNotMich` token. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default labels (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
