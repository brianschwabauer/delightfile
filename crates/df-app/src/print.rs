//! Printing a file: the PDF the print dialog is handed, and one file's turn
//! through that dialog (`builtin:print`, `Command::Print`).
//!
//! The desktop's print dialog ([`crate::platform::print`]) prints a PDF and
//! nothing else, so printing anything starts with a PDF of it. There are
//! three routes to one, chosen by what the bytes are rather than by what the
//! name says ([`route`]):
//!
//! - **A PDF** — `%PDF-` at the top, or a file the preview would page through
//!   as one — is printed as it is, and left alone afterwards ([`Pdf::Itself`]).
//! - **A picture this build decodes** (JPEG, PNG, WebP and GIF, the `image`
//!   codecs the workspace already compiles) becomes a one-page PDF written
//!   here, with no library: a catalog, a page, the picture, the line that
//!   places it, and the cross-reference table that says where each of them
//!   starts ([`picture_pdf`]). A JPEG whose decoder every PDF reader has —
//!   baseline or progressive, grey or YCbCr — goes in as the file's own bytes
//!   under `/DCTDecode`, so nothing is decoded or compressed again; anything
//!   else is decoded, laid over white where it is see-through, and deflated.
//!   The page is US Letter in a US locale and A4 everywhere else ([`paper_for`]),
//!   turned on its side for a picture wider than it is tall, with half an inch
//!   all round, and the picture is fitted into what is left, larger or
//!   smaller — a JPEG the right way up by its EXIF orientation, which a phone
//!   writes instead of turning the pixels.
//! - **Everything else** — Word, PowerPoint and Excel, old and new,
//!   OpenDocument, RTF, text, markdown, source, and the pictures the build
//!   does not decode (HEIC, AVIF, TIFF, SVG) — is LibreOffice's:
//!   `soffice --headless --convert-to pdf`. LibreOffice is not something
//!   delightfile needs, in the way pdfium is not (`preview::doc::pdf`): where
//!   neither `soffice` nor `libreoffice` is on `PATH`, those files say what
//!   printing them takes, and nothing else changes.
//!
//! What a route makes is written under a folder of its own in [`scratch`],
//! and the caller removes it once it is printed ([`Pdf::discard`]).
//!
//! ## One file's turn
//!
//! [`print_one`] puts the dialog up at once, on a thread of its own, and
//! makes the PDF while it is up, so the two seconds LibreOffice takes are
//! spent while the person is still choosing a printer. Whichever finishes
//! last, the PDF goes to the printer under the dialog's answer. A cancelled
//! dialog stops the making; a cancelled task stops it too, closes the dialog
//! if it is still up, and nothing is sent; and a PDF that could not be made
//! closes the dialog, so the failure is said at once. Which route a file
//! takes is settled before the dialog is put up, so a file nothing here can
//! print says so instead of asking first.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use df_core::tasks::TaskCtx;

/// How long LibreOffice may take over one file before it is given up on.
/// A text file takes about two seconds on Brian's machine, the first of a
/// session included; two minutes is a file that is not going to finish.
pub const OFFICE_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a wait here looks up to see whether it has been cancelled.
const POLL: Duration = Duration::from_millis(25);

/// How long a LibreOffice asked to stop is given before it is killed. Asked
/// gently because `soffice` hands the work to a `soffice.bin` of its own,
/// which goes with it on `SIGTERM` and is left running by `SIGKILL`.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// The margin on every side of a picture's page, in points: half an inch,
/// which every printer reaches inside of.
pub const MARGIN: f64 = 36.0;

/// A PDF to print, and whose it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pdf {
    /// The file was a PDF already: printed as it is, and never removed.
    Itself(PathBuf),
    /// A PDF made of the file, in a folder of its own under the scratch
    /// folder: printed, then [`Pdf::discard`]ed.
    Made(PathBuf),
}

impl Pdf {
    pub fn path(&self) -> &Path {
        match self {
            Pdf::Itself(path) | Pdf::Made(path) => path,
        }
    }

    /// Remove a made PDF and the folder made for it. The file a PDF already
    /// was is not touched.
    pub fn discard(self) {
        if let Pdf::Made(path) = self {
            if let Err(e) = std::fs::remove_file(&path) {
                log::debug!("{}: {e}", path.display());
            }
            if let Some(dir) = path.parent() {
                // `remove_dir`, not `_all`: the folder is empty once the PDF
                // has gone, and one that is not was not left by us.
                let _ = std::fs::remove_dir(dir);
            }
        }
    }
}

/// Where this process makes its PDFs: a folder in the system's temporary
/// directory, named for the process, which quitting removes once it is empty.
pub fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!("delightfile-print-{}", std::process::id()))
}

/// A PDF of `path`, ready for the print dialog: `path` itself when it is one,
/// else one made under `scratch` (see the module header for the three
/// routes). `ctx` cancels the making; LibreOffice is stopped when it does.
///
/// The error is a sentence for a toast: "LibreOffice is needed to print
/// notes.md" where the route is LibreOffice's and there is none, and
/// "cancelled" ([`df_core::DfError::Cancelled`]) when `ctx` was.
pub fn pdf_for(path: &Path, scratch: &Path, ctx: &TaskCtx) -> Result<Pdf, String> {
    let search = std::env::var_os("PATH").unwrap_or_default();
    pdf_on(path, scratch, ctx, &search)
}

/// [`pdf_for`] with the `PATH` LibreOffice is looked for on.
fn pdf_on(path: &Path, scratch: &Path, ctx: &TaskCtx, search: &OsStr) -> Result<Pdf, String> {
    make(route(path, search)?, path, scratch, ctx)
}

/// How a file becomes a PDF.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// It is one.
    Itself,
    /// A picture this build decodes.
    Picture,
    /// LibreOffice's, found on `PATH` at this program.
    Office(PathBuf),
}

/// Which route `path` takes, from its first few kilobytes and its name —
/// and, for LibreOffice's, whether there is a LibreOffice on `search` (a
/// `PATH`) to take it. Cheap: [`print_one`] asks it before the dialog goes
/// up, and the making asks it again.
fn route(path: &Path, search: &OsStr) -> Result<Route, String> {
    let name = display_name(path);
    let head = head(path).map_err(|e| format!("{name}: {e}"))?;
    if head.is_empty() {
        return Err(format!("{name} is empty: there is nothing to print"));
    }
    if head.starts_with(b"%PDF-") {
        return Ok(Route::Itself);
    }
    let hint = df_core::fs::mime::hint_for_name(&name);
    let mime = df_core::preview::sniff_or_hint(&head, hint);
    if df_core::preview::kind_for_mime(mime, &name) == df_core::preview::PreviewKind::Pdf {
        return Ok(Route::Itself);
    }
    if matches!(
        image::guess_format(&head),
        Ok(image::ImageFormat::Jpeg
            | image::ImageFormat::Png
            | image::ImageFormat::WebP
            | image::ImageFormat::Gif)
    ) {
        return Ok(Route::Picture);
    }
    match office(search) {
        Some(program) => Ok(Route::Office(program)),
        None => Err(format!("LibreOffice is needed to print {name}")),
    }
}

