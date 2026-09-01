//! Proving the vfs without a network.
//!
//! Three tiers, in order of how much of the stack each one trusts:
//!
//! 1. **Codec tests** — every [`wire::Request`] and [`wire::Reply`] goes out
//!    through its encoder and back through its decoder and must be the same
//!    value; every bound (oversize, truncated, overrun, implausible counts) is
//!    poked with a hostile packet. Pure functions, no processes.
//! 2. **Fake-server tests** — [`Service::program`] pointed at `sh` replaying
//!    crafted bytes, which is how "the server sent a reply with somebody
//!    else's id" and "the server claimed a 4 GB packet" are exercised without
//!    a server that would never send them.
//! 3. **The hermetic integration test** — OpenSSH's own `sftp-server` binary
//!    *is* the other end of this protocol, and spawning it directly against a
//!    temp directory exercises every byte of framing, pipelining, status
//!    mapping and teardown that a real connection does, minus only the ssh
//!    transport (which is OpenSSH's code, not ours). Skipped with a printed
//!    reason on machines without the binary.

#![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::conn::{classify_ssh_failure, Connection};
use super::wire::{
    self, decode_reply, decode_request, Attrs, NameEntry, Packet, ProtocolError, Reply, Request,
    Status, StatusCode, MAX_NAME_ENTRIES, MAX_PACKET,
};
use super::{Service, Vfs, VfsConfig, VfsError, VfsPath, VfsUpdate};
use crate::fs::{no_notifier, Kind, LinkTarget};
use crate::tasks::{ProgressSink, TaskCtx, TaskFlags};

/// Generous enough for a loaded CI box, short enough that a genuine hang fails
/// the suite instead of hanging it.
const T: Duration = Duration::from_secs(10);

// ── Fixtures ────────────────────────────────────────────────────────────────

/// A directory under `$TMPDIR` that removes itself — same twelve lines as
/// `fs::tests`, because `tempfile` would be a dependency for them (PLAN §1).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("delightfile-{tag}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the fixture directory");
        TempDir { path }
    }

    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.path.join(name);
        std::fs::write(&path, contents).expect("write a fixture file");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ── Codec: request roundtrips ───────────────────────────────────────────────

/// Encode with an id, frame-parse, decode, and demand the same value back.
fn request_roundtrip(request: Request) {
    let id = 7;
    let bytes = request.encode(id).expect("encode");
    let packet = Packet::parse(&bytes).expect("parse the frame");
    let (got_id, got) = decode_request(&packet).expect("decode");
    if matches!(request, Request::Init { .. }) {
        assert_eq!(got_id, 0, "INIT has no id");
    } else {
        assert_eq!(got_id, id);
    }
    assert_eq!(got, request);
}

#[test]
fn every_request_survives_the_wire() {
    let attrs = Attrs {
        size: Some(42),
        uid: Some(1000),
        gid: Some(1000),
        permissions: Some(0o100_644),
        atime: Some(1_700_000_000),
        mtime: Some(1_700_000_001),
    };
    let requests = vec![
        Request::Init { version: 3 },
        Request::Open {
            path: b"/tmp/a file".to_vec(),
            pflags: wire::FXF_WRITE | wire::FXF_CREAT | wire::FXF_TRUNC,
            attrs,
        },
        Request::Close {
            handle: b"h0".to_vec(),
        },
        Request::Read {
            handle: b"h0".to_vec(),
            offset: u64::MAX - 1,
            len: 32 * 1024,
        },
        Request::Write {
            handle: b"h0".to_vec(),
            offset: 65536,
            data: vec![0xAB; 512],
        },
        Request::Stat {
            path: b"/etc".to_vec(),
        },
        Request::LStat {
            path: b"/etc/link".to_vec(),
        },
        Request::FStat {
            handle: b"h1".to_vec(),
        },
        Request::SetStat {
            path: b"/x".to_vec(),
            attrs: Attrs::permissions(0o644),
        },
        Request::OpenDir {
            path: b"/home".to_vec(),
        },
        Request::ReadDir {
            handle: b"h2".to_vec(),
        },
        Request::Remove {
            path: b"/x".to_vec(),
        },
        Request::Mkdir {
            path: b"/y".to_vec(),
            attrs: Attrs::empty(),
        },
        Request::Rmdir {
            path: b"/y".to_vec(),
        },
        Request::RealPath {
            path: b".".to_vec(),
        },
        Request::Rename {
            old: b"/a".to_vec(),
            new: b"/b".to_vec(),
        },
        Request::ReadLink {
            path: b"/l".to_vec(),
        },
        Request::Symlink {
            target: b"target".to_vec(),
            link: b"the-link".to_vec(),
        },
    ];
    for request in requests {
        request_roundtrip(request);
    }
}

/// The de facto protocol, not the draft: `SYMLINK` puts the *target* first on
/// the wire, because that is what OpenSSH's server reads. See the `wire`
/// module header — sending the draft's order silently creates the link
/// backwards, which is why the byte order itself is pinned here.
#[test]
fn symlink_puts_the_target_first_on_the_wire() {
    let bytes = Request::Symlink {
        target: b"TARGET".to_vec(),
        link: b"LINK".to_vec(),
    }
    .encode(1)
    .unwrap();
    // frame(4) + type(1) + id(4) + strlen(4), then the first string's bytes.
    assert_eq!(&bytes[13..19], b"TARGET");
}

// ── Codec: reply roundtrips ─────────────────────────────────────────────────

fn reply_roundtrip(reply: Reply) {
    let id = 9;
    let bytes = reply.encode(id).expect("encode");
    let packet = Packet::parse(&bytes).expect("parse the frame");
    let (got_id, got) = decode_reply(&packet).expect("decode");
    if matches!(reply, Reply::Version { .. }) {
        assert_eq!(got_id, 0, "VERSION has no id");
    } else {
        assert_eq!(got_id, id);
    }
    assert_eq!(got, reply);
}

