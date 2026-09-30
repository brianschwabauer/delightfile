//! Reading `vfs.toml` — yazi's file, unchanged, plus delightfile's own on top.
//!
//! PLAN §3 says delightfile reads yazi's `vfs.toml` *directly* rather than
//! asking the user to copy it. That is not laziness about a migration path: the
//! file is the list of machines Brian's `g 1` and `g 2` go to, it already
//! exists, and a file manager that needs its bookmarks retyped before it can
//! reach a server is one that gets closed. So both files are read, in order:
//!
//! ```text
//!   ~/.config/yazi/vfs.toml         first
//!   ~/.config/delightfile/vfs.toml  second, and wins on a name collision
//! ```
//!
//! Second-wins-per-service rather than second-replaces-the-file, so overriding
//! one host's port does not silently drop the other five.
//!
//! ## The shape, from the real file
//!
//! ```toml
//! [services.showandtour1]
//! type = "sftp"
//! host = "showandtour1"
//! user = "brian"
//! port = 22
//! key_file = "~/.ssh/id_ed25519"
//! ```
//!
//! One `[services.<name>]` table per machine; the table's last dotted segment
//! *is* the service name, which is what `sftp://showandtour1/…` addresses.
//! `type = "sftp"` and `type = "rclone"` (below) exist; a table with any other
//! type is skipped with a warning rather than a failure, because a future
//! `type = "s3"` in yazi's file must not stop delightfile reading the sftp
//! entries beside it.
//!
//! For an sftp service `host` is the only required key. Everything else has a
//! default, and — this is the point of the whole design — *every* default is
//! "let `ssh` decide". A `host` of `showandtour1` with no `user`, no `port`
//! and no `key_file` is not underspecified: it is a `Host showandtour1` block
//! in `~/.ssh/config`, which is where the user already put the answer. See
//! [`super`] on why nothing here reimplements ssh.
//!
//! A bad line warns and the rest of the file still loads, per PLAN §3 and per
//! [`crate::toml`]'s whole reason for existing.
//!
//! ## Cloud storage: `type = "rclone"`, and rclone's own file
//!
//! A second kind of service is a remote in the user's `rclone config` — a
//! Google Drive, a Dropbox, an S3 or R2 bucket, anything rclone speaks — reached
//! through `rclone rcd` (see `super::rclone`). Such a service can be written
//! into `vfs.toml`:
//!
//! ```toml
//! [services.photos]
//! type = "rclone"
//! remote = "r2"          # the [section] in rclone.conf; defaults to the name
//! root = "photos-bucket" # optional: start inside the remote
//! ```
//!
//! — but it usually does not have to be, because **rclone's own config file is
//! read too**, and every remote in it becomes a service of the same name
//! ([`VfsConfig::load`]). `rclone config` is where the user already told a
//! program how to reach their cloud, with a token delightfile never sees; a
//! second list to keep in step with it would be the retyped-bookmarks problem
//! this file exists to avoid. `vfs.toml` still wins: a service it defines by
//! name is left exactly as written, and the discovered remotes come after the
//! file's own services, in `rclone.conf`'s order, so adding a remote to rclone
//! never moves `g 1` or `g 2`.
//!
//! An encrypted `rclone.conf` cannot be read without its password, and asking
//! for one is not this parser's job: it says so once, as a warning, and
//! discovers nothing. Services from such a file go in `vfs.toml` by hand, and
//! rclone itself reads the password from `RCLONE_CONFIG_PASS` when it runs.

use std::path::{Path, PathBuf};

use crate::toml::{self, ConfigWarning, Table, Value};

/// The port `ssh` uses when nothing says otherwise.
///
/// Present as a constant so that "no port in the file" and "port 22 in the file"
/// produce the same command line — [`Service::command`] omits `-p` when the port
/// is this, which keeps a `Port` directive in `~/.ssh/config` authoritative
/// instead of being overridden by a default nobody wrote down.
pub const DEFAULT_SSH_PORT: u16 = 22;

/// What a service speaks. An enum rather than a flag, because the parser has
/// to *recognise* the types it does not implement in order to skip them with a
/// useful warning rather than a confusing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceKind {
    /// `ssh -s <host> sftp`, spoken to by `super::conn`.
    Sftp,
    /// A remote in the user's `rclone config`, reached through `rclone rcd`
    /// (`super::rclone`).
    Rclone,
}

