//! Ruler guides on the canvas.
//!
//! Ported from figcraft's `crates/ui-egui/src/guides.rs`, which is the
//! specification: the guide lines, the rulers they are dragged out of, and
//! the gestures that make, move and remove them.
//!
//! # What a guide is
//!
//! A guide is a line on the page that a layer snaps to. It belongs to the page
//! or to a top-level frame and is kept in that owner's **own** coordinates: a
//! frame's guides run along the frame's axes and span only the frame, so a
//! guide on a rotated frame is rotated with it, and a page's guides are page
//! lines across the whole canvas.
//!
//! # What this layer does
//!
//! - it draws the page's guides and its top-level frames' guides over the
//!   artboard, under the rulers;
//! - a drag out of a ruler makes a new guide, dropped on the frame under the
//!   pointer (in that frame's coordinates) or on the page;
//! - a drag of a guide moves it; a drag back onto **its own** ruler removes it;
//! - a click on a guide selects it, so the keyboard can delete or nudge it;
//! - layers snap to the guides, which the engine already does
//!   (`figcraft_engine::snap`).
//!
//! # The commands
//!
//! Every write goes through the engine, like everything else in Spool:
//!
//! | command | params |
//! |---|---|
//! | `guide.add` | `vertical`, `offset`, and `frame` when the guide is a frame's |
//! | `guide.move` | `index`, `offset`, and `frame` |
//! | `guide.remove` | `index`, and `frame` |
//! | `guide.select` | `index`, and `frame` |
//!
//! A frame guide names its frame; a page guide needs no owner.
//!
//! # Where a guide lives
//!
//! A guide is neither a CSS property nor a session fact: it is one of the
//! facts `lamine.yaml` owns (Owner B in
//! `docs/02-document-and-source-model.md`, which names guides explicitly), and
//! the manifest already has the spelling — `spool_source::manifest::NodeRecord::guides`,
//! read back onto the node by `spool_source::reader`. The engine's `Document`
//! is where it lives while the project is open.
//!
//! Two gaps in the round trip are **not** this layer's to close, and are
//! reported rather than papered over:
//!
//! - the save path builds the manifest it writes from the *authored* metadata
//!   and the document's identity, so a guide added in the editor does not
//!   reach `lamine.yaml` yet (`app/src/engine/source_edits.rs`, the manifest
//!   builder);
//! - the page is not a managed node, so a **page** guide has no record to live
//!   in, while a frame's does.
//!
//! # What is not here
//!
//! figcraft measures the distances from a dragged guide to the layers it
//! crosses while Alt is held. That measurement is written into the engine's
//! published overlay, which in Spool is the *snap* guide overlay — a different
//! thing with the same name — so it is left out rather than overwriting it.

use figcraft_doc::{Document, NodeId};
use figcraft_engine::guides as engine;
use figcraft_engine::ViewState;
use figcraft_geom::Point;
use gpui::{Bounds, Pixels, Point as GPoint};
use serde_json::{json, Value};

use super::{page_to_screen, Viewport};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The rulers' thickness, in logical pixels: the canvas's own ruler bar.
pub const RULER: f32 = 20.0;

/// How close the pointer must be to a guide to grab it, in logical pixels.
const HIT_PX: f32 = 3.0;

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// A ruler guide, placed on the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// The page or the frame the guide belongs to.
    pub owner: NodeId,
    /// Its index in that owner's list of guides.
    pub index: usize,
    /// `true` for a vertical guide (an x position), `false` for a horizontal one.
    pub vertical: bool,
    /// Its offset in the owner's own coordinates.
    pub offset: f64,
    /// A frame guide's segment on the page. `None` for a page guide, which crosses the canvas.
    pub line: Option<[Point; 2]>,
}

/// The gesture in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Drag {
    /// A new guide, dragged out of a ruler.
    New {
        /// `true` for a guide dragged out of the left ruler: a vertical one.
        vertical: bool,
    },
    /// Guide `index` of `owner`, being moved. `copy` (Alt held at the press) drops a new one.
    Move {
        /// The page or frame the guide belongs to.
        owner: NodeId,
        /// Its index in that owner's list of guides.
        index: usize,
        /// `true` for a vertical guide.
        vertical: bool,
        /// Whether the press held Alt: the guide is copied rather than moved.
        copy: bool,
    },
}

impl Drag {
    /// Whether the guide runs down the screen.
    pub fn vertical(self) -> bool {
        match self {
            Drag::New { vertical } | Drag::Move { vertical, .. } => vertical,
        }
    }
}

/// What the guides layer remembers between frames.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GuideState {
    /// The gesture in progress.
    pub drag: Option<Drag>,
    /// Where the dragged guide would land.
    pub preview: Option<Placed>,
}

// ---------------------------------------------------------------------------
// Where the guides are
// ---------------------------------------------------------------------------