#[test]
fn every_reply_survives_the_wire() {
    let replies = vec![
        Reply::Version {
            version: 3,
            extensions: vec![("posix-rename@openssh.com".into(), "1".into())],
        },
        Reply::Status(Status {
            code: StatusCode::NoSuchFile,
            message: "No such file".into(),
        }),
        Reply::Handle(b"\x00\x01\x02".to_vec()),
        Reply::Data(vec![0x5A; 32 * 1024]),
        Reply::Name(vec![
            NameEntry {
                filename: b"hello.txt".to_vec(),
                longname: b"-rw-r--r--    1 brian    brian          16 Aug 31 12:00 hello.txt"
                    .to_vec(),
                attrs: Attrs {
                    size: Some(16),
                    uid: Some(1000),
                    gid: Some(1000),
                    permissions: Some(0o100_644),
                    atime: Some(1),
                    mtime: Some(2),
                },
            },
            NameEntry {
                // Not UTF-8, on purpose: a v3 filename is bytes.
                filename: vec![0x66, 0xFF, 0x6F],
                longname: Vec::new(),
                attrs: Attrs::empty(),
            },
        ]),
        Reply::Attrs(Attrs {
            size: Some(1),
            ..Attrs::empty()
        }),
    ];
    for reply in replies {
        reply_roundtrip(reply);
    }
}

// ── Codec: attrs ────────────────────────────────────────────────────────────

#[test]
fn attrs_only_carry_what_their_flags_claim() {
    // Empty: one flags word, nothing else.
    let mut e = wire::Encoder::raw(0);
    Attrs::empty().encode(&mut e);
    assert_eq!(e.finish().unwrap().len(), 4 + 1 + 4);

    // A uid without a gid still has to send both — v3 packs them under one
    // flag — and the absent half goes out as 0.
    let one_sided = Attrs {
        uid: Some(7),
        ..Attrs::empty()
    };
    let mut e = wire::Encoder::raw(0);
    one_sided.encode(&mut e);
    let bytes = e.finish().unwrap();
    let mut d = wire::Decoder::new(&bytes[5..]);
    let back = Attrs::decode(&mut d).unwrap();
    assert_eq!(back.uid, Some(7));
    assert_eq!(back.gid, Some(0), "the missing half is encoded as zero");

    // Absent fields decode as absent, never as zero.
    let sparse = Attrs {
        size: Some(10),
        ..Attrs::empty()
    };
    let mut e = wire::Encoder::raw(0);
    sparse.encode(&mut e);
    let bytes = e.finish().unwrap();
    let mut d = wire::Decoder::new(&bytes[5..]);
    let back = Attrs::decode(&mut d).unwrap();
    assert_eq!(back.size, Some(10));
    assert_eq!(back.uid, None);
    assert_eq!(back.permissions, None);
    assert_eq!(back.mtime, None);
}

#[test]
fn attr_extensions_are_skipped_not_choked_on() {
    let mut e = wire::Encoder::raw(0);
    e.put_u32(wire::ATTR_SIZE | wire::ATTR_EXTENDED);
    e.put_u64(99);
    e.put_u32(2); // two extension pairs
    e.put_str("vendor@example");
    e.put_str("data");
    e.put_str("other@example");
    e.put_str("more");
    let bytes = e.finish().unwrap();
    let mut d = wire::Decoder::new(&bytes[5..]);
    let attrs = Attrs::decode(&mut d).unwrap();
    assert_eq!(attrs.size, Some(99));
    assert!(
        d.is_empty(),
        "the extension bytes were stepped over exactly"
    );
}

#[test]
fn a_hostile_extension_count_is_refused() {
    let mut e = wire::Encoder::raw(0);
    e.put_u32(wire::ATTR_EXTENDED);
    e.put_u32(u32::MAX); // 4 billion extensions in a 9-byte packet
    let bytes = e.finish().unwrap();
    let mut d = wire::Decoder::new(&bytes[5..]);
    assert!(matches!(
        Attrs::decode(&mut d),
        Err(ProtocolError::ImplausibleCount { .. })
    ));
}

#[test]
fn attrs_classify_file_types() {
    let dir = Attrs::permissions(wire::S_IFDIR | 0o755);
    assert!(dir.is_dir() && !dir.is_file() && !dir.is_symlink());
    let link = Attrs::permissions(wire::S_IFLNK | 0o777);
    assert!(link.is_symlink() && !link.is_dir());
    let file = Attrs::permissions(wire::S_IFREG | 0o644);
    assert!(file.is_file());
    assert_eq!(Attrs::empty().file_type(), None, "no mode, no claim");
}

// ── Codec: status ───────────────────────────────────────────────────────────

#[test]
fn status_codes_roundtrip_and_read_like_sentences() {
    for code in 0..=9u32 {
        let status = StatusCode::from_u32(code);
        assert_eq!(status.as_u32(), code);
        assert!(!status.describe().is_empty());
        assert!(
            !status.describe().contains("SSH_FX"),
            "constant names are not explanations"
        );
    }
    assert_eq!(StatusCode::from_u32(2), StatusCode::NoSuchFile);
    assert_eq!(StatusCode::from_u32(3), StatusCode::PermissionDenied);
    assert_eq!(StatusCode::from_u32(999), StatusCode::Other(999));
    assert!(StatusCode::Eof.is_eof() && !StatusCode::Ok.is_eof());
}

#[test]
fn a_status_with_no_message_still_decodes() {
    // Old OpenSSH stops after the code: no message, no language tag. The
    // draft calls that malformed; reality calls it Tuesday.
    let mut e = wire::Encoder::request(wire::FXP_STATUS, 3);
    e.put_u32(StatusCode::PermissionDenied.as_u32());
    let bytes = e.finish().unwrap();
    let packet = Packet::parse(&bytes).unwrap();
    let (id, reply) = decode_reply(&packet).unwrap();
    assert_eq!(id, 3);
    match reply {
        Reply::Status(status) => {
            assert_eq!(status.code, StatusCode::PermissionDenied);
            assert_eq!(status.message, "");
            // With no server words, the code's own sentence carries it.
            assert_eq!(status.to_string(), "permission denied");
        }
        other => panic!("expected STATUS, got {other:?}"),
    }
}

#[test]
fn a_status_prefers_the_servers_own_words() {
    let status = Status {
        code: StatusCode::Failure,
        message: "quota exceeded".into(),
    };
    assert_eq!(status.to_string(), "quota exceeded");
}

// ── Codec: framing bounds ───────────────────────────────────────────────────

