//! One dialog, from the portal's call to its answer.
//!
//! The call's options are read into a [`Dialog`]; the dialog is written out as
//! a request file (`$XDG_RUNTIME_DIR/delightfile/portal-<pid>-<n>.toml`) that
//! `--chooser-request` reads back into a [`crate::cli::Chooser`]; a picker
//! window is started on it and waited for; and whatever it wrote beside the
//! request file — one absolute path per line, as `--chooser-file` always has —
//! becomes the `file://` URIs of the answer.
//!
//! A file between the two processes rather than more flags, because what the
//! portal carries is structured (filters are named lists of two kinds of
//! pattern) and the one parser already in the program for structured text is
//! the TOML one the config uses.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use super::lock;
use crate::cli::TypeFilter;
use crate::dbus::Value;

/// The portal's response codes.
pub const SUCCESS: u32 = 0;
pub const CANCELLED: u32 = 1;
/// "The interaction was ended in some other way": closed by the portal, a
/// window that could not be started, a window that died.
pub const OTHER: u32 = 2;

/// How often a request thread looks at its window.
///
/// Polled rather than blocked on, because a blocked `wait` would own the
/// `Child` for as long as the dialog is up, and `Close` needs it to kill the
/// window. `Child::kill` on a child this thread may have reaped a moment ago
/// is safe (std knows); a raw `kill(pid)` would not be, the pid being free to
/// belong to somebody else by then. Twenty looks a second at an exit status
/// is nothing, and 50 ms between a pick and its answer is not something a
/// person can see.
const POLL: Duration = Duration::from_millis(50);

/// Which of the three file-chooser methods a call was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Open,
    Save,
    /// Pick a *folder* to save several named files into.
    SaveFiles,
}

impl Kind {
    pub fn of_method(member: &str) -> Option<Kind> {
        match member {
            "OpenFile" => Some(Kind::Open),
            "SaveFile" => Some(Kind::Save),
            "SaveFiles" => Some(Kind::SaveFiles),
            _ => None,
        }
    }

    pub fn method(self) -> &'static str {
        match self {
            Kind::Open => "OpenFile",
            Kind::Save => "SaveFile",
            Kind::SaveFiles => "SaveFiles",
        }
    }

    /// How the request file spells it (`kind = "…"`).
    fn word(self) -> &'static str {
        match self {
            Kind::Open => "open",
            Kind::Save => "save",
            Kind::SaveFiles => "save-files",
        }
    }
}

/// Everything a file-chooser call asked for that the window can use.
///
/// `modal`, `parent_window` and `choices` are read past: the window is its
/// own toplevel with no way to attach to the caller's, and has no widgets for
/// a caller's extra combo boxes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Dialog {
    /// The `Request` object this call is, for `Close`.
    pub handle: String,
    pub kind: Kind,
    /// The call's `title` argument; `None` when it was empty.
    pub title: Option<String>,
    /// `accept_label`, its mnemonic underscores gone.
    pub accept: Option<String>,
    pub multiple: bool,
    pub directory: bool,
    /// `current_folder`.
    pub folder: Option<PathBuf>,
    /// `current_name`: a save's suggested file name.
    pub name: Option<String>,
    /// `current_file`: the file a save is saving over, when it is one.
    pub file: Option<PathBuf>,
    pub filters: Vec<TypeFilter>,
    /// `current_filter`, by name.
    pub current_filter: Option<String>,
    /// `SaveFiles`' `files`: the names that go into the chosen folder, as
    /// bytes, because they are file names.
    pub files: Vec<Vec<u8>>,
}

impl Dialog {
    /// Read a call's `(handle, app_id, parent_window, title, options)`.
    ///
    /// Strict about the five arguments (a call that does not have them is
    /// answered `InvalidArgs`), lenient about the options: one of the wrong
    /// type is ignored, the way GTK's own backend ignores it, rather than
    /// failing a dialog that would otherwise have worked.
    pub fn from_args(kind: Kind, args: &[Value]) -> Result<Dialog, String> {
        let [Value::Path(handle), Value::Str(_app_id), Value::Str(_parent_window), Value::Str(title), Value::Dict(options)] =
            args
        else {
            return Err("expected (handle, app_id, parent_window, title, options)".into());
        };
        let mut dialog = Dialog {
            handle: handle.clone(),
            kind,
            title: Some(title.clone()).filter(|title| !title.is_empty()),
            ..Dialog::default()
        };
        let mut current = None;
        for (key, value) in options {
            let Some(key) = key.as_str() else {
                continue;
            };
            let value = value.peeled();
            match key {
                "accept_label" => {
                    dialog.accept = value
                        .as_str()
                        .map(strip_mnemonic)
                        .filter(|label| !label.is_empty());
                }
                "multiple" if kind == Kind::Open => {
                    dialog.multiple = value.as_bool().unwrap_or(false);
                }
                "directory" if kind == Kind::Open => {
                    dialog.directory = value.as_bool().unwrap_or(false);
                }
                "current_folder" => dialog.folder = value.as_bytes().and_then(path_from_bytes),
                "current_name" if kind == Kind::Save => {
                    dialog.name = value
                        .as_str()
                        .filter(|name| !name.is_empty())
                        .map(str::to_string);
                }
                "current_file" if kind == Kind::Save => {
                    dialog.file = value.as_bytes().and_then(path_from_bytes);
                }
                "filters" if kind != Kind::SaveFiles => {
                    if let Value::Array(items) = value {
                        dialog.filters = items.iter().filter_map(filter_from).collect();
                    }
                }
                "current_filter" if kind != Kind::SaveFiles => current = filter_from(value),
                "files" if kind == Kind::SaveFiles => {
                    if let Value::Array(items) = value {
                        dialog.files = items
                            .iter()
                            .filter_map(Value::as_bytes)
                            .map(|bytes| up_to_nul(&bytes).to_vec())
                            .collect();
                    }
                }
                _ => {}
            }
        }
        // A current filter with no list to pick it from is, the portal says,
        // a filter applied unconditionally — which a list of one is.
        if let Some(current) = current {
            if dialog.filters.is_empty() {
                dialog.filters.push(current.clone());
            }
            dialog.current_filter = Some(current.name);
        }
        Ok(dialog)
    }
}

