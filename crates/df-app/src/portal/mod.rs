//! `delightfile --portal`: delightfile as an xdg-desktop-portal **backend**
//! for the file chooser.
//!
//! Every file dialog a sandboxed or portal-aware program opens — Chrome's
//! upload box, Firefox's "Save as", an Electron app's "Attach" — reaches
//! `xdg-desktop-portal`, which forwards it to whichever backend
//! `portals.conf` prefers for `org.freedesktop.impl.portal.FileChooser`. This
//! is that backend. It owns
//! `org.freedesktop.impl.portal.desktop.delightfile` on the session bus and
//! answers `OpenFile`, `SaveFile` and `SaveFiles` at
//! `/org/freedesktop/portal/desktop`.
//!
//! ## Why a backend, and not only the termfilechooser wrapper
//!
//! `xdg-desktop-portal-termfilechooser` is a backend too, and it hands its
//! wrapper script five positional arguments: multiple, directory, save, a
//! path, an output file. Everything else the portal carried — the dialog's
//! title, the caller's button label, the suggested name, the starting folder,
//! the file-type filters — is dropped on the floor before delightfile is
//! started. Being the backend is the only way to be *told* those things. The
//! wrapper stays (`build/delightfile-wrapper.sh`) for machines that want it.
//!
//! ## Shape
//!
//! One process, one connection, one reader. The loop in [`Service::serve`]
//! reads every message the bus delivers and answers what it can at once —
//! properties, introspection, `Close`, errors. A file-chooser call is the one
//! thing that takes as long as a person does, so it gets a thread of its own
//! ([`request`]): the thread writes the dialog out as a request file, runs a
//! picker window (`delightfile --chooser-file=… --chooser-request=…`, the
//! same binary), waits for it, and sends the answer through the shared
//! [`Outbox`]. Two dialogs can be open at once, and `Close` on one of them is
//! read and acted on while its call is still pending — which is exactly what
//! a single loop blocked on a child could not do.
//!
//! Every `handle` a call names is a `org.freedesktop.impl.portal.Request`
//! object for as long as its dialog is open. D-Bus routes by *name*, not by
//! path, so there is nothing to register: a `Close` arriving at a path that
//! is a pending handle is that request's `Close`.
//!
//! ## Nothing a caller sends can stop the service
//!
//! A body that does not match its signature, options of the wrong type, a
//! duplicate handle: each is an error *reply* to that one call. The request
//! threads never panic on bus data, and the shared state is behind locks that
//! shrug off poisoning, so one bad request is one failed dialog.
//!
//! ## And "Show in folder"
//!
//! The same process serves `org.freedesktop.FileManager1` at
//! `/org/freedesktop/FileManager1` ([`show`]): what Chrome's "Show in folder"
//! calls, and every other program that asks the desktop to point at a file.
//! Its name is claimed second and waited in line for rather than insisted
//! on, so a desktop whose file manager already owns it still gets its file
//! dialogs, and gets "Show in folder" the moment that file manager exits.

mod request;
mod show;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::dbus::{Bus, Claim, Inbox, Message, Outbox, Value, MSG_METHOD_CALL};
use request::{Answer, Dialog, Kind, Pending, Setup};

/// The well-known name the `.portal` file points xdg-desktop-portal at.
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.delightfile";

/// Where every portal backend lives.
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

const FILE_CHOOSER: &str = "org.freedesktop.impl.portal.FileChooser";
const REQUEST: &str = "org.freedesktop.impl.portal.Request";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const INTROSPECTABLE: &str = "org.freedesktop.DBus.Introspectable";
const PEER: &str = "org.freedesktop.DBus.Peer";

/// The interface version this backend answers `version` with.
///
/// The backend's own XML documents no version; the portal-side
/// `org.freedesktop.portal.FileChooser` documents 4, and 4 is the feature set
/// implemented here (`SaveFiles`, `current_folder` on open, `current_filter`).
/// xdg-desktop-portal 1.22 does not read it — it is here for tools that ask.
const VERSION: u32 = 4;

/// Every file-chooser method takes `(handle, app_id, parent_window, title,
/// options)`.
const CALL_SIGNATURE: &str = "osssa{sv}";

