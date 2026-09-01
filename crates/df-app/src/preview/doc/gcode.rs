//! G-code toolpaths, as a static layer view.
//!
//! A `.gcode` file is a list of moves and the only interesting question a
//! preview pane can answer about one is *what does this thing look like*. So
//! this parses the moves, groups them into layers by their Z, and rasterises
//! one layer on the CPU into an [`super::Rgba`] the pane can upload as a
//! texture — the same shape of work the image decoder next door does, on the
//! same worker thread, for the same reason: a preview must never rasterise on
//! the paint thread.
//!
//! ## What was left behind from delightviewer's version
//!
//! delightviewer's `dlv-doc::gcode` is a *viewer* for toolpaths: it carries a
//! print clock derived from the file's feed rates, `at_us`/`layer_at_us`
//! scrubbing, per-layer start times, and the timeline lane's tick fractions.
//! None of that survives the port. A file browser's preview pane is a glance,
//! not a session — there is no transport to scrub, no playhead, nothing to
//! edit — and the clock was the expensive half of that module (two extra
//! arrays a file, built at parse). What is kept is the part that draws: the
//! sniffer, the state machine over the dialects, layer grouping, and the
//! handful of facts a slicer writes into its own comment header, because those
//! cost one `split_once` a line and fill the pane's one-line chip.
//!
//! Travels are also kept, where delightviewer dropped them on the floor. It
//! could afford to: it drew an orbitable 3D model where a travel line would be
//! a wire through the middle of the part. Looking straight down at one layer,
//! the travels *are* information — they show the print order and where the
//! seams land — so they are parsed, grouped with the layer they happen on, and
//! drawn much quieter than the extrusions.
//!
//! ## The decisions that matter here
//!
//! **A static layer view, not a time scrubber.** The pane's control is a layer
//! index. Layer *n* is the thing being looked at.
//!
//! **Layers below are drawn faintly underneath.** One layer on its own is a
//! floating outline that says nothing about the part; a few dozen layers of
//! history behind it give the thing a body, and the bright line on top is
//! unmistakably the one being pointed at.
//!
//! **The whole model's XY bounds set the scale, never the current layer's.**
//! Fitting each layer to the canvas would make a cone rescale on every step
//! and a part with a small top jump around. Holding one projection for the
//! whole file is what makes stepping through layers read as moving *up through
//! a part* rather than flipping through unrelated pictures. It is the entire
//! point of the view.
//!
//! Parsing never fails. A file that says nothing useful yields an empty
//! toolpath, which renders as an empty canvas and summarises as such.

use std::collections::{BTreeMap, BTreeSet};

/// Z values are snapped onto a lattice this fine before they count as one
/// layer. A micron is two orders of magnitude finer than any slicer's layer
/// height and coarse enough that a Z printed with three decimals lands in one
/// bucket rather than two neighbouring ones.
const LAYER_LATTICE_MM: f64 = 0.001;

/// How many lines of the head the sniffer reads. A slicer writes a comment
/// header of a few hundred settings lines before its first move — PrusaSlicer's
/// runs past two hundred — and 400 clears every one seen on this machine while
/// still being a bounded scan of a buffer somebody else sized.
const SNIFF_LINES: usize = 400;

/// A move of less than a nanometre of filament is the number's own rounding,
/// not an extrusion.
const E_EPSILON: f64 = 1.0e-6;

/// The layers of history drawn faintly under the current one.
///
/// Twenty-four layers is about five millimetres of body at a normal 0.2 mm
/// height: enough for the part to read as solid rather than as one floating
/// outline, and few enough that a step costs a bounded redraw. The alternative
/// — every layer below — is a 300-layer redraw on every arrow key in a pane
/// that is supposed to feel instant, and past a few dozen layers the added
/// lines are behind so much ink that nothing new appears.
const HISTORY_LAYERS: usize = 24;

/// The alpha the layer immediately below the current one is drawn at, and the
/// alpha the oldest layer of history fades to. The ramp between them is what
/// makes the stack read as depth instead of as one grey smear; the floor is
/// kept above zero so the oldest layers still contribute a footprint.
const HISTORY_NEAR_ALPHA: f32 = 0.55;
const HISTORY_FAR_ALPHA: f32 = 0.12;

/// The alpha a travel move is drawn at. Travels outnumber extrusions on a
/// sparse layer and mean much less, so they sit far enough back to be a texture
/// behind the part rather than a competing set of lines. They are drawn for the
/// current layer only — travels from twenty-four layers of history would be a
/// hairball with the part hidden inside it.
const TRAVEL_ALPHA: f32 = 0.16;

/// The total moves any one render is allowed to draw, across the current layer
/// and all of its history.
///
/// A real 300-layer print is a million moves; a preview pane redrawing that on
/// every arrow key is tens of milliseconds of a worker thread per keypress and
/// a queue that never drains. 120 000 antialiased segments is a few
/// milliseconds, and past the cap the picture is already dense enough that the
/// dropped lines change nothing anybody can see. History is drawn first so a
/// truncated render always keeps the current layer whole.
const MAX_MOVES: usize = 120_000;

/// The fraction of the shorter canvas side left empty around the model, so the
/// part does not touch the pane's edges and the fit has somewhere to put a
/// stroke that lands right on the bounding box.
const MARGIN_FRAC: f32 = 0.06;

/// Stroke widths in pixels: the current layer heavier than the history behind
/// it, so the live layer wins on weight as well as on colour. Under two pixels
/// keeps a dense infill from filling in solid at preview sizes.
const CURRENT_STROKE_PX: f32 = 1.8;
const HISTORY_STROKE_PX: f32 = 1.0;

/// How far apart, in pixels, a line is sampled while being drawn. Below one
/// pixel the trail is continuous with bilinear coverage on either side of it;
/// 0.7 gives a little overlap so no gaps open on a diagonal, without paying for
/// the doubled work a 0.35 step would cost.
const SAMPLE_STEP_PX: f32 = 0.7;

