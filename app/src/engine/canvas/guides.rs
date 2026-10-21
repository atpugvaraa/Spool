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

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use figcraft_doc::{Document, NodeId};
use figcraft_engine::guides as engine;
use figcraft_engine::tools::ToolId;
use figcraft_engine::{Session, ViewState};
use figcraft_geom::Point;
use gpui::{
    canvas, div, point, px, rgb, size, App, Bounds, Corners, CursorStyle, FocusHandle, Hitbox,
    HitboxBehavior, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, ParentElement, PathBuilder, Pixels, Point as GPoint,
    SharedString, Styled, TextRun, Window,
};
use serde_json::{json, Value};

use super::{fmt, page_to_screen, px_point, screen_to_page, Viewport};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The rulers' thickness, in logical pixels: the canvas's own ruler bar.
pub const RULER: f32 = 20.0;

/// How close the pointer must be to a guide to grab it, in logical pixels.
const HIT_PX: f32 = 3.0;

/// The colour of a ruler guide.
///
/// figcraft's orange-red. It is not a colour the canvas uses for anything
/// else, so a guide never reads as part of the artwork.
const GUIDE_HEX: u32 = 0xf24822;

/// The width of a guide line, in logical pixels. A hovered or selected guide is
/// drawn twice as wide.
const LINE_PX: f32 = 1.0;

/// The size a guide's offset is drawn at, in logical points.
const LABEL_PT: f32 = 10.0;

/// A readout's line box, as a multiple of the point size.
const LABEL_LINE: f32 = 1.2;

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
#[derive(Clone, Debug)]
pub struct GuideState {
    /// The gesture in progress.
    pub drag: Option<Drag>,
    /// Where the dragged guide would land.
    pub preview: Option<Placed>,
    /// Where the canvas is: the layer's own bounds.
    pub viewport: Viewport,
    /// The hitbox the pointer is captured on while a guide is dragged.
    pub hitbox: Option<Hitbox>,
}

impl Default for GuideState {
    fn default() -> Self {
        Self {
            drag: None,
            preview: None,
            viewport: Viewport::empty(),
            hitbox: None,
        }
    }
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
            [point(x, top), point(x, bottom)]
        }
        None => {
            let y = page_to_screen(view, viewport, 0.0, g.offset).y;
            let (left, right) = (f32::from(canvas.left()), f32::from(canvas.right()));
            [point(left, y), point(right, y)]
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

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// The colour of a ruler guide.
fn guide_colour() -> Hsla {
    Hsla::from(rgb(GUIDE_HEX))
}

/// White: a guide's offset is read on the guide's own colour.
fn white() -> Hsla {
    Hsla {
        h: 0.0,
        s: 0.0,
        l: 1.0,
        a: 1.0,
    }
}

/// The part of the segment `a`-`b` inside the box, if any.
///
/// The guides are drawn under the rulers, and the rulers are opaque, so a
/// guide that runs under one is drawn only where it shows. Liang-Barsky: the
/// segment is clipped against the four sides of the box in one pass.
fn clip_to_box(
    a: GPoint<f32>,
    b: GPoint<f32>,
    lo: GPoint<f32>,
    hi: GPoint<f32>,
) -> Option<[GPoint<f32>; 2]> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let p = [-dx, dx, -dy, dy];
    let q = [lo.x - a.x, b.x - hi.x, lo.y - a.y, b.y - hi.y];
    let (mut u1, mut u2) = (0.0_f32, 1.0_f32);
    for i in 0..4 {
        if p[i].abs() < 1e-9 {
            // The segment is parallel to this side: it misses the box when it
            // is already outside it.
            if q[i] < 0.0 {
                return None;
            }
            continue;
        }
        let r = q[i] / p[i];
        if p[i] < 0.0 {
            u1 = u1.max(r);
        } else {
            u2 = u2.min(r);
        }
        if u1 > u2 {
            return None;
        }
    }
    Some([
        point(a.x + dx * u1, a.y + dy * u1),
        point(a.x + dx * u2, a.y + dy * u2),
    ])
}

/// The guide layer's painter: page coordinates in, GPUI primitives out.
///
/// The canvas's own `Overlays` belongs to that module, so the few primitives a
/// guide needs are drawn here.
struct GuidePainter<'a> {
    window: &'a mut Window,
    app: &'a mut App,
    view: ViewState,
    viewport: Viewport,
}

