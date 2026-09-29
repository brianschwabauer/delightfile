# 03 — Paths

Status: **in progress** (port/paths, 2026-09-29)

Scope: everything df-core assumes about the shape of a path that is false on
Windows, and the case-sensitivity assumptions that are false on Windows and on the
default macOS volume. This phase is the prerequisite for `04-windows.md`; nothing in
Phase 4 is started before every task here is `[x]` or `[~]`. macOS (Phase 2) does
**not** wait for this phase, except for the two case-sensitivity tasks marked
"(macOS too)".

The factual basis is `appendix-inventory-df-core.md` §3 (107 rows in 7 tables). Each
task below names the rows it closes; when a task is done, every row it names must be
resolved, and the appendix row gets a `✓ P3.n` suffix in its Note column so the
appendix stays a checklist too.

Prerequisite: Phase 1 complete. Phase 1's S1.16 already created
`platform::os::{as_bytes, from_bytes}` (the spec below under P3.1) and rewrote every
`OsStrExt` site mechanically, so the §2 tasks here add *behaviour* (separators,
roots, budgets) to sites that already compile everywhere.

## Decisions already made

- **Persisted and wire formats carry paths as UTF-8.** On Unix nothing changes: the
  existing byte encodings (state file, `.trashinfo`, archive member names, rsync
  argv) are kept byte-for-byte, because for a valid-UTF-8 path the bytes *are* the
  UTF-8. On Windows the same call converts through `OsStr::to_str`, and a name that
  is not valid Unicode (an unpaired UTF-16 surrogate, which real files essentially
  never have) is refused with `DfError::Unsupported("non-Unicode file name")` by the
  feature that would persist it. This is done by one pair of functions,
  `platform::os::as_bytes` and `platform::os::from_bytes`, and every §3.1 row is
  rewritten to call them. No site outside `platform/` mentions `OsStrExt` again.
- **`std::path` does the parsing.** Splitting on a literal `/` for a *local* path is
  replaced by `components()`, `file_name()`, `parent()`, `Path::join`. A literal `/`
  stays only where the format owns it: git's porcelain output, rsync's itemized
  output, zip and tar member names, SFTP wire paths, `VfsPath`. Those are
  `(format)` rows in the appendix and are not tasks.
- **The root is `root_of(path)`, never `Path::new("/")`.** A new `df_core::path`
  module owns the handful of questions `std::path` does not answer: what the root of
  a path is, whether a path is a root, the breadcrumb segments, trailing-separator
  handling, and the case-folded key for lookup tables.
- **Case: filesystem truth for destructive decisions, folded keys for lookups.**
  "Is this the same file?" before an overwrite or a self-paste is answered by
  `platform::fs::same_file` (device+inode on Unix, volume serial + file index on
  Windows), never by comparing strings. Tables keyed by path that are written from
  one spelling and read with another (state records, git status, du cache, cursor
  memory) key on `path::key(p)`, which lowercases on Windows and is the identity on
  Unix. macOS keeps identity keys: its spellings come from `read_dir` on both sides,
  and the inventory found no macOS-specific mis-keying that the Windows fix would not
  also cover if it ever mattered. **No `key()` on Linux changes anything.**
- **Windows-illegal names are refused at creation and extraction**, not silently
  mangled: `path::name_is_valid(name)` returns why (`<>:"|?*`, control characters,
  trailing dot or space, reserved device names). On Unix it refuses only NUL and the
  separator, which is what the filesystem refuses anyway, so Linux behaviour is
  unchanged.
