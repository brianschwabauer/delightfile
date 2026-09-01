//! The mount manager — PLAN §7.4's "`M`: udisks2 over hand-rolled D-Bus —
//! list/mount/unmount/eject".
//!
//! ## Why udisks2 and not `/proc/mounts`
//!
//! Reading `/proc/mounts` would list what is mounted; it would not list the USB
//! stick that is plugged in and *not* mounted, which is the whole reason to
//! press `M`. udisks2 knows about block devices before anything has been done
//! with them, and it is the service that can mount one without root — polkit
//! grants a logged-in user permission for their own removable media, which is
//! why this works at all and why `mount(8)` would not.
//!
//! ## The shape of the conversation
//!
//! One call gets everything: `ObjectManager.GetManagedObjects` on
//! `/org/freedesktop/UDisks2` returns every object udisks2 knows with every
//! interface and every property. A *device* in this file is a block object that
//! has both a `Filesystem` interface (so it can be mounted) and a `Drive` (so
//! there is real hardware behind it, which filters out loop devices and
//! ramdisks). [`devices_from`] is that filter and it is a pure function, so the
//! rules are a test over a hand-built reply rather than something that needs a
//! machine with a USB stick in it.
//!
//! Acting is three more calls, each taking an empty options dictionary:
//! `Filesystem.Mount`, `Filesystem.Unmount`, `Drive.Eject`.
//!
//! ## Every one of them blocks
//!
//! Mounting waits for the filesystem; unmounting waits for the writeback; and
//! any of them may sit in a polkit prompt for as long as the user takes to type
//! a password. So the bus lives on a worker thread ([`Mounts`]), the event loop
//! only ever sends it a request and reads its answers, and a device that is
//! being acted on is drawn as busy rather than the window being unresponsive.
//!
//! ## When udisks2 is not there
//!
//! A notice, once, and no card. A machine without udisks2 is a machine without
//! removable disks to manage, not a machine with an error in it — the same rule
//! [`df_core::git`] follows for a machine without git.

use std::path::PathBuf;


use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::dbus::{Bus, Interfaces, Value};

/// The udisks2 names, in one place.
const SERVICE: &str = "org.freedesktop.UDisks2";
const MANAGER_PATH: &str = "/org/freedesktop/UDisks2";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";

/// How many rows the card shows before it scrolls. A machine with more than
/// this many mountable filesystems is a server, and a server is not what `M` is
/// for.
pub const ROWS: usize = 10;

/// One mountable filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The block object's path — what `Mount` and `Unmount` are called on.
    pub object: String,
    /// The drive object's path, for `Eject`. `"/"` means "no drive", which is
    /// how udisks2 spells it and which this type stores as `None`.
    pub drive: Option<String>,
    /// `/dev/sda1`.
    pub node: String,
    /// The filesystem label, or the device node's last component when it has
    /// none — a row has to be *called* something.
    pub label: String,
    /// `ext4`, `vfat`, `ntfs`… Empty when udisks2 could not tell.
    pub fs: String,
    pub size: u64,
    /// Where it is mounted, or `None`. The first mount point only: a filesystem
    /// bind-mounted in four places is still one row, and the first is the one
    /// udisks2 made.
    pub mount: Option<PathBuf>,
    pub removable: bool,
    pub ejectable: bool,
    /// "SanDisk Cruzer" — the hardware, for the second line of the row.
    pub hardware: String,
}

impl Device {
    pub fn is_mounted(&self) -> bool {
        self.mount.is_some()
    }

    /// The right-hand text: where it is, or what it is.
    pub fn status(&self) -> String {
        match &self.mount {
            Some(path) => path.to_string_lossy().into_owned(),
            None => "not mounted".to_string(),
        }
    }

    /// The dim second line: size, filesystem, device node, and whether it comes
    /// out.
    pub fn detail(&self) -> String {
        let mut parts = vec![crate::format::human_size(self.size)];
        if !self.fs.is_empty() {
            parts.push(self.fs.clone());
        }
        parts.push(self.node.clone());
        if !self.hardware.is_empty() {
            parts.push(self.hardware.clone());
        }
        if self.removable {
            parts.push("removable".to_string());
        }
        parts.join(" · ")
    }
}