impl GuidePainter<'_> {
    /// A page point on screen.
    fn at(&self, x: f64, y: f64) -> GPoint<f32> {
        page_to_screen(&self.view, self.viewport, x, y)
    }

    /// A line through two window points.
    fn line(&mut self, a: GPoint<f32>, b: GPoint<f32>, width: f32, colour: Hsla) {
        let mut path = PathBuilder::stroke(px(width));
        path.move_to(px_point(a));
        path.line_to(px_point(b));
        if let Ok(p) = path.build() {
            self.window.paint_path(p, colour);
        }
    }

    /// A rounded rectangle in `fill`.
    fn fill_radii(&mut self, bounds: Bounds<Pixels>, radius: f32, fill: Hsla) {
        if bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
            return;
        }
        self.window.paint_quad(PaintQuad {
            bounds,
            corner_radii: Corners::all(px(radius)),
            background: fill.into(),
            border_widths: Default::default(),
            border_color: white().alpha(0.0).into(),
            border_style: Default::default(),
        });
    }

    /// A readout: `text` in `ink` on a pill in `fill`, top-left at `at`.
    fn pill(&mut self, text: &str, at: GPoint<f32>, fill: Hsla, ink: Hsla) {
        let (pad_x, pad_y) = (4.0_f32, 2.0_f32);
        let height = LABEL_PT * LABEL_LINE;
        let run = TextRun {
            len: text.len(),
            font: self.window.text_style().font(),
            color: ink.into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = self.window.text_system().shape_line(
            SharedString::from(text.to_owned()),
            px(LABEL_PT),
            &[run],
            None,
        );
        let w = f32::from(line.width());
        let b = Bounds::new(
            point(px(at.x), px(at.y)),
            size(px(w + pad_x * 2.0), px(height + pad_y * 2.0)),
        );
        self.fill_radii(b, 3.0, fill);
        let _ = line.paint(
            point(px(at.x + pad_x), px(at.y + pad_y)),
            px(height),
            gpui::TextAlign::Left,
            None,
            self.window,
            self.app,
        );
    }

    /// Draw the page's and its frames' guides, and the one being dragged.
    fn draw(
        &mut self,
        doc: &Document,
        page: NodeId,
        drag: Option<Drag>,
        preview: Option<Placed>,
        hover: Option<Placed>,
        selected: Option<(NodeId, usize)>,
    ) {
        let moving = match drag {
            Some(Drag::Move {
                owner,
                index,
                copy: false,
                ..
            }) => Some((owner, index)),
            _ => None,
        };
        for g in placed_guides(doc, page) {
            if Some((g.owner, g.index)) == moving {
                continue;
            }
            let hovered = hover == Some(g) || selected == Some((g.owner, g.index));
            self.guide_line(&g, if hovered { 2.0 } else { LINE_PX });
            if hovered {
                self.label(&g);
            }
        }
        if let Some(g) = preview {
            self.guide_line(&g, LINE_PX);
            self.label(&g);
        }
    }

    /// The guide's line, clipped to the canvas area so it does not run under
    /// the rulers.
    fn guide_line(&mut self, g: &Placed, width: f32) {
        let colour = guide_colour();
        let canvas = self.viewport.bounds();
        let (lo_x, lo_y) = (
            f32::from(canvas.left()) + RULER,
            f32::from(canvas.top()) + RULER,
        );
        let (hi_x, hi_y) = (
            f32::from(canvas.right()).max(lo_x),
            f32::from(canvas.bottom()).max(lo_y),
        );
        let line = match g.line {
            Some([a, b]) => [self.at(a.x, a.y), self.at(b.x, b.y)],
            None if g.vertical => {
                let x = self.at(g.offset, 0.0).x;
                [
                    point(x, f32::from(canvas.top())),
                    point(x, f32::from(canvas.bottom())),
                ]
            }
            None => {
                let y = self.at(0.0, g.offset).y;
                [
                    point(f32::from(canvas.left()), y),
                    point(f32::from(canvas.right()), y),
                ]
            }
        };
        let Some([a, b]) = clip_to_box(line[0], line[1], point(lo_x, lo_y), point(hi_x, hi_y))
        else {
            return;
        };
        self.line(a, b, width, colour);
    }

    /// The guide's offset, in a tag next to its ruler or at the start of a
    /// frame guide.
    fn label(&mut self, g: &Placed) {
        let canvas = self.viewport.bounds();
        let [a, _] = segment(g, &self.view, self.viewport, canvas);
        // A canvas narrower or shorter than the ruler leaves min > max; keep
        // the tag at the ruler edge then.
        let (lo_x, lo_y) = (
            f32::from(canvas.left()) + RULER,
            f32::from(canvas.top()) + RULER,
        );
        let (hi_x, hi_y) = (
            f32::from(canvas.right()).max(lo_x),
            f32::from(canvas.bottom()).max(lo_y),
        );
        let at = point(a.x.clamp(lo_x, hi_x) + 4.0, a.y.clamp(lo_y, hi_y) + 4.0);
        self.pill(&fmt(g.offset), at, guide_colour(), white());
    }
}

