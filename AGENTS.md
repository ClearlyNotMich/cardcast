# AGENTS.md

Cardcast mirrors a screen from the sender onto the card display, so a second PC can capture it, and keeps other windows off that display. Windows-only app, written in Rust.

## Read first

- `SPEC.md`: what v1 does, the tech stack (section 8) and the acceptance checks (section 9).
- `GLOSSARY.md`: use its terms in code, issues and commits (say "sender", not "gaming PC").
- `docs/adr/`: decisions already made. Flag it if your work contradicts one.

## Build and test

- Code is written on Linux. Mirror and window guard only run on Windows, so they can't be run here.
- Before every push: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
- GitHub Actions builds the Windows `.exe` on every push. The owner tests it on the sender.
- Keep decisions in plain functions with no Windows calls (for example "where does this window go"), so they can be unit-tested on Linux.

## Agent skills

### Issue tracker

GitHub Issues on `ClearlyNotMich/cardcast`; writes need the `ClearlyNotMich` token. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default labels (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