#[test]
fn framing_refuses_the_hostile_cases() {
    // Oversize: a length prefix past MAX_PACKET is refused before any body.
    let mut oversize = ((MAX_PACKET + 1) as u32).to_be_bytes().to_vec();
    oversize.push(wire::FXP_DATA);
    assert!(matches!(
        Packet::parse(&oversize),
        Err(ProtocolError::TooLarge(_))
    ));

    // Zero-length: every packet has at least a type byte.
    assert!(matches!(
        Packet::parse(&[0, 0, 0, 0]),
        Err(ProtocolError::Empty)
    ));

    // Truncated: the prefix promises more than arrived.
    assert!(matches!(
        Packet::parse(&[0, 0, 0, 10, wire::FXP_DATA, 1, 2]),
        Err(ProtocolError::Truncated { .. })
    ));

    // A frame too short to even hold a prefix.
    assert!(matches!(
        Packet::parse(&[0, 0]),
        Err(ProtocolError::Truncated { .. })
    ));
}

#[test]
fn a_string_cannot_read_past_its_packet() {
    // A HANDLE reply whose string claims 100 bytes in a 2-byte body.
    let mut e = wire::Encoder::request(wire::FXP_HANDLE, 1);
    e.put_u32(100); // the lie
    e.put_u8(0xAA);
    e.put_u8(0xBB);
    let bytes = e.finish().unwrap();
    let packet = Packet::parse(&bytes).unwrap();
    assert!(matches!(
        decode_reply(&packet),
        Err(ProtocolError::StringOverrun { want: 100, .. })
    ));
}

#[test]
fn trailing_junk_after_a_request_is_reported() {
    let mut bytes = Request::Stat {
        path: b"/x".to_vec(),
    }
    .encode(1)
    .unwrap();
    // Append junk and fix up the length prefix to include it.
    bytes.extend_from_slice(&[0xDE, 0xAD]);
    let body = (bytes.len() - 4) as u32;
    bytes[..4].copy_from_slice(&body.to_be_bytes());
    let packet = Packet::parse(&bytes).unwrap();
    assert!(matches!(
        decode_request(&packet),
        Err(ProtocolError::Trailing(2))
    ));
}

#[test]
fn an_implausible_name_count_is_refused_before_allocating() {
    let mut e = wire::Encoder::request(wire::FXP_NAME, 1);
    e.put_u32(MAX_NAME_ENTRIES + 1);
    let bytes = e.finish().unwrap();
    let packet = Packet::parse(&bytes).unwrap();
    assert!(matches!(
        decode_reply(&packet),
        Err(ProtocolError::ImplausibleCount { .. })
    ));
}

#[test]
fn the_encoder_refuses_to_build_an_oversize_packet() {
    let mut e = wire::Encoder::request(wire::FXP_WRITE, 1);
    e.put_bytes(&vec![0u8; MAX_PACKET]);
    assert!(matches!(e.finish(), Err(ProtocolError::TooLarge(_))));
}

#[test]
fn unknown_packet_types_are_unexpected_not_fatal_panics() {
    let packet = Packet {
        kind: 200,
        body: vec![0, 0, 0, 1],
    };
    assert!(matches!(
        decode_reply(&packet),
        Err(ProtocolError::UnexpectedType { kind: 200, .. })
    ));
    assert!(matches!(
        decode_request(&packet),
        Err(ProtocolError::UnexpectedType { kind: 200, .. })
    ));
}

// ── Longname fallbacks ──────────────────────────────────────────────────────

#[test]
fn longname_yields_a_type_and_an_owner_when_it_is_ls_shaped() {
    let entry = NameEntry {
        filename: b"sub".to_vec(),
        longname: b"drwxr-xr-x    2 brian    users        4096 Aug 31 12:00 sub".to_vec(),
        attrs: Attrs::empty(),
    };
    assert_eq!(entry.longname_type(), Some('d'));
    assert_eq!(
        entry.longname_owner(),
        Some(("brian".to_string(), "users".to_string()))
    );

    let unshaped = NameEntry {
        filename: b"x".to_vec(),
        longname: b"whatever".to_vec(),
        attrs: Attrs::empty(),
    };
    assert_eq!(unshaped.longname_type(), None);
}

// ── ssh failure classification ──────────────────────────────────────────────

#[test]
fn ssh_stderr_is_classified_into_auth_versus_network() {
    let auth = [
        "brian@host: Permission denied (publickey).",
        "Host key verification failed.",
        "No supported authentication methods available",
        "Cannot read the passphrase in batch mode",
    ];
    for detail in auth {
        assert!(
            matches!(
                classify_ssh_failure("s".into(), detail.into()),
                VfsError::Auth { .. }
            ),
            "{detail:?} should classify as auth"
        );
    }
    let network = [
        "ssh: connect to host example port 22: Connection timed out",
        "ssh: Could not resolve hostname nope",
        "",
    ];
    for detail in network {
        assert!(
            matches!(
                classify_ssh_failure("s".into(), detail.into()),
                VfsError::Disconnected { .. }
            ),
            "{detail:?} should classify as disconnected"
        );
    }
}

#[test]
fn error_fatality_sorts_the_right_piles() {
    let status = VfsError::Status {
        path: "sftp://s/x".into(),
        status: Status {
            code: StatusCode::NoSuchFile,
            message: String::new(),
        },
    };
    assert!(!status.is_connection_fatal(), "the session survives a `no`");
    assert!(!VfsError::Cancelled.is_connection_fatal());
    let protocol = VfsError::Protocol {
        service: "s".into(),
        source: ProtocolError::Empty,
    };
    assert!(protocol.is_connection_fatal(), "a violation hangs up");
    let timeout = VfsError::Timeout {
        service: "s".into(),
        op: "stat",
        timeout: super::OP_TIMEOUT,
    };
    assert!(
        timeout.is_connection_fatal(),
        "a wedged stream is torn down"
    );
}

// ── vfs.toml ────────────────────────────────────────────────────────────────

/// The real file's shape, verbatim — `~/.config/yazi/vfs.toml` today.
const YAZI_FIXTURE: &str = r#"
[services.showandtour1]
type = "sftp"
host = "showandtour1"
user = "brian"
port = 22
key_file = "~/.ssh/id_ed25519"

[services.showandtour2]
type = "sftp"
host = "showandtour2"
user = "brian"
port = 22
key_file = "~/.ssh/id_ed25519"
"#;