// ---------------------------------------------------------------------------
// The cursors
// ---------------------------------------------------------------------------

/// The cursor for a guide that runs down the screen, or across it.
fn guide_cursor(vertical: bool) -> CursorStyle {
    if vertical {
        CursorStyle::ResizeLeftRight
    } else {
        CursorStyle::ResizeUpDown
    }
}

/// The resize cursor for dragging a guide across itself, whichever way it runs
/// on screen.
fn across_cursor(a: GPoint<f32>, b: GPoint<f32>) -> CursorStyle {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    // The line's direction in eighths of a turn, rounded (screen y points
    // down): 0 runs across, 2 runs down.
    let eighths = (dy.atan2(dx).to_degrees().rem_euclid(180.0) + 22.5) / 45.0;
    match eighths as u8 % 4 {
        0 => CursorStyle::ResizeUpDown,
        1 => CursorStyle::ResizeUpLeftDownRight,
        2 => CursorStyle::ResizeLeftRight,
        _ => CursorStyle::ResizeUpRightDownLeft,
    }
}

// ---------------------------------------------------------------------------
// The layer
// ---------------------------------------------------------------------------

/// What the layer reads from the session in one go.
struct Read {
    /// The document.
    doc: Arc<Document>,
    /// The page on it.
    page: NodeId,
    /// The camera.
    view: ViewState,
    /// The tool in effect.
    tool: ToolId,
    /// Whether text or a vector is being edited.
    editing: bool,
    /// The guide the engine has selected, if any.
    selected: Option<(NodeId, usize)>,
}

/// The session's state, read once.
fn read(session: &Rc<RefCell<Session>>) -> Option<Read> {
    let session = session.borrow();
    let ds = session.ds().ok()?;
    Some(Read {
        doc: ds.doc.clone(),
        page: ds.page,
        view: ds.view.clone(),
        tool: session.tool.tool,
        editing: ds.text_edit.is_some() || ds.vector_edit.is_some(),
        selected: ds.selected_guide(),
    })
}

/// The ruler-guides layer: the guide lines over the artboard, and the gestures
/// that make, move and remove them.
///
/// A plain element rather than an entity, and that is the whole design: it is
/// mounted over the canvas, and it must be drawn again whenever the canvas is
/// — a pan moves every guide on it — so it must not be cached the way an
/// entity's element is. The state it remembers between frames therefore lives
/// in a cell the canvas owns.
pub struct GuidesLayer {
    /// The engine, shared with the canvas.
    session: Rc<RefCell<Session>>,
    /// The canvas's keyboard focus. A guide is selected by a click, and the
    /// keyboard deletes and nudges it, so the click has to give the canvas the
    /// keyboard back: the canvas never sees the press, so it cannot take the
    /// focus itself.
    focus: FocusHandle,
    /// The gesture in progress, and where the canvas is.
    state: Rc<RefCell<GuideState>>,
}

impl GuidesLayer {
    /// A layer over the canvas, reading the one session.
    pub fn new(
        session: Rc<RefCell<Session>>,
        focus: FocusHandle,
        state: Rc<RefCell<GuideState>>,
    ) -> Self {
        Self {
            session,
            focus,
            state,
        }
    }

    /// The layer's element: the guide lines, and the pointer handlers.
    pub fn element(self) -> impl IntoElement {
        let down_session = Rc::clone(&self.session);
        let down_focus = self.focus.clone();
        let down_state = Rc::clone(&self.state);
        let move_session = Rc::clone(&self.session);
        let move_state = Rc::clone(&self.state);
        let up_session = Rc::clone(&self.session);
        let up_state = Rc::clone(&self.state);
        let prepaint_state = Rc::clone(&self.state);
        let paint_session = Rc::clone(&self.session);
        let paint_state = Rc::clone(&self.state);
        div()
            .id("guides-layer")
            .debug_selector(|| "guides-layer".to_string())
            .absolute()
            .size_full()
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                on_mouse_down(&down_session, &down_focus, &down_state, event, window, cx);
            })
            .on_mouse_move(move |event, window, cx| {
                on_mouse_move(&move_session, &move_state, event, window, cx);
            })
            .on_mouse_up(MouseButton::Left, move |event, window, cx| {
                on_mouse_up(&up_session, &up_state, event, window, cx);
            })
            .child(
                canvas(
                    move |bounds, window, _app| {
                        // The layer's own hitbox: the pointer is captured on it
                        // while a guide is dragged, and the cursor is set on it.
                        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                        let mut state = prepaint_state.borrow_mut();
                        state.viewport = Viewport::new(bounds);
                        state.hitbox = Some(hitbox);
                        bounds
                    },
                    move |bounds, _bounds, window, app| {
                        paint(&paint_session, &paint_state, window, app, bounds);
                    },
                )
                .absolute()
                .size_full(),
            )
    }
}

