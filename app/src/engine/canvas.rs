//! Spool's canvas: a GPUI port of figcraft's `crates/ui-egui/src/canvas.rs`.
//!
//! # What this module is
//!
//! The engine owns the canvas's behaviour. [`figcraft_engine::Session::pointer`] runs the whole
//! eighteen-tool gesture state machine — pan, marquee, move, resize, rotate, pen, text, vector
//! edit — and republishes a serialisable [`Overlay`] describing what the canvas should draw on top
//! of the rendered page. Nothing here reimplements a gesture, a snapping rule or a hit test: this
//! module turns GPUI input into [`PointerEvent`]s, hands them to the engine, and paints what comes
//! back.
//!
//! # What was ported, and from where
//!
//! | figcraft (egui) | here (GPUI) |
//! |---|---|
//! | `ui-egui/src/canvas.rs::show` | [`EngineCanvas`]'s `Render` impl — same draw order |
//! | `ui-egui/src/canvas.rs::overlays` | [`EngineCanvas::paint_overlays`] — every overlay, in order |
//! | `ui-egui/src/canvas.rs::input` | [`EngineCanvas::on_mouse_move`] and friends |
//! | `ui-egui/src/canvas.rs::send` | [`EngineCanvas::forward`] — one `Session::pointer` call |
//! | `ui-egui/src/canvas.rs::TextOnScreen` | [`TextOnScreen`] |
//! | `ui-egui/src/canvas.rs::hatch` | [`hatch_lines`] — 45° scanline hatching |
//! | `ui-egui/src/cursors.rs` | [`cursor_style`] and [`draw_cursor_glyph`] |
//! | `ui-egui/src/render_worker.rs` | [`crate::canvas_render::CanvasRenderer`] (already written) |
//!
//! No egui call survives. `Painter` is replaced by [`Overlays`], a small struct that turns page
//! coordinates into GPUI primitives; everything else is the same geometry with a different
//! vocabulary.
//!
//! # The session is shared, and there is only one
//!
//! [`EngineCanvas`] holds `Rc<RefCell<Session>>`. Panels get the same handle from
//! [`EngineCanvas::session_handle`] and read or act through it. The borrows are short by
//! construction — [`EngineCanvas::with_session`] takes a closure — so a panel that held the
//! session across a frame would deadlock rather than silently doing the wrong thing.
//!
//! There is deliberately no second copy of the camera. The camera *is*
//! [`figcraft_engine::ViewState`], which lives inside the session's `DocState`; [`Viewport`] holds
//! only where the canvas element is on the window. `zoom_to_fit` and `zoom_to_selection` write
//! `ds.view`, so the engine's own commands (`view.zoomAt`, `view.fit`) see the camera the canvas
//! drew with.
//!
//! # The camera maths
//!
//! One pure pair, [`page_to_screen`] and [`screen_to_page`], wraps
//! [`ViewState::page_to_screen`] / [`ViewState::screen_to_page`] with the viewport's top-left added.
//! Every coordinate in this file goes through one of them, and every `PointerEvent` is built by
//! [`pointer_event`], so a pointer event cannot disagree with what was painted.
//!
//! # The overlay's JSON twin
//!
//! [`Overlay`] and everything in it are `Serialize` but not `Deserialize`: they are what the engine
//! *publishes*, not a format it reads back. [`OverlayView::from_tool_info`] therefore parses the
//! JSON by hand, field by field, rather than deriving a mirror type. The tests assert the two
//! readers agree on a live session, which is the only thing that makes the hand-written parser
//! trustworthy.
//!
//! # What is not here
//!
//! Three things figcraft draws are not ported, and each says so where it would have gone: the
//! Variable-width tool's overlay (it needs `vectoredit::width_handles` plumbed through the
//! overlay), prototype connections, and a frame's Dev Mode status badge. Everything else in
//! `overlays()` is here, including the ones that are easy to skip: the pixel grid, layout grids,
//! frame labels, the rotation origin, arc and shape handles, the eraser brush, the lasso and the
//! rulers.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use figcraft_doc::{Document, NodeId, NodeKind};
use figcraft_engine::arc_handles::{ArcHandle, ArcHandles, ArcHint};
use figcraft_engine::shape_handles::{ShapeHandle, ShapeHint};
use figcraft_engine::snap::Guide;
use figcraft_engine::tools::{
    Cursor, Overlay, PenCursor, PointerEvent, PointerKind, ScaleHint, ToolId,
};
use figcraft_engine::vectoredit::{VHit, WHit};
use figcraft_engine::{textedit, DocState, Session, ViewState};
use figcraft_geom::{Affine, Point, Rect as GRect};
use figcraft_render::Options;
use gpui::{
    canvas, div, point, px, rgb, rgba, size, App, Bounds, Corners, CursorStyle, Edges,
    ElementInputHandler, EntityInputHandler, FocusHandle, Hitbox, HitboxBehavior, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke, MouseButton,
    MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, ParentElement,
    PathBuilder, PinchEvent, Pixels, Point as GPoint, Render, ScrollDelta, ScrollWheelEvent,
    SharedString, Styled, TextRun, UTF16Selection, Window,
};
use serde_json::Value;

use crate::canvas_render::{CanvasFrame, CanvasRenderer};
use crate::theme;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// figcraft's selection corner square, in logical pixels.
const CORNER_PX: f32 = 8.0;
/// Radius of the dots the canvas draws on anchors and handles.
const DOT_PX: f32 = 3.5;
/// Width of the eraser brush's white halo, in logical pixels.
const BRUSH_HALO_PX: f32 = 3.0;
/// Width of a solid overlay line, in logical pixels.
const LINE_PX: f32 = 1.0;
/// Spacing between hatch lines, in logical pixels.
const HATCH_GAP: f32 = 6.0;
/// The dash pattern overlays use (on, off), in logical pixels.
const DASH: [f32; 2] = [4.0, 3.0];
/// The rulers' thickness, in logical pixels. figcraft's ruler bar.
const RULER_PX: f32 = 20.0;
/// The size every readout is drawn at, in logical points.
const LABEL_PT: f32 = 10.0;
/// A readout's line box, as a multiple of the point size.
const LABEL_LINE: f32 = 1.2;
/// The caret's half-period, in milliseconds.
const CARET_BLINK_MS: u128 = 500;
/// A readout longer than this is elided rather than drawn across the canvas.
const MAX_LABEL_CHARS: usize = 24;
/// The most lines a loop over page coordinates will draw, whatever the zoom.
const MAX_GRID_LINES: usize = 2048;
/// The one colour the canvas adds to the theme: components, as `engine::layers` draws them.
const COMPONENT_HEX: u32 = 0x6fd0b0;
/// How many paints between two diagnostics reports.
///
/// Two seconds of a 60 Hz canvas: often enough to see what a session is doing
/// and rare enough not to drown the output.
const DIAGNOSTIC_EVERY_PAINTS: u64 = 120;

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// The colours the canvas paints with, resolved once from [`theme`].
///
/// [`theme`] is a table of hex integers and `gpui::rgb` is not a `const fn`, so the conversions
/// happen here rather than in the drawing code. Holding them together also keeps the palette
/// auditable: every colour the canvas can draw is a field of this struct.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Behind the page: the viewport's own background.
    pub canvas: Hsla,
    /// Selection and handles: figcraft's `ACCENT`.
    pub accent: Hsla,
    /// Snap and measurement guides: Spool's magenta, for the reason [`theme::SNAP_GUIDE`] gives.
    pub guide: Hsla,
    /// Components, sets and instances.
    pub component: Hsla,
    /// Primary text: labels, carets, rulers.
    pub ink: Hsla,
    /// Secondary text.
    pub ink_soft: Hsla,
    /// The rulers' bar.
    pub panel: Hsla,
    /// A destructive hint (the shape builder's Remove mode, a measured box).
    pub danger: Hsla,
    /// The handle fill, so a handle reads on any artwork.
    pub white: Hsla,
    /// Fully transparent. Named so a "ring this dot in nothing" call reads as itself.
    pub none: Hsla,
}

impl Default for Palette {
    fn default() -> Self {
        Self::new()
    }
}

impl Palette {
    pub fn new() -> Self {
        Self {
            canvas: hsla(theme::CANVAS),
            accent: hsla(theme::ACCENT),
            guide: hsla(theme::SNAP_GUIDE),
            component: hsla(COMPONENT_HEX),
            ink: hsla(theme::TEXT),
            ink_soft: hsla(theme::TEXT_SECONDARY),
            panel: hsla(theme::SURFACE),
            danger: hsla(0xe5534b),
            white: Hsla {
                h: 0.0,
                s: 0.0,
                l: 1.0,
                a: 1.0,
            },
            none: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.0,
                a: 0.0,
            },
        }
    }

    /// `c` at `a` alpha. GPUI's `alpha` already clamps, so a bad value cannot escape.
    pub fn fade(c: Hsla, a: f32) -> Hsla {
        c.alpha(a)
    }
}

fn hsla(hex: u32) -> Hsla {
    Hsla::from(rgb(hex))
}

/// An engine [`figcraft_doc::color::Color`] as a GPUI colour.
///
/// Through 8-bit channels on purpose: the engine's colours already quantise to 8 bits per channel,
/// and going through `Rgba` rather than computing HSL from floats keeps the same quantisation every
/// other GPUI colour in the app has.
fn engine_color(c: figcraft_doc::color::Color) -> Hsla {
    let [r, g, b, a] = c.to_rgba8();
    Hsla::from(rgba(
        (u32::from(r) << 24) | (u32::from(g) << 16) | (u32::from(b) << 8) | u32::from(a),
    ))
}

// ---------------------------------------------------------------------------
// The camera
// ---------------------------------------------------------------------------

/// A viewport the page is drawn into: where its top-left sits on the window, and how big it is.
///
/// This is the only geometry the canvas owns. The camera proper — the page point at the centre and
/// the zoom — is [`ViewState`], owned by the session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// The viewport's top-left, in window pixels.
    pub origin: GPoint<f32>,
    /// The viewport's size, in logical points.
    pub size: (f32, f32),
}

impl Viewport {
    pub fn new(bounds: Bounds<Pixels>) -> Self {
        Self {
            origin: point(f32::from(bounds.origin.x), f32::from(bounds.origin.y)),
            size: (f32::from(bounds.size.width), f32::from(bounds.size.height)),
        }
    }

    /// A viewport that has never been laid out: zero-sized, so nothing draws.
    pub const fn empty() -> Self {
        Self {
            origin: GPoint { x: 0.0, y: 0.0 },
            size: (0.0, 0.0),
        }
    }

    /// Whether `at` (window pixels) is inside the viewport.
    pub fn contains(&self, at: GPoint<f32>) -> bool {
        let d = at - self.origin;
        d.x >= 0.0 && d.y >= 0.0 && d.x <= self.size.0 && d.y <= self.size.1
    }

    /// The viewport as a rectangle, for intersection tests.
    pub fn bounds(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.origin.x), px(self.origin.y)),
            size(px(self.size.0), px(self.size.1)),
        )
    }
}

/// Page point `(x, y)`, in logical points relative to the viewport's top-left.
pub fn page_to_screen(view: &ViewState, viewport: Viewport, x: f64, y: f64) -> GPoint<f32> {
    let (sx, sy) = view.page_to_screen(x, y);
    point(viewport.origin.x + sx as f32, viewport.origin.y + sy as f32)
}

/// The inverse of [`page_to_screen`].
pub fn screen_to_page(view: &ViewState, viewport: Viewport, at: GPoint<f32>) -> Point {
    let (x, y) = view.screen_to_page(
        f64::from(at.x - viewport.origin.x),
        f64::from(at.y - viewport.origin.y),
    );
    Point::new(x, y)
}

/// The pointer position as a page point. The one place a window coordinate becomes a page one.
pub fn page_point(view: &ViewState, viewport: Viewport, at: GPoint<f32>) -> Point {
    screen_to_page(view, viewport, at)
}

/// Build a [`PointerEvent`] for `kind` at window point `at`.
///
/// `sx`/`sy` are relative to the viewport, not the window: the engine's pan gesture measures them
/// against the camera's own origin, so the viewport offset must not be in them. `cmd` is Cmd on
/// macOS and the Windows key elsewhere; `ctrl` is the physical Control key, which figcraft uses as
/// the line-motion modifier on every platform and as the word modifier while editing text.
#[allow(clippy::too_many_arguments)]
pub fn pointer_event(
    view: &ViewState,
    viewport: Viewport,
    kind: PointerKind,
    at: GPoint<f32>,
    shift: bool,
    alt: bool,
    cmd: bool,
    ctrl: bool,
    pressure: Option<f32>,
) -> PointerEvent {
    let p = page_point(view, viewport, at);
    PointerEvent {
        kind,
        x: p.x,
        y: p.y,
        sx: f64::from(at.x - viewport.origin.x),
        sy: f64::from(at.y - viewport.origin.y),
        shift,
        alt,
        cmd,
        ctrl,
        zoom: view.zoom,
        pressure,
    }
}

/// The four corners of `node`'s box on the page, in world space (tl, tr, br, bl).
pub fn node_quad(doc: &Document, id: NodeId) -> Option<[Point; 4]> {
    let n = doc.node(id)?;
    let w = doc.world_transform(id);
    let (wd, ht) = (n.width, n.height);
    Some([
        w * Point::ZERO,
        w * Point::new(wd, 0.0),
        w * Point::new(wd, ht),
        w * Point::new(0.0, ht),
    ])
}

/// The layer a click at `at` on `page` selects: the hit leaf, resolved to the level the click
/// means (`deep` selects the leaf itself, a plain click its outermost group).
///
/// Both halves are the engine's: [`Session::hit`] and [`figcraft_doc::geometry::select_target`].
pub fn select_at(
    session: &Session,
    doc: &Document,
    page: NodeId,
    at: Point,
    deep: bool,
) -> Option<NodeId> {
    let zoom = session.ds().ok()?.view.zoom.max(1e-9);
    let leaf = session.hit(doc, page, at, 2.0 / zoom)?;
    Some(figcraft_doc::geometry::select_target(
        doc,
        leaf,
        &session.selection(),
        deep,
    ))
}

/// What a marquee from `(x0, y0)` to `(x1, y1)` selects, restricted to `within` when the drag
/// started inside a frame. [`figcraft_doc::geometry::marquee`], named so a panel that already holds
/// a canvas does not need a second import.
pub fn marquee_targets(
    doc: &Document,
    page: NodeId,
    rect: [f64; 4],
    within: Option<NodeId>,
) -> Vec<NodeId> {
    let [x0, y0, x1, y1] = rect;
    figcraft_doc::geometry::marquee(
        doc,
        page,
        GRect::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)),
        within,
    )
}

// ---------------------------------------------------------------------------
// The overlay
// ---------------------------------------------------------------------------

/// Everything [`Overlay`] says to draw, read once per frame.
///
/// This is the struct the painter walks. It holds the engine's own types rather than re-decoding
/// them: the point of the view is to decide *what* is on and to be assertable without a `Window`.
///
/// A field is absent rather than defaulted where "absent" *is* the drawing instruction: `marquee`
/// is `None` when no marquee is being dragged, and that is not the same as a degenerate rect.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OverlayView {
    pub marquee: Option<[f64; 4]>,
    pub guides: Vec<Guide>,
    pub cursor: Cursor,
    pub pen: Vec<[f64; 2]>,
    pub rubber: Option<[f64; 4]>,
    pub pen_handles: Vec<[f64; 4]>,
    pub rubber_handle: Option<[f64; 2]>,
    pub drop_target: Option<NodeId>,
    pub insert_line: Option<[f64; 4]>,
    pub swap_target: Option<NodeId>,
    pub drop_cell: Option<[f64; 4]>,
    pub vector_hover: Option<VHit>,
    pub vector_hover_on: Option<NodeId>,
    pub width_hover: Option<WHit>,
    pub lasso: Vec<[f64; 2]>,
    pub brush: Option<[f64; 3]>,
    pub face: Vec<Vec<[f64; 2]>>,
    pub builder_regions: Vec<usize>,
    pub builder_remove: bool,
    pub measure_box: Option<[f64; 4]>,
    pub scale: Option<ScaleHint>,
    pub arc_hover: Option<ArcHandle>,
    pub arc: Option<ArcHint>,
    pub shape_hover: Option<ShapeHandle>,
    pub shape: Option<ShapeHint>,
}

impl OverlayView {
    /// The typed read: what `session.tool.overlay` holds.
    pub fn from_overlay(o: &Overlay) -> Self {
        Self {
            marquee: o.marquee,
            guides: o.guides.clone(),
            cursor: o.cursor,
            pen: o.pen.clone(),
            rubber: o.rubber,
            pen_handles: o.pen_handles.clone(),
            rubber_handle: o.rubber_handle,
            drop_target: o.drop_target,
            insert_line: o.insert_line,
            swap_target: o.swap_target,
            drop_cell: o.drop_cell,
            vector_hover: o.vector_hover,
            vector_hover_on: o.vector_hover_on,
            width_hover: o.width_hover,
            lasso: o.lasso.clone(),
            brush: o.brush,
            face: o.face.clone(),
            builder_regions: o.builder_regions.clone(),
            builder_remove: o.builder_remove,
            measure_box: o.measure_box,
            scale: o.scale,
            arc_hover: o.arc_hover,
            arc: o.arc.clone(),
            shape_hover: o.shape_hover,
            shape: o.shape.clone(),
        }
    }

    /// The JSON read: `tool_info()["overlay"]`.
    ///
    /// Parsed field by field because [`Overlay`] and everything in it are `Serialize` only. `None`
    /// when the value is not an object — a `tool_info` from something that is not the engine —
    /// rather than an all-empty overlay that would paint nothing and look like an idle canvas.
    pub fn from_tool_info(info: &Value) -> Option<Self> {
        let o = info.get("overlay")?;
        if !o.is_object() {
            return None;
        }
        Some(Self {
            marquee: json_rect(o.get("marquee")),
            guides: json_guides(o.get("guides")),
            cursor: json_cursor(o.get("cursor")),
            pen: json_points(o.get("pen")),
            rubber: json_rect(o.get("rubber")),
            pen_handles: json_rects(o.get("pen_handles")),
            rubber_handle: json_point(o.get("rubber_handle")),
            drop_target: json_id(o.get("drop_target")),
            insert_line: json_rect(o.get("insert_line")),
            swap_target: json_id(o.get("swap_target")),
            drop_cell: json_rect(o.get("drop_cell")),
            vector_hover: json_vhit(o.get("vector_hover")),
            vector_hover_on: json_id(o.get("vector_hover_on")),
            width_hover: json_whit(o.get("width_hover")),
            lasso: json_points(o.get("lasso")),
            brush: json_brush(o.get("brush")),
            face: json_faces(o.get("face")),
            builder_regions: json_usizes(o.get("builder_regions")),
            builder_remove: o
                .get("builder_remove")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            measure_box: json_rect(o.get("measure_box")),
            scale: json_scale(o.get("scale")),
            arc_hover: json_arc_handle(o.get("arc_hover")),
            arc: json_arc_hint(o.get("arc")),
            shape_hover: json_shape_handle(o.get("shape_hover")),
            shape: json_shape_hint(o.get("shape")),
        })
    }