#[test]
fn the_real_yazi_file_shape_parses() {
    let (config, warnings) = VfsConfig::parse(YAZI_FIXTURE, std::path::Path::new("vfs.toml"));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(config.services.len(), 2);
    let one = config.service("showandtour1").unwrap();
    assert_eq!(one.host, "showandtour1");
    assert_eq!(one.user.as_deref(), Some("brian"));
    assert_eq!(one.port, 22);
    assert_eq!(one.key_file.as_deref(), Some("~/.ssh/id_ed25519"));
    assert_eq!(one.destination(), "brian@showandtour1");
    // File order is the `g 1`/`g 2` order.
    assert_eq!(config.services[0].name, "showandtour1");
    assert_eq!(config.services[1].name, "showandtour2");
}

#[test]
fn a_minimal_service_leaves_everything_to_ssh() {
    let (config, warnings) = VfsConfig::parse(
        "[services.min]\nhost = \"minimal\"\n",
        std::path::Path::new("vfs.toml"),
    );
    assert!(warnings.is_empty());
    let min = config.service("min").unwrap();
    assert_eq!(min.user, None);
    assert_eq!(min.port, super::DEFAULT_SSH_PORT);
    assert_eq!(min.key_file, None);
    assert_eq!(min.destination(), "minimal");
    assert_eq!(
        min.root_path(),
        ".",
        "the login directory, like `sftp host`"
    );
}

#[test]
fn bad_lines_warn_and_the_good_ones_still_load() {
    let text = r#"
[services.good]
host = "fine"

[services.bad-port]
host = "h"
port = 70000

[services.no-host]
user = "brian"

[services.future]
type = "s3"
host = "bucket"

[services.a.b]
host = "typo"
"#;
    let (config, warnings) = VfsConfig::parse(text, std::path::Path::new("vfs.toml"));
    assert_eq!(
        config.services.len(),
        1,
        "only the good one loads: {:?}",
        config.services
    );
    assert!(config.service("good").is_some());
    assert_eq!(warnings.len(), 4, "{warnings:?}");
    let all = warnings
        .iter()
        .map(|w| w.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("port"), "{all}");
    assert!(all.contains("host"), "{all}");
    assert!(all.contains("s3"), "{all}");
    assert!(all.contains("dotted"), "{all}");
}

#[test]
fn root_and_yazis_path_key_are_the_same_thing() {
    let (config, _) = VfsConfig::parse(
        "[services.a]\nhost = \"h\"\npath = \"/srv\"\n[services.b]\nhost = \"h\"\nroot = \"/opt\"\n",
        std::path::Path::new("vfs.toml"),
    );
    assert_eq!(config.service("a").unwrap().root_path(), "/srv");
    assert_eq!(config.service("b").unwrap().root_path(), "/opt");
}

#[test]
fn delightfiles_file_overrides_per_service_not_per_file() {
    let dir = TempDir::new("vfs-toml");
    let yazi = dir.file("yazi.toml", YAZI_FIXTURE.as_bytes());
    let ours = dir.file(
        "delightfile.toml",
        b"[services.showandtour2]\nhost = \"showandtour2\"\nport = 2222\n",
    );
    let (config, warnings) = VfsConfig::load_files(&[yazi, ours]);
    assert!(warnings.is_empty(), "{warnings:?}");
    // Overriding one host did not drop the other...
    assert_eq!(config.services.len(), 2);
    assert_eq!(config.service("showandtour2").unwrap().port, 2222);
    // ...and did not move it in the g-number order either.
    assert_eq!(config.services[1].name, "showandtour2");
    assert_eq!(config.service("showandtour1").unwrap().port, 22);
}

#[test]
fn a_missing_file_is_silence() {
    let (config, warnings) = VfsConfig::load_file(std::path::Path::new("/nonexistent/vfs.toml"));
    assert!(config.services.is_empty());
    assert!(warnings.is_empty(), "nothing to warn about");
}

#[test]
fn key_path_expands_a_leading_tilde() {
    let mut service = Service::new("s", "h");
    service.key_file = Some("~/.ssh/id_ed25519".into());
    let expanded = service.key_path().unwrap();
    assert!(!expanded.to_string_lossy().contains('~'), "{expanded:?}");
    assert!(expanded.to_string_lossy().ends_with("/.ssh/id_ed25519"));

    service.key_file = Some("/abs/key".into());
    assert_eq!(service.key_path().unwrap(), PathBuf::from("/abs/key"));
}

#[test]
fn the_ssh_command_line_is_exactly_what_the_design_promises() {
    let mut service = Service::new("s", "myhost");
    service.user = Some("brian".into());
    let command = service.command();
    assert_eq!(command.get_program(), "ssh");
    let args: Vec<String> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let joined = args.join(" ");
    assert!(joined.contains("-o BatchMode=yes"), "{joined}");
    assert!(joined.contains("-o ConnectTimeout="), "{joined}");
    assert!(joined.ends_with("-s brian@myhost sftp"), "{joined}");
    assert!(joined.contains("-x"), "{joined}");
    // Port 22 is *omitted*, so a `Port` directive in ~/.ssh/config wins.
    assert!(!joined.contains("-p"), "{joined}");
    assert!(!joined.contains("-i"), "{joined}");

    service.port = 2222;
    service.key_file = Some("/k".into());
    let args: Vec<String> = service
        .command()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let joined = args.join(" ");
    assert!(joined.contains("-p 2222"), "{joined}");
    assert!(joined.contains("-i /k"), "{joined}");
}

// ── VfsPath ─────────────────────────────────────────────────────────────────

#[test]
fn urls_parse_and_print_symmetrically() {
    let cases = [
        ("sftp://showandtour1", "showandtour1", ""),
        ("sftp://showandtour1/", "showandtour1", ""),
        ("sftp://s/Work/delightfile", "s", "/Work/delightfile"),
        ("sftp://s//etc", "s", "//etc"),
    ];
    for (url, service, path) in cases {
        let parsed = VfsPath::parse(url).unwrap();
        assert_eq!(parsed.service, service, "{url}");
        assert_eq!(parsed.path, path, "{url}");
        assert_eq!(VfsPath::parse(&parsed.to_url()), Some(parsed), "{url}");
    }
    assert_eq!(
        VfsPath::parse("/home/brian"),
        None,
        "local paths are not urls"
    );
    assert_eq!(VfsPath::parse("sftp://"), None);
    assert_eq!(VfsPath::parse("http://x/y"), None);
}