const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";
const UNKNOWN_OBJECT: &str = "org.freedesktop.DBus.Error.UnknownObject";
const UNKNOWN_INTERFACE: &str = "org.freedesktop.DBus.Error.UnknownInterface";
const UNKNOWN_PROPERTY: &str = "org.freedesktop.DBus.Error.UnknownProperty";
const PROPERTY_READ_ONLY: &str = "org.freedesktop.DBus.Error.PropertyReadOnly";
const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const FAILED: &str = "org.freedesktop.DBus.Error.Failed";

/// `delightfile --portal`: own the name and serve until the bus goes away.
///
/// The exit code is what D-Bus activation logs: 0 when the session ended
/// under a running service, 1 when it could not start at all.
pub fn run() -> i32 {
    let setup = Setup::from_env();
    let mut bus = match Bus::session() {
        Ok(bus) => bus,
        Err(e) => {
            log::error!("portal: {e}");
            return 1;
        }
    };
    if let Err(e) = bus.request_name(BUS_NAME) {
        log::error!("portal: {e}");
        return 1;
    }
    // Second, and never fatal: the file chooser is what this service is
    // started for, and a FileManager1 somebody else owns is no reason to stop
    // serving it. In line is as good as owned in the end — the bus hands the
    // name over when its owner goes.
    match bus.request_name_queued(show::BUS_NAME) {
        Ok(Claim::Owned) => log::info!("portal: serving {} too", show::BUS_NAME),
        Ok(Claim::Queued) => log::info!(
            "portal: {} is another program's; in line for it",
            show::BUS_NAME
        ),
        Err(e) => log::warn!("portal: not serving {}: {e}", show::BUS_NAME),
    }
    let (inbox, outbox) = match bus.into_service() {
        Ok(halves) => halves,
        Err(e) => {
            log::error!("portal: {e}");
            return 1;
        }
    };
    log::info!("portal: serving {BUS_NAME} at {OBJECT_PATH}");
    let why = Service::new(outbox, setup).serve(inbox);
    log::info!("portal: stopping: {why}");
    0
}

/// Lock, and carry on past a poisoned lock: the state behind these is a map
/// and a flag, and a thread that panicked holding one left nothing half-done
/// that the next holder could trip on.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The dialogs open right now, by their `handle`.
type Requests = Arc<Mutex<HashMap<String, Arc<Mutex<Pending>>>>>;

/// The running backend: what the reader loop needs to answer anything.
struct Service {
    outbox: Arc<Mutex<Outbox>>,
    requests: Requests,
    setup: Arc<Setup>,
    /// How many dialogs this process has started; the `<n>` in each request
    /// file's name, so two open at once never share one.
    started: u64,
}

impl Service {
    fn new(outbox: Outbox, setup: Setup) -> Service {
        Service {
            outbox: Arc::new(Mutex::new(outbox)),
            requests: Arc::new(Mutex::new(HashMap::new())),
            setup: Arc::new(setup),
            started: 0,
        }
    }

    /// Read and answer until the connection ends; the reason it ended.
    fn serve(mut self, mut inbox: Inbox) -> String {
        loop {
            match inbox.next() {
                Ok(msg) => self.dispatch(msg),
                Err(e) => {
                    self.close_all();
                    return e;
                }
            }
        }
    }

    fn dispatch(&mut self, call: Message) {
        // Signals (`NameAcquired`, whatever the bus broadcasts) and stray
        // replies are not questions.
        if call.kind != MSG_METHOD_CALL {
            return;
        }
        if let Some(reply) = self.answer(&call) {
            if call.wants_reply() {
                send(&self.outbox, reply);
            }
        }
    }