/// The samples one line may take, however long it claims to be. A move whose
/// coordinates came out of a file with a stray `X9999999` would otherwise ask
/// for millions of steps that all land off-canvas; the cap makes a garbage
/// coordinate cost nothing rather than hanging the worker.
const MAX_STEPS_PER_LINE: usize = 4096;

/// The largest canvas that will be allocated: 4096 × 4096. A preview pane is
/// never this big, and refusing past it means a bad size cannot ask for a
/// multi-gigabyte buffer.
const MAX_CANVAS_PIXELS: u64 = 16_777_216;

/// One extrusion or travel move, in millimetres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Move {
    pub from: [f32; 3],
    pub to: [f32; 3],
    /// True when filament went *out* over this move. A retraction moves `E`
    /// down and is a travel with the filament pulled back, which is why the
    /// sign decides rather than the presence of the letter.
    pub extruding: bool,
}

/// One printed layer: the height it sits at, and every move made while the
/// nozzle was there, extrusions and travels alike, in the order the file wrote
/// them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layer {
    pub z: f32,
    pub moves: Vec<Move>,
}

impl Layer {
}

/// A parsed toolpath: the layers, ascending, and whatever the slicer said about
/// the print in its comments.
#[derive(Debug, Clone, Default)]
pub struct Toolpath {
    pub layers: Vec<Layer>,
    pub facts: Facts,
}

impl Toolpath {
    /// How many layers the file turned out to have.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// The axis-aligned bounds of everything that gets *extruded*, `None` for a
    /// file that extrudes nothing.
    ///
    /// Travels are deliberately outside it. A printer that parks its head at
    /// 250 mm would otherwise frame the whole view around a place nothing is
    /// printed, and since these bounds are what the renderer fits to, that
    /// would shrink the part to a speck in the corner. Non-finite coordinates
    /// are skipped rather than poisoning the answer with a NaN.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        let mut any = false;
        for layer in &self.layers {
            for m in layer.moves.iter().filter(|m| m.extruding) {
                for p in [m.from, m.to] {
                    if !p.iter().all(|v| v.is_finite()) {
                        continue;
                    }
                    any = true;
                    for a in 0..3 {
                        lo[a] = lo[a].min(p[a]);
                        hi[a] = hi[a].max(p[a]);
                    }
                }
            }
        }
        any.then_some((lo, hi))
    }

    /// The layer height the *moves* turned out to have: the median gap between
    /// consecutive layers, which is right for a print with a thicker first
    /// layer and for one with a variable-height stretch in the middle, where a
    /// mean is wrong for both. `None` for a single-layer file, which has no gap
    /// to measure.
    pub fn measured_layer_height(&self) -> Option<f32> {
        if self.layers.len() < 2 {
            return None;
        }
        let mut gaps: Vec<f32> = self
            .layers
            .windows(2)
            .map(|w| w[1].z - w[0].z)
            .filter(|g| *g > 0.0 && g.is_finite())
            .collect();
        if gaps.is_empty() {
            return None;
        }
        gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Some(gaps[gaps.len() / 2])
    }

    /// The layer height to print in the pane's chip: what the slicer said it
    /// asked for, or failing that what the moves measure.
    pub fn layer_height_mm(&self) -> Option<f32> {
        self.facts
            .layer_height_mm
            .or_else(|| self.measured_layer_height())
    }
}

/// What the file says about itself, from its header comments.
///
/// Every field is read rather than derived: a file that does not say how many
/// grams it costs gets no grams, not a guess made from an extrusion length and
/// a filament diameter nobody stated.
#[derive(Debug, Default, Clone)]
pub struct Facts {
    /// The `generated by` line, trimmed to the program and its version.
    pub slicer: Option<String>,
    pub filament_g: Option<f64>,
    pub filament_mm: Option<f64>,
    /// The estimate as the slicer worded it — "1h 4m 12s". Kept as text on
    /// purpose: it is a quotation, and re-formatting somebody's estimate into
    /// another shape is how a number acquires a false precision.
    pub print_time: Option<String>,
    /// The declared layer height, from `; layer_height = 0.2`.
    pub layer_height_mm: Option<f32>,
}

impl Facts {
    /// "4.7 g" — the filament figure, in whichever unit the file gave. Metres
    /// for the millimetre spelling, because a slicer's millimetre figure is
    /// five digits long and filament is bought and discussed by the metre.
    pub fn filament_field(&self) -> Option<String> {
        if let Some(g) = self.filament_g {
            return Some(format!("{g:.1} g"));
        }
        self.filament_mm.map(|mm| format!("{:.2} m", mm / 1000.0))
    }
}

/// Whether these bytes look like G-code.
///
/// delightviewer could ask the extension first and use the content only as a
/// sanity check. Here the bytes are all there is, so the test is: no NUL (which
/// no toolpath contains and most binaries have in their first kilobyte), and at
/// least one line in the head that begins with a G, M or T word — a letter
/// followed by digits, which is the whole of the format's grammar and which no
/// prose file does at the start of a line. Comments and blank lines are skipped
/// over, since a slicer writes hundreds of them before the first move.
pub fn sniff(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    let text = String::from_utf8_lossy(head);
    text.lines().take(SNIFF_LINES).any(is_command_line)
}

/// Does this line start a G-code command — `G1 X…`, `M104 S200`, `T0`?
fn is_command_line(line: &str) -> bool {
    let mut chars = line.trim_start().chars();
    let Some(letter) = chars.next() else {
        return false;
    };
    if !matches!(letter.to_ascii_uppercase(), 'G' | 'M' | 'T') {
        return false;
    }
    chars.take_while(|c| c.is_ascii_digit()).count() > 0
}

/// The state a toolpath is read with. Every field is something a line in the
/// file can change, which is why the parser is a struct rather than a fold.
struct Machine {
    pos: [f64; 3],
    e: f64,
    absolute: bool,
    /// Extrusion is tracked separately from position because `M82`/`M83` are
    /// separate from `G90`/`G91`, and a slicer writing relative extrusion with
    /// absolute moves — PrusaSlicer's default — is the common case rather than
    /// the exotic one.
    absolute_e: bool,
    /// `G20` sets inches. Rare on a printer and free to honour.
    to_mm: f64,
}

