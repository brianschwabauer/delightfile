//! 3D models for the preview pane: STL, OBJ, PLY and 3MF, parsed into one
//! indexed triangle mesh and rasterised on the CPU.
//!
//! This is delightviewer's `dlv-doc::model` cut down to what a file manager's
//! preview pane actually needs. **Geometry only**: no materials, no textures,
//! no scene graph. Whatever the file spelled, what comes out is the same
//! thing — an indexed [`Mesh`] with deduplicated vertices and one normal per
//! vertex, in millimetres.
//!
//! **Millimetres is a decision, not a reading.** STL, OBJ and PLY are unitless
//! — the numbers in them are whatever the tool that wrote them meant — and the
//! printing world reads them as mm, which is why a 20 mm calibration cube is
//! "20" in every slicer on the machine. 3MF is the one format that *says*, in
//! its `<model unit="…">` attribute, and it is honoured. [`Units`] carries
//! which of the two happened so the pane can qualify the number rather than let
//! it stand there bare.
//!
//! **A software rasteriser, not wgpu.** The preview pane's whole contract is
//! that a worker thread hands back pixels ([`crate::preview::decode::Rgba`]),
//! and a GPU context is not a thing a worker owns. An isometric view of a few
//! hundred thousand triangles with a depth buffer is a couple of milliseconds
//! of plain arithmetic, which is cheaper than the surface it would take to ask
//! a GPU for the same picture.
//!
//! What delightviewer has and this does not: `Scene` and the G-code toolpath
//! arm (delightfile previews a mesh, it does not open a slice), `Stats` with
//! its volume, watertightness and filament weights (numbers for an info panel
//! nobody is reading here), and every editing path. What is kept is every
//! parser, the zip reading, the vertex weld, and the small vector maths they
//! all lean on.
//!
//! **Every parser is a whole-file read.** These are files a person is looking
//! at, so they are small by construction; a mesh streamed in would buy nothing
//! and cost the dedup pass its random access.

use std::collections::HashMap;
use std::path::Path;

use super::{Ink, Rgba};

/// How the model's numbers got to be millimetres.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Units {
    /// STL/OBJ/PLY: the file does not say, and the printing convention is mm.
    #[default]
    AssumedMillimetres,
    /// 3MF said so. The word is the file's own — "inch", "meter" — and the
    /// geometry has already been scaled into mm on the way in, so this is the
    /// pane's to print and nothing else's.
    Declared(&'static str),
}

/// An indexed triangle mesh, in millimetres.
///
/// `positions` and `normals` are the same length and parallel; `indices` is a
/// flat list of triangle corners, so `indices.len()` is always a multiple of 3.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    /// Per-vertex normals, angle-weighted, parallel to `positions`.
    ///
    /// Nothing in the preview reads them: [`render`] shades **flat**, from each
    /// triangle's own face normal, because a printed part reads as facets and
    /// that is how every slicer on this machine draws it. They are still built
    /// and still carried, because they are exactly what a smooth-shaded viewer
    /// would need and the parsers have them in hand — deriving them again later
    /// would mean walking the mesh a second time to recover something that was
    /// thrown away here.
    #[allow(dead_code)]
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub units: Units,
}

impl Mesh {
    /// How many triangles the mesh draws.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// The axis-aligned bounds, `None` for an empty mesh.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut it = self.positions.iter();
        let first = *it.next()?;
        let (mut lo, mut hi) = (first, first);
        for p in it {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        Some((lo, hi))
    }
}

// ── Routing ────────────────────────────────────────────────────────────────

/// The four spellings of a mesh this module opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFormat {
    Stl,
    Obj,
    Ply,
    ThreeMf,
}

/// **How a mesh is recognized, and why it is mostly the name.**
///
/// Two of these four formats genuinely announce themselves and two genuinely
/// cannot. **PLY** starts with the three letters `ply` on a line of their own,
/// which is a magic number and is trusted under any extension. **3MF** is a
/// zip, so it starts `PK`, which every zip does; the part that makes it a 3MF
/// is the entry named `3D/3dmodel.model` inside, visible in the local file
/// headers, which is why the sniff looks for that string as well as accepting
/// the extension over `PK`. **STL has no magic at all** — a binary STL opens
/// with eighty bytes of arbitrary header and an ASCII one opens with `solid`,
/// which is also how a great many text files that are not STL begin — so the
/// extension decides. **OBJ is extension-only** for the same reason with less
/// ambiguity: `v ` is not a magic number.
pub fn sniff(head: &[u8], path: &Path) -> Option<ModelFormat> {
    if head.starts_with(b"ply\n") || head.starts_with(b"ply\r\n") {
        return Some(ModelFormat::Ply);
    }
    let ext = extension(path);
    if head.starts_with(b"PK\x03\x04") && (ext.as_deref() == Some("3mf") || has_3mf_part(head)) {
        return Some(ModelFormat::ThreeMf);
    }
    match ext.as_deref() {
        Some("stl") => Some(ModelFormat::Stl),
        Some("obj") => Some(ModelFormat::Obj),
        Some("ply") => Some(ModelFormat::Ply),
        Some("3mf") => Some(ModelFormat::ThreeMf),
        _ => None,
    }
}

fn extension(path: &Path) -> Option<String> {
    Some(path.extension()?.to_string_lossy().to_ascii_lowercase())
}

/// Does this zip's head name the part every 3MF must carry?
fn has_3mf_part(head: &[u8]) -> bool {
    head.windows(13).any(|w| w == b"3dmodel.model")
}