    /// The reply to `call`, or `None` when a request thread will send it.
    fn answer(&mut self, call: &Message) -> Option<Message> {
        let path = call.path.as_deref().unwrap_or_default();
        let member = call.member.as_deref().unwrap_or_default();
        let interface = call.interface.as_deref();
        // The interface field is optional on a call; without it the member
        // alone decides.
        let on = |name: &str| interface.is_none_or(|given| given == name);

        if path == OBJECT_PATH && on(FILE_CHOOSER) {
            if let Some(kind) = Kind::of_method(member) {
                return self.start(call, kind);
            }
        }
        if path == show::OBJECT_PATH && on(show::INTERFACE) {
            if let Some(method) = show::Method::of_member(member) {
                return Some(show::answer(call, method, &self.setup));
            }
        }
        if on(REQUEST) && member == "Close" {
            return Some(self.close(call, path));
        }
        if on(PROPERTIES) && path == OBJECT_PATH {
            if let Some(reply) = properties(call, member) {
                return Some(reply);
            }
        }
        if on(PROPERTIES) && path == show::OBJECT_PATH {
            if let Some(reply) = show::properties(call, member) {
                return Some(reply);
            }
        }
        if on(INTROSPECTABLE) && member == "Introspect" {
            return Some(self.introspect(call, path));
        }
        if on(PEER) {
            match member {
                "Ping" => return Some(Message::method_return(call)),
                "GetMachineId" => return Some(machine_id(call)),
                _ => {}
            }
        }
        Some(Message::error(
            call,
            UNKNOWN_METHOD,
            &format!(
                "no method {}.{member} at {path}",
                interface.unwrap_or("(any interface)")
            ),
        ))
    }

    /// A file-chooser call: read it, and hand it to a thread of its own.
    fn start(&mut self, call: &Message, kind: Kind) -> Option<Message> {
        if call.signature.as_deref() != Some(CALL_SIGNATURE) {
            return Some(invalid_args(
                call,
                &format!(
                    "{} takes ({CALL_SIGNATURE}), not ({})",
                    kind.method(),
                    call.signature.as_deref().unwrap_or("")
                ),
            ));
        }
        let dialog = match call.args().and_then(|args| Dialog::from_args(kind, &args)) {
            Ok(dialog) => dialog,
            Err(e) => return Some(invalid_args(call, &e)),
        };
        let handle = dialog.handle.clone();
        let pending = Arc::new(Mutex::new(Pending::default()));
        {
            let mut requests = lock(&self.requests);
            if requests.contains_key(&handle) {
                return Some(invalid_args(
                    call,
                    &format!("a dialog is already open at {handle}"),
                ));
            }
            requests.insert(handle.clone(), Arc::clone(&pending));
        }
        self.started += 1;
        let job = Job {
            call: call.clone(),
            dialog,
            pending,
            number: self.started,
            outbox: Arc::clone(&self.outbox),
            requests: Arc::clone(&self.requests),
            setup: Arc::clone(&self.setup),
        };
        let spawned = std::thread::Builder::new()
            .name("portal-request".into())
            .spawn(move || job.run());
        match spawned {
            Ok(_) => None,
            Err(e) => {
                log::warn!("portal: no thread for {handle}: {e}");
                lock(&self.requests).remove(&handle);
                reply_to(call, &Answer::other())
                    .map_err(|e| log::warn!("portal: {e}"))
                    .ok()
            }
        }
    }

    /// `Request.Close` on `path`: shut that dialog's window. Its call then
    /// answers 2 from its own thread; this call only says it was heard.
    fn close(&self, call: &Message, path: &str) -> Message {
        let pending = lock(&self.requests).get(path).cloned();
        match pending {
            Some(pending) => {
                lock(&pending).close();
                Message::method_return(call)
            }
            None => Message::error(
                call,
                UNKNOWN_OBJECT,
                &format!("no dialog is open at {path}"),
            ),
        }
    }

    /// `Introspect` on any path this service has something at: the portal
    /// object, the FileManager1 object, an open request, or a node on the way
    /// down to any of them.
    fn introspect(&self, call: &Message, path: &str) -> Message {
        let handles: Vec<String> = lock(&self.requests).keys().cloned().collect();
        let known = [OBJECT_PATH, show::OBJECT_PATH]
            .into_iter()
            .chain(handles.iter().map(String::as_str));
        let children = children_of(path, known);
        let interfaces = if path == OBJECT_PATH {
            PORTAL_INTERFACES
        } else if path == show::OBJECT_PATH {
            show::INTERFACES
        } else if handles.iter().any(|handle| handle == path) {
            REQUEST_INTERFACES
        } else if !children.is_empty() {
            STANDARD_INTERFACES
        } else {
            return Message::error(call, UNKNOWN_OBJECT, &format!("nothing at {path}"));
        };
        let mut xml = String::from(DOCTYPE);
        xml.push_str("<node>\n");
        xml.push_str(interfaces);
        for child in children {
            xml.push_str(&format!("  <node name=\"{child}\"/>\n"));
        }
        xml.push_str("</node>\n");
        with_args(call, "s", &[Value::Str(xml)])
    }