impl Default for Machine {
    fn default() -> Machine {
        Machine {
            pos: [0.0; 3],
            e: 0.0,
            // Marlin powers up in absolute positioning with absolute extrusion,
            // and a file that means otherwise says so.
            absolute: true,
            absolute_e: true,
            to_mm: 1.0,
        }
    }
}

/// One move as the scan collects it, before the layer set is known.
struct RawMove {
    from: [f32; 3],
    to: [f32; 3],
    extruding: bool,
    /// The Z lattice key of where the move *ended*, which is the layer the
    /// nozzle was on while making it.
    key: i64,
}

/// Parse the whole of a toolpath's text. This never fails: a file with nothing
/// useful in it yields an empty [`Toolpath`].
pub fn parse(text: &str) -> Toolpath {
    let mut m = Machine::default();
    let mut facts = Facts::default();
    let mut raw: Vec<RawMove> = Vec::new();
    // A layer exists because something was *printed* on it. Collecting the keys
    // of the extruding moves first is what keeps a homing move at Z0 or a lift
    // between objects from inventing a layer with nothing on it.
    let mut printed: BTreeSet<i64> = BTreeSet::new();
    let mut z_of: BTreeMap<i64, f32> = BTreeMap::new();

    for line in text.lines() {
        let (code, comment) = split_comment(line);
        if let Some(c) = comment {
            read_fact(c, &mut facts);
        }
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        let Some(word) = word(code) else { continue };
        match word.as_str() {
            "G0" | "G1" => {
                let from = m.pos;
                let mut extruding = false;
                for (letter, value) in params(code) {
                    match letter {
                        'X' | 'Y' | 'Z' => {
                            let axis = match letter {
                                'X' => 0,
                                'Y' => 1,
                                _ => 2,
                            };
                            let v = value * m.to_mm;
                            m.pos[axis] = if m.absolute { v } else { m.pos[axis] + v };
                        }
                        'E' => {
                            let delta = if m.absolute_e {
                                let v = value * m.to_mm;
                                let d = v - m.e;
                                m.e = v;
                                d
                            } else {
                                let d = value * m.to_mm;
                                m.e += d;
                                d
                            };
                            extruding |= delta > E_EPSILON;
                        }
                        // `F` is modal and stated per minute. Nothing here
                        // needs a feed — the clock that used it stayed in
                        // delightviewer — so it is read past and dropped.
                        _ => {}
                    }
                }
                // A move that goes nowhere is a retraction or a bare feed
                // change, and there is no line to draw for it.
                if from == m.pos {
                    continue;
                }
                let key = lattice(m.pos[2]);
                z_of.entry(key).or_insert(m.pos[2] as f32);
                if extruding {
                    printed.insert(key);
                }
                raw.push(RawMove {
                    from: as_f32(from),
                    to: as_f32(m.pos),
                    extruding,
                    key,
                });
            }
            // "You are here" — no movement, just a new origin for the numbers
            // that follow. A slicer writing absolute extrusion resets `E` with
            // it on every layer, and a parser that missed it would read the
            // whole next layer as one enormous retraction.
            "G92" => {
                for (letter, value) in params(code) {
                    match letter {
                        'X' => m.pos[0] = value * m.to_mm,
                        'Y' => m.pos[1] = value * m.to_mm,
                        'Z' => m.pos[2] = value * m.to_mm,
                        'E' => m.e = value * m.to_mm,
                        _ => {}
                    }
                }
            }
            "G20" => m.to_mm = 25.4,
            "G21" => m.to_mm = 1.0,
            "G90" => m.absolute = true,
            "G91" => m.absolute = false,
            "M82" => m.absolute_e = true,
            "M83" => m.absolute_e = false,
            // `G28` homes: the head goes to zero on whichever axes are named,
            // and on all three when none are.
            "G28" => {
                let named: Vec<char> = params(code).into_iter().map(|(l, _)| l).collect();
                for (axis, letter) in ['X', 'Y', 'Z'].iter().enumerate() {
                    if named.is_empty() || named.contains(letter) {
                        m.pos[axis] = 0.0;
                    }
                }
            }
            _ => {}
        }
    }

    // The keys are ascending (a `BTreeSet`), so the layer index is the position
    // in the walk and the layers come out sorted by height for free.
    let index: BTreeMap<i64, usize> = printed.iter().enumerate().map(|(i, k)| (*k, i)).collect();
    let mut layers: Vec<Layer> = printed
        .iter()
        .map(|k| Layer {
            z: z_of.get(k).copied().unwrap_or(0.0),
            moves: Vec::new(),
        })
        .collect();
    for r in raw {
        // A travel at a height nothing was ever printed at — the lift over a
        // finished object, the park at the end — belongs to no layer and is
        // dropped rather than given one of its own.
        if let Some(i) = index.get(&r.key) {
            if let Some(layer) = layers.get_mut(*i) {
                layer.moves.push(Move {
                    from: r.from,
                    to: r.to,
                    extruding: r.extruding,
                });
            }
        }
    }
    Toolpath { layers, facts }
}

/// Snap a Z onto the layer lattice. A non-finite Z would make a nonsense key
/// rather than a panic, and is mapped to zero so it lands with the bed.
fn lattice(z: f64) -> i64 {
    if !z.is_finite() {
        return 0;
    }
    (z / LAYER_LATTICE_MM).round() as i64
}

fn as_f32(p: [f64; 3]) -> [f32; 3] {
    [p[0] as f32, p[1] as f32, p[2] as f32]
}

/// Split a line at its `;`, giving the code before it and the comment after.
fn split_comment(line: &str) -> (&str, Option<&str>) {
    match line.split_once(';') {
        Some((code, comment)) => (code, Some(comment)),
        None => (line, None),
    }
}

/// The command word at the start of a line — `G1`, `M104` — upper-cased, with
/// the leading zeros of `G01` normalised away so `G01` and `G1` are one word.
fn word(code: &str) -> Option<String> {
    let mut chars = code.trim_start().chars();
    let letter = chars.next()?.to_ascii_uppercase();
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    let digits: String = chars.take_while(|c| c.is_ascii_digit()).collect();
    let n: u32 = digits.parse().ok()?;
    Some(format!("{letter}{n}"))
}