impl ServiceKind {
    pub fn parse(text: &str) -> Option<ServiceKind> {
        match text {
            "sftp" | "ssh" => Some(ServiceKind::Sftp),
            "rclone" => Some(ServiceKind::Rclone),
            _ => None,
        }
    }

    /// The URL scheme a place on this kind of service is written with —
    /// `sftp://` or `rclone://` — which is how a path in a pane says which
    /// backend it belongs to.
    pub fn scheme(self) -> &'static str {
        match self {
            ServiceKind::Sftp => super::URL_SCHEME,
            ServiceKind::Rclone => super::RCLONE_URL_SCHEME,
        }
    }
}

/// One remote machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// The `[services.<name>]` segment. What a [`super::VfsPath`] addresses and
    /// what the pane's header shows.
    pub name: String,
    pub kind: ServiceKind,
    pub host: String,
    /// `None` means "whatever `~/.ssh/config` or the local username says".
    pub user: Option<String>,
    pub port: u16,
    /// Written as configured, `~` and all — expanded by [`Service::key_path`]
    /// at the moment it is handed to `ssh`, so that `$HOME` moving under a
    /// long-running process is not a class of bug.
    pub key_file: Option<String>,
    /// A directory to treat as the service's root, when the user does not want
    /// to start at `/`. `None` starts wherever the server puts you, which for
    /// OpenSSH is the login home directory.
    pub root: Option<String>,
    /// Run this program instead of `ssh`, and speak SFTP to its stdio.
    ///
    /// For a [`ServiceKind::Rclone`] service it is the program run instead of
    /// `rclone`, and its arguments are added *after* `rcd`'s own — which is how
    /// the tests point a daemon at a scratch `--config` rather than the user's.
    ///
    /// The seam that makes the whole client testable without a network: OpenSSH's
    /// `sftp-server` binary *is* the other end of the protocol, and spawning it
    /// directly against a temp directory exercises every byte of framing,
    /// pipelining and status handling that a real connection does — minus the
    /// ssh transport, which is the one part of this module that is not
    /// delightfile's code. See `super::tests`.
    ///
    /// It is a real field rather than a test-only one because it is also how a
    /// service could point at a local `sftp-server` or a container's, and
    /// because a test seam that only exists under `cfg(test)` is a seam the
    /// shipping code does not have.
    pub program: Option<(PathBuf, Vec<String>)>,
    /// The rclone remote a [`ServiceKind::Rclone`] service reaches: the
    /// `[section]` name in `rclone.conf`. `None` means "the same as the
    /// service's name". It may also be a whole rclone path — `r2:bucket` — or,
    /// as the tests use it, a local directory, which rclone accepts anywhere
    /// it accepts a remote.
    pub remote: Option<String>,
    /// rclone's `type` for the remote (`drive`, `dropbox`, `s3`…), when it is
    /// known. Informational: the mount card's second line. Nothing is decided
    /// by it — rclone knows what its remotes are.
    pub provider: Option<String>,
    /// Where an rclone service's daemon puts its socket. `None` — every real
    /// service — is `$XDG_RUNTIME_DIR/delightfile`, or a private directory
    /// under the temp dir without one.
    ///
    /// A real field for the reason [`Service::program`] is one: it is how the
    /// tests, in this crate and in df-app, start daemons without creating or
    /// re-permissioning a directory in the user's own runtime directory.
    pub socket_dir: Option<PathBuf>,
}

impl Service {
    /// An sftp service with everything left to `ssh`.
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Service {
        Service {
            name: name.into(),
            kind: ServiceKind::Sftp,
            host: host.into(),
            user: None,
            port: DEFAULT_SSH_PORT,
            key_file: None,
            root: None,
            program: None,
            remote: None,
            provider: None,
            socket_dir: None,
        }
    }

    /// An rclone service called `name` that reaches `remote` — a section of
    /// `rclone.conf`, an rclone path, or a local directory.
    pub fn rclone(name: impl Into<String>, remote: impl Into<String>) -> Service {
        Service {
            kind: ServiceKind::Rclone,
            remote: Some(remote.into()),
            ..Service::new(name, "")
        }
    }

    /// A service that runs `program` directly. See [`Service::program`].
    pub fn direct(
        name: impl Into<String>,
        program: impl Into<PathBuf>,
        args: Vec<String>,
    ) -> Service {
        Service {
            program: Some((program.into(), args)),
            ..Service::new(name, "localhost")
        }
    }