    /// The bus is gone, so no answer can be sent: shut every window still
    /// open and clear its files, since the threads that would have done it
    /// end with the process.
    fn close_all(&self) {
        for pending in lock(&self.requests).values() {
            lock(pending).abandon();
        }
    }
}

/// One file-chooser call, carried to the thread that answers it.
struct Job {
    call: Message,
    dialog: Dialog,
    pending: Arc<Mutex<Pending>>,
    number: u64,
    outbox: Arc<Mutex<Outbox>>,
    requests: Requests,
    setup: Arc<Setup>,
}

impl Job {
    fn run(self) {
        let answer = request::ask(&self.dialog, &self.setup, self.number, &self.pending);
        // Gone from the map before the answer goes out, so a `Close` racing
        // the answer finds no dialog rather than one that has already replied.
        lock(&self.requests).remove(&self.dialog.handle);
        log::info!(
            "portal: {} {} answered {} with {} uri(s)",
            self.dialog.kind.method(),
            self.dialog.handle,
            answer.response,
            answer.uris.len()
        );
        if !self.call.wants_reply() {
            return;
        }
        match reply_to(&self.call, &answer) {
            Ok(reply) => send(&self.outbox, reply),
            Err(e) => log::warn!("portal: could not build the answer: {e}"),
        }
    }
}

/// The `(ua{sv})` a file-chooser call answers with: the response code, and on
/// success the `uris`. `choices` and `current_filter` are never returned —
/// the window has no choice widgets, and which filter was active is nothing a
/// caller acts on.
fn reply_to(call: &Message, answer: &Answer) -> Result<Message, String> {
    let results = if answer.response == request::SUCCESS {
        let uris = answer.uris.iter().cloned().map(Value::Str).collect();
        vec![(
            Value::Str("uris".into()),
            Value::variant("as", Value::Array(uris)),
        )]
    } else {
        Vec::new()
    };
    Message::method_return(call).with_args(
        "ua{sv}",
        &[Value::U32(answer.response), Value::Dict(results)],
    )
}

/// A reply with a body, or an error saying why the body could not be built.
fn with_args(call: &Message, sig: &str, args: &[Value]) -> Message {
    Message::method_return(call)
        .with_args(sig, args)
        .unwrap_or_else(|e| Message::error(call, FAILED, &e))
}

fn invalid_args(call: &Message, why: &str) -> Message {
    log::warn!(
        "portal: refusing {}: {why}",
        call.member.as_deref().unwrap_or("a call")
    );
    Message::error(call, INVALID_ARGS, why)
}

fn send(outbox: &Mutex<Outbox>, msg: Message) {
    // A failed write is the connection going; the reader loop sees that on
    // its next read and ends the service.
    if let Err(e) = lock(outbox).send(msg) {
        log::warn!("portal: {e}");
    }
}

