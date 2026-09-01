//! The chrome around the panes: the tab strip, the top row (crumbs, prompt and
//! status cluster), the which-key card and the help browser.
//!
//! All four are painted, like everything else in delightfile, straight onto an
//! [`egui::Painter`] — see [`crate::ui`]'s header for why there are no widgets
//! here. They are in their own file because they are the pieces that *float*:
//! the panes own the window's space and these four are laid over or beside it,
//! and keeping them apart means the pane painter never grows a special case for
//! "unless the help is open".
//!
//! ## The card look
//!
//! One card style, shared (ported from delightviewer's `ui::card`): a
//! near-opaque plate of the palette's darkest ground, a hairline edge to lift it
//! off whatever it covers, and generous rounding. The which-key card and the
//! help overlay are the same surface at two sizes, which is what makes them read
//! as one program rather than two panels somebody drew on different days.
//!
//! Nested rounding is concentric throughout (`delightful-ui` §15): a row inside
//! a card is inset by [`CARD_PAD`] and its radius is the card's minus that
//! inset, so the gap around a highlighted row stays a constant width as it turns
//! the card's corner.

use df_core::fs::is_case_sensitive;

use crate::help::{Help, HelpLine};
use crate::hover::{pressed_rect, Hovers};
use crate::input::Prompt;
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting, CHROME_HEIGHT, GAP, ROW_RADIUS};

/// The padding inside a card, and the inset its rows get on every adjacent
/// side (`delightful-ui` §15's even insets).
pub const CARD_PAD: f32 = 10.0;

/// A row inside a card rounds like a row inside a pane: they are the same kind
/// of thing at the same size, and two radii for one shape would be two.
pub const CARD_ROW_RADIUS: u8 = ROW_RADIUS;

/// The floating card's corner radius: **its row's plus the padding**, so the
/// gap around a highlighted row stays a constant width as it turns the card's
/// corner. Derived, never picked.
pub const CARD_RADIUS: u8 = CARD_ROW_RADIUS + CARD_PAD as u8;

/// How far a floating card keeps off the window's edge, and off the chrome it
/// sits above.
pub const CARD_MARGIN: f32 = 16.0;

/// How opaque a card's plate is, 0–255.
///
/// Not quite opaque: a hint of the pane beneath is what says the card is a
/// temporary thing lying on top rather than a region of the window that has
/// changed. Below about 220 the file names underneath start showing through the
/// text on the card, which is where "layered" turns into "unreadable".
const CARD_ALPHA: u8 = 242;

/// Body text on the chrome, in logical points. A shade under the pane's row
/// text: this is the frame, not the content.
pub const FONT: f32 = 12.5;

/// A line of a card's list.
pub const CARD_ROW: f32 = 20.0;

/// Horizontal padding inside a bar or a chip.
pub const PAD_X: f32 = 8.0;

/// A chip's inset inside the top row, on **every** adjacent side
/// (`delightful-ui` §15's even insets).
///
/// Three: the row is [`CHROME_HEIGHT`] tall and a chip has to keep enough of
/// its own plate to read as a pill, so this is about as much as the row can
/// give away and still have two distinguishable surfaces.
pub const CHIP_INSET: f32 = 3.0;

/// A chip's corner radius: **the row's less its inset**, so the gap between a
/// chip and the row's own corner stays a constant width as it turns
/// (`delightful-ui` §15). Derived, never picked.
pub const CHIP_RADIUS: u8 = ROW_RADIUS - CHIP_INSET as u8;

/// How far a chip's plate is tinted towards its accent at rest.
///
/// The number every count on the top row has always been drawn with, named
/// now that the hover and the fade multiply it: a wash, not a fill — the text
/// on the chip is what is being read, and a plate at much above this starts
/// competing with it.
const CHIP_TINT: f32 = 0.16;

// ── Tab strip (PLAN §2) ─────────────────────────────────────────────────────

/// The gap between two tab chips. Half the window's [`GAP`]: the chips are one
/// group and should read as one, so they sit closer to each other than the
/// strip does to the panes below it.
const TAB_GAP: f32 = 4.0;

/// The widest a tab chip gets, in logical points.
///
/// Directory names are short and a strip of nine equal chips across a 1400 pt
/// window would give each one 150 pt of mostly empty plate. Capping the width
/// keeps two tabs looking like two tabs rather than like a segmented control
/// that has taken over the top of the window.
const TAB_MAX_WIDTH: f32 = 190.0;

/// How far an inactive chip's plate is lifted off the window ground. Small:
/// the strip's job is to show *which* tab is active, so the inactive ones are
/// nearly the ground itself.
const TAB_INACTIVE_LIFT: f32 = 0.5;

/// Where each tab's chip goes.
///
/// Shared by the paint and the hit test, so a click lands on the chip it looks
/// like it landed on — two functions computing this separately is how a strip
/// grows a one-pixel lie at its edges.
pub fn tab_rects(strip: egui::Rect, count: usize) -> Vec<egui::Rect> {
    if count == 0 {
        return Vec::new();
    }
    let total_gap = TAB_GAP * (count - 1) as f32;
    let width = ((strip.width() - total_gap) / count as f32).min(TAB_MAX_WIDTH);
    (0..count)
        .map(|i| {
            let left = strip.left() + i as f32 * (width + TAB_GAP);
            egui::Rect::from_min_size(
                egui::pos2(left, strip.top()),
                egui::vec2(width.max(0.0), strip.height()),
            )
        })
        .collect()
}

/// Which tab chip a point is over, if any.
pub fn tab_at(strip: egui::Rect, count: usize, pos: egui::Pos2) -> Option<usize> {
    tab_rects(strip, count)
        .into_iter()
        .position(|rect| rect.contains(pos))
}

/// Draw the strip. Only called with two or more tabs (PLAN §2).
///
/// The chips sit on the window's own ground rather than inside a plate of
/// their own, so `delightful-ui` §15's concentric rule has nothing to be
/// concentric *with* here: a chip is [`ROW_RADIUS`], the same as a row in a
/// pane, because it is the same kind of thing at the same size. The nested
/// case is the top row, where the chips do sit inside a rounded container —
/// see [`CHIP_RADIUS`].
pub fn tab_strip(
    paint: &Painting<'_>,
    strip: egui::Rect,
    titles: &[String],
    active: usize,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    for (index, (rect, title)) in tab_rects(strip, titles.len())
        .into_iter()
        .zip(titles)
        .enumerate()
    {
        let key = Control::Tab(index);
        let is_active = index == active;
        let hover = hovers.hover(key);
        let ground = mix(palette.crust, palette.surface0, TAB_INACTIVE_LIFT);
        let fill = if is_active {
            // The active chip is the *pane's* ground, so the tab and the column
            // it belongs to are visibly one surface.
            mix(palette.base, palette.blue, 0.10)
        } else {
            mix(ground, palette.surface1, hover)
        };
        let rect = pressed_rect(rect, hovers.press(key));
        paint.painter.rect_filled(rect, ROW_RADIUS, fill);
        if is_active {
            // The same 2 pt accent rule the focused pane wears (PLAN §2.1), on
            // the same edge, because it is saying the same thing.
            let rule = egui::Rect::from_min_max(
                egui::pos2(rect.left() + ROW_RADIUS as f32, rect.top()),
                egui::pos2(rect.right() - ROW_RADIUS as f32, rect.top() + 2.0),
            );
            paint.painter.rect_filled(rule, 1, palette.blue);
        }

        let inside = paint.painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }
        let color = if is_active {
            palette.text
        } else {
            palette.overlay1
        };
        // The number is what `1`–`9` press, so it is on the chip rather than in
        // the help sheet: the strip teaches its own shortcut.
        inside.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{}", index + 1),
            key_font(FONT - 1.0),
            palette.overlay0,
        );
        let text_left = rect.left() + PAD_X + FONT;
        truncated(
            &inside,
            egui::pos2(text_left, rect.center().y),
            title,
            color,
            (rect.right() - PAD_X - text_left).max(0.0),
        );
    }
}

// ── The breadcrumb path bar (PLAN §2) ───────────────────────────────────────

/// The separator drawn between two crumbs.
///
/// A chevron rather than the platform's `/`: the slash is *in* the path, and a
/// separator that looks like content makes a segment's own name ambiguous the
/// moment a directory has a slash-like character in it.
const CRUMB_SEPARATOR: &str = "›";

/// The separator's column, in logical points. Symmetric padding either side of
/// a one-character glyph.
const CRUMB_SEPARATOR_WIDTH: f32 = 13.0;

/// What is drawn when the path is too long for the bar.
const CRUMB_ELLIPSIS: &str = "…";

/// One clickable segment of the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    /// What is drawn: the directory's own name, or `/` for the root.
    pub label: String,
    /// Where clicking goes.
    pub path: std::path::PathBuf,
    /// Draw this segment as a **chip** — a plate in the accent colour — rather
    /// than as plain text.
    ///
    /// A local path has none: every segment of it is a directory on this
    /// machine and they are all the same kind of thing. The virtual locations
    /// have exactly one, and it is the first (PLAN §7.4, §7.6): the machine a
    /// remote listing is on, or the word `Trash`. Neither is a folder, and a
    /// bar reading `showandtour1 › srv › www` in one colour would look like a
    /// directory called `showandtour1` on this computer — which is precisely
    /// the mistake those two features must not invite.
    pub accent: bool,
}