/// The PDF `route` makes of `path`.
fn make(route: Route, path: &Path, scratch: &Path, ctx: &TaskCtx) -> Result<Pdf, String> {
    let name = display_name(path);
    match route {
        Route::Itself => Ok(Pdf::Itself(path.to_path_buf())),
        Route::Picture => {
            let bytes = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
            let page = picture_pdf(&bytes, paper_here()).map_err(|e| format!("{name}: {e}"))?;
            if ctx.is_cancelled() {
                return Err(df_core::DfError::Cancelled.to_string());
            }
            let dir = fresh_dir(scratch)?;
            let out = dir.join(pdf_name(&name));
            if let Err(e) = std::fs::write(&out, page) {
                let _ = std::fs::remove_file(&out);
                let _ = std::fs::remove_dir(&dir);
                return Err(format!("{}: {e}", out.display()));
            }
            Ok(Pdf::Made(out))
        }
        Route::Office(program) => {
            let dir = fresh_dir(scratch)?;
            match convert(&program, path, &dir, ctx) {
                Ok(pdf) => Ok(Pdf::Made(pdf)),
                Err(e) => {
                    // What LibreOffice left — a lock file, a half-written
                    // PDF — in a folder that was made for this one file.
                    let _ = std::fs::remove_dir_all(&dir);
                    Err(e)
                }
            }
        }
    }
}

/// The first few kilobytes of `path`, the same read the preview sniffs.
fn head(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut head = Vec::with_capacity(df_core::preview::SNIFF_BYTES);
    std::fs::File::open(path)?
        .take(df_core::preview::SNIFF_BYTES as u64)
        .read_to_end(&mut head)?;
    Ok(head)
}

/// What a toast calls `path`.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// The made PDF's name: the file's own `name`, with `.pdf` for its extension,
/// which is the name a print queue shows for the job.
fn pdf_name(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "print".to_string());
    format!("{}.pdf", df_core::path::made_valid(&stem))
}

/// A folder of its own under `scratch`, for one PDF and whatever its maker
/// leaves beside it — or for a remote file downloaded to be printed, under
/// its own name, so the PDF made of it is named as it is.
pub fn fresh_dir(scratch: &Path) -> Result<PathBuf, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    loop {
        let dir = scratch.join(NEXT.fetch_add(1, Ordering::Relaxed).to_string());
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        }
    }
}

// ── LibreOffice ─────────────────────────────────────────────────────────────

/// The LibreOffice on `search` (a `PATH`): `soffice`, else `libreoffice`.
fn office(search: &OsStr) -> Option<PathBuf> {
    ["soffice", "libreoffice"].into_iter().find_map(|program| {
        std::env::split_paths(search)
            .flat_map(|dir| {
                df_core::platform::process::candidates(program)
                    .into_iter()
                    .map(move |candidate| dir.join(candidate))
            })
            .find(|candidate| df_core::platform::process::is_executable(candidate))
    })
}

/// `program --headless --convert-to pdf --outdir dir path`, waited for: the
/// PDF it wrote in `dir`, which it names after the file. Stopped when `ctx`
/// is cancelled, and given up on after [`OFFICE_TIMEOUT`], with the last line
/// it wrote to stderr in the error when it said anything.
fn convert(program: &Path, path: &Path, dir: &Path, ctx: &TaskCtx) -> Result<PathBuf, String> {
    let name = display_name(path);
    // Beside the folder rather than in it, so the folder holds the PDF and
    // nothing else of ours.
    let log = dir.with_extension("stderr");
    let stderr = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let mut command = Command::new(program);
    command
        .args(["--headless", "--convert-to", "pdf", "--outdir"])
        .arg(dir)
        .arg(argument(path).as_os_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr));
    df_core::platform::process::quiet(&mut command);
    // Tied to this thread, which waits for it: a window that dies does not
    // leave a LibreOffice behind it converting for nobody.
    df_core::vfs::child::tie_to_this_thread(&mut command);
    let said = || -> String {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        text.lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .map(|line| format!(": {line}"))
            .unwrap_or_default()
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = said();
            return Err(format!("LibreOffice would not start for {name}: {e}"));
        }
    };
    let _tie = match df_core::vfs::child::tie(&child) {
        Ok(tie) => tie,
        Err(e) => {
            stop(&mut child);
            let _ = said();
            return Err(format!("LibreOffice would not start for {name}: {e}"));
        }
    };
    let deadline = Instant::now() + OFFICE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                stop(&mut child);
                let _ = said();
                return Err(format!("LibreOffice over {name}: {e}"));
            }
        }
        if ctx.is_cancelled() {
            stop(&mut child);
            let _ = said();
            return Err(df_core::DfError::Cancelled.to_string());
        }
        if Instant::now() >= deadline {
            stop(&mut child);
            return Err(format!(
                "LibreOffice took more than {} s over {name}{}",
                OFFICE_TIMEOUT.as_secs(),
                said()
            ));
        }
        std::thread::sleep(POLL);
    };
    let pdf = std::fs::read_dir(dir).ok().and_then(|entries| {
        entries
            .flatten()
            .map(|entry| entry.path())
            .find(|made| made.extension().is_some_and(|ext| ext == "pdf") && made.is_file())
    });
    let words = said();
    match pdf {
        Some(pdf) => Ok(pdf),
        None if status.success() => Err(format!("LibreOffice made no PDF of {name}{words}")),
        None => Err(format!(
            "LibreOffice could not convert {name} (exit {}){words}",
            df_core::platform::process::exit_code(&status)
        )),
    }
}

/// `path` as an argument nothing will read as an option: a file called
/// `-p.txt` in the folder on screen is still a file.
fn argument(path: &Path) -> Cow<'_, Path> {
    if path.to_string_lossy().starts_with('-') {
        Cow::Owned(Path::new(".").join(path))
    } else {
        Cow::Borrowed(path)
    }
}

/// End a LibreOffice that is still running: asked first, killed if it will
/// not go, and reaped either way.
fn stop(child: &mut Child) {
    if let Err(e) = df_core::platform::process::terminate(child) {
        log::debug!("LibreOffice would not take a SIGTERM: {e}");
    }
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            return;
        }
        std::thread::sleep(POLL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

// ── Paper ───────────────────────────────────────────────────────────────────

/// The sheet a picture is laid out on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paper {
    Letter,
    A4,
}

impl Paper {
    /// Width and height upright, in points.
    pub fn size(self) -> (f64, f64) {
        match self {
            Paper::Letter => (612.0, 792.0),
            Paper::A4 => (595.276, 841.89),
        }
    }
}

/// The paper a locale prints on: US Letter where the first of `LC_PAPER`,
/// `LC_ALL` and `LANG` that is set (an empty one is not) names the United
/// States, `en_US.UTF-8` and the like; A4, which the rest of the world uses,
/// everywhere else.
pub fn paper_for(lc_paper: Option<&str>, lc_all: Option<&str>, lang: Option<&str>) -> Paper {
    let first = [lc_paper, lc_all, lang]
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty());
    if first.is_some_and(|value| value.contains("_US")) {
        Paper::Letter
    } else {
        Paper::A4
    }
}

/// [`paper_for`] this process's locale.
fn paper_here() -> Paper {
    let var = |name: &str| std::env::var(name).ok();
    paper_for(
        var("LC_PAPER").as_deref(),
        var("LC_ALL").as_deref(),
        var("LANG").as_deref(),
    )
}

// ── A picture's page ────────────────────────────────────────────────────────

/// A picture as the PDF holds it.
struct Picture<'a> {
    width: u32,
    height: u32,
    /// `DeviceRGB` or `DeviceGray`.
    color: &'static str,
    /// `DCTDecode` for a JPEG's own bytes, `FlateDecode` for decoded ones.
    filter: &'static str,
    data: Cow<'a, [u8]>,
}