/// The pointer's place in the canvas, relative to its top-left.
fn local(state: &GuideState, at: GPoint<f32>) -> GPoint<f32> {
    point(
        at.x - state.viewport.origin.x,
        at.y - state.viewport.origin.y,
    )
}

/// Run an engine command. There is no other write path.
fn run(session: &Rc<RefCell<Session>>, id: &str, params: Value) {
    let mut session = session.borrow_mut();
    if let Err(error) = session.execute(id, &params) {
        eprintln!("spool_engine_command_failed id={id} error={error}");
    }
}

/// Take the pointer, so a drag that leaves the canvas still ends.
fn capture(state: &GuideState, window: &mut Window) {
    let Some(hitbox) = state.hitbox.as_ref() else {
        return;
    };
    window.capture_pointer(hitbox.id);
}

// --- the pointer -----------------------------------------------------------

fn on_mouse_down(
    session: &Rc<RefCell<Session>>,
    focus: &FocusHandle,
    state: &Rc<RefCell<GuideState>>,
    event: &MouseDownEvent,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(r) = read(session) else { return };
    if !r.view.rulers {
        return;
    }
    let at = point(f32::from(event.position.x), f32::from(event.position.y));
    let (in_top, in_left, bounds) = {
        let state = state.borrow();
        let local = local(&state, at);
        (
            local.y < RULER && local.x > RULER,
            local.x < RULER && local.y > RULER,
            state.viewport.bounds(),
        )
    };
    if in_top || in_left {
        // A new guide, dragged out of the ruler: the left one makes a vertical
        // guide, the top one a horizontal one.
        {
            let mut state = state.borrow_mut();
            state.drag = Some(Drag::New { vertical: in_left });
            state.preview = None;
        }
        capture(&state.borrow(), window);
        focus.focus(window, cx);
        cx.stop_propagation();
        window.refresh();
        return;
    }
    // An existing guide. It moves with the Move tool, and not while text or a
    // vector is being edited.
    if r.tool != ToolId::Move || r.editing {
        return;
    }
    let g = guide_at(&r.doc, r.page, at, &r.view, state.borrow().viewport, bounds);
    let Some(g) = g else { return };
    // The guide is selected, so the keyboard can delete or nudge it.
    run(
        session,
        "guide.select",
        owned(g.owner, r.page, json!({ "index": g.index })),
    );
    {
        let mut state = state.borrow_mut();
        state.drag = Some(Drag::Move {
            owner: g.owner,
            index: g.index,
            vertical: g.vertical,
            copy: event.modifiers.alt,
        });
        state.preview = None;
    }
    capture(&state.borrow(), window);
    focus.focus(window, cx);
    cx.stop_propagation();
    window.refresh();
}

fn on_mouse_move(
    session: &Rc<RefCell<Session>>,
    state: &Rc<RefCell<GuideState>>,
    event: &MouseMoveEvent,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(drag) = state.borrow().drag else {
        return;
    };
    let Some(r) = read(session) else { return };
    if !r.view.rulers {
        return;
    }
    let at = point(f32::from(event.position.x), f32::from(event.position.y));
    let p = screen_to_page(&r.view, state.borrow().viewport, at);
    let snap = r.view.snap_pixel;
    let preview = match drag {
        Drag::New { vertical } => drop_target(&r.doc, r.page, p, vertical, snap),
        Drag::Move {
            owner,
            index,
            vertical,
            ..
        } => through(&r.doc, r.page, owner, index, vertical, p, snap),
    };
    state.borrow_mut().preview = preview;
    // The canvas must not see this move: it would hover a layer under a guide
    // that is being dragged.
    cx.stop_propagation();
    window.refresh();
}

