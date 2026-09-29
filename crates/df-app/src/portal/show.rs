//! `org.freedesktop.FileManager1`: "Show in folder".
//!
//! Chrome's "Show in folder", and every other program's "reveal this file",
//! asks the session bus for `org.freedesktop.FileManager1` and calls
//! `ShowItems` on it with the file's URI. Whoever owns that name answers.
//! Wherever Nautilus is installed, that is Nautilus, and a Nautilus that does
//! not answer holds the caller until its call times out — Chrome waits some
//! 25 seconds before it falls back to `xdg-open`. `--portal` owns the name
//! beside the file chooser's — or waits in line for it while another file
//! manager has it (see [`super::run`]) — and answers each call at once, with
//! windows of its own.
//!
//! ## What each method opens
//!
//! An ordinary delightfile window each, not a picker, started from the same
//! program a picker is ([`Setup::picker`]):
//!
//! - `ShowItems`: the folder each item is in, with the cursor on it
//!   (`delightfile --reveal <item>`). Items that share a folder share a
//!   window, which stands on the first of them the caller named.
//! - `ShowItemProperties`: the same. There is no properties window to open,
//!   and the item's row, with the spot panel a `Tab` away, is where a person
//!   would go for its properties anyway.
//! - `ShowFolders`: each folder itself (`delightfile <folder>`).
//!
//! No more than [`MOST_WINDOWS`] a call: a caller that sends a hundred URIs
//! has made a mistake, and it should not cost a hundred windows.
//!
//! ## Never waiting on a window
//!
//! The call is answered from the reader loop the moment its windows have been
//! started: a caller is not held for as long as a window is open, and neither
//! is the next file dialog. Nothing here touches the filesystem, either — the
//! folders are the URIs' own, taken apart as text, and a window finds out for
//! itself whether its folder is there — so a URI on a network mount that has
//! stopped answering stops that one window, not the service. Each window gets
//! a thread that waits for it to exit, so none is left behind as a zombie.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread::JoinHandle;

use super::request::Setup;
use super::{invalid_args, no_interface, no_property, with_args, INTROSPECTABLE, PEER, PROPERTIES};
use crate::dbus::{Message, Value};

/// The name "Show in folder" asks the session bus for.
pub const BUS_NAME: &str = "org.freedesktop.FileManager1";

/// Where every file manager answers it.
pub const OBJECT_PATH: &str = "/org/freedesktop/FileManager1";

pub const INTERFACE: &str = "org.freedesktop.FileManager1";

/// Every method takes `(uris, startup_id)` and answers with nothing.
const CALL_SIGNATURE: &str = "ass";

/// The most windows one call opens.
const MOST_WINDOWS: usize = 8;

/// Where a window finds the caller's activation token: the Wayland spelling
/// of the `startup_id` every method is given, which is what lets a
/// compositor hand focus to a window the person asked for from another one.
const TOKEN: &str = "XDG_ACTIVATION_TOKEN";

/// Which of the three methods a call was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `ShowItems`: each item's folder, the cursor on it.
    Items,
    /// `ShowFolders`: each folder itself.
    Folders,
    /// `ShowItemProperties`: shown as `ShowItems` shows them.
    ItemProperties,
}

impl Method {
    pub fn of_member(member: &str) -> Option<Method> {
        match member {
            "ShowItems" => Some(Method::Items),
            "ShowFolders" => Some(Method::Folders),
            "ShowItemProperties" => Some(Method::ItemProperties),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Method::Items => "ShowItems",
            Method::Folders => "ShowFolders",
            Method::ItemProperties => "ShowItemProperties",
        }
    }
}

/// A call to one of the three methods: read it, start its windows, and
/// answer.
///
/// Every call whose arguments are `(as, s)` is answered with an empty
/// success: one whose URIs were all skipped too, and one whose windows could
/// not be started. What was skipped, and what failed, is in the log.
pub fn answer(call: &Message, method: Method, setup: &Setup) -> Message {
    let (uris, startup_id) = match read(call) {
        Ok(read) => read,
        Err(e) => return invalid_args(call, &format!("{} {e}", method.name())),
    };
    let windows = windows(method, &uris);
    let opened = open(setup, &windows, &startup_id);
    log::info!(
        "portal: {} opened {} window(s) for {} uri(s)",
        method.name(),
        opened.len(),
        uris.len()
    );
    // The reapers are let go: each sees its window out on its own.
    drop(opened);
    Message::method_return(call)
}