/// The path, as segments from the root rightwards.
///
/// The last one is the directory you are in. It is still a crumb and still
/// clickable — clicking it is a no-op navigation, which is exactly what a user
/// who clicked it expects, and special-casing it would mean one segment of the
/// bar behaves differently from all the others for no visible reason.
pub fn crumbs(path: &std::path::Path) -> Vec<Crumb> {
    use std::path::Component;
    let mut out = Vec::new();
    let mut here = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => {
                here.push("/");
                out.push(Crumb {
                    label: "/".to_string(),
                    path: here.clone(),
                    accent: false,
                });
            }
            Component::Normal(name) => {
                here.push(name);
                out.push(Crumb {
                    label: name.to_string_lossy().into_owned(),
                    path: here.clone(),
                    accent: false,
                });
            }
            // A relative path's `.`/`..`/prefix components cannot be clicked to
            // anywhere meaningful, so they are pushed onto the accumulator and
            // not offered as segments. In practice every path here is absolute.
            other => here.push(other.as_os_str()),
        }
    }
    out
}

/// Where each crumb goes, sharing the measurement with the paint so a click
/// lands on the segment it looks like it landed on.
///
/// A crumb that did not fit gets [`egui::Rect::NOTHING`], which contains no
/// point — so an elided segment is simply not hit-testable, and the vector
/// stays index-aligned with `crumbs`.
pub fn crumb_rects(
    painter: &egui::Painter,
    bar: egui::Rect,
    crumbs: &[Crumb],
    reserved_right: f32,
) -> Vec<egui::Rect> {
    let font = egui::FontId::proportional(FONT);
    let widths: Vec<f32> = crumbs
        .iter()
        .map(|crumb| text_width(painter, &crumb.label, font.clone()) + PAD_X * 2.0)
        .collect();
    let room = (bar.width() - PAD_X * 2.0 - reserved_right).max(0.0);
    // Elide from the *left*: the segment you are in and the ones just above it
    // are what a person is reading, and the root is the part they can guess.
    let mut first = 0;
    loop {
        let shown = &widths[first..];
        let separators = CRUMB_SEPARATOR_WIDTH * shown.len().saturating_sub(1) as f32;
        let ellipsis = if first > 0 {
            text_width(painter, CRUMB_ELLIPSIS, font.clone()) + CRUMB_SEPARATOR_WIDTH
        } else {
            0.0
        };
        if shown.iter().sum::<f32>() + separators + ellipsis <= room || first + 1 >= widths.len() {
            break;
        }
        first += 1;
    }

    let mut rects = vec![egui::Rect::NOTHING; crumbs.len()];
    let mut x = bar.left() + PAD_X;
    if first > 0 {
        x += text_width(painter, CRUMB_ELLIPSIS, font.clone()) + CRUMB_SEPARATOR_WIDTH;
    }
    for (index, width) in widths.iter().enumerate().skip(first) {
        if index > first {
            x += CRUMB_SEPARATOR_WIDTH;
        }
        rects[index] = egui::Rect::from_min_size(
            // Inset by the same amount on every adjacent side as every other
            // chip on this row, so the row has one nesting rule and not two.
            egui::pos2(x, bar.top() + CHIP_INSET),
            egui::vec2(*width, (bar.height() - CHIP_INSET * 2.0).max(0.0)),
        );
        x += width;
    }
    rects
}

/// The text of the breadcrumb's git chip: the branch, and the dirty count once
/// a status has landed (PLAN §7.3).
///
/// `main ·3` — the branch, a middle dot, and one number. **One** number, not
/// git's four: the chip is read at a glance while doing something else, and its
/// job is to answer "is there anything uncommitted here", which is a yes or a
/// no with a magnitude. The breakdown lives in the terminal the user is going to
/// type `git status` into anyway.
///
/// The count is omitted entirely when the tree is clean and when no scan has
/// landed yet, and those two are deliberately the same picture: a chip that read
/// `main ·0` for the half-second before the first status came back would be a
/// lie, and one that showed a spinner would be motion asking to be watched. The
/// branch is what the breadcrumb is for; the count arrives when it arrives.
///
/// Pure, so the formatting is a test rather than a repository.
pub fn branch_label(branch: &str, counts: Option<df_core::git::DirtyCounts>) -> String {
    match counts {
        Some(counts) if !counts.is_clean() => format!("{branch} ·{}", counts.total()),
        _ => branch.to_string(),
    }
}

// ── The top row's right-hand cluster ────────────────────────────────────────

/// What the top row says about the listing, on its right-hand end.
///
/// One row, read right to left: the position counter is the number that is
/// *always* true and so is always in the same place at the far end; the git
/// chip is next because it is about the directory rather than the cursor; and
/// the status chips — the ones that are only there when they have something to
/// say — grow leftwards from those two towards the crumbs.
pub struct Cluster<'a> {
    /// How many files are selected (PLAN §4.1's `Space`/`Ctrl+a`/`v`).
    pub selected: usize,
    /// Visual mode, and which kind — the one piece of modal state in the
    /// browser, so it has to be visible somewhere that is not the row colours.
    pub visual: Option<bool>,
    /// What `y` / `x` is holding (PLAN §4.1), if anything.
    pub yank: Option<Yank<'a>>,
    /// The branch chip's text, when git has an answer (PLAN §7.3).
    pub branch: Option<&'a str>,
    /// Where the cursor is, 1-based, and how many rows there are.
    pub position: usize,
    pub rows: usize,
}

/// The clipboard, as the top row sees it.
///
/// The row marks in the current directory only answer "is *this* file yanked";
/// a yank made two directories ago is invisible until you paste it somewhere
/// you did not mean to. The chip is the other half of that fact, and it is on
/// the one strip of chrome that is on screen wherever you have wandered to.
pub struct Yank<'a> {
    pub paths: &'a [std::path::PathBuf],
    /// `x` rather than `y` — the same distinction the row marks draw.
    pub cut: bool,
    /// The chip's fade: 1 while there is a clipboard, and its eased way out
    /// after `X` (PLAN §8's instant-in, eased-out).
    pub alpha: f32,
}

/// The most names the yank chip's tooltip lists.
///
/// Six: enough that a yank of a handful is entirely readable, few enough that
/// the card stays something the eye takes in rather than a file listing
/// floating over the file listing. Past that it says how many more there are,
/// which is the question a longer list would have been answering anyway.
const YANK_TOOLTIP_NAMES: usize = 6;

/// Where each piece of the cluster is, measured before the crumbs so they know
/// how much of the row is left (and so a click lands on the chip it looks like
/// it landed on).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClusterGeom {
    /// What the crumbs must keep clear on the right, the separating gap
    /// included.
    pub width: f32,
    pub counter: egui::Rect,
    pub git: Option<egui::Rect>,
    pub yank: Option<egui::Rect>,
    pub selected: Option<egui::Rect>,
    pub visual: Option<egui::Rect>,
}

/// `12 / 340`, or `0 / 0` for an empty directory.
fn counter_text(cluster: &Cluster<'_>) -> String {
    if cluster.rows == 0 {
        "0 / 0".to_string()
    } else {
        format!("{} / {}", cluster.position, cluster.rows)
    }
}

/// The yank chip's label: `3 yanked` / `3 cut`.
pub fn yank_label(count: usize, cut: bool) -> String {
    format!("{count} {}", if cut { "cut" } else { "yanked" })
}

/// The chip's own colour, matching the mark on the rows it is about.
fn yank_color(palette: &crate::theme::Palette, cut: bool) -> egui::Color32 {
    if cut {
        palette.peach
    } else {
        palette.teal
    }
}

/// Lay the cluster out, right to left.
pub fn cluster_geometry(
    painter: &egui::Painter,
    row: egui::Rect,
    cluster: &Cluster<'_>,
) -> ClusterGeom {
    let font = egui::FontId::proportional(FONT);
    let inner = row.shrink2(egui::vec2(PAD_X, 0.0));
    let counter_w = text_width(painter, &counter_text(cluster), font.clone());
    let counter = egui::Rect::from_min_max(
        egui::pos2(inner.right() - counter_w, inner.top()),
        egui::pos2(inner.right(), inner.bottom()),
    );
    let mut right = counter.left();
    // A chip is inset from the row on every adjacent side and rounded
    // concentrically with it (`delightful-ui` §15) — see [`CHIP_INSET`].
    let chip_at = |width: f32, right: &mut f32| {
        let rect = egui::Rect::from_min_max(
            egui::pos2(*right - GAP - width, row.top() + CHIP_INSET),
            egui::pos2(*right - GAP, row.bottom() - CHIP_INSET),
        );
        *right = rect.left();
        rect
    };
    let git = cluster.branch.map(|branch| {
        // The glyph's column plus its gap is the `FONT` the branch text is set
        // in: one em is what a single-character ornament needs beside a word.
        let width = text_width(painter, branch, font.clone()) + PAD_X * 2.0 + FONT;
        chip_at(width, &mut right)
    });
    let yank = cluster
        .yank
        .as_ref()
        .filter(|y| !y.paths.is_empty())
        .map(|y| {
            let label = yank_label(y.paths.len(), y.cut);
            let width = text_width(painter, &label, font.clone()) + PAD_X * 2.0;
            chip_at(width, &mut right)
        });
    let selected = (cluster.selected > 0).then(|| {
        let label = format!("{} selected", cluster.selected);
        let width = text_width(painter, &label, font.clone()) + PAD_X * 2.0;
        chip_at(width, &mut right)
    });
    let visual = cluster.visual.map(|selecting| {
        let width = text_width(painter, visual_label(selecting), font.clone()) + PAD_X * 2.0;
        chip_at(width, &mut right)
    });
    ClusterGeom {
        // The gap between the cluster and the crumbs is the window's own, so
        // the two groups on one row are separated by the same distance as
        // everything else on it.
        width: (row.right() - right + GAP).max(0.0),
        counter,
        git,
        yank,
        selected,
        visual,
    }
}