/// Parse whatever `bytes` are into a mesh in millimetres, using `path`'s
/// extension only as a tiebreak where the bytes say nothing.
///
/// A file that parses to no triangles at all is an error rather than an empty
/// mesh: the pane would draw a blank rectangle and the user would read it as a
/// broken preview, which is exactly what it is.
pub fn parse(bytes: &[u8], path: &Path) -> Result<Mesh, String> {
    let format = sniff(bytes, path).ok_or_else(|| {
        format!(
            "{}: not a model this build reads",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    })?;
    let mesh = match format {
        ModelFormat::Stl => parse_stl(bytes),
        ModelFormat::Obj => parse_obj(bytes),
        ModelFormat::Ply => parse_ply(bytes),
        ModelFormat::ThreeMf => parse_3mf(bytes),
    }?;
    if mesh.indices.is_empty() {
        return Err(format!(
            "{}: no triangles",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    Ok(mesh)
}

/// One line for the pane's chip: the triangle count and the bounding box.
pub fn summary(mesh: &Mesh) -> String {
    let triangles = mesh.triangle_count();
    let word = if triangles == 1 { "triangle" } else { "triangles" };
    let count = format!("{} {word}", grouped(triangles));
    match mesh.bounds() {
        Some((lo, hi)) => format!(
            "{count} · {:.1} × {:.1} × {:.1} mm",
            hi[0] - lo[0],
            hi[1] - lo[1],
            hi[2] - lo[2]
        ),
        None => count,
    }
}

/// A count with thousands separators, written out rather than pulled in: one
/// loop over the digits is the whole of what a formatting crate would do here.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ── STL ────────────────────────────────────────────────────────────────────

/// Binary or ASCII, decided by arithmetic rather than by the leading `solid`.
///
/// The usual test — "does it start with `solid`?" — is wrong in the one
/// direction that matters: several CAD tools write the word `solid` into the
/// binary header's eighty free bytes, and reading such a file as text produces
/// a mesh with no triangles in it. The **length** cannot lie: a binary STL is
/// exactly `84 + 50 × n` bytes for the triangle count it states at byte 80, so
/// the file either is that size or it is text.
pub fn parse_stl(bytes: &[u8]) -> Result<Mesh, String> {
    if let Some(mesh) = parse_stl_binary(bytes) {
        return Ok(mesh);
    }
    parse_stl_ascii(bytes)
}

fn parse_stl_binary(bytes: &[u8]) -> Option<Mesh> {
    if bytes.len() < 84 {
        return None;
    }
    let count = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    let expected = count.checked_mul(50).and_then(|n| n.checked_add(84))?;
    // Some writers pad the tail; nothing sane truncates it.
    if bytes.len() < expected {
        return None;
    }
    let mut build = Builder::default();
    for i in 0..count {
        let at = 84 + i * 50;
        // The face normal at `at..at+12` is deliberately ignored: STL's
        // per-face normal is redundant with the winding, frequently zero, and
        // occasionally wrong, and this module computes angle-weighted vertex
        // normals for every format anyway.
        let mut corners = [[0.0f32; 3]; 3];
        for (c, corner) in corners.iter_mut().enumerate() {
            let base = at + 12 + c * 12;
            for (a, v) in corner.iter_mut().enumerate() {
                *v = f32::from_le_bytes([
                    bytes[base + a * 4],
                    bytes[base + a * 4 + 1],
                    bytes[base + a * 4 + 2],
                    bytes[base + a * 4 + 3],
                ]);
            }
        }
        build.triangle(corners[0], corners[1], corners[2]);
    }
    Some(build.finish(Units::AssumedMillimetres))
}

fn parse_stl_ascii(bytes: &[u8]) -> Result<Mesh, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut build = Builder::default();
    let mut corners: Vec<[f32; 3]> = Vec::with_capacity(3);
    for line in text.lines() {
        let mut words = line.split_ascii_whitespace();
        match words.next() {
            Some("vertex") => {
                let v = read3(&mut words)
                    .ok_or_else(|| format!("stl: bad vertex line '{}'", line.trim()))?;
                corners.push(v);
            }
            Some("endloop") | Some("endfacet") => {
                // A facet is a triangle; anything else in an STL is not a thing
                // the format allows, so a stray count is dropped rather than
                // fanned into shapes the file never drew.
                if corners.len() == 3 {
                    build.triangle(corners[0], corners[1], corners[2]);
                }
                corners.clear();
            }
            _ => {}
        }
    }
    Ok(build.finish(Units::AssumedMillimetres))
}

// ── OBJ ────────────────────────────────────────────────────────────────────

/// `v` / `vn` / `f` and nothing else. Faces with more than three corners are
/// fan-triangulated; indices may be negative, meaning "counted back from the
/// end of the list so far", which is what a concatenated OBJ relies on.
pub fn parse_obj(bytes: &[u8]) -> Result<Mesh, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut build = Builder::default();
    let mut face: Vec<(usize, Option<usize>)> = Vec::new();

    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        let mut words = line.split_ascii_whitespace();
        match words.next() {
            Some("v") => {
                if let Some(v) = read3(&mut words) {
                    positions.push(v);
                }
            }
            Some("vn") => {
                if let Some(v) = read3(&mut words) {
                    normals.push(v);
                }
            }
            Some("f") => {
                face.clear();
                for word in words {
                    // `v`, `v/vt`, `v//vn`, `v/vt/vn`.
                    let mut parts = word.split('/');
                    let Some(v) = parts.next().and_then(|s| s.parse::<i64>().ok()) else {
                        continue;
                    };
                    let _texture = parts.next();
                    let vn = parts.next().and_then(|s| s.parse::<i64>().ok());
                    let Some(v) = obj_index(v, positions.len()) else {
                        continue;
                    };
                    face.push((v, vn.and_then(|n| obj_index(n, normals.len()))));
                }
                if face.len() < 3 {
                    continue;
                }
                // A fan from the first corner. Correct for the convex polygons
                // an exporter writes, which is every polygon an OBJ from a CAD
                // tool contains.
                for i in 1..face.len() - 1 {
                    for &(v, n) in &[face[0], face[i], face[i + 1]] {
                        let p = positions.get(v).copied().unwrap_or([0.0; 3]);
                        match n.and_then(|n| normals.get(n).copied()) {
                            Some(normal) => build.corner_with_normal(p, normal),
                            None => build.corner(p),
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(build.finish(Units::AssumedMillimetres))
}

/// OBJ indices are 1-based, and negative means "from the end".
fn obj_index(raw: i64, len: usize) -> Option<usize> {
    let idx = if raw > 0 {
        raw - 1
    } else if raw < 0 {
        len as i64 + raw
    } else {
        return None;
    };
    (idx >= 0 && (idx as usize) < len).then_some(idx as usize)
}

// ── PLY ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlyScalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl PlyScalar {
    fn parse(name: &str) -> Option<PlyScalar> {
        Some(match name {
            "char" | "int8" => PlyScalar::I8,
            "uchar" | "uint8" => PlyScalar::U8,
            "short" | "int16" => PlyScalar::I16,
            "ushort" | "uint16" => PlyScalar::U16,
            "int" | "int32" => PlyScalar::I32,
            "uint" | "uint32" => PlyScalar::U32,
            "float" | "float32" => PlyScalar::F32,
            "double" | "float64" => PlyScalar::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            PlyScalar::I8 | PlyScalar::U8 => 1,
            PlyScalar::I16 | PlyScalar::U16 => 2,
            PlyScalar::I32 | PlyScalar::U32 | PlyScalar::F32 => 4,
            PlyScalar::F64 => 8,
        }
    }

    /// One value off the front of `bytes`. Big-endian is handled by reversing
    /// the bytes into a little-endian buffer, so there is one decoder for both
    /// orders rather than two that can drift apart.
    fn read(self, bytes: &[u8], big_endian: bool) -> Option<f64> {
        let raw = bytes.get(..self.size())?;
        let mut buf = [0u8; 8];
        let n = raw.len();
        buf[..n].copy_from_slice(raw);
        if big_endian {
            buf[..n].reverse();
        }
        Some(match self {
            PlyScalar::I8 => buf[0] as i8 as f64,
            PlyScalar::U8 => buf[0] as f64,
            PlyScalar::I16 => i16::from_le_bytes([buf[0], buf[1]]) as f64,
            PlyScalar::U16 => u16::from_le_bytes([buf[0], buf[1]]) as f64,
            PlyScalar::I32 => i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as f64,
            PlyScalar::U32 => u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as f64,
            PlyScalar::F32 => f32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as f64,
            PlyScalar::F64 => f64::from_le_bytes(buf),
        })
    }
}

/// One declared property: a scalar, or a list with a count of its own type.
#[derive(Debug, Clone)]
struct PlyProp {
    name: String,
    scalar: PlyScalar,
    /// `Some(count_type)` for a `property list`.
    list: Option<PlyScalar>,
}

#[derive(Debug, Clone)]
struct PlyElement {
    name: String,
    count: usize,
    props: Vec<PlyProp>,
}

/// How the body after `end_header` is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlyOrder {
    Ascii,
    LittleEndian,
    BigEndian,
}

/// ASCII and both binary byte orders.
///
/// delightviewer refuses `binary_big_endian` for want of a real file to test
/// against; here it is read, because the byte order is the *only* thing that
/// differs and it is checked against a little-endian file of the same cube in
/// this module's tests — which is a better answer than a refusal in a pane
/// whose whole job is to show the file.
pub fn parse_ply(bytes: &[u8]) -> Result<Mesh, String> {
    let header_end = find(bytes, b"end_header")
        .and_then(|at| find(&bytes[at..], b"\n").map(|nl| at + nl + 1))
        .ok_or_else(|| "ply: no end_header".to_string())?;
    let header = String::from_utf8_lossy(&bytes[..header_end]);

    let mut order = PlyOrder::Ascii;
    let mut elements: Vec<PlyElement> = Vec::new();
    for line in header.lines() {
        let mut words = line.split_ascii_whitespace();
        match words.next() {
            Some("format") => match words.next() {
                Some("ascii") => order = PlyOrder::Ascii,
                Some("binary_little_endian") => order = PlyOrder::LittleEndian,
                Some("binary_big_endian") => order = PlyOrder::BigEndian,
                Some(other) => return Err(format!("ply: unknown format '{other}'")),
                None => {}
            },
            Some("element") => {
                let name = words.next().unwrap_or_default().to_string();
                let count = words.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                elements.push(PlyElement {
                    name,
                    count,
                    props: Vec::new(),
                });
            }
            Some("property") => {
                let Some(element) = elements.last_mut() else {
                    continue;
                };
                match words.next() {
                    Some("list") => {
                        let (Some(count_ty), Some(item_ty), Some(name)) =
                            (words.next(), words.next(), words.next())
                        else {
                            continue;
                        };
                        if let (Some(count_ty), Some(item_ty)) =
                            (PlyScalar::parse(count_ty), PlyScalar::parse(item_ty))
                        {
                            element.props.push(PlyProp {
                                name: name.to_string(),
                                scalar: item_ty,
                                list: Some(count_ty),
                            });
                        }
                    }
                    Some(ty) => {
                        let (Some(ty), Some(name)) = (PlyScalar::parse(ty), words.next()) else {
                            continue;
                        };
                        element.props.push(PlyProp {
                            name: name.to_string(),
                            scalar: ty,
                            list: None,
                        });
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    let body = &bytes[header_end..];
    let (positions, normals, faces) = match order {
        PlyOrder::Ascii => read_ply_ascii(body, &elements),
        PlyOrder::LittleEndian => read_ply_binary(body, &elements, false)?,
        PlyOrder::BigEndian => read_ply_binary(body, &elements, true)?,
    };

    let mut build = Builder::default();
    for face in &faces {
        if face.len() < 3 {
            continue;
        }
        for i in 1..face.len() - 1 {
            for &v in &[face[0], face[i], face[i + 1]] {
                let Some(p) = positions.get(v).copied() else {
                    continue;
                };
                match normals.get(v).copied() {
                    Some(n) => build.corner_with_normal(p, n),
                    None => build.corner(p),
                }
            }
        }
    }
    Ok(build.finish(Units::AssumedMillimetres))
}

type PlyBody = (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<Vec<usize>>);

fn read_ply_ascii(body: &[u8], elements: &[PlyElement]) -> PlyBody {
    let text = String::from_utf8_lossy(body);
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut faces = Vec::new();
    for element in elements {
        for _ in 0..element.count {
            let Some(line) = lines.next() else { break };
            let values: Vec<f64> = line
                .split_ascii_whitespace()
                .filter_map(|w| w.parse::<f64>().ok())
                .collect();
            let mut at = 0usize;
            let mut named: Vec<(&str, f64)> = Vec::new();
            let mut list: Vec<usize> = Vec::new();
            for prop in &element.props {
                if prop.list.is_some() {
                    let n = values.get(at).copied().unwrap_or(0.0).max(0.0) as usize;
                    at += 1;
                    for k in 0..n {
                        if let Some(v) = values.get(at + k) {
                            list.push(*v as usize);
                        }
                    }
                    at += n;
                } else {
                    if let Some(v) = values.get(at) {
                        named.push((prop.name.as_str(), *v));
                    }
                    at += 1;
                }
            }
            push_ply_row(element, &named, list, &mut positions, &mut normals, &mut faces);
        }
    }
    (positions, normals, faces)
}

fn read_ply_binary(
    body: &[u8],
    elements: &[PlyElement],
    big_endian: bool,
) -> Result<PlyBody, String> {
    let mut at = 0usize;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut faces = Vec::new();
    for element in elements {
        for _ in 0..element.count {
            let mut named: Vec<(&str, f64)> = Vec::new();
            let mut list: Vec<usize> = Vec::new();
            for prop in &element.props {
                match prop.list {
                    Some(count_ty) => {
                        let n = count_ty
                            .read(&body[at.min(body.len())..], big_endian)
                            .ok_or_else(|| "ply: truncated list".to_string())?
                            .max(0.0) as usize;
                        at += count_ty.size();
                        for _ in 0..n {
                            let v = prop
                                .scalar
                                .read(&body[at.min(body.len())..], big_endian)
                                .ok_or_else(|| "ply: truncated list".to_string())?;
                            list.push(v.max(0.0) as usize);
                            at += prop.scalar.size();
                        }
                    }
                    None => {
                        let v = prop
                            .scalar
                            .read(&body[at.min(body.len())..], big_endian)
                            .ok_or_else(|| "ply: truncated body".to_string())?;
                        named.push((prop.name.as_str(), v));
                        at += prop.scalar.size();
                    }
                }
            }
            push_ply_row(element, &named, list, &mut positions, &mut normals, &mut faces);
        }
    }
    Ok((positions, normals, faces))
}

/// One decoded element row, filed under whichever element it belongs to. Every
/// element but `vertex` and `face` is read and dropped — which is how a PLY
/// full of colour, confidence and material properties still parses.
fn push_ply_row(
    element: &PlyElement,
    named: &[(&str, f64)],
    list: Vec<usize>,
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    faces: &mut Vec<Vec<usize>>,
) {
    let get = |name: &str| {
        named
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v as f32)
    };
    match element.name.as_str() {
        "vertex" => {
            positions.push([
                get("x").unwrap_or(0.0),
                get("y").unwrap_or(0.0),
                get("z").unwrap_or(0.0),
            ]);
            if let (Some(nx), Some(ny), Some(nz)) = (get("nx"), get("ny"), get("nz")) {
                // Normals are optional and all-or-nothing: a file with two of
                // the three components has not said anything.
                normals.push([nx, ny, nz]);
            }
        }
        "face" => faces.push(list),
        _ => {}
    }
}

// ── 3MF ────────────────────────────────────────────────────────────────────

/// The largest part this module will inflate out of a 3MF, in bytes: 256 MiB.
///
/// A zip entry states its own uncompressed size and a malicious one may state
/// anything, so the inflate is given a ceiling rather than the file's word. A
/// 256 MiB XML part is around six million triangles — far past anything a
/// preview pane will finish drawing, and still far short of a zip bomb.
const MAX_3MF_PART_BYTES: usize = 256 * 1024 * 1024;

/// The floor under that ceiling, in bytes: 1 MiB.
///
/// The limit handed to the inflater is twice the size the entry claims, which
/// covers a writer that rounded; this keeps a small part — or one whose header
/// says zero — from being squeezed by its own arithmetic.
const MIN_3MF_PART_BYTES: usize = 1024 * 1024;

/// A 3MF is a zip with an XML part in it. Deflate is the one thing here that is
/// impractical to rewrite, so `miniz_oxide` — already in the build behind the
/// png codec — does the inflating and everything around it is local: the
/// central directory is forty-odd bytes of little-endian fields, and the XML is
/// read with a tag scanner rather than a parser because the three elements that
/// matter carry all of their information in attributes.
pub fn parse_3mf(bytes: &[u8]) -> Result<Mesh, String> {
    let part = zip_entry(bytes, "3D/3dmodel.model")
        .or_else(|| zip_entry(bytes, "3d/3dmodel.model"))
        .ok_or_else(|| "3mf: no 3D/3dmodel.model part".to_string())?;
    let xml = String::from_utf8_lossy(&part).into_owned();
    parse_3mf_xml(&xml)
}

/// Millimetres per one of the unit names 3MF's `<model unit="…">` allows. The
/// default when the attribute is absent is the spec's own: millimetre.
fn unit_scale(unit: &str) -> Option<(f32, &'static str)> {
    Some(match unit {
        "micron" => (0.001, "micron"),
        "millimeter" => (1.0, "millimeter"),
        "centimeter" => (10.0, "centimeter"),
        "inch" => (25.4, "inch"),
        "foot" => (304.8, "foot"),
        "meter" => (1000.0, "meter"),
        _ => return None,
    })
}

/// One `<object>`: its own vertex list and its own triangle list, before the
/// build's items have said where to put it.
type Object = (Vec<[f32; 3]>, Vec<[usize; 3]>);

fn parse_3mf_xml(xml: &str) -> Result<Mesh, String> {
    let (scale, unit) = attr(tag(xml, "model").unwrap_or(""), "unit")
        .and_then(|u| unit_scale(&u))
        .unwrap_or((1.0, "millimeter"));

    let mut objects: HashMap<String, Object> = HashMap::new();
    let mut current: Option<String> = None;
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut tris: Vec<[usize; 3]> = Vec::new();
    let mut items: Vec<(String, Option<[f32; 12]>)> = Vec::new();

    for element in elements(xml) {
        match element.name {
            "object" => {
                if let Some(id) = attr(element.body, "id") {
                    current = Some(id);
                    verts = Vec::new();
                    tris = Vec::new();
                }
            }
            "vertex" => {
                let x = attr_f32(element.body, "x").unwrap_or(0.0);
                let y = attr_f32(element.body, "y").unwrap_or(0.0);
                let z = attr_f32(element.body, "z").unwrap_or(0.0);
                verts.push([x, y, z]);
            }
            "triangle" => {
                let (Some(a), Some(b), Some(c)) = (
                    attr_usize(element.body, "v1"),
                    attr_usize(element.body, "v2"),
                    attr_usize(element.body, "v3"),
                ) else {
                    continue;
                };
                tris.push([a, b, c]);
            }
            "/object" => {
                if let Some(id) = current.take() {
                    objects.insert(id, (std::mem::take(&mut verts), std::mem::take(&mut tris)));
                }
            }
            "item" => {
                if let Some(id) = attr(element.body, "objectid") {
                    items.push((id, attr(element.body, "transform").and_then(|t| matrix(&t))));
                }
            }
            _ => {}
        }
    }
    // An `<object>` that never closed (a truncated file) still counts.
    if let Some(id) = current.take() {
        objects.insert(id, (verts, tris));
    }

    // **Build items place the objects, when the build says so.** A 3×4
    // row-major matrix is the one piece of the 3MF scene graph a printing file
    // actually uses, and without it a model authored away from the origin sits
    // off the plate. A build with no items at all is every object as authored —
    // the shape a one-object export from a CAD tool takes.
    let mut build = Builder::default();
    let placements: Vec<(String, Option<[f32; 12]>)> = if items.is_empty() {
        let mut ids: Vec<String> = objects.keys().cloned().collect();
        // A HashMap iterates in whatever order it likes, and the triangle order
        // decides the vertex numbering; sorting keeps a re-parse of the same
        // file byte-identical to the last one.
        ids.sort();
        ids.into_iter().map(|id| (id, None)).collect()
    } else {
        items
    };
    for (id, transform) in placements {
        let Some((verts, tris)) = objects.get(&id) else {
            continue;
        };
        for tri in tris {
            let mut corners = [[0.0f32; 3]; 3];
            let mut ok = true;
            for (slot, &v) in corners.iter_mut().zip(tri.iter()) {
                match verts.get(v) {
                    Some(p) => *slot = place(*p, transform.as_ref(), scale),
                    None => ok = false,
                }
            }
            if ok {
                build.triangle(corners[0], corners[1], corners[2]);
            }
        }
    }
    Ok(build.finish(Units::Declared(unit)))
}

/// Apply a 3MF item transform (row-major 4×3, translation last) and the unit
/// scale, in that order — the transform is in the file's own units.
fn place(p: [f32; 3], m: Option<&[f32; 12]>, scale: f32) -> [f32; 3] {
    let q = match m {
        Some(m) => [
            p[0] * m[0] + p[1] * m[3] + p[2] * m[6] + m[9],
            p[0] * m[1] + p[1] * m[4] + p[2] * m[7] + m[10],
            p[0] * m[2] + p[1] * m[5] + p[2] * m[8] + m[11],
        ],
        None => p,
    };
    [q[0] * scale, q[1] * scale, q[2] * scale]
}

fn matrix(text: &str) -> Option<[f32; 12]> {
    let mut out = [0.0f32; 12];
    let mut n = 0;
    for word in text.split_ascii_whitespace() {
        if n == 12 {
            return None;
        }
        out[n] = word.parse().ok()?;
        n += 1;
    }
    (n == 12).then_some(out)
}

/// One `<name …>` occurrence: the tag's own name and the text inside the angle
/// brackets, which is where every attribute lives.
struct Element<'a> {
    name: &'a str,
    body: &'a str,
}

/// Every tag in the document, opening and closing alike, in order.
///
/// A scanner rather than a parser, deliberately: a 3MF's geometry is entirely
/// in attributes on `<vertex>`, `<triangle>` and `<item>`, with `<object>` and
/// `</object>` as the only nesting that matters. Text nodes, namespaces,
/// entities and comments carry nothing this module wants, so recognizing them
/// would be work in service of dropping them.
fn elements(xml: &str) -> impl Iterator<Item = Element<'_>> {
    let mut rest = xml;
    std::iter::from_fn(move || loop {
        let open = rest.find('<')?;
        let after = &rest[open + 1..];
        let close = after.find('>')?;
        let body = &after[..close];
        rest = &after[close + 1..];
        // Comments, declarations and processing instructions.
        if body.starts_with('!') || body.starts_with('?') {
            continue;
        }
        let name_end = body
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(body.len());
        let name = &body[..name_end];
        if name.is_empty() {
            continue;
        }
        return Some(Element { name, body });
    })
}

/// The body of the first `<name …>` in the document.
fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    elements(xml).find(|e| e.name == name).map(|e| e.body)
}

/// `name="value"`, single or double quoted.
fn attr(body: &str, name: &str) -> Option<String> {
    let mut rest = body;
    while let Some(at) = rest.find(name) {
        let before_ok = at == 0
            || rest[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        let after = &rest[at + name.len()..];
        let trimmed = after.trim_start();
        if before_ok && trimmed.starts_with('=') {
            let value = trimmed[1..].trim_start();
            let quote = value.chars().next()?;
            if quote == '"' || quote == '\'' {
                let end = value[1..].find(quote)?;
                return Some(value[1..1 + end].to_string());
            }
        }
        rest = &rest[at + name.len()..];
    }
    None
}

fn attr_f32(body: &str, name: &str) -> Option<f32> {
    attr(body, name)?.trim().parse().ok()
}

fn attr_usize(body: &str, name: &str) -> Option<usize> {
    attr(body, name)?.trim().parse().ok()
}

// ── The zip a 3MF is ───────────────────────────────────────────────────────

/// How far back from the end of the file the end-of-central-directory record
/// is looked for, in bytes: 66,000.
///
/// A zip may carry a comment of up to 64 KiB after the EOCD, and the record
/// itself is 22 bytes; a shade over 64 KiB covers every legal file and bounds
/// the backwards scan on one that is not a zip at all.
const ZIP_EOCD_SEARCH_BYTES: usize = 66_000;

/// Read one entry out of a zip by name, stored or deflated.
///
/// Written here rather than pulled in: everything a 3MF needs from the format
/// is the end-of-central-directory record, the fixed-width central directory
/// entries and the local header's two variable lengths. What is genuinely
/// impractical to rewrite is **deflate**, and that is the one thing this asks a
/// dependency for. Every field read is bounds-checked, because the bytes are
/// somebody else's file.
fn zip_entry(bytes: &[u8], name: &str) -> Option<Vec<u8>> {
    let eocd = (0..bytes
        .len()
        .saturating_sub(21)
        .min(ZIP_EOCD_SEARCH_BYTES))
        .map(|back| bytes.len() - 22 - back)
        .find(|&at| bytes.get(at..at + 4) == Some(b"PK\x05\x06"))?;
    let count = u16::from_le_bytes([*bytes.get(eocd + 10)?, *bytes.get(eocd + 11)?]) as usize;
    let mut at = u32::from_le_bytes([
        *bytes.get(eocd + 16)?,
        *bytes.get(eocd + 17)?,
        *bytes.get(eocd + 18)?,
        *bytes.get(eocd + 19)?,
    ]) as usize;

    for _ in 0..count {
        // The whole fixed part of the entry, checked once, so the field reads
        // below cannot walk off the end of a truncated directory.
        let entry = bytes.get(at..at.checked_add(46)?)?;
        if !entry.starts_with(b"PK\x01\x02") {
            return None;
        }
        let le16 = |o: usize| u16::from_le_bytes([entry[o], entry[o + 1]]) as usize;
        let le32 =
            |o: usize| u32::from_le_bytes([entry[o], entry[o + 1], entry[o + 2], entry[o + 3]]) as usize;
        let method = le16(10);
        let compressed = le32(20);
        let uncompressed = le32(24);
        let name_len = le16(28);
        let extra_len = le16(30);
        let comment_len = le16(32);
        let local = le32(42);
        let entry_name = bytes.get(at + 46..at + 46 + name_len)?;
        if entry_name.eq_ignore_ascii_case(name.as_bytes()) {
            // The local header repeats the name and extra lengths, and they are
            // the *only* trustworthy ones: writers routinely disagree with the
            // central directory about the extra field.
            let header = bytes.get(local..local.checked_add(30)?)?;
            if !header.starts_with(b"PK\x03\x04") {
                return None;
            }
            let ln = u16::from_le_bytes([header[26], header[27]]) as usize;
            let le = u16::from_le_bytes([header[28], header[29]]) as usize;
            let start = local.checked_add(30)?.checked_add(ln)?.checked_add(le)?;
            let data = bytes.get(start..start.checked_add(compressed)?)?;
            return match method {
                0 => Some(data.to_vec()),
                // A lying header cannot ask for the world.
                8 =>miniz_oxide::inflate::decompress_to_vec_with_limit(
                    data,
                    uncompressed
                        .saturating_mul(2)
                        .clamp(MIN_3MF_PART_BYTES, MAX_3MF_PART_BYTES),
                )
                .ok(),
                _ => None,
            };
        }
        at = at
            .checked_add(46)?
            .checked_add(name_len)?
            .checked_add(extra_len)?
            .checked_add(comment_len)?;
    }
    None
}

// ── Building an indexed mesh ───────────────────────────────────────────────

/// Accumulates triangles, deduplicating vertices as they arrive.
///
/// The dedup is what turns a triangle soup into a mesh: a binary STL names
/// every corner of every triangle separately, so two triangles that meet along
/// an edge share nothing until their coincident positions are folded together.
/// Folding them is what makes the vertex normals — and therefore the shading —
/// mean anything, and it is what keeps a million-corner file from being a
/// million positions.
#[derive(Default)]
struct Builder {
    positions: Vec<[f32; 3]>,
    /// File-supplied normals, parallel to `positions`; `None` where the file
    /// said nothing about a vertex.
    given: Vec<Option<[f32; 3]>>,
    indices: Vec<u32>,
    lookup: HashMap<[i64; 3], u32>,
}

/// How close two corners have to be to be **the same corner**: a tenth of a
/// micron, which is a thousand times finer than any printer's nozzle and a
/// hundred times finer than any CAD kernel's own tolerance.
///
/// Welding on a lattice rather than on the exact bits is what makes the weld
/// answer about the *part* instead of about the exporter. A binary STL stores
/// every triangle's corners independently as `f32`, so a corner shared by two
/// faces is only bit-identical when the writer happened to compute it the same
/// way twice — an instance placed by a transform, a mesh merged from two
/// bodies, or a value that round-tripped through a text format lands an ULP or
/// two away. The lattice absorbs `f32`'s own spacing (about 6e-5 mm at a metre,
/// well inside the cell) and is far too coarse to weld two corners a person
/// meant to be apart.
const WELD_MM: f64 = 1.0e-4;

fn weld_key(v: f32) -> i64 {
    let scaled = v as f64 / WELD_MM;
    // A NaN or an infinity has no cell. Both are files that lie, and they weld
    // to one arbitrary point rather than being allowed to `as`-cast into
    // whatever saturation happens to give.
    if scaled.is_finite() {
        scaled.round() as i64
    } else {
        i64::MIN
    }
}

impl Builder {
    fn corner(&mut self, p: [f32; 3]) {
        let i = self.intern(p);
        self.indices.push(i);
    }

    fn corner_with_normal(&mut self, p: [f32; 3], n: [f32; 3]) {
        let i = self.intern(p);
        // First writer wins: a position shared by two vertices with different
        // normals is a crease the file drew, and this module has one normal per
        // position by construction. The angle-weighted average below is what
        // smooths those, and it is a better answer than the last one read.
        if self.given[i as usize].is_none() {
            self.given[i as usize] = Some(n);
        }
        self.indices.push(i);
    }

    fn triangle(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3]) {
        self.corner(a);
        self.corner(b);
        self.corner(c);
    }

    fn intern(&mut self, p: [f32; 3]) -> u32 {
        // −0.0 and +0.0 are the same point and different bit patterns.
        let norm = |v: f32| if v == 0.0 { 0.0f32 } else { v };
        let p = [norm(p[0]), norm(p[1]), norm(p[2])];
        let key = [weld_key(p[0]), weld_key(p[1]), weld_key(p[2])];
        if let Some(&i) = self.lookup.get(&key) {
            return i;
        }
        let i = self.positions.len() as u32;
        self.positions.push(p);
        self.given.push(None);
        self.lookup.insert(key, i);
        i
    }

    /// Finish, computing the normals the file did not give.
    ///
    /// **Angle-weighted**: each triangle contributes to a vertex in proportion
    /// to the angle it subtends there, which is what stops a corner where three
    /// long thin triangles meet one fat one from being lit as though the thin
    /// ones outvoted it. Area weighting has the opposite bias and plain
    /// averaging has both.
    fn finish(mut self, units: Units) -> Mesh {
        let mut normals = vec![[0.0f32; 3]; self.positions.len()];
        for tri in self.indices.chunks_exact(3) {
            let (ia, ib, ic) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            let (a, b, c) = (self.positions[ia], self.positions[ib], self.positions[ic]);
            let face = normalize(cross(sub(b, a), sub(c, a)));
            for (i, (u, v)) in [
                (ia, (sub(b, a), sub(c, a))),
                (ib, (sub(c, b), sub(a, b))),
                (ic, (sub(a, c), sub(b, c))),
            ] {
                let w = angle(u, v);
                for k in 0..3 {
                    normals[i][k] += face[k] * w;
                }
            }
        }
        for (i, n) in normals.iter_mut().enumerate() {
            match self.given[i] {
                Some(given) => *n = normalize(given),
                None => *n = normalize(*n),
            }
        }
        Mesh {
            positions: std::mem::take(&mut self.positions),
            normals,
            indices: std::mem::take(&mut self.indices),
            units,
        }
    }
}

// ── The rasteriser ─────────────────────────────────────────────────────────

/// The most triangles a single preview draw will touch: 200,000.
///
/// A preview pane is drawn while the cursor is moving, and the user is often
/// past the file before the picture lands. Two hundred thousand triangles is a
/// few milliseconds of projection and fill on one worker thread; a
/// four-million-triangle scan is most of a second, which is a hang the user
/// feels. Past this the mesh is drawn from an evenly strided subset — the
/// silhouette of a dense scan survives sampling one triangle in twenty, and the
/// alternative is a pane that stalls.
const MAX_TRIANGLES: usize = 200_000;

/// The camera's elevation above the horizon, in radians: `atan(1/√2)`, 35.26°.
///
/// This is the true isometric angle — the one where the three axes of a cube
/// project to equal lengths and 120° apart — which is the view every CAD tool's
/// "home" button lands on and therefore the one a part looks *right* in.
const ELEVATION_RADIANS: f32 = 0.615_479_7;

/// The fraction of the canvas the projected model fills: 0.86.
///
/// A little air on every side keeps the silhouette from touching the pane's
/// border, where it would read as clipped. Any tighter and a corner of the
/// bounding box sits on the edge; any looser and the model looks lost in the
/// pane at small sizes.
const FIT_MARGIN: f32 = 0.86;

/// How lit the darkest face still is, 0–1: 0.28.
///
/// Pure Lambert puts a face turned away from the key light at zero, which on a
/// dark background makes half the object disappear and the model read as a
/// silhouette with a bite out of it. A little ambient keeps every face on the
/// `dim`-to-`accent` ramp, which is what makes it read as a solid.
const AMBIENT: f32 = 0.28;

/// The key light, in view space: up and to the left, and towards the viewer.
///
/// Over the viewer's left shoulder is where every renderer and every studio
/// puts a key light, because it is where the sun is in the pictures people are
/// used to. The forward component keeps the faces nearest the camera brightest,
/// so the near corner of a box is the lit one.
const LIGHT_DIR: [f32; 3] = [-0.35, 0.55, 0.75];

/// Rasterise the mesh, isometric, z-buffered, flat-shaded, on the CPU.
///
/// `yaw` is the turntable angle in radians: the model spins about its own
/// vertical (Z, the printing convention) axis, and the camera stays put at
/// [`ELEVATION_RADIANS`] above it. The mesh is fitted to the canvas with a
/// margin and is never enlarged past filling it.
///
/// Faces are shaded between `ink.dim` and `ink.accent` — `dim` is the palette's
/// unlit end, and `accent` is the one colour in the four that is *supposed* to
/// carry a shape rather than text, so the object reads as an object and not as
/// a paragraph. Lighting is two-sided: an open shell or a mesh wound inside-out
/// still shades rather than going black, and a preview pane is not the place to
/// tell the user their normals are backwards.
pub fn render(mesh: &Mesh, width: u32, height: u32, yaw: f32, ink: &Ink) -> Rgba {
    let w = width as usize;
    let h = height as usize;
    let mut pixels = vec![0u8; w.saturating_mul(h).saturating_mul(4)];
    for px in pixels.chunks_exact_mut(4) {
        px.copy_from_slice(&ink.bg);
    }
    // Every early return below hands back the background it already painted.
    let frame = |pixels: Vec<u8>| Rgba {
        width,
        height,
        pixels,
    };
    // The length check is the one that matters on a 32-bit target, where a
    // canvas big enough to overflow `w * h * 4` would leave a buffer shorter
    // than the loops below think it is.
    let wanted = w.checked_mul(h).and_then(|n| n.checked_mul(4));
    if w == 0 || h == 0 || wanted != Some(pixels.len()) || mesh.indices.len() < 3 {
        return frame(pixels);
    }
    let Some((lo, hi)) = mesh.bounds() else {
        return frame(pixels);
    };
    // A file with a NaN or an infinity in it has no bounds worth fitting, and
    // every number downstream of one is poison.
    if !lo.iter().chain(hi.iter()).all(|v| v.is_finite()) {
        return frame(pixels);
    }
    let centre = [
        (lo[0] + hi[0]) * 0.5,
        (lo[1] + hi[1]) * 0.5,
        (lo[2] + hi[2]) * 0.5,
    ];

    let (sin_yaw, cos_yaw) = yaw.sin_cos();
    let (sin_el, cos_el) = ELEVATION_RADIANS.sin_cos();
    // Model space to view space: spin about Z, then tilt the camera up. Right
    // is +x, up is (0, sin el, cos el) and depth grows towards the viewer.
    let view = |p: [f32; 3]| -> [f32; 3] {
        let (x, y, z) = (p[0] - centre[0], p[1] - centre[1], p[2] - centre[2]);
        let rx = x * cos_yaw - y * sin_yaw;
        let ry = x * sin_yaw + y * cos_yaw;
        [rx, ry * sin_el + z * cos_el, -ry * cos_el + z * sin_el]
    };

    // The projection is affine, so the projected bounding box of the mesh is
    // the bounding box of the projected corners — eight points instead of a
    // pass over every vertex.
    let (mut min_x, mut max_x) = (f32::MAX, f32::MIN);
    let (mut min_y, mut max_y) = (f32::MAX, f32::MIN);
    for i in 0..8 {
        let corner = [
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        ];
        let v = view(corner);
        min_x = min_x.min(v[0]);
        max_x = max_x.max(v[0]);
        min_y = min_y.min(v[1]);
        max_y = max_y.max(v[1]);
    }
    let span_x = max_x - min_x;
    let span_y = max_y - min_y;
    let scale_x = if span_x > f32::EPSILON {
        w as f32 * FIT_MARGIN / span_x
    } else {
        f32::MAX
    };
    let scale_y = if span_y > f32::EPSILON {
        h as f32 * FIT_MARGIN / span_y
    } else {
        f32::MAX
    };
    let scale = scale_x.min(scale_y);
    if !scale.is_finite() || scale <= 0.0 {
        // A model with no extent at all — one point, or a degenerate file.
        return frame(pixels);
    }
    // The projected box is centred on the canvas about its own middle rather
    // than about the origin, so a model authored far from (0,0,0) still lands.
    let origin_x = w as f32 * 0.5 - (min_x + max_x) * 0.5 * scale;
    let origin_y = h as f32 * 0.5 + (min_y + max_y) * 0.5 * scale;

    let light = normalize(LIGHT_DIR);
    let mut depth_buffer = vec![f32::NEG_INFINITY; w * h];

    let triangles = mesh.triangle_count();
    let stride = triangles / MAX_TRIANGLES + 1;
    for t in (0..triangles).step_by(stride) {
        let tri = &mesh.indices[t * 3..t * 3 + 3];
        // View space first, then the screen, so the shading and the fill are
        // reading one set of numbers.
        let mut eye = [[0.0f32; 3]; 3];
        let mut ok = true;
        for (slot, &index) in eye.iter_mut().zip(tri.iter()) {
            let Some(p) = mesh.positions.get(index as usize) else {
                ok = false;
                break;
            };
            let v = view(*p);
            if !v.iter().all(|a| a.is_finite()) {
                ok = false;
                break;
            }
            *slot = v;
        }
        if !ok {
            continue;
        }
        let mut screen = [[0.0f32; 3]; 3];
        for (slot, v) in screen.iter_mut().zip(eye.iter()) {
            *slot = [origin_x + v[0] * scale, origin_y - v[1] * scale, v[2]];
        }

        // Flat shading, from the triangle's own face normal in view space —
        // one normal per triangle is what makes a printed part read as facets,
        // which is how a slicer draws it and how the person expects it.
        let normal = normalize(cross(sub(eye[1], eye[0]), sub(eye[2], eye[0])));
        let lambert = (normal[0] * light[0] + normal[1] * light[1] + normal[2] * light[2]).abs();
        let shade = (AMBIENT + (1.0 - AMBIENT) * lambert).clamp(0.0, 1.0);
        let colour = mix(ink.dim, ink.accent, shade);

        let area = edge(screen[0], screen[1], screen[2][0], screen[2][1]);
        if area.abs() < f32::EPSILON {
            continue;
        }
        let x0 = screen
            .iter()
            .fold(f32::MAX, |m, v| m.min(v[0]))
            .floor()
            .max(0.0) as usize;
        let x1 = (screen.iter().fold(f32::MIN, |m, v| m.max(v[0])).ceil()).min(w as f32 - 1.0);
        let y0 = screen
            .iter()
            .fold(f32::MAX, |m, v| m.min(v[1]))
            .floor()
            .max(0.0) as usize;
        let y1 = (screen.iter().fold(f32::MIN, |m, v| m.max(v[1])).ceil()).min(h as f32 - 1.0);
        if x1 < 0.0 || y1 < 0.0 {
            continue;
        }
        let (x1, y1) = (x1 as usize, y1 as usize);

        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                // Barycentric weights, divided by the signed area so either
                // winding gives the same "all three non-negative" test.
                let w0 = edge(screen[1], screen[2], px, py) / area;
                let w1 = edge(screen[2], screen[0], px, py) / area;
                let w2 = edge(screen[0], screen[1], px, py) / area;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let depth = w0 * screen[0][2] + w1 * screen[1][2] + w2 * screen[2][2];
                let at = y * w + x;
                if depth <= depth_buffer[at] {
                    continue;
                }
                depth_buffer[at] = depth;
                pixels[at * 4..at * 4 + 4].copy_from_slice(&colour);
            }
        }
    }

    frame(pixels)
}

/// The signed area of the triangle `(a, b, p)`, doubled — the edge function
/// every half-space rasteriser is built out of.
fn edge(a: [f32; 3], b: [f32; 3], px: f32, py: f32) -> f32 {
    (b[0] - a[0]) * (py - a[1]) - (b[1] - a[1]) * (px - a[0])
}

/// Blend two palette colours, `t` from all `a` to all `b`.
fn mix(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    let mut out = [0u8; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        let lerped = a[i] as f32 + (b[i] as f32 - a[i] as f32) * t;
        *slot = lerped.round().clamp(0.0, 255.0) as u8;
    }
    out
}

// ── Small vector maths ─────────────────────────────────────────────────────

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if !len.is_finite() || len <= f32::EPSILON {
        // A degenerate triangle has no direction to face. Up is the least
        // surprising answer and the shading treats it like any other.
        return [0.0, 0.0, 1.0];
    }
    [v[0] / len, v[1] / len, v[2] / len]
}

/// The angle between two edge vectors, in radians — the corner's weight.
fn angle(u: [f32; 3], v: [f32; 3]) -> f32 {
    let u = normalize(u);
    let v = normalize(v);
    (u[0] * v[0] + u[1] * v[1] + u[2] * v[2])
        .clamp(-1.0, 1.0)
        .acos()
}

fn read3<'a>(words: &mut impl Iterator<Item = &'a str>) -> Option<[f32; 3]> {
    let x = words.next()?.parse().ok()?;
    let y = words.next()?.parse().ok()?;
    let z = words.next()?.parse().ok()?;
    Some([x, y, z])
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── Files the tests build themselves ───────────────────────────────────

    /// The eight corners of an axis-aligned box, `lo` to `hi`.
    fn corners(lo: [f32; 3], hi: [f32; 3]) -> [[f32; 3]; 8] {
        [
            [lo[0], lo[1], lo[2]],
            [hi[0], lo[1], lo[2]],
            [hi[0], hi[1], lo[2]],
            [lo[0], hi[1], lo[2]],
            [lo[0], lo[1], hi[2]],
            [hi[0], lo[1], hi[2]],
            [hi[0], hi[1], hi[2]],
            [lo[0], hi[1], hi[2]],
        ]
    }

    /// The six faces of a box, each as four corner numbers, wound
    /// counter-clockwise seen from outside.
    const FACES: [[usize; 4]; 6] = [
        [0, 3, 2, 1], // z = lo, seen from below
        [4, 5, 6, 7], // z = hi
        [0, 1, 5, 4], // y = lo
        [2, 3, 7, 6], // y = hi
        [0, 4, 7, 3], // x = lo
        [1, 2, 6, 5], // x = hi
    ];

    /// A closed box as twelve outward-wound triangles.
    fn cube_triangles(lo: [f32; 3], hi: [f32; 3]) -> Vec<[[f32; 3]; 3]> {
        let c = corners(lo, hi);
        let mut out = Vec::with_capacity(12);
        for f in FACES {
            out.push([c[f[0]], c[f[1]], c[f[2]]]);
            out.push([c[f[0]], c[f[2]], c[f[3]]]);
        }
        out
    }

    /// The binary STL for a triangle soup — 84 bytes plus fifty each.
    fn binary_stl(tris: &[[[f32; 3]; 3]]) -> Vec<u8> {
        let mut out = Vec::with_capacity(84 + tris.len() * 50);
        // Eighty bytes of arbitrary header — and the word `solid` in it, which
        // is exactly the trap the length test exists to walk past.
        let mut header = [0x20u8; 80];
        header[..5].copy_from_slice(b"solid");
        out.extend_from_slice(&header);
        out.extend_from_slice(&(tris.len() as u32).to_le_bytes());
        for tri in tris {
            out.extend_from_slice(&[0u8; 12]); // the normal, deliberately zero
            for v in tri {
                for a in v {
                    out.extend_from_slice(&a.to_le_bytes());
                }
            }
            out.extend_from_slice(&[0u8; 2]); // attribute byte count
        }
        out
    }

    fn ascii_stl(tris: &[[[f32; 3]; 3]]) -> Vec<u8> {
        let mut out = String::from("solid test\n");
        for tri in tris {
            out.push_str("  facet normal 0 0 0\n    outer loop\n");
            for v in tri {
                out.push_str(&format!("      vertex {} {} {}\n", v[0], v[1], v[2]));
            }
            out.push_str("    endloop\n  endfacet\n");
        }
        out.push_str("endsolid test\n");
        out.into_bytes()
    }

    /// An OBJ with **quad** faces and **negative** indices — the two things the
    /// OBJ reader promises, in one file.
    fn obj_cube(lo: [f32; 3], hi: [f32; 3]) -> Vec<u8> {
        let c = corners(lo, hi);
        let mut out = String::from("# a cube, written by the test\n");
        for v in c {
            out.push_str(&format!("v {} {} {}\n", v[0], v[1], v[2]));
        }
        for (i, f) in FACES.iter().enumerate() {
            if i % 2 == 0 {
                // 1-based, positive.
                out.push_str(&format!(
                    "f {} {} {} {}\n",
                    f[0] + 1,
                    f[1] + 1,
                    f[2] + 1,
                    f[3] + 1
                ));
            } else {
                // The same corners counted back from the end of the list.
                out.push_str(&format!(
                    "f {} {} {} {}\n",
                    f[0] as i64 - 8,
                    f[1] as i64 - 8,
                    f[2] as i64 - 8,
                    f[3] as i64 - 8
                ));
            }
        }
        out.into_bytes()
    }

    /// An ASCII PLY of a triangle soup, with a normal per vertex when asked.
    fn ascii_ply(tris: &[[[f32; 3]; 3]], normals: bool) -> Vec<u8> {
        let (verts, faces) = indexed(tris);
        let mut out = String::from("ply\nformat ascii 1.0\n");
        out.push_str(&format!("element vertex {}\n", verts.len()));
        out.push_str("property float x\nproperty float y\nproperty float z\n");
        if normals {
            out.push_str("property float nx\nproperty float ny\nproperty float nz\n");
        }
        out.push_str(&format!("element face {}\n", faces.len()));
        out.push_str("property list uchar int vertex_indices\nend_header\n");
        for v in &verts {
            out.push_str(&format!("{} {} {}", v[0], v[1], v[2]));
            if normals {
                out.push_str(" 0 0 1");
            }
            out.push('\n');
        }
        for f in &faces {
            out.push_str(&format!("3 {} {} {}\n", f[0], f[1], f[2]));
        }
        out.into_bytes()
    }

    /// The same, binary, in either byte order, with a colour property in the
    /// middle that the parser has to read past rather than trip over.
    fn binary_ply(tris: &[[[f32; 3]; 3]], big_endian: bool) -> Vec<u8> {
        let (verts, faces) = indexed(tris);
        let order = if big_endian {
            "binary_big_endian"
        } else {
            "binary_little_endian"
        };
        let mut header = format!("ply\nformat {order} 1.0\n");
        header.push_str(&format!("element vertex {}\n", verts.len()));
        header.push_str("property float x\nproperty float y\nproperty float z\n");
        header.push_str("property uchar red\nproperty uchar green\nproperty uchar blue\n");
        header.push_str(&format!("element face {}\n", faces.len()));
        header.push_str("property list uchar uint vertex_indices\nend_header\n");
        let mut out = header.into_bytes();
        let f32_bytes = |a: f32| {
            if big_endian {
                a.to_be_bytes()
            } else {
                a.to_le_bytes()
            }
        };
        let u32_bytes = |a: u32| {
            if big_endian {
                a.to_be_bytes()
            } else {
                a.to_le_bytes()
            }
        };
        for v in &verts {
            for a in v {
                out.extend_from_slice(&f32_bytes(*a));
            }
            out.extend_from_slice(&[10, 20, 30]);
        }
        for f in &faces {
            out.push(3);
            for i in f {
                out.extend_from_slice(&u32_bytes(*i as u32));
            }
        }
        out
    }

    /// Fold a triangle soup into (vertices, faces) — the test's own dedup, so a
    /// PLY fixture is a real indexed mesh rather than a soup in disguise.
    fn indexed(tris: &[[[f32; 3]; 3]]) -> (Vec<[f32; 3]>, Vec<[usize; 3]>) {
        let mut verts: Vec<[f32; 3]> = Vec::new();
        let mut faces = Vec::new();
        for tri in tris {
            let mut f = [0usize; 3];
            for (slot, v) in f.iter_mut().zip(tri.iter()) {
                *slot = match verts.iter().position(|w| w == v) {
                    Some(i) => i,
                    None => {
                        verts.push(*v);
                        verts.len() - 1
                    }
                };
            }
            faces.push(f);
        }
        (verts, faces)
    }

    /// A 3MF: the XML part, zipped. `stored` picks between the two methods a
    /// zip entry can use, both of which real writers emit.
    fn three_mf(
        tris: &[[[f32; 3]; 3]],
        unit: &str,
        transform: Option<&str>,
        stored: bool,
    ) -> Vec<u8> {
        let (verts, faces) = indexed(tris);
        let mut xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <model unit=\"{unit}\" xml:lang=\"en-US\">\n<resources>\n\
             <object id=\"1\" type=\"model\">\n<mesh>\n<vertices>\n"
        );
        for v in &verts {
            xml.push_str(&format!(
                "<vertex x=\"{}\" y=\"{}\" z=\"{}\" />\n",
                v[0], v[1], v[2]
            ));
        }
        xml.push_str("</vertices>\n<triangles>\n");
        for f in &faces {
            xml.push_str(&format!(
                "<triangle v1=\"{}\" v2=\"{}\" v3=\"{}\" />\n",
                f[0], f[1], f[2]
            ));
        }
        xml.push_str("</triangles>\n</mesh>\n</object>\n</resources>\n<build>\n");
        match transform {
            Some(t) => xml.push_str(&format!("<item objectid=\"1\" transform=\"{t}\" />\n")),
            None => xml.push_str("<item objectid=\"1\" />\n"),
        }
        xml.push_str("</build>\n</model>\n");
        zip_of(
            &[
                ("[Content_Types].xml", b"<Types/>".to_vec()),
                ("3D/3dmodel.model", xml.into_bytes()),
            ],
            stored,
        )
    }

    /// A minimal zip. Enough of the format for `zip_entry` to be exercised
    /// against both compression methods and a multi-entry directory.
    fn zip_of(entries: &[(&str, Vec<u8>)], stored: bool) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut directory: Vec<u8> = Vec::new();
        let mut count = 0u16;
        for (name, data) in entries {
            let local = out.len() as u32;
            let (method, payload) = if stored {
                (0u16, data.clone())
            } else {
                (8u16, miniz_oxide::deflate::compress_to_vec(data, 6))
            };
            let crc = 0u32; // never checked — `zip_entry` reads, it does not verify
            for chunk in [
                b"PK\x03\x04".as_slice(),
                &20u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &method.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &crc.to_le_bytes(),
                &(payload.len() as u32).to_le_bytes(),
                &(data.len() as u32).to_le_bytes(),
                &(name.len() as u16).to_le_bytes(),
                &0u16.to_le_bytes(),
            ] {
                out.extend_from_slice(chunk);
            }
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&payload);

            for chunk in [
                b"PK\x01\x02".as_slice(),
                &20u16.to_le_bytes(),
                &20u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &method.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &crc.to_le_bytes(),
                &(payload.len() as u32).to_le_bytes(),
                &(data.len() as u32).to_le_bytes(),
                &(name.len() as u16).to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u32.to_le_bytes(),
                &local.to_le_bytes(),
            ] {
                directory.extend_from_slice(chunk);
            }
            directory.extend_from_slice(name.as_bytes());
            count += 1;
        }
        let dir_at = out.len() as u32;
        let dir_len = directory.len() as u32;
        out.extend_from_slice(&directory);
        for chunk in [
            b"PK\x05\x06".as_slice(),
            &0u16.to_le_bytes(),
            &0u16.to_le_bytes(),
            &count.to_le_bytes(),
            &count.to_le_bytes(),
            &dir_len.to_le_bytes(),
            &dir_at.to_le_bytes(),
            &0u16.to_le_bytes(),
        ] {
            out.extend_from_slice(chunk);
        }
        out
    }

    // ── The rules ──────────────────────────────────────────────────────────

    /// The two spellings of STL are the same file, and the reader must not be
    /// able to tell which one it read.
    #[test]
    fn a_binary_and_an_ascii_stl_of_one_triangle_parse_alike() {
        let tri = [[[0.0f32, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]]];
        let binary = parse_stl(&binary_stl(&tri)).expect("binary stl");
        let ascii = parse_stl(&ascii_stl(&tri)).expect("ascii stl");
        assert_eq!(binary.positions, ascii.positions);
        assert_eq!(binary.indices, ascii.indices);
        assert_eq!(binary.normals, ascii.normals);
        assert_eq!(binary.units, Units::AssumedMillimetres);
        assert_eq!(binary.triangle_count(), 1);
    }

    /// A binary STL whose eighty free bytes begin with `solid` is still binary
    /// — the trap the length test exists for.
    #[test]
    fn a_binary_stl_that_says_solid_is_read_as_binary() {
        let bytes = binary_stl(&cube_triangles([0.0; 3], [1.0; 3]));
        assert!(bytes.starts_with(b"solid"));
        assert_eq!(parse_stl(&bytes).expect("stl").triangle_count(), 12);
    }

    /// Faces with four corners are fanned, and negative indices count back from
    /// the end — the two things the OBJ reader is asked for.
    #[test]
    fn an_obj_fans_quads_and_counts_backwards() {
        let mesh = parse_obj(&obj_cube([0.0; 3], [2.0; 3])).expect("obj");
        assert_eq!(mesh.triangle_count(), 12);
        assert_eq!(mesh.positions.len(), 8);

        // One quad on its own is two triangles and no more.
        let quad = parse_obj(b"v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n").expect("obj");
        assert_eq!(quad.triangle_count(), 2);

        // A face named entirely with negative indices is the same triangle as
        // one named positively.
        let a = parse_obj(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").expect("obj");
        let b = parse_obj(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\n").expect("obj");
        assert_eq!(a.indices, b.indices);
        assert_eq!(a.positions, b.positions);
    }

    /// The three PLY bodies are three spellings of one cube.
    #[test]
    fn ascii_and_binary_ply_agree_about_the_same_cube() {
        let tris = cube_triangles([0.0; 3], [20.0; 3]);
        let ascii = parse_ply(&ascii_ply(&tris, false)).expect("ascii ply");
        let little = parse_ply(&binary_ply(&tris, false)).expect("little-endian ply");
        let big = parse_ply(&binary_ply(&tris, true)).expect("big-endian ply");
        for other in [&little, &big] {
            assert_eq!(ascii.positions, other.positions);
            assert_eq!(ascii.indices, other.indices);
            assert_eq!(ascii.normals, other.normals);
        }
        assert_eq!(ascii.triangle_count(), 12);
        assert_eq!(ascii.positions.len(), 8);
    }

    /// A PLY that carries its own normals keeps them; one that does not gets
    /// the angle-weighted ones, and both are unit length.
    #[test]
    fn ply_normals_are_taken_when_given_and_computed_when_not() {
        let tris = cube_triangles([0.0; 3], [4.0; 3]);
        let given = parse_ply(&ascii_ply(&tris, true)).expect("ply");
        for n in &given.normals {
            assert_eq!(*n, [0.0, 0.0, 1.0], "the file said so");
        }
        let computed = parse_ply(&ascii_ply(&tris, false)).expect("ply");
        for n in &computed.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "{n:?}");
        }
        // A cube's corner normal points diagonally out of the corner, and each
        // component has the same magnitude by symmetry.
        let corner = computed
            .positions
            .iter()
            .position(|p| *p == [0.0, 0.0, 0.0])
            .expect("the origin corner");
        let n = computed.normals[corner];
        assert!(n.iter().all(|a| (a.abs() - 0.577).abs() < 0.01), "{n:?}");
    }

    /// The angle weighting is the thing being claimed, so it is checked where
    /// it differs from a plain average: a corner where one large triangle meets
    /// two slivers must lean towards the large one.
    #[test]
    fn a_vertex_normal_is_weighted_by_the_angle_at_the_corner() {
        let apex = [0.0f32, 0.0, 0.0];
        // One triangle facing +z over a quarter turn…
        let mut tris = vec![[apex, [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]]];
        // …and two thin ones in the x = 0 plane, facing +x.
        tris.push([apex, [0.0, 0.0, 10.0], [0.0, 0.2, 10.0]]);
        tris.push([apex, [0.0, 0.2, 10.0], [0.0, 0.4, 10.0]]);
        let mesh = parse_stl(&binary_stl(&tris)).expect("stl");
        let at = mesh
            .positions
            .iter()
            .position(|p| *p == apex)
            .expect("the apex");
        let n = mesh.normals[at];
        // The quarter turn outvotes the two slivers: the z component wins.
        assert!(n[2].abs() > n[0].abs() * 3.0, "{n:?}");
    }

    /// A 3MF that says inch is a 3MF in inches, and it says so in [`Units`].
    #[test]
    fn a_3mf_is_scaled_by_the_unit_it_declares() {
        let tris = cube_triangles([0.0; 3], [1.0; 3]);
        let mesh = parse_3mf(&three_mf(&tris, "inch", None, false)).expect("3mf");
        assert_eq!(mesh.units, Units::Declared("inch"));
        assert_eq!(mesh.triangle_count(), 12);
        let (lo, hi) = mesh.bounds().expect("bounds");
        for a in 0..3 {
            assert!((hi[a] - lo[a] - 25.4).abs() < 1e-3, "{lo:?} {hi:?}");
        }
        // …and one that says millimetre is left alone, which is both the spec's
        // default and the assumption every other format gets.
        let mm = parse_3mf(&three_mf(&tris, "millimeter", None, true)).expect("stored 3mf");
        let (lo, hi) = mm.bounds().expect("bounds");
        assert_eq!([hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]], [1.0, 1.0, 1.0]);
        assert_eq!(
            parse_stl(&binary_stl(&tris)).expect("stl").units,
            Units::AssumedMillimetres
        );
    }

    /// A build item's transform places the object — a part authored at the
    /// origin and built at (100, 50, 0) is a part at (100, 50, 0).
    #[test]
    fn a_3mf_build_item_moves_the_object() {
        let tris = cube_triangles([0.0; 3], [10.0; 3]);
        let moved = parse_3mf(&three_mf(
            &tris,
            "millimeter",
            Some("1 0 0 0 1 0 0 0 1 100 50 0"),
            false,
        ))
        .expect("3mf");
        let (lo, hi) = moved.bounds().expect("bounds");
        assert_eq!(lo, [100.0, 50.0, 0.0]);
        assert_eq!(hi, [110.0, 60.0, 10.0]);
    }

    /// Both compression methods a zip entry can use.
    #[test]
    fn a_3mf_part_reads_stored_and_deflated_alike() {
        let tris = cube_triangles([0.0; 3], [10.0; 3]);
        for stored in [true, false] {
            let bytes = three_mf(&tris, "millimeter", None, stored);
            let mesh = parse_3mf(&bytes).unwrap_or_else(|e| panic!("stored={stored}: {e}"));
            assert_eq!(mesh.triangle_count(), 12, "stored={stored}");
        }
    }

    /// **A soup comes back a mesh.** A binary STL names thirty-six corners for
    /// a cube that has eight, and a mesh that kept all thirty-six would light
    /// every facet as though it stood alone.
    #[test]
    fn duplicated_vertices_are_welded_into_one() {
        let mesh = parse_stl(&binary_stl(&cube_triangles([0.0; 3], [20.0; 3]))).expect("stl");
        assert_eq!(mesh.triangle_count(), 12);
        assert!(mesh.positions.len() < mesh.triangle_count() * 3);
        assert_eq!(mesh.positions.len(), 8);
        assert_eq!(mesh.normals.len(), mesh.positions.len());
    }

    /// A corner an ULP away is the same corner: the shape of every real STL
    /// whose faces were written by two different calculations.
    #[test]
    fn corners_a_hair_apart_are_welded_into_one() {
        let mut tris = cube_triangles([0.0; 3], [10.0; 3]);
        let nudge = |v: f32| {
            if v == 10.0 {
                f32::from_bits(v.to_bits() + 1)
            } else {
                v
            }
        };
        for tri in tris.iter_mut().skip(6) {
            for p in tri.iter_mut() {
                *p = [nudge(p[0]), nudge(p[1]), nudge(p[2])];
            }
        }
        let mesh = parse_stl(&binary_stl(&tris)).expect("stl");
        assert_eq!(mesh.positions.len(), 8, "welded back into one cube");

        // …and the lattice is far too fine to weld two corners anybody meant to
        // be apart: a hundredth of a millimetre stays two vertices.
        let apart = [[[0.0f32, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 0.01, 0.0]]];
        let mesh = parse_stl(&binary_stl(&apart)).expect("stl");
        assert_eq!(mesh.positions.len(), 3);
    }

    /// Every format reads the same cube, through the same front door.
    #[test]
    fn every_format_reads_the_same_cube() {
        let tris = cube_triangles([0.0; 3], [20.0; 3]);
        let files: [(&str, Vec<u8>); 6] = [
            ("cube.stl", binary_stl(&tris)),
            ("cube-ascii.stl", ascii_stl(&tris)),
            ("cube.obj", obj_cube([0.0; 3], [20.0; 3])),
            ("cube.ply", ascii_ply(&tris, false)),
            ("cube-binary.ply", binary_ply(&tris, false)),
            ("cube.3mf", three_mf(&tris, "millimeter", None, false)),
        ];
        for (name, bytes) in files {
            let mesh = parse(&bytes, &PathBuf::from(name))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(mesh.triangle_count(), 12, "{name}");
            assert_eq!(mesh.positions.len(), 8, "{name}");
            let (lo, hi) = mesh.bounds().unwrap_or_default();
            assert_eq!([hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]], [20.0; 3], "{name}");
            assert_eq!(mesh.normals.len(), mesh.positions.len(), "{name}");
        }
    }

    /// The routing rule, in both directions.
    #[test]
    fn a_mesh_is_recognized_by_magic_where_it_has_any_and_by_name_where_it_has_none() {
        // PLY names itself, under any extension at all.
        assert_eq!(
            sniff(b"ply\nformat ascii 1.0\n", Path::new("mystery.dat")),
            Some(ModelFormat::Ply)
        );
        // A 3MF is a zip whose head names the part.
        let mf = three_mf(&cube_triangles([0.0; 3], [1.0; 3]), "millimeter", None, true);
        assert_eq!(sniff(&mf, Path::new("part.zip")), Some(ModelFormat::ThreeMf));
        assert_eq!(sniff(&mf, Path::new("part.3mf")), Some(ModelFormat::ThreeMf));
        // STL and OBJ have nothing to sniff, so the name is the whole answer.
        assert_eq!(sniff(b"\x00\x01\x02", Path::new("part.stl")), Some(ModelFormat::Stl));
        assert_eq!(sniff(b"# blender\n", Path::new("part.obj")), Some(ModelFormat::Obj));
        assert_eq!(sniff(b"# blender\n", Path::new("part.OBJ")), Some(ModelFormat::Obj));
        // …and nothing else is a mesh.
        assert_eq!(sniff(b"solid but a text file\n", Path::new("notes.txt")), None);
        assert_eq!(sniff(b"", Path::new("part.gcode")), None);
    }

    /// Nothing in here may panic on a file that is not what it says it is.
    #[test]
    fn garbage_bytes_come_back_as_errors_not_panics() {
        assert!(parse(b"\x7fELF not a model at all", Path::new("mystery.bin")).is_err());
        assert!(parse(b"\x00\x01\x02\x03", Path::new("broken.stl")).is_err());
        assert!(parse_ply(b"ply\n").is_err());
        assert!(parse_3mf(b"PK\x03\x04 not really").is_err());
        assert!(parse_3mf(b"PK\x05\x06").is_err());
        // A binary STL claiming four billion triangles is 200 GB of file that
        // is not there, and it must fall through to the text reader and find
        // nothing rather than allocate.
        let mut lying = vec![0u8; 84];
        lying[80..84].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_stl(&lying).expect("stl").triangle_count(), 0);
        // An empty everything.
        assert_eq!(parse_obj(b"").expect("obj").triangle_count(), 0);
        assert_eq!(parse_stl(b"").expect("stl").triangle_count(), 0);
        // A truncated zip directory is read to its end and gives up.
        let mut clipped = three_mf(&cube_triangles([0.0; 3], [1.0; 3]), "millimeter", None, true);
        clipped.truncate(clipped.len() / 2);
        assert!(parse_3mf(&clipped).is_err());
    }

    /// The pane's chip: a count with separators, and the box in millimetres.
    #[test]
    fn the_summary_names_the_triangles_and_the_bounding_box() {
        let mesh = parse_stl(&binary_stl(&cube_triangles([0.0; 3], [20.0; 3]))).expect("stl");
        assert_eq!(summary(&mesh), "12 triangles · 20.0 × 20.0 × 20.0 mm");

        let one = parse_stl(&binary_stl(&[[
            [0.0f32, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 2.5, 0.0],
        ]]))
        .expect("stl");
        assert_eq!(summary(&one), "1 triangle · 1.0 × 2.5 × 0.0 mm");

        assert_eq!(summary(&Mesh::default()), "0 triangles");
        assert_eq!(grouped(12_345), "12,345");
        assert_eq!(grouped(1_000_000), "1,000,000");
    }

    /// The renderer draws the canvas it was asked for, draws *something* on it,
    /// and draws the same thing twice.
    #[test]
    fn rendering_a_cube_fills_the_canvas_and_is_stable() {
        let mesh = parse_stl(&binary_stl(&cube_triangles([0.0; 3], [20.0; 3]))).expect("stl");
        let ink = Ink::test();
        let a = render(&mesh, 160, 120, 0.6, &ink);
        assert_eq!((a.width, a.height), (160, 120));
        assert_eq!(a.pixels.len(), 160 * 120 * 4);

        let drawn = a
            .pixels
            .chunks_exact(4)
            .filter(|px| *px != &ink.bg[..])
            .count();
        assert!(drawn > 160 * 120 / 10, "only {drawn} pixels were painted");

        let again = render(&mesh, 160, 120, 0.6, &ink);
        assert_eq!(a.pixels, again.pixels, "the same yaw is the same picture");

        // A different yaw is a different picture, which is what makes the
        // turntable a turntable.
        let turned = render(&mesh, 160, 120, 1.4, &ink);
        assert_ne!(a.pixels, turned.pixels);

        // Every painted pixel sits on the dim-to-accent ramp rather than being
        // whatever the depth buffer left behind.
        for px in a.pixels.chunks_exact(4) {
            if px == &ink.bg[..] {
                continue;
            }
            for c in 0..4 {
                let (lo, hi) = (
                    ink.dim[c].min(ink.accent[c]),
                    ink.dim[c].max(ink.accent[c]),
                );
                assert!(px[c] >= lo && px[c] <= hi, "{px:?} off the ramp");
            }
        }
    }

    /// The degenerate canvases and the degenerate files, none of which may
    /// panic and all of which come back the size they were asked for.
    #[test]
    fn rendering_survives_an_empty_mesh_a_zero_canvas_and_nan_coordinates() {
        let ink = Ink::test();
        let empty = render(&Mesh::default(), 32, 32, 0.0, &ink);
        assert_eq!(empty.pixels.len(), 32 * 32 * 4);
        assert!(empty.pixels.chunks_exact(4).all(|px| px == &ink.bg[..]));

        let cube = parse_stl(&binary_stl(&cube_triangles([0.0; 3], [20.0; 3]))).expect("stl");
        let nothing = render(&cube, 0, 0, 0.0, &ink);
        assert!(nothing.pixels.is_empty());
        assert!(render(&cube, 0, 40, 0.0, &ink).pixels.is_empty());
        assert!(render(&cube, 40, 0, 0.0, &ink).pixels.is_empty());

        // A single point has no extent to fit, and a file full of NaN has no
        // bounds at all; both draw the background rather than dividing by zero.
        let point = parse_stl(&binary_stl(&[[[1.0f32; 3]; 3]])).expect("stl");
        assert_eq!(render(&point, 16, 16, 0.0, &ink).pixels.len(), 16 * 16 * 4);

        let nan = parse_stl(&binary_stl(&[[
            [f32::NAN, 0.0, 0.0],
            [1.0, f32::INFINITY, 0.0],
            [0.0, 1.0, 0.0],
        ]]))
        .expect("stl");
        let drawn = render(&nan, 24, 24, 0.3, &ink);
        assert_eq!(drawn.pixels.len(), 24 * 24 * 4);

        // A huge yaw is still a yaw.
        assert_eq!(render(&cube, 8, 8, 1.0e9, &ink).pixels.len(), 8 * 8 * 4);
    }
}