/// Every `X12.3`-shaped parameter on a line, after the command word.
///
/// Scanned character by character rather than split on whitespace, because the
/// spaces are optional: `G1X10.5Y2` is as valid as `G1 X10.5 Y2` and some
/// firmwares' own output leaves them out. The first pair the scan finds is the
/// command word itself and is dropped here, having already been read by
/// [`word`].
fn params(code: &str) -> Vec<(char, f64)> {
    let chars: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if !c.is_ascii_alphabetic() {
            continue;
        }
        let start = i;
        while i < chars.len()
            && (chars[i].is_ascii_digit()
                || chars[i] == '.'
                || ((chars[i] == '-' || chars[i] == '+') && i == start))
        {
            i += 1;
        }
        let number: String = chars[start..i].iter().collect();
        if let Ok(v) = number.parse::<f64>() {
            if v.is_finite() {
                out.push((c.to_ascii_uppercase(), v));
            }
        }
    }
    if !out.is_empty() {
        out.remove(0);
    }
    out
}

/// Read one comment line for anything the pane's chip can say.
///
/// PrusaSlicer separates its keys with ` = ` and Bambu Studio and OrcaSlicer
/// with ` : `; both are `key<sep>value`, so one split serves both and the *key*
/// is what tells them apart from prose.
fn read_fact(comment: &str, facts: &mut Facts) {
    let comment = comment.trim();
    // The header line is the one fact that is not a `key = value` at all —
    // PrusaSlicer and Orca both write `generated by <program> <version> on
    // <date>` as prose — so it is read as a prefix before the split is tried.
    let lower = comment.to_ascii_lowercase();
    for prefix in ["generated by ", "generated with "] {
        if lower.starts_with(prefix) {
            if facts.slicer.is_none() {
                let rest = &comment[prefix.len()..];
                // Everything up to the date, which is a fact about when the
                // file was written rather than about what wrote it.
                let name = rest.split(" on ").next().unwrap_or(rest);
                facts.slicer = Some(name.trim().to_string());
            }
            return;
        }
    }
    let Some((key, value)) = comment.split_once('=').or_else(|| comment.split_once(':')) else {
        return;
    };
    let key = key.trim().to_ascii_lowercase();
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    match key.as_str() {
        "filament used [g]" | "total filament used [g]" => {
            facts.filament_g = facts.filament_g.or_else(|| first_number(value));
        }
        "filament used [mm]" => {
            facts.filament_mm = facts.filament_mm.or_else(|| first_number(value));
        }
        // Prusa's wording, then Bambu's and Orca's two.
        "estimated printing time (normal mode)"
        | "model printing time"
        | "total estimated time" => {
            if facts.print_time.is_none() {
                // Cut at the semicolon: Orca and Bambu write both figures on
                // one line — `; model printing time: 3m 20s; total estimated
                // time: 5m 51s` — and the value handed here is everything after
                // the first colon, so without this the whole line comes through
                // as the estimate. Keeping the slicer's own wording is about
                // formatting, not about swallowing a second sentence.
                let quoted = value.split(';').next().unwrap_or(value).trim();
                if !quoted.is_empty() {
                    facts.print_time = Some(quoted.to_string());
                }
            }
        }
        "layer_height" | "layer height" => {
            facts.layer_height_mm = facts
                .layer_height_mm
                .or_else(|| first_number(value).map(|v| v as f32));
        }
        _ => {}
    }
}

/// The first number in a value, so `4.66`, `4.66g` and `0.5 m` all answer.
fn first_number(value: &str) -> Option<f64> {
    let mut out = String::new();
    for c in value.chars() {
        if c.is_ascii_digit() || c == '.' || (c == '-' && out.is_empty()) {
            out.push(c);
        } else if !out.is_empty() {
            break;
        }
    }
    out.parse().ok().filter(|v: &f64| v.is_finite())
}

/// One line for the pane's chip: the layers, the height, the size and whoever
/// wrote the file — "312 layers · 0.20 mm · 120 × 120 × 62 mm · PrusaSlicer".
///
/// Anything the file did not say is left out rather than guessed at, so a bare
/// toolpath from a CAM post-processor gets the two facts its moves prove and
/// nothing else. The filament figure and the print-time quotation stay on
/// [`Facts`] for the info rows; a chip that carried everything would be a
/// paragraph.
pub fn summary(toolpath: &Toolpath) -> String {
    let count = toolpath.layer_count();
    if count == 0 {
        return "no printed layers".to_string();
    }
    let mut parts = vec![if count == 1 {
        "1 layer".to_string()
    } else {
        format!("{count} layers")
    }];
    if let Some(h) = toolpath.layer_height_mm().filter(|h| h.is_finite()) {
        parts.push(format!("{h:.2} mm"));
    }
    if let Some((lo, hi)) = toolpath.bounds() {
        parts.push(format!(
            "{:.0} × {:.0} × {:.0} mm",
            hi[0] - lo[0],
            hi[1] - lo[1],
            hi[2] - lo[2]
        ));
    }
    if let Some(slicer) = &toolpath.facts.slicer {
        parts.push(slicer.clone());
    }
    parts.join(" · ")
}