/// The page's guides and those of its top-level frames (the frames' last, so
/// they are found first).
///
/// A frame's guides are only shown while the frame is: a frame moved into
/// another keeps its guides, but they stop showing and snapping until it is
/// back on the page.
pub fn placed_guides(doc: &Document, page: NodeId) -> Vec<Placed> {
    let mut out: Vec<Placed> = doc
        .node(page)
        .map(|n| n.guides())
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(index, g)| Placed {
            owner: page,
            index,
            vertical: g.vertical,
            offset: g.offset,
            line: None,
        })
        .collect();
    for f in doc.top_frames(page) {
        let Some(n) = doc.node(f).filter(|n| n.visible) else {
            continue;
        };
        out.extend(
            n.guides()
                .iter()
                .enumerate()
                .filter_map(|(index, g)| place(doc, f, index, g.vertical, g.offset)),
        );
    }
    out
}

/// A guide of `owner` at `offset`, as a segment on the page.
fn place(
    doc: &Document,
    owner: NodeId,
    index: usize,
    vertical: bool,
    offset: f64,
) -> Option<Placed> {
    Some(Placed {
        owner,
        index,
        vertical,
        offset,
        line: Some(engine::segment(doc, owner, vertical, offset)?),
    })
}

/// The offset of a guide of `owner` through the page point `p`, in the owner's
/// coordinates: rounded to whole pixels with `snap`, else to hundredths.
pub fn through(
    doc: &Document,
    page: NodeId,
    owner: NodeId,
    index: usize,
    vertical: bool,
    p: Point,
    snap: bool,
) -> Option<Placed> {
    let v = engine::offset_at(doc, owner, vertical, p)?;
    let offset = if snap {
        v.round()
    } else {
        (v * 100.0).round() / 100.0
    };
    if owner == page {
        Some(Placed {
            owner,
            index,
            vertical,
            offset,
            line: None,
        })
    } else {
        place(doc, owner, index, vertical, offset)
    }
}

/// A guide dragged out of a ruler and dropped at the page point `p`: on the
/// top-level frame under it, in that frame's coordinates and along the frame's
/// axis nearer the ruler's, or on the page.
pub fn drop_target(
    doc: &Document,
    page: NodeId,
    p: Point,
    vertical: bool,
    snap: bool,
) -> Option<Placed> {
    match engine::drop_owner(doc, page, p) {
        f if f != page => through(
            doc,
            page,
            f,
            usize::MAX,
            engine::frame_axis(doc, f, vertical),
            p,
            snap,
        ),
        page => through(doc, page, page, usize::MAX, vertical, p, snap),
    }
}

/// A placed guide as a page line — a vertical guide's x, or a horizontal one's
/// y — when it runs along the page's axes.
pub fn page_line(doc: &Document, g: &Placed) -> Option<(bool, f64)> {
    match g.line {
        None => Some((g.vertical, g.offset)),
        Some(_) => engine::line_in(doc.world_transform(g.owner), g.vertical, g.offset),
    }
}

// ---------------------------------------------------------------------------
// On screen
// ---------------------------------------------------------------------------

/// The guide's line on screen: a page guide across the canvas, a frame guide
/// across its frame.
fn segment(
    g: &Placed,
    view: &ViewState,
    viewport: Viewport,
    canvas: Bounds<Pixels>,
) -> [GPoint<f32>; 2] {
    match g.line {
        Some([a, b]) => [
            page_to_screen(view, viewport, a.x, a.y),
            page_to_screen(view, viewport, b.x, b.y),
        ],
        None if g.vertical => {
            let x = page_to_screen(view, viewport, g.offset, 0.0).x;
            let (top, bottom) = (f32::from(canvas.top()), f32::from(canvas.bottom()));
            [gpui::point(x, top), gpui::point(x, bottom)]
        }
        None => {
            let y = page_to_screen(view, viewport, 0.0, g.offset).y;
            let (left, right) = (f32::from(canvas.left()), f32::from(canvas.right()));
            [gpui::point(left, y), gpui::point(right, y)]
        }
    }
}

/// The guide within a few points of `at` — the frame guides and the later ones
/// first, so the one on top is the one that is found.
pub fn guide_at(
    doc: &Document,
    page: NodeId,
    at: GPoint<f32>,
    view: &ViewState,
    viewport: Viewport,
    canvas: Bounds<Pixels>,
) -> Option<Placed> {
    placed_guides(doc, page).into_iter().rev().find(|g| {
        let [a, b] = segment(g, view, viewport, canvas);
        distance_to_segment(at, a, b) <= HIT_PX
    })
}

/// The distance from `p` to the segment `a`-`b`.
fn distance_to_segment(p: GPoint<f32>, a: GPoint<f32>, b: GPoint<f32>) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len_sq = dx * dx + dy * dy;
    let t = if len_sq > 1e-6 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len_sq).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (x, y) = (a.x + dx * t, a.y + dy * t);
    ((p.x - x).powi(2) + (p.y - y).powi(2)).sqrt()
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

/// A guide command's params: a frame guide names its frame, a page guide needs
/// no owner.
pub fn owned(owner: NodeId, page: NodeId, mut params: Value) -> Value {
    if owner != page {
        if let Some(o) = params.as_object_mut() {
            o.insert("frame".into(), json!(owner));
        }
    }
    params
}
