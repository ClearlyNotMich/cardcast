# Coding standards

A reviewer rejects a change that breaks any rule below.

1. Use `GLOSSARY.md` terms in code, tests, issues and commits ("sender", not "gaming PC").
2. Windows calls only in `mirror`, `guard`, `audio` and `input`. Decisions (for example "where does this window go") live in plain functions with Linux unit tests.
3. Every `unsafe` block has a `// SAFETY:` comment saying why it is sound. `ui` and logic have no `unsafe`.
4. No `unwrap` outside tests. `expect` only under `#[expect(clippy::expect_used, reason = "why it cannot fail")]`; clippy enforces this.
5. Threads per SPEC: guard hooks and the mirror window each run on their own thread.
6. No low-level mouse or keyboard hooks (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`).
7. No new crate without checking its current version and maintenance status.
8. Errors reach the user in the status line, in plain words.