/// Rasterise one layer, looking straight down, on the CPU.
///
/// Layers below `layer` are drawn faintly underneath in `ink.dim` so the part
/// has a body rather than being one floating outline, and the current layer is
/// drawn on top of them in `ink.accent`. Accent rather than `fg`: the history
/// is already a ramp of `dim`, and `fg` sits in the same neutral family, so a
/// bright neutral line on top of two dozen dimmer neutral lines would be a
/// matter of contrast alone. The one saturated colour in the palette makes the
/// live layer a different *kind* of mark, which is what it is.
///
/// The projection is fitted to the whole model's XY bounds, not this layer's,
/// so stepping through layers moves up through a part instead of rescaling it.
///
/// Every degenerate case answers rather than panicking: no layers, an index
/// past the end (which clamps to the last layer), a zero-size model, a 0 × 0 or
/// absurdly large canvas, and non-finite coordinates.
pub fn render(
    toolpath: &Toolpath,
    layer: usize,
    width: u32,
    height: u32,
    ink: &super::Ink,
) -> super::Rgba {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_CANVAS_PIXELS {
        return super::Rgba {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        };
    }
    let count = (width as usize) * (height as usize);
    let mut pixels = ink.bg.repeat(count);

    // Nothing printed, or nothing to fit a projection to: the background is the
    // honest picture of an empty toolpath.
    let Some((lo, hi)) = toolpath.bounds() else {
        return super::Rgba {
            width,
            height,
            pixels,
        };
    };
    if toolpath.layers.is_empty() {
        return super::Rgba {
            width,
            height,
            pixels,
        };
    }
    // Past the end is the top of the part, not an error: a pane whose layer
    // index outran a re-parse should show the last layer, not a blank.
    let current = layer.min(toolpath.layers.len() - 1);

    let fit = Fit::new(lo, hi, width, height);
    let mut canvas = Canvas {
        w: width as i32,
        h: height as i32,
        px: &mut pixels,
    };
    let mut budget = MAX_MOVES;

    // History first, oldest to newest, so the newer layers paint over the older
    // ones — and so that a render that runs out of budget loses the faintest
    // history rather than the layer being looked at.
    let first = current.saturating_sub(HISTORY_LAYERS);
    for below in first..current {
        // How recent this layer is, 0 at the back of the history and 1 just
        // under the current layer.
        let depth = current - below;
        let span = (current - first).max(1) as f32;
        let recency = 1.0 - (depth as f32 - 1.0) / span;
        let alpha = HISTORY_FAR_ALPHA + (HISTORY_NEAR_ALPHA - HISTORY_FAR_ALPHA) * recency;
        let Some(l) = toolpath.layers.get(below) else {
            continue;
        };
        for m in l.moves.iter().filter(|m| m.extruding) {
            if budget == 0 {
                break;
            }
            budget -= 1;
            fit.stroke(&mut canvas, m, ink.dim, alpha, HISTORY_STROKE_PX);
        }
    }

    if let Some(l) = toolpath.layers.get(current) {
        // Travels under the extrusions: they say where the head went, and they
        // must never sit on top of the thing being printed.
        for m in l.moves.iter().filter(|m| !m.extruding) {
            if budget == 0 {
                break;
            }
            budget -= 1;
            fit.stroke(&mut canvas, m, ink.dim, TRAVEL_ALPHA, HISTORY_STROKE_PX);
        }
        for m in l.moves.iter().filter(|m| m.extruding) {
            if budget == 0 {
                break;
            }
            budget -= 1;
            fit.stroke(&mut canvas, m, ink.accent, 1.0, CURRENT_STROKE_PX);
        }
    }

    super::Rgba {
        width,
        height,
        pixels,
    }
}

/// The orthographic top-down projection: millimetres in, pixels out.
struct Fit {
    centre_mm: [f32; 2],
    centre_px: [f32; 2],
    scale: f32,
}

impl Fit {
    fn new(lo: [f32; 3], hi: [f32; 3], width: u32, height: u32) -> Fit {
        let (w, h) = (width as f32, height as f32);
        let margin = MARGIN_FRAC * w.min(h);
        let avail_w = (w - 2.0 * margin).max(1.0);
        let avail_h = (h - 2.0 * margin).max(1.0);
        // A single-point model has no span to divide by; the floor keeps the
        // scale finite and the point lands dead centre either way, since its
        // offset from the centre is zero.
        let span_x = (hi[0] - lo[0]).max(1.0e-3);
        let span_y = (hi[1] - lo[1]).max(1.0e-3);
        let mut scale = (avail_w / span_x).min(avail_h / span_y);
        if !scale.is_finite() || scale <= 0.0 {
            scale = 1.0;
        }
        Fit {
            centre_mm: [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5],
            centre_px: [w * 0.5, h * 0.5],
            scale,
        }
    }

    /// Project a point. Screen Y grows downward and the bed's does not, so the
    /// sign flips and the part is not printed upside down.
    fn project(&self, p: [f32; 3]) -> [f32; 2] {
        [
            self.centre_px[0] + (p[0] - self.centre_mm[0]) * self.scale,
            self.centre_px[1] - (p[1] - self.centre_mm[1]) * self.scale,
        ]
    }

    /// Draw one move, with a stroke width in pixels.
    fn stroke(&self, canvas: &mut Canvas, m: &Move, colour: [u8; 4], alpha: f32, width_px: f32) {
        let a = self.project(m.from);
        let b = self.project(m.to);
        if !a.iter().chain(b.iter()).all(|v| v.is_finite()) {
            return;
        }
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt();
        if width_px <= 1.0 || len <= f32::EPSILON {
            canvas.line(a, b, colour, alpha);
            return;
        }
        // Thickness by two parallel passes half a stroke either side of the
        // centre line. Cheaper than a polygon rasteriser and, at these widths,
        // indistinguishable from one.
        let off = (width_px - 1.0) * 0.5;
        let (nx, ny) = (-dy / len * off, dx / len * off);
        canvas.line(
            [a[0] - nx, a[1] - ny],
            [b[0] - nx, b[1] - ny],
            colour,
            alpha,
        );
        canvas.line(
            [a[0] + nx, a[1] + ny],
            [b[0] + nx, b[1] + ny],
            colour,
            alpha,
        );
    }
}

/// The pixel buffer being drawn into, non-premultiplied RGBA8.
struct Canvas<'a> {
    w: i32,
    h: i32,
    px: &'a mut [u8],
}