/// Turn a `GetManagedObjects` reply into the rows the card shows.
///
/// The filter, and why each half of it is there:
///
/// - **A `Filesystem` interface**, because a row that cannot be mounted is a row
///   whose `Enter` does nothing. That drops the whole-disk objects (`/dev/sda`
///   as opposed to `/dev/sda1`), swap partitions, and unformatted space.
/// - **A `Drive`**, because everything else is a loop device, a ramdisk or a
///   device-mapper node — real to the kernel and not what anybody means by
///   "the disks".
/// - **`HintIgnore` false**, which is udisks2's own "do not show this to a
///   person" flag, set for things like the EFI system partition on some setups.
///
/// Sorted removable-first and then by label, because the answer to `M` is
/// almost always the USB stick that was just plugged in.
pub fn devices_from(objects: &[(String, Interfaces)]) -> Vec<Device> {
    let drives: std::collections::HashMap<&str, &Interfaces> = objects
        .iter()
        .filter(|(_, interfaces)| interfaces.contains_key(DRIVE))
        .map(|(path, interfaces)| (path.as_str(), interfaces))
        .collect();

    let mut out: Vec<Device> = Vec::new();
    for (path, interfaces) in objects {
        let (Some(block), Some(filesystem)) =
            (interfaces.get(BLOCK), interfaces.get(FILESYSTEM))
        else {
            continue;
        };
        let get = |props: &std::collections::HashMap<String, Value>, key: &str| {
            props.get(key).cloned()
        };
        if get(block, "HintIgnore")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let drive_path = get(block, "Drive")
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|path| path != "/");
        let Some(drive_path) = drive_path else {
            continue;
        };
        let drive = drives.get(drive_path.as_str()).and_then(|i| i.get(DRIVE));

        let node = get(block, "Device")
            .and_then(|v| v.as_bytestring())
            .unwrap_or_default();
        let label = get(block, "IdLabel")
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| {
                // No label: the device node's last component, which is what
                // every other tool falls back to and what the user will
                // recognise from `lsblk`.
                node.rsplit('/')
                    .next()
                    .unwrap_or("disk")
                    .to_string()
            });
        let mount = get(filesystem, "MountPoints")
            .and_then(|v| v.as_bytestrings())
            .unwrap_or_default()
            .into_iter()
            .find(|m| !m.is_empty())
            .map(PathBuf::from);

        let drive_str = |key: &str| {
            drive
                .and_then(|props| props.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let hardware = [drive_str("Vendor"), drive_str("Model")]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");

        out.push(Device {
            object: path.clone(),
            drive: Some(drive_path),
            node,
            label,
            fs: get(block, "IdType")
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            size: get(block, "Size").and_then(|v| v.as_u64()).unwrap_or(0),
            mount,
            removable: drive
                .and_then(|props| props.get("Removable"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            ejectable: drive
                .and_then(|props| props.get("Ejectable"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            hardware,
        });
    }
    out.sort_by(|a, b| {
        b.removable
            .cmp(&a.removable)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
            .then_with(|| a.node.cmp(&b.node))
    });
    out
}

/// What the event loop asks the worker to do.
#[derive(Debug, Clone)]
pub enum Request {
    List,
    Mount(String),
    Unmount(String),
    Eject(String),
}

/// What comes back.
#[derive(Debug, Clone)]
pub enum Reply {
    Devices(Vec<Device>),
    /// A mount landed, and this is where. The card cds there on the next
    /// `Enter`.
    Mounted(PathBuf),
    Unmounted,
    Ejected,
    /// Something failed, already turned into a sentence by
    /// [`crate::dbus::readable_error`].
    Failed(String),
}

/// The worker, and the two channels either side of it.
///
/// Dropping it closes the request queue and joins the thread, so a test — and a
/// closed card — cannot outlive its connection.
pub struct Mounts {
    requests: Option<Sender<Request>>,
    replies: Receiver<Reply>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Mounts {
    /// Start the worker. `notify` is rung once per reply.
    pub fn start(notify: df_core::fs::Notifier) -> Mounts {
        let (tx, rx) = unbounded::<Request>();
        let (reply_tx, reply_rx) = unbounded::<Reply>();
        let handle = std::thread::Builder::new()
            .name("df-udisks".to_string())
            .spawn(move || run(rx, reply_tx, notify));
        let worker = match handle {
            Ok(h) => Some(h),
            Err(e) => {
                log::warn!("the udisks2 worker did not start: {e}");
                None
            }
        };
        Mounts {
            requests: Some(tx),
            replies: reply_rx,
            worker,
        }
    }

    pub fn ask(&self, request: Request) {
        if let Some(requests) = &self.requests {
            let _ = requests.send(request);
        }
    }

    pub fn drain(&self) -> Vec<Reply> {
        self.replies.try_iter().collect()
    }
}

impl Drop for Mounts {
    fn drop(&mut self) {
        self.requests = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The worker loop.
///
/// The connection is opened lazily and kept: a card that is opened and closed
/// four times should not authenticate four times, and a connection that has
/// gone away is reopened on the next request rather than being an error the
/// user has to do something about.
fn run(requests: Receiver<Request>, replies: Sender<Reply>, notify: df_core::fs::Notifier) {
    let mut bus: Option<Bus> = None;
    for request in requests {
        if bus.is_none() {
            match Bus::connect() {
                Ok(connected) => bus = Some(connected),
                Err(e) => {
                    let _ = replies.send(Reply::Failed(e));
                    notify();
                    continue;
                }
            }
        }
        let Some(connection) = &mut bus else { continue };
        let reply = match handle(connection, &request) {
            Ok(reply) => reply,
            Err(e) => {
                // A broken connection is dropped so the next request reopens
                // it; a refusal is not, because the connection is fine.
                if e.contains("closed the connection") || e.contains("reading from") {
                    bus = None;
                }
                Reply::Failed(e)
            }
        };
        let _ = replies.send(reply);
        notify();
    }
}

fn handle(bus: &mut Bus, request: &Request) -> Result<Reply, String> {
    match request {
        Request::List => {
            let body = bus.call(
                SERVICE,
                MANAGER_PATH,
                OBJECT_MANAGER,
                "GetManagedObjects",
                None,
                &[],
            )?;
            let objects = crate::dbus::parse_managed_objects(&body)?;
            Ok(Reply::Devices(devices_from(&objects)))
        }
        Request::Mount(object) => {
            let mut args = Vec::new();
            crate::dbus::marshal_no_options(&mut args);
            let body = bus.call(SERVICE, object, FILESYSTEM, "Mount", Some("a{sv}"), &args)?;
            let path = crate::dbus::Reader::new(&body).string()?;
            Ok(Reply::Mounted(PathBuf::from(path)))
        }
        Request::Unmount(object) => {
            let mut args = Vec::new();
            crate::dbus::marshal_no_options(&mut args);
            bus.call(SERVICE, object, FILESYSTEM, "Unmount", Some("a{sv}"), &args)?;
            Ok(Reply::Unmounted)
        }
        Request::Eject(drive) => {
            let mut args = Vec::new();
            crate::dbus::marshal_no_options(&mut args);
            bus.call(SERVICE, drive, DRIVE, "Eject", Some("a{sv}"), &args)?;
            Ok(Reply::Ejected)
        }
    }
}

/// The card's own state, while it is open.
pub struct Card {
    pub devices: Vec<Device>,
    pub cursor: usize,
    pub first: usize,
    /// A call is in flight, so the card is drawn busy and takes no new one.
    /// One at a time: two mounts of the same device is one of them failing with
    /// `AlreadyMounted`, and a card that let you start it is a card that
    /// produced an error you caused by being allowed to.
    pub busy: Option<String>,
    /// Nothing has come back yet.
    pub loading: bool,
}

impl Card {
    pub fn new() -> Card {
        Card {
            devices: Vec::new(),
            cursor: 0,
            first: 0,
            busy: None,
            loading: true,
        }
    }

    pub fn selected(&self) -> Option<&Device> {
        self.devices.get(self.cursor)
    }

    /// `↑`/`↓`, clamping. A list of disks has a top and a bottom, and running
    /// off the end of it would be the cursor landing on a different disk from
    /// the one the eye is on.
    pub fn move_cursor(&mut self, delta: isize) {
        if self.devices.is_empty() {
            self.cursor = 0;
            return;
        }
        let last = self.devices.len() - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last as isize) as usize;
        self.first =
            crate::viewport::first_visible(self.first, self.cursor, self.devices.len(), ROWS, 1);
    }

    /// Take a new listing without losing the user's place.
    ///
    /// Keyed on the object path, so a refresh that arrives after a mount leaves
    /// the cursor on the disk that was just mounted rather than on whatever now
    /// sorts into that row (`delightful-ui` §8).
    pub fn update(&mut self, devices: Vec<Device>) {
        let on = self.selected().map(|device| device.object.clone());
        self.devices = devices;
        self.loading = false;
        self.cursor = on
            .and_then(|object| self.devices.iter().position(|d| d.object == object))
            .unwrap_or(self.cursor)
            .min(self.devices.len().saturating_sub(1));
        self.first =
            crate::viewport::first_visible(self.first, self.cursor, self.devices.len(), ROWS, 1);
    }

    /// What the empty state says, or `None` when there are rows to draw.
    ///
    /// Three different nothings, and they must not look alike
    /// (`delightful-ui` §11): still asking, nothing to manage, and a card that
    /// is between listings.
    pub fn empty_message(&self) -> Option<&'static str> {
        if !self.devices.is_empty() {
            return None;
        }
        Some(if self.loading {
            "asking udisks2…"
        } else {
            "no removable filesystems"
        })
    }
}

// ── The card ────────────────────────────────────────────────────────────────

/// One device's row. Two lines: the name and where it is, then the hardware.
const ROW: f32 = 34.0;
const PAD: f32 = 14.0;
const TITLE: f32 = 20.0;
const MAX_WIDTH: f32 = 560.0;
const FONT: f32 = 13.0;

/// Where the card's pieces are.
#[derive(Debug, Clone)]
pub struct Geometry {
    pub card: egui::Rect,
    pub body: egui::Rect,
    pub rows: Vec<egui::Rect>,
}

impl Geometry {
    pub fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.rows.iter().position(|r| r.contains(pos))
    }
}

/// Lay the card out, centred and biased above true centre
/// (`delightful-ui` §16).
pub fn geometry(area: egui::Rect, card: &Card) -> Geometry {
    let visible = card.devices.len().saturating_sub(card.first).clamp(1, ROWS);
    let height = PAD * 2.0 + TITLE * 2.0 + 6.0 + visible as f32 * ROW;
    let width = (area.width() - 40.0).clamp(0.0, MAX_WIDTH);
    let height = height.min((area.height() - 40.0).max(0.0));
    let top = area.top() + (area.height() - height).max(0.0) * 0.4;
    let rect = egui::Rect::from_min_size(
        egui::pos2(area.center().x - width / 2.0, top),
        egui::vec2(width, height),
    );
    let body_top = rect.top() + PAD + TITLE * 2.0 + 6.0;
    let body = egui::Rect::from_min_max(
        egui::pos2(rect.left() + PAD, body_top),
        egui::pos2(rect.right() - PAD, rect.bottom() - PAD),
    );
    let rows = (0..card.devices.len().saturating_sub(card.first).min(ROWS))
        .map(|i| {
            egui::Rect::from_min_size(
                egui::pos2(body.left(), body_top + i as f32 * ROW),
                egui::vec2(body.width(), ROW),
            )
        })
        .collect();
    Geometry { card: rect, body, rows }
}

/// Draw it.
pub fn paint(
    paint: &crate::ui::Painting<'_>,
    area: egui::Rect,
    card: &Card,
    geometry: &Geometry,
    hovers: &crate::hover::Hovers<crate::ui::Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    painter.rect_filled(area, 0, egui::Color32::from_black_alpha(crate::chrome::HELP_SCRIM));
    crate::chrome::card(paint, geometry.card, 1.0);

    let left = geometry.card.left() + PAD;
    painter.text(
        egui::pos2(left, geometry.card.top() + PAD + TITLE / 2.0),
        egui::Align2::LEFT_CENTER,
        "Disks",
        egui::FontId::proportional(FONT + 2.0),
        palette.text,
    );
    // The keys, on the card rather than in a help sheet: this surface has three
    // of them and they are not guessable from the rows.
    painter.text(
        egui::pos2(geometry.card.right() - PAD, geometry.card.top() + PAD + TITLE / 2.0),
        egui::Align2::RIGHT_CENTER,
        "Enter mount / open · e eject · Esc close",
        egui::FontId::proportional(FONT - 1.5),
        palette.overlay0,
    );

    if let Some(message) = card.empty_message() {
        painter.text(
            geometry.body.center(),
            egui::Align2::CENTER_CENTER,
            message,
            egui::FontId::proportional(FONT),
            palette.overlay0,
        );
        return;
    }

    let clipped = painter.with_clip_rect(geometry.body);
    for (i, rect) in geometry.rows.iter().enumerate() {
        let index = card.first + i;
        let Some(device) = card.devices.get(index) else {
            break;
        };
        let on_cursor = index == card.cursor;
        let hover = hovers.hover(crate::ui::Control::PanelRow(i));
        if on_cursor || hover > 0.0 {
            clipped.rect_filled(
                rect.shrink2(egui::vec2(0.0, 2.0)),
                crate::ui::ROW_RADIUS,
                crate::theme::mix(
                    palette.crust,
                    palette.surface1,
                    if on_cursor { 1.0 } else { hover * 0.6 },
                ),
            );
        }
        // A mounted device is the palette's own "this is live" colour; an
        // unmounted one is plain text. The busy one is amber and says so, so a
        // polkit prompt behind the window is not read as the card having hung.
        let busy = card.busy.as_deref() == Some(device.object.as_str());
        let accent = if busy {
            palette.peach
        } else if device.is_mounted() {
            palette.green
        } else {
            palette.overlay1
        };
        clipped.text(
            egui::pos2(rect.left() + 10.0, rect.top() + 10.0),
            egui::Align2::LEFT_TOP,
            &device.label,
            egui::FontId::proportional(FONT),
            palette.text,
        );
        crate::chrome::truncated(
            &clipped,
            egui::pos2(rect.left() + 10.0, rect.bottom() - 10.0),
            &device.detail(),
            palette.overlay0,
            (rect.width() * 0.62).max(0.0),
        );
        crate::chrome::truncated(
            &clipped,
            egui::pos2(rect.left() + rect.width() * 0.64, rect.center().y),
            &if busy {
                "working…".to_string()
            } else {
                device.status()
            },
            accent,
            (rect.width() * 0.36 - 10.0).max(0.0),
        );
    }
    if card.devices.len() > geometry.rows.len() {
        let more = card.devices.len() - geometry.rows.len() - card.first;
        if more > 0 {
            painter.text(
                egui::pos2(geometry.body.right(), geometry.body.bottom()),
                egui::Align2::RIGHT_BOTTOM,
                format!("+{more} more"),
                egui::FontId::proportional(FONT - 1.0),
                palette.overlay0,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn props(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn bytes(text: &str) -> Value {
        Value::Array(text.bytes().map(Value::U8).chain([Value::U8(0)]).collect())
    }

    fn objects() -> Vec<(String, Interfaces)> {
        let drive: Interfaces = [(
            DRIVE.to_string(),
            props(&[
                ("Removable", Value::Bool(true)),
                ("Ejectable", Value::Bool(true)),
                ("Vendor", Value::Str("SanDisk".into())),
                ("Model", Value::Str("Cruzer".into())),
            ]),
        )]
        .into_iter()
        .collect();

        let usb: Interfaces = [
            (
                BLOCK.to_string(),
                props(&[
                    ("Device", bytes("/dev/sdb1")),
                    ("IdLabel", Value::Str("PHOTOS".into())),
                    ("IdType", Value::Str("vfat".into())),
                    ("Size", Value::U64(16_000_000_000)),
                    ("Drive", Value::Path("/drives/usb".into())),
                ]),
            ),
            (
                FILESYSTEM.to_string(),
                props(&[("MountPoints", Value::Array(vec![]))]),
            ),
        ]
        .into_iter()
        .collect();

        let internal_drive: Interfaces = [(
            DRIVE.to_string(),
            props(&[("Removable", Value::Bool(false))]),
        )]
        .into_iter()
        .collect();

        let root: Interfaces = [
            (
                BLOCK.to_string(),
                props(&[
                    ("Device", bytes("/dev/nvme0n1p2")),
                    ("IdLabel", Value::Str("".into())),
                    ("IdType", Value::Str("ext4".into())),
                    ("Size", Value::U64(500_000_000_000)),
                    ("Drive", Value::Path("/drives/nvme".into())),
                ]),
            ),
            (
                FILESYSTEM.to_string(),
                props(&[("MountPoints", Value::Array(vec![bytes("/")]))]),
            ),
        ]
        .into_iter()
        .collect();

        // A loop device: a filesystem with no drive behind it.
        let loop_dev: Interfaces = [
            (
                BLOCK.to_string(),
                props(&[
                    ("Device", bytes("/dev/loop0")),
                    ("Drive", Value::Path("/".into())),
                ]),
            ),
            (
                FILESYSTEM.to_string(),
                props(&[("MountPoints", Value::Array(vec![]))]),
            ),
        ]
        .into_iter()
        .collect();

        // The whole disk, which has no filesystem of its own.
        let whole: Interfaces = [(
            BLOCK.to_string(),
            props(&[
                ("Device", bytes("/dev/sdb")),
                ("Drive", Value::Path("/drives/usb".into())),
            ]),
        )]
        .into_iter()
        .collect();

        // Something udisks2 itself says not to show.
        let hidden: Interfaces = [
            (
                BLOCK.to_string(),
                props(&[
                    ("Device", bytes("/dev/nvme0n1p1")),
                    ("Drive", Value::Path("/drives/nvme".into())),
                    ("HintIgnore", Value::Bool(true)),
                ]),
            ),
            (
                FILESYSTEM.to_string(),
                props(&[("MountPoints", Value::Array(vec![]))]),
            ),
        ]
        .into_iter()
        .collect();

        vec![
            ("/drives/usb".to_string(), drive),
            ("/drives/nvme".to_string(), internal_drive),
            ("/block/nvme0n1p2".to_string(), root),
            ("/block/sdb1".to_string(), usb),
            ("/block/loop0".to_string(), loop_dev),
            ("/block/sdb".to_string(), whole),
            ("/block/nvme0n1p1".to_string(), hidden),
        ]
    }

    /// The filter, in one test: what is a row, what is not, and why.
    #[test]
    fn only_mountable_filesystems_on_real_drives_become_rows() {
        let devices = devices_from(&objects());
        let nodes: Vec<&str> = devices.iter().map(|d| d.node.as_str()).collect();
        assert_eq!(
            nodes,
            vec!["/dev/sdb1", "/dev/nvme0n1p2"],
            "removable first, and nothing else got in"
        );
        // The loop device has no drive, the whole disk has no filesystem, and
        // the EFI partition asked not to be shown.
        assert!(!nodes.contains(&"/dev/loop0"));
        assert!(!nodes.contains(&"/dev/sdb"));
        assert!(!nodes.contains(&"/dev/nvme0n1p1"));
    }

    /// Every fact a row shows, taken off the right interface.
    #[test]
    fn a_device_carries_what_the_row_needs() {
        let devices = devices_from(&objects());
        let usb = &devices[0];
        assert_eq!(usb.label, "PHOTOS");
        assert_eq!(usb.fs, "vfat");
        assert_eq!(usb.size, 16_000_000_000);
        assert!(usb.removable && usb.ejectable);
        assert_eq!(usb.hardware, "SanDisk Cruzer");
        assert!(!usb.is_mounted());
        assert_eq!(usb.status(), "not mounted");
        assert!(usb.detail().contains("vfat"));
        assert!(usb.detail().contains("/dev/sdb1"));
        assert!(usb.detail().contains("removable"));
        assert_eq!(usb.drive.as_deref(), Some("/drives/usb"));

        // No label: the device node's last component, which is what `lsblk`
        // would have shown.
        let root = &devices[1];
        assert_eq!(root.label, "nvme0n1p2");
        assert!(root.is_mounted());
        assert_eq!(root.status(), "/");
        assert!(!root.removable);
        assert!(!root.ejectable, "an unstated flag is false, not true");
        assert_eq!(root.hardware, "", "a drive with no vendor says nothing");
    }

    /// The card lays out and paints in every state without panicking.
    #[test]
    fn the_card_paints_in_every_state() {
        let theme = df_core::config::Theme::default();
        let palette = crate::theme::Palette::from_theme(&theme);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let painting = crate::ui::Painting {
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd: false,
                show_symlink: true,
                now: std::time::Instant::now(),
            };
            let hovers = crate::hover::Hovers::new();
            for area in [
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0)),
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 90.0)),
            ] {
                // Still loading, empty, populated, scrolled, and busy.
                let mut card = Card::new();
                paint(&painting, area, &card, &geometry(area, &card), &hovers);
                card.update(Vec::new());
                paint(&painting, area, &card, &geometry(area, &card), &hovers);

                let many: Vec<Device> = (0..20)
                    .map(|i| Device {
                        object: format!("/block/{i}"),
                        drive: Some("/drives/x".into()),
                        node: format!("/dev/sd{i}"),
                        label: format!("Disk {i}"),
                        fs: "ext4".into(),
                        size: 1 << 30,
                        mount: (i % 2 == 0).then(|| PathBuf::from("/run/media/x")),
                        removable: i % 3 == 0,
                        ejectable: true,
                        hardware: "Acme Widget".into(),
                    })
                    .collect();
                card.update(many);
                paint(&painting, area, &card, &geometry(area, &card), &hovers);
                card.first = 15;
                card.busy = Some("/block/16".to_string());
                paint(&painting, area, &card, &geometry(area, &card), &hovers);
            }
        });
    }

    /// The geometry caps at [`ROWS`] and hit-tests to the row it drew.
    #[test]
    fn the_card_shows_a_windowful_and_hit_tests_it() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let mut card = Card::new();
        card.update(devices_from(&objects()));
        let g = geometry(area, &card);
        assert_eq!(g.rows.len(), 2);
        for (i, rect) in g.rows.iter().enumerate() {
            assert_eq!(g.row_at(rect.center()), Some(i));
            assert!(g.body.contains_rect(*rect));
        }
        assert!(g.row_at(egui::pos2(0.0, 0.0)).is_none());
        // An empty card still has a body to write the empty state into.
        card.update(Vec::new());
        let g = geometry(area, &card);
        assert!(g.rows.is_empty());
        assert!(g.body.is_positive());
    }

    /// An empty reply is a normal machine, not a failure.
    #[test]
    fn a_machine_with_no_disks_produces_no_rows() {
        assert!(devices_from(&[]).is_empty());
    }

    /// The cursor clamps, and a refresh keeps it on the disk it was on.
    #[test]
    fn the_cursor_holds_its_place_across_a_refresh() {
        let mut card = Card::new();
        assert_eq!(card.empty_message(), Some("asking udisks2…"));
        card.update(devices_from(&objects()));
        assert_eq!(card.empty_message(), None);
        assert_eq!(card.cursor, 0);

        card.move_cursor(1);
        assert_eq!(card.selected().map(|d| d.node.as_str()), Some("/dev/nvme0n1p2"));
        // Clamped, not wrapped: a list of disks has a bottom.
        card.move_cursor(5);
        assert_eq!(card.cursor, 1);
        card.move_cursor(-9);
        assert_eq!(card.cursor, 0);

        // The USB stick is now mounted and sorts the same way; the cursor is on
        // it before and after.
        card.move_cursor(1);
        let on = card.selected().map(|d| d.object.clone());
        let mut again = devices_from(&objects());
        again.reverse();
        card.update(again);
        assert_eq!(card.selected().map(|d| d.object.clone()), on);

        // An empty listing leaves nothing selected and does not panic.
        card.update(Vec::new());
        assert!(card.selected().is_none());
        assert_eq!(card.empty_message(), Some("no removable filesystems"));
    }
}