    /// Whether anything at all is to be drawn over the page.
    pub fn is_empty(&self) -> bool {
        self.marquee.is_none()
            && self.guides.is_empty()
            && self.pen.is_empty()
            && self.rubber.is_none()
            && self.pen_handles.is_empty()
            && self.lasso.is_empty()
            && self.face.is_empty()
            && self.builder_regions.is_empty()
            && self.measure_box.is_none()
            && self.insert_line.is_none()
            && self.drop_cell.is_none()
            && self.brush.is_none()
            && self.drop_target.is_none()
            && self.swap_target.is_none()
    }
}

// --- reading the overlay out of JSON ---------------------------------------
//
// The engine's overlay is published, not consumed, so none of these types implement `Deserialize`.
// Each reader below is the mirror of one `Serialize` impl, and the tests assert the mirrors round
// trip against a live session.

fn json_rect(v: Option<&Value>) -> Option<[f64; 4]> {
    let a = v?.as_array()?;
    let n = [
        a.first()?.as_f64()?,
        a.get(1)?.as_f64()?,
        a.get(2)?.as_f64()?,
        a.get(3)?.as_f64()?,
    ];
    n.iter().all(|x| x.is_finite()).then_some(n)
}

fn json_rects(v: Option<&Value>) -> Vec<[f64; 4]> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|r| json_rect(Some(r))).collect())
        .unwrap_or_default()
}

fn json_point(v: Option<&Value>) -> Option<[f64; 2]> {
    let a = v?.as_array()?;
    let n = [a.first()?.as_f64()?, a.get(1)?.as_f64()?];
    n.iter().all(|x| x.is_finite()).then_some(n)
}

fn json_points(v: Option<&Value>) -> Vec<[f64; 2]> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|p| json_point(Some(p))).collect())
        .unwrap_or_default()
}

fn json_faces(v: Option<&Value>) -> Vec<Vec<[f64; 2]>> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(|poly| json_points(Some(poly))).collect())
        .unwrap_or_default()
}

fn json_brush(v: Option<&Value>) -> Option<[f64; 3]> {
    let a = v?.as_array()?;
    let n = [
        a.first()?.as_f64()?,
        a.get(1)?.as_f64()?,
        a.get(2)?.as_f64()?,
    ];
    n.iter().all(|x| x.is_finite()).then_some(n)
}

fn json_id(v: Option<&Value>) -> Option<NodeId> {
    Some(NodeId(v?.as_u64()?))
}

fn json_usizes(v: Option<&Value>) -> Vec<usize> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| i.as_u64())
                .filter_map(|i| usize::try_from(i).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn json_guides(v: Option<&Value>) -> Vec<Guide> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(json_guide).collect())
        .unwrap_or_default()
}