impl Canvas<'_> {
    /// A line, sampled along its length and laid down with bilinear coverage —
    /// a poor relation of Wu's algorithm that costs four blends a sample and
    /// looks like an antialiased line at preview sizes.
    fn line(&mut self, a: [f32; 2], b: [f32; 2], colour: [u8; 4], alpha: f32) {
        // Wholly off one edge: nothing to draw, and no steps to walk.
        let (w, h) = (self.w as f32, self.h as f32);
        if (a[0] < 0.0 && b[0] < 0.0)
            || (a[1] < 0.0 && b[1] < 0.0)
            || (a[0] >= w && b[0] >= w)
            || (a[1] >= h && b[1] >= h)
        {
            return;
        }
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt();
        let steps = ((len / SAMPLE_STEP_PX).ceil() as usize).clamp(1, MAX_STEPS_PER_LINE);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            self.plot(a[0] + dx * t, a[1] + dy * t, colour, alpha);
        }
    }

    /// One sample, spread across the four pixels it sits between.
    fn plot(&mut self, x: f32, y: f32, colour: [u8; 4], alpha: f32) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        let (fx, fy) = (x.floor(), y.floor());
        let (tx, ty) = (x - fx, y - fy);
        let (ix, iy) = (fx as i32, fy as i32);
        for (ox, oy, coverage) in [
            (0, 0, (1.0 - tx) * (1.0 - ty)),
            (1, 0, tx * (1.0 - ty)),
            (0, 1, (1.0 - tx) * ty),
            (1, 1, tx * ty),
        ] {
            self.blend(ix + ox, iy + oy, colour, alpha * coverage);
        }
    }

    fn blend(&mut self, x: i32, y: i32, colour: [u8; 4], alpha: f32) {
        if !alpha.is_finite() || alpha <= 0.0 || x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let a = (alpha.min(1.0) * (colour[3] as f32 / 255.0)).clamp(0.0, 1.0);
        let i = ((y as usize) * (self.w as usize) + x as usize) * 4;
        let Some(dst) = self.px.get_mut(i..i + 4) else {
            return;
        };
        for (d, c) in dst.iter_mut().zip(colour.iter()) {
            let was = *d as f32;
            *d = (was + (*c as f32 - was) * a).round().clamp(0.0, 255.0) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How many of a layer's moves put filament down. A test helper rather
    /// than a method: nothing in the renderer needs the count, and a `pub fn`
    /// nobody calls is a warning waiting to be silenced.
    fn extrusions(layer: &Layer) -> usize {
        layer.moves.iter().filter(|m| m.extruding).count()
    }

    /// A three-layer square in the shape a slicer writes it: a comment header
    /// with the facts in it, relative extrusion, a travel to the start of each
    /// layer, and a retraction that must not draw a line.
    ///
    /// Written by the test rather than borrowed from a real print, because a
    /// sliced file is a megabyte of somebody else's settings and everything
    /// asserted here would then be a fact about that file rather than about
    /// this parser.
    fn sample() -> String {
        let mut s = String::new();
        s.push_str("; generated by PrusaSlicer 2.7.4 on 2026-08-08\n");
        s.push_str("; layer_height = 0.2\n");
        s.push_str("M83 ; relative extrusion\n");
        s.push_str("G21 ; millimetres\n");
        s.push_str("G90\n");
        s.push_str("G28 ; home\n");
        for layer in 0..3 {
            let z = 0.2 + 0.2 * layer as f64;
            s.push_str(&format!("G1 Z{z:.1} F1200 ; layer change\n"));
            s.push_str("G1 X10 Y10 F9000 ; travel to the start\n");
            for (x, y) in [(20.0, 10.0), (20.0, 20.0), (10.0, 20.0), (10.0, 10.0)] {
                s.push_str(&format!("G1 X{x:.1} Y{y:.1} E0.4 F1800\n"));
            }
            s.push_str("G1 E-2 F2400 ; retract\n");
        }
        s.push_str("; filament used [g] = 4.66\n");
        s.push_str("; estimated printing time (normal mode) = 1h 4m 12s\n");
        s
    }

    #[test]
    fn a_sliced_square_parses_into_three_layers_of_four_walls() {
        let path = parse(&sample());
        assert_eq!(path.layer_count(), 3);
        assert_eq!(path.layers[0].z, 0.2);
        assert!(
            (path.layers[1].z - 0.4).abs() < 1e-6,
            "{:?}",
            path.layers[1].z
        );
        assert!(
            (path.layers[2].z - 0.6).abs() < 1e-6,
            "{:?}",
            path.layers[2].z
        );
        for layer in &path.layers {
            assert_eq!(extrusions(layer), 4, "four walls");
        }
        // The four walls plus the Z change, and on the first layer the travel
        // in from the home position as well. The later layers close their
        // square on the corner the next one starts at, so their travel moves
        // nowhere and is not a move — and neither is the retraction.
        assert_eq!(path.layers[0].moves.len(), 6);
        assert_eq!(path.layers[1].moves.len(), 5);
        assert_eq!(path.layers[2].moves.len(), 5);
        let (lo, hi) = path.bounds().expect("bounds");
        assert_eq!((lo[0], hi[0]), (10.0, 20.0));
        assert_eq!((lo[1], hi[1]), (10.0, 20.0));
        assert!((hi[2] - 0.6).abs() < 1e-5, "{hi:?}");
    }

    /// E going up is an extrusion; E unchanged or pulled back is a travel.
    #[test]
    fn extruding_moves_are_told_apart_from_travels_and_retractions() {
        let path = parse(&sample());
        for layer in &path.layers {
            for m in layer.moves.iter().filter(|m| m.extruding) {
                assert!(m.from[0] >= 10.0 && m.from[0] <= 20.0, "{:?}", m.from);
                assert!(m.to[0] >= 10.0 && m.to[0] <= 20.0, "{:?}", m.to);
            }
        }
        // A move with a retraction on it draws nothing, even though it moves.
        let retracting = parse("G90\nM83\nG1 Z0.2\nG1 X10 Y10 E1\nG1 X30 Y10 E-2\n");
        assert_eq!(retracting.layer_count(), 1);
        assert_eq!(extrusions(&retracting.layers[0]), 1);
        assert_eq!(retracting.layers[0].moves.len(), 3);
        // And a file of nothing but travels prints nothing, so it has no layers
        // at all — a layer exists because something was printed on it.
        let travels = parse("G90\nM83\nG1 X10 Y10 F9000\nG1 X50 Y50 F9000\n");
        assert!(travels.layers.is_empty());
    }

    /// Absolute extrusion is the other dialect, and the same square in it has
    /// to come out the same shape — including across the `G92 E0` a slicer
    /// writes at every layer, which a parser that missed it would read as one
    /// enormous retraction.
    #[test]
    fn absolute_extrusion_and_g92_read_the_same_square() {
        let mut s = String::from("G90\nM82\nG1 Z0.2\nG1 X10 Y10 F9000\n");
        let mut e = 0.0;
        for (x, y) in [(20.0, 10.0), (20.0, 20.0)] {
            e += 0.4;
            s.push_str(&format!("G1 X{x:.1} Y{y:.1} E{e:.2}\n"));
        }
        s.push_str("G92 E0\n");
        e = 0.0;
        for (x, y) in [(10.0, 20.0), (10.0, 10.0)] {
            e += 0.4;
            s.push_str(&format!("G1 X{x:.1} Y{y:.1} E{e:.2}\n"));
        }
        let path = parse(&s);
        assert_eq!(path.layer_count(), 1);
        assert_eq!(extrusions(&path.layers[0]), 4);
    }

    /// Relative positioning and relative extrusion are the third and fourth
    /// dialects, and inches are the fifth.
    #[test]
    fn relative_positioning_and_relative_extrusion_are_honoured() {
        let path = parse("M83\nG91\nG1 X10 E1\nG1 Y10 E1\n");
        let (lo, hi) = path.bounds().expect("bounds");
        assert_eq!(lo, [0.0, 0.0, 0.0]);
        assert_eq!(hi, [10.0, 10.0, 0.0]);
        // Relative extrusion: every move states its own positive delta, so all
        // of them extrude even though E never climbs in absolute terms.
        assert_eq!(extrusions(&path.layers[0]), 2);
        // In absolute extrusion the same numbers are one extrusion and one
        // move with no filament behind it.
        let absolute = parse("M82\nG91\nG1 X10 E1\nG1 Y10 E1\n");
        assert_eq!(extrusions(&absolute.layers[0]), 1);

        let inches = parse("M83\nG20\nG90\nG1 X1 E1\n");
        let (_, hi) = inches.bounds().expect("bounds");
        assert!((hi[0] - 25.4).abs() < 1e-3, "{hi:?}");
    }

    #[test]
    fn a_uniform_file_measures_two_tenths_of_a_millimetre_a_layer() {
        let path = parse(&sample());
        let h = path.layer_height_mm().expect("a height");
        assert!((h - 0.2).abs() < 1e-5, "{h}");
        // The slicer's declared height is preferred, and the moves agree with
        // it here — which is the point of checking both.
        assert_eq!(path.facts.layer_height_mm, Some(0.2));
        let measured = path.measured_layer_height().expect("a measured height");
        assert!((measured - 0.2).abs() < 1e-5, "{measured}");
    }

    /// The height a variable print measures is the median gap, not the mean:
    /// one thick first layer must not move the number.
    #[test]
    fn the_measured_layer_height_is_the_median_gap() {
        let mut s = String::from("M83\nG90\n");
        for z in [0.3f64, 0.5, 0.7, 0.9] {
            s.push_str(&format!("G1 Z{z}\nG1 X10 E1\nG1 X0 E1\n"));
        }
        let path = parse(&s);
        assert_eq!(path.layer_count(), 4);
        let h = path.measured_layer_height().expect("a height");
        assert!((h - 0.2).abs() < 1e-5, "{h}");
        // One layer has no gap to measure and says so.
        assert_eq!(parse("M83\nG1 X10 E1\n").measured_layer_height(), None);
    }

    #[test]
    fn the_slicers_comments_are_read_in_both_spellings() {
        let prusa = parse(&sample());
        assert_eq!(prusa.facts.filament_g, Some(4.66));
        assert_eq!(prusa.facts.print_time.as_deref(), Some("1h 4m 12s"));
        assert_eq!(prusa.facts.filament_field().as_deref(), Some("4.7 g"));
        assert!(prusa
            .facts
            .slicer
            .as_deref()
            .is_some_and(|s| s.starts_with("PrusaSlicer")));

        let orca = parse(concat!(
            "; generated by OrcaSlicer 1.9.0\n",
            "; filament used [g] : 12.3\n",
            "; model printing time: 3m 20s; total estimated time: 5m 51s\n",
            "M83\nG1 X1 E1\n"
        ));
        assert_eq!(orca.facts.slicer.as_deref(), Some("OrcaSlicer 1.9.0"));
        assert_eq!(orca.facts.filament_g, Some(12.3));
        // Orca writes both figures on one line; the quotation is the first.
        assert_eq!(orca.facts.print_time.as_deref(), Some("3m 20s"));
    }

    #[test]
    fn the_sniff_takes_a_real_header_and_refuses_prose_and_binary() {
        assert!(sniff(sample().as_bytes()));
        assert!(sniff(b"M104 S200\n"));
        assert!(!sniff(b"Good code is its own documentation.\nGenerally.\n"));
        assert!(!sniff(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"));
        assert!(!sniff(b""));
        // A header of nothing but comments still sniffs, because the check
        // reads past them.
        let mut commented = String::new();
        for i in 0..200 {
            commented.push_str(&format!("; setting_{i} = {i}\n"));
        }
        commented.push_str("G1 X1 E1\n");
        assert!(sniff(commented.as_bytes()));
    }

    #[test]
    fn a_file_with_no_moves_is_an_empty_toolpath_that_still_renders() {
        let ink = super::super::Ink::test();
        for text in ["", "; just a header\n", "M104 S200\nG28\n"] {
            let path = parse(text);
            assert_eq!(path.layer_count(), 0, "{text:?}");
            assert!(path.bounds().is_none(), "{text:?}");
            assert_eq!(path.layer_height_mm(), None);
            let out = render(&path, 0, 16, 16, &ink);
            assert_eq!(out.width, 16);
            assert_eq!(out.pixels.len(), 16 * 16 * 4);
            assert!(
                out.pixels.chunks_exact(4).all(|p| p == ink.bg.as_slice()),
                "an empty toolpath is a background-coloured canvas"
            );
        }
    }

    /// The same twenty-millimetre square on every one of five layers, with no
    /// travel that leaves it: identical geometry layer to layer, which is what
    /// makes the projection's stability measurable.
    fn stack() -> String {
        let mut s = String::from("M83\nG90\nG21\n");
        for layer in 0..5 {
            s.push_str(&format!("G1 Z{:.1}\n", 0.2 + 0.2 * layer as f64));
            for (x, y) in [(20.0, 0.0), (20.0, 20.0), (0.0, 20.0), (0.0, 0.0)] {
                s.push_str(&format!("G1 X{x:.1} Y{y:.1} E0.4\n"));
            }
        }
        s
    }

    /// The ink-covered area of a render, as the box it fits in.
    fn marked_box(r: &super::super::Rgba, bg: [u8; 4]) -> Option<(u32, u32, u32, u32)> {
        let mut lo = (u32::MAX, u32::MAX);
        let mut hi = (0u32, 0u32);
        let mut any = false;
        for (i, p) in r.pixels.chunks_exact(4).enumerate() {
            if p == bg.as_slice() {
                continue;
            }
            any = true;
            let (x, y) = ((i as u32) % r.width, (i as u32) / r.width);
            lo = (lo.0.min(x), lo.1.min(y));
            hi = (hi.0.max(x), hi.1.max(y));
        }
        any.then_some((lo.0, lo.1, hi.0, hi.1))
    }

    #[test]
    fn a_rendered_layer_draws_something_and_draws_it_the_same_way_twice() {
        let ink = super::super::Ink::test();
        let path = parse(&sample());
        let a = render(&path, 2, 96, 64, &ink);
        let b = render(&path, 2, 96, 64, &ink);
        assert_eq!((a.width, a.height), (96, 64));
        assert_eq!(a.pixels.len(), 96 * 64 * 4);
        assert!(
            a.pixels.chunks_exact(4).any(|p| p != ink.bg.as_slice()),
            "the layer has to leave a mark"
        );
        assert_eq!(
            a.pixels, b.pixels,
            "the same layer twice is the same picture"
        );
    }

    /// The one property the whole view is built around: the projection comes
    /// from the model's bounds, so walking up the layers of a prism must not
    /// move or rescale it by a pixel.
    #[test]
    fn stepping_through_layers_never_moves_or_rescales_the_part() {
        let ink = super::super::Ink::test();
        let path = parse(&stack());
        assert_eq!(path.layer_count(), 5);
        let boxes: Vec<_> = (0..5)
            .map(|l| {
                marked_box(&render(&path, l, 80, 80, &ink), ink.bg).expect("ink on the canvas")
            })
            .collect();
        for b in &boxes {
            assert_eq!(*b, boxes[0], "{boxes:?}");
        }
        // …and it really is a square in the middle, not the whole canvas.
        let (x0, y0, x1, y1) = boxes[0];
        assert!(x0 > 0 && y0 > 0 && x1 < 79 && y1 < 79, "{:?}", boxes[0]);
    }

    #[test]
    fn a_layer_index_past_the_end_clamps_to_the_top_of_the_part() {
        let ink = super::super::Ink::test();
        let path = parse(&sample());
        let top = render(&path, path.layer_count() - 1, 64, 64, &ink);
        let past = render(&path, 9_999, 64, 64, &ink);
        assert_eq!(top.pixels, past.pixels);
    }

    #[test]
    fn a_degenerate_canvas_or_a_degenerate_model_never_panics() {
        let ink = super::super::Ink::test();
        let path = parse(&sample());
        // No canvas at all.
        let empty = render(&path, 0, 0, 0, &ink);
        assert_eq!((empty.width, empty.height), (0, 0));
        assert!(empty.pixels.is_empty());
        assert_eq!(render(&path, 0, 8, 0, &ink).width, 0);
        // A canvas nobody could want is refused rather than allocated.
        assert_eq!(render(&path, 0, 100_000, 100_000, &ink).width, 0);
        // One pixel, and a model with no XY extent at all — a bead extruded
        // straight up, whose span is zero on both axes.
        let dot = parse("G90\nM83\nG1 Z0.2\nG1 Z0.4 E1\n");
        assert_eq!(dot.layer_count(), 1);
        let out = render(&dot, 0, 1, 1, &ink);
        assert_eq!(out.pixels.len(), 4);
        let _ = render(&dot, 0, 32, 32, &ink);

        // Coordinates no parse can produce but a caller could hand over.
        let nasty = Toolpath {
            layers: vec![Layer {
                z: f32::NAN,
                moves: vec![
                    Move {
                        from: [f32::NAN, 0.0, 0.0],
                        to: [10.0, 10.0, 0.0],
                        extruding: true,
                    },
                    Move {
                        from: [0.0, 0.0, 0.0],
                        to: [f32::INFINITY, 1.0e30, 0.0],
                        extruding: true,
                    },
                    Move {
                        from: [0.0, 0.0, 0.0],
                        to: [10.0, 10.0, 0.0],
                        extruding: false,
                    },
                ],
            }],
            facts: Facts::default(),
        };
        let out = render(&nasty, 0, 32, 32, &ink);
        assert_eq!(out.pixels.len(), 32 * 32 * 4);
        // The finite endpoints still give bounds, and nothing panicked.
        let (lo, hi) = nasty.bounds().expect("the finite points still bound");
        assert!(
            lo.iter().chain(hi.iter()).all(|v| v.is_finite()),
            "{lo:?} {hi:?}"
        );
    }

    #[test]
    fn the_summary_names_the_layers_the_height_the_size_and_the_slicer() {
        let path = parse(&sample());
        assert_eq!(
            summary(&path),
            "3 layers · 0.20 mm · 10 × 10 × 0 mm · PrusaSlicer 2.7.4"
        );
        // A bare toolpath says only what its moves prove.
        let bare = parse("G90\nM83\nG1 Z0.2\nG1 X0 Y0\nG1 X5 Y5 E1\n");
        assert_eq!(summary(&bare), "1 layer · 5 × 5 × 0 mm");
        assert_eq!(summary(&Toolpath::default()), "no printed layers");
    }
}