/// `org.freedesktop.DBus.Properties` on the portal object. `None` for a
/// member that interface does not have.
fn properties(call: &Message, member: &str) -> Option<Message> {
    let args = match call.args() {
        Ok(args) => args,
        Err(e) => return Some(invalid_args(call, &e)),
    };
    let version = || Value::variant("u", Value::U32(VERSION));
    let reply = match (member, call.signature.as_deref(), args.as_slice()) {
        ("Get", Some("ss"), [Value::Str(interface), Value::Str(name)]) => {
            match (interface.as_str(), name.as_str()) {
                (FILE_CHOOSER, "version") => with_args(call, "v", &[version()]),
                (FILE_CHOOSER, _) => no_property(call, name),
                (other, _) => no_interface(call, other),
            }
        }
        ("GetAll", Some("s"), [Value::Str(interface)]) => match interface.as_str() {
            FILE_CHOOSER => with_args(
                call,
                "a{sv}",
                &[Value::Dict(vec![(Value::Str("version".into()), version())])],
            ),
            // The standard interfaces are here and have no properties.
            PROPERTIES | INTROSPECTABLE | PEER => {
                with_args(call, "a{sv}", &[Value::Dict(Vec::new())])
            }
            other => no_interface(call, other),
        },
        ("Set", Some("ssv"), [Value::Str(interface), Value::Str(name), _]) => {
            match (interface.as_str(), name.as_str()) {
                (FILE_CHOOSER, "version") => {
                    Message::error(call, PROPERTY_READ_ONLY, "version is read-only")
                }
                (FILE_CHOOSER, _) => no_property(call, name),
                (other, _) => no_interface(call, other),
            }
        }
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

fn no_property(call: &Message, name: &str) -> Message {
    Message::error(call, UNKNOWN_PROPERTY, &format!("no property {name}"))
}

fn no_interface(call: &Message, name: &str) -> Message {
    Message::error(
        call,
        UNKNOWN_INTERFACE,
        &format!("no interface {name} here"),
    )
}

/// `Peer.GetMachineId`, which every D-Bus peer is expected to answer.
fn machine_id(call: &Message) -> Message {
    match std::fs::read_to_string("/etc/machine-id") {
        Ok(id) => with_args(call, "s", &[Value::Str(id.trim().to_string())]),
        Err(e) => Message::error(call, FAILED, &format!("/etc/machine-id: {e}")),
    }
}

/// The next path element under `path` of every known path below it — what an
/// introspection of an intermediate node lists, so `busctl tree` can walk
/// down to the portal object and to each open request.
fn children_of<'a>(path: &str, known: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    let prefix = if path == "/" {
        "/".to_string()
    } else {
        format!("{path}/")
    };
    known
        .filter_map(|full| full.strip_prefix(prefix.as_str()))
        .filter_map(|rest| rest.split('/').next())
        .filter(|child| !child.is_empty())
        .map(str::to_string)
        .collect()
}

const DOCTYPE: &str = "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\"\n \"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">\n";

/// What every object here answers besides its own interface.
const STANDARD_INTERFACES: &str = r#"  <interface name="org.freedesktop.DBus.Introspectable">
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

/// `/org/freedesktop/portal/desktop`: the file chooser, as
/// `org.freedesktop.impl.portal.FileChooser.xml` describes it, plus its
/// version property and the standard interfaces.
const PORTAL_INTERFACES: &str = r#"  <interface name="org.freedesktop.impl.portal.FileChooser">
    <method name="OpenFile">
      <arg type="o" name="handle" direction="in"/>
      <arg type="s" name="app_id" direction="in"/>
      <arg type="s" name="parent_window" direction="in"/>
      <arg type="s" name="title" direction="in"/>
      <arg type="a{sv}" name="options" direction="in"/>
      <arg type="u" name="response" direction="out"/>
      <arg type="a{sv}" name="results" direction="out"/>
    </method>
    <method name="SaveFile">
      <arg type="o" name="handle" direction="in"/>
      <arg type="s" name="app_id" direction="in"/>
      <arg type="s" name="parent_window" direction="in"/>
      <arg type="s" name="title" direction="in"/>
      <arg type="a{sv}" name="options" direction="in"/>
      <arg type="u" name="response" direction="out"/>
      <arg type="a{sv}" name="results" direction="out"/>
    </method>
    <method name="SaveFiles">
      <arg type="o" name="handle" direction="in"/>
      <arg type="s" name="app_id" direction="in"/>
      <arg type="s" name="parent_window" direction="in"/>
      <arg type="s" name="title" direction="in"/>
      <arg type="a{sv}" name="options" direction="in"/>
      <arg type="u" name="response" direction="out"/>
      <arg type="a{sv}" name="results" direction="out"/>
    </method>
    <property name="version" type="u" access="read"/>
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