    /// `user@host`, or just `host`.
    pub fn destination(&self) -> String {
        match &self.user {
            Some(user) => format!("{user}@{}", self.host),
            None => self.host.clone(),
        }
    }

    /// The key file with a leading `~` expanded ([`crate::path::expand_home`]),
    /// if there is one — and none when it starts with `~` and there is no
    /// home to put there.
    pub fn key_path(&self) -> Option<PathBuf> {
        let key = self.key_file.as_deref()?;
        if key.starts_with('~') && crate::platform::dirs::home().is_none() {
            return None;
        }
        Some(PathBuf::from(crate::path::expand_home(key)))
    }

    /// The rclone "fs" every call on this service is made against: the remote
    /// and the service's `root` inside it, the way rclone writes one on its
    /// own command line.
    ///
    /// `r2` with no root is `r2:`, the top of the remote, and with `root =
    /// "photos"` it is `r2:photos`. A remote that is already an rclone path
    /// (`r2:bucket`) or a local directory (`/tmp/x`) has the root joined on
    /// with a `/` instead, because a second `:` there would name a different
    /// place — and for a local directory, a directory with a colon in its
    /// name.
    pub fn rclone_fs(&self) -> String {
        let remote = self.remote.as_deref().unwrap_or(&self.name);
        let root = self.root.as_deref().unwrap_or("").trim_matches('/');
        if remote.starts_with('/') || remote.contains(':') {
            if root.is_empty() {
                remote.to_string()
            } else if remote.ends_with(':') || remote.ends_with('/') {
                format!("{remote}{root}")
            } else {
                format!("{remote}/{root}")
            }
        } else {
            format!("{remote}:{root}")
        }
    }

    /// The path a bare `/` means for this service.
    pub fn root_path(&self) -> &str {
        // "." rather than "/" as the fallback: OpenSSH's sftp-server resolves a
        // relative path against the login directory, which is where a person
        // expects to land, and where `sftp host` itself lands.
        self.root.as_deref().unwrap_or(".")
    }

    /// The command that will speak SFTP on its stdio.
    ///
    /// The options, and why each one is here:
    ///
    /// - `-s <dest> sftp` — invoke the `sftp` **subsystem**. This is the whole
    ///   design: `ssh` does the key, the agent, the `known_hosts` check, the
    ///   `~/.ssh/config` `Host` block, the jump host, the `ProxyCommand`. None of
    ///   that is reimplemented here, and none of it can drift from what the
    ///   user's own `ssh host` does, because it *is* what `ssh host` does.
    /// - `-o BatchMode=yes` — never prompt. Without it, a host that wants a
    ///   password makes `ssh` sit on a terminal delightfile does not have, and
    ///   the connection thread waits out its whole timeout for a prompt nobody
    ///   will ever see. With it, `ssh` fails immediately and says why, which the
    ///   user can act on.
    /// - `-o ConnectTimeout=…` — a second, earlier deadline than
    ///   [`super::CONNECT_TIMEOUT`]: ssh's own gives a real error message
    ///   ("Connection timed out"), where ours can only say that nothing arrived.
    /// - `-x` — no X11 forwarding for a file transfer, ever.
    /// - `-p` and `-i` only when the file said so, so `~/.ssh/config` stays in
    ///   charge of what the file left out; `-i ~/…` has its `~` expanded
    ///   ([`Service::key_path`]), since no shell will.
    ///
    /// Either way the child is a console tool run headless
    /// ([`crate::platform::process::quiet`]: no console window on Windows).
    /// `ssh` is found by `std`'s own lookup, which on Windows adds the `.exe`
    /// that [`crate::platform::process::candidates`] would, and finds Windows'
    /// own OpenSSH on `PATH` as it ships.
    pub fn command(&self) -> std::process::Command {
        if let Some((program, args)) = &self.program {
            let mut command = std::process::Command::new(program);
            command.args(args);
            crate::platform::process::quiet(&mut command);
            return command;
        }
        let mut command = std::process::Command::new("ssh");
        crate::platform::process::quiet(&mut command);
        command.arg("-x");
        command.arg("-o").arg("BatchMode=yes");
        command.arg("-o").arg(format!(
            "ConnectTimeout={}",
            super::CONNECT_TIMEOUT.as_secs()
        ));
        if self.port != DEFAULT_SSH_PORT {
            command.arg("-p").arg(self.port.to_string());
        }
        if let Some(key) = self.key_path() {
            command.arg("-i").arg(key);
        }
        command.arg("-s").arg(self.destination()).arg("sftp");
        command
    }
}