fn json_guide(v: &Value) -> Option<Guide> {
    Some(Guide {
        x0: v.get("x0")?.as_f64()?,
        y0: v.get("y0")?.as_f64()?,
        x1: v.get("x1")?.as_f64()?,
        y1: v.get("y1")?.as_f64()?,
        label: v.get("label").and_then(Value::as_str).map(str::to_owned),
        dashed: v.get("dashed").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// [`Cursor`] serialises as an externally tagged enum: `"Default"`, `{"Resize": 45.0}`,
/// `{"Pen": "Close"}`. Anything else is [`Cursor::Default`], because a cursor the canvas cannot
/// name is the default cursor.
pub fn json_cursor(v: Option<&Value>) -> Cursor {
    let Some(v) = v else { return Cursor::Default };
    if let Some(s) = v.as_str() {
        return match s {
            "Crosshair" => Cursor::Crosshair,
            "Move" => Cursor::Move,
            "Grab" => Cursor::Grab,
            "Grabbing" => Cursor::Grabbing,
            "Text" => Cursor::Text,
            "Copy" => Cursor::Copy,
            "Pointer" => Cursor::Pointer,
            "Eyedropper" => Cursor::Eyedropper,
            "Comment" => Cursor::Comment,
            "NotAllowed" => Cursor::NotAllowed,
            _ => Cursor::Default,
        };
    }
    let Some((name, arg)) = v.as_object().and_then(|o| o.iter().next()) else {
        return Cursor::Default;
    };
    match name.as_str() {
        "Resize" => Cursor::Resize(arg.as_f64().unwrap_or(0.0)),
        "Rotate" => Cursor::Rotate(arg.as_f64().unwrap_or(0.0)),
        "Pen" => Cursor::Pen(json_pen_cursor(arg)),
        _ => Cursor::Default,
    }
}

fn json_pen_cursor(v: &Value) -> PenCursor {
    match v.as_str() {
        Some("Close") => PenCursor::Close,
        Some("Add") => PenCursor::Add,
        Some("Bend") => PenCursor::Bend,
        _ => PenCursor::New,
    }
}

fn json_scale(v: Option<&Value>) -> Option<ScaleHint> {
    let v = v?;
    Some(ScaleHint {
        factor: v.get("factor")?.as_f64()?,
        pivot: json_point(v.get("pivot"))?,
    })
}

fn json_arc_handle(v: Option<&Value>) -> Option<ArcHandle> {
    match v?.as_str()? {
        "sweep" => Some(ArcHandle::Sweep),
        "start" => Some(ArcHandle::Start),
        "ratio" => Some(ArcHandle::Ratio),
        _ => None,
    }
}

fn json_shape_handle(handle: Option<&Value>) -> Option<ShapeHandle> {
    let v = handle?;
    match v.get("kind")?.as_str()? {
        "radius" => Some(ShapeHandle::Radius {
            corner: v.get("corner").and_then(Value::as_u64).unwrap_or(0) as usize,
        }),
        "ratio" => Some(ShapeHandle::Ratio),
        _ => None,
    }
}

fn json_arc_hint(v: Option<&Value>) -> Option<ArcHint> {
    let v = v?;
    Some(ArcHint {
        handle: json_arc_handle(v.get("handle"))?,
        label: v.get("label")?.as_str()?.to_owned(),
        at: json_point(v.get("at"))?,
    })
}

fn json_shape_hint(v: Option<&Value>) -> Option<ShapeHint> {
    let v = v?;
    Some(ShapeHint {
        handle: json_shape_handle(v.get("handle"))?,
        value: v.get("value")?.as_f64()?,
        at: json_point(v.get("at"))?,
    })
}

fn json_usize(v: &Value, key: &str) -> Option<usize> {
    usize::try_from(v.get(key)?.as_u64()?).ok()
}

fn json_vhit(v: Option<&Value>) -> Option<VHit> {
    let v = v?;
    match v.get("kind")?.as_str()? {
        "vertex" => Some(VHit::Vertex {
            index: json_usize(v, "index")?,
        }),
        "handle" => Some(VHit::Handle {
            seg: json_usize(v, "seg")?,
            start: v.get("start").and_then(Value::as_bool).unwrap_or(false),
        }),
        "segment" => Some(VHit::Segment {
            seg: json_usize(v, "seg")?,
            t: v.get("t")?.as_f64()?,
        }),
        _ => None,
    }
}

fn json_whit(v: Option<&Value>) -> Option<WHit> {
    let v = v?;
    match v.get("kind")?.as_str()? {
        "point" => Some(WHit::Point {
            index: json_usize(v, "index")?,
        }),
        "handle" => Some(WHit::Handle {
            index: json_usize(v, "index")?,
            side: json_width_side(v.get("side"))?,
        }),
        "path" => Some(WHit::Path {
            t: v.get("t")?.as_f64()?,
            side: json_width_side(v.get("side"))?,
        }),
        _ => None,
    }
}

fn json_width_side(v: Option<&Value>) -> Option<figcraft_doc::WidthSide> {
    match v?.as_str()? {
        "left" => Some(figcraft_doc::WidthSide::Left),
        "right" => Some(figcraft_doc::WidthSide::Right),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The cursor
// ---------------------------------------------------------------------------

/// The system's two-headed arrow nearest a resize along `degrees` (0 = horizontal, 90 = vertical;
/// screen angles, so 45 runs from the top left to the bottom right). figcraft's `resize_icon`.
pub fn resize_cursor(degrees: f64) -> CursorStyle {
    let a = degrees.rem_euclid(180.0);
    if !(22.5..157.5).contains(&a) {
        CursorStyle::ResizeLeftRight
    } else if a < 67.5 {
        CursorStyle::ResizeUpLeftDownRight
    } else if a < 112.5 {
        CursorStyle::ResizeUpDown
    } else {
        CursorStyle::ResizeUpRightDownLeft
    }
}

/// What the engine's [`Cursor`] is as a GPUI cursor style.
///
/// GPUI has no drawn cursors, so the four figcraft draws — the rotate arc, the pen's four states,
/// the eyedropper and the comment pin — come back as `None` and [`draw_cursor_glyph`] paints them
/// instead. Every cursor figcraft draws as a glyph is one where the system's arrow would say
/// nothing: a rotate cursor turned to its corner, or what a pen click does here.
pub fn cursor_style(cursor: Cursor) -> Option<CursorStyle> {
    match cursor {
        Cursor::Default | Cursor::Move => Some(CursorStyle::Arrow),
        Cursor::Crosshair => Some(CursorStyle::Crosshair),
        Cursor::Resize(a) if a.is_finite() => Some(resize_cursor(a)),
        Cursor::Resize(_) => Some(CursorStyle::Arrow),
        Cursor::Rotate(_) => None,
        Cursor::Grab => Some(CursorStyle::OpenHand),
        Cursor::Grabbing => Some(CursorStyle::ClosedHand),
        Cursor::Text => Some(CursorStyle::IBeam),
        Cursor::Pen(_) => None,
        Cursor::Copy => Some(CursorStyle::DragCopy),
        Cursor::Pointer => Some(CursorStyle::PointingHand),
        Cursor::Eyedropper | Cursor::Comment => None,
        Cursor::NotAllowed => Some(CursorStyle::OperationNotAllowed),
    }
}

/// What the pointer shows on the canvas.
///
/// Panning (Space or the middle button) comes before the tool's cursor, as in figcraft's
/// `on_canvas`. The zoom tool has no GPUI cursor and no glyph; the accent rectangle drawn for its
/// drag is what says so, and the system arrow is left in place rather than inventing a magnifier.
pub fn on_canvas(
    panning: bool,
    button_down: bool,
    _zoom_tool: Option<bool>,
    cursor: Cursor,
) -> Option<CursorStyle> {
    if panning {
        return Some(if button_down {
            CursorStyle::ClosedHand
        } else {
            CursorStyle::OpenHand
        });
    }
    cursor_style(cursor)
}

/// A name for the cursor, for a diagnostics line. figcraft's `Shown::name`.
pub fn cursor_name(cursor: Cursor) -> String {
    match cursor {
        Cursor::Default => "default".into(),
        Cursor::Crosshair => "crosshair".into(),
        Cursor::Move => "move".into(),
        Cursor::Resize(a) => format!("resize {}", a.rem_euclid(360.0).round()),
        Cursor::Rotate(a) => format!("rotate {}", a.rem_euclid(360.0).round()),
        Cursor::Grab => "grab".into(),
        Cursor::Grabbing => "grabbing".into(),
        Cursor::Text => "text".into(),
        Cursor::Pen(p) => match p {
            PenCursor::New => "pen".into(),
            PenCursor::Close => "pen-close".into(),
            PenCursor::Add => "pen-add".into(),
            PenCursor::Bend => "pen-bend".into(),
        },
        Cursor::Copy => "copy".into(),
        Cursor::Pointer => "pointer".into(),
        Cursor::Eyedropper => "eyedropper".into(),
        Cursor::Comment => "comment".into(),
        Cursor::NotAllowed => "not-allowed".into(),
    }
}

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

/// The layers that get canvas labels and layout grids, in z-order: the page's visible top-level
/// layers and, inside sections whose contents show, their visible children (sections nest).
pub fn boards(doc: &Document, page: NodeId) -> Vec<NodeId> {
    fn walk(doc: &Document, id: NodeId, depth: usize, out: &mut Vec<NodeId>) {
        let Some(n) = doc.node(id) else { return };
        for c in &n.children {
            let Some(cn) = doc.node(*c) else { continue };
            if !cn.visible {
                continue;
            }
            out.push(*c);
            if matches!(
                cn.kind,
                NodeKind::Section {
                    contents_hidden: false
                }
            ) && depth < 32
            {
                walk(doc, *c, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(doc, page, 0, &mut out);
    out
}

/// 45° hatch lines `gap` apart inside `polys` (even-odd), in window pixels.
///
/// figcraft's `hatch`: rotate so the hatch is horizontal, scan, rotate back. Every clip is in here
/// because a page-sized polygon at 1% zoom crosses the scanlines more often than there are pixels.
pub fn hatch_lines(polys: &[Vec<GPoint<f32>>], gap: f32) -> Vec<[GPoint<f32>; 2]> {
    const S: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let rot = |p: GPoint<f32>| point((p.x + p.y) * S, (p.y - p.x) * S);
    let back = |x: f32, y: f32| point((x - y) * S, (x + y) * S);
    let edges: Vec<(GPoint<f32>, GPoint<f32>)> = polys
        .iter()
        .flat_map(|poly| {
            let pts: Vec<GPoint<f32>> = poly.iter().map(|p| rot(*p)).collect();
            let n = pts.len();
            (0..n).filter_map(move |k| Some((*pts.get(k)?, *pts.get((k + 1) % n)?)))
        })
        .collect();
    if edges.is_empty() || !(gap > 0.0) || !gap.is_finite() {
        return Vec::new();
    }
    // `f32::min` ignores a NaN, so a non-finite corner would silently drop out of the bounds
    // fold and hatch a region that has no corner there. Checked up front instead.
    if !edges
        .iter()
        .all(|(a, b)| a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite())
    {
        return Vec::new();
    }
    let (y0, y1) = edges
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), (a, b)| {
            (lo.min(a.y).min(b.y), hi.max(a.y).max(b.y))
        });
    let mut out = Vec::new();
    if !(y0.is_finite() && y1.is_finite()) || !((y1 - y0) / gap).is_finite() {
        return out;
    }
    if (y1 - y0) / gap > 4000.0 {
        return out;
    }
    let mut y = (y0 / gap).ceil() * gap;
    while y < y1 {
        let mut xs: Vec<f32> = edges
            .iter()
            .filter(|(a, b)| (a.y <= y && y < b.y) || (b.y <= y && y < a.y))
            .map(|(a, b)| a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y))
            .collect();
        xs.sort_by(f32::total_cmp);
        for pair in xs.chunks_exact(2) {
            if let [a, b] = pair {
                out.push([back(*a, y), back(*b, y)]);
            }
        }
        y += gap;
    }
    out
}

/// A number as the canvas labels it: no trailing `.0`, at most two decimals.
pub fn fmt(v: f64) -> String {
    if !v.is_finite() {
        return String::new();
    }
    let r = (v * 100.0).round() / 100.0;
    if (r - r.round()).abs() < 1e-9 {
        format!("{}", r.round() as i64)
    } else {
        format!("{r}")
    }
}

/// A size label: `120 × 80`, or just the length when the box has no height.
pub fn size_label(w: f64, h: f64, line: bool) -> String {
    if line {
        fmt(w)
    } else {
        format!("{} × {}", fmt(w), fmt(h))
    }
}

// ---------------------------------------------------------------------------
// Text on the canvas
// ---------------------------------------------------------------------------

/// A text layer's layout placed on the canvas: where carets and selected characters show.
pub struct TextOnScreen {
    pub layout: Arc<figcraft_text::TextLayout>,
    /// The layout's vertical offset in the box (vertical alignment).
    dy: f64,
    world: Affine,
}

impl TextOnScreen {
    /// Text layer `id` of `doc` (`None`: not a text layer, or no fonts to lay it out with).
    pub fn of(doc: &Document, id: NodeId) -> Option<Self> {
        let n = doc.node(id)?;
        if !matches!(n.kind, NodeKind::Text(_)) {
            return None;
        }
        let (layout, dy) = figcraft_text::layout_node(n)?;
        Some(Self {
            layout,
            dy,
            world: doc.world_transform(id),
        })
    }

    /// The page point a layout `(x, y)` maps to (on the path, for text on a path).
    pub fn page_point(&self, x: f64, y: f64) -> Point {
        let q = self.layout.warp_point(x, y);
        self.world * Point::new(q.x, q.y + self.dy)
    }

    /// Layout point `(x, y)` in window pixels.
    pub fn at(&self, view: &ViewState, viewport: Viewport, x: f64, y: f64) -> GPoint<f32> {
        let p = self.page_point(x, y);
        page_to_screen(view, viewport, p.x, p.y)
    }

    /// The highlight of characters `a..b`, one window quad per line.
    pub fn selection(
        &self,
        view: &ViewState,
        viewport: Viewport,
        a: usize,
        b: usize,
    ) -> Vec<[GPoint<f32>; 4]> {
        self.layout
            .selection_rects(a, b)
            .iter()
            .map(|r| {
                [
                    self.at(view, viewport, r.x0, r.y0),
                    self.at(view, viewport, r.x1, r.y0),
                    self.at(view, viewport, r.x1, r.y1),
                    self.at(view, viewport, r.x0, r.y1),
                ]
            })
            .collect()
    }

    /// The caret before character `index`: its top and bottom in window pixels (an empty layer: a
    /// line of default height at its origin).
    pub fn caret(
        &self,
        view: &ViewState,
        viewport: Viewport,
        index: usize,
        zoom: f64,
    ) -> [GPoint<f32>; 2] {
        let (cx, li) = self.layout.caret(index);
        match self.layout.lines.get(li).or(self.layout.lines.last()) {
            Some(line) => [
                self.at(view, viewport, cx, line.top),
                self.at(view, viewport, cx, line.top + line.height),
            ],
            None => {
                let p = self.at(view, viewport, 0.0, 0.0);
                [p, point(p.x, p.y + 14.0 * zoom as f32)]
            }
        }
    }

    /// The character index under page point `(x, y)`, in the text's own characters.
    ///
    /// The world transform is inverted first, so a caret placed by a click and a caret placed by a
    /// key land in the same place. `warp_point` is deliberately *not* applied: it maps layout
    /// coordinates onto a path, and there is no inverse for it. Text on a path is placed by the
    /// engine's own vector-edit hit test, not by this.
    pub fn hit(&self, x: f64, y: f64) -> usize {
        let local = self.world.inverse() * Point::new(x, y);
        self.layout.hit(local.x, local.y - self.dy)
    }
}

// ---------------------------------------------------------------------------
// The composition in progress
// ---------------------------------------------------------------------------

/// The document with an IME composition inserted at the caret, plus the composition's extent.
///
/// The real document only gets the composition on commit, so the canvas renders *this* document
/// and underlines the composition at the caret. Rebuilt only when the document, the composition or
/// the caret actually changed.
#[derive(Clone, Debug)]
pub struct Composition {
    /// The document, composition and caret this was built from.
    key: (u64, Arc<Document>, String, usize),
    pub doc: Arc<Document>,
    /// The composition's char range, and the IME cursor, in `doc`'s edited text.
    pub range: (usize, usize),
    pub cursor: usize,
}

impl Composition {
    /// The composition view for `session`, reusing `cached` when nothing it was built from moved.
    pub fn build(session: &Session, cached: Option<&Composition>) -> Option<Self> {
        let ds = session.ds().ok()?;
        let te = ds.text_edit.as_ref()?;
        let composition = te.preedit.as_ref()?;
        let uid = Arc::as_ptr(&ds.doc) as u64;
        if let Some(c) = cached {
            let same = c.key.0 == uid
                && c.key.2 == composition.text
                && c.key.3 == te.caret
                && Arc::ptr_eq(&c.key.1, &ds.doc);
            if same {
                return Some(c.clone());
            }
        }
        let (doc, range, cursor) = textedit::preedit_doc(session)?;
        Some(Self {
            key: (uid, ds.doc.clone(), composition.text.clone(), te.caret),
            doc: Arc::new(doc),
            range,
            cursor,
        })
    }
}

// ---------------------------------------------------------------------------
// The painter
// ---------------------------------------------------------------------------

/// The drawing surface: page coordinates in, GPUI primitives out.
///
/// One struct replaces egui's `Painter`. It holds the viewport and the camera so every method
/// takes *page* coordinates, which is the only coordinate system the engine and the document
/// speak. Nothing here knows about the document.
struct Overlays<'a> {
    window: &'a mut Window,
    app: &'a mut App,
    viewport: Viewport,
    view: ViewState,
    pal: Palette,
}

impl Overlays<'_> {
    fn at(&self, x: f64, y: f64) -> GPoint<f32> {
        page_to_screen(&self.view, self.viewport, x, y)
    }

    fn at2(&self, p: [f64; 2]) -> GPoint<f32> {
        self.at(p[0], p[1])
    }

    /// The page rect `(x0, y0)-(x1, y1)`, normalised, as window pixels.
    fn rect(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Bounds<Pixels> {
        corners(self.at(x0, y0), self.at(x1, y1))
    }

    fn line(&mut self, a: GPoint<f32>, b: GPoint<f32>, width: f32, color: Hsla) {
        let mut path = PathBuilder::stroke(px(width));
        path.move_to(px_point(a));
        path.line_to(px_point(b));
        if let Ok(p) = path.build() {
            self.window.paint_path(p, color);
        }
    }

    /// A page-space segment.
    fn seg(&mut self, a: [f64; 2], b: [f64; 2], width: f32, color: Hsla) {
        let (a, b) = (self.at2(a), self.at2(b));
        self.line(a, b, width, color);
    }

    fn dashed_seg(&mut self, a: [f64; 2], b: [f64; 2], width: f32, color: Hsla) {
        let (a, b) = (self.at2(a), self.at2(b));
        let mut path = PathBuilder::stroke(px(width)).dash_array(&[px(DASH[0]), px(DASH[1])]);
        path.move_to(px_point(a));
        path.line_to(px_point(b));
        if let Ok(p) = path.build() {
            self.window.paint_path(p, color);
        }
    }

    /// A polyline through window points, stroked.
    fn stroke_poly(&mut self, pts: &[GPoint<f32>], width: f32, color: Hsla, close: bool) {
        if pts.len() < 2 {
            return;
        }
        let mut path = PathBuilder::stroke(px(width));
        if let Some(first) = pts.first().copied() {
            path.move_to(px_point(first));
            for p in pts.iter().skip(1) {
                path.line_to(px_point(*p));
            }
            if close {
                path.close();
            }
            if let Ok(p) = path.build() {
                self.window.paint_path(p, color);
            }
        }
    }

    /// A polyline through window points, dashed.
    fn dashed_poly(&mut self, pts: &[GPoint<f32>], width: f32, color: Hsla) {
        if pts.len() < 2 {
            return;
        }
        let mut path = PathBuilder::stroke(px(width)).dash_array(&[px(DASH[0]), px(DASH[1])]);
        if let Some(first) = pts.first().copied() {
            path.move_to(px_point(first));
            for p in pts.iter().skip(1) {
                path.line_to(px_point(*p));
            }
            if let Ok(p) = path.build() {
                self.window.paint_path(p, color);
            }
        }
    }

    /// A closed polygon through window points, filled.
    fn fill_poly(&mut self, pts: &[GPoint<f32>], color: Hsla) {
        if pts.len() < 3 {
            return;
        }
        let pixels: Vec<GPoint<Pixels>> = pts.iter().map(|p| px_point(*p)).collect();
        let mut path = PathBuilder::fill();
        path.add_polygon(&pixels, true);
        if let Ok(p) = path.build() {
            self.window.paint_path(p, color);
        }
    }

    /// A page-space quadrilateral (tl, tr, br, bl), stroked.
    fn quad(&mut self, q: [Point; 4], width: f32, color: Hsla) {
        let pts: Vec<GPoint<f32>> = q.iter().map(|p| self.at(p.x, p.y)).collect();
        self.stroke_poly(&pts, width, color, true);
    }

    /// A page-space cubic `[a, c1, c2, d]`, stroked: a vector segment, or the pen's rubber band.
    fn cubic(&mut self, seg: [[f64; 2]; 4], width: f32, color: Hsla) {
        let (a, b, c, d) = (
            px_point(self.at2(seg[0])),
            px_point(self.at2(seg[1])),
            px_point(self.at2(seg[2])),
            px_point(self.at2(seg[3])),
        );
        let mut path = PathBuilder::stroke(px(width));
        path.move_to(a);
        path.cubic_bezier_to(d, b, c);
        if let Ok(p) = path.build() {
            self.window.paint_path(p, color);
        }
    }

    fn fill(&mut self, bounds: Bounds<Pixels>, color: Hsla) {
        if bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
            return;
        }
        self.window.paint_quad(PaintQuad {
            bounds,
            corner_radii: Corners::all(px(0.0)),
            background: color.into(),
            border_widths: Default::default(),
            border_color: self.pal.none,
            border_style: Default::default(),
        });
    }

    fn fill_radii(&mut self, bounds: Bounds<Pixels>, radius: f32, color: Hsla) {
        if bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
            return;
        }
        self.window.paint_quad(PaintQuad {
            bounds,
            corner_radii: Corners::all(px(radius)),
            background: color.into(),
            border_widths: Default::default(),
            border_color: self.pal.none,
            border_style: Default::default(),
        });
    }

    fn stroke_box(&mut self, bounds: Bounds<Pixels>, width: f32, color: Hsla) {
        if bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
            return;
        }
        let w = px(width);
        self.window.paint_quad(PaintQuad {
            bounds,
            corner_radii: Corners::all(px(0.0)),
            background: self.pal.none.into(),
            border_widths: Edges {
                top: w,
                bottom: w,
                left: w,
                right: w,
            },
            border_color: color,
            border_style: Default::default(),
        });
    }

    /// A dot: a `fill` disc with an optional `ring` around it, as every handle and anchor is
    /// drawn.
    ///
    /// Two arcs rather than a `circle` call, because GPUI's `PathBuilder` has no circle.
    fn dot(&mut self, at: GPoint<f32>, radius: f32, fill: Hsla, ring: Option<Hsla>) {
        if !(radius > 0.0) || !radius.is_finite() {
            return;
        }
        let (x, y, r) = (px(at.x), px(at.y), px(radius));
        if fill.a > 0.0 {
            let mut path = PathBuilder::fill();
            circle_path(&mut path, x, y, r);
            if let Ok(p) = path.build() {
                self.window.paint_path(p, fill);
            }
        }
        if let Some(ring) = ring {
            let mut path = PathBuilder::stroke(px(LINE_PX));
            circle_path(&mut path, x, y, r);
            if let Ok(p) = path.build() {
                self.window.paint_path(p, ring);
            }
        }
    }

    /// The selection's corner handle: a white square with an accent ring.
    fn corner_handle(&mut self, at: GPoint<f32>, color: Hsla) {
        let b = Bounds::new(
            point(px(at.x - CORNER_PX / 2.0), px(at.y - CORNER_PX / 2.0)),
            size(px(CORNER_PX), px(CORNER_PX)),
        );
        self.fill_radii(b, 1.0, self.pal.white);
        self.stroke_box(b, LINE_PX, color);
    }

    /// A readout: `text` in `ink` on a pill in `fill`, top-left at `at`. Its size is returned so a
    /// caller can lay labels out around each other.
    fn pill(&mut self, text: &str, at: GPoint<f32>, fill: Hsla, ink: Hsla) -> (f32, f32) {
        let (pad_x, pad_y) = (4.0_f32, 2.0_f32);
        let (w, h) = self.text(&text, ink, at, fill, pad_x, pad_y);
        (w + pad_x * 2.0, h + pad_y * 2.0)
    }

    /// `text` at `at` with no background. Its width is returned.
    fn label(&mut self, text: &str, at: GPoint<f32>, ink: Hsla) -> f32 {
        self.text(text, ink, at, self.pal.none, 0.0, 0.0).0
    }

    /// The shared body of [`Overlays::label`] and [`Overlays::pill`]: shape, draw, return the
    /// glyph size.
    fn text(
        &mut self,
        text: &str,
        ink: Hsla,
        at: GPoint<f32>,
        fill: Hsla,
        pad_x: f32,
        pad_y: f32,
    ) -> (f32, f32) {
        let label = truncate(text, MAX_LABEL_CHARS);
        let height = LABEL_PT * LABEL_LINE;
        let run = TextRun {
            len: label.len(),
            font: self.window.text_style().font(),
            color: ink.into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = self.window.text_system().shape_line(
            SharedString::from(label),
            px(LABEL_PT),
            &[run],
            None,
        );
        let w = f32::from(line.width());
        if fill.a > 0.0 {
            let b = Bounds::new(
                point(px(at.x), px(at.y)),
                size(px(w + pad_x * 2.0), px(height + pad_y * 2.0)),
            );
            self.fill_radii(b, 3.0, fill);
        }
        let _ = line.paint(
            point(px(at.x + pad_x), px(at.y + pad_y)),
            px(height),
            gpui::TextAlign::Left,
            None,
            self.window,
            self.app,
        );
        (w, height)
    }
}

/// A full circle as two half-arcs, the only circle GPUI's path builder can express.
fn circle_path(path: &mut PathBuilder, x: Pixels, y: Pixels, r: Pixels) {
    path.move_to(point(x + r, y));
    path.arc_to(point(r, r), px(0.0), false, true, point(x - r, y));
    path.arc_to(point(r, r), px(0.0), false, true, point(x + r, y));
    path.close();
}

fn px_point(p: GPoint<f32>) -> GPoint<Pixels> {
    point(px(p.x), px(p.y))
}

fn f32_point(p: GPoint<Pixels>) -> GPoint<f32> {
    point(f32::from(p.x), f32::from(p.y))
}

/// The rectangle with `a` and `b` as opposite corners, whichever is which.
fn corners(a: GPoint<f32>, b: GPoint<f32>) -> Bounds<Pixels> {
    Bounds::new(
        point(px(a.x.min(b.x)), px(a.y.min(b.y))),
        size(px((a.x - b.x).abs()), px((a.y - b.y).abs())),
    )
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Whether the canvas must ask for another frame.
///
/// The rule, in one place, because it is the difference between a canvas that
/// keeps up and one that shows a stale picture until the user touches it:
///
/// * `drawn` is the generation of the frame the last paint drew, `current` the
///   newest frame the render thread has published. A frame that arrived *while
///   the canvas was painting* is newer than the one on the screen, so the
///   canvas has to ask for another one. Without this the canvas sits on a
///   stale frame until the next input event — an undone object that takes a
///   click to come back.
/// * `awaiting` is the generation of the answer the canvas is waiting for. A
///   request in flight means an answer is coming that is not on the screen
///   yet, and the canvas has to be awake to draw it.
/// * A faulted renderer never publishes, so it is explicitly *not* pending:
///   otherwise the canvas would ask for a redraw forever, waiting for a frame
///   that cannot arrive.
fn needs_repaint(drawn: u64, current: u64, awaiting: u64, faulted: bool) -> bool {
    if faulted {
        return false;
    }
    current > drawn || current < awaiting
}

/// Where the engine's frame goes on the window, in logical points.
///
/// The engine renders one device pixel wider and taller than the view it was given, and the device
/// origin is absolute in page space, so the placement has to come from the frame rather than from
/// the viewport: [`CanvasFrame::origin_in_view`] measures from the viewport origin the request was
/// made for, and [`CanvasFrame::logical_size`] divides out the ratio it was rendered at.
fn blit_bounds(
    f: &CanvasFrame,
    view_origin: (f64, f64),
    zoom: f64,
    viewport_origin: GPoint<f32>,
) -> Bounds<Pixels> {
    let (x, y) = f.origin_in_view(view_origin, zoom);
    let (w, h) = f.logical_size(zoom);
    // THE VIEWPORT'S ORIGIN, or the image lands in the wrong place.
    //
    // `origin_in_view` measures from the VIEWPORT's top-left, and the overlay
    // draws in WINDOW coordinates: `page_to_screen` adds the viewport's origin
    // to every point. `paint_image` is given the element's bounds, which are
    // window coordinates too, and intersects the destination with them -- so a
    // destination in viewport-relative coordinates is a different place
    // entirely, and the engine's pixels end up somewhere the selection is not.
    //
    // This was not visible while the canvas element sat at the window's
    // top-left, and it broke the moment it did not: the canvas sits beside a
    // sidebar and below a toolbar, so the viewport's origin is never zero.
    Bounds::new(
        point(px(viewport_origin.x + x), px(viewport_origin.y + y)),
        size(px(w), px(h)),
    )
}

// ---------------------------------------------------------------------------
// Modifiers
// ---------------------------------------------------------------------------

/// The modifiers of a GPUI event, in the names the engine uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    /// Cmd on macOS, the Windows key elsewhere — `Modifiers::platform`.
    pub cmd: bool,
    /// `Modifiers::control`, which figcraft uses as the word modifier while editing text.
    pub ctrl: bool,
}

impl Mods {
    pub fn of(m: &gpui::Modifiers) -> Self {
        Self {
            shift: m.shift,
            alt: m.alt,
            cmd: m.platform,
            ctrl: m.control,
        }
    }

    /// The command modifier, the way the engine's keymap means it.
    ///
    /// figcraft writes `Cmd` for the platform's command key in every spec, and
    /// resolves it to Control off macOS — which is why its Windows and Linux
    /// column reads `Ctrl` where the macOS one reads `Cmd`. GPUI reports the
    /// two separately, so the platform's rule is applied here rather than in
    /// every caller: on macOS the command key is the platform modifier, and
    /// everywhere else it is Control.
    ///
    /// This is EXACTLY the engine's own `PointerEvent::cmd` — "Cmd on macOS,
    /// Ctrl elsewhere" — so the pointer path passes this, not [`Self::cmd`].
    /// Passing the physical platform modifier there meant that on Windows and
    /// Linux a Control-click did not deep-select, and the Windows key did:
    /// the opposite of every product on those platforms.
    pub fn command(&self) -> bool {
        self.command_for(figcraft_engine::keymap::Platform::current())
    }

    /// The command modifier, for a given platform.
    ///
    /// Split out from [`Self::command`] so the platform's rule can be tested
    /// for platforms this build is not running on: only macOS has ever been
    /// run here, and the rule is different on the other two.
    pub fn command_for(&self, platform: figcraft_engine::keymap::Platform) -> bool {
        if platform == figcraft_engine::keymap::Platform::Mac {
            self.cmd
        } else {
            self.ctrl
        }
    }
}

/// How far a text editing key moves or deletes, as the engine's `(word, line)`.
///
/// The engine's own rule, read for the platform this is running on: macOS
/// moves by word with `⌥` and to the line's ends with `⌘`; Windows and Linux
/// move by word with `Ctrl` and to the text's ends with `Ctrl`+`Home`/`End`.
/// The engine says so in one place, so the canvas asks it rather than
/// deciding which modifier means what.
fn text_motion(name: &str, m: Mods) -> (bool, bool) {
    figcraft_engine::keymap::text_motion(
        figcraft_engine::keymap::Platform::current(),
        name,
        figcraft_engine::keymap::Mods {
            shift: m.shift,
            alt: m.alt,
            cmd: m.cmd,
            ctrl: m.ctrl,
        },
    )
}

/// A wheel event's delta in window pixels, whatever the platform reported.
pub fn wheel_pixels(delta: ScrollDelta) -> (f32, f32) {
    match delta {
        ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        ScrollDelta::Lines(l) => (f32::from(l.x) * 40.0, f32::from(l.y) * 40.0),
    }
}

// ---------------------------------------------------------------------------
// The canvas
// ---------------------------------------------------------------------------

/// What the canvas itself owns between frames: input state, the render request in flight, and what
/// the last frame consumed.
///
/// None of this belongs in the document or the session. Selection, tool, hover, camera and the text
/// edit all live in the engine and are read from there.
pub struct CanvasState {
    /// The camera has not been framed onto a newly loaded document.
    ///
    /// It cannot be framed at load time: the camera's fit needs the canvas's own
    /// pixel size, and that is not known until the element has been laid out. So
    /// the request is recorded here and honoured on the first paint that has a
    /// non-empty viewport. Fitting earlier submits a request with a zero-size
    /// viewport, which the render thread refuses, and the refusal used to be
    /// swallowed -- so the project opened to a permanently black canvas.
    pub pending_fit: bool,
    /// A first render request has been logged, so the launch log says once.
    pub reported_first_submit: bool,
    /// The worker thread refused a frame. Sticky, because the cause does not clear
    /// on its own and a retried request would only hide it again.
    pub render_failed: bool,
    /// A button went down on the canvas and has not come up: keep sending moves even if the
    /// pointer has left the element.
    pub dragging: bool,
    /// The last page point the pointer was at, so a drag released off the canvas can end.
    pub last_pointer: Option<Point>,
    /// The pointer was over the canvas last frame (leaving it clears what it hovered).
    pub hovering: bool,
    /// Space is held: a drag pans.
    pub space_pan: bool,
    /// A middle-button pan in progress: where it started, and the camera centre at that moment.
    pub middle_pan: Option<(GPoint<f32>, f64, f64)>,
    /// The zoom tool (Z held): `Some(true)` is Alt, zooming out.
    pub zoom_tool: Option<bool>,
    /// Where the zoom tool's drag started.
    pub zoom_drag: Option<GPoint<f32>>,
    /// The composition view, rebuilt when the document, composition or caret changes.
    pub composition: Option<Composition>,
    /// The generation the canvas is waiting for; the render thread publishes one per frame.
    pub awaiting: u64,
    /// The generation of the frame the last paint drew.
    ///
    /// A frame that arrives while the canvas is painting is not on the screen
    /// yet, and the canvas has to ask for another one or the picture stays
    /// stale until the next input event — which is what made an undone object
    /// take a click to come back.
    pub drawn: u64,
    /// The labels the last paint drew, fed back to the engine so a press on one hits its frame.
    pub labels: Vec<(NodeId, [Point; 4])>,
    /// The last error the engine reported, for a diagnostics line.
    pub fault: Option<String>,
    /// How many times the canvas has painted, so the diagnostics can be
    /// reported every so often rather than on every frame.
    pub paints: u64,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            pending_fit: false,
            reported_first_submit: false,
            render_failed: false,
            dragging: false,
            last_pointer: None,
            hovering: false,
            space_pan: false,
            middle_pan: None,
            zoom_tool: None,
            zoom_drag: None,
            composition: None,
            awaiting: 0,
            drawn: 0,
            labels: Vec::new(),
            fault: None,
            paints: 0,
        }
    }
}

/// Spool's canvas: the rendered page plus the overlays the engine asks for.
///
/// An ordinary GPUI entity. Put it in a `div().flex_1()` and it fills the space it is given; hand
/// [`EngineCanvas::session_handle`] to the panels so they read and act through the same session.
pub struct EngineCanvas {
    /// The Prototype tab is open, so the canvas draws the prototype's
    /// connections.
    prototype_mode: bool,
    /// The Dev Mode tab is open, so the canvas draws the annotations' callouts
    /// and the engine measures distances on hover.
    dev_mode: bool,
    /// Which annotations the canvas's callouts show.
    dev_filter: crate::engine::panel_devmode::CategoryFilter,
    /// The engine. Shared with the panels through [`EngineCanvas::session_handle`].
    session: Rc<RefCell<Session>>,
    /// The retained render, on its own thread.
    renderer: CanvasRenderer,
    focus: FocusHandle,
    /// Whether another surface owns the keyboard.
    ///
    /// A text field in the Inspector is a text field: a character typed into
    /// it is a character, not a canvas gesture. The canvas is the only
    /// surface that takes the keyboard for itself, so it is the only one that
    /// has to be told when a field wants it instead.
    ///
    /// The Inspector decides, because it is the surface that knows which of
    /// its fields are open.
    keyboard_claimed: bool,
    state: CanvasState,
    palette: Palette,
    /// The hitbox the last prepaint inserted: the cursor style needs it by reference.
    hitbox: Rc<RefCell<Option<Hitbox>>>,
    /// Where the canvas element is, from the last prepaint.
    viewport: Viewport,
    /// What the ruler-guides layer remembers between frames.
    guide_state: Rc<RefCell<guides::GuideState>>,
    /// When the entity was made, for the caret's blink.
    born: Instant,
}

impl EngineCanvas {
    /// A canvas over a session it then owns.
    pub fn new(session: Session, cx: &mut gpui::Context<Self>) -> Self {
        Self::build(Rc::new(RefCell::new(session)), cx)
    }

    /// A canvas over a fresh session.
    pub fn empty(cx: &mut gpui::Context<Self>) -> Self {
        Self::new(Session::new(), cx)
    }

    /// A canvas over a session something else already owns.
    pub fn from_handle(session: Rc<RefCell<Session>>, cx: &mut gpui::Context<Self>) -> Self {
        Self::build(session, cx)
    }

    fn build(session: Rc<RefCell<Session>>, cx: &mut gpui::Context<Self>) -> Self {
        Self {
            session,
            renderer: CanvasRenderer::new(),
            focus: cx.focus_handle(),
            keyboard_claimed: false,
            prototype_mode: false,
            dev_mode: false,
            dev_filter: crate::engine::panel_devmode::CategoryFilter::All,
            state: CanvasState::default(),
            palette: Palette::new(),
            hitbox: Rc::new(RefCell::new(None)),
            viewport: Viewport::empty(),
            guide_state: Rc::new(RefCell::new(guides::GuideState::default())),
            born: Instant::now(),
        }
    }

    // --- the shared session -------------------------------------------------

    /// The session, for panels that want to read or act through it directly.
    pub fn session_handle(&self) -> Rc<RefCell<Session>> {
        Rc::clone(&self.session)
    }

    /// The engine's own count of edits to the open document.
    ///
    /// The layers panel shares this session, so this is a number both panels
    /// agree on without either of them telling the other anything. It exists so
    /// the shell can invalidate across panels ONLY when the document actually
    /// changed: two entities that observe each other and notify unconditionally
    /// wake each other forever, because in GPUI every notification is itself an
    /// observation. Comparing this is what breaks that cycle.
    pub fn document_revision(&self) -> u64 {
        let session = self.session.borrow();
        session
            .docs
            .get(session.active)
            .map_or(0, |doc| doc.revision)
    }

    /// Choose a tool, by the engine's own name.
    ///
    /// The name is the engine's, not Spool's: `move`, `frame`, `rectangle`,
    /// `ellipse`, `pen`, `text`, `comment`. It goes through `tool.set`, the
    /// command the CLI and the palette use, rather than reaching into the
    /// session's tool state -- one operation surface, so the same tool can be
    /// set by a click, a shortcut, the CLI or an agent.
    pub fn set_tool(&mut self, tool: &str, cx: &mut gpui::Context<Self>) {
        let params = serde_json::json!({ "tool": tool });
        self.run_engine_command("tool.set", params, cx);
    }

    /// The engine's name for the tool in effect.
    ///
    /// The toolbar asks this rather than keeping its own answer, so the
    /// highlighted tool is the engine's and cannot drift from it.
    pub fn active_tool(&self) -> &'static str {
        use figcraft_engine::tools::ToolId;
        match self.session.borrow().tool.tool {
            ToolId::Move => "move",
            ToolId::Scale => "scale",
            ToolId::Hand => "hand",
            ToolId::Frame => "frame",
            ToolId::Section => "section",
            ToolId::Slice => "slice",
            ToolId::Rectangle => "rectangle",
            ToolId::Line => "line",
            ToolId::Arrow => "arrow",
            ToolId::Ellipse => "ellipse",
            ToolId::Polygon => "polygon",
            ToolId::Star => "star",
            ToolId::Pen => "pen",
            ToolId::Pencil => "pencil",
            ToolId::Text => "text",
            ToolId::Comment => "comment",
            ToolId::Eyedropper => "eyedropper",
            ToolId::ShapeBuilder => "shapeBuilder",
        }
    }

    /// Run one engine command, and report what it did.
    ///
    /// The engine's commands are the operation surface: the toolbar, the
    /// keymap, the command palette, the CLI and a future agent all dispatch
    /// command ids, so an edit made by a click and an edit made by a shortcut
    /// go through the same code and produce the same history.
    ///
    /// A refused command is not an error the user has to be told about -- the
    /// engine declines commands that do not apply to the current state, and
    /// reporting every one of those as a failure would be noise. It IS worth
    /// reporting, because a command that fails for an unexpected reason is a
    /// bug, and the user should be able to see it.
    pub fn run_engine_command(
        &mut self,
        id: &str,
        params: serde_json::Value,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let result = self.session.borrow_mut().execute(id, &params);
        cx.notify();
        match result {
            Ok(_) => true,
            Err(error) => {
                eprintln!("spool_engine_command_failed id={id} error={error}");
                false
            }
        }
    }

    /// Turn the prototype's connections on and off.
    ///
    /// The Prototype tab is a mode as well as a panel: while it is open the
    /// canvas draws the noodles between the layers that act on one another.
    pub fn set_prototype_mode(&mut self, active: bool, cx: &mut gpui::Context<Self>) {
        if self.prototype_mode != active {
            self.prototype_mode = active;
            cx.notify();
        }
    }

    /// Turn Dev Mode on and off.
    ///
    /// Dev Mode is a MODE as well as a panel: while it is open the canvas
    /// draws the annotations' callouts, and the engine measures distances on
    /// hover without Alt held -- `tool.redlines`, which is what
    /// `figcraft_engine::distance::hover_lines` answers.
    pub fn set_dev_mode(&mut self, active: bool, cx: &mut gpui::Context<Self>) {
        if self.dev_mode != active {
            self.dev_mode = active;
            self.session.borrow_mut().tool.redlines = active;
            cx.notify();
        }
    }

    /// Which annotations the canvas's callouts show.
    ///
    /// The Inspector owns the filter and the canvas draws the callouts it
    /// selects, so the two have to agree.
    pub fn set_dev_category_filter(
        &mut self,
        filter: crate::engine::panel_devmode::CategoryFilter,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.dev_filter != filter {
            self.dev_filter = filter;
            cx.notify();
        }
    }

    /// Whether the canvas holds the keyboard focus.
    ///
    /// The shell needs this to know whose keyboard it is: while the engine
    /// canvas has focus, the engine's own key handling is the authority, and
    /// the shell must not answer the same chord a second time through the
    /// retired runtime's command table.
    pub fn has_focus(&self, window: &gpui::Window) -> bool {
        self.focus.is_focused(window)
    }

    /// The engine's own escape: cancel whatever the engine is in the middle of.
    ///
    /// A gesture in flight, an open text session, or neither. The engine knows
    /// which, so the shell does not have to guess and cannot cancel the wrong
    /// thing.
    pub fn escape(&mut self, cx: &mut gpui::Context<Self>) -> bool {
        self.with_session_mut(cx, |session| session.escape().is_ok())
    }

    /// Move the camera by a screen-space amount.
    ///
    /// The engine's own scroll, so the camera is the session's `ViewState` and
    /// not a second copy that could disagree with it.
    pub fn pan_by(&mut self, dx: f32, dy: f32, cx: &mut gpui::Context<Self>) {
        self.with_session_mut(cx, |session| {
            if let Ok(ds) = session.ds_mut() {
                ds.view.scroll(f64::from(dx), f64::from(dy), false);
            }
        });
    }

    /// Run `f` with the session. The borrow cannot outlive the call.
    pub fn with_session<R>(&self, f: impl FnOnce(&Session) -> R) -> R {
        f(&self.session.borrow())
    }

    /// Run `f` with the session mutably, and redraw afterwards.
    pub fn with_session_mut<R>(
        &mut self,
        cx: &mut gpui::Context<Self>,
        f: impl FnOnce(&mut Session) -> R,
    ) -> R {
        let out = f(&mut self.session.borrow_mut());
        cx.notify();
        out
    }

    // --- what panels read ---------------------------------------------------

    /// The selection.
    pub fn selection(&self) -> Vec<NodeId> {
        self.session.borrow().selection()
    }

    /// The active tool.
    pub fn tool(&self) -> ToolId {
        self.session.borrow().tool.tool
    }

    /// Whether the engine is mid-gesture.
    pub fn busy(&self) -> bool {
        self.session.borrow().tool.busy()
    }

    /// The hovered layer, for an outline a panel draws.
    pub fn hover(&self) -> Option<NodeId> {
        self.session.borrow().ds().ok().and_then(|ds| ds.hover)
    }

    /// The text being edited on the canvas.
    pub fn text_edit(&self) -> Option<figcraft_engine::TextEdit> {
        self.session
            .borrow()
            .ds()
            .ok()
            .and_then(|ds| ds.text_edit.clone())
    }

    /// The camera: what's at the centre of the viewport and how big it is.
    pub fn view(&self) -> ViewState {
        self.session
            .borrow()
            .ds()
            .map(|ds| ds.view.clone())
            .unwrap_or_default()
    }

    /// The overlay the engine wants drawn this frame.
    pub fn overlay(&self) -> OverlayView {
        OverlayView::from_overlay(&self.session.borrow().tool.overlay)
    }

    /// The overlay as [`Session::tool_info`] reports it.
    pub fn tool_info(&self) -> Value {
        self.session.borrow().tool_info()
    }

    /// The live text session: the caret and its anchor, in characters.
    pub fn caret(&self) -> Option<(usize, usize)> {
        self.text_edit().map(|te| (te.caret, te.anchor))
    }

    /// The canvas's keyboard focus handle.
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The viewport the canvas was last laid out at.
    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// One line describing what the renderer has done, for a diagnostics view.
    pub fn diagnostics(&self) -> String {
        let mut line = self.renderer.diagnostics();
        if let Some(fault) = self.state.fault.as_ref() {
            line.push_str(" · engine: ");
            line.push_str(fault);
        }
        line
    }

    /// Whether the picture on the canvas is of the document as it is now.
    ///
    /// The engine's document is immutable behind its `Arc` — a change is a new
    /// `Arc` — so the frame's document and the session's document are the same
    /// document when their addresses are the same. A frame that is not current
    /// is a frame that was rendered before the last edit, and the canvas is
    /// showing a picture of a document that is not the one on screen.
    pub fn frame_is_current(&self) -> bool {
        let Some(frame) = self.renderer.frame() else {
            return false;
        };
        let session = self.session.borrow();
        let Ok(ds) = session.ds() else { return false };
        frame.doc == Arc::as_ptr(&ds.doc) as usize
    }

    /// Whether the canvas is waiting for a frame it has not drawn.
    ///
    /// A canvas that is waiting has asked for another frame; one that is not
    /// has everything it needs and the app can go idle.
    pub fn needs_repaint(&self) -> bool {
        self.frame_pending()
    }

    // --- what panels drive --------------------------------------------------

    /// Ask for the camera to be framed on the next paint that has a size.
    ///
    /// This is the correct way to frame a freshly loaded document, and
    /// [`Self::zoom_to_fit`] is not: a fit computes the camera from the canvas's
    /// pixel size, which before the first layout is zero, and a render submitted
    /// with a zero-size viewport is refused by the render thread.
    pub fn fit_on_next_paint(&mut self) {
        self.state.pending_fit = true;
    }

    /// Frame the page.
    pub fn zoom_to_fit(&mut self, cx: &mut gpui::Context<Self>) {
        self.with_session_mut(cx, |s| {
            let page = match s.page() {
                Ok(p) => p,
                Err(_) => return,
            };
            let Ok(ds) = s.ds_mut() else { return };
            if let Some(b) = ds.doc.world_bounds(page) {
                ds.view.fit(b, 4.0);
            }
        });
    }

    /// Frame the selection, or the page when nothing is selected.
    pub fn zoom_to_selection(&mut self, cx: &mut gpui::Context<Self>) {
        self.with_session_mut(cx, |s| {
            let Ok(ds) = s.ds() else { return };
            let selection = ds.selection.clone();
            let page = s.page().ok();
            let bounds = selection
                .first()
                .and_then(|id| ds.doc.world_bounds(*id))
                .or_else(|| page.and_then(|p| ds.doc.world_bounds(p)));
            let Some(b) = bounds else { return };
            let Ok(ds) = s.ds_mut() else { return };
            ds.view.fit(b, 8.0);
        });
    }

    /// Undo, if there is anything to undo.
    ///
    /// A text session owns undo while it is open: the session's own step is
    /// undone, and the document's history is not reached. See
    /// [`Self::undo_text`].
    pub fn undo(&mut self, cx: &mut gpui::Context<Self>) -> bool {
        if self.is_editing_text() {
            return self.undo_text(cx);
        }
        self.with_session_mut(cx, |s| s.undo()).unwrap_or(false)
    }

    /// Redo, if there is anything to redo.
    ///
    /// A text session owns redo while it is open, and an open session has
    /// nothing to redo: it has not been committed yet, so there is no text
    /// step to replay, and the document's redo stack is not the session's to
    /// spend.
    pub fn redo(&mut self, cx: &mut gpui::Context<Self>) -> bool {
        if self.is_editing_text() {
            return false;
        }
        self.with_session_mut(cx, |s| s.redo()).unwrap_or(false)
    }

    /// Run an engine command. The only way this canvas changes the document outside a gesture.
    pub fn run(&mut self, cx: &mut gpui::Context<Self>, id: &str, params: Value) -> bool {
        self.with_session_mut(cx, |s| s.execute(id, &params).is_ok())
    }

    /// Give the canvas keyboard focus.
    pub fn focus(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        self.focus.focus(window, cx);
    }

    /// Say whether another surface owns the keyboard.
    ///
    /// The canvas takes the focus for itself on every click and answers keys
    /// as the engine's gestures, so it is the one surface that has to be told
    /// when a text field wants the keyboard instead: `z` typed into a colour
    /// code is not the zoom tool, a space typed into one is not a pan, and a
    /// click on the canvas does not take the field's focus away.
    ///
    /// The Inspector decides, because it is the surface that knows which of
    /// its fields are open.
    pub fn set_keyboard_claimed(&mut self, claimed: bool, cx: &mut gpui::Context<Self>) {
        if self.keyboard_claimed != claimed {
            self.keyboard_claimed = claimed;
            cx.notify();
        }
    }

    /// Whether another surface owns the keyboard.
    pub fn keyboard_claimed(&self) -> bool {
        self.keyboard_claimed
    }

    // --- rendering ----------------------------------------------------------

    /// The render options for the current view state, mirroring figcraft's `JobKey`.
    ///
    /// Outline mode, the selection-only variant, the solo frame and the LOD preview all come from
    /// the engine's own view state, so a View-menu command changes what the canvas renders with and
    /// there is no second copy of the flags to fall out of step.
    fn render_options(ds: &DocState) -> Options {
        let view = &ds.view;
        Options {
            background: None,
            outline_mode: view.outlines,
            outline: figcraft_render::Outline {
                hidden: view.outline_hidden,
                bounds: view.outline_bounds,
                light: false,
            },
            outlined: if view.outlines_selection {
                ds.selection.clone()
            } else {
                Vec::new()
            },
            skip: Vec::new(),
            solo: ds.focused(),
            lod: true,
        }
    }

    /// Ask the render thread for the viewport.
    ///
    /// The renderer coalesces to one pending request, so a pointer move that changes nothing costs
    /// at most one discarded submission.
    ///
    /// A request that would be answered with the frame that is already there is
    /// not made at all: the canvas paints on every frame it is given, and a
    /// request that cannot change the picture is a thread round trip and a
    /// repaint for nothing — and, worse, a repaint the user waits for. figcraft's
    /// egui frontend keeps the same key and does the same thing.
    fn request_render(&mut self, ratio: f32) {
        let request = {
            let session = self.session.borrow();
            let Ok(ds) = session.ds() else { return };
            (
                ds.doc.clone(),
                ds.page,
                ds.view.origin(),
                ds.view.zoom,
                Self::render_options(ds),
            )
        };
        let (doc, page, origin, zoom, options) = request;
        if self.renderer.would_repeat(
            &doc,
            page,
            origin,
            zoom,
            self.viewport.size,
            ratio,
            &options,
        ) {
            return;
        }
        match self.renderer.render_canvas(
            doc,
            page,
            origin,
            zoom,
            self.viewport.size,
            ratio,
            options,
        ) {
            Ok(_) => {
                self.state.awaiting = self.renderer.generation() + 1;
                // Once, on the first submission that the render thread accepted.
                // A canvas that renders nothing and a canvas that was never asked
                // to render anything look identical from the outside, so this says
                // which one happened.
                if !self.state.reported_first_submit {
                    self.state.reported_first_submit = true;
                    eprintln!(
                        "SPOOL_RENDER_SUBMITTED page={:?} origin={:?} zoom={} viewport={:?}",
                        page, origin, zoom, self.viewport.size,
                    );
                }
            }
            Err(error) => {
                // Previously swallowed. A render that fails on the worker thread
                // leaves a black canvas with no symptom anywhere, which is
                // indistinguishable from an empty page.
                eprintln!("SPOOL_RENDER_FAILED {error:?}");
                self.state.render_failed = true;
            }
        }
    }

    /// Whether the canvas has to ask for another frame.
    ///
    /// See [`needs_repaint`], which is the rule and the reason.
    fn frame_pending(&self) -> bool {
        needs_repaint(
            self.state.drawn,
            self.renderer.generation(),
            self.state.awaiting,
            self.renderer.fault().is_some(),
        )
    }

    // --- input --------------------------------------------------------------

    /// Send one pointer event to the engine. The only path into the gesture state machine.
    fn forward(&mut self, kind: PointerKind, at: GPoint<f32>, m: Mods, pressure: Option<f32>) {
        // The engine's own gesture work, on the UI thread: the document preview
        // it builds for a drag is the whole cost of a pointer move, and it is
        // what the pointer waits behind.
        let started = crate::diagnostics::start();
        let ev = {
            let session = self.session.borrow();
            let Ok(ds) = session.ds() else { return };
            pointer_event(
                &ds.view,
                self.viewport,
                kind,
                at,
                m.shift,
                m.alt,
                // The engine's command modifier: Cmd on macOS, Control
                // elsewhere. The physical platform modifier would mean
                // Control-click did not deep-select on Windows or Linux.
                m.command(),
                m.ctrl,
                pressure,
            )
        };
        self.state.last_pointer = Some(Point::new(ev.x, ev.y));
        let result = self.session.borrow_mut().pointer(&ev);
        crate::diagnostics::record("canvas_pointer", started);
        if let Err(error) = result {
            self.state.fault = Some(error.to_string());
        }
    }

    /// Handle a keystroke.
    ///
    /// Text editing first (the engine's own `textedit::key`), then the engine's `tool_key`, then
    /// Escape, which is the one key that means something at both levels.
    ///
    /// A CHARACTER is not written here at all: the platform's text input is
    /// the one writer, and it carries the character the user meant where the
    /// key event carries only the key that was pressed. See
    /// [`EntityInputHandler::replace_text_in_range`].
    fn handle_key(&mut self, keystroke: &Keystroke, cx: &mut gpui::Context<Self>) {
        let m = Mods::of(&keystroke.modifiers);
        if self.is_editing_text() {
            // A TEXT SESSION OWNS UNDO, REDO AND SELECT-ALL.
            //
            // The engine's own keymap is the authority on this: `edit.undo`,
            // `edit.redo`, `select.all` and `edit.duplicate` are CANVAS
            // chords, so they are not live while text is being edited — the
            // text session is above the canvas. The shell enforces that half
            // (see [`crate::engine::keymap::live_while_editing_text`]), which
            // is what lets these chords reach the session here.
            //
            //   * `⌘A` selects the TEXT. The layers are not selected.
            //   * `⌘Z` undoes the TEXT: the session's own edit, which is one
            //     history step, because a whole editing session is one undo
            //     step. The document's own history is not reachable from
            //     here.
            //   * `⌘⇧Z` has nothing to redo: the session has not been
            //     committed yet, so there is no text step to replay, and the
            //     document's redo stack is not this session's to spend.
            //   * `⌘D` is not a text verb at all. It does nothing.
            //
            // `⌘⇧A` is inverse selection, which is a canvas chord too, so it
            // does nothing here either.
            if m.command() {
                match keystroke.key.as_str() {
                    "a" if !m.shift => {
                        let _ = textedit::key(
                            &mut self.session.borrow_mut(),
                            "SelectAll",
                            false,
                            false,
                            false,
                        );
                        cx.notify();
                        return;
                    }
                    "z" if !m.shift => {
                        self.undo_text(cx);
                        return;
                    }
                    "z" => return,
                    _ => {}
                }
            }
        }
        let name = match engine_key_name(&keystroke.key) {
            Some(name) => name,
            None => {
                // No named key: it is a character to type.
                //
                // THE CHARACTER IS NOT WRITTEN HERE. The platform's text input
                // is the one writer: `replace_text_in_range` carries the
                // character the user meant — an IME's committed text, a dead
                // key's composition, a paste — where the key event carries
                // only the key that was pressed.
                //
                // Writing from both would type every character twice, because
                // GPUI delivers the key event and the text input for the same
                // keystroke: the key event first on a plain keyboard, and the
                // text input first under an IME. One writer, one character.
                //
                // So the key is left alone here, and it keeps travelling: the
                // character arrives as the input event, which the platform
                // only sends when the key event was not consumed.
                cx.propagate();
                return;
            }
        };
        if self.is_editing_text() {
            // How far the key moves or deletes is the engine's own rule, read
            // for the platform this is running on: macOS moves by word with
            // `⌥` and to the line's ends with `⌘`, Windows and Linux move by
            // word with `Ctrl`.
            let (word, line) = text_motion(name, m);
            let _ = textedit::key(&mut self.session.borrow_mut(), name, m.shift, word, line);
            cx.notify();
            return;
        }
        let mut session = self.session.borrow_mut();
        let used = session
            .tool_key(name, m.shift, m.alt, m.cmd)
            .unwrap_or(false);
        drop(session);
        if used {
            cx.notify();
        } else {
            // Not the engine's key. Let it travel to the shell, which answers the
            // editor-wide chords. Without this the canvas is a dead end for
            // every key it does not itself use.
            cx.propagate();
        }
    }

    /// Whether the engine is editing text right now.
    pub fn is_editing_text(&self) -> bool {
        self.session
            .borrow()
            .ds()
            .ok()
            .is_some_and(|ds| ds.text_edit.is_some())
    }

    /// Undo the text being edited: the text session's own step, and nothing else.
    ///
    /// The engine's text editing has no history of its own. A whole editing
    /// session is ONE undo step — figcraft's rule, and the rule the corpus in
    /// `docs/research/interaction/text-editing.md` settled — so the only way
    /// to undo the text is to end the session, which commits that one step,
    /// and then undo the step.
    ///
    /// The order matters, and it is figcraft's order (`textedit::end` and then
    /// `edit.undo`): the engine's own undo DROPS an open interaction without
    /// committing it, so undoing with the session still open would throw the
    /// typed text away *and* undo the step before it.
    ///
    /// A session that committed nothing has nothing to undo, and the
    /// document's own history is not reachable from here: while a text session
    /// is open, the text session owns undo.
    ///
    /// Whether anything was undone.
    fn undo_text(&mut self, cx: &mut gpui::Context<Self>) -> bool {
        let mut session = self.session.borrow_mut();
        let before = session.ds().map(|ds| ds.history.undo.len()).unwrap_or(0);
        let _ = textedit::end(&mut session);
        let after = session.ds().map(|ds| ds.history.undo.len()).unwrap_or(0);
        let undone = after > before;
        if undone {
            if let Err(error) = session.execute("edit.undo", &serde_json::json!({})) {
                eprintln!("spool_text_undo_failed error={error}");
            }
        }
        drop(session);
        cx.notify();
        undone
    }

    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        // A TEXT FIELD OWNS THE KEYBOARD, so the canvas does not answer keys.
        //
        // The canvas is the engine's own key handling, and the engine's keys
        // are gestures: `z` is the zoom tool, a space is a pan, Enter places
        // a shape. Typed into a field, those are characters. So while a field
        // is open the canvas steps aside and lets the key travel up to the
        // surface that has the field.
        //
        // The key still travels: `cx.propagate()` is what carries it to the
        // shell, which routes it to the field's own handler.
        if self.keyboard_claimed {
            cx.propagate();
            return;
        }
        if !event.is_held {
            match event.keystroke.key.as_str() {
                "space" => {
                    self.state.space_pan = !self.is_editing_text();
                }
                "z" => {
                    self.state.zoom_tool = Some(event.keystroke.modifiers.alt);
                    self.state.zoom_drag = None;
                }
                _ => {}
            }
        }
        // ESCAPE BELONGS TO THE SHELL'S LADDER, not to the engine canvas.
        //
        // The ladder is Spool's documented behaviour -- panel, then inspector,
        // then gesture, then text, then selection, then tool -- and it reads the
        // engine's state to decide which rung answers. If the engine canvas
        // ate Escape, the ladder would never run: a key event only reaches the
        // nodes on the path from the focused node up to the root, and a handler
        // that does not propagate stops it there.
        //
        // So Escape is passed up, and the ladder calls back into the engine for
        // the rungs that are the engine's business.
        if event.keystroke.key == "escape" {
            cx.propagate();
            return;
        }
        self.handle_key(&event.keystroke, cx);
    }

    fn on_key_up(
        &mut self,
        event: &KeyUpEvent,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "space" => {
                self.state.space_pan = false;
                self.state.middle_pan = None;
            }
            "z" => {
                self.state.zoom_tool = None;
                self.state.zoom_drag = None;
            }
            _ => return,
        }
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        // A FIELD OWNS THE KEYBOARD, SO THE CANVAS DOES NOT TAKE IT.
        //
        // The canvas takes the focus on every click, which is right while the
        // canvas is the surface the user is on. It is wrong while a text
        // field is open: the field owns the keyboard, and the next click on
        // the canvas does not take it away.
        //
        // The default is prevented as well as the focus skipped: GPUI focuses
        // a focusable element on the mouse-down, and the canvas is one.
        if self.keyboard_claimed {
            window.prevent_default();
        } else {
            self.focus.focus(window, cx);
        }
        let at = f32_point(event.position);
        let m = Mods::of(&event.modifiers);
        match event.button {
            MouseButton::Middle => {
                let view = self.view();
                self.state.middle_pan = Some((at, view.cx, view.cy));
                cx.notify();
            }
            MouseButton::Left => {
                if self.state.zoom_tool.is_some() {
                    self.state.zoom_drag = Some(at);
                    cx.notify();
                    return;
                }
                if event.click_count >= 2 {
                    self.forward(PointerKind::DoubleClick, at, m, None);
                } else {
                    self.state.dragging = true;
                    self.forward(PointerKind::Down, at, m, None);
                }
                cx.notify();
            }
            _ => {}
        }
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let at = f32_point(event.position);
        let m = Mods::of(&event.modifiers);
        // Middle-button or space panning is a UI modifier, not a tool: the engine has no tool for
        // it, and figcraft does it in the UI too.
        if self.state.middle_pan.is_some() || (self.state.space_pan && self.state.dragging) {
            self.pan_to(at, cx);
            return;
        }
        if self.state.dragging || self.busy() {
            self.forward(PointerKind::Move, at, m, None);
            cx.notify();
            return;
        }
        if self.is_editing_text() {
            // While text is being edited the canvas sends no hover: a stray outline would appear
            // under the caret, and the engine's own text tool handles the rest.
            return;
        }
        self.state.hovering = true;
        self.forward(PointerKind::Hover, at, m, None);
        cx.notify();
    }

    fn on_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let at = f32_point(event.position);
        let m = Mods::of(&event.modifiers);
        match event.button {
            MouseButton::Middle => {
                self.state.middle_pan = None;
            }
            MouseButton::Left => {
                if self.state.zoom_drag.take().is_some() {
                    self.run_zoom_to(at, m);
                    cx.notify();
                    return;
                }
                if self.state.dragging || self.busy() {
                    self.state.dragging = false;
                    self.forward(PointerKind::Up, at, m, None);
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    fn on_mouse_exit(
        &mut self,
        _event: &MouseExitEvent,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.state.hovering {
            self.state.hovering = false;
            let _ = self.session.borrow_mut().vector_pointer_left();
            cx.notify();
        }
    }

    /// Pan by moving to `at` from where the current pan started.
    fn pan_to(&mut self, at: GPoint<f32>, cx: &mut gpui::Context<Self>) {
        let Some((start, cx0, cy0)) = self.state.middle_pan else {
            return;
        };
        let zoom = self.view().zoom.max(1e-6);
        let (dx, dy) = (at.x - start.x, at.y - start.y);
        let mut session = self.session.borrow_mut();
        if let Ok(ds) = session.ds_mut() {
            ds.view.cx = cx0 - f64::from(dx) / zoom;
            ds.view.cy = cy0 - f64::from(dy) / zoom;
            ds.revision += 1;
        }
        drop(session);
        cx.notify();
    }

    /// The zoom tool's release: a click steps the zoom about the pointer, a drag zooms to the area.
    fn run_zoom_to(&mut self, at: GPoint<f32>, m: Mods) {
        let Some(start) = self.state.zoom_drag.take() else {
            return;
        };
        let area = corners(start, at);
        let view = self.view();
        let params = if area.size.width > px(4.0) && area.size.height > px(4.0) {
            let a = screen_to_page(&view, self.viewport, start);
            let b = screen_to_page(&view, self.viewport, at);
            serde_json::json!({"x": a.x, "y": a.y, "width": b.x - a.x, "height": b.y - a.y})
        } else {
            let p = screen_to_page(&view, self.viewport, at);
            serde_json::json!({"x": p.x, "y": p.y, "out": m.alt})
        };
        let _ = self.session.borrow_mut().execute("view.zoomAt", &params);
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let m = Mods::of(&event.modifiers);
        let at = f32_point(event.position);
        let (dx, dy) = wheel_pixels(event.delta);
        let mut session = self.session.borrow_mut();
        let Ok(ds) = session.ds_mut() else { return };
        // Cmd/Ctrl + wheel zooms about the pointer; a plain wheel pans. figcraft's rule.
        if m.cmd || m.ctrl {
            if dy != 0.0 {
                let anchor = screen_to_page(&ds.view, self.viewport, at);
                let factor = (1.0 - f64::from(dy) / 400.0).clamp(0.05, 20.0);
                ds.view
                    .set_zoom_at(ds.view.zoom * factor, (anchor.x, anchor.y));
            }
        } else if dx != 0.0 || dy != 0.0 {
            ds.view.scroll(f64::from(dx), f64::from(dy), m.shift);
        } else {
            return;
        }
        ds.revision += 1;
        drop(session);
        cx.notify();
    }

    fn on_pinch(&mut self, event: &PinchEvent, _window: &mut Window, cx: &mut gpui::Context<Self>) {
        let factor = 1.0 + f64::from(event.delta);
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let at = f32_point(event.position);
        let anchor = screen_to_page(&self.view(), self.viewport, at);
        let mut session = self.session.borrow_mut();
        if let Ok(ds) = session.ds_mut() {
            ds.view
                .set_zoom_at(ds.view.zoom * factor, (anchor.x, anchor.y));
            ds.revision += 1;
        }
        drop(session);
        cx.notify();
    }

    // --- painting -----------------------------------------------------------

    /// One frame: measure, ask for pixels, then draw.
    ///
    /// figcraft's draw order, unchanged: page background, the engine's image, then the pixel grid,
    /// layout grids and frame labels, hover outline, mask and frame outlines, drop targets, vector
    /// edit, selection and its handles, the shape builder, text editing, guides, marquee, pen,
    /// brush, lasso, the zoom area, and the rulers. The cursor goes on last, over everything.
    fn paint(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
        bounds: Bounds<Pixels>,
        ratio: f32,
        hitbox: Option<Hitbox>,
    ) {
        // The canvas's own cost, on the UI thread: the snapshot, the overlays
        // and the blit. This is the half of a frame that is not the engine's
        // render, and it is what the pointer waits behind.
        let paint_started = crate::diagnostics::start();
        self.viewport = Viewport::new(bounds);

        // Honour a deferred fit, now that there is a viewport to fit into. The
        // size check is what makes this safe to run on every paint: before the
        // element has been laid out the viewport is empty, and fitting against it
        // is exactly the failure this defers past.
        if self.state.pending_fit && self.viewport.size.0 > 0.0 && self.viewport.size.1 > 0.0 {
            self.state.pending_fit = false;
            let page = self.session.borrow().page().ok();
            if let Some(page) = page {
                if let Ok(ds) = self.session.borrow_mut().ds_mut() {
                    if let Some(bounds) = ds.doc.world_bounds(page) {
                        ds.view.fit(bounds, 4.0);
                    }
                }
            }
        }

        self.request_render(ratio);
        let frame = self.renderer.frame();
        // Which frame this paint is about to draw. A frame that arrives while
        // the canvas is painting is newer than this one, and the canvas asks
        // for another one at the end of the paint rather than waiting for the
        // next input event to bring the picture up to date.
        self.state.drawn = frame.as_ref().map(|f| f.generation).unwrap_or(0);
        let snapshot = self.snapshot();

        let background = snapshot
            .as_ref()
            .and_then(|s| page_background(&s.doc, s.page))
            .unwrap_or(self.palette.canvas);

        let mut ov = Overlays {
            window,
            app: cx,
            viewport: self.viewport,
            view: self.view(),
            pal: self.palette,
        };

        // A gesture owns the pointer until it ends: without this a drag that leaves the canvas
        // would stop receiving moves and the engine would never see the release.
        if self.state.dragging || self.state.middle_pan.is_some() {
            if let Some(h) = hitbox.as_ref() {
                ov.window.capture_pointer(h.id);
            }
        }

        ov.fill(bounds, background);
        if let Some(f) = frame.as_ref() {
            let place = blit_bounds(f, ov.view.origin(), ov.view.zoom, ov.viewport.origin);
            if let Some(image) = f.image.clone() {
                let _ =
                    ov.window
                        .paint_image(bounds, place, Corners::all(px(0.0)), image, 0, false);
            }
        }

        if let Some(s) = snapshot.as_ref() {
            self.paint_overlays(&mut ov, s);
            self.paint_cursor(&mut ov, s, hitbox.as_ref());
        }

        // Ask for another frame while a render is in flight, so a frame finished between two
        // paints is not left sitting in the renderer until something else happens to notify.
        if self.frame_pending() {
            ov.window.refresh();
        }
        crate::diagnostics::record("canvas_paint", paint_started);
        crate::diagnostics::count("canvas_paint", 1);
        self.state.paints += 1;
        // The first paint, so a run with the diagnostics on says something even
        // if the app then sits still, and then every so often after that.
        if self.state.paints == 1 || self.state.paints % DIAGNOSTIC_EVERY_PAINTS == 0 {
            self.report_diagnostics();
        }
    }

    /// What the canvas and the renderer have done, for a diagnostics run.
    ///
    /// The engine's own timings are on the UI thread, in the diagnostics
    /// module; the renderer's are counters the render thread keeps, because
    /// that thread has no thread-local store to keep them in. Both are printed
    /// here, when the diagnostics are on.
    fn report_diagnostics(&self) {
        if !crate::diagnostics::enabled() {
            return;
        }
        crate::diagnostics::report("canvas");
        eprintln!(
            "spool_diagnostics label=\"canvas\" kind=\"renderer\" {}",
            self.renderer.diagnostics()
        );
    }

    /// Everything the painter needs from the session, taken once and released.
    ///
    /// Held for the paint only: the engine is not borrowed while GPUI draws, so a panel may act on
    /// it from an event handler in the same frame without deadlocking.
    fn snapshot(&mut self) -> Option<Snapshot> {
        let mut snapshot = {
            let session = self.session.borrow();
            let ds = session.ds().ok()?;
            Snapshot {
                doc: ds.doc.clone(),
                page: ds.page,
                selection: ds.selection.clone(),
                hover: ds.hover,
                text_edit: ds.text_edit.clone(),
                vector_edit: ds.vector_edit.clone(),
                view: ds.view.clone(),
                focus: ds.focused(),
                tool: session.tool.tool,
                overlay: OverlayView::from_overlay(&session.tool.overlay),
                show_dimensions: session.prefs.show_dimensions,
                highlight_hover: session.prefs.highlight_on_hover,
                scale_anchor: session.tool.scale_anchor,
                arc_handles: session.arc_handles(),
                shape_handles: session.shape_handles(),
                builder: figcraft_engine::shapebuilder::current(&session),
                edit_box: session.vector_edit_box(),
                origin: ds.origin_handle(),
                composition: None,
            }
        };
        snapshot.composition =
            Composition::build(&self.session.borrow(), self.state.composition.as_ref());
        self.state.composition = snapshot.composition.clone();
        // Frame labels are hit-tested by the engine, so what was drawn last frame is what it uses.
        let labels = std::mem::take(&mut self.state.labels);
        self.session.borrow_mut().tool.labels = labels;
        Some(snapshot)
    }

    fn paint_overlays(&mut self, ov: &mut Overlays<'_>, s: &Snapshot) {
        let doc = &s.doc;
        let view = &s.view;
        let pal = self.palette;

        self.paint_pixel_grid(ov, s);

        // The Dev Mode callouts, when the Dev Mode tab is open.
        if self.dev_mode {
            let viewport = ov.viewport;
            crate::engine::panel_devmode::paint_dev_mode_overlay(
                &mut *ov.window,
                &mut *ov.app,
                view,
                viewport,
                doc,
                s.page,
                &s.selection,
                &pal,
                &self.dev_filter,
            );
        }

        // The prototype's connections, when the Prototype tab is open.
        //
        // The viewport is read first: `ov.window` is borrowed mutably by the
        // call, so the viewport has to be copied out before it.
        if self.prototype_mode {
            let viewport = ov.viewport;
            crate::engine::panel_prototyping::paint_proto_connections(
                ov.window,
                view,
                viewport,
                doc,
                s.page,
                &s.selection,
                &pal,
            );
        }

        // Layout grids and frame labels for top-level frames.
        for c in boards(doc, s.page) {
            let focused = match s.focus {
                Some(f) => f != c,
                None => false,
            };
            if focused {
                continue;
            }
            let Some(n) = doc.node(c) else { continue };
            let Some(b) = doc.world_bounds(c) else {
                continue;
            };
            // An off-screen frame draws neither grids nor a label (the label sits just outside it).
            let on_screen = corners(
                page_to_screen(view, ov.viewport, b.x0, b.y0),
                page_to_screen(view, ov.viewport, b.x1, b.y1),
            )
            .dilate(px(20.0));
            if !on_screen.intersects(&ov.viewport.bounds()) {
                continue;
            }
            if view.layout_grids && !n.grids.is_empty() && figcraft_layout::grids_shown(doc, c) {
                self.paint_layout_grids(ov, doc, c);
            }
            let labelled = s.focus.is_none()
                && (n.kind.is_frame_like() || matches!(n.kind, NodeKind::Section { .. }));
            if labelled {
                self.paint_frame_label(ov, doc, c, s);
            }
        }

        // Hover outline (measuring outlines the measured box instead).
        if let Some(h) = s.hover {
            let show = !s.selection.contains(&h)
                && matches!(s.tool, ToolId::Move | ToolId::Scale)
                && s.highlight_hover
                && s.overlay.measure_box.is_none();
            if show {
                let col = self.outline_colour(h);
                self.paint_node_outline(ov, doc, h, LINE_PX + 0.5, col);
            }
        }

        // Mask, frame and slice outlines, as the View menu shows them.
        if view.mask_outlines || view.frame_outlines || view.show_slices {
            for id in doc.subtree(s.page) {
                let Some(n) = doc.node(id).filter(|n| n.visible) else {
                    continue;
                };
                if view.mask_outlines && n.is_mask {
                    self.paint_node_outline(ov, doc, id, LINE_PX, pal.accent);
                } else if view.frame_outlines && n.kind.is_frame_like() {
                    self.paint_node_outline(ov, doc, id, LINE_PX, pal.ink_soft);
                } else if view.show_slices && matches!(n.kind, NodeKind::Slice) {
                    if let Some(q) = node_quad(doc, id) {
                        ov.quad(q, LINE_PX, pal.ink_soft);
                    }
                }
            }
        }

        self.paint_drop_targets(ov, doc, s);
        self.paint_vector_edit(ov, doc, s);
        self.paint_selection(ov, doc, s);
        self.paint_shape_builder(ov, s);
        if self.paint_text_edit(ov, s) {
            // A blinking caret needs frames to blink in.
            ov.window.refresh();
        }
        self.paint_guides(ov, s);
        self.paint_pen(ov, s);
        self.paint_lasso(ov, s);
        self.paint_zoom_area(ov);

        if view.rulers {
            self.paint_rulers(ov, s);
        }
    }

    fn paint_layout_grids(&self, ov: &mut Overlays<'_>, doc: &Document, frame: NodeId) {
        let Some(n) = doc.node(frame) else { return };
        let world = doc.world_transform(frame);
        for g in n.grids.iter().filter(|g| g.visible) {
            let col = engine_color(g.color);
            for r in figcraft_layout::grid_lines(g, n.width, n.height) {
                let a = world * Point::new(r.x0, r.y0);
                let b = world * Point::new(r.x1, r.y1);
                if g.kind == figcraft_doc::GridKind::Grid {
                    ov.seg([a.x, a.y], [b.x, b.y], LINE_PX, col);
                } else {
                    let bounds = ov.rect(a.x, a.y, b.x, b.y);
                    ov.fill(bounds, col);
                }
            }
        }
    }

    fn paint_drop_targets(&self, ov: &mut Overlays<'_>, doc: &Document, s: &Snapshot) {
        let pal = self.palette;
        if let Some(d) = s.overlay.drop_target {
            if let Some(q) = node_quad(doc, d) {
                let col = if figcraft_engine::components::is_slot(doc, d) {
                    pal.component
                } else {
                    pal.accent
                };
                ov.quad(q, LINE_PX, col);
            }
        }
        if let Some(t) = s.overlay.swap_target {
            if let Some(q) = node_quad(doc, t) {
                ov.quad(q, 2.0, pal.component);
                let at = page_to_screen(&s.view, ov.viewport, q[0].x, q[0].y);
                ov.pill(
                    "Swap instance",
                    point(at.x, at.y - 20.0),
                    pal.component,
                    pal.white,
                );
            }
        }
        if let Some([x0, y0, x1, y1]) = s.overlay.insert_line {
            ov.seg([x0, y0], [x1, y1], 2.0, pal.accent);
        }
        if let Some([x0, y0, x1, y1]) = s.overlay.drop_cell {
            let r = ov.rect(x0, y0, x1, y1);
            ov.fill_radii(r, 2.0, Palette::fade(pal.accent, 0.12));
            let pts = [
                r.origin,
                r.top_right(),
                r.bottom_right(),
                r.bottom_left(),
                r.origin,
            ];
            let float: Vec<GPoint<f32>> = pts
                .iter()
                .map(|p| point(f32::from(p.x), f32::from(p.y)))
                .collect();
            ov.dashed_poly(&float, 1.5, pal.accent);
        }
    }

    fn paint_vector_edit(&self, ov: &mut Overlays<'_>, doc: &Document, s: &Snapshot) {
        let Some(ve) = s.vector_edit.as_ref() else {
            return;
        };
        // The Variable width tool shows width points in place of the path's; its own overlay is
        // not ported, so it draws the path's points like every other vector edit.
        let points = ve.tool != figcraft_engine::vectoredit::VTool::Width;
        // Other layers edited at once go under the active one.
        for part in &ve.others {
            if let Some(po) = figcraft_engine::vectoredit::overlay(doc, &ve.view(part)) {
                self.paint_vector_overlay(ov, &po, points, None, false, s);
            }
        }
        let Some(o) = figcraft_engine::vectoredit::overlay(doc, ve) else {
            return;
        };
        self.paint_vector_overlay(ov, &o, points, s.overlay.vector_hover, true, s);
        if let Some(c) = s.edit_box {
            let label = size_label((c[1] - c[0]).hypot(), (c[3] - c[0]).hypot(), false);
            self.transform_box(
                ov,
                c,
                self.palette.accent,
                true,
                4,
                Some(label),
                s.show_dimensions,
            );
        }
    }

    fn paint_selection(&self, ov: &mut Overlays<'_>, doc: &Document, s: &Snapshot) {
        if s.selection.is_empty() || s.text_edit.is_some() || s.vector_edit.is_some() {
            return;
        }
        let pal = self.palette;
        let zoom = s.view.zoom;
        let col = if s
            .selection
            .iter()
            .any(|i| self.outline_colour(*i) == pal.component)
        {
            pal.component
        } else {
            pal.accent
        };
        for id in &s.selection {
            self.paint_node_outline(ov, doc, *id, LINE_PX, col);
        }
        if s.tool == ToolId::ShapeBuilder {
            // The shape builder reshapes by region; there is no transform box.
            return;
        }
        let Some(b) = figcraft_engine::tools::selection_box(doc, &s.selection) else {
            return;
        };
        let Some((_, w, h)) = figcraft_engine::tools::selection_frame(doc, &s.selection) else {
            return;
        };
        let is_line = s.selection.len() == 1
            && s.selection
                .first()
                .and_then(|i| doc.node(*i))
                .is_some_and(|n| matches!(n.kind, NodeKind::Line));
        let mut label = size_label(w, h, is_line);
        if let Some(hint) = s.overlay.scale {
            label = format!("{label} · {}x", fmt(hint.factor.abs()));
        }
        // A layer smaller than its handles on screen shows just its outline (the handles still
        // answer just outside it).
        let tiny =
            figcraft_engine::tools::box_sides_px(&b, zoom).1 < figcraft_engine::tools::SMALL_BOX_PX;
        let locked = s
            .selection
            .iter()
            .any(|i| figcraft_engine::tools::locked_in(doc, *i));
        let handles = if locked {
            0
        } else if is_line {
            2
        } else if tiny {
            0
        } else {
            4
        };
        self.transform_box(
            ov,
            b,
            col,
            s.selection.len() > 1,
            handles,
            Some(label),
            s.show_dimensions,
        );
        self.paint_arc_handles(ov, s, col);
        self.paint_shape_handles(ov, s, col);
        // The Scale tool's pivot: the point that stays put.
        if s.tool == ToolId::Scale {
            let pivot = s
                .overlay
                .scale
                .map(|h| Point::new(h.pivot[0], h.pivot[1]))
                .or_else(|| {
                    figcraft_engine::tools::scale_anchor_point(doc, &s.selection, s.scale_anchor)
                });
            if let Some(p) = pivot {
                let c = page_to_screen(&s.view, ov.viewport, p.x, p.y);
                ov.dot(c, 4.0, pal.white, Some(col));
                ov.dot(c, 1.5, col, None);
            }
        }
        // The rotation origin while it shows: a ring with a cross through it.
        if let Some(o) = s.origin {
            let c = page_to_screen(&s.view, ov.viewport, o.x, o.y);
            ov.dot(c, 5.0, pal.white, Some(col));
            for d in [point(1.0, 0.0), point(0.0, 1.0)] {
                ov.line(
                    point(c.x - d.x * 10.0, c.y - d.y * 10.0),
                    point(c.x - d.x * 5.0, c.y - d.y * 5.0),
                    1.5,
                    col,
                );
                ov.line(
                    point(c.x + d.x * 5.0, c.y + d.y * 5.0),
                    point(c.x + d.x * 10.0, c.y + d.y * 10.0),
                    1.5,
                    col,
                );
            }
            ov.dot(c, 1.5, col, None);
        }
    }

    fn paint_shape_builder(&self, ov: &mut Overlays<'_>, s: &Snapshot) {
        if s.tool != ToolId::ShapeBuilder {
            return;
        }
        let Some(arr) = s.builder.as_ref() else {
            return;
        };
        let pal = self.palette;
        for r in &arr.regions {
            for poly in &r.polys {
                let pts: Vec<GPoint<f32>> = poly.iter().map(|p| ov.at(p.x, p.y)).collect();
                // Edges read on any fill: a light halo under a thin accent line.
                ov.stroke_poly(&pts, 2.5, Palette::fade(pal.white, 0.6), true);
                ov.stroke_poly(&pts, LINE_PX, pal.accent, true);
            }
        }
        let col = if s.overlay.builder_remove {
            pal.danger
        } else {
            pal.accent
        };
        for r in s
            .overlay
            .builder_regions
            .iter()
            .filter_map(|i| arr.regions.get(*i))
        {
            let polys: Vec<Vec<GPoint<f32>>> = r
                .polys
                .iter()
                .map(|poly| poly.iter().map(|p| ov.at(p.x, p.y)).collect())
                .collect();
            self.paint_hatched(ov, &polys, col);
        }
    }

    /// Guides, the measured box, then the marquee.
    fn paint_guides(&self, ov: &mut Overlays<'_>, s: &Snapshot) {
        let pal = self.palette;
        for g in &s.overlay.guides {
            let color = if g.label.is_some() {
                pal.ink
            } else {
                pal.guide
            };
            if g.dashed {
                ov.dashed_seg([g.x0, g.y0], [g.x1, g.y1], LINE_PX, color);
            } else {
                ov.seg([g.x0, g.y0], [g.x1, g.y1], LINE_PX, color);
            }
            if let Some(label) = g.label.as_ref() {
                let at = page_to_screen(&s.view, ov.viewport, g.x0, g.y0);
                ov.pill(label, point(at.x + 6.0, at.y - 16.0), pal.guide, pal.white);
            }
        }
        if let Some([x0, y0, x1, y1]) = s.overlay.measure_box {
            ov.quad(
                [
                    Point::new(x0, y0),
                    Point::new(x1, y0),
                    Point::new(x1, y1),
                    Point::new(x0, y1),
                ],
                LINE_PX + 0.5,
                pal.danger,
            );
        }
        if let Some([x0, y0, x1, y1]) = s.overlay.marquee {
            let r = ov.rect(x0, y0, x1, y1);
            ov.fill(r, Palette::fade(pal.accent, 0.10));
            ov.stroke_box(r, LINE_PX, pal.accent);
        }
    }

    fn paint_pen(&self, ov: &mut Overlays<'_>, s: &Snapshot) {
        let pal = self.palette;
        if !s.overlay.pen.is_empty() {
            let pts: Vec<GPoint<f32>> = s.overlay.pen.iter().map(|p| ov.at2(*p)).collect();
            if s.tool == ToolId::Pencil {
                ov.stroke_poly(&pts, 2.0, pal.ink, false);
            } else {
                for [x0, y0, x1, y1] in &s.overlay.pen_handles {
                    self.paint_bezier_handle(
                        ov,
                        ov.at2([*x0, *y0]),
                        ov.at2([*x1, *y1]),
                        DOT_PX,
                        pal.white,
                    );
                }
                for p in &pts {
                    ov.dot(*p, DOT_PX, pal.white, Some(pal.accent));
                }
                if let Some([x0, y0, x1, y1]) = s.overlay.rubber {
                    let (a, b) = (ov.at2([x0, y0]), ov.at2([x1, y1]));
                    // The segment a click would make: along the last anchor's out handle, if any.
                    match s.overlay.rubber_handle {
                        Some([hx, hy]) => {
                            let h = ov.at2([hx, hy]);
                            let mut path = PathBuilder::stroke(px(LINE_PX));
                            path.move_to(px_point(a));
                            path.cubic_bezier_to(px_point(b), px_point(h), px_point(b));
                            if let Ok(p) = path.build() {
                                ov.window.paint_path(p, pal.accent);
                            }
                        }
                        None => ov.line(a, b, LINE_PX, pal.accent),
                    }
                }
            }
        }
        // The eraser brush (vector edit mode).
        if let Some([x, y, r]) = s.overlay.brush {
            let c = page_to_screen(&s.view, ov.viewport, x, y);
            let rs = (r * s.view.zoom) as f32;
            ov.dot(c, rs + BRUSH_HALO_PX, pal.white, None);
            ov.dot(c, rs, pal.none, Some(pal.accent));
        }
    }

    fn paint_lasso(&self, ov: &mut Overlays<'_>, s: &Snapshot) {
        if s.overlay.lasso.len() <= 1 {
            return;
        }
        let mut pts: Vec<GPoint<f32>> = s.overlay.lasso.iter().map(|p| ov.at2(*p)).collect();
        if let Some(first) = pts.first().copied() {
            pts.push(first);
        }
        ov.dashed_poly(&pts, LINE_PX, self.palette.accent);
    }

    fn paint_zoom_area(&self, ov: &mut Overlays<'_>) {
        let (Some(start), Some(at)) = (self.state.zoom_drag, self.pointer_on_canvas()) else {
            return;
        };
        let area = corners(start, at);
        if area.size.width <= px(4.0) && area.size.height <= px(4.0) {
            return;
        }
        let pal = self.palette;
        ov.fill(area, Palette::fade(pal.accent, 0.10));
        ov.stroke_box(area, LINE_PX, pal.accent);
    }

    fn paint_pixel_grid(&self, ov: &mut Overlays<'_>, s: &Snapshot) {
        if !s.view.pixel_grid_shown() {
            return;
        }
        let view = &s.view;
        let col = Palette::fade(self.palette.ink, 0.10);
        let top_left = ov.viewport.origin;
        let bottom_right = point(
            top_left.x + ov.viewport.size.0,
            top_left.y + ov.viewport.size.1,
        );
        let p0 = page_point(view, ov.viewport, top_left);
        let p1 = page_point(view, ov.viewport, bottom_right);
        let mut x = p0.x.floor();
        let mut guard = 0;
        while x <= p1.x && guard < MAX_GRID_LINES {
            let at = page_to_screen(view, ov.viewport, x, 0.0);
            ov.line(
                point(at.x, top_left.y),
                point(at.x, bottom_right.y),
                LINE_PX,
                col,
            );
            x += 1.0;
            guard += 1;
        }
        let mut y = p0.y.floor();
        let mut guard = 0;
        while y <= p1.y && guard < MAX_GRID_LINES {
            let at = page_to_screen(view, ov.viewport, 0.0, y);
            ov.line(
                point(top_left.x, at.y),
                point(bottom_right.x, at.y),
                LINE_PX,
                col,
            );
            y += 1.0;
            guard += 1;
        }
    }

    /// The transform box: its outline, `handles` corner squares, and the size label under it when
    /// dimensions are on.
    fn transform_box(
        &self,
        ov: &mut Overlays<'_>,
        q: [Point; 4],
        col: Hsla,
        outline: bool,
        handles: usize,
        label: Option<String>,
        show_dimensions: bool,
    ) {
        if outline {
            ov.quad(q, LINE_PX, col);
        }
        for corner in q.iter().take(handles) {
            let at = ov.at(corner.x, corner.y);
            ov.corner_handle(at, col);
        }
        let Some(label) = label.filter(|_| show_dimensions) else {
            return;
        };
        let bottom = q.iter().map(|p| p.y).fold(f64::MIN, f64::max);
        let cx = (q.iter().map(|p| p.x).sum::<f64>()) / 4.0;
        let at = page_to_screen(&ov.view, ov.viewport, cx, bottom);
        ov.pill(&label, point(at.x, at.y + 4.0), col, self.palette.white);
    }

    fn paint_arc_handles(&self, ov: &mut Overlays<'_>, s: &Snapshot, col: Hsla) {
        let Some(hs) = s.arc_handles.as_ref() else {
            return;
        };
        for h in [ArcHandle::Ratio, ArcHandle::Start, ArcHandle::Sweep] {
            let Some([x, y]) = hs.at(h) else { continue };
            let at = page_to_screen(&s.view, ov.viewport, x, y);
            let r = if s.overlay.arc_hover == Some(h) {
                5.0
            } else {
                4.0
            };
            ov.dot(at, r, self.palette.white, Some(col));
            if h == ArcHandle::Start {
                ov.dot(at, 1.5, col, None);
            }
        }
        if let Some(hint) = &s.overlay.arc {
            let at = page_to_screen(&s.view, ov.viewport, hint.at[0], hint.at[1]);
            ov.pill(
                &hint.label,
                point(at.x + 12.0, at.y + 12.0),
                col,
                self.palette.white,
            );
        }
    }

    fn paint_shape_handles(&self, ov: &mut Overlays<'_>, s: &Snapshot, col: Hsla) {
        for (h, [x, y]) in &s.shape_handles {
            let r = if s.overlay.shape_hover == Some(*h) {
                4.5
            } else {
                3.5
            };
            let at = page_to_screen(&s.view, ov.viewport, *x, *y);
            ov.dot(at, r, self.palette.white, Some(col));
        }
        if let Some(hint) = &s.overlay.shape {
            let at = page_to_screen(&s.view, ov.viewport, hint.at[0], hint.at[1]);
            let text = match hint.handle {
                ShapeHandle::Radius { .. } => format!("Radius {}", fmt(hint.value)),
                ShapeHandle::Ratio => format!("Ratio {}%", fmt(hint.value)),
            };
            ov.pill(
                &text,
                point(at.x + 12.0, at.y + 12.0),
                col,
                self.palette.white,
            );
        }
    }

    /// A layer's outline: its shape for ellipses, vectors, polygons, stars and boolean groups, its
    /// box otherwise.
    fn paint_node_outline(
        &self,
        ov: &mut Overlays<'_>,
        doc: &Document,
        id: NodeId,
        width: f32,
        color: Hsla,
    ) {
        match self.outline_polylines(ov, doc, id) {
            Some(lines) => {
                for l in lines {
                    ov.stroke_poly(&l, width, color, false);
                }
            }
            None => {
                if let Some(q) = node_quad(doc, id) {
                    ov.quad(q, width, color);
                }
            }
        }
    }

    /// The outline of a shape layer, flattened and projected, or `None` for a box-shaped one.
    fn outline_polylines(
        &self,
        ov: &Overlays<'_>,
        doc: &Document,
        id: NodeId,
    ) -> Option<Vec<Vec<GPoint<f32>>>> {
        let n = doc.node(id)?;
        if !matches!(
            n.kind,
            NodeKind::Ellipse { .. }
                | NodeKind::Vector { .. }
                | NodeKind::Polygon { .. }
                | NodeKind::Star { .. }
                | NodeKind::BooleanOp { .. }
        ) {
            return None;
        }
        let (path, _) = figcraft_render::node_outline(doc, n)?;
        let world = doc.world_transform(id);
        let tol = 0.5 / ov.view.zoom.max(1e-9);
        let mut out = Vec::new();
        let mut cur: Vec<GPoint<f32>> = Vec::new();
        figcraft_geom::kurbo::flatten(path.iter(), tol, |el| {
            use figcraft_geom::kurbo::PathEl::*;
            match el {
                MoveTo(p) => {
                    if cur.len() > 1 {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                    let q = world * p;
                    cur.push(ov.at(q.x, q.y));
                }
                LineTo(p) => {
                    let q = world * p;
                    cur.push(ov.at(q.x, q.y));
                }
                ClosePath => {
                    if let Some(f) = cur.first().copied() {
                        cur.push(f);
                    }
                }
                _ => {}
            }
        });
        if cur.len() > 1 {
            out.push(cur);
        }
        Some(out)
    }

    /// A bezier handle from its anchor `v` to its end `e`.
    fn paint_bezier_handle(
        &self,
        ov: &mut Overlays<'_>,
        v: GPoint<f32>,
        e: GPoint<f32>,
        r: f32,
        fill: Hsla,
    ) {
        ov.line(v, e, LINE_PX, self.palette.accent);
        ov.dot(e, r, fill, Some(self.palette.accent));
    }

    /// Vector edit mode: a layer's segments, handles and points, with its hovered part, and for the
    /// `active` layer the closed area under the paint bucket.
    fn paint_vector_overlay(
        &self,
        ov: &mut Overlays<'_>,
        o: &figcraft_engine::vectoredit::EditOverlay,
        points: bool,
        hover: Option<VHit>,
        active: bool,
        s: &Snapshot,
    ) {
        let pal = self.palette;
        // The paint bucket target, hatched like a fill to come.
        if active && !s.overlay.face.is_empty() {
            let polys: Vec<Vec<GPoint<f32>>> = s
                .overlay
                .face
                .iter()
                .map(|poly| poly.iter().map(|p| ov.at2(*p)).collect())
                .collect();
            self.paint_hatched(ov, &polys, pal.accent);
        }
        let hover_seg = match hover {
            Some(VHit::Segment { seg, .. }) => Some(seg),
            _ => None,
        };
        for (i, seg) in o.segments.iter().enumerate() {
            let w = if o.selected_segments.contains(&i) {
                3.0
            } else if hover_seg == Some(i) {
                2.5
            } else {
                1.5
            };
            ov.cubic(*seg, w, pal.accent);
        }
        if !points {
            return;
        }
        for h in &o.handles {
            let hovered = hover
                == Some(VHit::Handle {
                    seg: h.seg,
                    start: h.start,
                });
            let on = o.selected_handles.contains(&(h.seg, h.start));
            let v = ov.at2(h.vertex);
            let e = ov.at2(h.handle);
            self.paint_bezier_handle(
                ov,
                v,
                e,
                if hovered { 4.5 } else { DOT_PX },
                if on { pal.accent } else { pal.white },
            );
        }
        for (i, v) in o.vertices.iter().enumerate() {
            let at = ov.at2(*v);
            let hovered = matches!(hover, Some(VHit::Vertex { index }) if index == i);
            let side = if hovered { 9.0 } else { 7.0 };
            let b = Bounds::new(
                point(px(at.x - side / 2.0), px(at.y - side / 2.0)),
                size(px(side), px(side)),
            );
            ov.fill_radii(
                b,
                5.0,
                if o.selected_vertices.contains(&i) {
                    pal.accent
                } else {
                    pal.white
                },
            );
            ov.stroke_box(b, LINE_PX, pal.accent);
            if o.anchor == Some(i) {
                ov.dot(at, 7.5, pal.none, Some(pal.accent));
            }
        }
    }

    /// Polygons hatched and outlined in `col`, each line on a light halo so it reads on any fill.
    fn paint_hatched(&self, ov: &mut Overlays<'_>, polys: &[Vec<GPoint<f32>>], col: Hsla) {
        let halo = Palette::fade(self.palette.white, 0.78);
        for l in hatch_lines(polys, HATCH_GAP) {
            ov.line(l[0], l[1], 3.0, halo);
            ov.line(l[0], l[1], 1.25, col);
        }
        for poly in polys {
            ov.stroke_poly(poly, 4.0, halo, true);
            ov.stroke_poly(poly, 2.0, col, true);
        }
    }

    /// A top-level frame's name, above its top-left corner, recorded so the engine hit-tests it.
    fn paint_frame_label(
        &mut self,
        ov: &mut Overlays<'_>,
        doc: &Document,
        id: NodeId,
        s: &Snapshot,
    ) {
        let Some(n) = doc.node(id) else { return };
        let name = if n.name.is_empty() {
            n.kind.label().to_owned()
        } else {
            n.name.clone()
        };
        let Some(q) = node_quad(doc, id) else { return };
        let height = (LABEL_PT * LABEL_LINE + 4.0) as f64;
        let at = page_to_screen(&s.view, ov.viewport, q[0].x, q[0].y);
        let (w, _) = ov.pill(
            &name,
            point(at.x, at.y - height as f32),
            self.palette.accent,
            self.palette.white,
        );
        // The label's quad on the page, turned with its frame: the engine treats a press or a hover
        // on it as one on the frame, which is the whole point of drawing it above the box.
        let world = doc.world_transform(id);
        let width = f64::from(w as i32);
        let quad = [
            world * Point::new(0.0, -height),
            world * Point::new(width, -height),
            world * Point::new(width, 0.0),
            world * Point::new(0.0, 0.0),
        ];
        self.state.labels.push((id, quad));
        // The Dev Mode status badge, beside the frame's name.
        crate::engine::panel_devmode::paint_dev_status_badge(
            &mut *ov.window,
            &mut *ov.app,
            n,
            GPoint {
                x: at.x + w + 6.0,
                y: at.y - height as f32 + LABEL_PT * LABEL_LINE / 2.0,
            },
        );
    }

    /// Text editing: the box, the selection, the composition underline and the caret.
    ///
    /// Returns whether a caret was drawn, so the caller can ask for a frame to blink in.
    fn paint_text_edit(&self, ov: &mut Overlays<'_>, s: &Snapshot) -> bool {
        let Some(te) = s.text_edit.as_ref() else {
            return false;
        };
        let pal = self.palette;
        let composition = s.composition.as_ref();
        let doc: &Document = match composition {
            Some(c) => &c.doc,
            None => &s.doc,
        };
        let Some(text) = TextOnScreen::of(doc, te.id) else {
            return false;
        };
        if let Some(q) = node_quad(doc, te.id) {
            ov.quad(q, LINE_PX, pal.accent);
        }
        let (a, b) = (te.caret.min(te.anchor), te.caret.max(te.anchor));
        if a != b && composition.is_none() {
            for q in text.selection(&s.view, ov.viewport, a, b) {
                ov.fill_poly(&q, Palette::fade(pal.accent, 0.30));
            }
        }
        if let Some(c) = composition {
            for r in text.layout.selection_rects(c.range.0, c.range.1) {
                // Underline just below the baseline, across the composed chars only.
                let Some(line) = text
                    .layout
                    .lines
                    .iter()
                    .find(|l| (l.top - r.y0).abs() < 1e-6)
                else {
                    continue;
                };
                let x1 = text
                    .layout
                    .glyphs
                    .get(line.glyph_start..line.glyph_end)
                    .unwrap_or(&[])
                    .iter()
                    .filter(|g| {
                        g.char_end > g.char_start
                            && g.char_start >= c.range.0
                            && g.char_end <= c.range.1
                    })
                    .map(|g| g.x + g.advance)
                    .fold(r.x0, f64::max);
                let y = line.baseline + (line.top + line.height - line.baseline) * 0.35;
                let from = text.at(&s.view, ov.viewport, r.x0, y);
                let to = text.at(&s.view, ov.viewport, x1, y);
                ov.line(from, to, LINE_PX, pal.ink);
            }
        }
        let caret = composition.map(|c| c.cursor).unwrap_or(te.caret);
        let [top, bottom] = text.caret(&s.view, ov.viewport, caret, s.view.zoom);
        let phase = self.born.elapsed().as_millis() / CARET_BLINK_MS;
        if phase % 2 == 0 || a != b {
            ov.line(top, bottom, LINE_PX, pal.ink);
            return true;
        }
        false
    }

    fn paint_rulers(&self, ov: &mut Overlays<'_>, s: &Snapshot) {
        let pal = self.palette;
        let origin = ov.viewport.origin;
        ov.fill(
            Bounds::new(
                point(px(origin.x), px(origin.y)),
                size(px(ov.viewport.size.0), px(RULER_PX)),
            ),
            pal.panel,
        );
        ov.fill(
            Bounds::new(
                point(px(origin.x), px(origin.y)),
                size(px(RULER_PX), px(ov.viewport.size.1)),
            ),
            pal.panel,
        );
        let step = figcraft_engine::guides::ruler_step(s.view.zoom);
        let p0 = page_point(&s.view, ov.viewport, origin);
        let p1 = page_point(
            &s.view,
            ov.viewport,
            point(origin.x + ov.viewport.size.0, origin.y + ov.viewport.size.1),
        );
        // Highlight the selection's span.
        if let Some(b) = s.doc.bounds_of(&s.selection) {
            let a = page_to_screen(&s.view, ov.viewport, b.x0, b.y0);
            let c = page_to_screen(&s.view, ov.viewport, b.x1, b.y1);
            let wash = Palette::fade(pal.accent, 0.15);
            ov.fill(
                Bounds::new(
                    point(px(a.x), px(origin.y)),
                    size(px((c.x - a.x).max(0.0)), px(RULER_PX)),
                ),
                wash,
            );
            ov.fill(
                Bounds::new(
                    point(px(origin.x), px(a.y)),
                    size(px(RULER_PX), px((c.y - a.y).max(0.0))),
                ),
                wash,
            );
        }
        let mut x = (p0.x / step).floor() * step;
        let mut guard = 0;
        while x <= p1.x && guard < MAX_GRID_LINES {
            let at = page_to_screen(&s.view, ov.viewport, x, 0.0);
            ov.line(
                point(at.x, origin.y + RULER_PX - 6.0),
                point(at.x, origin.y + RULER_PX),
                LINE_PX,
                pal.ink_soft,
            );
            ov.label(&fmt(x), point(at.x + 2.0, origin.y + 3.0), pal.ink_soft);
            x += step;
            guard += 1;
        }
        let mut y = (p0.y / step).floor() * step;
        let mut guard = 0;
        while y <= p1.y && guard < MAX_GRID_LINES {
            let at = page_to_screen(&s.view, ov.viewport, 0.0, y);
            ov.line(
                point(origin.x + RULER_PX - 6.0, at.y),
                point(origin.x + RULER_PX, at.y),
                LINE_PX,
                pal.ink_soft,
            );
            ov.label(&fmt(y), point(origin.x + 3.0, at.y + 2.0), pal.ink_soft);
            y += step;
            guard += 1;
        }
        ov.line(
            point(origin.x, origin.y + RULER_PX),
            point(origin.x + ov.viewport.size.0, origin.y + RULER_PX),
            LINE_PX,
            pal.ink_soft,
        );
        ov.line(
            point(origin.x + RULER_PX, origin.y),
            point(origin.x + RULER_PX, origin.y + ov.viewport.size.1),
            LINE_PX,
            pal.ink_soft,
        );
    }

    /// The pointer cursor: the tool's, unless something on the canvas has its own.
    fn paint_cursor(&mut self, ov: &mut Overlays<'_>, s: &Snapshot, hitbox: Option<&Hitbox>) {
        let panning =
            self.state.middle_pan.is_some() || (self.state.space_pan && self.state.dragging);
        let style = on_canvas(
            panning,
            self.state.dragging,
            self.state.zoom_tool,
            s.overlay.cursor,
        );
        let drawn = style.is_none();
        if let Some(hitbox) = hitbox {
            ov.window
                .set_cursor_style(style.unwrap_or(CursorStyle::Arrow), hitbox);
        }
        if drawn {
            if let Some(at) = self.pointer_on_canvas() {
                draw_cursor_glyph(ov, at, s.overlay.cursor);
            }
        }
    }

    fn pointer_on_canvas(&self) -> Option<GPoint<f32>> {
        let p = self.state.last_pointer?;
        let at = page_to_screen(&self.view(), self.viewport, p.x, p.y);
        self.viewport.contains(at).then_some(at)
    }

    /// The outline colour for a layer: the component colour for components, sets and instances and
    /// everything in them, the accent otherwise.
    fn outline_colour(&self, id: NodeId) -> Hsla {
        use figcraft_engine::components::{owning_component, owning_instance};
        let session = self.session.borrow();
        let Ok(doc) = session.doc() else {
            return self.palette.accent;
        };
        let set = matches!(
            doc.node(id).map(|n| &n.kind),
            Some(NodeKind::ComponentSet { .. })
        );
        if set || owning_component(doc, id).is_some() || owning_instance(doc, id).is_some() {
            self.palette.component
        } else {
            self.palette.accent
        }
    }
}

/// Everything the painter needs from the session, taken once and released.
struct Snapshot {
    doc: Arc<Document>,
    page: NodeId,
    selection: Vec<NodeId>,
    hover: Option<NodeId>,
    text_edit: Option<figcraft_engine::TextEdit>,
    vector_edit: Option<figcraft_engine::vectoredit::VectorEdit>,
    view: ViewState,
    /// Dev Mode's focus view: only this frame is labelled and gridded.
    focus: Option<NodeId>,
    tool: ToolId,
    overlay: OverlayView,
    show_dimensions: bool,
    highlight_hover: bool,
    scale_anchor: (u8, u8),
    arc_handles: Option<ArcHandles>,
    shape_handles: Vec<(ShapeHandle, [f64; 2])>,
    builder: Option<Arc<figcraft_engine::shapebuilder::Arrangement>>,
    edit_box: Option<[Point; 4]>,
    origin: Option<Point>,
    composition: Option<Composition>,
}

/// The page's own background colour, or `None` when the page has none to show.
fn page_background(doc: &Document, page: NodeId) -> Option<Hsla> {
    match doc.node(page).map(|n| &n.kind) {
        Some(NodeKind::Page { background, .. }) => Some(engine_color(*background)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Cursor glyphs
// ---------------------------------------------------------------------------

/// The glyphs GPUI has no cursor for: the rotate arc, the pen's four states, the eyedropper and
/// the comment pin. Drawn from lines, as figcraft draws them.
///
/// Dark lines over a light edge, so the glyph reads on any artwork.
fn draw_cursor_glyph(ov: &mut Overlays<'_>, at: GPoint<f32>, cursor: Cursor) {
    let ink = Hsla::from(rgba(0x1e1e1eff));
    let edge = Hsla::from(rgba(0xffffffff));
    // The glyph is drawn in plain logical points and handed to the painter as such; the painter
    // is the only thing that knows about device pixels.
    let stroke = |ov: &mut Overlays<'_>, pts: Vec<GPoint<f32>>| {
        ov.stroke_poly(&pts, 3.5, edge, false);
        ov.stroke_poly(&pts, 1.5, ink, false);
    };
    match cursor {
        Cursor::Rotate(degrees) => {
            // An arc bulging away from the box, through the pointer, with an arrowhead at each end.
            let d = if degrees.is_finite() {
                degrees.to_radians() as f32
            } else {
                0.0
            };
            let (r, half) = (11.0_f32, 0.8_f32);
            let c = (at.x - d.cos() * r, at.y - d.sin() * r);
            let arc: Vec<GPoint<f32>> = (0..=12)
                .map(|i| {
                    let a = d - half + (2.0 * half) * i as f32 / 12.0;
                    point(c.0 + a.cos() * r, c.1 + a.sin() * r)
                })
                .collect();
            stroke(ov, arc);
            for (end, sign) in [(d - half, -1.0_f32), (d + half, 1.0_f32)] {
                let tip = (c.0 + end.cos() * r, c.1 + end.sin() * r);
                // Along the arc at its end, pointing onwards.
                let t = (-end.sin() * sign, end.cos() * sign);
                let n = (end.cos(), end.sin());
                stroke(
                    ov,
                    vec![
                        point(tip.0 - t.0 * 4.5 + n.0 * 3.5, tip.1 - t.1 * 4.5 + n.1 * 3.5),
                        point(tip.0 + t.0, tip.1 + t.1),
                        point(tip.0 - t.0 * 4.5 - n.0 * 3.5, tip.1 - t.1 * 4.5 - n.1 * 3.5),
                    ],
                );
            }
        }
        Cursor::Pen(state) => {
            // A cross open at the centre (the point it lands on stays visible), and what a click
            // does at its lower right.
            for (a, b) in [
                ((-8.0, 0.0), (-2.5, 0.0)),
                ((2.5, 0.0), (8.0, 0.0)),
                ((0.0, -8.0), (0.0, -2.5)),
                ((0.0, 2.5), (0.0, 8.0)),
            ] {
                stroke(
                    ov,
                    vec![point(at.x + a.0, at.y + a.1), point(at.x + b.0, at.y + b.1)],
                );
            }
            let b = (at.x + 10.0, at.y + 10.0);
            match state {
                PenCursor::New => {}
                PenCursor::Close => {
                    let mut ring: Vec<GPoint<f32>> = (0..=12)
                        .map(|i| {
                            let a = std::f32::consts::TAU * i as f32 / 12.0;
                            point(b.0 + a.cos() * 3.0, b.1 + a.sin() * 3.0)
                        })
                        .collect();
                    ring.push(point(b.0 + 3.0, b.1));
                    stroke(ov, ring);
                }
                PenCursor::Add => {
                    stroke(ov, vec![point(b.0 - 3.5, b.1), point(b.0 + 3.5, b.1)]);
                    stroke(ov, vec![point(b.0, b.1 - 3.5), point(b.0, b.1 + 3.5)]);
                }
                PenCursor::Bend => {
                    let bend: Vec<GPoint<f32>> = (0..=12)
                        .map(|i| {
                            let a = -std::f32::consts::FRAC_PI_2
                                + std::f32::consts::FRAC_PI_2 * i as f32 / 12.0;
                            point(b.0 - 3.5 + a.cos() * 7.0, b.1 + 3.5 + a.sin() * 7.0)
                        })
                        .collect();
                    stroke(ov, bend);
                }
            }
        }
        Cursor::Eyedropper => {
            let c = (at.x - 2.5, at.y - 13.5);
            ov.fill_radii(
                Bounds::new(point(px(c.0 - 2.5), px(c.1 + 6.0)), size(px(10.0), px(9.0))),
                2.0,
                ink,
            );
            stroke(ov, vec![point(c.0 + 1.0, c.1 + 6.0), point(c.0 + 7.0, c.1)]);
        }
        Cursor::Comment => {
            let c = (at.x - 5.0, at.y - 13.5);
            ov.fill_poly(
                &[
                    point(c.0, c.1),
                    point(c.0 + 10.0, c.1),
                    point(c.0 + 10.0, c.1 + 8.0),
                    point(c.0 + 5.0, c.1 + 8.0),
                    point(c.0 + 3.0, c.1 + 11.0),
                    point(c.0 + 3.0, c.1 + 8.0),
                    point(c.0, c.1 + 8.0),
                ],
                ink,
            );
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// GPUI's key name for the engine's, for the keys the engine names.
///
/// GPUI reports names lower-cased (`"left"`, `"escape"`, `"pageup"`) and the engine's are
/// `PascalCase`, so a plain lookup would never match. `None` for a key with no name at all, which
/// is how a character to type is recognised.
pub fn engine_key_name(key: &str) -> Option<&'static str> {
    match key {
        "left" => Some("Left"),
        "right" => Some("Right"),
        "up" => Some("Up"),
        "down" => Some("Down"),
        "home" => Some("Home"),
        "end" => Some("End"),
        "backspace" => Some("Backspace"),
        "delete" | "del" => Some("Delete"),
        "enter" | "return" => Some("Enter"),
        "tab" => Some("Tab"),
        "escape" | "esc" => Some("Escape"),
        "space" => Some("Space"),
        "pagedown" => Some("PageDown"),
        "pageup" => Some("PageUp"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// UTF-8 / UTF-16
//
// GPUI's input protocol is in UTF-16 code units and the engine's text is indexed in `char`s, so
// every range crossing the boundary is converted here rather than anywhere else.
// ---------------------------------------------------------------------------

fn utf8_boundary(text: &str, offset: usize) -> usize {
    let mut at = offset.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn utf16_to_utf8(text: &str, offset: usize) -> usize {
    let mut utf16_offset = 0;
    let mut utf8_offset = 0;
    for character in text.chars() {
        if utf16_offset + character.len_utf16() > offset {
            break;
        }
        utf16_offset += character.len_utf16();
        utf8_offset += character.len_utf8();
    }
    utf8_offset
}

fn utf8_to_utf16(text: &str, offset: usize) -> usize {
    text[..utf8_boundary(text, offset)].encode_utf16().count()
}

fn utf16_range_to_utf8(text: &str, range: Range<usize>) -> Range<usize> {
    let length = text.encode_utf16().count();
    let start = range.start.min(length);
    let end = range.end.min(length).max(start);
    utf16_to_utf8(text, start)..utf16_to_utf8(text, end)
}

/// The character index a UTF-16 offset names.
///
/// GPUI's input protocol counts UTF-16 code units and the engine counts
/// `char`s, so an offset that crosses the boundary has to be converted
/// rather than reused. The byte offset [`utf16_to_utf8`] finds is the way
/// into the string; the character count is the way into the engine.
fn utf16_to_char(text: &str, offset: usize) -> usize {
    text[..utf16_to_utf8(text, offset)].chars().count()
}

fn utf8_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    utf8_to_utf16(text, range.start)..utf8_to_utf16(text, range.end)
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

impl Render for EngineCanvas {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        // THE RULERS ARE ON, because the ruler-guides layer is mounted.
        //
        // The engine's `view.rulers` flag is figcraft's VIEW MENU state: in
        // figcraft the user turns the rulers on from View > Rulers. Spool has
        // no View menu yet, so there is no way for the user to reach the flag
        // and the layer would be mounted and invisible.
        //
        // So the layer is the switch, and the flag follows it. The moment a
        // View menu exists this goes away and the flag becomes the user's
        // choice again -- and the guides layer's own `view.rulers` guard comes
        // back with it.
        {
            let mut session = self.session.borrow_mut();
            if let Ok(mut ds) = session.ds_mut() {
                ds.view.rulers = true;
            }
        }
        let hitbox_pre = self.hitbox.clone();
        let hitbox_paint = self.hitbox.clone();
        let entity_pre = cx.entity();
        let entity_paint = cx.entity();
        let focus = self.focus.clone();

        div()
            .id("engine-canvas")
            .debug_selector(|| "engine-canvas".to_string())
            .relative()
            // `size_full` as well as `flex_1`: the canvas paint closure is handed
            // the element's bounds, and an empty bounds is refused by the render
            // thread as NoUsableScale. `flex_1` alone did not give it a size in
            // the shell's column, so the project loaded and never drew.
            .size_full()
            .flex_1()
            .overflow_hidden()
            .bg(rgb(theme::CANVAS))
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_exit(cx.listener(Self::on_mouse_exit))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_pinch(cx.listener(Self::on_pinch))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                        hitbox_pre.replace(Some(hitbox.clone()));
                        let viewport = Viewport::new(bounds);
                        // Keep the engine's viewport in step: zoom-to-fit and the rulers need it.
                        entity_pre.update(cx, |this, _| {
                            this.viewport = viewport;
                            let mut session = this.session.borrow_mut();
                            if let Ok(ds) = session.ds_mut() {
                                ds.view.vw = f64::from(viewport.size.0);
                                ds.view.vh = f64::from(viewport.size.1);
                            }
                        });
                        hitbox
                    },
                    move |bounds, _hitbox, window, cx| {
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, entity_paint.clone()),
                            cx,
                        );
                        let ratio = window.scale_factor();
                        let hitbox = hitbox_paint.borrow().clone();
                        entity_paint.update(cx, |this, cx| {
                            this.paint(window, cx, bounds, ratio, hitbox);
                        });
                    },
                )
                // GPUI's `Canvas` element lays itself out from its OWN style
                // (`request_layout` builds `Style::default()` and refines it with
                // whatever is set here). It is childless, so with no size set it
                // collapses to zero height -- and a zero-height viewport is
                // refused by the render thread as NoUsableScale, which used to be
                // swallowed, so a project that compiled correctly opened to a
                // black canvas with no error anywhere.
                //
                // Sizing the wrapping div does not help: an explicit height on the
                // parent was tried and the bounds stayed `918px x 0px`, because
                // the bounds handed to the closure are this element's, not the
                // parent's.
                .absolute()
                .size_full(),
            )
            // The ruler guides: the guide lines over the artboard, and the
            // gestures that make, move and remove them.
            .child(
                guides::GuidesLayer::new(
                    self.session.clone(),
                    self.focus.clone(),
                    self.guide_state.clone(),
                )
                .element(),
            )
    }
}

// ---------------------------------------------------------------------------
// Text input
// ---------------------------------------------------------------------------

impl EngineCanvas {
    /// The edited text and its characters, or `None` when no text is being edited.
    ///
    /// The engine indexes text in `char`s and GPUI in UTF-16, so both come back together and
    /// nothing has to remember which index space it is holding. The `String` is cloned because the
    /// document is behind a borrow that cannot outlive this call.
    fn edited_text(&self) -> Option<(String, usize)> {
        let session = self.session.borrow();
        let te = session.ds().ok()?.text_edit.as_ref()?;
        let node = session.doc().ok()?.node(te.id)?;
        let data = node.kind.text()?;
        Some((data.characters.clone(), data.char_count()))
    }

    fn doc_arc(&self) -> Option<Arc<Document>> {
        self.session.borrow().ds().ok().map(|ds| ds.doc.clone())
    }

    /// Put the caret at `a` and the anchor at `b` (or both at `a`).
    fn select_chars(&self, a: usize, b: usize) {
        let mut session = self.session.borrow_mut();
        let _ = textedit::set_caret(&mut session, a, false);
        if b != a {
            let _ = textedit::set_caret(&mut session, b, true);
        }
    }
}

impl EntityInputHandler for EngineCanvas {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut gpui::Context<Self>,
    ) -> Option<String> {
        let (text, _) = self.edited_text()?;
        let range = utf16_range_to_utf8(&text, range_utf16);
        adjusted_range.replace(utf8_range_to_utf16(&text, range.clone()));
        Some(text[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut gpui::Context<Self>,
    ) -> Option<UTF16Selection> {
        let (text, _) = self.edited_text()?;
        let te = self.text_edit()?;
        let (start, end) = (te.anchor.min(te.caret), te.anchor.max(te.caret));
        Some(UTF16Selection {
            range: utf8_range_to_utf16(&text, start..end),
            reversed: te.anchor > te.caret,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut gpui::Context<Self>,
    ) -> Option<Range<usize>> {
        // Deliberately empty: the composition is not in the document, so there is no range in the
        // document's own text to mark. The IME sends each composition through
        // `replace_and_mark_text_in_range`, which lands it in `TextEdit::preedit`, and the canvas
        // draws it from [`Composition`]. Reporting a marked range here would point the IME at text
        // that is not there.
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        // THE ONE WRITER OF CHARACTERS.
        //
        // Everything that puts text into the document comes through here: a
        // typed character, an IME's committed composition, and a paste (the
        // default `paste` routes through this method too). The key event
        // writes nothing, so a character lands once — see `handle_key`.
        if !self.is_editing_text() {
            return;
        }
        if let Some(range) = range {
            // The range is in UTF-16 code units and the engine's text is
            // indexed in `char`s, which are not the same thing for anything
            // outside ASCII.
            let (characters, len) = self.edited_text().unwrap_or_default();
            let a = utf16_to_char(&characters, range.start).min(len);
            let b = utf16_to_char(&characters, range.end).min(len);
            self.select_chars(a, b);
        }
        let owned = text.to_owned();
        let _ = textedit::insert(&mut self.session.borrow_mut(), &owned);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Some(range) = range {
            let (_, len) = self.edited_text().unwrap_or_default();
            let (a, b) = (range.start.min(len), range.end.min(len));
            self.select_chars(a, b);
        }
        let owned = new_text.to_owned();
        {
            let mut session = self.session.borrow_mut();
            // An empty composition cancels it; the engine filters control characters and caps it.
            let _ = textedit::preedit(&mut session, &owned, None);
        }
        if let Some(selected) = new_selected_range {
            let chars: Vec<char> = owned.chars().collect();
            let a = selected.start.min(chars.len());
            let b = selected.end.min(chars.len()).max(a);
            let cursor = a + chars[a..b].iter().filter(|c| !c.is_control()).count();
            let _ = textedit::preedit(&mut self.session.borrow_mut(), &owned, Some(cursor));
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut gpui::Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (text, _) = self.edited_text()?;
        let te = self.text_edit()?;
        let start = utf16_range_to_utf8(&text, range_utf16).start;
        let doc = self.doc_arc()?;
        let on_screen = TextOnScreen::of(&doc, te.id)?;
        let viewport = self.viewport;
        let view = self.view();
        let caret = on_screen.caret(&view, viewport, start.min(te.caret), view.zoom);
        let height = (caret[1].y - caret[0].y).max(1.0);
        Some(Bounds::new(
            point(px(caret[0].x), px(caret[0].y)),
            size(px(LINE_PX), px(height)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _at: GPoint<Pixels>,
        _window: &mut Window,
        _cx: &mut gpui::Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some((text, _)) = self.edited_text() else {
            return;
        };
        let range = utf16_range_to_utf8(&text, range_utf16);
        self.select_chars(range.start, range.end);
        cx.notify();
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut gpui::Context<Self>,
    ) -> Option<usize> {
        let (text, _) = self.edited_text()?;
        Some(text.encode_utf16().count())
    }

    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> bool {
        self.is_editing_text()
    }
}

pub mod guides;

#[cfg(test)]
mod tests;
