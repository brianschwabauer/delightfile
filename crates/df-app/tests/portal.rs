//! `delightfile --portal` on a private session bus, called the way
//! xdg-desktop-portal calls it — through `busctl`, so the client side is
//! somebody else's D-Bus implementation, not this crate's.
//!
//! The picker window is a shell stub (`DELIGHTFILE_PICKER_EXE`): it keeps the
//! request file it was handed, then either writes two paths and exits, or —
//! for the dialog titled "Slow" — records its pid and sleeps until `Close`
//! kills it.
//!
//! Skipped, with a line saying so, on a machine without `dbus-daemon` or
//! `busctl`. Linux only: `--portal` exists nowhere else.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "org.freedesktop.impl.portal.desktop.delightfile";
const OBJECT: &str = "/org/freedesktop/portal/desktop";
const FILE_CHOOSER: &str = "org.freedesktop.impl.portal.FileChooser";

/// A child process killed when the test lets go of it, pass or fail.
struct Reaped(Option<Child>);

impl Reaped {
    fn new(child: Child) -> Reaped {
        Reaped(Some(child))
    }

    /// The child back, to be waited on for its output.
    fn into_inner(mut self) -> Child {
        self.0.take().unwrap()
    }
}

impl Drop for Reaped {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn have(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// A bus of the test's own, listening on `listen`. Its only service
/// directory, if any, is `services` — never the session's, so nothing a real
/// install put there (a real delightfile, which would open a window) can be
/// started on it. `env` is the environment an activated service inherits.
fn private_bus(
    dir: &Path,
    listen: &str,
    services: Option<&Path>,
    env: &[(&str, &Path)],
) -> (Reaped, String) {
    let config = dir.join("bus.conf");
    let servicedir = services
        .map(|services| format!("<servicedir>{}</servicedir>", services.display()))
        .unwrap_or_default();
    std::fs::write(
        &config,
        format!(
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>{listen}</listen>
  <auth>EXTERNAL</auth>
  {servicedir}
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#
        ),
    )
    .unwrap();
    let mut daemon = Command::new("dbus-daemon");
    daemon
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--print-address"])
        .stdout(Stdio::piped());
    for (key, value) in env {
        daemon.env(key, value);
    }
    let mut daemon = daemon.spawn().unwrap();
    let mut address = String::new();
    BufReader::new(daemon.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    (Reaped::new(daemon), address.trim().to_string())
}

fn busctl(address: &str, args: &[&str]) -> Output {
    Command::new("busctl")
        .arg(format!("--address={address}"))
        .arg("--timeout=20")
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "busctl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Wait for `ready`, for at most ten seconds.
fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "gave up waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The stand-in picker window.
fn write_stub(dir: &Path, picked: &Path) -> PathBuf {
    let stub = dir.join("stub.sh");
    let script = format!(
        r#"#!/bin/sh
for arg; do
    case "$arg" in
        --chooser-file=*) out="${{arg#--chooser-file=}}" ;;
        --chooser-request=*) request="${{arg#--chooser-request=}}" ;;
    esac
done
cp "$request" "{dir}/last-request.toml"
if grep -q '^title = "Slow"' "$request"; then
    echo $$ > "{dir}/slow.pid"
    exec sleep 30
fi
printf '%s\n%s\n' "{picked}/a file.txt" "{picked}/b#1 ü.png" > "$out"
"#,
        dir = dir.display(),
        picked = picked.display()
    );
    std::fs::write(&stub, script).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    stub
}

#[test]
fn the_portal_backend_answers_on_a_real_bus() {
    if !have("dbus-daemon") || !have("busctl") {
        eprintln!("skipping: dbus-daemon or busctl is not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("df-portal-bus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["run", "state", "home", "picked"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let picked = dir.join("picked");
    let stub = write_stub(&dir, &picked);
    let listen = format!("unix:path={}", dir.join("bus").display());
    let (_bus, address) = private_bus(&dir, &listen, None, &[]);

    let _service = Reaped::new(
        Command::new(env!("CARGO_BIN_EXE_delightfile"))
            .arg("--portal")
            .env("DBUS_SESSION_BUS_ADDRESS", &address)
            .env_remove("DBUS_STARTER_ADDRESS")
            .env("DELIGHTFILE_PICKER_EXE", &stub)
            .env("XDG_RUNTIME_DIR", dir.join("run"))
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("HOME", dir.join("home"))
            .spawn()
            .unwrap(),
    );
    wait_for("the service to own its name", || {
        let output = busctl(
            &address,
            &[
                "call",
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameHasOwner",
                "s",
                NAME,
            ],
        );
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("true")
    });

    // ── OpenFile: two paths picked, two URIs back ─────────────────────────
    let reply = stdout(&busctl(
        &address,
        &[
            "call",
            NAME,
            OBJECT,
            FILE_CHOOSER,
            "OpenFile",
            "osssa{sv}",
            "/org/freedesktop/portal/desktop/request/1_1/t1",
            "org.example.Test",
            "",
            "Open Things",
            "2",
            "multiple",
            "b",
            "true",
            "accept_label",
            "s",
            "_Upload",
        ],
    ));
    let base = format!("file://{}", picked.display());
    assert_eq!(
        reply,
        format!(r#"ua{{sv}} 0 1 "uris" as 2 "{base}/a%20file.txt" "{base}/b%231%20%C3%BC.png""#)
    );
    let request = std::fs::read_to_string(dir.join("last-request.toml")).unwrap();
    for line in [
        "kind = \"open\"",
        "title = \"Open Things\"",
        "accept = \"Upload\"",
        "multiple = true",
        // No current_folder and nothing picked before: home.
        &format!("folder = \"{}\"", dir.join("home").display()),
    ] {
        assert!(request.contains(line), "{line} in:\n{request}");
    }
    // The request and answer files are gone once the answer is out.
    assert_eq!(
        std::fs::read_dir(dir.join("run").join("delightfile"))
            .unwrap()
            .count(),
        0
    );
    // …and where the pick came from is remembered.
    assert_eq!(
        std::fs::read_to_string(
            dir.join("state")
                .join("delightfile")
                .join("portal-last-dir")
        )
        .unwrap(),
        format!("{}\n", picked.display())
    );

    // ── Close: the window is killed and the call answers 2 ────────────────
    let slow = Reaped::new(
        Command::new("busctl")
            .arg(format!("--address={address}"))
            .args([
                "--timeout=20",
                "call",
                NAME,
                OBJECT,
                FILE_CHOOSER,
                "OpenFile",
                "osssa{sv}",
                "/org/freedesktop/portal/desktop/request/1_1/t2",
                "org.example.Test",
                "",
                "Slow",
                "0",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let pid_file = dir.join("slow.pid");
    wait_for("the slow window to start", || {
        std::fs::read_to_string(&pid_file).is_ok_and(|pid| pid.ends_with('\n'))
    });
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_string();
    // The last pick's folder is where a dialog with none starts.
    let request = std::fs::read_to_string(dir.join("last-request.toml")).unwrap();
    assert!(
        request.contains(&format!("folder = \"{}\"", picked.display())),
        "{request}"
    );
    // Introspection sees the open request.
    let xml = stdout(&busctl(
        &address,
        &[
            "call",
            NAME,
            "/org/freedesktop/portal/desktop/request/1_1/t2",
            "org.freedesktop.DBus.Introspectable",
            "Introspect",
        ],
    ));
    assert!(xml.contains("org.freedesktop.impl.portal.Request"), "{xml}");

    let started = Instant::now();
    stdout(&busctl(
        &address,
        &[
            "call",
            NAME,
            "/org/freedesktop/portal/desktop/request/1_1/t2",
            "org.freedesktop.impl.portal.Request",
            "Close",
        ],
    ));
    let output = slow.into_inner().wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ua{sv} 2 0");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "Close answered long before the window's 30 s sleep"
    );
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the window ({pid}) was killed and reaped"
    );

    // ── SaveFiles: the names, inside the (stub's) chosen folder ───────────
    let reply = stdout(&busctl(
        &address,
        &[
            "call",
            NAME,
            OBJECT,
            FILE_CHOOSER,
            "SaveFiles",
            "osssa{sv}",
            "/org/freedesktop/portal/desktop/request/1_1/t3",
            "org.example.Test",
            "",
            "",
            "2",
            "current_folder",
            "ay",
            "5",
            "47",
            "116",
            "109",
            "112",
            "0",
            "files",
            "aay",
            "2",
            "4",
            "120",
            "46",
            "106",
            "0",
            "2",
            "121",
            "0",
        ],
    ));
    assert_eq!(
        reply,
        format!(r#"ua{{sv}} 0 1 "uris" as 2 "{base}/a%20file.txt/x.j" "{base}/a%20file.txt/y""#)
    );
    let request = std::fs::read_to_string(dir.join("last-request.toml")).unwrap();
    assert!(request.contains("kind = \"save-files\""), "{request}");
    assert!(request.contains("folder = \"/tmp\""), "{request}");

    // ── The rest of the object ────────────────────────────────────────────
    let version = stdout(&busctl(
        &address,
        &[
            "call",
            NAME,
            OBJECT,
            "org.freedesktop.DBus.Properties",
            "Get",
            "ss",
            FILE_CHOOSER,
            "version",
        ],
    ));
    assert_eq!(version, "v u 4");
    let xml = stdout(&busctl(
        &address,
        &[
            "call",
            NAME,
            OBJECT,
            "org.freedesktop.DBus.Introspectable",
            "Introspect",
        ],
    ));
    // busctl prints the string with its quotes escaped, so match on names.
    for part in ["OpenFile", "SaveFile", "SaveFiles", "version"] {
        assert!(xml.contains(part), "{part} in {xml}");
    }

    // An unknown method is an error reply, and the service lives on.
    let unknown = busctl(
        &address,
        &["call", NAME, OBJECT, FILE_CHOOSER, "Frobnicate"],
    );
    assert!(!unknown.status.success());
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("no method"),
        "{}",
        String::from_utf8_lossy(&unknown.stderr)
    );
    // So is a call with the wrong arguments.
    let wrong = busctl(
        &address,
        &["call", NAME, OBJECT, FILE_CHOOSER, "OpenFile", "s", "nope"],
    );
    assert!(!wrong.status.success());
    // Close on a request that is not open.
    let gone = busctl(
        &address,
        &[
            "call",
            NAME,
            "/org/freedesktop/portal/desktop/request/1_1/nothing",
            "org.freedesktop.impl.portal.Request",
            "Close",
        ],
    );
    assert!(!gone.status.success());
    stdout(&busctl(
        &address,
        &["call", NAME, OBJECT, "org.freedesktop.DBus.Peer", "Ping"],
    ));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The way the backend really starts: nobody runs `--portal`; the first call
/// to its name makes the bus start it from the service file `install.sh`
/// writes. That first call must be answered, not lost — it arrives while the
/// service is still in `RequestName`.
///
/// The bus listens in the abstract namespace here, so the client's
/// `unix:abstract=` address is exercised too.
#[test]
fn the_bus_starts_the_backend_on_the_first_call() {
    if !have("dbus-daemon") || !have("busctl") {
        eprintln!("skipping: dbus-daemon or busctl is not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("df-portal-activate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["run", "state", "home", "services"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../build")
        .join(format!("{NAME}.service.in"));
    let service = std::fs::read_to_string(template)
        .unwrap()
        .replace("@BIN@", env!("CARGO_BIN_EXE_delightfile"));
    std::fs::write(
        dir.join("services").join(format!("{NAME}.service")),
        service,
    )
    .unwrap();

    let listen = format!("unix:abstract={}", dir.join("bus").display());
    let (bus, address) = private_bus(
        &dir,
        &listen,
        Some(&dir.join("services")),
        &[
            // A picker that quits at once having picked nothing: a cancel.
            ("DELIGHTFILE_PICKER_EXE", Path::new("/usr/bin/true")),
            ("XDG_RUNTIME_DIR", &dir.join("run")),
            ("XDG_STATE_HOME", &dir.join("state")),
            ("HOME", &dir.join("home")),
        ],
    );
    assert!(address.starts_with("unix:abstract="), "{address}");

    let started = Instant::now();
    let reply = stdout(&busctl(
        &address,
        &[
            "call",
            NAME,
            OBJECT,
            FILE_CHOOSER,
            "OpenFile",
            "osssa{sv}",
            "/org/freedesktop/portal/desktop/request/1_1/first",
            "org.example.Test",
            "",
            "First",
            "0",
        ],
    ));
    assert_eq!(reply, "ua{sv} 1 0");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the activating call was answered, not timed out: {:?}",
        started.elapsed()
    );

    let pid = stdout(&busctl(
        &address,
        &[
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetConnectionUnixProcessID",
            "s",
            NAME,
        ],
    ));
    let pid = pid.trim_start_matches("u ").to_string();
    // The service lives as long as its bus, and no longer.
    drop(bus);
    wait_for("the service to exit with its bus", || has_exited(&pid));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Whether process `pid` has exited: it is gone from /proc, or it is a zombie,
/// finished with nobody yet to collect its status.
///
/// The bus started the service, so killing the bus orphans it, and whoever
/// adopts orphans reaps it: systemd or a subreaper on a desktop, PID 1 in a
/// container. GitHub's job container runs `tail -f /dev/null` as PID 1, which
/// never reaps, so there a service that has exited stays in /proc for good.
fn has_exited(pid: &str) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/status")) {
        Err(_) => true,
        Ok(status) => status.lines().any(|line| {
            line.strip_prefix("State:")
                .is_some_and(|state| state.trim_start().starts_with(['Z', 'X']))
        }),
    }
}