/// Every service delightfile knows about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VfsConfig {
    /// In file order, yazi's first: the order the `M`ount-style pickers and the
    /// service list show them in, and the order a person wrote them down.
    pub services: Vec<Service>,
}

impl VfsConfig {
    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.name == name)
    }

    /// Add or replace by name, keeping the original position on a replace.
    ///
    /// Position-preserving because a user who overrides `showandtour1`'s port in
    /// delightfile's file has not asked for it to jump to the bottom of the
    /// list — and the list's order is a keyboard shortcut's order.
    pub fn insert(&mut self, service: Service) {
        match self.services.iter_mut().find(|s| s.name == service.name) {
            Some(existing) => *existing = service,
            None => self.services.push(service),
        }
    }

    /// Parse one `vfs.toml`. Never fails; unreadable lines become warnings.
    pub fn parse(text: &str, file: &Path) -> (VfsConfig, Vec<ConfigWarning>) {
        let doc = toml::parse(text, file);
        let mut config = VfsConfig::default();
        let mut warnings = Vec::new();

        for (name, table) in doc.tables_under("services") {
            // `[services.a.b]` is not a service called `a.b`; it is a typo, and
            // reading it as a machine name would produce a host nobody can
            // reach.
            if name.contains('.') {
                warnings.push(ConfigWarning::new(
                    file,
                    table.line,
                    format!("[services.{name}] has a dotted name; expected one segment"),
                ));
                continue;
            }
            // An rclone service shares nothing with an ssh one but the table
            // it is written in, so it has its own reader rather than a branch
            // through every line of the ssh one.
            if table.get("type").and_then(Value::as_str) == Some("rclone") {
                let (service, notes) = parse_rclone_service(name, table);
                for note in notes {
                    warnings.push(ConfigWarning::new(file, table.line, note));
                }
                config.insert(service);
                continue;
            }
            match parse_service(name, table) {
                Ok(service) => config.insert(service),
                Err(message) => warnings.push(ConfigWarning::new(file, table.line, message)),
            }
        }

        // The parser's own warnings (bad syntax) come before this file's
        // shape warnings, matching the order a reader meets the problems in.
        let mut all = doc.warnings;
        all.append(&mut warnings);
        (config, all)
    }

    /// Read one file, if it is there. **A missing file is silence** (PLAN §3):
    /// most people have no `vfs.toml` and that is not a thing to warn about.
    pub fn load_file(path: &Path) -> (VfsConfig, Vec<ConfigWarning>) {
        match std::fs::read_to_string(path) {
            Ok(text) => VfsConfig::parse(&text, path),
            Err(_) => (VfsConfig::default(), Vec::new()),
        }
    }

    /// Read every path in order, later files overriding earlier ones per
    /// service name.
    pub fn load_files(paths: &[PathBuf]) -> (VfsConfig, Vec<ConfigWarning>) {
        let mut config = VfsConfig::default();
        let mut warnings = Vec::new();
        for path in paths {
            let (next, mut next_warnings) = VfsConfig::load_file(path);
            for service in next.services {
                config.insert(service);
            }
            warnings.append(&mut next_warnings);
        }
        (config, warnings)
    }

    /// Every `vfs.toml` in `paths`, then the remotes of the `rclone.conf` at
    /// `rclone_conf` that none of them defined by name.
    ///
    /// The seam [`VfsConfig::load`] is a thin wrapper round, with both
    /// locations explicit so a test can hand it fixtures and never read the
    /// user's own files.
    pub fn load_with_rclone(
        paths: &[PathBuf],
        rclone_conf: Option<&Path>,
    ) -> (VfsConfig, Vec<ConfigWarning>) {
        let (mut config, mut warnings) = VfsConfig::load_files(paths);
        if let Some(path) = rclone_conf {
            let (remotes, mut notes) = load_rclone_conf(path);
            for service in remotes {
                // `vfs.toml` wins by name, and wins whole: a service written
                // there is exactly what it says, whatever rclone calls the same
                // name. Appended, never inserted, so the file's own services
                // keep their numbers.
                if config.service(&service.name).is_none() {
                    config.services.push(service);
                }
            }
            warnings.append(&mut notes);
        }
        (config, warnings)
    }

    /// The real thing: yazi's file, delightfile's, then rclone's remotes.
    pub fn load() -> (VfsConfig, Vec<ConfigWarning>) {
        VfsConfig::load_with_rclone(&config_paths(), rclone_config_path().as_deref())
    }
}