/// A GTK label with its mnemonic marks taken out: `_Upload` → `Upload`, and
/// the escaped `__` → a literal `_`.
pub fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '_' {
            out.push(c);
        } else if chars.peek() == Some(&'_') {
            chars.next();
            out.push('_');
        }
        // A lone `_` only marks the letter after it as the access key.
    }
    out
}

/// A `ay` path as the portal sends it: NUL-terminated, and only meaningful
/// when absolute.
fn path_from_bytes(bytes: Vec<u8>) -> Option<PathBuf> {
    let bytes = up_to_nul(&bytes);
    (!bytes.is_empty())
        .then(|| PathBuf::from(OsString::from_vec(bytes.to_vec())))
        .filter(|path| path.is_absolute())
}

/// The bytes before the first NUL. A path cannot hold one, and the portal
/// terminates every byte string with one.
fn up_to_nul(bytes: &[u8]) -> &[u8] {
    bytes.split(|b| *b == 0).next().unwrap_or_default()
}

/// One `(sa(us))`: a name and its patterns, `0` a glob and `1` a MIME type.
/// A pattern of any other kind is skipped — a newer portal's idea this build
/// cannot match on.
fn filter_from(value: &Value) -> Option<TypeFilter> {
    let Value::Struct(fields) = value.peeled() else {
        return None;
    };
    let [name, Value::Array(patterns)] = fields.as_slice() else {
        return None;
    };
    let mut filter = TypeFilter {
        name: name.as_str()?.to_string(),
        ..TypeFilter::default()
    };
    for pattern in patterns {
        let Value::Struct(pair) = pattern else {
            continue;
        };
        match pair.as_slice() {
            [Value::U32(0), Value::Str(glob)] => filter.globs.push(glob.clone()),
            [Value::U32(1), Value::Str(mime)] => filter.mimes.push(mime.clone()),
            _ => {}
        }
    }
    Some(filter)
}

// ── The request file ────────────────────────────────────────────────────────

/// The dialog as the TOML `--chooser-request` reads (see
/// `crate::cli::parse_request`), starting in `folder`.
///
/// Paths are written with `to_string_lossy`: TOML is text, and a folder whose
/// name is not UTF-8 comes out with U+FFFD in it. The window then cannot find
/// that folder and starts in its fallback instead — a wrong start, never a
/// wrong answer, because the answer comes back through the picked paths,
/// which are bytes end to end.
pub fn request_toml(dialog: &Dialog, folder: Option<&Path>) -> String {
    let mut out = String::from(
        "# A file dialog xdg-desktop-portal asked delightfile for (delightfile --portal).\n",
    );
    let mut line = |key: &str, value: String| {
        let _ = writeln!(out, "{key} = {value}");
    };
    line("kind", toml_string(dialog.kind.word()));
    if let Some(title) = &dialog.title {
        line("title", toml_string(title));
    }
    if let Some(accept) = &dialog.accept {
        line("accept", toml_string(accept));
    }
    line("multiple", dialog.multiple.to_string());
    line("directory", dialog.directory.to_string());
    if let Some(folder) = folder {
        line("folder", toml_string(&folder.to_string_lossy()));
    }
    if let Some(name) = &dialog.name {
        line("name", toml_string(name));
    }
    if let Some(file) = &dialog.file {
        line("file", toml_string(&file.to_string_lossy()));
    }
    if let Some(current) = &dialog.current_filter {
        line("current_filter", toml_string(current));
    }
    for filter in &dialog.filters {
        let _ = writeln!(out, "\n[[filter]]");
        let _ = writeln!(out, "name = {}", toml_string(&filter.name));
        if !filter.globs.is_empty() {
            let _ = writeln!(out, "glob = {}", toml_array(&filter.globs));
        }
        if !filter.mimes.is_empty() {
            let _ = writeln!(out, "mime = {}", toml_array(&filter.mimes));
        }
    }
    out
}