fn visual_label(selecting: bool) -> &'static str {
    if selecting {
        "visual"
    } else {
        "visual unset"
    }
}

/// Draw the cluster into the rectangles [`cluster_geometry`] measured.
fn paint_cluster(
    paint: &Painting<'_>,
    cluster: &Cluster<'_>,
    geom: &ClusterGeom,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    // `12 / 340`, right-aligned: the one number that is always true, in the one
    // place it can be found without reading.
    painter.text(
        egui::pos2(geom.counter.right(), geom.counter.center().y),
        egui::Align2::RIGHT_CENTER,
        counter_text(cluster),
        egui::FontId::proportional(FONT),
        palette.overlay1,
    );
    if let (Some(rect), Some(branch)) = (geom.git, cluster.branch) {
        // The branch, in the palette's own git colour, on a plate of it — the
        // same chip treatment every count on this row gets.
        plate(paint, rect, palette.mauve, 1.0);
        painter.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            // A plain branch glyph, in the proportional face: the nerd-font
            // icons need a patched font that may not be there, and the row must
            // read the same either way.
            "⑂",
            egui::FontId::proportional(FONT),
            palette.mauve,
        );
        painter.text(
            egui::pos2(rect.left() + PAD_X + FONT, rect.center().y),
            egui::Align2::LEFT_CENTER,
            branch,
            egui::FontId::proportional(FONT),
            palette.mauve,
        );
    }
    if let (Some(rect), Some(yank)) = (geom.yank, &cluster.yank) {
        let accent = yank_color(palette, yank.cut);
        let hover = hovers.hover(Control::YankChip);
        let rect = pressed_rect(rect, hovers.press(Control::YankChip));
        // Lifted a little under the pointer, because it is the one chip on the
        // row that *does* something when clicked (it clears the clipboard, the
        // same as `X`).
        plate(paint, rect, accent, yank.alpha * (1.0 + hover * 0.6));
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(Control::YankChip, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * yank.alpha * 255.0).round() as u8),
            );
        }
        inside.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            yank_label(yank.paths.len(), yank.cut),
            egui::FontId::proportional(FONT),
            fade(accent, yank.alpha),
        );
    }
    if let Some(rect) = geom.selected {
        // The count is in the selection's own colour, on a plate of it: the
        // badge and the yellow bars down the column are visibly the same fact,
        // said twice, in the two places the eye looks.
        chip(
            paint,
            rect,
            &format!("{} selected", cluster.selected),
            palette.yellow,
        );
    }
    if let (Some(rect), Some(selecting)) = (geom.visual, cluster.visual) {
        // Visual mode is the browser's one piece of modal state, and a mode you
        // cannot see is a mode you get caught in.
        chip(paint, rect, visual_label(selecting), palette.sky);
    }
}

/// The yank chip's tooltip: what is actually on the clipboard.
///
/// Hung off the chip rather than shown in a toast, because it answers a
/// question the user asked by pointing at it — and a yank the user has
/// forgotten the contents of is exactly the yank that pastes the wrong files.
fn yank_tooltip(
    paint: &Painting<'_>,
    area: egui::Rect,
    rect: egui::Rect,
    yank: &Yank<'_>,
    warm: f32,
) {
    if warm <= 0.0 || yank.paths.is_empty() {
        return;
    }
    let painter = paint.painter;
    let palette = paint.palette;
    let mut lines: Vec<String> = yank
        .paths
        .iter()
        .take(YANK_TOOLTIP_NAMES)
        .map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        })
        .collect();
    if yank.paths.len() > YANK_TOOLTIP_NAMES {
        lines.push(format!(
            "and {} more",
            yank.paths.len() - YANK_TOOLTIP_NAMES
        ));
    }
    let font = egui::FontId::proportional(FONT);
    let width = lines.iter().fold(0.0f32, |m, line| {
        m.max(text_width(painter, line, font.clone()))
    }) + CARD_PAD * 2.0;
    let height = lines.len() as f32 * CARD_ROW + CARD_PAD * 2.0;
    // Under the chip and right-aligned with it: the row is at the top of the
    // window, so there is no room above, and hanging the card off the chip's
    // own edge keeps it pointing at what it is about.
    let left = (rect.right() - width).max(area.left() + CARD_MARGIN);
    let card_rect = egui::Rect::from_min_size(
        egui::pos2(left, rect.bottom() + CHIP_INSET),
        egui::vec2(width, height),
    );
    card(paint, card_rect, warm);
    for (i, line) in lines.iter().enumerate() {
        painter.text(
            egui::pos2(
                card_rect.left() + CARD_PAD,
                card_rect.top() + CARD_PAD + i as f32 * CARD_ROW + CARD_ROW / 2.0,
            ),
            egui::Align2::LEFT_CENTER,
            line,
            font.clone(),
            fade(
                if i < YANK_TOOLTIP_NAMES {
                    palette.subtext0
                } else {
                    palette.overlay0
                },
                warm,
            ),
        );
    }
}

// ── The top row ─────────────────────────────────────────────────────────────

/// Everything the top row's geometry has to agree about: where the crumbs go,
/// where the committed filter's chip goes, and where the cluster's chips are.
///
/// Measured once a frame and handed to both the hit test and the paint, for the
/// reason [`tab_rects`] is shared: two functions computing this separately is
/// how a row grows a one-pixel lie at its edges.
pub struct TopGeom {
    pub crumbs: Vec<egui::Rect>,
    /// The committed `f` filter's trailing chip, when there is one.
    pub filter: Option<egui::Rect>,
    pub cluster: ClusterGeom,
}

/// The glyph the filter chip wears: a search lens, in the proportional face for
/// the reason the branch glyph is.
const FILTER_GLYPH: &str = "⌕";

/// How wide the filter chip is, so the crumbs can be measured against what is
/// left. Zero when nothing is filtered.
fn filter_width(painter: &egui::Painter, filter: &str) -> f32 {
    if filter.is_empty() {
        return 0.0;
    }
    text_width(painter, filter, egui::FontId::proportional(FONT))
        + PAD_X * 2.0
        + FONT
        + CRUMB_SEPARATOR_WIDTH
}

/// Lay the whole top row out.
pub fn top_geometry(
    painter: &egui::Painter,
    row: egui::Rect,
    crumbs: &[Crumb],
    filter: &str,
    cluster: &Cluster<'_>,
) -> TopGeom {
    let cluster_geom = cluster_geometry(painter, row, cluster);
    let filter_w = filter_width(painter, filter);
    let rects = crumb_rects(painter, row, crumbs, cluster_geom.width + filter_w);
    // After the last crumb that fitted, with a separator's worth of space
    // before it: the committed filter reads as one more step down the path,
    // because that is what it is — `Downloads › ⌕ invoice` is the directory
    // and then the part of it you are looking at (PLAN §7.2).
    let filter_rect = (filter_w > 0.0)
        .then(|| rects.iter().rev().find(|r| **r != egui::Rect::NOTHING))
        .flatten()
        .map(|last| {
            egui::Rect::from_min_max(
                egui::pos2(last.right() + CRUMB_SEPARATOR_WIDTH, row.top() + CHIP_INSET),
                egui::pos2(last.right() + filter_w, row.bottom() - CHIP_INSET),
            )
        });
    TopGeom {
        crumbs: rects,
        filter: filter_rect,
        cluster: cluster_geom,
    }
}

/// The top row's ground: the window's own, so it reads as part of the frame
/// rather than as a fourth pane.
fn bar_ground(paint: &Painting<'_>, rect: egui::Rect) -> egui::Rect {
    paint.painter.rect_filled(
        rect,
        ROW_RADIUS,
        mix(paint.palette.crust, paint.palette.base, 0.5),
    );
    rect.shrink2(egui::vec2(PAD_X, 0.0))
}

