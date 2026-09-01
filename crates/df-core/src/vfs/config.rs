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
//! *is* the service name, which is what `sftp://showandtour1/…` addresses. Only
//! `type = "sftp"` exists; a table with any other type is skipped with a warning
//! rather than a failure, because a future `type = "s3"` in yazi's file must not
//! stop delightfile reading the sftp entries beside it.
//!
//! `host` is the only required key. Everything else has a default, and — this is
//! the point of the whole design — *every* default is "let `ssh` decide". A
//! `host` of `showandtour1` with no `user`, no `port` and no `key_file` is not
//! underspecified: it is a `Host showandtour1` block in `~/.ssh/config`, which
//! is where the user already put the answer. See [`super`] on why nothing here
//! reimplements ssh.
//!
//! A bad line warns and the rest of the file still loads, per PLAN §3 and per
//! [`crate::toml`]'s whole reason for existing.

use std::path::{Path, PathBuf};

use crate::toml::{self, ConfigWarning, Table, Value};

/// The port `ssh` uses when nothing says otherwise.
///
/// Present as a constant so that "no port in the file" and "port 22 in the file"
/// produce the same command line — [`Service::command`] omits `-p` when the port
/// is this, which keeps a `Port` directive in `~/.ssh/config` authoritative
/// instead of being overridden by a default nobody wrote down.
pub const DEFAULT_SSH_PORT: u16 = 22;

/// What a service speaks. One variant today; an enum anyway, because the parser
/// has to *recognise* the types it does not implement in order to skip them
/// with a useful warning rather than a confusing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Sftp,
}

impl ServiceKind {
    pub fn parse(text: &str) -> Option<ServiceKind> {
        match text {
            "sftp" | "ssh" => Some(ServiceKind::Sftp),
            _ => None,
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
    /// The seam that makes the whole client testable without a network: OpenSSH's
    /// `sftp-server` binary *is* the other end of the protocol, and spawning it
    /// directly against a temp directory exercises every byte of framing,
    /// pipelining and status handling that a real connection does — minus the
    /// ssh transport, which is the one part of this module that is not
    /// delightfile's code. See [`super::tests`].
    ///
    /// It is a real field rather than a test-only one because it is also how a
    /// service could point at a local `sftp-server` or a container's, and
    /// because a test seam that only exists under `cfg(test)` is a seam the
    /// shipping code does not have.
    pub program: Option<(PathBuf, Vec<String>)>,
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

    /// The key file with a leading `~` expanded, if there is one.
    pub fn key_path(&self) -> Option<PathBuf> {
        let key = self.key_file.as_deref()?;
        let Some(rest) = key.strip_prefix('~') else {
            return Some(PathBuf::from(key));
        };
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(format!("{}{rest}", home.to_string_lossy())))
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
    ///   charge of what the file left out.
    pub fn command(&self) -> std::process::Command {
        if let Some((program, args)) = &self.program {
            let mut command = std::process::Command::new(program);
            command.args(args);
            return command;
        }
        let mut command = std::process::Command::new("ssh");
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

    /// The real thing: yazi's file then delightfile's.
    pub fn load() -> (VfsConfig, Vec<ConfigWarning>) {
        VfsConfig::load_files(&config_paths())
    }
}

/// Where [`VfsConfig::load`] looks, in precedence order (last wins).
pub fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(base) = xdg_config_home() {
        paths.push(base.join("yazi").join("vfs.toml"));
    }
    if let Some(dir) = crate::config::config_dir() {
        paths.push(dir.join("vfs.toml"));
    }
    paths
}

fn xdg_config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

fn parse_service(name: &str, table: &Table) -> Result<Service, String> {
    let kind = match table.get("type").and_then(Value::as_str) {
        Some(text) => ServiceKind::parse(text)
            .ok_or_else(|| format!("[services.{name}] has type = \"{text}\", which is not sftp"))?,
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

    Ok(Service {
        name: name.to_string(),
        kind,
        host,
        user,
        port,
        key_file,
        root,
        program: None,
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