/// A TOML basic string: quotes and backslashes escaped, and every control
/// character (newlines included — the parser reads a line at a time) written
/// as `\uXXXX`.
fn toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn toml_array(items: &[String]) -> String {
    let items: Vec<String> = items.iter().map(|item| toml_string(item)).collect();
    format!("[{}]", items.join(", "))
}

// ── Running it ──────────────────────────────────────────────────────────────

/// Everything a request reads from the environment, read once at startup so a
/// test can hand the service another one.
#[derive(Debug, Clone, Default)]
pub struct Setup {
    /// The program each dialog runs, and each "Show in folder" window
    /// ([`super::show`]): this binary, unless `DELIGHTFILE_PICKER_EXE` names
    /// another (the tests' stub).
    pub picker: Option<PathBuf>,
    /// `$XDG_RUNTIME_DIR/delightfile`: where request and answer files live.
    pub runtime_dir: Option<PathBuf>,
    /// `$XDG_STATE_HOME/delightfile/portal-last-dir`.
    pub last_dir_file: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl Setup {
    pub fn from_env() -> Setup {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        Setup {
            picker: picker_exe(var("DELIGHTFILE_PICKER_EXE")),
            runtime_dir: var("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("delightfile")),
            // Beside the view-state file, by the same XDG rules.
            last_dir_file: df_core::state::state_path_from(var("XDG_STATE_HOME"), var("HOME"))
                .map(|state| state.with_file_name("portal-last-dir")),
            home: var("HOME").map(PathBuf::from),
        }
    }
}

/// The program a dialog runs.
///
/// Reinstalling delightfile while the service runs replaces the file under
/// it, and the kernel then reports this process's executable as
/// `<path> (deleted)`. The path itself holds the new binary — the one a new
/// window should be — so the suffix is dropped rather than every dialog
/// failing to start until the service is restarted.
fn picker_exe(overridden: Option<OsString>) -> Option<PathBuf> {
    if let Some(exe) = overridden {
        return Some(PathBuf::from(exe));
    }
    let exe = std::env::current_exe()
        .map_err(|e| log::warn!("portal: cannot find my own executable: {e}"))
        .ok()?;
    if !exe.exists() {
        if let Some(live) = exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
            return Some(PathBuf::from(live));
        }
    }
    Some(exe)
}

/// A dialog's state while its window is up, shared with the reader loop so
/// `Close` can reach it.
#[derive(Debug, Default)]
pub struct Pending {
    closed: bool,
    child: Option<Child>,
    /// The request and answer files, for [`Pending::abandon`].
    files: Vec<PathBuf>,
}

impl Pending {
    /// `Request.Close`: kill the window if there is one, and make sure there
    /// never will be. The request thread sees the flag and answers 2.
    pub fn close(&mut self) {
        self.closed = true;
        if let Some(child) = &mut self.child {
            if let Err(e) = child.kill() {
                log::warn!("portal: could not close the picker window: {e}");
            }
        }
    }