/// Where [`VfsConfig::load`] looks, in precedence order (last wins).
pub fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(yazi) = crate::platform::dirs::yazi_config_dir() {
        paths.push(yazi.join("vfs.toml"));
    }
    if let Some(dir) = crate::config::config_dir() {
        paths.push(dir.join("vfs.toml"));
    }
    paths
}

/// Where rclone keeps its config, by rclone's own rule: `$RCLONE_CONFIG` when
/// it is set; else `$XDG_CONFIG_HOME/rclone/rclone.conf` (or
/// `~/.config/rclone/rclone.conf`) when that file exists; else the legacy
/// `~/.rclone.conf` when *that* exists; else the XDG path again, where rclone
/// would create one and where finding nothing is silence.
///
/// The same answer `rclone` itself reaches, which matters because the daemon
/// that does the work reads the file this function names — a service
/// discovered from one file and served from another would be a remote that
/// lists and then cannot be reached.
pub fn rclone_config_path() -> Option<PathBuf> {
    rclone_config_path_from(
        std::env::var_os("RCLONE_CONFIG"),
        xdg_config_home(),
        crate::platform::dirs::home(),
    )
}

/// [`rclone_config_path`]'s rule over explicit inputs — `$RCLONE_CONFIG`, the
/// XDG config home, `$HOME` — so it can be tested against a temp directory
/// rather than the environment.
pub(super) fn rclone_config_path_from(
    explicit: Option<std::ffi::OsString>,
    config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit.filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    let xdg = config_home.map(|base| base.join("rclone").join("rclone.conf"));
    let legacy = home.map(|home| home.join(".rclone.conf"));
    if xdg.as_deref().is_some_and(Path::exists) {
        return xdg;
    }
    if legacy.as_deref().is_some_and(Path::exists) {
        return legacy;
    }
    xdg.or(legacy)
}

/// The line an encrypted `rclone.conf` carries instead of its sections.
const RCLONE_ENCRYPTED: &str = "RCLONE_ENCRYPT_V0:";

/// Read the remotes out of one `rclone.conf`. A missing file is silence, like
/// a missing `vfs.toml`.
pub fn load_rclone_conf(path: &Path) -> (Vec<Service>, Vec<ConfigWarning>) {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_rclone_conf(&text, path),
        Err(_) => (Vec::new(), Vec::new()),
    }
}

/// The remotes an `rclone.conf` defines, in file order: one
/// [`ServiceKind::Rclone`] service per `[section]`, named after it, with the
/// section's `type` as its [`Service::provider`].
///
/// The file is INI, and only two of its shapes matter here: `[name]` and
/// `type = …`. Everything else — the tokens, the keys, the endpoints — is
/// rclone's business and is not read, let alone kept. A line that is neither
/// a section, a `key = value` nor a comment is skipped without a warning: this
/// is rclone's file, rclone validates it, and a second opinion on its syntax
/// would only ever be wrong about something rclone accepts.
pub fn parse_rclone_conf(text: &str, file: &Path) -> (Vec<Service>, Vec<ConfigWarning>) {
    let mut services: Vec<Service> = Vec::new();
    // The section the lines are being read into, by index; `None` before the
    // first header and under a repeated one.
    let mut current: Option<usize> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // Encrypted: the first line that is not a comment is the marker, and
        // everything after it is ciphertext.
        if services.is_empty() && line.starts_with(RCLONE_ENCRYPTED) {
            let warning = ConfigWarning::new(
                file,
                index + 1,
                "the rclone config is encrypted, so its remotes cannot be listed — \
                 add each one to vfs.toml as a service with type = \"rclone\"",
            );
            return (Vec::new(), vec![warning]);
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = name.trim();
            // A section written twice is one remote, as it is to rclone, and
            // the first one's `type` is the one kept: the keys under the
            // repeat go nowhere, rather than onto whichever section happens to
            // be last in the list.
            current = if name.is_empty() || services.iter().any(|s| s.name == name) {
                None
            } else {
                services.push(Service::rclone(name, name));
                Some(services.len() - 1)
            };
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "type" {
            if let Some(service) = current.and_then(|at| services.get_mut(at)) {
                let value = value.trim();
                service.provider = (!value.is_empty()).then(|| value.to_string());
            }
        }
    }
    (services, Vec::new())
}

fn xdg_config_home() -> Option<PathBuf> {
    crate::platform::dirs::config_dir()
}