#[test]
fn join_parent_and_name_agree() {
    let root = VfsPath::new("s", "");
    assert_eq!(root.name(), "s", "the root is named after its service");
    assert_eq!(root.parent(), None);

    let child = root.join("Work");
    assert_eq!(child.path, "Work", "children of the root are relative");
    assert_eq!(child.name(), "Work");
    assert_eq!(child.parent(), Some(root.clone()));

    let deeper = child.join("delightfile");
    assert_eq!(deeper.path, "Work/delightfile");
    assert_eq!(deeper.parent(), Some(child));
    assert_eq!(deeper.to_url(), "sftp://s/Work/delightfile");

    let absolute = VfsPath::new("s", "/etc/ssh");
    assert_eq!(absolute.name(), "ssh");
    assert_eq!(absolute.parent().unwrap().path, "/etc");
    assert_eq!(absolute.parent().unwrap().parent().unwrap().path, "/");
}

// ── Remote rows ─────────────────────────────────────────────────────────────

fn name_entry(name: &str, attrs: Attrs, longname: &str) -> NameEntry {
    NameEntry {
        filename: name.as_bytes().to_vec(),
        longname: longname.as_bytes().to_vec(),
        attrs,
    }
}

#[test]
fn remote_rows_become_entries_the_panes_can_draw() {
    let dir = VfsPath::new("s", "/srv");

    let file = super::remote_entry(
        &dir,
        &name_entry(
            "notes.md",
            Attrs {
                size: Some(123),
                uid: Some(1000),
                gid: Some(100),
                permissions: Some(wire::S_IFREG | 0o644),
                atime: Some(0),
                mtime: Some(1_700_000_000),
            },
            "",
        ),
        None,
    );
    assert_eq!(file.name, "notes.md");
    assert_eq!(file.kind, Kind::File);
    assert_eq!(file.len, 123);
    assert_eq!(file.path, PathBuf::from("sftp://s/srv/notes.md"));
    assert!(file.mtime.is_some());
    assert_eq!(file.mime, "text/markdown");
    assert!(!file.is_hidden);

    let dotfile = super::remote_entry(
        &dir,
        &name_entry(".env", Attrs::permissions(wire::S_IFREG | 0o600), ""),
        None,
    );
    assert!(dotfile.is_hidden);

    let sub = super::remote_entry(
        &dir,
        &name_entry("sub", Attrs::permissions(wire::S_IFDIR | 0o755), ""),
        None,
    );
    assert_eq!(sub.kind, Kind::Dir);
    assert_eq!(
        sub.len, 0,
        "directory sizes are 'not known yet', as locally"
    );
    assert!(sub.is_dir());
}

#[test]
fn remote_symlinks_carry_their_resolution() {
    let dir = VfsPath::new("s", "");
    let link_attrs = Attrs::permissions(wire::S_IFLNK | 0o777);

    let to_dir = super::remote_entry(
        &dir,
        &name_entry("link", link_attrs, ""),
        Some(&Some(Attrs::permissions(wire::S_IFDIR | 0o755))),
    );
    assert_eq!(
        to_dir.kind,
        Kind::Symlink {
            target: Some(LinkTarget::Dir)
        }
    );
    assert!(to_dir.is_dir(), "`→` may enter it");

    let target_attrs = Attrs {
        size: Some(999),
        permissions: Some(wire::S_IFREG | 0o644),
        ..Attrs::empty()
    };
    let to_file = super::remote_entry(
        &dir,
        &name_entry("link", link_attrs, ""),
        Some(&Some(target_attrs)),
    );
    assert_eq!(to_file.len, 999, "a resolving link shows the target's size");

    let broken = super::remote_entry(&dir, &name_entry("link", link_attrs, ""), Some(&None));
    assert_eq!(broken.kind, Kind::Symlink { target: None });
    assert!(broken.is_broken_symlink());

    // Budget ran out: shown as a link to a file, never a guessed directory.
    let unresolved = super::remote_entry(&dir, &name_entry("link", link_attrs, ""), None);
    assert_eq!(unresolved.kind, Kind::Symlink { target: None });
}

#[test]
fn a_server_with_no_attrs_falls_back_to_the_longname() {
    let dir = VfsPath::new("s", "");
    let bare_dir = super::remote_entry(
        &dir,
        &name_entry(
            "sub",
            Attrs::empty(),
            "drwxr-xr-x    2 brian    users     4096 Aug 31 12:00 sub",
        ),
        None,
    );
    assert_eq!(bare_dir.kind, Kind::Dir);

    let bare_file = super::remote_entry(&dir, &name_entry("f", Attrs::empty(), ""), None);
    assert_eq!(bare_file.kind, Kind::File);
}

// ── Fake servers: the failures a real one will not stage ────────────────────

/// A service whose "server" is `sh` replaying `bytes`, then holding its pipes
/// open until delightfile hangs up (the `read` waits for a newline that never
/// comes; killing the child on drop ends it).
fn replay_service(dir: &TempDir, bytes: &[u8]) -> Service {
    assert!(
        !bytes.contains(&b'\n'),
        "replayed bytes must not satisfy the holding `read`"
    );
    let fixture = dir.file("replay.bin", bytes);
    Service::direct(
        "fake",
        "/bin/sh",
        vec![
            "-c".into(),
            "cat \"$0\"; read _hold".into(),
            fixture.display().to_string(),
        ],
    )
}

/// A valid VERSION 3 reply, followed by a REALPATH `NAME` answer for the
/// connect-time home resolution (request id 1), so a fake server can get a
/// [`Connection`] all the way up before misbehaving.
fn handshake_bytes() -> Vec<u8> {
    let mut bytes = Reply::Version {
        version: 3,
        extensions: Vec::new(),
    }
    .encode(0)
    .unwrap();
    bytes.extend(
        Reply::Name(vec![NameEntry {
            filename: b"/fake".to_vec(),
            longname: b"/fake".to_vec(),
            attrs: Attrs::empty(),
        }])
        .encode(1)
        .unwrap(),
    );
    bytes
}

fn connect_fake(service: Service) -> Result<Connection, VfsError> {
    Connection::connect(Arc::new(service))
}

/// `expect_err` needs `Debug` on the success type, which a live connection
/// deliberately does not have; this is the same assertion by hand.
fn connect_err(service: Service, why: &str) -> VfsError {
    match connect_fake(service) {
        Ok(_) => panic!("{why}"),
        Err(e) => e,
    }
}