    /// The service is going: close, and clear the files, because the thread
    /// that would have is going with it.
    pub fn abandon(&mut self) {
        self.close();
        for file in &self.files {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// What a dialog answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub response: u32,
    pub uris: Vec<String>,
}

impl Answer {
    pub fn cancelled() -> Answer {
        Answer {
            response: CANCELLED,
            uris: Vec::new(),
        }
    }

    pub fn other() -> Answer {
        Answer {
            response: OTHER,
            uris: Vec::new(),
        }
    }
}

/// Put the dialog on screen and wait for it.
pub fn ask(dialog: &Dialog, setup: &Setup, number: u64, pending: &Mutex<Pending>) -> Answer {
    let files = match RequestFiles::write(dialog, setup, number) {
        Ok(files) => files,
        Err(e) => {
            log::warn!("portal: {}: {e}", dialog.handle);
            return Answer::other();
        }
    };
    lock(pending).files = vec![files.request.clone(), files.out.clone()];
    match run_window(setup, &files, pending) {
        Ended::Exited { cleanly } => {
            let picked = read_picked(&files.out);
            let answer = answer_for(dialog, &picked, cleanly);
            if answer.response == SUCCESS {
                remember(setup, dialog, &picked);
            }
            answer
        }
        Ended::Closed | Ended::NotStarted => Answer::other(),
    }
    // `files` is dropped here, before the answer is sent: nothing is left in
    // the runtime directory once the caller has its URIs.
}

/// How a picker window's life ended.
enum Ended {
    Exited {
        cleanly: bool,
    },
    /// `Close` came first: before the window started, or while it was up.
    Closed,
    NotStarted,
}

fn run_window(setup: &Setup, files: &RequestFiles, pending: &Mutex<Pending>) -> Ended {
    let Some(exe) = &setup.picker else {
        return Ended::NotStarted;
    };
    {
        let mut state = lock(pending);
        if state.closed {
            return Ended::Closed;
        }
        let mut chooser_file = OsString::from("--chooser-file=");
        chooser_file.push(&files.out);
        let mut request = OsString::from("--chooser-request=");
        request.push(&files.request);
        match Command::new(exe)
            .arg(chooser_file)
            .arg(request)
            .stdin(Stdio::null())
            .spawn()
        {
            Ok(child) => state.child = Some(child),
            Err(e) => {
                log::warn!("portal: could not start {}: {e}", exe.display());
                return Ended::NotStarted;
            }
        }
    }
    loop {
        {
            let mut state = lock(pending);
            let closed = state.closed;
            let Some(child) = state.child.as_mut() else {
                return Ended::NotStarted;
            };
            match child.try_wait() {
                Ok(Some(status)) if closed => {
                    log::info!("portal: picker window closed ({status})");
                    return Ended::Closed;
                }
                Ok(Some(status)) => {
                    return Ended::Exited {
                        cleanly: status.success(),
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("portal: lost track of the picker window: {e}");
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ended::NotStarted;
                }
            }
        }
        std::thread::sleep(POLL);
    }
}

/// The answer to a dialog whose window has exited, given what it picked.
///
/// Nothing picked is a cancel when the window quit the way a person quits it
/// (`q`, `Esc`, Cancel, closing it), and "ended some other way" when it did
/// not — killed, crashed, or refused the request file.
pub fn answer_for(dialog: &Dialog, picked: &[PathBuf], cleanly: bool) -> Answer {
    let Some(first) = picked.first() else {
        return if cleanly {
            Answer::cancelled()
        } else {
            Answer::other()
        };
    };
    let uris = match dialog.kind {
        Kind::Open => picked.iter().map(|path| file_uri(path)).collect(),
        // "It must have exactly one element."
        Kind::Save => vec![file_uri(first)],
        Kind::SaveFiles => save_files_uris(first, &dialog.files),
    };
    Answer {
        response: SUCCESS,
        uris,
    }
}

/// `SaveFiles`: each of the caller's names inside the chosen folder, in the
/// caller's order.
///
/// A name that is not a bare file name (`a/b`, `..`) is dropped with a
/// warning rather than joined: xdg-desktop-portal already refuses those, and
/// if one got through, a URI outside the folder the person chose is the one
/// answer this must not give.
pub fn save_files_uris(folder: &Path, names: &[Vec<u8>]) -> Vec<String> {
    names
        .iter()
        .filter_map(|name| {
            let name = OsStr::from_bytes(name);
            match Path::new(name).file_name() {
                Some(base) if base == name => Some(file_uri(&folder.join(base))),
                _ => {
                    log::warn!("portal: SaveFiles: skipping the name {name:?}");
                    None
                }
            }
        })
        .collect()
}

/// A `file://` URI for an absolute path, percent-encoded byte by byte.
///
/// Only RFC 3986's unreserved characters and `/` are left as they are, so a
/// `#` (a fragment to GLib), a `%`, a space, and every non-ASCII or non-UTF-8
/// byte are escaped — the portal passes the URI to GLib, which decodes it back
/// into exactly these bytes.
pub fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            uri.push(char::from(b));
        } else {
            let _ = write!(uri, "%{b:02X}");
        }
    }
    uri
}

/// The paths a picker wrote: one absolute path per line. A line that is not
/// absolute cannot be made a `file://` URI and is dropped, as the portal asks.
fn read_picked(out: &Path) -> Vec<PathBuf> {
    let Ok(bytes) = std::fs::read(out) else {
        return Vec::new();
    };
    bytes
        .split(|b| *b == b'\n')
        .filter(|line| line.first() == Some(&b'/'))
        .map(|line| PathBuf::from(OsStr::from_bytes(line)))
        .collect()
}

/// Where a dialog with no `current_folder` starts: beside the file it is
/// saving over (the window opens that file's folder with the cursor on it),
/// else where the last pick came from, else home.
fn start_folder(dialog: &Dialog, setup: &Setup) -> Option<PathBuf> {
    if let Some(folder) = &dialog.folder {
        return Some(folder.clone());
    }
    if dialog.file.is_some() {
        return None;
    }
    last_dir(setup).or_else(|| setup.home.clone())
}

/// The folder the last successful pick came from, if it still is one.
fn last_dir(setup: &Setup) -> Option<PathBuf> {
    let bytes = std::fs::read(setup.last_dir_file.as_ref()?).ok()?;
    let line = bytes.split(|b| *b == b'\n').next()?;
    let dir = PathBuf::from(OsStr::from_bytes(line));
    (dir.is_absolute() && dir.is_dir()).then_some(dir)
}

/// Remember where this pick came from: a picked folder itself, or the folder
/// a picked file is in.
fn remember(setup: &Setup, dialog: &Dialog, picked: &[PathBuf]) {
    let (Some(file), Some(first)) = (&setup.last_dir_file, picked.first()) else {
        return;
    };
    let folder_pick =
        dialog.kind == Kind::SaveFiles || (dialog.kind == Kind::Open && dialog.directory);
    let dir = if folder_pick {
        Some(first.as_path())
    } else {
        first.parent()
    };
    let Some(dir) = dir else {
        return;
    };
    let mut line = dir.as_os_str().as_bytes().to_vec();
    line.push(b'\n');
    let written = file
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(file, line));
    if let Err(e) = written {
        log::warn!("portal: could not remember {}: {e}", dir.display());
    }
}