fn on_mouse_up(
    session: &Rc<RefCell<Session>>,
    state: &Rc<RefCell<GuideState>>,
    event: &MouseUpEvent,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(drag) = state.borrow().drag else {
        return;
    };
    let preview = state.borrow().preview;
    {
        let mut state = state.borrow_mut();
        state.drag = None;
        state.preview = None;
    }
    window.release_pointer();
    let Some(r) = read(session) else { return };
    let at = point(f32::from(event.position.x), f32::from(event.position.y));
    let (local, viewport, bounds) = {
        let state = state.borrow();
        (local(&state, at), state.viewport, state.viewport.bounds())
    };
    // The direction the guide runs on screen: a guide that runs down the
    // screen is removed by dropping it on the left ruler, one that runs across
    // by dropping it on the top ruler.
    let down = match preview {
        Some(g) => {
            let [a, b] = segment(&g, &r.view, viewport, bounds);
            (b.y - a.y).abs() >= (b.x - a.x).abs()
        }
        None => drag.vertical(),
    };
    let on_ruler = if down {
        local.x < RULER
    } else {
        local.y < RULER
    };
    // Where the guide was before the drag.
    let before = match drag {
        Drag::Move { owner, index, .. } => r
            .doc
            .node(owner)
            .and_then(|n| n.guides().get(index))
            .map(|g| g.offset),
        Drag::New { .. } => None,
    };
    match (drag, on_ruler, preview) {
        // A new guide, or a copy of one (Alt at the press).
        (Drag::New { .. } | Drag::Move { copy: true, .. }, false, Some(g))
            if Some(g.offset) != before =>
        {
            run(
                session,
                "guide.add",
                owned(
                    g.owner,
                    r.page,
                    json!({ "vertical": g.vertical, "offset": g.offset }),
                ),
            );
        }
        // Dropped back on its own ruler: the guide goes.
        (
            Drag::Move {
                owner,
                index,
                copy: false,
                ..
            },
            true,
            _,
        ) => {
            run(
                session,
                "guide.remove",
                owned(owner, r.page, json!({ "index": index })),
            );
        }
        // Moved.
        (
            Drag::Move {
                owner,
                index,
                copy: false,
                ..
            },
            false,
            Some(g),
        ) if Some(g.offset) != before => {
            run(
                session,
                "guide.move",
                owned(owner, r.page, json!({ "index": index, "offset": g.offset })),
            );
        }
        _ => {}
    }
    cx.stop_propagation();
    window.refresh();
}

// --- the picture -----------------------------------------------------------

/// Draw the guides, and give the pointer the cursor of whatever it is over.
fn paint(
    session: &Rc<RefCell<Session>>,
    state: &Rc<RefCell<GuideState>>,
    window: &mut Window,
    app: &mut App,
    bounds: Bounds<Pixels>,
) {
    let Some(r) = read(session) else { return };
    if !r.view.rulers {
        return;
    }
    let (drag, preview, viewport) = {
        let state = state.borrow();
        (state.drag, state.preview, state.viewport)
    };
    let pointer = window.mouse_position();
    let at = point(f32::from(pointer.x), f32::from(pointer.y));
    let inside = bounds.contains(&pointer);
    // The guide under the pointer, unless a guide is being dragged.
    let hover = if drag.is_none() && inside {
        guide_at(&r.doc, r.page, at, &r.view, viewport, bounds)
    } else {
        None
    };
    if let Some(style) = cursor(state, &r, at, inside, hover) {
        if let Some(hitbox) = state.borrow().hitbox.as_ref() {
            window.set_cursor_style(style, hitbox);
        }
    }
    let mut painter = GuidePainter {
        window,
        app,
        view: r.view,
        viewport,
    };
    painter.draw(&r.doc, r.page, drag, preview, hover, r.selected);
}

/// The pointer's cursor: over a ruler, that ruler's axis; over a guide, the
/// guide's own direction.
fn cursor(
    state: &Rc<RefCell<GuideState>>,
    r: &Read,
    at: GPoint<f32>,
    inside: bool,
    hover: Option<Placed>,
) -> Option<CursorStyle> {
    let state = state.borrow();
    let local = local(&state, at);
    let in_top = inside && local.y < RULER && local.x > RULER;
    let in_left = inside && local.x < RULER && local.y > RULER;
    if in_top || in_left {
        return Some(guide_cursor(in_left));
    }
    // The guide being dragged, or the one under the pointer.
    let g = match state.drag {
        Some(_) => state.preview,
        None => hover,
    };
    match g {
        Some(g) => {
            let bounds = state.viewport.bounds();
            let [a, b] = segment(&g, &r.view, state.viewport, bounds);
            Some(across_cursor(a, b))
        }
        None => match state.drag {
            Some(drag) => Some(guide_cursor(drag.vertical())),
            None => None,
        },
    }
}