/// Draw the top row in browse mode: the crumbs, the filter chip, and the
/// cluster (PLAN §2, §7.2, §7.3).
#[allow(clippy::too_many_arguments)] // a painter's arguments are its inputs
pub fn path_bar(
    paint: &Painting<'_>,
    area: egui::Rect,
    bar: egui::Rect,
    crumbs: &[Crumb],
    filter: &str,
    cluster: &Cluster<'_>,
    geom: &TopGeom,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    bar_ground(paint, bar);
    let font = egui::FontId::proportional(FONT);
    let rects = &geom.crumbs;

    // The leading ellipsis, when the path did not fit.
    let elided = rects.iter().position(|r| *r != egui::Rect::NOTHING);
    if elided.unwrap_or(0) > 0 {
        painter.text(
            egui::pos2(bar.left() + PAD_X, bar.center().y),
            egui::Align2::LEFT_CENTER,
            CRUMB_ELLIPSIS,
            font.clone(),
            palette.overlay0,
        );
    }

    let last = crumbs.len().saturating_sub(1);
    for (index, (crumb, rect)) in crumbs.iter().zip(rects).enumerate() {
        if *rect == egui::Rect::NOTHING {
            continue;
        }
        let key = Control::Crumb(index);
        let hover = hovers.hover(key);
        let rect = pressed_rect(*rect, hovers.press(key));
        if crumb.accent {
            // The chip that says "this is not a folder on this machine" — the
            // same plate treatment the git branch wears at the other end of the
            // row, so the two read as one kind of ornament (PLAN §7.4, §7.6).
            plate(paint, rect, palette.sky, 1.0 + hover * 0.9);
        } else if hover > 0.0 {
            painter.rect_filled(
                rect,
                CHIP_RADIUS,
                mix(paint.palette.crust, palette.surface1, hover),
            );
        }
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }
        // The directory you are actually in is the bright one; the ancestors
        // are the route you took to it. A chip has its own ink for the same
        // reason it has its own plate.
        let color = if crumb.accent {
            palette.sky
        } else if index == last {
            palette.text
        } else {
            mix(palette.overlay1, palette.text, hover)
        };
        inside.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &crumb.label,
            font.clone(),
            color,
        );
        let has_next = rects
            .get(index + 1)
            .is_some_and(|next| *next != egui::Rect::NOTHING);
        if (index < last && has_next) || (index == last && geom.filter.is_some()) {
            painter.text(
                egui::pos2(rect.right() + CRUMB_SEPARATOR_WIDTH / 2.0, bar.center().y),
                egui::Align2::CENTER_CENTER,
                CRUMB_SEPARATOR,
                font.clone(),
                palette.overlay0,
            );
        }
    }

    // The committed filter, as the trailing crumb it behaves like: clicking it
    // re-opens the prompt that set it, which is the only way the pointer has of
    // editing a query the keyboard typed.
    if let Some(rect) = geom.filter {
        let key = Control::FilterChip;
        let hover = hovers.hover(key);
        let rect = pressed_rect(rect, hovers.press(key));
        plate(paint, rect, palette.blue, 1.0 + hover * 0.9);
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }
        inside.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            FILTER_GLYPH,
            font.clone(),
            palette.blue,
        );
        inside.text(
            egui::pos2(rect.left() + PAD_X + FONT, rect.center().y),
            egui::Align2::LEFT_CENTER,
            filter,
            font.clone(),
            palette.blue,
        );
    }

    paint_cluster(paint, cluster, &geom.cluster, hovers, ripples);
    if let (Some(rect), Some(yank)) = (geom.cluster.yank, &cluster.yank) {
        // …and the card leaves with the chip it hangs off, rather than
        // outliving the fact it is explaining.
        yank_tooltip(
            paint,
            area,
            rect,
            yank,
            hovers.hover(Control::YankChip) * yank.alpha,
        );
    }
}

/// The top row while something is being typed into it: `f`, `/`, `?`, `a`, `;`
/// and the help browser's own filter (PLAN §4.2).
///
/// The prompt takes the crumbs' place rather than opening a line of its own:
/// the keyboard is in one place at a time, and a row that grew under the panes
/// every time a query was typed would move rows under the pointer
/// (`delightful-ui` §8's spatial stability).
pub fn prompt_row(paint: &Painting<'_>, row: egui::Rect, prompt: &Prompt, tail: Option<&str>) {
    let palette = paint.palette;
    let mut inner = bar_ground(paint, row);
    // The accent rule says the keyboard is *here* and not in the list — the
    // same 2 pt mark a focused pane wears, for the same reason (PLAN §2.1).
    paint.painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(row.left() + ROW_RADIUS as f32, row.top()),
            egui::pos2(row.right() - ROW_RADIUS as f32, row.top() + 2.0),
        ),
        1,
        palette.blue,
    );

    // The row grew a second line for an error that would not fit beside the
    // query: the field keeps the first line and the error gets the second.
    let error_line = (row.height() > CHROME_HEIGHT + 1.0).then(|| {
        let split = row.top() + CHROME_HEIGHT;
        let line = egui::Rect::from_min_max(
            egui::pos2(inner.left(), split),
            egui::pos2(inner.right(), row.bottom()),
        );
        inner = egui::Rect::from_min_max(inner.min, egui::pos2(inner.right(), split));
        line
    });

    // The directory the prompt is about, kept at the far left as context when
    // there is room for it: a filter with no idea what it is filtering is a
    // text field floating in a window.
    if let Some(tail) = tail {
        let font = egui::FontId::proportional(FONT);
        let width = text_width(paint.painter, tail, font.clone()) + CRUMB_SEPARATOR_WIDTH;
        if inner.width() - width >= PROMPT_MIN_WIDTH {
            paint.painter.text(
                egui::pos2(inner.left(), inner.center().y),
                egui::Align2::LEFT_CENTER,
                tail,
                font.clone(),
                palette.overlay0,
            );
            paint.painter.text(
                egui::pos2(
                    inner.left() + width - CRUMB_SEPARATOR_WIDTH / 2.0,
                    inner.center().y,
                ),
                egui::Align2::CENTER_CENTER,
                CRUMB_SEPARATOR,
                font,
                palette.overlay0,
            );
            inner =
                egui::Rect::from_min_max(egui::pos2(inner.left() + width, inner.top()), inner.max);
        }
    }
    prompt_field(paint, inner, prompt, error_line);
}

/// How many lines the top row needs while `prompt` is open.
///
/// Two only when there is an error and no room to say it beside the query:
/// an inline error that has been squeezed to three characters is not an error
/// message, and a row that is always two lines tall would cost the panes a
/// line for a state they are usually not in.
pub fn prompt_lines(painter: &egui::Painter, prompt: &Prompt, width: f32) -> usize {
    let Some(error) = &prompt.error else {
        return 1;
    };
    let font = egui::FontId::proportional(FONT);
    let wanted = text_width(painter, prompt.kind.title(), font.clone())
        + PAD_X
        + text_width(painter, prompt.query(), font.clone())
        + PAD_X
        + text_width(painter, error, font)
        + PAD_X
        + text_width(painter, prompt.mode_label(), key_font(FONT - 1.5))
        + 10.0;
    if wanted > width - PAD_X * 2.0 {
        2
    } else {
        1
    }
}

/// One labelled pill, in the rectangle the geometry gave it.
fn chip(paint: &Painting<'_>, rect: egui::Rect, text: &str, accent: egui::Color32) {
    plate(paint, rect, accent, 1.0);
    paint.painter.text(
        egui::pos2(rect.left() + PAD_X, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(FONT),
        accent,
    );
}

/// A chip's plate: the window's darkest ground tinted towards the chip's own
/// accent, at `strength` (1 at rest, more under a pointer, less on the way
/// out).
fn plate(paint: &Painting<'_>, rect: egui::Rect, accent: egui::Color32, strength: f32) {
    let tint = mix(
        paint.palette.crust,
        accent,
        CHIP_TINT * strength.clamp(0.0, 2.0),
    );
    paint
        .painter
        .rect_filled(rect, CHIP_RADIUS, fade(tint, strength.min(1.0)));
}

/// How wide a floating rename prompt is, and how far it may hang past its row.
///
/// Anchored prompts sit over the row they rename (PLAN §4.2's yazi geometry),
/// so the width is the row's — a popup narrower than its own file name would be
/// the one field in the program you cannot see the end of.
const PROMPT_MIN_WIDTH: f32 = 260.0;

/// The floating prompt: `r`, `R`, and the conflict dialog's rename.
///
/// Drawn over `anchor` — the cursor's row — because that is the thing being
/// renamed, and the eye is already there.
pub fn prompt_popup(paint: &Painting<'_>, area: egui::Rect, anchor: egui::Rect, prompt: &Prompt) {
    let rect = prompt_rect(area, anchor);
    card(paint, rect, 1.0);
    prompt_field(paint, rect.shrink2(egui::vec2(CARD_PAD, 0.0)), prompt, None);
}

/// Where a floating prompt goes. Shared with the hit test so a click lands
/// inside the field it looks like it landed in.
pub fn prompt_rect(area: egui::Rect, anchor: egui::Rect) -> egui::Rect {
    let width = anchor
        .width()
        .max(PROMPT_MIN_WIDTH)
        .min(area.width() - CARD_MARGIN * 2.0);
    let height = CHROME_HEIGHT + CARD_PAD;
    let left = anchor
        .left()
        .min(area.right() - CARD_MARGIN - width)
        .max(area.left() + CARD_MARGIN);
    // Centred on the row, so the name being edited does not appear to jump to
    // another line as the popup opens (`delightful-ui` §8).
    let top = (anchor.center().y - height / 2.0).clamp(
        area.top() + CARD_MARGIN,
        (area.bottom() - CARD_MARGIN - height).max(area.top() + CARD_MARGIN),
    );
    egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(width.max(0.0), height))
}