- **Drive roots have no parent.** On Windows the parent pane of `C:\` is empty, the
  way `/`'s is today; the way across drives is the drives card (`M`, Phase 4),
  `Go to:`, and bookmarks. No synthetic "This PC" listing.
- **Canonicalized paths are never displayed.** `std::fs::canonicalize` on Windows
  yields `\\?\C:\…`; `ops::resolved` and its callers only compare, and the tasks
  below keep it that way. If a future feature needs a displayable canonical path, it
  strips the verbatim prefix in `path::display`.
- **Remote rows keep an `sftp://` URL in `Entry.path`.** Changing `Entry.path`'s type
  is out of scope. Instead, no code may apply `Path::join`, `parent` or `file_name`
  to a remote row's path; the `VfsPath` string type does that. Phase 4 audits the
  df-app side (`appendix-inventory-df-app.md` names the sites).

## 1. `platform::os` and `df_core::path` (the foundation)

- [x] **P3.1** (**Landed by S1.16 in Phase 1**; mark `[x]` with S1.16's commit and
      keep the spec here as the reference.) `crates/df-core/src/platform/os.rs`: `pub fn as_bytes(s: &OsStr) ->
      Result<Cow<'_, [u8]>>` and `pub fn from_bytes(b: &[u8]) -> Result<OsString>`.
      Unix bodies: `OsStrExt::as_bytes` / `OsStringExt::from_vec`, infallible in
      practice (the `Result` is always `Ok`). Windows bodies: `to_str` /
      `String::from_utf8`, `Err(DfError::Unsupported("non-Unicode file name"))`
      otherwise. Unit tests on both. Done when: both bodies compile on their
      targets (CI) and the Unix tests pass on Linux. — done e34eab8 (S1.16),
      cross-checked locally, CI pending