/// The request file and the answer file beside it, removed when dropped.
struct RequestFiles {
    request: PathBuf,
    out: PathBuf,
}

impl RequestFiles {
    fn write(dialog: &Dialog, setup: &Setup, number: u64) -> Result<RequestFiles, String> {
        let dir = setup
            .runtime_dir
            .as_ref()
            .ok_or("no $XDG_RUNTIME_DIR to put the request file in")?;
        private_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let stem = format!("portal-{}-{number}", std::process::id());
        let files = RequestFiles {
            request: dir.join(format!("{stem}.toml")),
            out: dir.join(format!("{stem}.out")),
        };
        let text = request_toml(dialog, start_folder(dialog, setup).as_deref());
        private_file(&files.request, text.as_bytes())
            .map_err(|e| format!("{}: {e}", files.request.display()))?;
        // Created empty, and 0600 like the request: the window's write
        // replaces the contents and keeps the mode, and a window that picks
        // nothing leaves it empty, which is the cancel.
        private_file(&files.out, b"").map_err(|e| format!("{}: {e}", files.out.display()))?;
        Ok(files)
    }
}

impl Drop for RequestFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.request);
        let _ = std::fs::remove_file(&self.out);
    }
}

/// A directory only this user can read: what file names and folders people
/// are picking is nobody else's business.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// A new 0600 file holding `bytes`, replacing any stale one of the same name
/// (a service that died with a dialog open, under a pid since reused).
fn private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

    use super::*;

    fn s(text: &str) -> Value {
        Value::Str(text.to_string())
    }

    fn filter_value(name: &str, patterns: &[(u32, &str)]) -> Value {
        Value::Struct(vec![
            s(name),
            Value::Array(
                patterns
                    .iter()
                    .map(|(kind, pattern)| Value::Struct(vec![Value::U32(*kind), s(pattern)]))
                    .collect(),
            ),
        ])
    }

    /// A call's five arguments, with `options` as `(key, signature, value)`.
    fn call(title: &str, options: Vec<(&str, &str, Value)>) -> Vec<Value> {
        vec![
            Value::Path("/org/freedesktop/portal/desktop/request/1_9/t".into()),
            s("org.example.App"),
            s("wayland:abc"),
            s(title),
            Value::Dict(
                options
                    .into_iter()
                    .map(|(key, sig, value)| (s(key), Value::variant(sig, value)))
                    .collect(),
            ),
        ]
    }

    fn filter(name: &str, globs: &[&str], mimes: &[&str]) -> TypeFilter {
        TypeFilter {
            name: name.to_string(),
            globs: globs.iter().map(|g| g.to_string()).collect(),
            mimes: mimes.iter().map(|m| m.to_string()).collect(),
        }
    }

    /// A scratch directory of this test's own.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("df-portal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn mnemonics_are_stripped() {
        assert_eq!(strip_mnemonic("_Upload"), "Upload");
        assert_eq!(strip_mnemonic("_Open"), "Open");
        assert_eq!(strip_mnemonic("Save _As"), "Save As");
        assert_eq!(strip_mnemonic("__init__"), "_init_");
        assert_eq!(strip_mnemonic("a__b"), "a_b");
        assert_eq!(strip_mnemonic("trailing_"), "trailing");
        assert_eq!(strip_mnemonic("Plain"), "Plain");
        assert_eq!(strip_mnemonic(""), "");
    }

    /// Space, `#` (a fragment to GLib), `%` (an escape to everyone) and
    /// anything outside ASCII are escaped; a byte that is not UTF-8 at all
    /// still makes a URI, since the URI is of bytes.
    #[test]
    fn a_path_becomes_a_uri_byte_for_byte() {
        assert_eq!(
            file_uri(Path::new("/tmp/a b/#1 100%/ü.png")),
            "file:///tmp/a%20b/%231%20100%25/%C3%BC.png"
        );
        assert_eq!(
            file_uri(Path::new(OsStr::from_bytes(b"/x/\xFF?y"))),
            "file:///x/%FF%3Fy"
        );
        assert_eq!(file_uri(Path::new("/a-b_c.d~e/F9")), "file:///a-b_c.d~e/F9");
    }

    /// `SaveFiles`: the chosen folder joined with each name, in the caller's
    /// order, and never a name that would leave the folder.
    #[test]
    fn save_files_names_land_in_the_chosen_folder_in_order() {
        let names: Vec<Vec<u8>> = [
            &b"b.txt"[..],
            b"a c.png",
            b"../evil",
            b"x/y",
            b"",
            b"..",
            b"\xFFraw",
        ]
        .iter()
        .map(|name| name.to_vec())
        .collect();
        assert_eq!(
            save_files_uris(Path::new("/home/b/Out Dir"), &names),
            vec![
                "file:///home/b/Out%20Dir/b.txt",
                "file:///home/b/Out%20Dir/a%20c.png",
                "file:///home/b/Out%20Dir/%FFraw",
            ]
        );
    }

    #[test]
    fn an_open_call_reads_into_a_dialog() {
        let args = call(
            "Upload files",
            vec![
                ("accept_label", "s", s("_Upload")),
                ("modal", "b", Value::Bool(true)),
                ("multiple", "b", Value::Bool(true)),
                (
                    "filters",
                    "a(sa(us))",
                    Value::Array(vec![
                        filter_value("Images", &[(0, "*.png"), (1, "image/*"), (7, "??")]),
                        filter_value("Text", &[(1, "text/plain")]),
                    ]),
                ),
                (
                    "current_filter",
                    "(sa(us))",
                    filter_value("Text", &[(1, "text/plain")]),
                ),
                (
                    "choices",
                    "a(ssa(ss)s)",
                    Value::Array(vec![Value::Struct(vec![
                        s("k"),
                        s("Label"),
                        Value::Array(vec![]),
                        s("true"),
                    ])]),
                ),
                (
                    "current_folder",
                    "ay",
                    Value::bytes(b"/home/brian/Pictures\0"),
                ),
                // Only SaveFile reads these; an open ignores them.
                ("current_name", "s", s("ignored.txt")),
            ],
        );
        let dialog = Dialog::from_args(Kind::Open, &args).unwrap();
        assert_eq!(
            dialog,
            Dialog {
                handle: "/org/freedesktop/portal/desktop/request/1_9/t".into(),
                kind: Kind::Open,
                title: Some("Upload files".into()),
                accept: Some("Upload".into()),
                multiple: true,
                directory: false,
                folder: Some(PathBuf::from("/home/brian/Pictures")),
                name: None,
                file: None,
                filters: vec![
                    filter("Images", &["*.png"], &["image/*"]),
                    filter("Text", &[], &["text/plain"]),
                ],
                current_filter: Some("Text".into()),
                files: Vec::new(),
            }
        );
    }

    #[test]
    fn save_calls_read_their_own_options() {
        let args = call(
            "",
            vec![
                ("current_name", "s", s("untitled.png")),
                ("current_file", "ay", Value::bytes(b"/home/brian/x.png\0")),
                // A relative folder is no folder at all.
                ("current_folder", "ay", Value::bytes(b"relative\0")),
                // The wrong type is ignored, not fatal.
                ("accept_label", "u", Value::U32(3)),
            ],
        );
        let save = Dialog::from_args(Kind::Save, &args).unwrap();
        assert_eq!(save.title, None, "an empty title is no title");
        assert_eq!(save.name.as_deref(), Some("untitled.png"));
        assert_eq!(save.file, Some(PathBuf::from("/home/brian/x.png")));
        assert_eq!(save.folder, None);
        assert_eq!(save.accept, None);

        let args = call(
            "Save all",
            vec![
                ("current_folder", "ay", Value::bytes(b"/tmp\0")),
                (
                    "files",
                    "aay",
                    Value::Array(vec![Value::bytes(b"a.png\0"), Value::bytes(b"b c\0")]),
                ),
            ],
        );
        let save_files = Dialog::from_args(Kind::SaveFiles, &args).unwrap();
        assert_eq!(save_files.files, vec![b"a.png".to_vec(), b"b c".to_vec()]);
        assert_eq!(save_files.folder, Some(PathBuf::from("/tmp")));

        // A current filter with no list to choose from is applied on its own.
        let args = call(
            "",
            vec![(
                "current_filter",
                "(sa(us))",
                filter_value("PDF", &[(0, "*.pdf")]),
            )],
        );
        let open = Dialog::from_args(Kind::Open, &args).unwrap();
        assert_eq!(open.filters, vec![filter("PDF", &["*.pdf"], &[])]);
        assert_eq!(open.current_filter.as_deref(), Some("PDF"));

        // The five arguments are not optional.
        assert!(Dialog::from_args(Kind::Open, &args[..4]).is_err());
        let mut wrong = args.clone();
        wrong[0] = s("not a path");
        assert!(Dialog::from_args(Kind::Open, &wrong).is_err());
    }

    /// What the service writes, the window's `--chooser-request` reads back:
    /// every field, a filter with only MIME types and one with only globs,
    /// and text with quotes, backslashes, `#` and a newline in it.
    #[test]
    fn the_request_file_round_trips_through_the_parser() {
        let dir = scratch("roundtrip");
        let dialog = Dialog {
            kind: Kind::Open,
            title: Some("Pick \"one\" # C:\\ path\nplease".into()),
            accept: Some("Upload".into()),
            multiple: true,
            filters: vec![
                filter("Images", &["*.png", "*.jpg"], &["image/*"]),
                filter("Only MIME", &[], &["text/plain", "application/pdf"]),
                filter("Only globs", &["*.[ch]", "Makefile"], &[]),
            ],
            current_filter: Some("Only globs".into()),
            ..Dialog::default()
        };
        let text = request_toml(&dialog, Some(&dir));
        let request = dir.join("request.toml");
        let (chooser, start) =
            crate::cli::parse_request(&text, &request, PathBuf::from("/tmp/out")).unwrap();
        assert_eq!(chooser.out, PathBuf::from("/tmp/out"));
        assert!(chooser.multiple && !chooser.directory && !chooser.save);
        assert_eq!(chooser.title, dialog.title);
        assert_eq!(chooser.accept.as_deref(), Some("Upload"));
        assert_eq!(chooser.name, None);
        assert_eq!(chooser.filters, dialog.filters);
        assert_eq!(chooser.current_filter, 2);
        assert_eq!(start, Some(dir.clone()));

        // A save over a file that exists opens on that file; the suggested
        // name rides along for `Save as:`.
        let existing = dir.join("x.png");
        std::fs::write(&existing, b"").unwrap();
        let save = Dialog {
            kind: Kind::Save,
            name: Some("untitled.png".into()),
            file: Some(existing.clone()),
            ..Dialog::default()
        };
        let text = request_toml(&save, None);
        let (chooser, start) =
            crate::cli::parse_request(&text, &request, PathBuf::from("/tmp/out")).unwrap();
        assert!(chooser.save);
        assert_eq!(chooser.name.as_deref(), Some("untitled.png"));
        assert_eq!(chooser.current_filter, 0);
        assert_eq!(start, Some(existing));

        // A save over a file that is not there yet starts in its folder.
        let gone = Dialog {
            kind: Kind::Save,
            file: Some(dir.join("new.png")),
            ..Dialog::default()
        };
        let (_, start) = crate::cli::parse_request(
            &request_toml(&gone, None),
            &request,
            PathBuf::from("/tmp/out"),
        )
        .unwrap();
        assert_eq!(start, Some(dir.clone()));

        // SaveFiles is a folder dialog to the window.
        let save_files = Dialog {
            kind: Kind::SaveFiles,
            ..Dialog::default()
        };
        let (chooser, _) = crate::cli::parse_request(
            &request_toml(&save_files, Some(&dir)),
            &request,
            PathBuf::from("/tmp/out"),
        )
        .unwrap();
        assert!(chooser.directory && !chooser.multiple && !chooser.save);

        // A folder whose name is not UTF-8 is written lossily, and still
        // parses.
        let odd = Path::new(OsStr::from_bytes(b"/tmp/\xFFodd"));
        let text = request_toml(&dialog, Some(odd));
        assert!(text.contains("folder = \"/tmp/\u{FFFD}odd\""), "{text}");
        let (_, start) =
            crate::cli::parse_request(&text, &request, PathBuf::from("/tmp/out")).unwrap();
        assert_eq!(start, Some(PathBuf::from("/tmp/\u{FFFD}odd")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a window that has exited answers, by what it wrote and how it
    /// went.
    #[test]
    fn the_answer_follows_what_the_window_wrote() {
        let open = Dialog {
            kind: Kind::Open,
            multiple: true,
            ..Dialog::default()
        };
        let picked = [PathBuf::from("/a b"), PathBuf::from("/c")];
        assert_eq!(
            answer_for(&open, &picked, true),
            Answer {
                response: SUCCESS,
                uris: vec!["file:///a%20b".into(), "file:///c".into()],
            }
        );
        // Picked, even if the window then exited badly: the pick was made.
        assert_eq!(answer_for(&open, &picked, false).response, SUCCESS);
        assert_eq!(answer_for(&open, &[], true), Answer::cancelled());
        assert_eq!(answer_for(&open, &[], false), Answer::other());

        let save = Dialog {
            kind: Kind::Save,
            ..Dialog::default()
        };
        assert_eq!(answer_for(&save, &picked, true).uris, vec!["file:///a%20b"]);

        let save_files = Dialog {
            kind: Kind::SaveFiles,
            files: vec![b"x.png".to_vec(), b"y".to_vec()],
            ..Dialog::default()
        };
        assert_eq!(
            answer_for(&save_files, &[PathBuf::from("/out")], true).uris,
            vec!["file:///out/x.png", "file:///out/y"]
        );
    }

    #[test]
    fn only_absolute_lines_are_picked() {
        let dir = scratch("picked");
        let out = dir.join("out");
        std::fs::write(&out, b"/a\n\nrelative\n/b c\n/d\xFF").unwrap();
        assert_eq!(
            read_picked(&out),
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b c"),
                PathBuf::from(OsStr::from_bytes(b"/d\xFF")),
            ]
        );
        assert!(read_picked(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Where a dialog with no folder starts: the last pick's folder, then
    /// home; and a save over a file leaves it to the file.
    #[test]
    fn the_last_pick_is_where_the_next_dialog_starts() {
        let dir = scratch("lastdir");
        let setup = Setup {
            last_dir_file: Some(dir.join("state").join("portal-last-dir")),
            home: Some(dir.join("home")),
            ..Setup::default()
        };
        let open = Dialog::default();
        assert_eq!(start_folder(&open, &setup), Some(dir.join("home")));

        let pictures = dir.join("Pictures");
        std::fs::create_dir_all(&pictures).unwrap();
        remember(&setup, &open, &[pictures.join("cat.png")]);
        assert_eq!(start_folder(&open, &setup), Some(pictures.clone()));

        // A folder pick remembers the folder itself.
        let folder = Dialog {
            directory: true,
            ..Dialog::default()
        };
        remember(&setup, &folder, std::slice::from_ref(&dir));
        assert_eq!(last_dir(&setup), Some(dir.clone()));

        // A remembered folder that has since gone is forgotten.
        remember(&setup, &open, &[dir.join("gone").join("x")]);
        assert_eq!(start_folder(&open, &setup), Some(dir.join("home")));

        let explicit = Dialog {
            folder: Some(PathBuf::from("/srv")),
            ..Dialog::default()
        };
        assert_eq!(start_folder(&explicit, &setup), Some(PathBuf::from("/srv")));
        let save_over = Dialog {
            kind: Kind::Save,
            file: Some(PathBuf::from("/srv/x")),
            ..Dialog::default()
        };
        assert_eq!(start_folder(&save_over, &setup), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The three ways a window can fail to answer, each checked with a real
    /// process: one that cannot start, one that quits cleanly with nothing
    /// picked, one that fails. And whichever it was, no file is left behind.
    #[test]
    fn a_window_that_picks_nothing_is_a_cancel_or_a_failure() {
        let dir = scratch("windows");
        let runtime = dir.join("run").join("delightfile");
        let setup = |picker: &str| Setup {
            picker: Some(PathBuf::from(picker)),
            runtime_dir: Some(runtime.clone()),
            ..Setup::default()
        };
        let dialog = Dialog {
            handle: "/r/1".into(),
            ..Dialog::default()
        };
        let pending = Mutex::new(Pending::default());
        assert_eq!(
            ask(&dialog, &setup("/nonexistent/picker"), 1, &pending),
            Answer::other()
        );
        let pending = Mutex::new(Pending::default());
        assert_eq!(
            ask(&dialog, &setup("/usr/bin/true"), 2, &pending),
            Answer::cancelled()
        );
        let pending = Mutex::new(Pending::default());
        assert_eq!(
            ask(&dialog, &setup("/usr/bin/false"), 3, &pending),
            Answer::other()
        );
        // Closed before the window could start: it never does.
        let mut closed = Pending::default();
        closed.close();
        let pending = Mutex::new(closed);
        assert_eq!(
            ask(&dialog, &setup("/usr/bin/true"), 4, &pending),
            Answer::other()
        );
        assert!(lock(&pending).child.is_none(), "no window was started");

        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&runtime).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "the request directory is private");
        assert_eq!(
            std::fs::read_dir(&runtime).unwrap().count(),
            0,
            "every request file was cleaned up"
        );

        // With no runtime directory there is nowhere to put the request.
        let nowhere = Setup {
            picker: Some(PathBuf::from("/usr/bin/true")),
            ..Setup::default()
        };
        let pending = Mutex::new(Pending::default());
        assert_eq!(ask(&dialog, &nowhere, 5, &pending), Answer::other());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The request file is 0600, and says what was asked.
    #[test]
    fn the_request_file_is_private_and_readable() {
        let dir = scratch("files");
        let setup = Setup {
            runtime_dir: Some(dir.join("delightfile")),
            home: Some(PathBuf::from("/home/somebody")),
            ..Setup::default()
        };
        let dialog = Dialog {
            title: Some("Hello".into()),
            ..Dialog::default()
        };
        let files = RequestFiles::write(&dialog, &setup, 7).unwrap();
        let name = files
            .request
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(name, format!("portal-{}-7.toml", std::process::id()));
        let mode = std::fs::metadata(&files.request)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let mode = std::fs::metadata(&files.out).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let text = std::fs::read_to_string(&files.request).unwrap();
        assert!(text.contains("kind = \"open\""), "{text}");
        assert!(text.contains("title = \"Hello\""), "{text}");
        assert!(text.contains("folder = \"/home/somebody\""), "{text}");
        let (request, out) = (files.request.clone(), files.out.clone());
        drop(files);
        assert!(!request.exists() && !out.exists(), "dropped, and gone");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