/// The prompt itself, in whatever box it has been given: title, mode chip, the
/// text with its selection, the caret, and the inline error.
///
/// `error_line` is the second line the top row grew when the error would not
/// fit beside the query; with `None` the error keeps its place on the line, as
/// it does in an anchored popup.
fn prompt_field(
    paint: &Painting<'_>,
    inner: egui::Rect,
    prompt: &Prompt,
    error_line: Option<egui::Rect>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    let font = egui::FontId::proportional(FONT);

    let title_galley =
        painter.layout_no_wrap(prompt.kind.title().to_string(), font.clone(), palette.blue);
    painter.galley(
        egui::pos2(inner.left(), inner.center().y - title_galley.size().y / 2.0),
        title_galley.clone(),
        palette.blue,
    );

    // ── The right-hand furniture, measured first so the text knows its room ──
    // The mode chip is the outermost thing on the line: it is the answer to
    // "why did that letter not type", and it must be findable in the same place
    // every time (`delightful-ui` §8).
    let mode = prompt.mode_label();
    let mode_color = match mode {
        "INSERT" => palette.green,
        "VISUAL" => palette.mauve,
        "REPLACE" => palette.peach,
        _ => palette.blue,
    };
    let mode_galley = painter.layout_no_wrap(mode.to_string(), key_font(FONT - 1.5), mode_color);
    let chip_width = mode_galley.size().x + 10.0;
    let chip_rect = egui::Rect::from_min_size(
        egui::pos2(
            inner.right() - chip_width,
            inner.center().y - (mode_galley.size().y + 4.0) / 2.0,
        ),
        egui::vec2(chip_width, mode_galley.size().y + 4.0),
    );
    painter.rect_filled(chip_rect, 4, mix(paint.palette.crust, mode_color, 0.18));
    painter.galley(
        egui::pos2(chip_rect.left() + 5.0, chip_rect.top() + 2.0),
        mode_galley,
        mode_color,
    );

    let mut right = chip_rect.left() - PAD_X;
    if let (Some(error), Some(line)) = (&prompt.error, error_line) {
        // The row grew for this: the error gets a line of its own, under the
        // query it is about, rather than being squeezed into three characters
        // beside it.
        painter.text(
            egui::pos2(line.left(), line.center().y),
            egui::Align2::LEFT_CENTER,
            error,
            font.clone(),
            palette.red,
        );
    } else if let Some(error) = &prompt.error {
        // The error takes the place the case indicator would have had: it is
        // the more urgent thing to say about what has been typed.
        let galley = painter.layout_no_wrap(error.clone(), font.clone(), palette.red);
        let width = galley.size().x.min((right - inner.left()).max(0.0));
        painter.galley(
            egui::pos2(right - width, inner.center().y - galley.size().y / 2.0),
            galley,
            palette.red,
        );
        right -= width + PAD_X;
    } else if prompt.kind.is_live() {
        // The smart-case indicator: lit when the query has a capital in it and
        // is therefore case-*sensitive* (df-core's rule, PLAN §7.2). Dim the
        // rest of the time — it reports a mode nobody chose, so it must not
        // shout.
        let (color, text) = if is_case_sensitive(prompt.query()) {
            (palette.yellow, "Aa")
        } else {
            (palette.overlay0, "aa")
        };
        let galley = painter.layout_no_wrap(text.to_string(), key_font(FONT - 0.5), color);
        let width = galley.size().x;
        painter.galley(
            egui::pos2(right - width, inner.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        right -= width + PAD_X;
    }

    // ── The line ────────────────────────────────────────────────────────────
    let text_left = inner.left() + title_galley.size().x + PAD_X;
    let room = (right - text_left).max(0.0);
    let painter = painter.with_clip_rect(egui::Rect::from_min_max(
        egui::pos2(text_left, inner.top()),
        egui::pos2(text_left + room, inner.bottom()),
    ));
    let query = prompt.query();
    let width_of = |upto: usize| -> f32 {
        let upto = upto.min(query.len());
        painter
            .layout_no_wrap(query[..upto].to_string(), font.clone(), palette.text)
            .size()
            .x
    };

    if let Some(range) = prompt.selection() {
        // A visual run is a *region*, so it is drawn as one rather than as
        // differently coloured letters.
        let (from, to) = (
            text_left + width_of(range.start),
            text_left + width_of(range.end),
        );
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(from, inner.top() + 4.0),
                egui::pos2(to.max(from + 2.0), inner.bottom() - 4.0),
            ),
            2,
            mix(paint.palette.crust, palette.mauve, 0.35),
        );
    }

    painter.text(
        egui::pos2(text_left, inner.center().y),
        egui::Align2::LEFT_CENTER,
        query,
        font.clone(),
        palette.text,
    );

    // The caret. **Never blinking** — PLAN §4.2 says no blink, and a blink is
    // an animation that never stops asking for frames (PLAN §1). A block in
    // Normal, where the caret sits *on* a character; a bar in Insert, where it
    // sits between two.
    let caret = prompt.caret();
    let caret_x = text_left + width_of(caret);
    let caret_width = if prompt.block_caret() {
        let next = query[caret.min(query.len())..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(0);
        if next == 0 {
            CARET_WIDTH * 4.0
        } else {
            (width_of(caret + next) - width_of(caret)).max(CARET_WIDTH)
        }
    } else {
        CARET_WIDTH
    };
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(caret_x, inner.top() + 5.0),
            egui::pos2(caret_x + caret_width, inner.bottom() - 5.0),
        ),
        0,
        // A block caret is drawn *behind* nothing — egui has no blend mode for
        // "invert" — so it is the accent at a weight that leaves the glyph
        // readable through it.
        if prompt.block_caret() {
            mix(paint.palette.crust, palette.blue, 0.55)
        } else {
            palette.blue
        },
    );
    if prompt.block_caret() {
        // …and the character is redrawn over the block, so the caret never eats
        // the letter it is standing on.
        let under: String = query[caret.min(query.len())..].chars().take(1).collect();
        if !under.is_empty() {
            painter.text(
                egui::pos2(caret_x, inner.center().y),
                egui::Align2::LEFT_CENTER,
                under,
                font,
                palette.crust,
            );
        }
    }
}

/// The insert caret's width, in points. One-and-a-half rather than one: a
/// hairline caret disappears against a busy line at fractional scaling.
pub const CARET_WIDTH: f32 = 1.5;

// ── An overlay's hints (PLAN §4) ───────────────────────────────────────────

/// The height of the hint strip along the bottom of an overlay card.
///
/// Sixteen: a line of [`HINT_FONT`] plus the air a line of text needs under a
/// list of rows to read as a footnote rather than as one more row.
pub const HINT_ROW: f32 = 16.0;

/// The hints' text size. Under the card's own body text, because the hints are
/// what you can do next and the card is what you are doing now — a footer that
/// matched the body would be a second thing to read (`ui-anti-slop`: hierarchy
/// by size, not by decoration).
const HINT_FONT: f32 = FONT - 1.0;

/// Where a card's hints go: the strip inside its bottom padding.
///
/// Every overlay reserves [`HINT_ROW`] for this in its own geometry, so the
/// strip is *inside* the plate and the rows above it never run under it.
pub fn hint_rect(card: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(card.left() + CARD_PAD, card.bottom() - CARD_PAD - HINT_ROW),
        egui::pos2(card.right() - CARD_PAD, card.bottom() - CARD_PAD),
    )
}

/// What the keys do now, along the bottom of the surface that owns them.
///
/// On the overlay rather than on a strip of window chrome: a hint is about the
/// card it belongs to, and the eye that is reading the card should not have to
/// travel to the other end of the window to find out what `Enter` does there.
pub fn hints(paint: &Painting<'_>, rect: egui::Rect, hints: &[(&str, &str)]) {
    let painter = paint.painter;
    let painter = painter.with_clip_rect(rect);
    let mut x = rect.left();
    for (keys, what) in hints {
        let key_galley = painter.layout_no_wrap(
            keys.to_string(),
            key_font(HINT_FONT),
            paint.palette.subtext0,
        );
        painter.galley(
            egui::pos2(x, rect.center().y - key_galley.size().y / 2.0),
            key_galley.clone(),
            paint.palette.subtext0,
        );
        x += key_galley.size().x + 6.0;
        let what_galley = painter.layout_no_wrap(
            what.to_string(),
            egui::FontId::proportional(HINT_FONT),
            paint.palette.overlay0,
        );
        painter.galley(
            egui::pos2(x, rect.center().y - what_galley.size().y / 2.0),
            what_galley.clone(),
            paint.palette.overlay0,
        );
        x += what_galley.size().x + GAP * 2.0;
    }
}