/// A `type = "rclone"` table. Never fails: every key it reads is optional,
/// and the ssh keys it has no use for are named in a note rather than refused,
/// because a service that moved from sftp to rclone and kept its old `user`
/// line should still work.
fn parse_rclone_service(name: &str, table: &Table) -> (Service, Vec<String>) {
    let mut notes = Vec::new();
    let mut text = |key: &str| match table.get(key) {
        Some(value) => match value.as_str() {
            Some(text) => Some(text.to_string()),
            None => {
                notes.push(format!(
                    "[services.{name}] `{key}` should be a string, found {}",
                    value.type_name()
                ));
                None
            }
        },
        None => None,
    };
    let remote = text("remote");
    // `root` and yazi's `path` mean the same thing here as they do for sftp.
    let root = match text("root") {
        Some(root) => Some(root),
        None => text("path"),
    };
    let ignored: Vec<&str> = ["host", "user", "port", "key_file"]
        .into_iter()
        .filter(|key| table.get(key).is_some())
        .collect();
    if !ignored.is_empty() {
        let list = ignored
            .iter()
            .map(|key| format!("`{key}`"))
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(format!(
            "[services.{name}] is an rclone service; {list} {} for ssh and ignored",
            if ignored.len() == 1 { "is" } else { "are" }
        ));
    }
    let mut service = Service::rclone(name, remote.unwrap_or_else(|| name.to_string()));
    service.root = root;
    (service, notes)
}

fn parse_service(name: &str, table: &Table) -> Result<Service, String> {
    let kind = match table.get("type").and_then(Value::as_str) {
        Some(text) => ServiceKind::parse(text).ok_or_else(|| {
            format!("[services.{name}] has type = \"{text}\", which is neither sftp nor rclone")
        })?,
        // No `type` at all: the file only ever describes sftp services, so
        // assuming one is right far more often than refusing is.
        None => ServiceKind::Sftp,
    };

    let host = table
        .get("host")
        .and_then(Value::as_str)
        .map(str::to_string)
        // A service with no host is the one thing there is no sensible default
        // for — `~/.ssh/config` is keyed by host name, so without one there is
        // nothing to look up.
        .ok_or_else(|| format!("[services.{name}] has no `host`"))?;

    let user = match table.get("user") {
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| {
                    format!(
                        "[services.{name}] `user` should be a string, found {}",
                        value.type_name()
                    )
                })?
                .to_string(),
        ),
        None => None,
    };

    let port = match table.get("port") {
        Some(value) => {
            let number = value.as_int().ok_or_else(|| {
                format!(
                    "[services.{name}] `port` should be a number, found {}",
                    value.type_name()
                )
            })?;
            u16::try_from(number)
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| format!("[services.{name}] `port` of {number} is not a port"))?
        }
        None => DEFAULT_SSH_PORT,
    };

    let key_file = optional_string(table, name, "key_file")?;
    // `root` and yazi's `path` mean the same thing; accepting both costs one
    // `or` and saves a support question.
    let root = match optional_string(table, name, "root")? {
        Some(root) => Some(root),
        None => optional_string(table, name, "path")?,
    };

    // `ssh` parses its operands with `getopt`, and there is no `--` that would
    // stop it: a value beginning with `-` becomes a *flag*. `host =
    // "-oProxyCommand=sh -c …"` is a command line, and delightfile reads
    // yazi's `vfs.toml` as well as its own, so this file is not always one the
    // user wrote today. Refused at parse rather than quoted at spawn, because
    // there is no quoting that helps and no legitimate value of this shape.
    for (key, value) in [
        ("host", Some(&host)),
        ("user", user.as_ref()),
        ("key_file", key_file.as_ref()),
    ] {
        if value.is_some_and(|v| v.starts_with('-')) {
            return Err(format!(
                "[services.{name}] `{key}` starts with `-`, which ssh would read as an option"
            ));
        }
    }

    Ok(Service {
        name: name.to_string(),
        kind,
        host,
        user,
        port,
        key_file,
        root,
        program: None,
        remote: None,
        provider: None,
        socket_dir: None,
    })
}

fn optional_string(table: &Table, service: &str, key: &str) -> Result<Option<String>, String> {
    match table.get(key) {
        Some(value) => match value.as_str() {
            Some(text) => Ok(Some(text.to_string())),
            None => Err(format!(
                "[services.{service}] `{key}` should be a string, found {}",
                value.type_name()
            )),
        },
        None => Ok(None),
    }
}