/// A one-page PDF of the picture in `bytes`, on `paper`.
pub fn picture_pdf(bytes: &[u8], paper: Paper) -> Result<Vec<u8>, String> {
    let jpeg = read_jpeg(bytes);
    let orientation = jpeg.as_ref().map_or(1, |jpeg| jpeg.orientation);
    let picture = match jpeg.filter(Jpeg::passes_through) {
        Some(jpeg) => Picture {
            width: u32::from(jpeg.width),
            height: u32::from(jpeg.height),
            color: if jpeg.components == 1 {
                "DeviceGray"
            } else {
                "DeviceRGB"
            },
            filter: "DCTDecode",
            data: Cow::Borrowed(bytes),
        },
        None => decoded(bytes)?,
    };
    page(&picture, orientation, paper).map_err(|e| e.to_string())
}

/// The picture decoded, as 8-bit RGB laid over white, deflated.
fn decoded(bytes: &[u8]) -> Result<Picture<'static>, String> {
    // The first frame, for an animated GIF or WebP.
    let image = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let (width, height) = (image.width(), image.height());
    let rgb: Vec<u8> = if image.color().has_alpha() {
        let rgba = image.into_rgba8();
        let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
        for pixel in rgba.pixels() {
            let [r, g, b, a] = pixel.0;
            let a = u32::from(a);
            for channel in [r, g, b] {
                // Over white: what a sheet of paper is under the ink.
                let over = (u32::from(channel) * a + 255 * (255 - a) + 127) / 255;
                rgb.push(over.min(255) as u8);
            }
        }
        rgb
    } else {
        image.into_rgb8().into_raw()
    };
    Ok(Picture {
        width,
        height,
        color: "DeviceRGB",
        filter: "FlateDecode",
        data: Cow::Owned(miniz_oxide::deflate::compress_to_vec_zlib(&rgb, 6)),
    })
}

/// The page: the picture fitted into the paper inside [`MARGIN`], turned to
/// landscape when it is wider than tall, and the five objects and the
/// cross-reference table that make it a PDF.
fn page(picture: &Picture, orientation: u8, paper: Paper) -> std::io::Result<Vec<u8>> {
    // The sides as the picture is seen: a quarter turn swaps them.
    let (seen_w, seen_h) = if orientation >= 5 {
        (f64::from(picture.height), f64::from(picture.width))
    } else {
        (f64::from(picture.width), f64::from(picture.height))
    };
    let (upright_w, upright_h) = paper.size();
    let (page_w, page_h) = if seen_w > seen_h {
        (upright_h, upright_w)
    } else {
        (upright_w, upright_h)
    };
    let (room_w, room_h) = (page_w - 2.0 * MARGIN, page_h - 2.0 * MARGIN);
    let scale = (room_w / seen_w).min(room_h / seen_h);
    let (drawn_w, drawn_h) = (seen_w * scale, seen_h * scale);
    let x = MARGIN + (room_w - drawn_w) / 2.0;
    let y = MARGIN + (room_h - drawn_h) / 2.0;
    let [a, b, c, d, e, f] = placement(orientation, x, y, drawn_w, drawn_h);
    let content = format!(
        "q\n{} {} {} {} {} {} cm\n/Im0 Do\nQ\n",
        num(a),
        num(b),
        num(c),
        num(d),
        num(e),
        num(f)
    );

    let mut out: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();
    writeln!(out, "%PDF-1.4")?;
    // Four bytes past ASCII, so a transfer that guesses at text leaves it be.
    out.extend_from_slice(b"%\xe2\xe3\xcf\xd3\n");
    offsets.push(out.len());
    write!(out, "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n")?;
    offsets.push(out.len());
    write!(
        out,
        "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n"
    )?;
    offsets.push(out.len());
    write!(
        out,
        "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] \
         /Resources << /XObject << /Im0 4 0 R >> /ProcSet [/PDF /ImageC /ImageB] >> \
         /Contents 5 0 R >>\nendobj\n",
        num(page_w),
        num(page_h)
    )?;
    offsets.push(out.len());
    write!(
        out,
        "4 0 obj\n<< /Type /XObject /Subtype /Image /Width {} /Height {} \
         /ColorSpace /{} /BitsPerComponent 8 /Filter /{} /Length {} >>\nstream\n",
        picture.width,
        picture.height,
        picture.color,
        picture.filter,
        picture.data.len()
    )?;
    out.extend_from_slice(&picture.data);
    write!(out, "\nendstream\nendobj\n")?;
    offsets.push(out.len());
    write!(
        out,
        "5 0 obj\n<< /Length {} >>\nstream\n{content}endstream\nendobj\n",
        content.len()
    )?;
    let xref = out.len();
    write!(out, "xref\n0 {}\n", offsets.len() + 1)?;
    // Twenty bytes a line, the end of line included, as the table requires.
    writeln!(out, "0000000000 65535 f ")?;
    for offset in &offsets {
        writeln!(out, "{offset:010} 00000 n ")?;
    }
    write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        offsets.len() + 1
    )?;
    Ok(out)
}

/// The matrix that draws the picture's unit square into the `w` × `h` box at
/// (`x`, `y`) the right way up, for an EXIF `orientation` (1–8): which side of
/// the stored picture is the top, and whether it is mirrored.
///
/// The image's first row is the top of its unit square, `v = 1`, and its
/// first column the left, `u = 0`. Orientation 6, a phone held upright, has
/// that first row on the right of what is seen and the first column at the
/// top: `u` runs down the box and `v` across it.
fn placement(orientation: u8, x: f64, y: f64, w: f64, h: f64) -> [f64; 6] {
    match orientation {
        2 => [-w, 0.0, 0.0, h, x + w, y],
        3 => [-w, 0.0, 0.0, -h, x + w, y + h],
        4 => [w, 0.0, 0.0, -h, x, y + h],
        5 => [0.0, -h, -w, 0.0, x + w, y + h],
        6 => [0.0, -h, w, 0.0, x, y + h],
        7 => [0.0, h, w, 0.0, x, y],
        8 => [0.0, h, -w, 0.0, x + w, y],
        _ => [w, 0.0, 0.0, h, x, y],
    }
}

/// A number as a PDF writes one: three places at most, no trailing zeros.
fn num(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    match text {
        "-0" | "" => "0".to_string(),
        text => text.to_string(),
    }
}

/// What a JPEG's headers say, up to its frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Jpeg {
    /// The start-of-frame marker: `0xC0` baseline, `0xC2` progressive, and
    /// the rarer kinds.
    sof: u8,
    precision: u8,
    width: u16,
    height: u16,
    components: u8,
    /// EXIF's orientation, 1 (as stored) when it says none.
    orientation: u8,
}

impl Jpeg {
    /// Whether every PDF reader decodes it as it is: baseline or progressive,
    /// eight bits, one component (grey) or three (YCbCr) — not CMYK.
    fn passes_through(&self) -> bool {
        matches!(self.sof, 0xC0 | 0xC2)
            && self.precision == 8
            && matches!(self.components, 1 | 3)
            && self.width > 0
            && self.height > 0
    }
}