// ── The which-key card (PLAN §4, §8) ────────────────────────────────────────

/// The most continuations one column shows before the card grows a second one.
///
/// The `g` chord has a dozen bookmarks and the `,` chord thirteen sorts; a
/// single column of those is a tower up the middle of the window that the eye
/// has to scan end to end. Nine is about the length a list is still taken in at
/// a glance rather than read.
const WHICH_KEY_COLUMN: usize = 9;

/// Between a key and what it does.
const WHICH_KEY_GAP: f32 = 14.0;

/// Between two columns of the card. Wider than the key/label gap by enough that
/// the columns are unambiguously separate groups.
const WHICH_KEY_COL_SEP: f32 = 26.0;

/// Draw the card listing what could finish the pending chord.
///
/// `alpha` is [`crate::whichkey::WhichKey::alpha`] — 1 while the card is up, and
/// its fade on the way out. `bottom` is what the card sits above: the bar, so
/// the card never covers the thing it is a hint about.
pub fn which_key(
    paint: &Painting<'_>,
    area: egui::Rect,
    bottom: f32,
    rows: &[(String, String)],
    alpha: f32,
) {
    if rows.is_empty() || alpha <= 0.0 {
        return;
    }
    let painter = paint.painter;
    let columns: Vec<&[(String, String)]> = rows.chunks(WHICH_KEY_COLUMN).collect();
    let measure = |group: &[(String, String)]| {
        let key_w = group.iter().fold(0.0f32, |m, (key, _)| {
            m.max(text_width(painter, key, key_font(FONT)))
        });
        let label_w = group.iter().fold(0.0f32, |m, (_, label)| {
            m.max(text_width(painter, label, egui::FontId::proportional(FONT)))
        });
        (key_w, key_w + WHICH_KEY_GAP + label_w)
    };
    let widths: Vec<(f32, f32)> = columns.iter().map(|g| measure(g)).collect();
    let tall = columns.iter().map(|g| g.len()).max().unwrap_or(0);
    let size = egui::vec2(
        widths.iter().map(|(_, w)| w).sum::<f32>()
            + WHICH_KEY_COL_SEP * (columns.len().saturating_sub(1)) as f32
            + CARD_PAD * 2.0,
        tall as f32 * CARD_ROW + CARD_PAD * 2.0,
    );
    // Bottom-anchored and horizontally centred: the card is an answer to
    // something the hand is doing right now, so it belongs where the eyes are —
    // and near the bar, which is where every other transient thing appears.
    let rect = egui::Rect::from_min_size(
        egui::pos2(
            (area.center().x - size.x / 2.0).max(area.left() + CARD_MARGIN),
            (bottom - CARD_MARGIN - size.y).max(area.top() + CARD_MARGIN),
        ),
        size,
    );
    card(paint, rect, alpha);

    let mut left = rect.left() + CARD_PAD;
    for (group, (key_w, col_w)) in columns.iter().zip(&widths) {
        for (i, (key, label)) in group.iter().enumerate() {
            let y = rect.top() + CARD_PAD + i as f32 * CARD_ROW + CARD_ROW / 2.0;
            painter.text(
                egui::pos2(left, y),
                egui::Align2::LEFT_CENTER,
                key,
                key_font(FONT),
                fade(paint.palette.yellow, alpha),
            );
            painter.text(
                egui::pos2(left + key_w + WHICH_KEY_GAP, y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(FONT),
                fade(paint.palette.subtext0, alpha),
            );
        }
        left += col_w + WHICH_KEY_COL_SEP;
    }
}

// ── The help browser (PLAN §4.1's `~` / `F1`) ───────────────────────────────

/// One line of the help sheet, and the height its scrolling is measured in.
pub const HELP_ROW: f32 = 20.0;

/// The keys column's width, in logical points. Fixed rather than measured: the
/// descriptions have to start at the same x on every line or the sheet is a
/// ragged mess, and the widest chord in the shipped table (`Ctrl+Shift+z`) fits
/// inside this.
const HELP_KEYS_COLUMN: f32 = 104.0;

/// The widest the help card gets. Beyond about this a line of "keys …
/// description … command-id" is three things separated by a desert, and the eye
/// loses the row on the way across (`ui-anti-slop`: line length ≤ ~80
/// characters).
const HELP_MAX_WIDTH: f32 = 860.0;

/// How far the window behind the help is dimmed, 0–255.
///
/// A flat wash, not a gradient: it covers the whole window uniformly, so there
/// is no fade-to-transparent to ease (`delightful-ui` §14 applies to the ones
/// that *do* fade). Enough to push the panes back, little enough that you can
/// still see where you were.
pub const HELP_SCRIM: u8 = 150;

// ── Optical centring (`delightful-ui` §16) ──────────────────────────────────
// Content centred in a large region reads as sitting *low* at the true 50/50
// mark; the fix is to bias it up towards a 60/40 split. One rule, but it is
// applied to two different quantities depending on what is being centred, so
// it is two constants rather than one used two ways — and they live here, in
// the module every surface already imports, rather than as a private constant
// in one card and a bare literal in five others.

/// The share of the *slack* that goes above a centred block — a card in a
/// window, a panel in a pane. Four tenths above, six below.
pub const OPTICAL_CENTRE: f32 = 0.4;

/// The share of a *region's height* at which a single centred line of text
/// sits. Milder than [`OPTICAL_CENTRE`] because it is measuring a different
/// thing: a lone baseline in an otherwise empty pane needs a nudge, not the
/// full 60/40 a whole card wants, and at 0.4 an empty-state label reads as
/// having drifted towards the top rather than as being centred well.
pub const OPTICAL_BASELINE: f32 = 0.42;

/// Where the help card goes: most of the window, down to `bottom`.
pub fn help_rect(area: egui::Rect, bottom: f32) -> egui::Rect {
    let width = (area.width() - CARD_MARGIN * 2.0).min(HELP_MAX_WIDTH);
    egui::Rect::from_min_max(
        egui::pos2(area.center().x - width / 2.0, area.top() + CARD_MARGIN),
        egui::pos2(
            area.center().x + width / 2.0,
            bottom.max(area.top() + CARD_MARGIN + CHROME_HEIGHT),
        ),
    )
}

/// How many help lines fit in the card — its own heading row and its hint strip
/// come out of the height first.
pub fn help_page(rect: egui::Rect) -> usize {
    crate::viewport::visible_rows(
        rect.height() - CARD_PAD * 2.0 - CARD_ROW - HINT_ROW,
        HELP_ROW,
    )
}

/// Draw the help overlay: every live binding, grouped by context.
///
/// Deliberately plain (`ui-anti-slop`: no gratuitous chrome). This is a
/// reference sheet — it is read, not admired — so it is a card, a heading per
/// group, and three columns: what to press, what it does, and the id to write in
/// `keymap.toml` if you want to change it.
pub fn help_overlay(
    paint: &Painting<'_>,
    area: egui::Rect,
    rect: egui::Rect,
    lines: &[HelpLine],
    help: &Help,
    total: usize,
) {
    let painter = paint.painter;
    let palette = paint.palette;
    painter.rect_filled(area, 0, egui::Color32::from_black_alpha(HELP_SCRIM));
    card(paint, rect, 1.0);

    let shown = lines.iter().filter(|l| l.selectable()).count();
    let heading = egui::pos2(
        rect.left() + CARD_PAD,
        rect.top() + CARD_PAD + CARD_ROW / 2.0,
    );
    painter.text(
        heading,
        egui::Align2::LEFT_CENTER,
        "Keys",
        egui::FontId::proportional(FONT + 3.0),
        palette.text,
    );
    painter.text(
        egui::pos2(rect.right() - CARD_PAD, heading.y),
        egui::Align2::RIGHT_CENTER,
        if shown == total {
            format!("{total} bindings")
        } else {
            format!("{shown} of {total}")
        },
        egui::FontId::proportional(FONT),
        palette.overlay0,
    );

    let content = egui::Rect::from_min_max(
        egui::pos2(rect.left() + CARD_PAD, rect.top() + CARD_PAD + CARD_ROW),
        egui::pos2(rect.right() - CARD_PAD, rect.bottom() - CARD_PAD - HINT_ROW),
    );
    let painter = painter.with_clip_rect(content);
    let page = help_page(rect);
    for (offset, line) in lines.iter().skip(help.first).take(page + 1).enumerate() {
        let index = help.first + offset;
        let top = content.top() + offset as f32 * HELP_ROW;
        let row = egui::Rect::from_min_size(
            egui::pos2(content.left(), top),
            egui::vec2(content.width(), HELP_ROW),
        );
        match line {
            HelpLine::Group(name) => {
                painter.text(
                    egui::pos2(row.left(), row.center().y + 2.0),
                    egui::Align2::LEFT_CENTER,
                    *name,
                    egui::FontId::proportional(FONT),
                    palette.blue,
                );
            }
            HelpLine::Row(binding) => {
                if index == help.cursor {
                    painter.rect_filled(row, CARD_ROW_RADIUS, palette.surface1);
                }
                painter.text(
                    egui::pos2(row.left() + PAD_X, row.center().y),
                    egui::Align2::LEFT_CENTER,
                    &binding.keys,
                    key_font(FONT),
                    palette.yellow,
                );
                let description_left = row.left() + PAD_X + HELP_KEYS_COLUMN;
                let id_galley = painter.layout_no_wrap(
                    binding.id.clone(),
                    egui::FontId::proportional(FONT - 1.0),
                    palette.overlay0,
                );
                painter.galley(
                    egui::pos2(
                        row.right() - PAD_X - id_galley.size().x,
                        row.center().y - id_galley.size().y / 2.0,
                    ),
                    id_galley.clone(),
                    palette.overlay0,
                );
                truncated(
                    &painter,
                    egui::pos2(description_left, row.center().y),
                    &binding.description,
                    palette.subtext0,
                    (row.right() - PAD_X - id_galley.size().x - GAP - description_left).max(0.0),
                );
            }
            // A legend entry, in a binding's geometry: the mark where the keys
            // go and the meaning where the description goes, so the eye tracks
            // one pair of columns down the whole sheet.
            //
            // The mark is `subtext1` rather than the bindings' `yellow` — it is
            // a description of something on screen, not a key you press, and
            // wearing the key colour would invite people to try pressing it.
            // No third column: a mark has no id to write in `keymap.toml`.
            HelpLine::Legend(entry) => {
                painter.text(
                    egui::pos2(row.left() + PAD_X, row.center().y),
                    egui::Align2::LEFT_CENTER,
                    entry.mark,
                    egui::FontId::proportional(FONT),
                    palette.subtext1,
                );
                let meaning_left = row.left() + PAD_X + HELP_KEYS_COLUMN;
                truncated(
                    &painter,
                    egui::pos2(meaning_left, row.center().y),
                    entry.meaning,
                    palette.subtext0,
                    (row.right() - PAD_X - meaning_left).max(0.0),
                );
            }
        }
    }

    if lines.is_empty() {
        // An empty state that says what to do about it, not "no results"
        // (`delightful-ui` §11).
        painter.text(
            egui::pos2(
                content.center().x,
                content.top() + content.height() * OPTICAL_BASELINE,
            ),
            egui::Align2::CENTER_CENTER,
            "No binding matches. Backspace to widen the filter.",
            egui::FontId::proportional(FONT),
            palette.overlay0,
        );
    }
}

// ── Shared bits ─────────────────────────────────────────────────────────────

/// A card plate and its hairline edge, at `alpha`.
pub fn card(paint: &Painting<'_>, rect: egui::Rect, alpha: f32) {
    let plate = paint.palette.crust;
    let a = (alpha.clamp(0.0, 1.0) * CARD_ALPHA as f32).round() as u8;
    paint.painter.rect_filled(
        rect,
        CARD_RADIUS,
        egui::Color32::from_rgba_unmultiplied(plate.r(), plate.g(), plate.b(), a),
    );
    paint.painter.rect_stroke(
        rect,
        CARD_RADIUS,
        egui::Stroke::new(1.0, fade(paint.palette.surface1, alpha)),
        egui::StrokeKind::Inside,
    );
}

/// The same colour, at `alpha`.
pub fn fade(color: egui::Color32, alpha: f32) -> egui::Color32 {
    let a = (alpha.clamp(0.0, 1.0) * color.a() as f32).round() as u8;
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

/// The font every keyboard shortcut on the chrome is drawn in — monospace, so
/// a column of chords lines up and `l` and `1` are not the same shape.
pub fn key_font(size: f32) -> egui::FontId {
    egui::FontId::monospace(size)
}

pub fn text_width(painter: &egui::Painter, text: &str, font: egui::FontId) -> f32 {
    painter
        .layout_no_wrap(text.to_string(), font, egui::Color32::WHITE)
        .size()
        .x
}

/// Left-aligned, vertically centred, ellipsised at `max_width`.
pub fn truncated(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    color: egui::Color32,
    max_width: f32,
) {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let mut job = LayoutJob::single_section(
        text.to_string(),
        TextFormat {
            font_id: egui::FontId::proportional(FONT),
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(pos.x, pos.y - galley.size().y / 2.0),
        galley,
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(1384.0, CHROME_HEIGHT))
    }

    /// The chip is the branch alone until there is something to count, and the
    /// two states that show no number — clean, and not yet scanned — look the
    /// same on purpose.
    #[test]
    fn the_branch_chip_counts_only_when_there_is_something_to_count() {
        use df_core::git::DirtyCounts;
        assert_eq!(branch_label("main", None), "main");
        assert_eq!(branch_label("main", Some(DirtyCounts::default())), "main");
        assert_eq!(
            branch_label(
                "main",
                Some(DirtyCounts {
                    staged: 1,
                    unstaged: 2,
                    ..Default::default()
                })
            ),
            "main ·3"
        );
        // Untracked and conflicted are dirt too — a repository with one
        // unmerged path is not clean.
        assert_eq!(
            branch_label(
                "feature/long-name",
                Some(DirtyCounts {
                    untracked: 4,
                    conflicted: 1,
                    ..Default::default()
                })
            ),
            "feature/long-name ·5"
        );
        // A detached head's short hash goes through unchanged: it is whatever
        // the caller decided to call the place you are standing.
        assert_eq!(branch_label("a1b2c3d", None), "a1b2c3d");
    }

    /// The chips tile the strip left to right with one gap between, and stop
    /// growing past the cap rather than spreading over the whole window.
    #[test]
    fn the_tab_chips_are_laid_out_left_to_right() {
        let rects = tab_rects(strip(), 3);
        assert_eq!(rects.len(), 3);
        assert!((rects[0].left() - strip().left()).abs() < 1e-3);
        assert!((rects[1].left() - rects[0].right() - TAB_GAP).abs() < 1e-3);
        assert!(rects[0].width() <= TAB_MAX_WIDTH + 1e-3);
        assert!(rects.iter().all(|r| r.height() == CHROME_HEIGHT));
        assert!(tab_rects(strip(), 0).is_empty());
    }

    /// A click lands on the chip it looks like it landed on, and on nothing in
    /// the empty space past the last one.
    #[test]
    fn hit_testing_finds_the_chip_under_the_pointer() {
        let strip = strip();
        let rects = tab_rects(strip, 4);
        for (index, rect) in rects.iter().enumerate() {
            assert_eq!(tab_at(strip, 4, rect.center()), Some(index));
        }
        assert_eq!(
            tab_at(strip, 4, egui::pos2(strip.right() - 1.0, strip.center().y)),
            None
        );
        assert_eq!(tab_at(strip, 4, egui::pos2(-10.0, -10.0)), None);
    }

    /// The path, as segments you can click: the root first, the directory you
    /// are in last, and each one addressing where it points.
    #[test]
    fn the_breadcrumb_is_the_path_one_segment_at_a_time() {
        let path = crumbs(std::path::Path::new("/home/brian/Work/delightfile"));
        let labels: Vec<&str> = path.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["/", "home", "brian", "Work", "delightfile"]);
        assert_eq!(
            path.last().map(|c| c.path.as_path()),
            Some(std::path::Path::new("/home/brian/Work/delightfile"))
        );
        assert_eq!(
            path[1].path.as_path(),
            std::path::Path::new("/home"),
            "a segment addresses where it points, not where you are"
        );
        // The root on its own is one crumb, not none.
        assert_eq!(crumbs(std::path::Path::new("/")).len(), 1);
    }

    /// The crumbs tile the bar, the hit test finds what was drawn, and a path
    /// too long for the window loses its *leading* segments rather than
    /// overflowing.
    #[test]
    fn the_crumbs_are_laid_out_and_hit_tested_the_same_way() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let bar =
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(700.0, CHROME_HEIGHT));
            let path = crumbs(std::path::Path::new("/home/brian/Work/delightfile/crates"));
            let rects = crumb_rects(ui.painter(), bar, &path, 0.0);
            assert_eq!(rects.len(), path.len());
            for (index, rect) in rects.iter().enumerate() {
                assert_ne!(*rect, egui::Rect::NOTHING, "{index} did not fit a wide bar");
                assert!(bar.contains(rect.center()));
            }
            // Left to right, with a separator's worth of space between.
            for pair in rects.windows(2) {
                assert!(pair[1].left() > pair[0].right());
            }

            // A narrow bar elides from the left and keeps the tail.
            let narrow =
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(150.0, CHROME_HEIGHT));
            let rects = crumb_rects(ui.painter(), narrow, &path, 0.0);
            assert_eq!(rects[0], egui::Rect::NOTHING, "the root should be elided");
            assert_ne!(
                rects[path.len() - 1],
                egui::Rect::NOTHING,
                "the directory you are in is never elided"
            );
            // An elided segment is not hit-testable, which is what keeps a
            // click landing on the crumb it looks like it landed on.
            assert!(!rects[0].contains(narrow.center()));

            // The cluster's room comes out of the crumbs' room.
            let with_cluster = crumb_rects(ui.painter(), bar, &path, 200.0);
            assert_eq!(with_cluster.len(), path.len());
            assert!(with_cluster
                .iter()
                .filter(|r| **r != egui::Rect::NOTHING)
                .all(|r| r.right() <= bar.right() - 200.0 + 1e-3));
        });
    }

    /// `delightful-ui` §15: a row inside a card is inset by the padding and its
    /// radius is the card's less that inset, so the gap stays constant round the
    /// corner. The same rule holds for a chip inside the top row.
    #[test]
    fn the_card_radii_are_concentric() {
        assert_eq!(CARD_ROW_RADIUS as f32 + CARD_PAD, CARD_RADIUS as f32);
        assert_eq!(CHIP_RADIUS as f32 + CHIP_INSET, ROW_RADIUS as f32);
    }

    /// The right-hand cluster is laid out right to left, every chip is inside
    /// the row it is on, and what it occupies is what the crumbs are told to
    /// keep clear.
    #[test]
    fn the_cluster_reserves_what_it_occupies() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let row =
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(900.0, CHROME_HEIGHT));
            let yanked = [std::path::PathBuf::from("/tmp/a")];
            let full = Cluster {
                selected: 3,
                visual: Some(false),
                yank: Some(Yank {
                    paths: &yanked,
                    cut: true,
                    alpha: 1.0,
                }),
                branch: Some("main ·3"),
                position: 12,
                rows: 340,
            };
            let geom = cluster_geometry(ui.painter(), row, &full);
            let chips = [
                geom.counter,
                geom.git.expect("a branch was given"),
                geom.yank.expect("a clipboard was given"),
                geom.selected.expect("a selection was given"),
                geom.visual.expect("visual mode was given"),
            ];
            // Right to left, in that order, none of them overlapping.
            for pair in chips.windows(2) {
                assert!(pair[1].right() <= pair[0].left() + 1e-3);
            }
            for chip in chips {
                assert!(row.contains(chip.center()));
                assert!(chip.top() >= row.top() - 1e-3 && chip.bottom() <= row.bottom() + 1e-3);
            }
            // What it reserves reaches from the leftmost chip to the row's end.
            assert!((geom.width - (row.right() - chips[4].left() + GAP)).abs() < 1e-3);

            // A directory with nothing to say about it reserves the counter
            // alone, which is always there.
            let bare = Cluster {
                selected: 0,
                visual: None,
                yank: None,
                branch: None,
                position: 0,
                rows: 0,
            };
            let quiet = cluster_geometry(ui.painter(), row, &bare);
            assert!(quiet.git.is_none() && quiet.yank.is_none());
            assert!(quiet.selected.is_none() && quiet.visual.is_none());
            assert!(quiet.width < geom.width);

            // The committed filter is a trailing crumb, after the last one.
            let path = crumbs(std::path::Path::new("/home/brian/Downloads"));
            let top = top_geometry(ui.painter(), row, &path, "invoice", &bare);
            let filter = top.filter.expect("a filter was given");
            let last = top.crumbs.last().copied().expect("a crumb was drawn");
            assert!(filter.left() > last.right());
            assert!(filter.right() <= row.right() - quiet.width + 1e-3);
            // …and nothing is drawn for it when nothing is filtered.
            assert!(top_geometry(ui.painter(), row, &path, "", &bare)
                .filter
                .is_none());
        });
    }

    /// The chip says what it is holding and how it got there.
    #[test]
    fn the_clipboard_chip_names_its_verb() {
        assert_eq!(yank_label(3, false), "3 yanked");
        assert_eq!(yank_label(1, true), "1 cut");
    }

    /// The help card stays inside the window and above the bar, however small
    /// the window gets.
    #[test]
    fn the_help_card_fits_the_window() {
        for size in [egui::vec2(1400.0, 900.0), egui::vec2(320.0, 200.0)] {
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), size);
            let rect = help_rect(area, area.bottom() - GAP);
            assert!(rect.width() > 0.0 && rect.width() <= HELP_MAX_WIDTH + 1e-3);
            assert!(rect.left() >= area.left() && rect.right() <= area.right() + 1e-3);
            assert!(rect.top() >= area.top());
        }
    }

    /// Everything draws, including the states that are easy to forget: an empty
    /// help sheet, a one-column card, a prompt with a caret mid-string.
    #[test]
    fn the_chrome_paints_without_panicking() {
        use crate::input::{Prompt, PromptKind};
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let paint = Painting {
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd: false,
                show_symlink: true,
                now: std::time::Instant::now(),
            };
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));

            tab_strip(
                &paint,
                strip(),
                &["work".to_string(), "downloads".to_string()],
                1,
                &Hovers::new(),
                &Ripples::new(),
            );
            let path = crumbs(std::path::Path::new("/home/brian/Work/delightfile"));
            let path_rect =
                egui::Rect::from_min_size(egui::pos2(8.0, 40.0), egui::vec2(1384.0, CHROME_HEIGHT));
            let yanked = [
                std::path::PathBuf::from("/tmp/one.txt"),
                std::path::PathBuf::from("/tmp/two.txt"),
            ];
            // Every state the row can be in at once, and the bare one.
            for (branch, filter, yank) in [
                (None, "", None),
                (
                    Some("main ·3"),
                    "invoice",
                    Some(Yank {
                        paths: &yanked,
                        cut: false,
                        alpha: 1.0,
                    }),
                ),
            ] {
                let cluster = Cluster {
                    selected: 3,
                    visual: Some(true),
                    yank,
                    branch,
                    position: 12,
                    rows: 340,
                };
                let geom = top_geometry(paint.painter, path_rect, &path, filter, &cluster);
                path_bar(
                    &paint,
                    area,
                    path_rect,
                    &path,
                    filter,
                    &cluster,
                    &geom,
                    &Hovers::new(),
                    &Ripples::new(),
                );
            }
            // …and the elided case, which draws its own leading ellipsis.
            let narrow =
                egui::Rect::from_min_size(egui::pos2(8.0, 40.0), egui::vec2(90.0, CHROME_HEIGHT));
            let cluster = Cluster {
                selected: 0,
                visual: None,
                yank: None,
                branch: None,
                position: 0,
                rows: 0,
            };
            let geom = top_geometry(paint.painter, narrow, &path, "", &cluster);
            path_bar(
                &paint,
                area,
                narrow,
                &path,
                "",
                &cluster,
                &geom,
                &Hovers::new(),
                &Ripples::new(),
            );

            let mut prompt = Prompt::with(
                PromptKind::Filter,
                0,
                df_core::input::InputBuffer::new("READ", 2),
            );
            prompt_row(&paint, path_rect, &prompt, Some("delightfile"));
            // …and every mode of it, since each one draws a different caret.
            prompt.feed(df_core::keymap::Chord::plain(df_core::keymap::Key::Escape));
            prompt_row(&paint, path_rect, &prompt, None);
            prompt.feed(df_core::keymap::Chord::from_char('v').expect("v"));
            prompt_row(&paint, path_rect, &prompt, Some("delightfile"));
            // …and the two-line form, which an error too long for the line
            // asks the layout for.
            prompt.error = Some("that name is already taken by a directory".to_string());
            let tall = egui::Rect::from_min_size(
                path_rect.min,
                egui::vec2(220.0, CHROME_HEIGHT + crate::ui::PROMPT_ERROR_LINE),
            );
            assert_eq!(prompt_lines(paint.painter, &prompt, 220.0), 2);
            prompt_row(&paint, tall, &prompt, Some("delightfile"));
            prompt.error = None;
            let mut rename = Prompt::with(
                PromptKind::Rename,
                0,
                df_core::input::InputBuffer::for_rename_stem("photo.jpg"),
            );
            rename.error = Some("photo.jpg already exists".to_string());
            let row = egui::Rect::from_min_size(egui::pos2(300.0, 400.0), egui::vec2(400.0, 22.0));
            prompt_popup(&paint, area, row, &rename);
            hints(
                &paint,
                hint_rect(egui::Rect::from_min_size(
                    egui::pos2(300.0, 500.0),
                    egui::vec2(400.0, 120.0),
                )),
                &[("Esc", "close"), ("f", "filter")],
            );

            let rows: Vec<(String, String)> = (0..14)
                .map(|i| (format!("{i}"), format!("do the {i}th thing")))
                .collect();
            which_key(&paint, area, area.bottom(), &rows, 1.0);
            which_key(&paint, area, area.bottom(), &rows, 0.4);
            which_key(&paint, area, area.bottom(), &[], 1.0);

            let registry = df_core::keymap::Registry::defaults();
            let stack = df_core::keymap::ContextStack::with(&[df_core::keymap::Context::Help]);
            let all = crate::help::all_rows(&registry, &stack, df_core::keymap::WhenFlags::LIST);
            let lines = crate::help::lines(&all, "");
            let mut help = Help::default();
            help.reset(&lines);
            let rect = help_rect(area, area.bottom() - GAP);
            help_overlay(&paint, area, rect, &lines, &help, all.len());
            help_overlay(&paint, area, rect, &[], &help, all.len());
        });
    }
}
