# delightfile on macOS and Windows — the plan

This folder, `plans/other-platforms/`, is the source of truth for the cross-platform
port. Sibling folders under `plans/` hold plans for other features and are unrelated. An agent picking up
work reads `00-ground-rules.md` first, then the phase document it is assigned, and
marks its progress **in the document** as it goes. Nothing about the port lives only
in a chat transcript or a commit message; if it matters, it is in here.

## The documents

| # | File | What it covers | Depends on |
|---|------|----------------|------------|
| 00 | `00-ground-rules.md` | Conventions every phase obeys: the `platform` module shape, the cfg strategy, the Linux-unchanged rule, dependency policy, how to mark progress, how to test | — |
| 01 | `01-platform-seam.md` | Phase 1: cut the code along a platform seam so every crate *compiles* on all three targets, with stubs where a feature has no native implementation yet. Adds the CI matrix. | 00 |
| 02 | `02-macos.md` | Phase 2: native macOS implementations behind the seam — FSEvents, `clonefile`, trash, pasteboard and drag-out, volumes, openers, fonts, keyboard, the `.app` bundle | 01 |
| 03 | `03-paths.md` | Phase 3: the path model df-core needs before Windows can work — no byte-path assumptions, no literal `/` root, drive letters, case-insensitive comparison, UTF-8 persisted formats | 01 |
| 04 | `04-windows.md` | Phase 4: native Windows implementations — `ReadDirectoryChangesW`, Recycle Bin, `CF_HDROP` clipboard, drives, openers without a shell, fonts, the SFTP pipe I/O, symlinks, console subsystem | 01, 03 |
| 05 | `05-defaults-and-config.md` | Per-platform defaults: config/state/cache directories, opener tables, terminal, keymap modifier conventions, help text | 01 |
| 06 | `06-build-and-release.md` | CI matrix, FFmpeg and pdfium per platform, packaging (tarball, `.app` + dmg, zip), Homebrew tap and scoop, the signing decision and the optional notarization workflow | 01 |
| 07 | `07-verification.md` | What CI can prove and what needs a human at a real machine; the live checklist per platform; the release gate | all |
| A | `appendix-inventory-df-core.md` | Factual inventory of every Linux-specific site in df-core, file:line, with the pub API that must be preserved | — |
| B | `appendix-inventory-df-app.md` | The same for df-app, `build/`, and the vendored dv-* crates | — |

Phase order is **01 → 02 → 03 → 04**, with 05 and 06 running alongside whichever
phase first needs them. Phase 2 (macOS) before Phase 3 (paths) is deliberate: macOS
is Unix and gets a working build off Phase 1 alone, which proves the seam before the
larger Windows work starts. Windows is not attempted before Phase 3 is done.

## How to read a task

Every task looks like this:

```
- [ ] **S1.4** `crates/df-core/src/fs/watch.rs` — move `Watcher::new` behind
      `platform::watch::open`; the Linux body is `fs::inotify` unchanged.
      Done when: `cargo test -p df-core` green on Linux; `cargo check --target
      aarch64-apple-darwin -p df-core` passes with the stub.
```

- The bold ID (`S1.4`, `M2.7`, `P3.2`, `W4.9`, `D5.1`, `B6.3`, `V7.2`) is stable.
  Never renumber. New tasks get the next free number in that section and are appended.
- "Done when" is the acceptance test. If it cannot be met, the task is not done.
- Tasks inside one numbered section are in dependency order unless the section says
  otherwise.

## How to mark progress

- `- [ ]` not started
- `- [>]` in progress — add ` — <agent/session>, started <YYYY-MM-DD>` after the text
- `- [x]` done — add ` — done <short sha>` (the commit that finished it). If there is
  no commit yet because Brian reviews before committing, write ` — done, uncommitted
  <YYYY-MM-DD>` and replace it with the sha later.
- `- [~]` blocked or deliberately skipped — add ` — blocked: <one line why>` or
  ` — skipped: <one line why>`. A skip needs a line in that document's Decisions log.

Each document has a `Status:` line under its title. Keep it current:
`not started` → `in progress` → `done` (every task `[x]` or `[~]` with a logged reason).

Each document ends with a **Decisions log** and an **Open questions** section. When you
make a call that the document did not already make, write it in the log with the
date. When you hit something the documents cannot answer, add it to Open questions and
stop that task with `[~] blocked` rather than guessing. Only Brian removes an open
question.

## The one rule that overrides everything

**Linux does not change.** The Linux build is in daily use. Every phase must leave
`cargo test --workspace` and `cargo clippy --workspace` green on Linux, and the
runtime behaviour on Linux identical, before its tasks are marked done. Where a
task says "Linux body unchanged", it means byte-for-byte the same logic, moved but
not rewritten. Any Linux-visible change needs its own justification in the
Decisions log.