- [x] **P3.2** `crates/df-core/src/path.rs` (new, portable, no cfg inside):
      - `pub fn root_of(p: &Path) -> PathBuf` — the `Prefix`+`RootDir` components
        (`C:\`, `\\server\share\`) or `/`; for a relative path, the root of the
        current directory.
      - `pub fn is_root(p: &Path) -> bool`.
      - `pub fn has_trailing_separator(s: &OsStr) -> bool` and
        `pub fn trim_trailing_separator(s: &OsStr) -> Cow<'_, OsStr>` — `/` on
        Unix; `/` or `\` on Windows; the root itself is never trimmed to empty.
        Implemented with `std::path::is_separator` on the UTF-8 view where
        possible, and via `platform::os` bytes otherwise.
      - `pub fn segments(p: &Path) -> Vec<Segment { label: String, path: PathBuf }>`
        for the breadcrumb: on Unix the first segment is `/`; on Windows it is the
        prefix as typed by people (`C:`, `\\server\share`), then one per component.
      - `pub fn key(p: &Path) -> PathBuf` — identity on Unix; on Windows the
        path with every component lowercased (`to_lowercase` on the UTF-8 view,
        unchanged when not UTF-8).
      - `pub fn name_is_valid(name: &OsStr) -> Result<(), &'static str>` — see
        Decisions; the Windows rule set is compiled in on every target but the
        function asks `platform::path::STRICT_NAMES` (a `const bool`) whether to
        apply it, so Unix keeps the permissive rule.
      - `pub fn display(p: &Path) -> String` — `to_string_lossy` with the Windows
        `\\?\` verbatim prefix stripped.
      Done when: unit tests cover `/`, `/a/b`, `C:\`, `C:\a\b`, `\\s\sh\a`,
      `\\?\C:\a`, trailing separators, and `key` on a mixed-case Windows path; the
      Windows-shaped tests run on every target by building the inputs with
      `PathBuf::from` of a literal only when `cfg!(windows)`, otherwise by testing
      the pure helpers on `&str`. — done (port/paths). As built: `key` returns
      `Cow<'_, Path>` (borrowed, no copy, where it is the identity) with
      `into_key(PathBuf)` for inserts; the Windows rules are public as
      `folded` and `name_is_valid_strict` (and `name_is_valid_permissive`), so
      Linux tests them; the constants are `platform::os::{STRICT_NAMES,
      FOLD_CASE}` (Decisions log).
- [x] **P3.3** `platform::fs::same_file(a: &Path, b: &Path) -> io::Result<bool>`:
      Unix `dev`+`ino` of `symlink_metadata` (the body of `ops.rs:212–218` moved);
      Windows `GetFileInformationByHandle` volume serial + file index via a
      `File::open` with `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS`.
      Done when: `ops::same_file` is a one-line call to it and every §3.4 row that
      decides an overwrite uses it (see P3.14). — done (port/paths): the Windows
      body compares `platform::meta::identity` (P3.4) of both paths;
      `ops::same_file` was already the one line (S1.5); the overwrite rows are
      P3.18's (the "see P3.14" above means P3.18).
- [x] **P3.4** `platform::meta` (the `MetadataExt` surface, appendix §2 `fs::entry`,
      `du::walk`, `preview::cache`, `archive::write`): `dev(&Metadata) -> u64`,
      `ino`, `nlink`, `mode -> u32`, `uid`, `gid`, `blocks_bytes -> u64`,
      `change_time -> (i64, i64)`, `is_hidden(name: &OsStr, meta: &Metadata) ->
      bool`. Unix bodies are the `MetadataExt` calls as today. Windows bodies:
      `dev` = volume serial of the path (cached per root), `ino` = file index,
      `nlink` = `number_of_links`, `mode` synthesized (`S_IFDIR|0o755` for
      directories, `S_IFLNK|0o777` for reparse points, `S_IFREG|0o644` or `0o444`
      when read-only), `uid`/`gid` = 0, `blocks_bytes` = `file_size`,
      `change_time` = `last_write_time`, `is_hidden` = dot-prefix **or**
      `FILE_ATTRIBUTE_HIDDEN`. macOS `is_hidden` = dot-prefix only (decision: the
      `UF_HIDDEN` flag hides `~/Library`, which a file manager for power users should
      show). Done when: every `use std::os::unix::fs::MetadataExt` outside
      `platform/` is gone from df-core (`grep` is the test) and `Entry`'s fields are
      filled through these. — done (port/paths). S1.7 had moved the
      `MetadataExt` surface; this task added `is_hidden` (called by
      `Entry::from_parts` with the row's own `lstat`) and the real Windows
      numbers, which a `Metadata` cannot carry on stable Rust: `Identity { dev,
      ino, nlink }` from `identity(path, &meta)` (Unix: off the `stat`, no
      syscall; Windows: `GetFileInformationByHandle` on a handle opened with
      `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS`, through
      `windows-sys` 0.52.0, winit's), and `maybe_linked(&meta)` (Unix `nlink >
      1`; Windows any regular file), which the du walk asks before paying for a
      handle. `dev`/`ino`/`nlink` of a bare `Metadata` stay `0`/`0`/`1` on
      Windows (Decisions log). This is W4.5's identity half, marked done there;
      `blocks_bytes` by `GetCompressedFileSizeW` became W4.35.

## 2. Bytes → UTF-8 at every persisted and wire format (appendix §3.1, §3.6)

Each task: replace the `OsStrExt`/`OsStringExt` calls with `platform::os`, propagate
the `Result` to the nearest place that already returns `DfError`, keep the Unix bytes
identical. A test per format that round-trips a non-ASCII UTF-8 name on every target,
and the existing non-UTF-8 test kept under `#[cfg(unix)]`.

- [x] **P3.5** State file: `state/mod.rs:744–752` (`path_bytes`, `path_from`), and
      the record-key rule at `:536` (`starts_with(b"/")`) becomes "parses as an
      absolute path" via `Path::is_absolute` after decoding. Header stays
      `# delightfile state v1`: the on-disk bytes on Linux do not change. Done when:
      `state/tests.rs` passes on Linux unchanged and a Windows-shaped key
      (`C:\Users\x`) round-trips in a new test. — done (port/paths):
      `path_bytes`/`path_from` were already `platform::os` (S1.16); the key is
      decoded first and kept when `is_absolute`, which on Unix is the leading
      `/` it was. `a_key_is_absolute_as_the_platform_reads_it` round-trips
      `C:\Users\x` on Windows (`/home/x` on Unix), and the byte-for-byte
      fixture test holds. What `state/tests.rs` asserts on Linux is unchanged;
      its literal `/tmp/…` keys go through `test_support::abs`, the identity on
      Unix and `C:\tmp\…` on Windows, where the new rule drops `/tmp/…`
      (P3.30, bin 2).
- [x] **P3.6** `.trashinfo`: `ops/trash.rs:679–720` (`encode_path`, `decode_path`)
      and `:255–270` (`list`), `:609–646` (`fit`, `clip` — clip on UTF-8 char
      boundaries of the UTF-8 view, and `MAX_NAME_BYTES` stays a byte budget on Unix
      while Windows uses a 255-UTF-16-unit budget from `platform::path::MAX_NAME`).
      Note that the whole freedesktop trash is Linux-only after Phase 1 (macOS and
      Windows have their own `platform::trash`); this task is still needed because
      `suffixed`/`fit` are used by paste and vfs on every target. Done when:
      `ops/trash.rs` tests pass on Linux and `suffixed` has a UTF-8 test. — done
      (port/paths). `encode_path`, `decode_path` and `list` are in
      `platform/linux/trash.rs` since S1.6 and already byte-exact there, so
      nothing changed in them; `fit` and `clip` (in `fs/names.rs` since S1.6)
      count a name in `platform::os::MAX_NAME` units — bytes on Unix, so Linux
      clips exactly as before, UTF-16 units on Windows — and never split a
      character or a surrogate pair. `MAX_NAME_BYTES` stays, the Linux trash's
      byte budget. Tests: a UTF-8 name clipped by both rules, a surrogate pair
      kept whole, and the existing ones measured in the platform's unit.
- [x] **P3.7** Archive member names: `archive/write/mod.rs:529–580`; member
      `mode`/`uid`/`gid`/`mtime` via `platform::meta`. `MADE_BY` stays "Unix" on
      every target (readers accept it; the synthesized mode is valid). Symlink
      members on Windows: `read_link` works for symlinks; junctions are skipped with
      a warning line. Done when: `archive/write/tests.rs` passes on Linux and a UTF-8
      member name test exists. — done (port/paths). Names and the member stat
      were already `platform::os`/`platform::meta` (S1.16, S1.7). New:
      `platform::fs::is_junction` (Unix `false`; Windows the reparse tag
      `IO_REPARSE_TAG_MOUNT_POINT` by `GetFileInformationByHandleEx`) — a
      junction goes to the walk's `skipped` list with a `warn!` line; a link's
      target is written with `/` between names (`path::with_slashes`, the
      identity on Unix, so Linux writes the same bytes). Test:
      `a_unicode_member_name_goes_in_and_comes_back_as_its_utf8` (zip and
      tar). The test fixture's `set_mtime` goes through `platform::fs::set_times`
      (a read-only `File` cannot set a time on Windows) and the mode checks
      compare with the platform's `st_mode` as well as, on Unix, the literal
      (P3.30, bin 2).
- [x] **P3.8** git: `git/status.rs:556–565` (`insert`), the `-c core.hooksPath=/dev/null`
      argument at `:291` becomes `platform::process::NULL_DEVICE` (`/dev/null` or
      `NUL`). Done when: `git/tests.rs` passes on Linux; the three real-`git` tests
      keep their `GIT_CONFIG_GLOBAL` override through the same constant. — done
      (port/paths). `insert` (S1.16) and `core.hooksPath` (S1.13) were already
      through the platform; the tests' `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`
      now are too. `non_utf8_filenames_still_get_a_dot` is `#[cfg(unix)]`, with
      a Unicode twin that runs everywhere, and the hostile-config test escapes
      the backslashes of the marker's path inside git's quoted value (P3.30,
      bin 2 both).
- [x] **P3.9** rsync and sync: `sync/rsync.rs:230–244, 383–450, 1143–1211` and
      `sync/mod.rs:310–318` (`is_debris`). The whole rsync-backed remote sync is
      `Unsupported` on Windows (no rsync, and `endpoint`'s `host:` syntax collides
      with drive letters), so these sites only need to *compile*: route the byte
      conversions through `platform::os` and let Phase 4 stub `sync::rsync::available`
      to `false` on Windows. macOS keeps the real body (Phase 2 handles the rsync
      version gate). Done when: df-core compiles on all three targets and
      `sync/tests.rs` passes on Linux. — done (port/paths), nothing left to
      change: S1.16 routed every byte conversion here through `platform::os`,
      and S1.14 already gates `available` on `platform::process::HAS_RSYNC`
      (`false` on Windows), so Phase 4 has no stub to add. df-core compiles on
      all three targets (CI, and the local cross checks) and `sync/tests.rs`
      passes on Linux.
- [x] **P3.10** `ops.rs:109–120` (`is_url`) scans the UTF-8 view (`to_str`), and a
      non-UTF-8 path is simply not a URL. `ops.rs:196–204` (`trim_trailing_slash`)
      becomes `path::trim_trailing_separator`. `ops/create.rs:36–43` uses
      `path::has_trailing_separator` for the "create a directory" signal. Done when:
      the three functions have no `OsStrExt` and their tests pass. — done
      (port/paths). `ops::trim_trailing_slash` is gone; `remove_tree` and
      `remove_tree_unchecked` trim with `path::trim_trailing_separator` (Linux:
      the same bytes, the root kept). `create` reads the marker with
      `has_trailing_separator` and trims that one separator, as before; a new
      test makes a folder with the platform's own separator. `is_url` keeps
      S1.16's `platform::os::as_bytes` scan (Decisions log).
- [x] **P3.11** `du/fstype.rs:64`, `ops/copy.rs:672`, `sync/mod.rs:481`,
      `fs/inotify.rs:109`: `CString` construction for libc calls. These move into
      `platform/` in Phase 1 (they are Unix-only bodies) and keep `OsStrExt` there,
      which is allowed inside `platform/`. Done when: confirmed moved; nothing to do
      here beyond checking. — done (port/paths), confirmed: the four are
      `platform/linux/fs.rs` (`magic_of`), `platform/unix/fs.rs` (`set_times`,
      `writable`) and `platform/linux/inotify.rs`; `grep -rn
      "OsStrExt\|OsStringExt\|CString::new" crates/df-core/src` outside
      `platform/` finds only the `#[cfg(unix)]` state fixture test.

## 3. Literal `/` and the root (appendix §3.2, §3.3)

- [x] **P3.12** `ops.rs:73–98` (`normalize`): pop components with `Path::components`
      keeping `Prefix` and `RootDir`; the empty case yields `path::root_of`. On
      Windows `is_absolute` is false for `\foo` and `/foo`; both resolve against the
      current directory's drive, which is what `current_dir().join` does. Done when:
      the existing `normalize` tests pass on Linux and a Windows-shaped test
      (`C:\a\b\..\..` → `C:\`) exists (built only under `cfg!(windows)`). — done
      (port/paths). The walk already went by `components` and `PathBuf::pop`,
      which never pops a prefix or a root; the one change is the empty case,
      `root_of` instead of `/` (on Unix `/` either way). The Linux asserts of
      `normalize_cleans_lexically` run under `cfg!(unix)` as they were, beside
      Windows ones (`C:\a\b\..\..`, a share's root, `\a` onto the current
      drive) under `cfg!(windows)` (P3.30: bin 1 for the root, bin 2 for the
      literals).
- [x] **P3.13** Root rails and fallbacks: `ops/delete.rs:35, 63`, `ops/create.rs:145`,
      `ops/link.rs:88`, `fs/entry.rs:104–109` (a path with no `file_name` is a root:
      its display name is `path::segments(p)[0].label`). Done when: no
      `Path::new("/")`/`PathBuf::from("/")` outside `platform/` and tests in df-core
      (`grep` is the test) and the delete-rail tests pass on Linux. — done
      (port/paths). The root rail is `path::is_root` of the normalized target
      (Linux: exactly `/` as before); the unreadable-cwd fallback, the link
      parent and the rename message fall back to `path::root_of`; `Entry::read`
      names a root by its first segment, and anything else without a
      `file_name` as before. The grep finds only test code. New tests: a
      Windows drive and share refused by the rail, and a root entry named as
      the breadcrumb names it (P3.30: `rail_refuses_the_root` was bin 1).
- [ ] **P3.14** Home expansion: `config.rs:767–775` and `state/pins.rs:59, 93`,
      `vfs/config.rs:144–151` all call one `df_core::path::expand_home(text: &str) ->
      String` that uses `platform::dirs::home()` (`HOME` on Unix, `USERPROFILE` on
      Windows) and keeps the separator the person typed. `ops/trash.rs:489` and
      `zoxide/mod.rs:123, 125` move behind `platform::dirs` in Phase 1/5. Done when:
      one `expand_home` exists and `config.rs`, `pins.rs`, `vfs/config.rs` tests pass.
- [ ] **P3.15** `config.rs:1220–1229` (`Theme::dir_icon`): a pattern containing `/`
      is matched against `path::display(p)` with `\` normalized to `/` on Windows
      first, so a config written with `/` matches on every target. Done when: an
      icon test with a Windows-shaped path passes.
- [ ] **P3.16** `zoxide/mod.rs:338–364` (`classify`): last component via
      `Path::new(s).file_name()`; and `archive/write/mod.rs:187, 213` (leaf of a typed
      archive name) via `Path::new(text).file_name()`. Done when: their tests pass and
      a `\`-separated input has a test under `cfg!(windows)`.
- [ ] **P3.17** `sync/mod.rs:267–282` (`SyncPlan::label`): render `rel` with
      `path::display` and the platform separator; the trailing separator for folders
      is `std::path::MAIN_SEPARATOR`. Done when: the label test passes on Linux
      unchanged.

## 4. Case (appendix §3.4)

- [ ] **P3.18** Overwrite and self-paste decisions use `platform::fs::same_file`:
      `ops/create.rs:139–141` (rename into itself — a case-only rename `Foo`→`foo`
      must be *allowed* on a case-insensitive volume: when `same_file` is true and
      the names differ only by case, do the rename via a temporary name),
      `ops/paste.rs:273`, `ops/copy.rs` `copies_into_itself` and `move_path`
      (already through `resolved`; confirm), `sync/plan.rs:178–199` (`visit`:
      an existing destination found under another spelling is "Changed", not
      "New"; confirm `case_twins` at `:366–424` already covers it and extend the test).
      (macOS too.) Done when: a test on a case-insensitive temp volume is skipped
      unless the volume is case-insensitive (probe by creating `A` and opening `a`),
      and passes there.
- [ ] **P3.19** Lookup tables keyed on `path::key`: `state/mod.rs:202, 305–318,
      357, 367, 382–398` (`dirs` map: every `get` **and every write path**),
      `fs/memory.rs:29, 53, 69` (`Recent`), `ops.rs:124–135` (`is_ancestor`/
      `is_strict_ancestor`: both sides through `key` before `starts_with`),
      `sync/plan.rs:51–67` (duplicate yanked names) and `:307–321` (`ours`/`theirs`
      sets: fold with `key` on the name, and keep `case_twins`),
      `fs/history.rs:54`, `du/cache.rs:220, 434, 478`, `du/scanner.rs:497, 604–614`,
      `git/status.rs:180–233, 580–585`, `git/cache.rs:92–93`, `state/pins.rs:92–94`.
      Rule: key at insert **and** lookup; store the original spelling as the value
      when the spelling is displayed. On Unix `key` is the identity, so this is a
      no-op there; the test is that Linux tests pass unchanged. Done when: each
      listed map goes through `key` and a Windows-shaped test per module exists.
- [ ] **P3.20** Leave as-is, with a Decisions-log line each: `ops/paste.rs:128–176,
      193, 400, 457, 462` (clipboard membership: both spellings come from
      `read_dir`), `ops/trash.rs:316`, `ops/journal.rs:872, 911`, `fs/mod.rs:370,
      462, 506` (cursor by name from `read_dir`), `sync/mod.rs:367–383`,
      `sync/rsync.rs:462–473, 814–839`, `vfs/config.rs:214, 224`. Done when: logged.
- [ ] **P3.25** Thumbnail cache key (`preview/cache.rs:136–160`) hashes `Path` via
      `Hash for Path`, whose input bytes are platform-specific. Decision: leave the
      code; yazi-parity of *keys* is not attempted on Windows (the directory is still
      shared, and a thumbnail yazi wrote is simply regenerated once). Record it in
      the module doc. Done when: the doc comment says so.
- [ ] **P3.26** Removing links: `ops/delete.rs:96–111, 118–130` and
      `ops/journal.rs:879, 933` remove a symlink with `remove_file`; on Windows a
      directory symlink or junction needs `remove_dir`. Add
      `platform::fs::remove_link(path) -> io::Result<()>` (Unix: `remove_file`;
      Windows: `remove_dir` when `symlink_metadata().file_type().is_symlink_dir()`,
      else `remove_file`) and call it at those sites. Done when: Linux tests
      unchanged; a Windows-runner test removes a directory symlink made under
      Developer Mode (skipped when `symlink_dir` fails with 1314).
- [ ] **P3.27** `ops/link.rs:40–70` (`relative_to`): when the two paths have
      different `Prefix` components (different drives or a UNC), return the target
      absolute rather than a relative path with prefix components in it. Done when:
      a `cfg!(windows)` test for `C:\a` → `D:\b` yields `D:\b`.
- [ ] **P3.28** `fs/kind.rs:315–357, 402` decides `Executable` by `mode & 0o111`;
      with the synthesized Windows mode nothing is executable. Add
      `platform::meta::is_executable(name: &OsStr, mode: u32) -> bool` (Unix: the
      mode test as today; Windows: extension in `PATHEXT`) and call it from
      `classify`. Done when: `classify("setup.exe")` is `Executable` under
      `cfg!(windows)` in a test; Linux tests unchanged.

## 5. Names (appendix §3.5 and the Windows rules)

- [ ] **P3.21** `path::name_is_valid` is called by `ops/create.rs` (create, rename),
      `rename/` (bulk rename preview says why a row cannot be applied, using the
      existing "cannot be filled" mechanism), `archive/tree.rs:469–494`
      (`name_is_unsafe` adds the Windows rule set through it, so extraction refuses
      `con.txt` and `a:b` on Windows), `vfs/mod.rs:657–668` (`download_to_temp`
      sanitizes a remote name for the local temp file: replace each invalid character
      with `_`, since that file is disposable). Done when: tests for each on every
      target (the rule set is testable on Linux by calling the strict variant
      directly).
- [ ] **P3.22** `fs/scan.rs:328, 369` and `fs/mod.rs:506`: `Entry.name` stays a lossy
      `String` for display, but `selected_paths` and every op takes `Entry.path`
      (the real `PathBuf`), never `dir.join(name)`. Audit and fix the callers listed
      in the appendix row. Done when: `grep -n "join(&*name\|join(name" crates/df-core/src`
      finds no remaining rebuild of a path from a lossy name.
- [ ] **P3.23** `test_support::gnarly_names()` returns the Windows-legal subset when
      `cfg!(windows)` (drop `\n`, `\t`, `\`, `"`, and the 255-`x` name becomes 200
      to stay under `MAX_PATH` with the temp prefix). Linux list unchanged. Done
      when: Linux tests unchanged; the df-core test suite compiles on Windows.

## 6. Tests (appendix §5)

- [ ] **P3.24** Sweep the 36 test files: replace literal `/tmp/...` fixture paths that
      reach the filesystem with `TempTree`/`temp_dir()`; leave pure-string tests
      that only parse or format (they still pass on Windows because they never
      touch a disk) but wrap those that assert Unix-shaped output (`== "/"`,
      `ends_with("/Work")`) in `#[cfg(unix)]` with a Windows twin where cheap. Mode,
      symlink, uid, `EXDEV` tests get `#[cfg(unix)]`. Done when: `cargo test -p
      df-core` is green on all three CI targets, with the Linux count not lower
      than before this phase.

## Decisions log

- 2026-09-25 — UTF-8 for persisted paths, byte-identical on Unix; non-Unicode
  Windows names refused, not mangled.
- 2026-09-25 — `path::key` lowercases on Windows only; macOS keeps identity keys.
- 2026-09-25 — Drive roots have no parent pane; no synthetic "This PC".
- 2026-09-25 — `Entry.path` keeps the `sftp://` URL; the fix is discipline at the
  call sites, not a type change.
- 2026-09-25 — `MADE_BY` in written zips stays Unix on every target.
- 2026-09-29 — Before any P3 task changed code, `main`'s formats were pinned
  as fixtures (`state/testdata/state-419a776`, `zoxide/testdata/db-v3.zo`,
  written by the code at 419a776) with tests that Linux loads them and
  writes the state file back byte for byte. Pins, tags, panes and tabs live
  in the state file, so its fixture covers them; the undo journal is in
  memory only (appendix §3.6), so there is no journal file to pin.
- 2026-09-29 — P3.2: `path::key` returns `Cow<'_, Path>`, not `PathBuf`, and
  `path::into_key(PathBuf) -> PathBuf` is its owned twin. Reason: on Unix the
  key is the identity, and `git::StatusData::status_for` asks for every
  visible row on every frame; a `PathBuf` return would copy the path each
  time, a cost Linux never had.
- 2026-09-29 — P3.2: the constants the plan put in `platform::path` are in
  `platform::os` (`STRICT_NAMES`, `FOLD_CASE`, and P3.6's `MAX_NAME`,
  `NAME_IN_UTF16`). Reason: `os` is already the module about how this
  platform spells a name, and macOS takes it from `unix` through its existing
  re-export, so no new module has to be threaded through every target.
- 2026-09-29 — P3.2: `name_is_valid`'s Unix rule is also public as
  `name_is_valid_permissive`, beside `name_is_valid_strict`, and `key`'s
  Windows fold as `folded`, so that each platform's rule is tested on Linux.
- 2026-09-29 — P3.4: Windows' volume serial, file index and link count come
  from `platform::meta::identity(path, &meta)`, not from `dev`/`ino`/`nlink`
  of a `Metadata`, which stay `0`/`0`/`1` there. Reason: std keeps those
  fields unstable (`windows_by_handle`) and a `Metadata` has no path to open,
  so the plan's "volume serial of the path" cannot be read from one. The
  only callers of the bare three off Linux are the du walk's boundary check,
  which is right with one device because a walk never follows the reparse
  point a mounted volume hangs from, and the Linux-only chmod walk. On Unix
  `identity` reads the `stat` the caller already has, so the du walk makes
  exactly the calls it made before.
- 2026-09-29 — P3.10: `is_url` keeps scanning `platform::os::as_bytes`
  rather than `to_str`. On Windows that already is the UTF-8 view, and a
  non-Unicode path is not a URL, as the task wants; on Unix it is the bytes,
  so an `sftp://` path with a non-UTF-8 byte in it stays a URL there, as it
  is today. `to_str` would have turned it into a local path on Linux, a
  change the task did not need.
- 2026-09-29 — P3.10: `create` trims exactly one trailing separator, as it
  did, not every one through `trim_trailing_separator`: `a//` stays `a/`
  and a typed `/` stays "no name given" on Linux.

## Open questions

(none)