/// Walk a JPEG's segments to its frame header. `None` when `bytes` is not a
/// JPEG, or its headers end before one.
fn read_jpeg(bytes: &[u8]) -> Option<Jpeg> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut orientation = 1;
    let mut at = 2;
    loop {
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        // Any number of fill bytes may come before a marker.
        while *bytes.get(at)? == 0xFF {
            at += 1;
        }
        let marker = *bytes.get(at)?;
        at += 1;
        match marker {
            // Markers with no segment after them.
            0x01 | 0xD0..=0xD8 => continue,
            // The image ended, or its data began, with no frame header.
            0xD9 | 0xDA => return None,
            _ => {}
        }
        let length = usize::from(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]));
        let body = bytes.get(at + 2..at.checked_add(length)?)?;
        match marker {
            0xE1 if body.starts_with(b"Exif\0\0") => {
                orientation = exif_orientation(&body[6..]).unwrap_or(1);
            }
            // Every start-of-frame but the three markers in its range that
            // are not one: the Huffman tables, the extension and the
            // arithmetic conditioning.
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                if body.len() < 6 {
                    return None;
                }
                return Some(Jpeg {
                    sof: marker,
                    precision: body[0],
                    height: u16::from_be_bytes([body[1], body[2]]),
                    width: u16::from_be_bytes([body[3], body[4]]),
                    components: body[5],
                    orientation,
                });
            }
            _ => {}
        }
        at += length;
    }
}