/// A call's `(uris, startup_id)`.
fn read(call: &Message) -> Result<(Vec<String>, String), String> {
    if call.signature.as_deref() != Some(CALL_SIGNATURE) {
        return Err(format!(
            "takes ({CALL_SIGNATURE}), not ({})",
            call.signature.as_deref().unwrap_or("")
        ));
    }
    let args = call.args()?;
    let [Value::Array(items), Value::Str(startup_id)] = args.as_slice() else {
        return Err("expected (uris, startup_id)".into());
    };
    let uris = items
        .iter()
        .map(|item| match item {
            Value::Str(uri) => Ok(uri.clone()),
            _ => Err("expected a list of URIs".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((uris, startup_id.clone()))
}

/// The windows a call opens, each as the arguments after the program's name,
/// in the order the caller named them.
///
/// A URI that is not a local path is skipped with a warning, as is every
/// window past [`MOST_WINDOWS`]. The folders are compared as paths, so
/// `/a/b` and `/a/b/` are one folder.
fn windows(method: Method, uris: &[String]) -> Vec<Vec<OsString>> {
    // (the folder a window is on, the path it is started with)
    let mut windows: Vec<(PathBuf, PathBuf)> = Vec::new();
    for uri in uris {
        let path = match path_from_uri(uri) {
            Ok(path) => path,
            Err(e) => {
                log::warn!("portal: {}: skipping {uri:?}: {e}", method.name());
                continue;
            }
        };
        let folder = match method {
            Method::Folders => path.clone(),
            // `/` is in no folder; it is shown as itself.
            Method::Items | Method::ItemProperties => path
                .parent()
                .map_or_else(|| path.clone(), Path::to_path_buf),
        };
        if !windows.iter().any(|(open, _)| *open == folder) {
            windows.push((folder, path));
        }
    }
    if windows.len() > MOST_WINDOWS {
        log::warn!(
            "portal: {}: opening the first {MOST_WINDOWS} of {} folders; the rest are dropped",
            method.name(),
            windows.len()
        );
        windows.truncate(MOST_WINDOWS);
    }
    windows
        .into_iter()
        .map(|(_, path)| window_args(method, &path))
        .collect()
}

/// One window's command line: `--reveal` for an item, nothing for a folder,
/// and `--` before the path either way, as [`crate::window::spawn_args`] has
/// it.
fn window_args(method: Method, path: &Path) -> Vec<OsString> {
    let mut args = Vec::with_capacity(3);
    if method != Method::Folders {
        args.push(OsString::from("--reveal"));
    }
    args.push(OsString::from("--"));
    args.push(path.as_os_str().to_os_string());
    args
}

/// A `file://` URI's path, byte for byte: what [`super::request::file_uri`]
/// makes, taken back apart.
///
/// Only a local file has a path here — an empty host, or `localhost` — and
/// everything else is refused: another scheme, another machine, a `file:`
/// with no `//`. So is an escape that is not two hex digits, an escaped NUL
/// (no path holds one), and a `#`, which begins a fragment rather than
/// belonging to a name (GLib refuses it too). The path that is left starts
/// at the `/` after the host, so it is absolute.
fn path_from_uri(uri: &str) -> Result<PathBuf, String> {
    const SCHEME: &str = "file://";
    let rest = uri
        .get(..SCHEME.len())
        .filter(|scheme| scheme.eq_ignore_ascii_case(SCHEME))
        .and_then(|_| uri.get(SCHEME.len()..))
        .ok_or("not a file:// URI")?;
    let Some(slash) = rest.find('/') else {
        return Err("no path".into());
    };
    let (host, path) = rest.split_at(slash);
    if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
        return Err(format!("on another machine ({host})"));
    }
    if path.contains('#') {
        return Err("a fragment is not part of a path".into());
    }
    let bytes = percent_decode(path)?;
    if bytes.contains(&0) {
        return Err("a NUL is not part of a path".into());
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

/// `%XX` escapes decoded to the bytes they stand for, and everything else
/// kept as it is.
fn percent_decode(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    let digit = |at: usize| bytes.get(at).and_then(|b| char::from(*b).to_digit(16));
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&b) = bytes.get(at) {
        if b != b'%' {
            out.push(b);
            at += 1;
            continue;
        }
        let (Some(high), Some(low)) = (digit(at + 1), digit(at + 2)) else {
            return Err(format!("a broken %-escape at byte {at}"));
        };
        // Two hex digits are at most 0xFF.
        out.push((high * 16 + low) as u8);
        at += 3;
    }
    Ok(out)
}

/// A window that was started, by its pid, and the thread seeing it out —
/// which hands back what the window exited with once it has, or `None` when
/// it could not be waited on.
type Opened = (u32, JoinHandle<Option<ExitStatus>>);

/// Start each window, with the caller's activation token when it sent one.
///
/// A window that cannot be started is a warning, and the rest are started
/// all the same.
fn open(setup: &Setup, windows: &[Vec<OsString>], startup_id: &str) -> Vec<Opened> {
    if windows.is_empty() {
        return Vec::new();
    }
    let Some(exe) = &setup.picker else {
        log::warn!("portal: no program to open a window with");
        return Vec::new();
    };
    let mut opened = Vec::with_capacity(windows.len());
    for args in windows {
        let mut command = Command::new(exe);
        command.args(args).stdin(Stdio::null());
        // A token is good for one activation, so one this service was
        // started with is nobody's to pass on.
        if startup_id.is_empty() {
            command.env_remove(TOKEN);
        } else {
            command.env(TOKEN, startup_id);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                log::warn!("portal: could not start {}: {e}", exe.display());
                continue;
            }
        };
        let pid = child.id();
        let reaper = std::thread::Builder::new()
            .name("portal-window".into())
            .spawn(move || match child.wait() {
                Ok(status) => Some(status),
                Err(e) => {
                    log::warn!("portal: lost track of window {pid}: {e}");
                    None
                }
            });
        match reaper {
            Ok(reaper) => opened.push((pid, reaper)),
            // The window is up; it only lingers in the process table once
            // it closes, until the service ends.
            Err(e) => log::warn!("portal: nothing to wait on window {pid} with: {e}"),
        }
    }
    opened
}

/// `org.freedesktop.DBus.Properties` on the FileManager1 object, which has
/// no properties of its own. `None` for a member that interface does not
/// have.
pub fn properties(call: &Message, member: &str) -> Option<Message> {
    let args = match call.args() {
        Ok(args) => args,
        Err(e) => return Some(invalid_args(call, &e)),
    };
    let reply = match (member, call.signature.as_deref(), args.as_slice()) {
        ("Get", Some("ss"), [Value::Str(interface), Value::Str(name)])
        | ("Set", Some("ssv"), [Value::Str(interface), Value::Str(name), _]) => {
            match interface.as_str() {
                INTERFACE => no_property(call, name),
                other => no_interface(call, other),
            }
        }
        ("GetAll", Some("s"), [Value::Str(interface)]) => match interface.as_str() {
            INTERFACE | PROPERTIES | INTROSPECTABLE | PEER => {
                with_args(call, "a{sv}", &[Value::Dict(Vec::new())])
            }
            other => no_interface(call, other),
        },
        ("Get" | "GetAll" | "Set", ..) => invalid_args(
            call,
            &format!(
                "Properties.{member} does not take ({})",
                call.signature.as_deref().unwrap_or("")
            ),
        ),
        _ => return None,
    };
    Some(reply)
}

/// `/org/freedesktop/FileManager1`: the interface as freedesktop's
/// file-manager D-Bus interface describes it, plus the standard ones.
pub const INTERFACES: &str = r#"  <interface name="org.freedesktop.FileManager1">
    <method name="ShowFolders">
      <arg type="as" name="URIs" direction="in"/>
      <arg type="s" name="StartupId" direction="in"/>
    </method>
    <method name="ShowItems">
      <arg type="as" name="URIs" direction="in"/>
      <arg type="s" name="StartupId" direction="in"/>
    </method>
    <method name="ShowItemProperties">
      <arg type="as" name="URIs" direction="in"/>
      <arg type="s" name="StartupId" direction="in"/>
    </method>
  </interface>
  <interface name="org.freedesktop.DBus.Properties">
    <method name="Get">
      <arg type="s" name="interface_name" direction="in"/>
      <arg type="s" name="property_name" direction="in"/>
      <arg type="v" name="value" direction="out"/>
    </method>
    <method name="GetAll">
      <arg type="s" name="interface_name" direction="in"/>
      <arg type="a{sv}" name="props" direction="out"/>
    </method>
    <method name="Set">
      <arg type="s" name="interface_name" direction="in"/>
      <arg type="s" name="property_name" direction="in"/>
      <arg type="v" name="value" direction="in"/>
    </method>
  </interface>
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect">
      <arg type="s" name="xml_data" direction="out"/>
    </method>
  </interface>
  <interface name="org.freedesktop.DBus.Peer">
    <method name="Ping"/>
    <method name="GetMachineId">
      <arg type="s" name="machine_uuid" direction="out"/>
    </method>
  </interface>
"#;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

    use super::*;
    use crate::dbus::{MSG_ERROR, MSG_METHOD_RETURN};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::time::{Duration, Instant};

    fn uris(items: &[&str]) -> Vec<String> {
        items.iter().map(|uri| uri.to_string()).collect()
    }

    fn os(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    /// A call to `method` with `(uris, startup_id)` as its body.
    fn call(method: Method, items: &[&str], startup_id: &str) -> Message {
        Message {
            serial: 12,
            sender: Some(":1.40".into()),
            ..Message::method_call(BUS_NAME, OBJECT_PATH, INTERFACE, method.name())
        }
        .with_args(
            "ass",
            &[
                Value::Array(
                    items
                        .iter()
                        .map(|uri| Value::Str(uri.to_string()))
                        .collect(),
                ),
                Value::Str(startup_id.into()),
            ],
        )
        .unwrap()
    }

    /// A scratch directory of this test's own.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("df-show-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in for delightfile in `dir`: it writes its arguments and its
    /// activation token, a line each, to `ran.<its pid>` beside itself, and
    /// exits — once `hold` beside it is gone (or ten seconds on, so a failed
    /// test leaves nothing running).
    fn stub(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let stub = dir.join("stub");
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             here=$(dirname \"$0\")\n\
             n=0\n\
             while [ -e \"$here/hold\" ] && [ $n -lt 400 ]; do sleep 0.025; n=$((n + 1)); done\n\
             { for arg in \"$@\"; do printf '%s\\n' \"$arg\"; done\n\
               printf 'token=%s\\n' \"${XDG_ACTIVATION_TOKEN-(unset)}\"; } >\"$here/ran.$$.part\"\n\
             mv \"$here/ran.$$.part\" \"$here/ran.$$\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        stub
    }

    /// Everything the stub has recorded so far, sorted.
    fn records(dir: &Path) -> Vec<String> {
        let mut records: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("ran.") && !name.ends_with(".part")
            })
            .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
            .collect();
        records.sort();
        records
    }

    /// Spaces and `%`-escapes, bytes that are not UTF-8, `localhost`, and
    /// every way a URI can fail to be a local path.
    #[test]
    fn a_file_uri_becomes_its_path_byte_for_byte() {
        let path = |uri: &str| path_from_uri(uri).unwrap();
        assert_eq!(
            path("file:///home/brian/Downloads/a%20b%20(1).pdf"),
            PathBuf::from("/home/brian/Downloads/a b (1).pdf")
        );
        assert_eq!(
            path("file:///tmp/100%25%20%23done/%C3%BC.png"),
            PathBuf::from("/tmp/100% #done/ü.png")
        );
        assert_eq!(
            path("file:///tmp/%ff%FEraw"),
            PathBuf::from(OsStr::from_bytes(b"/tmp/\xFF\xFEraw"))
        );
        // Characters a careless encoder left bare are taken as they are.
        assert_eq!(path("file:///tmp/a b+c?d"), PathBuf::from("/tmp/a b+c?d"));
        assert_eq!(
            path("file://localhost/etc/hosts"),
            PathBuf::from("/etc/hosts")
        );
        assert_eq!(
            path("FILE://LocalHost/etc/hosts"),
            PathBuf::from("/etc/hosts")
        );
        assert_eq!(path("file:///"), PathBuf::from("/"));

        // What `file_uri` writes, this reads back.
        for odd in [&b"/tmp/a b/#1 100%/\xC3\xBC.png"[..], b"/x/\xFF?y", b"/"] {
            let odd = Path::new(OsStr::from_bytes(odd));
            assert_eq!(path(&super::super::request::file_uri(odd)), odd);
        }

        for refused in [
            "",
            "/tmp/x",
            "tmp/x",
            "file:/tmp/x",
            "file:tmp/x",
            "file://",
            "file://localhost",
            "file://example.com/tmp/x",
            "file://192.168.1.2/tmp/x",
            "http://example.com/tmp/x",
            "smb://nas/share/x",
            "trash:///x",
            "file:///tmp/a%2",
            "file:///tmp/a%zz",
            "file:///tmp/a%+1",
            "file:///tmp/a%00b",
            "file:///tmp/a#fragment",
        ] {
            assert!(path_from_uri(refused).is_err(), "{refused:?} was taken");
        }
    }

    /// Items that share a folder share a window, on the first of them named;
    /// URIs that are not paths are skipped; and the folders keep the caller's
    /// order.
    #[test]
    fn items_are_grouped_by_the_folder_they_are_in() {
        let items = uris(&[
            "file:///home/b/Downloads/one.pdf",
            "file:///home/b/Pictures/cat.png",
            "file:///home/b/Downloads/two.pdf",
            "https://example.com/x",
            "file:///home/b/Pictures",
            // In /home/b, like the folder above, so no window of its own.
            "file:///home/b/Downloads/",
            "file:///",
        ]);
        let expected = vec![
            os(&["--reveal", "--", "/home/b/Downloads/one.pdf"]),
            os(&["--reveal", "--", "/home/b/Pictures/cat.png"]),
            os(&["--reveal", "--", "/home/b/Pictures"]),
            os(&["--reveal", "--", "/"]),
        ];
        assert_eq!(windows(Method::Items, &items), expected);
        // There is no properties window: an item's properties are shown as
        // the item is.
        assert_eq!(windows(Method::ItemProperties, &items), expected);

        // Folders open themselves, once each however they are spelled.
        let folders = uris(&[
            "file:///srv/a%20b",
            "file:///srv/c/",
            "ftp://example.com/",
            "file:///srv/a%20b/",
            "file://localhost/srv/c",
            "file:///srv/d",
        ]);
        assert_eq!(
            windows(Method::Folders, &folders),
            vec![
                os(&["--", "/srv/a b"]),
                os(&["--", "/srv/c/"]),
                os(&["--", "/srv/d"]),
            ]
        );

        assert!(windows(Method::Items, &[]).is_empty());
        assert!(windows(Method::Items, &uris(&["nonsense"])).is_empty());
    }

    /// Eight windows at most, the first eight folders named; items beyond
    /// that in a folder already open cost nothing.
    #[test]
    fn a_call_opens_at_most_eight_windows() {
        let mut items: Vec<String> = (0..12).map(|n| format!("file:///f{n}/item")).collect();
        items.insert(3, "file:///f0/another".into());
        let opened = windows(Method::Items, &items);
        assert_eq!(opened.len(), MOST_WINDOWS);
        let expected: Vec<Vec<OsString>> = (0..8)
            .map(|n| vec!["--reveal".into(), "--".into(), format!("/f{n}/item").into()])
            .collect();
        assert_eq!(opened, expected);

        let folders: Vec<String> = (0..20).map(|n| format!("file:///f{n}")).collect();
        assert_eq!(windows(Method::Folders, &folders).len(), MOST_WINDOWS);
    }

    /// The command line each window gets means what it should to the
    /// window's own parser: a revealed item, or a folder opened.
    #[test]
    fn each_window_parses_as_what_it_was_asked_for() {
        use crate::cli::{parse, Args, Outcome};
        let parsed = |args: Vec<OsString>| {
            parse(
                args.into_iter()
                    .map(|arg| arg.into_string().unwrap())
                    .collect::<Vec<_>>(),
            )
        };
        let [item] = windows(Method::Items, &uris(&["file:///home/b/--odd%20name"]))
            .try_into()
            .unwrap();
        assert_eq!(
            parsed(item),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/home/b/--odd name")),
                reveal: true,
                ..Args::default()
            })
        );
        let [folder] = windows(Method::Folders, &uris(&["file:///home/b/Music"]))
            .try_into()
            .unwrap();
        assert_eq!(
            parsed(folder),
            Outcome::Run(Args {
                start: Some(PathBuf::from("/home/b/Music")),
                ..Args::default()
            })
        );
    }

    /// A body that is not `(as, s)` is refused as `InvalidArgs`; a good one
    /// is answered with an empty success, even when nothing in it could be
    /// shown or there is no program to show it with.
    #[test]
    fn the_answer_is_empty_or_invalid_args() {
        let nowhere = Setup::default();
        for method in [Method::Items, Method::Folders, Method::ItemProperties] {
            let good = call(method, &["file:///tmp", "gopher://x"], "");
            let reply = answer(&good, method, &nowhere);
            assert_eq!(reply.kind, MSG_METHOD_RETURN);
            assert_eq!(reply.reply_serial, Some(12));
            assert_eq!(reply.destination.as_deref(), Some(":1.40"));
            assert_eq!(reply.signature, None);
            assert!(reply.body.is_empty());

            let all_skipped = call(method, &["http://example.com/"], "");
            assert_eq!(
                answer(&all_skipped, method, &nowhere).kind,
                MSG_METHOD_RETURN
            );
        }

        let refused = |msg: Message| {
            let reply = answer(&msg, Method::Items, &nowhere);
            assert_eq!(reply.kind, MSG_ERROR);
            assert_eq!(
                reply.error_name.as_deref(),
                Some(super::super::INVALID_ARGS)
            );
        };
        let base = || Message {
            serial: 3,
            ..Message::method_call(BUS_NAME, OBJECT_PATH, INTERFACE, "ShowItems")
        };
        // No startup id.
        refused(
            base()
                .with_args("as", &[Value::Array(vec![Value::Str("file:///".into())])])
                .unwrap(),
        );
        // The two arguments the wrong way round.
        refused(
            base()
                .with_args(
                    "sas",
                    &[Value::Str(String::new()), Value::Array(Vec::new())],
                )
                .unwrap(),
        );
        // No body at all.
        refused(base());
        // A body that lies about its signature.
        let mut lying = base().with_args("s", &[Value::Str("x".into())]).unwrap();
        lying.signature = Some("ass".into());
        refused(lying);
    }

    /// A window is a real process, started with its folder's command line
    /// and the caller's token, and waited for once it exits, so nothing is
    /// left in the process table.
    #[test]
    fn each_window_is_started_with_its_token_and_reaped() {
        let dir = scratch("reaped");
        let setup = Setup {
            picker: Some(stub(&dir)),
            ..Setup::default()
        };
        let items = windows(
            Method::Items,
            &uris(&[
                "file:///home/b/Downloads/report%20final.pdf",
                "file:///home/b/Pictures/cat.png",
                "file:///home/b/Downloads/other.pdf",
            ]),
        );
        let opened = open(&setup, &items, "wayland-token-7");
        assert_eq!(opened.len(), 2);
        for (pid, reaper) in opened {
            let status = reaper.join().unwrap().expect("the window was waited on");
            assert!(status.success(), "{status}");
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "window {pid} was reaped"
            );
            let record = std::fs::read_to_string(dir.join(format!("ran.{pid}"))).unwrap();
            assert!(record.ends_with("token=wayland-token-7\n"), "{record}");
        }
        assert_eq!(
            records(&dir),
            vec![
                "--reveal\n--\n/home/b/Downloads/report final.pdf\ntoken=wayland-token-7\n",
                "--reveal\n--\n/home/b/Pictures/cat.png\ntoken=wayland-token-7\n",
            ]
        );

        // With no startup id there is no token, whatever this process had.
        let tokenless = scratch("tokenless");
        let setup = Setup {
            picker: Some(stub(&tokenless)),
            ..Setup::default()
        };
        let folders = windows(Method::Folders, &uris(&["file:///srv"]));
        for (_, reaper) in open(&setup, &folders, "") {
            assert!(reaper
                .join()
                .unwrap()
                .is_some_and(|status| status.success()));
        }
        assert_eq!(records(&tokenless), vec!["--\n/srv\ntoken=(unset)\n"]);

        // A program that is not there starts nothing, and says so.
        let missing = Setup {
            picker: Some(dir.join("not-there")),
            ..Setup::default()
        };
        assert!(open(&missing, &folders, "").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&tokenless);
    }

    /// The whole call, as the service answers it: the reply is sent while
    /// the windows are still up, and each of them was started.
    #[test]
    fn a_call_is_answered_without_waiting_for_its_windows() {
        let dir = scratch("answered");
        let setup = Setup {
            picker: Some(stub(&dir)),
            ..Setup::default()
        };
        // The windows stay up until this goes.
        let hold = dir.join("hold");
        std::fs::write(&hold, b"").unwrap();
        let msg = call(
            Method::Items,
            &[
                "file:///home/b/a.txt",
                "file:///home/b/b.txt",
                "file:///srv/c.txt",
            ],
            "t0k3n",
        );
        let reply = answer(&msg, Method::Items, &setup);
        assert_eq!(reply.kind, MSG_METHOD_RETURN);
        assert!(reply.body.is_empty());
        assert!(
            records(&dir).is_empty(),
            "answered while every window was still up"
        );
        std::fs::remove_file(&hold).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while records(&dir).len() < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            records(&dir),
            vec![
                "--reveal\n--\n/home/b/a.txt\ntoken=t0k3n\n",
                "--reveal\n--\n/srv/c.txt\ntoken=t0k3n\n",
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No properties of its own, the standard interfaces present and empty,
    /// and the wrong arguments refused.
    #[test]
    fn the_object_has_no_properties() {
        let get = Message {
            serial: 1,
            ..Message::method_call(BUS_NAME, OBJECT_PATH, PROPERTIES, "Get")
        }
        .with_args(
            "ss",
            &[Value::Str(INTERFACE.into()), Value::Str("version".into())],
        )
        .unwrap();
        assert_eq!(
            properties(&get, "Get").unwrap().error_name.as_deref(),
            Some(super::super::UNKNOWN_PROPERTY)
        );
        for interface in [INTERFACE, PEER] {
            let get_all = Message {
                serial: 2,
                ..Message::method_call(BUS_NAME, OBJECT_PATH, PROPERTIES, "GetAll")
            }
            .with_args("s", &[Value::Str(interface.into())])
            .unwrap();
            assert_eq!(
                properties(&get_all, "GetAll").unwrap().args().unwrap(),
                vec![Value::Dict(vec![])]
            );
        }
        let elsewhere = Message {
            serial: 3,
            ..Message::method_call(BUS_NAME, OBJECT_PATH, PROPERTIES, "GetAll")
        }
        .with_args(
            "s",
            &[Value::Str("org.freedesktop.impl.portal.FileChooser".into())],
        )
        .unwrap();
        assert_eq!(
            properties(&elsewhere, "GetAll")
                .unwrap()
                .error_name
                .as_deref(),
            Some(super::super::UNKNOWN_INTERFACE)
        );
        let bad = Message {
            serial: 4,
            ..Message::method_call(BUS_NAME, OBJECT_PATH, PROPERTIES, "Get")
        }
        .with_args("s", &[Value::Str(INTERFACE.into())])
        .unwrap();
        assert_eq!(
            properties(&bad, "Get").unwrap().error_name.as_deref(),
            Some(super::super::INVALID_ARGS)
        );
        assert!(properties(&bad, "Frobnicate").is_none());
    }
}