#[test]
fn a_server_speaking_the_wrong_version_is_refused() {
    let dir = TempDir::new("vfs-badver");
    let bytes = Reply::Version {
        version: 5,
        extensions: Vec::new(),
    }
    .encode(0)
    .unwrap();
    let error = connect_err(replay_service(&dir, &bytes), "must refuse");
    assert!(
        matches!(
            error,
            VfsError::Protocol {
                source: ProtocolError::Version(5),
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn a_reply_with_somebody_elses_id_hangs_up_the_connection() {
    let dir = TempDir::new("vfs-badid");
    let mut bytes = handshake_bytes();
    // The next request will carry id 2; answer id 77 instead.
    bytes.extend(
        Reply::Status(Status {
            code: StatusCode::Ok,
            message: String::new(),
        })
        .encode(77)
        .unwrap(),
    );
    let mut conn = connect_fake(replay_service(&dir, &bytes)).expect("handshake succeeds");
    let error = conn
        .stat(&VfsPath::new("fake", "x"), true)
        .expect_err("the mismatched id must be a protocol error");
    assert!(
        matches!(
            &error,
            VfsError::Protocol {
                source: ProtocolError::IdMismatch { got: 77, .. },
                ..
            }
        ),
        "{error}"
    );
    assert!(error.is_connection_fatal(), "pipelining's one failure mode");
}

#[test]
fn a_four_gigabyte_length_claim_costs_a_refusal_not_an_allocation() {
    let dir = TempDir::new("vfs-huge");
    let mut bytes = handshake_bytes();
    bytes.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xF0]); // "4 GB follows"
    let mut conn = connect_fake(replay_service(&dir, &bytes)).expect("handshake succeeds");
    let error = conn
        .stat(&VfsPath::new("fake", "x"), true)
        .expect_err("must refuse");
    assert!(
        matches!(
            error,
            VfsError::Protocol {
                source: ProtocolError::TooLarge(_),
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn a_server_that_dies_before_version_reports_ssh_own_words() {
    let error = connect_err(
        Service::direct(
            "fake",
            "/bin/sh",
            vec![
                "-c".into(),
                "echo 'brian@host: Permission denied (publickey).' >&2; exit 255".into(),
            ],
        ),
        "must fail",
    );
    match &error {
        VfsError::Auth { detail, .. } => {
            assert!(detail.contains("Permission denied"), "{detail}")
        }
        other => panic!("expected Auth, got {other}"),
    }
}

#[test]
fn a_server_that_closes_silently_is_a_disconnect() {
    let error = connect_err(
        Service::direct("fake", "/bin/true", Vec::new()),
        "must fail",
    );
    assert!(matches!(error, VfsError::Disconnected { .. }), "{error}");
}

#[test]
fn a_program_that_does_not_exist_is_a_spawn_error() {
    let error = connect_err(
        Service::direct(
            "fake",
            "/nonexistent/delightfile-no-such-binary",
            Vec::new(),
        ),
        "must fail",
    );
    assert!(matches!(error, VfsError::Spawn { .. }), "{error}");
}

// ── The manager, without a connection ───────────────────────────────────────

#[test]
fn an_unknown_service_fails_without_spawning_anything() {
    let vfs = Vfs::with_config(VfsConfig::default(), Vec::new(), no_notifier());
    let ctx = TaskCtx::detached();
    let error = vfs
        .stat(&VfsPath::new("nope", "x"), true, &ctx)
        .expect_err("no such service");
    assert!(matches!(error, VfsError::UnknownService { .. }), "{error}");

    // A listing of an unknown service fails through the channel, like every
    // other listing failure, so the pane draws a reason.
    let token = vfs.scan(VfsPath::new("nope", ""));
    let deadline = Instant::now() + T;
    loop {
        if let Some(update) = vfs.drain().into_iter().next() {
            match update {
                VfsUpdate::Failed {
                    token: t, error, ..
                } => {
                    assert_eq!(t, token);
                    assert!(matches!(error, VfsError::UnknownService { .. }));
                    break;
                }
                other => panic!("expected Failed, got {other:?}"),
            }
        }
        assert!(Instant::now() < deadline, "the failure never arrived");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn cancelled_before_dispatch_never_reaches_the_wire() {
    let mut config = VfsConfig::default();
    // A service that would fail to connect — the point is that it never tries.
    config.insert(Service::direct("s", "/nonexistent/binary", Vec::new()));
    let vfs = Vfs::with_config(config, Vec::new(), no_notifier());
    let ctx = TaskCtx::detached();
    ctx.flags().cancel();
    let error = vfs
        .stat(&VfsPath::new("s", "x"), true, &ctx)
        .expect_err("cancelled");
    assert!(matches!(error, VfsError::Cancelled), "{error}");
}

// ── The hermetic integration test ───────────────────────────────────────────

/// Where OpenSSH installs `sftp-server`, by distribution habit, then `$PATH`.
fn find_sftp_server() -> Option<PathBuf> {
    for candidate in [
        "/usr/lib/ssh/sftp-server",         // Arch
        "/usr/lib/openssh/sftp-server",     // Debian/Ubuntu
        "/usr/libexec/sftp-server",         // BSD-ish
        "/usr/libexec/openssh/sftp-server", // Fedora
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    let output = std::process::Command::new("which")
        .arg("sftp-server")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let found = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!found.is_empty()).then(|| PathBuf::from(found))
}

/// A sink that counts, for asserting transfer progress actually reports.
#[derive(Default)]
struct CountingSink {
    total: AtomicU64,
    advanced: AtomicU64,
}

impl ProgressSink for CountingSink {
    fn set_total(&self, bytes: u64, _files: u64) {
        self.total.store(bytes, Ordering::SeqCst);
    }
    fn advance(&self, bytes: u64, _files: u64) {
        self.advanced.fetch_add(bytes, Ordering::SeqCst);
    }
}

/// Drain listing updates until `Done`/`Failed` for `token`, or the deadline.
fn collect_listing(vfs: &Vfs, token: super::VfsToken) -> Vec<VfsUpdate> {
    let deadline = Instant::now() + T;
    let mut updates = Vec::new();
    loop {
        for update in vfs.drain() {
            if update.token() != token {
                continue;
            }
            let terminal = matches!(update, VfsUpdate::Done { .. } | VfsUpdate::Failed { .. });
            updates.push(update);
            if terminal {
                return updates;
            }
        }
        assert!(Instant::now() < deadline, "the listing never finished");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The whole protocol against OpenSSH's own `sftp-server`, spawned directly on
/// a temp directory: no ssh, no network, no credentials — the child's stdio
/// *is* the server side of every packet this module can send. See
/// [`Service::program`] for why this seam is a real field.
#[test]
fn sftp_server_round_trip_everything() {
    let Some(server) = find_sftp_server() else {
        eprintln!(
            "skipping sftp_server_round_trip_everything: no sftp-server binary \
             (looked in /usr/lib/ssh, /usr/lib/openssh, /usr/libexec and $PATH)"
        );
        return;
    };

    // The remote side: a tree with every row shape a listing distinguishes.
    let remote = TempDir::new("vfs-remote");
    // Big enough that a download needs multiple chunks *and* multiple windows
    // (16 × 32 KiB = 512 KiB in flight), so the pipelining actually pipelines.
    let big: Vec<u8> = (0..600_000u32).map(|i| (i % 251) as u8).collect();
    remote.file("big.bin", &big);
    remote.file("hello.txt", b"hello over sftp\n");
    std::fs::create_dir(remote.path.join("sub")).unwrap();
    std::fs::write(remote.path.join("sub/inner.txt"), b"inner").unwrap();
    std::os::unix::fs::symlink("sub", remote.path.join("link-to-dir")).unwrap();
    std::os::unix::fs::symlink("hello.txt", remote.path.join("link-to-file")).unwrap();
    std::os::unix::fs::symlink("missing", remote.path.join("broken")).unwrap();

    let mut service = Service::direct(
        "test",
        &server,
        // `-e`: log to stderr instead of syslog, so a server-side complaint
        // lands in the transport's stderr buffer like ssh's would.
        vec!["-e".into()],
    );
    service.root = Some(remote.path.display().to_string());
    let mut config = VfsConfig::default();
    config.insert(service);

    let wakes = Arc::new(AtomicUsize::new(0));
    let bell = Arc::clone(&wakes);
    let vfs = Vfs::with_config(
        config,
        Vec::new(),
        Arc::new(move || {
            bell.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let ctx = TaskCtx::detached();
    let root = VfsPath::new("test", "");

    // ── Listing ─────────────────────────────────────────────────────────
    let token = vfs.scan(root.clone());
    let updates = collect_listing(&vfs, token);
    assert!(
        matches!(updates.first(), Some(VfsUpdate::Started { .. })),
        "Started comes before any entries"
    );
    let mut entries = Vec::new();
    let mut total = 0;
    for update in &updates {
        match update {
            VfsUpdate::Batch { entries: batch, .. } => entries.extend(batch.iter().cloned()),
            VfsUpdate::Done { total: t, .. } => total = *t,
            VfsUpdate::Failed { error, .. } => panic!("listing failed: {error}"),
            VfsUpdate::Started { .. } => {}
        }
    }
    assert_eq!(total, entries.len());
    assert_eq!(
        entries.len(),
        6,
        "{:?}",
        entries.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    assert!(wakes.load(Ordering::SeqCst) > 0, "the bell was rung");

    let by_name = |name: &str| {
        entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} missing from the listing"))
    };
    assert_eq!(by_name("sub").kind, Kind::Dir);
    assert_eq!(by_name("hello.txt").kind, Kind::File);
    assert_eq!(by_name("hello.txt").len, 16);
    assert_eq!(by_name("big.bin").len, big.len() as u64);
    assert_eq!(
        by_name("link-to-dir").kind,
        Kind::Symlink {
            target: Some(LinkTarget::Dir)
        },
        "a link to a directory is enterable"
    );
    assert_eq!(
        by_name("link-to-file").kind,
        Kind::Symlink {
            target: Some(LinkTarget::File)
        }
    );
    assert_eq!(by_name("broken").kind, Kind::Symlink { target: None });
    assert_eq!(
        by_name("hello.txt").path,
        PathBuf::from("sftp://test/hello.txt"),
        "rows carry their URL, which is what the panes navigate by"
    );
    assert!(
        !entries.iter().any(|e| e.name == "." || e.name == ".."),
        "SFTP's dot rows never reach the model"
    );

    // A subdirectory lists through the same path.
    let token = vfs.scan(root.join("sub"));
    let updates = collect_listing(&vfs, token);
    let inner: Vec<_> = updates
        .iter()
        .filter_map(|u| match u {
            VfsUpdate::Batch { entries, .. } => Some(entries.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(inner.len(), 1);
    assert_eq!(inner[0].name, "inner.txt");
    assert_eq!(inner[0].path, PathBuf::from("sftp://test/sub/inner.txt"));

    // ── Stat, both flavours ─────────────────────────────────────────────
    let attrs = vfs.stat(&root.join("big.bin"), true, &ctx).unwrap();
    assert_eq!(attrs.size, Some(big.len() as u64));
    let through = vfs.stat(&root.join("link-to-dir"), true, &ctx).unwrap();
    assert!(through.is_dir(), "STAT follows");
    let at = vfs.stat(&root.join("link-to-dir"), false, &ctx).unwrap();
    assert!(at.is_symlink(), "LSTAT does not");

    // ── Download, pipelined, with progress ──────────────────────────────
    let local = TempDir::new("vfs-local");
    let sink = Arc::new(CountingSink::default());
    let progress_ctx = TaskCtx::with_sink(
        Arc::new(TaskFlags::new()),
        Arc::clone(&sink) as Arc<dyn ProgressSink>,
    );
    let destination = local.path.join("big.bin");
    let n = vfs
        .download(&root.join("big.bin"), &destination, &progress_ctx)
        .unwrap();
    assert_eq!(n, big.len() as u64);
    assert_eq!(std::fs::read(&destination).unwrap(), big, "byte-identical");
    assert_eq!(sink.total.load(Ordering::SeqCst), big.len() as u64);
    assert_eq!(sink.advanced.load(Ordering::SeqCst), big.len() as u64);

    // ── Download to a tempfile (download-on-open) ───────────────────────
    let opened = vfs.download_to_temp(&root.join("hello.txt"), &ctx).unwrap();
    assert_eq!(std::fs::read(&opened).unwrap(), b"hello over sftp\n");
    assert!(
        opened.to_string_lossy().ends_with("hello.txt"),
        "openers sniff extensions, so the name survives: {opened:?}"
    );
    let _ = std::fs::remove_file(&opened);

    // ── Upload (upload-on-drop) ─────────────────────────────────────────
    let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 199) as u8).collect();
    let source = local.file("upload-me.bin", &payload);
    let sent = vfs
        .upload(&source, &root.join("uploaded.bin"), &ctx)
        .unwrap();
    assert_eq!(sent, payload.len() as u64);
    assert_eq!(
        std::fs::read(remote.path.join("uploaded.bin")).unwrap(),
        payload
    );
    // TRUNC: uploading over it replaces, never appends.
    let smaller = local.file("smaller.bin", b"tiny");
    vfs.upload(&smaller, &root.join("uploaded.bin"), &ctx)
        .unwrap();
    assert_eq!(
        std::fs::read(remote.path.join("uploaded.bin")).unwrap(),
        b"tiny"
    );

    // ── mkdir / rename / rmdir / remove / symlink / readlink / chmod ────
    vfs.mkdir(&root.join("newdir"), &ctx).unwrap();
    assert!(remote.path.join("newdir").is_dir());
    vfs.rename(&root.join("newdir"), &root.join("renamed"), &ctx)
        .unwrap();
    assert!(!remote.path.join("newdir").exists());
    assert!(remote.path.join("renamed").is_dir());
    vfs.rmdir(&root.join("renamed"), &ctx).unwrap();
    assert!(!remote.path.join("renamed").exists());

    vfs.symlink("hello.txt", &root.join("made-link"), &ctx)
        .unwrap();
    assert_eq!(
        std::fs::read_link(remote.path.join("made-link")).unwrap(),
        PathBuf::from("hello.txt"),
        "OpenSSH argument order: the link points at the target, not vice versa"
    );
    assert_eq!(
        vfs.readlink(&root.join("made-link"), &ctx).unwrap(),
        "hello.txt"
    );
    vfs.remove(&root.join("made-link"), &ctx).unwrap();
    assert!(!remote.path.join("made-link").symlink_metadata().is_ok());

    vfs.chmod(&root.join("hello.txt"), 0o600, &ctx).unwrap();
    let mode = std::os::unix::fs::MetadataExt::mode(
        &std::fs::metadata(remote.path.join("hello.txt")).unwrap(),
    );
    assert_eq!(mode & 0o777, 0o600);

    // ── Status → readable error ─────────────────────────────────────────
    let error = vfs
        .stat(&root.join("does-not-exist"), true, &ctx)
        .expect_err("must be a status error");
    match &error {
        VfsError::Status { path, status } => {
            assert_eq!(status.code, StatusCode::NoSuchFile);
            assert_eq!(path, "sftp://test/does-not-exist");
            assert!(!error.is_connection_fatal(), "the session survives");
        }
        other => panic!("expected Status, got {other}"),
    }
    // ...and the session really did survive: the same connection still works.
    assert!(vfs.stat(&root.join("hello.txt"), true, &ctx).is_ok());

    // ── realpath ────────────────────────────────────────────────────────
    let canonical = vfs.realpath(&root, &ctx).unwrap();
    assert!(canonical.starts_with('/'), "{canonical}");

    // ── A cancelled download removes its partial file ───────────────────
    let cancelled = TaskCtx::detached();
    cancelled.flags().cancel();
    let victim = local.path.join("cancelled.bin");
    let error = vfs
        .download(&root.join("big.bin"), &victim, &cancelled)
        .expect_err("cancelled");
    assert!(matches!(error, VfsError::Cancelled), "{error}");
    assert!(
        !victim.exists(),
        "a half-downloaded file is worse than none"
    );

    // A cancelled upload removes its partial remote file too.
    let error = vfs
        .upload(&source, &root.join("never.bin"), &cancelled)
        .expect_err("cancelled");
    assert!(matches!(error, VfsError::Cancelled), "{error}");
    assert!(!remote.path.join("never.bin").exists());

    // ── Clean teardown ──────────────────────────────────────────────────
    // Dropping the Vfs joins the worker, which kills and reaps the child; a
    // hang here is the leak this test exists to catch.
    drop(vfs);
}

/// The reconnect path: kill the child out from under a live vfs, watch one
/// operation fail with a connection error, and the next one succeed on a
/// fresh connection.
#[test]
fn a_dropped_connection_reconnects_on_the_next_request() {
    let Some(server) = find_sftp_server() else {
        eprintln!(
            "skipping a_dropped_connection_reconnects_on_the_next_request: \
             no sftp-server binary (looked in /usr/lib/ssh, /usr/lib/openssh, \
             /usr/libexec and $PATH)"
        );
        return;
    };
    let remote = TempDir::new("vfs-reconnect");
    remote.file("still-here.txt", b"yes");

    // A wrapper that runs sftp-server but dies when told to: the pid file
    // lets the test kill exactly the right process.
    let pid_file = remote.path.join(".server-pid");
    let script = format!("echo $$ > {pid}; exec \"$0\" -e", pid = pid_file.display());
    let mut service = Service::direct(
        "test",
        "/bin/sh",
        vec!["-c".into(), script, server.display().to_string()],
    );
    service.root = Some(remote.path.display().to_string());
    let mut config = VfsConfig::default();
    config.insert(service);
    let vfs = Vfs::with_config(config, Vec::new(), no_notifier());
    let ctx = TaskCtx::detached();
    let root = VfsPath::new("test", "");

    // Connect and prove it works.
    assert!(vfs.stat(&root.join("still-here.txt"), true, &ctx).is_ok());

    // Kill the server behind the vfs's back.
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // `kill(1)` rather than `libc::kill`, so the test file stays out of the
    // crate's unsafe census (poll.rs and fs::inotify are its only islands).
    let killed = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(
        killed.success(),
        "the fixture server should have been killable"
    );

    // The next operation fails with a connection-level error...
    let deadline = Instant::now() + T;
    loop {
        match vfs.stat(&root.join("still-here.txt"), true, &ctx) {
            Err(e) if e.is_connection_fatal() => break,
            // The kill may not have landed before this stat went out.
            Ok(_) => {}
            Err(e) => panic!("expected a connection-fatal error, got {e}"),
        }
        assert!(Instant::now() < deadline, "the death was never noticed");
        std::thread::sleep(Duration::from_millis(5));
    }

    // ...and the one after that succeeds on a fresh child.
    let attrs = vfs
        .stat(&root.join("still-here.txt"), true, &ctx)
        .expect("the reconnect should be invisible beyond one error");
    assert_eq!(attrs.size, Some(3));
}