/// The orientation tag (0x0112) of an EXIF block's first directory: `tiff`
/// is the block after its `Exif\0\0`.
fn exif_orientation(tiff: &[u8]) -> Option<u8> {
    let big = match tiff.get(0..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let pair = [*tiff.get(at)?, *tiff.get(at + 1)?];
        Some(if big {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let quad = [
            *tiff.get(at)?,
            *tiff.get(at + 1)?,
            *tiff.get(at + 2)?,
            *tiff.get(at + 3)?,
        ];
        Some(if big {
            u32::from_be_bytes(quad)
        } else {
            u32::from_le_bytes(quad)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let directory = usize::try_from(u32_at(4)?).ok()?;
    let count = usize::from(u16_at(directory)?);
    (0..count).find_map(|n| {
        let entry = directory + 2 + n * 12;
        // A SHORT, whose value sits in the first two bytes of the field.
        if u16_at(entry)? != 0x0112 || u16_at(entry + 2)? != 3 {
            return None;
        }
        let value = u16_at(entry + 8)?;
        (1..=8).contains(&value).then_some(value as u8)
    })
}

// ── One file's turn ─────────────────────────────────────────────────────────

/// What one file's turn in a print run came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Printed {
    /// The PDF went to the printer.
    Sent,
    /// The dialog was cancelled, or the task was: the run stops here.
    Stopped,
    /// This file could not be printed, for the reason given; a run goes on
    /// to the next.
    Failed(String),
}

/// What the dialog's thread came to.
enum Asked {
    /// Cancelled: nothing printed.
    Cancelled,
    /// The dialog itself failed.
    Failed(String),
    /// Answered, and no PDF came to print under the answer.
    NoPdf,
    /// Answered, and the PDF was handed over: what the handing said.
    Printed(Result<(), String>),
}

/// One file through the print dialog: `dialog(name, close)` put up at once
/// on a thread of its own, titled with the file's name, its PDF made under
/// `scratch` meanwhile ([`pdf_for`]), and, when both are in,
/// `send(session, pdf)` under the dialog's answer, on the dialog's thread. A
/// made PDF is removed afterwards.
///
/// `close` is set to take a dialog that is still up down again: when the
/// task is cancelled — quitting cancels every task, and the engine waits for
/// its workers — and when the PDF could not be made, so the person is told
/// why at once rather than after choosing a printer for nothing. Either way
/// the turn waits for both threads, and a cancel is looked for until both
/// are done.
///
/// `dialog` and `send` are [`crate::platform::print::prepare`] and
/// [`crate::platform::print::Session::print`]; they are parameters so the
/// order of things can be tested without a dialog on anybody's screen. The
/// session never leaves the thread that asked for it.
pub fn print_one<S, D, P>(path: &Path, scratch: &Path, ctx: &TaskCtx, dialog: D, send: P) -> Printed
where
    D: FnOnce(&str, &AtomicBool) -> Result<Option<S>, String> + Send,
    P: FnOnce(S, &Path) -> Result<(), String> + Send,
{
    let title = display_name(path);
    let title = title.as_str();
    // Whether this can be printed at all, before anybody is asked how.
    let search = std::env::var_os("PATH").unwrap_or_default();
    if let Err(e) = route(path, &search) {
        return Printed::Failed(e);
    }
    if ctx.is_cancelled() {
        return Printed::Stopped;
    }
    // The making's own cancel: the dialog's thread pulls it when the dialog
    // is cancelled, and this one when the task is.
    let making = TaskCtx::detached();
    let stop_making = making.flags();
    // The dialog's own stop: set when the task is cancelled, and when the PDF
    // could not be made.
    let close = AtomicBool::new(false);
    let (hand, handed) = std::sync::mpsc::channel::<PathBuf>();
    std::thread::scope(|scope| {
        let stop_asked = Arc::clone(&stop_making);
        let close = &close;
        let asking = scope.spawn(move || match dialog(title, close) {
            // A task cancelled while the dialog was up sends nothing, whatever
            // the dialog was answered with.
            Ok(Some(session)) => match handed.recv() {
                Ok(pdf) if !ctx.is_cancelled() => Asked::Printed(send(session, &pdf)),
                _ => Asked::NoPdf,
            },
            Ok(None) => {
                stop_asked.cancel();
                Asked::Cancelled
            }
            Err(e) => {
                stop_asked.cancel();
                Asked::Failed(e)
            }
        });
        let making = &making;
        let mut maker = Some(scope.spawn(move || pdf_for(path, scratch, making)));
        let mut hand = Some(hand);
        let mut made = None;
        // Whether the making failed on its own — not because it was called
        // off — and the dialog was closed for it.
        let mut failed_making = false;
        // Until both are done: a cancel after the PDF is in still has a
        // dialog to close, or a hand-over to stop.
        while maker.is_some() || !asking.is_finished() {
            if ctx.is_cancelled() {
                stop_making.cancel();
                close.store(true, Ordering::SeqCst);
            }
            if let Some(finished) = maker.take_if(|maker| maker.is_finished()) {
                let result = finished
                    .join()
                    .unwrap_or_else(|_| Err(format!("making a PDF of {title} panicked")));
                // A dialog answered yes and waiting on a PDF is handed it,
                // or told — by the hand going — that none is coming.
                if let Some(hand) = hand.take() {
                    if let Ok(pdf) = &result {
                        if !ctx.is_cancelled() {
                            let _ = hand.send(pdf.path().to_path_buf());
                        }
                    }
                }
                if result.is_err() && !making.is_cancelled() {
                    failed_making = true;
                    close.store(true, Ordering::SeqCst);
                }
                made = Some(result);
                continue;
            }
            std::thread::sleep(POLL);
        }
        let asked = asking
            .join()
            .unwrap_or_else(|_| Asked::Failed("the print dialog panicked".to_string()));
        let failure = match made {
            Some(Ok(pdf)) => {
                pdf.discard();
                None
            }
            Some(Err(e)) => Some(e),
            None => None,
        };
        match (asked, failure) {
            (Asked::Printed(Ok(())), _) => Printed::Sent,
            (Asked::Printed(Err(e)), _) => Printed::Failed(e),
            _ if ctx.is_cancelled() => Printed::Stopped,
            // Closed because the PDF could not be made: that is what to say.
            (Asked::Cancelled, Some(e)) if failed_making => Printed::Failed(e),
            (Asked::Cancelled, _) => Printed::Stopped,
            (Asked::Failed(e), _) => Printed::Failed(e),
            (Asked::NoPdf, Some(e)) => Printed::Failed(e),
            (Asked::NoPdf, None) => Printed::Stopped,
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use df_core::test_support::TempTree;
    use std::io::Cursor;
    use std::sync::Mutex;

    // ── Reading a PDF back ──────────────────────────────────────────────────

    /// The parts of a PDF the tests look at, read back the way a reader
    /// would: through `startxref` and the table it points at.
    struct Parsed {
        bytes: Vec<u8>,
        /// Each object's offset, by number, from the table.
        offsets: Vec<usize>,
    }

    impl Parsed {
        fn new(bytes: Vec<u8>) -> Parsed {
            assert!(bytes.starts_with(b"%PDF-1."), "a PDF header");
            assert!(bytes.ends_with(b"%%EOF\n"), "an end-of-file marker");
            // Byte offsets throughout: the picture's bytes are not text, and
            // a lossy reading of them would move everything after them.
            let at = rfind(&bytes, b"startxref\n").unwrap() + "startxref\n".len();
            let xref: usize = line(&bytes, at).parse().unwrap();
            assert!(
                bytes[xref..].starts_with(b"xref\n0 "),
                "startxref points at the table"
            );
            let header = xref + "xref\n".len();
            let count: usize = line(&bytes, header)
                .split(' ')
                .nth(1)
                .unwrap()
                .parse()
                .unwrap();
            // Each entry exactly twenty bytes, its end of line included.
            let table_start = header + line(&bytes, header).len() + 1;
            let mut offsets = vec![0];
            for n in 0..count {
                let entry = &bytes[table_start + n * 20..table_start + (n + 1) * 20];
                assert_eq!(entry.len(), 20);
                assert!(
                    entry.ends_with(b" \n"),
                    "{:?}",
                    String::from_utf8_lossy(entry)
                );
                if n == 0 {
                    assert_eq!(entry, b"0000000000 65535 f \n");
                    continue;
                }
                let offset: usize = std::str::from_utf8(&entry[..10]).unwrap().parse().unwrap();
                assert_eq!(&entry[10..], b" 00000 n \n");
                offsets.push(offset);
            }
            let parsed = Parsed { bytes, offsets };
            for n in 1..count {
                assert!(
                    parsed.bytes[parsed.offsets[n]..].starts_with(format!("{n} 0 obj").as_bytes()),
                    "object {n} is where the table says"
                );
            }
            parsed
        }

        /// Object `n`'s dictionary, as text.
        fn dict(&self, n: usize) -> String {
            let object = &self.bytes[self.offsets[n]..];
            let start = find(object, b"<<").unwrap();
            let end = find(object, b">>\n").unwrap();
            std::str::from_utf8(&object[start..end + 2])
                .unwrap()
                .to_string()
        }

        /// Object `n`'s stream bytes, by its `/Length`.
        fn stream(&self, n: usize) -> &[u8] {
            let dict = self.dict(n);
            let length: usize = dict
                .split("/Length ")
                .nth(1)
                .unwrap()
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap()
                .parse()
                .unwrap();
            let from = self.offsets[n];
            let at = from + find(&self.bytes[from..], b"stream\n").unwrap() + "stream\n".len();
            let data = &self.bytes[at..at + length];
            assert!(
                self.bytes[at + length..].starts_with(b"\nendstream")
                    || self.bytes[at + length..].starts_with(b"endstream")
            );
            data
        }

        fn pages(&self) -> usize {
            let needle = b"/Type /Page ";
            self.bytes
                .windows(needle.len())
                .filter(|w| w == needle)
                .count()
        }

        fn media_box(&self) -> [f64; 4] {
            let dict = self.dict(3);
            let inside = dict
                .split("/MediaBox [")
                .nth(1)
                .unwrap()
                .split(']')
                .next()
                .unwrap();
            let numbers: Vec<f64> = inside.split(' ').map(|n| n.parse().unwrap()).collect();
            [numbers[0], numbers[1], numbers[2], numbers[3]]
        }

        /// The `cm` matrix the content stream places the picture with.
        fn matrix(&self) -> [f64; 6] {
            let content = String::from_utf8_lossy(self.stream(5)).into_owned();
            let line = content.lines().find(|line| line.ends_with(" cm")).unwrap();
            let numbers: Vec<f64> = line
                .trim_end_matches(" cm")
                .split(' ')
                .map(|n| n.parse().unwrap())
                .collect();
            [
                numbers[0], numbers[1], numbers[2], numbers[3], numbers[4], numbers[5],
            ]
        }
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).rposition(|w| w == needle)
    }

    /// The ASCII line that starts at `at`, without its end.
    fn line(bytes: &[u8], at: usize) -> &str {
        let end = at + find(&bytes[at..], b"\n").unwrap();
        std::str::from_utf8(&bytes[at..end]).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.01
    }

    fn encode(image: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x * 7) as u8, (y * 5) as u8, 128])
        });
        encode(
            image::DynamicImage::ImageRgb8(image),
            image::ImageFormat::Jpeg,
        )
    }

    /// `jpeg` with an EXIF block saying `orientation` put in after its SOI.
    fn with_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM\x00\x2a\x00\x00\x00\x08");
        tiff.extend_from_slice(&1u16.to_be_bytes());
        tiff.extend_from_slice(&0x0112u16.to_be_bytes());
        tiff.extend_from_slice(&3u16.to_be_bytes());
        tiff.extend_from_slice(&1u32.to_be_bytes());
        tiff.extend_from_slice(&orientation.to_be_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_be_bytes());
        let mut segment = b"Exif\0\0".to_vec();
        segment.extend_from_slice(&tiff);
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((segment.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&segment);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    // ── The picture's page ──────────────────────────────────────────────────

    /// A JPEG every reader decodes goes in as its own bytes, unchanged, under
    /// `/DCTDecode`, on one upright page of the paper asked for.
    #[test]
    fn a_jpeg_goes_in_as_its_own_bytes() {
        let bytes = jpeg(60, 80);
        assert!(read_jpeg(&bytes).unwrap().passes_through());
        let pdf = Parsed::new(picture_pdf(&bytes, Paper::A4).unwrap());
        assert_eq!(pdf.pages(), 1);
        let [x0, y0, w, h] = pdf.media_box();
        assert_eq!((x0, y0), (0.0, 0.0));
        assert!(
            close(w, 595.276) && close(h, 841.89),
            "A4 upright: {w} × {h}"
        );
        let image = pdf.dict(4);
        assert!(image.contains("/Filter /DCTDecode"), "{image}");
        assert!(image.contains("/ColorSpace /DeviceRGB"), "{image}");
        assert!(image.contains("/Width 60 /Height 80"), "{image}");
        assert_eq!(
            pdf.stream(4),
            &bytes[..],
            "the file's bytes, not a re-encoding"
        );
    }

    /// A PNG is decoded and deflated, and what was see-through is white.
    #[test]
    fn a_png_is_deflated_over_white() {
        let image = image::RgbaImage::from_fn(4, 2, |x, _| {
            if x < 2 {
                image::Rgba([0, 0, 255, 0])
            } else {
                image::Rgba([255, 0, 0, 255])
            }
        });
        let bytes = encode(
            image::DynamicImage::ImageRgba8(image),
            image::ImageFormat::Png,
        );
        let pdf = Parsed::new(picture_pdf(&bytes, Paper::Letter).unwrap());
        assert_eq!(pdf.pages(), 1);
        let image = pdf.dict(4);
        assert!(image.contains("/Filter /FlateDecode"), "{image}");
        assert!(image.contains("/ColorSpace /DeviceRGB"), "{image}");
        assert!(image.contains("/BitsPerComponent 8"), "{image}");
        let pixels = miniz_oxide::inflate::decompress_to_vec_zlib(pdf.stream(4)).unwrap();
        assert_eq!(pixels.len(), 4 * 2 * 3);
        assert_eq!(&pixels[..3], &[255, 255, 255], "clear is white on paper");
        assert_eq!(&pixels[6..9], &[255, 0, 0], "opaque is itself");
        // Wider than tall: Letter on its side.
        assert_eq!(pdf.media_box(), [0.0, 0.0, 792.0, 612.0]);
    }

    /// A picture wider than tall turns the page; one taller than wide does
    /// not; either is fitted inside the half-inch margin, centred, its shape
    /// kept, larger or smaller than it was.
    #[test]
    fn the_page_turns_for_a_wide_picture_and_fits_it_in_the_margin() {
        for (width, height, landscape) in [(800, 200, true), (20, 30, false), (5000, 5000, false)] {
            let pdf = Parsed::new(picture_pdf(&jpeg(width, height), Paper::Letter).unwrap());
            let [_, _, page_w, page_h] = pdf.media_box();
            assert_eq!(page_w > page_h, landscape, "{width}×{height}");
            let [a, b, c, d, e, f] = pdf.matrix();
            assert_eq!((b, c), (0.0, 0.0), "upright");
            let (room_w, room_h) = (page_w - 72.0, page_h - 72.0);
            assert!(
                a <= room_w + 0.01 && d <= room_h + 0.01,
                "inside the margin"
            );
            assert!(close(a, room_w) || close(d, room_h), "as large as fits");
            assert!(
                close(a / d, f64::from(width) / f64::from(height)),
                "its shape kept"
            );
            assert!(close(e + a / 2.0, page_w / 2.0) && close(f + d / 2.0, page_h / 2.0));
        }
    }

    /// A phone's portrait photo is stored on its side with EXIF saying so:
    /// it prints upright on an upright page, its bytes still its own.
    #[test]
    fn exif_orientation_turns_the_picture_not_its_bytes() {
        let bytes = with_orientation(&jpeg(80, 60), 6);
        assert_eq!(read_jpeg(&bytes).unwrap().orientation, 6);
        let pdf = Parsed::new(picture_pdf(&bytes, Paper::A4).unwrap());
        let [_, _, page_w, page_h] = pdf.media_box();
        assert!(
            page_h > page_w,
            "a portrait photo prints on an upright page"
        );
        assert_eq!(pdf.stream(4), &bytes[..]);
        let [a, b, c, d, e, f] = pdf.matrix();
        // The stored top row along the right, the stored left column along
        // the top.
        assert_eq!((a, d), (0.0, 0.0));
        assert!(b < 0.0 && c > 0.0, "{b} {c}");
        let (w, h) = (c, -b);
        assert!(
            close(w / h, 60.0 / 80.0),
            "seen 60 wide, 80 tall: {w} × {h}"
        );
        assert!(close(e, (page_w - w) / 2.0) && close(f, (page_h - h) / 2.0 + h));
        // Little-endian EXIF reads the same.
        let tiff =
            b"II\x2a\x00\x08\x00\x00\x00\x01\x00\x12\x01\x03\x00\x01\x00\x00\x00\x08\x00\x00\x00";
        assert_eq!(exif_orientation(tiff), Some(8));
    }

    /// Every orientation maps the unit square onto the same box, corners to
    /// corners: the picture is turned and mirrored, never moved or stretched.
    #[test]
    fn every_orientation_fills_the_same_box() {
        let (x, y, w, h) = (10.0, 20.0, 300.0, 400.0);
        for orientation in 1..=8 {
            let [a, b, c, d, e, f] = placement(orientation, x, y, w, h);
            let mut corners: Vec<(i64, i64)> = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
                .iter()
                .map(|(u, v)| {
                    (
                        (a * u + c * v + e).round() as i64,
                        (b * u + d * v + f).round() as i64,
                    )
                })
                .collect();
            corners.sort();
            assert_eq!(
                corners,
                vec![(10, 20), (10, 420), (310, 20), (310, 420)],
                "orientation {orientation}"
            );
        }
        // 3 is upside down: the stored first row is along the bottom.
        let [_, _, _, d, _, f] = placement(3, x, y, w, h);
        assert_eq!(d * 1.0 + f, y, "v = 1, the first row, at the bottom");
    }

    /// A CMYK JPEG, a 12-bit one and a lossless one are not handed to a
    /// reader's DCT decoder as they are.
    #[test]
    fn only_baseline_or_progressive_grey_or_ycbcr_passes_through() {
        let mut header = Jpeg {
            sof: 0xC0,
            precision: 8,
            width: 10,
            height: 10,
            components: 3,
            orientation: 1,
        };
        assert!(header.passes_through());
        header.sof = 0xC2;
        assert!(header.passes_through());
        header.components = 1;
        assert!(header.passes_through());
        header.components = 4;
        assert!(!header.passes_through(), "CMYK");
        header.components = 3;
        header.precision = 12;
        assert!(!header.passes_through(), "12-bit");
        header.precision = 8;
        header.sof = 0xC3;
        assert!(!header.passes_through(), "lossless");
        assert_eq!(read_jpeg(b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(read_jpeg(b"\xff\xd8\xff\xd9"), None, "no frame header");
    }

    /// An animated GIF prints its first frame.
    #[test]
    fn an_animated_gif_prints_its_first_frame() {
        use image::codecs::gif::{GifEncoder, Repeat};
        let frame = |colour: [u8; 4]| {
            image::Frame::new(image::RgbaImage::from_pixel(3, 3, image::Rgba(colour)))
        };
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            encoder
                .encode_frames(vec![frame([255, 0, 0, 255]), frame([0, 0, 255, 255])])
                .unwrap();
        }
        let pdf = Parsed::new(picture_pdf(&bytes, Paper::A4).unwrap());
        assert!(pdf.dict(4).contains("/FlateDecode"));
        let pixels = miniz_oxide::inflate::decompress_to_vec_zlib(pdf.stream(4)).unwrap();
        assert_eq!(&pixels[..3], &[255, 0, 0], "the first frame, red");
    }

    #[test]
    fn numbers_are_written_short() {
        assert_eq!(num(612.0), "612");
        assert_eq!(num(595.276), "595.276");
        assert_eq!(num(841.89), "841.89");
        assert_eq!(num(-0.0001), "0");
        assert_eq!(num(-12.5), "-12.5");
    }

    // ── The paper ───────────────────────────────────────────────────────────

    /// Letter where the first of `LC_PAPER`, `LC_ALL`, `LANG` that is set
    /// names the United States, A4 everywhere else.
    #[test]
    fn the_paper_follows_the_first_locale_variable_that_is_set() {
        assert_eq!(paper_for(Some("en_US.UTF-8"), None, None), Paper::Letter);
        assert_eq!(paper_for(None, Some("en_US.UTF-8"), None), Paper::Letter);
        assert_eq!(paper_for(None, None, Some("en_US.UTF-8")), Paper::Letter);
        assert_eq!(paper_for(None, None, Some("es_US")), Paper::Letter);
        assert_eq!(paper_for(None, None, Some("en_GB.UTF-8")), Paper::A4);
        assert_eq!(paper_for(None, None, Some("de_DE.UTF-8")), Paper::A4);
        assert_eq!(paper_for(None, None, None), Paper::A4);
        assert_eq!(paper_for(None, None, Some("C")), Paper::A4);
        // The first that is set decides, whatever the others say.
        assert_eq!(
            paper_for(
                Some("de_DE.UTF-8"),
                Some("en_US.UTF-8"),
                Some("en_US.UTF-8")
            ),
            Paper::A4
        );
        assert_eq!(
            paper_for(None, Some("en_US.UTF-8"), Some("fr_FR.UTF-8")),
            Paper::Letter
        );
        // An empty one is not set.
        assert_eq!(
            paper_for(Some(""), None, Some("en_US.UTF-8")),
            Paper::Letter
        );
        assert_eq!(Paper::Letter.size(), (612.0, 792.0));
        assert_eq!(Paper::A4.size(), (595.276, 841.89));
    }

    // ── The routes ──────────────────────────────────────────────────────────

    const A_PDF: &[u8] =
        b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n";

    /// A PDF is printed as it is — by its bytes, whatever it is called — and
    /// discarding it leaves it where it was.
    #[test]
    fn a_pdf_is_printed_as_it_is_and_kept() {
        let tree = TempTree::new("print-pdf");
        let scratch = tree.join("scratch");
        let ctx = TaskCtx::detached();
        for name in ["paper.pdf", "scan-without-extension"] {
            let path = tree.file(name, A_PDF);
            let pdf = pdf_for(&path, &scratch, &ctx).unwrap();
            assert_eq!(pdf, Pdf::Itself(path.clone()), "{name}");
            pdf.discard();
            assert!(path.exists(), "{name} is not ours to remove");
        }
        assert!(!scratch.exists(), "nothing was made");
    }

    /// A picture's PDF is made in a folder of its own, named after it, and
    /// discarding it removes both.
    #[test]
    fn a_picture_is_made_into_a_page_and_discarded_after() {
        let tree = TempTree::new("print-picture");
        let scratch = tree.join("scratch");
        // A JPEG with a name that says otherwise: the bytes decide.
        let path = tree.file("photo.txt", &jpeg(30, 20));
        let pdf = pdf_for(&path, &scratch, &TaskCtx::detached()).unwrap();
        let Pdf::Made(made) = pdf.clone() else {
            panic!("{pdf:?}")
        };
        assert_eq!(made.file_name().unwrap(), "photo.pdf");
        assert!(made.starts_with(&scratch));
        Parsed::new(std::fs::read(&made).unwrap());
        let dir = made.parent().unwrap().to_path_buf();
        pdf.discard();
        assert!(!made.exists() && !dir.exists());
        assert!(path.exists());
    }

    /// Without LibreOffice, a file only it could print says so — and so does
    /// one of the pictures this build cannot decode — before anything is
    /// made or asked.
    #[test]
    fn without_libreoffice_the_error_says_what_is_needed() {
        let tree = TempTree::new("print-no-office");
        let scratch = tree.join("scratch");
        let ctx = TaskCtx::detached();
        let empty = OsStr::new("");
        for name in ["notes.md", "letter.docx", "photo.heic"] {
            let path = tree.file(name, b"# notes\n\nsome words\n");
            let error = pdf_on(&path, &scratch, &ctx, empty).unwrap_err();
            assert_eq!(error, format!("LibreOffice is needed to print {name}"));
        }
        // A PATH of folders with no LibreOffice in them is no different.
        let bare = tree.dir("bin");
        let notes = tree.file("notes.txt", b"words\n");
        let error = pdf_on(&notes, &scratch, &ctx, bare.as_os_str()).unwrap_err();
        assert_eq!(error, "LibreOffice is needed to print notes.txt");
        assert!(!scratch.exists(), "nothing was made for them");
        // …and a PDF or a picture never needed it.
        let routed = |path: &Path| route(path, empty);
        assert!(matches!(
            routed(&tree.file("a.pdf", A_PDF)),
            Ok(Route::Itself)
        ));
        assert!(matches!(
            routed(&tree.file("a.jpg", &jpeg(4, 4))),
            Ok(Route::Picture)
        ));
        // An empty file has nothing to print, whoever would print it.
        let empty_file = tree.file("empty.txt", b"");
        assert_eq!(
            routed(&empty_file).unwrap_err(),
            "empty.txt is empty: there is nothing to print"
        );
    }

    /// A LibreOffice on `PATH` is found by either of its names.
    #[cfg(unix)]
    #[test]
    fn libreoffice_is_found_by_either_name() {
        use std::os::unix::fs::PermissionsExt;
        let tree = TempTree::new("print-office-names");
        let bin = tree.dir("bin");
        let program = tree.file("bin/libreoffice", b"#!/bin/sh\n");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(office(bin.as_os_str()), Some(program));
        let soffice = tree.file("bin/soffice", b"#!/bin/sh\n");
        std::fs::set_permissions(&soffice, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(office(bin.as_os_str()), Some(soffice), "soffice first");
    }

    /// The real conversion, where this machine has a LibreOffice: a text file
    /// becomes a PDF in a folder of its own, and discarding it leaves nothing.
    #[test]
    fn libreoffice_converts_a_text_file_when_it_is_installed() {
        let search = std::env::var_os("PATH").unwrap_or_default();
        if office(&search).is_none() {
            eprintln!("skipping: no soffice or libreoffice on PATH");
            return;
        }
        let tree = TempTree::new("print-office");
        let scratch = tree.join("scratch");
        let path = tree.file("notes.md", b"# Notes\n\nPrinted by a test.\n");
        let pdf = pdf_for(&path, &scratch, &TaskCtx::detached()).unwrap();
        let Pdf::Made(made) = pdf.clone() else {
            panic!("{pdf:?}")
        };
        assert_eq!(made.file_name().unwrap(), "notes.pdf");
        assert!(std::fs::read(&made).unwrap().starts_with(b"%PDF-"));
        pdf.discard();
        assert_eq!(
            std::fs::read_dir(&scratch).unwrap().count(),
            0,
            "nothing left"
        );

        // …and through a whole turn, the PDF handed over is LibreOffice's,
        // named after the file, and gone afterwards.
        let report = tree.file("report.txt", b"A report.\n");
        let named = Mutex::new(None);
        let printed = print_one(
            &report,
            &scratch,
            &TaskCtx::detached(),
            |_, _| Ok(Some(())),
            |(), pdf| {
                *named.lock().unwrap() = pdf.file_name().map(|n| n.to_os_string());
                Ok(())
            },
        );
        assert_eq!(printed, Printed::Sent);
        assert_eq!(
            named.lock().unwrap().as_deref(),
            Some(OsStr::new("report.pdf"))
        );
        assert_eq!(
            std::fs::read_dir(&scratch).unwrap().count(),
            0,
            "nothing left"
        );
    }

    // ── One file's turn ─────────────────────────────────────────────────────

    /// A stand-in session: what it was asked to print, and whether it was.
    #[derive(Default)]
    struct Spool {
        printed: Mutex<Vec<(PathBuf, bool)>>,
        asked: Mutex<Vec<String>>,
    }

    /// Answered, the PDF goes to the printer while it still exists, and a
    /// made one is gone afterwards.
    #[test]
    fn an_answered_dialog_prints_the_pdf_then_it_is_removed() {
        let tree = TempTree::new("print-one-sent");
        let scratch = tree.join("scratch");
        let path = tree.file(
            "cat.png",
            &encode(
                image::DynamicImage::ImageRgb8(image::RgbImage::new(4, 4)),
                image::ImageFormat::Png,
            ),
        );
        let spool = Spool::default();
        let printed = print_one(
            &path,
            &scratch,
            &TaskCtx::detached(),
            |title, _| {
                spool.asked.lock().unwrap().push(title.to_string());
                Ok(Some(()))
            },
            |(), pdf| {
                let whole = std::fs::read(pdf).is_ok_and(|bytes| bytes.starts_with(b"%PDF-"));
                spool
                    .printed
                    .lock()
                    .unwrap()
                    .push((pdf.to_path_buf(), whole));
                Ok(())
            },
        );
        assert_eq!(printed, Printed::Sent);
        assert_eq!(*spool.asked.lock().unwrap(), vec!["cat.png".to_string()]);
        let printed = spool.printed.lock().unwrap();
        assert_eq!(printed.len(), 1);
        assert!(printed[0].1, "the PDF existed when it was handed over");
        assert!(!printed[0].0.exists(), "and is gone now");
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    }

    /// A cancelled dialog stops the run, and nothing is printed; a PDF is
    /// the file itself and stays.
    #[test]
    fn a_cancelled_dialog_stops_and_prints_nothing() {
        let tree = TempTree::new("print-one-cancelled");
        let path = tree.file("paper.pdf", A_PDF);
        let sent = Mutex::new(false);
        let printed = print_one(
            &path,
            &tree.join("scratch"),
            &TaskCtx::detached(),
            |_, _| Ok(None::<()>),
            |(), _| {
                *sent.lock().unwrap() = true;
                Ok(())
            },
        );
        assert_eq!(printed, Printed::Stopped);
        assert!(!*sent.lock().unwrap());
        assert!(path.exists());
    }

    /// A dialog that fails, or a printer that refuses the PDF, is that file's
    /// failure, said as it was said.
    #[test]
    fn a_failing_dialog_or_printer_is_the_files_failure() {
        let tree = TempTree::new("print-one-failed");
        let path = tree.file("paper.pdf", A_PDF);
        let scratch = tree.join("scratch");
        let ctx = TaskCtx::detached();
        let failed = print_one(
            &path,
            &scratch,
            &ctx,
            |_, _| Err::<Option<()>, String>("no print portal".to_string()),
            |(), _| Ok(()),
        );
        assert_eq!(failed, Printed::Failed("no print portal".to_string()));
        let refused = print_one(
            &path,
            &scratch,
            &ctx,
            |_, _| Ok(Some(())),
            |(), _| Err("the printer is out of paper".to_string()),
        );
        assert_eq!(
            refused,
            Printed::Failed("the printer is out of paper".to_string())
        );
    }

    /// A file nothing here can print says so without a dialog being put up
    /// for it.
    #[test]
    fn a_file_that_cannot_be_printed_asks_nothing() {
        let tree = TempTree::new("print-one-unprintable");
        let path = tree.file("notes.md", b"");
        let asked = Mutex::new(false);
        let printed = print_one(
            &path,
            &tree.join("scratch"),
            &TaskCtx::detached(),
            |_, _| {
                *asked.lock().unwrap() = true;
                Ok(Some(()))
            },
            |(), _| Ok(()),
        );
        assert_eq!(
            printed,
            Printed::Failed("notes.md is empty: there is nothing to print".to_string())
        );
        assert!(
            !*asked.lock().unwrap(),
            "no dialog for a file that cannot print"
        );
    }

    /// A dialog cancelled while LibreOffice is converting stops it, and what
    /// it had started leaves nothing behind. Where there is a LibreOffice.
    #[test]
    fn a_cancelled_dialog_stops_libreoffice() {
        let search = std::env::var_os("PATH").unwrap_or_default();
        if office(&search).is_none() {
            eprintln!("skipping: no soffice or libreoffice on PATH");
            return;
        }
        let tree = TempTree::new("print-office-cancelled");
        let scratch = tree.join("scratch");
        let path = tree.file("notes.txt", b"words\n");
        let printed = print_one(
            &path,
            &scratch,
            &TaskCtx::detached(),
            |_, _| Ok(None::<()>),
            |(), _| Ok(()),
        );
        assert_eq!(printed, Printed::Stopped);
        let left = std::fs::read_dir(&scratch).map_or(0, |dir| dir.count());
        assert_eq!(left, 0, "nothing left in the scratch folder");
    }

    /// A cancelled task prints nothing, whatever the dialog said.
    #[test]
    fn a_cancelled_task_prints_nothing() {
        let tree = TempTree::new("print-one-task-cancelled");
        let path = tree.file("paper.pdf", A_PDF);
        let ctx = TaskCtx::detached();
        let flags = ctx.flags();
        let sent = Mutex::new(false);
        let printed = print_one(
            &path,
            &tree.join("scratch"),
            &ctx,
            |_, _| {
                // The task is cancelled while the dialog is up, and the
                // dialog is answered after it.
                flags.cancel();
                Ok(Some(()))
            },
            |(), _| {
                *sent.lock().unwrap() = true;
                Ok(())
            },
        );
        assert_eq!(printed, Printed::Stopped);
        assert!(!*sent.lock().unwrap());
    }

    /// A dialog that stays up until it is closed, as the portal's does —
    /// with a deadline of its own, so a close that never comes fails the
    /// test rather than hanging it — and whether it was closed.
    fn stays_up(
        closed: &Mutex<bool>,
    ) -> impl FnOnce(&str, &AtomicBool) -> Result<Option<()>, String> + Send + '_ {
        move |_: &str, close: &AtomicBool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !close.load(Ordering::SeqCst) {
                if Instant::now() >= deadline {
                    return Ok(Some(()));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            *closed.lock().unwrap() = true;
            Ok(None)
        }
    }

    /// A task cancelled while the dialog is up — after the PDF is in, so
    /// only the dialog is left to wait on — closes it, and the turn ends
    /// stopped with nothing sent. Quitting is this: every task cancelled,
    /// then the workers waited for.
    #[test]
    fn a_cancelled_task_closes_a_dialog_that_is_still_up() {
        let tree = TempTree::new("print-one-closed");
        let path = tree.file("paper.pdf", A_PDF);
        let ctx = TaskCtx::detached();
        let flags = ctx.flags();
        let closed = Mutex::new(false);
        let sent = Mutex::new(false);
        let start = Instant::now();
        let printed = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                flags.cancel();
            });
            print_one(
                &path,
                &tree.join("scratch"),
                &ctx,
                stays_up(&closed),
                |(), _| {
                    *sent.lock().unwrap() = true;
                    Ok(())
                },
            )
        });
        assert_eq!(printed, Printed::Stopped);
        assert!(*closed.lock().unwrap(), "the dialog was closed");
        assert!(!*sent.lock().unwrap());
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "not waited out: {:?}",
            start.elapsed()
        );
    }

    /// A PDF that could not be made closes the dialog, and the turn is that
    /// failure — said now, not after a printer has been chosen for nothing.
    #[test]
    fn a_pdf_that_cannot_be_made_closes_the_dialog_and_says_why() {
        let tree = TempTree::new("print-one-unmade");
        // A PNG by its signature, and nothing a decoder can read after it.
        let path = tree.file("broken.png", b"\x89PNG\r\n\x1a\n not a picture at all");
        let closed = Mutex::new(false);
        let printed = print_one(
            &path,
            &tree.join("scratch"),
            &TaskCtx::detached(),
            stays_up(&closed),
            |(), _| Ok(()),
        );
        let Printed::Failed(why) = printed else {
            panic!("{printed:?}");
        };
        assert!(why.starts_with("broken.png"), "{why}");
        assert!(*closed.lock().unwrap(), "the dialog was closed");
    }
}