/// An open request's handle: `org.freedesktop.impl.portal.Request.xml`.
const REQUEST_INTERFACES: &str = r#"  <interface name="org.freedesktop.impl.portal.Request">
    <method name="Close"/>
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

    #[test]
    fn introspection_walks_down_to_the_portal_and_its_requests() {
        let known = [OBJECT_PATH, "/org/freedesktop/portal/desktop/request/1_7/t"];
        let children = |path| children_of(path, known.iter().copied());
        assert_eq!(children("/"), BTreeSet::from(["org".to_string()]));
        assert_eq!(
            children("/org/freedesktop/portal"),
            BTreeSet::from(["desktop".to_string()])
        );
        assert_eq!(
            children(OBJECT_PATH),
            BTreeSet::from(["request".to_string()])
        );
        assert!(children("/org/freedesktop/portal/desktop/request/1_7/t").is_empty());
        // A sibling that merely shares a prefix is not a child.
        assert!(children("/org/free").is_empty());
    }

    /// A service over a socket nobody is at: enough to call `answer` on,
    /// which returns its replies rather than sending them.
    fn service() -> Service {
        let (ours, _theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let (_inbox, outbox) = Bus::on_socket(ours).into_service().unwrap();
        Service::new(outbox, Setup::default())
    }

    fn introspect(service: &mut Service, path: &str) -> Message {
        let call = Message {
            serial: 1,
            ..Message::method_call(BUS_NAME, path, INTROSPECTABLE, "Introspect")
        };
        service.answer(&call).unwrap()
    }

    fn xml(reply: &Message) -> String {
        match reply.args().unwrap().as_slice() {
            [Value::Str(xml)] => xml.clone(),
            other => panic!("not introspection: {other:?}"),
        }
    }

    /// `busctl tree` walks from `/` down to the FileManager1 object as it
    /// does to the portal's, and finds there the three methods and the
    /// standard interfaces.
    #[test]
    fn introspection_walks_down_to_the_file_manager_object() {
        let mut service = service();
        assert!(xml(&introspect(&mut service, "/")).contains("<node name=\"org\"/>"));
        let freedesktop = xml(&introspect(&mut service, "/org/freedesktop"));
        assert!(
            freedesktop.contains("<node name=\"FileManager1\"/>"),
            "{freedesktop}"
        );
        assert!(
            freedesktop.contains("<node name=\"portal\"/>"),
            "{freedesktop}"
        );

        let object = xml(&introspect(&mut service, show::OBJECT_PATH));
        assert!(object.contains("<interface name=\"org.freedesktop.FileManager1\">"));
        for method in ["ShowItems", "ShowFolders", "ShowItemProperties"] {
            assert!(
                object.contains(&format!("<method name=\"{method}\">")),
                "{method}"
            );
        }
        for standard in [PROPERTIES, INTROSPECTABLE, PEER] {
            assert!(
                object.contains(&format!("<interface name=\"{standard}\">")),
                "{standard}"
            );
        }
        assert!(!object.contains("<node name="), "a leaf: {object}");
        // Nothing of the file chooser's is there, nor of it at the portal's.
        assert!(!object.contains(FILE_CHOOSER));
        assert!(!xml(&introspect(&mut service, OBJECT_PATH)).contains(show::INTERFACE));
        let below = introspect(&mut service, "/org/freedesktop/FileManager1/x");
        assert_eq!(below.error_name.as_deref(), Some(UNKNOWN_OBJECT));
    }

    /// The three methods reach [`show`] at the FileManager1 object — with or
    /// without an interface — and nowhere else; the standard interfaces
    /// answer there as they do at the portal's.
    #[test]
    fn show_in_folder_is_answered_at_its_own_path() {
        let mut service = service();
        let show_items = |path: &str, interface: Option<&str>| {
            let mut call = Message {
                serial: 8,
                sender: Some(":1.9".into()),
                ..Message::method_call(show::BUS_NAME, path, show::INTERFACE, "ShowItems")
            }
            .with_args(
                "ass",
                &[
                    Value::Array(vec![Value::Str("file:///tmp/x".into())]),
                    Value::Str(String::new()),
                ],
            )
            .unwrap();
            call.interface = interface.map(str::to_string);
            call
        };
        for interface in [Some(show::INTERFACE), None] {
            let reply = service
                .answer(&show_items(show::OBJECT_PATH, interface))
                .unwrap();
            assert_eq!(reply.kind, crate::dbus::MSG_METHOD_RETURN, "{interface:?}");
            assert!(reply.body.is_empty());
        }
        let elsewhere = service
            .answer(&show_items(OBJECT_PATH, Some(show::INTERFACE)))
            .unwrap();
        assert_eq!(elsewhere.error_name.as_deref(), Some(UNKNOWN_METHOD));
        let wrong_interface = service
            .answer(&show_items(show::OBJECT_PATH, Some(FILE_CHOOSER)))
            .unwrap();
        assert_eq!(wrong_interface.error_name.as_deref(), Some(UNKNOWN_METHOD));

        // A bad body is the caller's error, and the service goes on.
        let mut bad = show_items(show::OBJECT_PATH, None);
        bad.signature = Some("as".into());
        bad.body = crate::dbus::marshal_body("as", &[Value::Array(Vec::new())]).unwrap();
        assert_eq!(
            service.answer(&bad).unwrap().error_name.as_deref(),
            Some(INVALID_ARGS)
        );

        let get_all = Message {
            serial: 9,
            ..Message::method_call(show::BUS_NAME, show::OBJECT_PATH, PROPERTIES, "GetAll")
        }
        .with_args("s", &[Value::Str(show::INTERFACE.into())])
        .unwrap();
        assert_eq!(
            service.answer(&get_all).unwrap().args().unwrap(),
            vec![Value::Dict(vec![])]
        );
        let ping = Message {
            serial: 10,
            ..Message::method_call(show::BUS_NAME, show::OBJECT_PATH, PEER, "Ping")
        };
        assert_eq!(
            service.answer(&ping).unwrap().kind,
            crate::dbus::MSG_METHOD_RETURN
        );
    }

    /// The answer's shape: `uris` only on success, and an empty results
    /// dictionary for a cancel or a failure.
    #[test]
    fn a_reply_carries_uris_only_when_something_was_picked() {
        let call = Message {
            serial: 5,
            sender: Some(":1.3".into()),
            ..Message::method_call(BUS_NAME, OBJECT_PATH, FILE_CHOOSER, "OpenFile")
        };
        let picked = Answer {
            response: request::SUCCESS,
            uris: vec!["file:///a".into()],
        };
        let reply = reply_to(&call, &picked).unwrap();
        assert_eq!(reply.reply_serial, Some(5));
        assert_eq!(
            reply.args().unwrap(),
            vec![
                Value::U32(0),
                Value::Dict(vec![(
                    Value::Str("uris".into()),
                    Value::variant("as", Value::Array(vec![Value::Str("file:///a".into())]))
                )])
            ]
        );
        let cancelled = reply_to(&call, &Answer::cancelled()).unwrap();
        assert_eq!(
            cancelled.args().unwrap(),
            vec![Value::U32(1), Value::Dict(vec![])]
        );
    }

    #[test]
    fn the_version_property_is_4_and_read_only() {
        let get = Message {
            serial: 1,
            ..Message::method_call(BUS_NAME, OBJECT_PATH, PROPERTIES, "Get")
        }
        .with_args(
            "ss",
            &[
                Value::Str(FILE_CHOOSER.into()),
                Value::Str("version".into()),
            ],
        )
        .unwrap();
        let reply = properties(&get, "Get").unwrap();
        assert_eq!(
            reply.args().unwrap(),
            vec![Value::variant("u", Value::U32(4))]
        );

        let get_all = Message {
            serial: 2,
            ..Message::method_call(BUS_NAME, OBJECT_PATH, PROPERTIES, "GetAll")
        }
        .with_args("s", &[Value::Str(FILE_CHOOSER.into())])
        .unwrap();
        let reply = properties(&get_all, "GetAll").unwrap();
        assert_eq!(
            reply.args().unwrap()[0],
            Value::Dict(vec![(
                Value::Str("version".into()),
                Value::variant("u", Value::U32(4))
            )])
        );

        let set = Message {
            serial: 3,
            ..Message::method_call(BUS_NAME, OBJECT_PATH, PROPERTIES, "Set")
        }
        .with_args(
            "ssv",
            &[
                Value::Str(FILE_CHOOSER.into()),
                Value::Str("version".into()),
                Value::variant("u", Value::U32(9)),
            ],
        )
        .unwrap();
        let reply = properties(&set, "Set").unwrap();
        assert_eq!(reply.error_name.as_deref(), Some(PROPERTY_READ_ONLY));

        // The wrong arguments are refused, not guessed at.
        let bad = Message {
            serial: 4,
            ..Message::method_call(BUS_NAME, OBJECT_PATH, PROPERTIES, "Get")
        }
        .with_args("s", &[Value::Str(FILE_CHOOSER.into())])
        .unwrap();
        let reply = properties(&bad, "Get").unwrap();
        assert_eq!(reply.error_name.as_deref(), Some(INVALID_ARGS));
    }
}
