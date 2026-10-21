//! Spool's Layers panel: a GPUI port of figcraft's `crates/ui-egui/src/layers.rs`.
//!
//! What was ported, and from where:
//!
//! | figcraft (egui) | here (GPUI) |
//! |---|---|
//! | `layers::Row`, `collect`, `matching`, `hides_children`, `listed_rows`, `RowCache` | [`Row`], [`listed_rows`], [`RowCache`] — unchanged semantics |
//! | `layers::rows_for` (egui temp-data cache) | [`RowCache::rows`], keyed on `Arc::ptr_eq` of the document |
//! | `egui::ScrollArea::show_viewport` + `skip_ahead_auto_ids` | [`visible_window`] + an absolutely-positioned row band over a full-height spacer |
//! | `layers::key_nav` | [`nav_step`] (pure), driven by [`LayersPanel::handle_keystroke`] |
//! | `list_drag::drop_slot` | [`drop_slot`] (pure) |
//! | the drop-zone maths inside the row loop | [`layer_drop_zone`] (pure) |
//! | `layers::row_action`-equivalent decisions | [`row_action`] / [`RowHit`] / [`SelectMode`] (pure) |
//! | `ui-egui/src/icons.rs` primitive painting | [`icon_svg`] — the same 16×16 path data, emitted as SVG |
//!
//! Everything that changes the document goes through [`Session::execute`] with a real command id
//! from `crates/engine/src/cmd/`. The panel holds no second copy of the tree, the selection or the
//! document half of the expanded set: `Node.visible` / `Node.locked` / `Node.name` are read from
//! the engine every frame, and [`PanelState`] keeps only what is genuinely the editor's (which
//! rows are open, what is being dragged, where the list is scrolled).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use figcraft_doc::{BoolOp, Document, LayoutMode, NodeId, NodeKind};
use figcraft_engine::{booleans, components, Session};
use gpui::{
    canvas, div, px, rgb, svg, AnyElement, ClickEvent, Context, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, Keystroke, MouseButton, ParentElement, Render, ScrollDelta,
    SharedString, StatefulInteractiveElement, Styled, Svg, Window,
};
use serde_json::{json, Value};

use crate::theme;

// ---------------------------------------------------------------------------
// Layout constants
// ---------------------------------------------------------------------------

/// Height of one layers row. figcraft's `layers::ROW`.
pub const ROW: f32 = 32.0;
/// Distance between one row's contents and the next level's.
pub const INDENT: f32 = 16.0;
/// The Layers header and the Pages header.
pub const HEADER_H: f32 = 40.0;
/// The file-name header.
pub const FILE_HEADER_H: f32 = 64.0;
/// One row plus the gap under it: the grid the viewport is windowed against.
pub const PITCH: f32 = ROW;
/// The search box under the Layers header.
pub const SEARCH_H: f32 = 32.0;
/// Width of the icon rail.
pub const RAIL_W: f32 = 48.0;
/// The pages list takes at most this share of the panel before it scrolls.
pub const PAGES_MAX_SHARE: f32 = 0.4;

/// figcraft's search stops after this many hits, so a filter over a huge file stays a frame.
pub const SEARCH_HIT_CAP: usize = 5000;
/// Beyond this depth the tree stops opening: 64 levels is already past the canvas.
pub const MAX_DEPTH: usize = 64;
/// How long a collapsed container must be held over before it opens for a drop.
pub const SPRING_LOADED_SECS: f32 = 0.6;
/// How close to a viewport edge a drag must be before the list scrolls itself.
pub const EDGE: f32 = 28.0;
/// Pointer travel before a press becomes a drag rather than a click.
pub const DRAG_THRESHOLD: f32 = 4.0;

/// Component rows and component icons: the one colour the panel adds to the theme.
const COMPONENT: u32 = 0x6fd0b0;
/// ... and the fill a component row takes when selected.
const COMPONENT_ROW: u32 = 0x1e3a33;

// ---------------------------------------------------------------------------
// Icons
// ---------------------------------------------------------------------------

/// The per-kind layer icon, plus the chrome the panel draws.
///
/// figcraft has 140 of these drawn from primitives on a 16×16 grid. Only the ones this panel
/// draws are carried over, and they are carried over as *path data* ([`LayerIcon::body`]) rather
/// than as painting code: GPUI's `svg()` element hands the bytes to the window, which renders
/// them to an alpha mask and tints that with the element's text colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayerIcon {
    // Layer kinds (figcraft's `layers::node_icon`).
    Page,
    Frame,
    Slot,
    LayoutH,
    LayoutV,
    LayoutGrid,
    Group,
    Section,
    Rectangle,
    Image,
    Ellipse,
    Polygon,
    Star,
    Line,
    Arrow,
    Vector,
    Text,
    Union,
    Subtract,
    Intersect,
    Exclude,
    Component,
    ComponentSet,
    Instance,
    Slice,
    // Chrome.
    ChevronDown,
    ChevronRight,
    ChevronLeft,
    ChevronUp,
    Eye,
    EyeOff,
    Lock,
    Unlock,
    Search,
    Plus,
    Panels,
    Variables,
    Assets,
    Dots,
    Logo,
}

impl LayerIcon {
    /// Every icon this panel can draw, for the gallery test.
    pub const ALL: &'static [LayerIcon] = &[
        LayerIcon::Page,
        LayerIcon::Frame,
        LayerIcon::Slot,
        LayerIcon::LayoutH,
        LayerIcon::LayoutV,
        LayerIcon::LayoutGrid,
        LayerIcon::Group,
        LayerIcon::Section,
        LayerIcon::Rectangle,
        LayerIcon::Image,
        LayerIcon::Ellipse,
        LayerIcon::Polygon,
        LayerIcon::Star,
        LayerIcon::Line,
        LayerIcon::Arrow,
        LayerIcon::Vector,
        LayerIcon::Text,
        LayerIcon::Union,
        LayerIcon::Subtract,
        LayerIcon::Intersect,
        LayerIcon::Exclude,
        LayerIcon::Component,
        LayerIcon::ComponentSet,
        LayerIcon::Instance,
        LayerIcon::Slice,
        LayerIcon::ChevronDown,
        LayerIcon::ChevronRight,
        LayerIcon::ChevronLeft,
        LayerIcon::ChevronUp,
        LayerIcon::Eye,
        LayerIcon::EyeOff,
        LayerIcon::Lock,
        LayerIcon::Unlock,
        LayerIcon::Search,
        LayerIcon::Plus,
        LayerIcon::Panels,
        LayerIcon::Variables,
        LayerIcon::Assets,
        LayerIcon::Dots,
        LayerIcon::Logo,
    ];

    /// The SVG body of the icon, on figcraft's 16×16 grid with the origin top-left.
    ///
    /// Coordinates are transcribed from `crates/ui-egui/src/icons.rs`: `p.line(a,b,c,d)` becomes
    /// a path segment, `p.rect` a `rect`, `p.circle` a `circle`, `p.fill`/`p.fill_rect` a solid
    /// shape, `p.poly`/`p.path` a `polyline`/`polygon`. The paint colour is `#000` throughout
    /// because GPUI's SVG path keeps only the alpha and tints with the element's text colour.
    pub fn body(self) -> &'static str {
        match self {
            LayerIcon::Frame => {
                "<path d=\"M5.5 2V14M10.5 2V14M2 5.5H14M2 10.5H14\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\"/>"
            }
            LayerIcon::Slot => {
                "<rect x=\"2.5\" y=\"2.5\" width=\"10\" height=\"10\" rx=\"1\" stroke=\"#000\" stroke-width=\"1.1\" stroke-dasharray=\"2.4 2\" fill=\"none\"/>\
                 <path d=\"M7.5 5V10M5 7.5H10\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\"/>"
            }
            LayerIcon::Section => {
                "<rect x=\"2.5\" y=\"5.5\" width=\"11\" height=\"8\" rx=\"1\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <rect x=\"2\" y=\"2\" width=\"7\" height=\"4\" rx=\"1\" fill=\"#000\"/>"
            }
            LayerIcon::Slice => {
                "<rect x=\"2.5\" y=\"2.5\" width=\"11\" height=\"11\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <path d=\"M2.5 13.5 13.5 2.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\"/>"
            }
            LayerIcon::Rectangle => {
                "<rect x=\"2.5\" y=\"2.5\" width=\"11\" height=\"11\" rx=\"1\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>"
            }
            LayerIcon::Image => {
                "<rect x=\"2.5\" y=\"3.5\" width=\"11\" height=\"9\" rx=\"1\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <path d=\"M2.5 11.5 6 8 9 11 10.5 9.5 13.5 12.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linejoin=\"round\"/>\
                 <circle cx=\"10.5\" cy=\"6.5\" r=\"1.1\" fill=\"#000\"/>"
            }
            LayerIcon::Ellipse => {
                "<circle cx=\"8\" cy=\"8\" r=\"5.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>"
            }
            LayerIcon::Polygon => "<polygon points=\"8,2.5 13.5,12.5 2.5,12.5\" fill=\"#000\"/>",
            LayerIcon::Star => {
                "<polygon points=\"8,2.6 9.59,6.42 13.71,6.75 10.57,9.43 11.53,13.45 8,11.3 4.47,13.45 5.43,9.43 2.29,6.75 6.41,6.42\" \
                 stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linejoin=\"round\"/>"
            }
            LayerIcon::Line => {
                "<path d=\"M2.5 13.5 13.5 2.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\"/>"
            }
            LayerIcon::Arrow => {
                "<path d=\"M2.5 13.5 13 3\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\"/>\
                 <path d=\"M7.5 2.5H13.5V8.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
            }
            LayerIcon::Vector => {
                "<path d=\"M3 12.5 6 4 10 12 13 3.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linejoin=\"round\"/>\
                 <rect x=\"1.5\" y=\"11\" width=\"3\" height=\"3\" fill=\"#000\"/>\
                 <rect x=\"11.5\" y=\"2\" width=\"3\" height=\"3\" fill=\"#000\"/>"
            }
            LayerIcon::Text => {
                "<path d=\"M2 3.5H13M7.5 3V13M2.5 3V6M12.5 3V6M5 12.5H10\" stroke=\"#000\" \
                 stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\"/>"
            }
            LayerIcon::Group => {
                "<path d=\"M6 2.5H2.5V6M10 2.5H13.5V6M6 13.5H2.5V10M10 13.5H13.5V10\" \
                 stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
            }
            LayerIcon::Component => {
                "<path d=\"M8 1.7 10.5 4.2 8 6.7 5.5 4.2Z\" fill=\"#000\"/>\
                 <path d=\"M11.8 5.5 14.3 8 11.8 10.5 9.3 8Z\" fill=\"#000\"/>\
                 <path d=\"M8 9.3 10.5 11.8 8 14.3 5.5 11.8Z\" fill=\"#000\"/>\
                 <path d=\"M4.2 5.5 6.7 8 4.2 10.5 1.7 8Z\" fill=\"#000\"/>"
            }
            LayerIcon::ComponentSet => {
                "<g stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\">\
                 <polygon points=\"8,2.1 10.2,4.3 8,6.5 5.8,4.3\"/>\
                 <polygon points=\"11.7,5.8 13.9,8 11.7,10.2 9.5,8\"/>\
                 <polygon points=\"8,9.5 10.2,11.7 8,13.9 5.8,11.7\"/>\
                 <polygon points=\"4.3,5.8 6.5,8 4.3,10.2 2.1,8\"/></g>"
            }
            LayerIcon::Instance => {
                "<polygon points=\"8,2 14,8 8,14 2,8\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>"
            }
            LayerIcon::Page => {
                "<polygon points=\"3.5,2.5 9.5,2.5 12.5,5.5 12.5,13.5 3.5,13.5\" fill=\"#000\"/>\
                 <path d=\"M9.5 2.5V5.5H12.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linejoin=\"round\" stroke-linecap=\"round\"/>"
            }
            LayerIcon::LayoutH => {
                "<rect x=\"2\" y=\"3\" width=\"5\" height=\"10\" rx=\"1\" fill=\"#000\"/>\
                 <rect x=\"9\" y=\"3\" width=\"5\" height=\"10\" rx=\"1\" fill=\"#000\"/>"
            }
            LayerIcon::LayoutV => {
                "<rect x=\"3\" y=\"2\" width=\"10\" height=\"5\" rx=\"1\" fill=\"#000\"/>\
                 <rect x=\"3\" y=\"9\" width=\"10\" height=\"5\" rx=\"1\" fill=\"#000\"/>"
            }
            LayerIcon::LayoutGrid => {
                "<g fill=\"none\" stroke=\"#000\" stroke-width=\"1\">\
                 <rect x=\"2.5\" y=\"2.5\" width=\"3\" height=\"3\"/><rect x=\"6.5\" y=\"2.5\" width=\"3\" height=\"3\"/>\
                 <rect x=\"10.5\" y=\"2.5\" width=\"3\" height=\"3\"/><rect x=\"2.5\" y=\"6.5\" width=\"3\" height=\"3\"/>\
                 <rect x=\"6.5\" y=\"6.5\" width=\"3\" height=\"3\"/><rect x=\"10.5\" y=\"6.5\" width=\"3\" height=\"3\"/>\
                 <rect x=\"2.5\" y=\"10.5\" width=\"3\" height=\"3\"/><rect x=\"6.5\" y=\"10.5\" width=\"3\" height=\"3\"/>\
                 <rect x=\"10.5\" y=\"10.5\" width=\"3\" height=\"3\"/></g>"
            }
            LayerIcon::Union | LayerIcon::Exclude => {
                "<rect x=\"2.5\" y=\"2.5\" width=\"7\" height=\"7\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <rect x=\"6.5\" y=\"6.5\" width=\"7\" height=\"7\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>"
            }
            LayerIcon::Subtract => {
                "<rect x=\"2\" y=\"2\" width=\"8\" height=\"8\" fill=\"#000\"/>\
                 <rect x=\"6.5\" y=\"6.5\" width=\"7\" height=\"7\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>"
            }
            LayerIcon::Intersect => {
                "<rect x=\"2.5\" y=\"2.5\" width=\"7\" height=\"7\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <rect x=\"6.5\" y=\"6.5\" width=\"7\" height=\"7\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <rect x=\"6\" y=\"6\" width=\"4\" height=\"4\" fill=\"#000\"/>"
            }
            LayerIcon::ChevronDown => {
                "<path d=\"M4.5 6 8 9.5 11.5 6\" stroke=\"#000\" stroke-width=\"1.2\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
            }
            LayerIcon::ChevronUp => {
                "<path d=\"M4.5 10 8 6.5 11.5 10\" stroke=\"#000\" stroke-width=\"1.2\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
            }
            LayerIcon::ChevronRight => {
                "<path d=\"M6 4.5 9.5 8 6 11.5\" stroke=\"#000\" stroke-width=\"1.2\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
            }
            LayerIcon::ChevronLeft => {
                "<path d=\"M10 4.5 6.5 8 10 11.5\" stroke=\"#000\" stroke-width=\"1.2\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
            }
            LayerIcon::Eye => {
                "<path d=\"M2.5 7.5A7 7 0 0 1 13.5 7.5A7 7 0 0 1 2.5 7.5Z\" stroke=\"#000\" \
                 stroke-width=\"1.1\" fill=\"none\" stroke-linejoin=\"round\"/>\
                 <circle cx=\"8\" cy=\"7.5\" r=\"2\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>"
            }
            LayerIcon::EyeOff => {
                "<path d=\"M2.5 7.5A7 7 0 0 1 13.5 7.5A7 7 0 0 1 2.5 7.5Z\" stroke=\"#000\" \
                 stroke-width=\"1.1\" fill=\"none\" stroke-linejoin=\"round\"/>\
                 <path d=\"M3 13 13 2\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\"/>"
            }
            LayerIcon::Lock => {
                "<rect x=\"3.5\" y=\"7.5\" width=\"9\" height=\"6\" rx=\"1\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <path d=\"M5.5 5.5A2.5 2.5 0 0 1 10.5 5.5V7.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <path d=\"M5.5 7.5V5.5M10.5 7.5V5.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\"/>\
                 <circle cx=\"8\" cy=\"10.5\" r=\"1\" fill=\"#000\"/>"
            }
            LayerIcon::Unlock => {
                "<rect x=\"3.5\" y=\"7.5\" width=\"9\" height=\"6\" rx=\"1\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <path d=\"M10.5 4.5A2.5 2.5 0 0 1 15.5 4.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <path d=\"M10.5 7.5V4.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\"/>\
                 <circle cx=\"8\" cy=\"10.5\" r=\"1\" fill=\"#000\"/>"
            }
            LayerIcon::Search => {
                "<circle cx=\"7\" cy=\"7\" r=\"4.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <path d=\"M10.5 10.5 14 14\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\"/>"
            }
            LayerIcon::Plus => {
                "<path d=\"M7.5 2V13M2 7.5H13\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" stroke-linecap=\"round\"/>"
            }
            LayerIcon::Panels => {
                "<rect x=\"2.5\" y=\"3.5\" width=\"11\" height=\"9\" rx=\"1\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <path d=\"M5.5 4V12M10.5 4V12\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>"
            }
            LayerIcon::Variables => {
                "<circle cx=\"5\" cy=\"5\" r=\"2.2\" fill=\"#000\"/><circle cx=\"11\" cy=\"5\" r=\"2.2\" fill=\"#000\"/>\
                 <circle cx=\"5\" cy=\"11\" r=\"2.2\" fill=\"#000\"/><circle cx=\"11\" cy=\"11\" r=\"2.2\" fill=\"#000\"/>"
            }
            LayerIcon::Assets => {
                "<polygon points=\"5,2 8,5 5,8 2,5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <circle cx=\"11\" cy=\"5\" r=\"2.5\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <rect x=\"2.5\" y=\"9.5\" width=\"5\" height=\"4\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\"/>\
                 <polygon points=\"11,9 13.5,13.5 8.5,13.5\" fill=\"#000\"/>"
            }
            LayerIcon::Dots => {
                "<circle cx=\"3.5\" cy=\"8\" r=\"1.25\" fill=\"#000\"/><circle cx=\"8\" cy=\"8\" r=\"1.25\" fill=\"#000\"/>\
                 <circle cx=\"12.5\" cy=\"8\" r=\"1.25\" fill=\"#000\"/>"
            }
            // FigCraft's own mark, carried over as path data: the curled tail of the chameleon on
            // the app icon — a shaft along the bottom, then three half turns, each tighter.
            LayerIcon::Logo => {
                "<path d=\"M13.75 13H7.25A5 5 0 0 1 7.25 3A3.5 3.5 0 0 1 7.25 10A1.75 1.75 0 0 1 7.25 6.5\" \
                 stroke=\"#000\" stroke-width=\"1.7\" fill=\"none\" stroke-linecap=\"round\"/>\
                 <circle cx=\"13.75\" cy=\"13\" r=\"0.85\" fill=\"#000\"/>\
                 <circle cx=\"7.25\" cy=\"6.5\" r=\"0.85\" fill=\"#000\"/>"
            }
        }
    }
}

/// A boolean operation's icon. figcraft's `layers::op_icon`.
pub fn op_icon(op: BoolOp) -> LayerIcon {
    match op {
        BoolOp::Union => LayerIcon::Union,
        BoolOp::Subtract => LayerIcon::Subtract,
        BoolOp::Intersect => LayerIcon::Intersect,
        BoolOp::Exclude => LayerIcon::Exclude,
    }
}

/// The icon for `id`, from its kind and layout. figcraft's `layers::node_icon`.
pub fn node_icon(doc: &Document, id: NodeId) -> LayerIcon {
    let Some(n) = doc.node(id) else {
        return LayerIcon::Rectangle;
    };
    // An operand combining by an operation other than its group's shows that operation.
    if let Some(op) = booleans::distinct_op(doc, id) {
        return op_icon(op);
    }
    match &n.kind {
        NodeKind::Page { .. } => LayerIcon::Page,
        NodeKind::Frame if components::is_slot(doc, id) => LayerIcon::Slot,
        NodeKind::Frame => match n.layout.as_ref().map(|l| l.mode) {
            Some(LayoutMode::Vertical) => LayerIcon::LayoutV,
            Some(LayoutMode::Grid) => LayerIcon::LayoutGrid,
            Some(_) => LayerIcon::LayoutH,
            None => LayerIcon::Frame,
        },
        NodeKind::Group => LayerIcon::Group,
        NodeKind::Section { .. } => LayerIcon::Section,
        NodeKind::Rectangle => {
            if n.fills
                .iter()
                .any(|f| matches!(f.kind, figcraft_doc::color::PaintKind::Image(_)))
            {
                LayerIcon::Image
            } else {
                LayerIcon::Rectangle
            }
        }
        NodeKind::Ellipse { .. } => LayerIcon::Ellipse,
        NodeKind::Polygon { .. } => LayerIcon::Polygon,
        NodeKind::Star { .. } => LayerIcon::Star,
        NodeKind::Line => {
            if n.stroke.cap_end.is_arrow() {
                LayerIcon::Arrow
            } else {
                LayerIcon::Line
            }
        }
        NodeKind::Vector { .. } => LayerIcon::Vector,
        NodeKind::Text(_) => LayerIcon::Text,
        NodeKind::BooleanOp { op } => op_icon(*op),
        NodeKind::Component { .. } => LayerIcon::Component,
        NodeKind::ComponentSet { .. } => LayerIcon::ComponentSet,
        NodeKind::Instance { .. } => LayerIcon::Instance,
        NodeKind::Slice => LayerIcon::Slice,
    }
}

/// The icon as a standalone SVG document on a 16×16 viewBox.
pub fn icon_svg(icon: LayerIcon) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\" width=\"16\" height=\"16\">{}</svg>",
        icon.body()
    )
}

/// Where a drop landed on a row: inside it, or on one of its edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropZone {
    /// Into `id`, as a new child.
    Into(NodeId),
}

/// Which zone a pointer at `rel` (0 at the row's top edge, 1 at its bottom) over a row drops into.
///
/// figcraft computes this inline in its row loop: the middle quarter of a container takes the
/// layer *into* it, everything else splits at the nearer horizontal half. An instance is a
/// container whose structure is fixed, so its middle quarter is a reorder like any other row's.
pub fn layer_drop_zone(rel: f32, container: bool, instance: bool) -> i8 {
    let into = container && !instance && (0.25..0.75).contains(&rel);
    if into {
        0
    } else if rel < 0.5 {
        -1
    } else {
        1
    }
}

/// How far a drag at `rel_y` inside a `height`-tall viewport scrolls the list per frame: nothing
/// in the middle, half a pixel-40 of it at the very edge. figcraft's edge auto-scroll.
pub fn edge_scroll_delta(rel_y: f32, height: f32) -> f32 {
    if height <= 0.0 || !rel_y.is_finite() {
        return 0.0;
    }
    let dy = if rel_y < EDGE {
        (EDGE - rel_y).min(EDGE)
    } else if rel_y > height - EDGE {
        -((rel_y - (height - EDGE)).min(EDGE))
    } else {
        0.0
    };
    // Matches figcraft's `dy * 0.5` scroll request.
    dy * 0.5
}

// ---------------------------------------------------------------------------
// Row projection
// ---------------------------------------------------------------------------

/// One line of the layers list. figcraft's `layers::Row`.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: NodeId,
    pub depth: usize,
    /// Whether the row has a disclosure triangle.
    pub has_kids: bool,
    /// Not shown on the canvas because of an ancestor (hidden, or a section hiding its contents).
    pub dimmed: bool,
    /// Locked because an ancestor is.
    pub locked_above: bool,
}

/// The rows of the last frame and what they were built from, so a frame where neither the
/// document, the page, the expanded set nor the filter changed doesn't walk the tree again.
///
/// figcraft keeps this in egui's temp data keyed by an `Id`; here it is a field, because the
/// panel is a view rather than a stateless `show_*` function.
#[derive(Clone, Default)]
pub struct RowCache {
    doc: Option<Arc<Document>>,
    page: Option<NodeId>,
    expanded: u64,
    filter: String,
    rows: Arc<Vec<Row>>,
}

fn hash_expanded(expanded: &BTreeSet<NodeId>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    expanded.hash(&mut h);
    h.finish()
}

impl RowCache {
    /// The rows for `doc`/`page`, rebuilding only when something they depend on changed.
    pub fn rows(
        &mut self,
        doc: &Arc<Document>,
        page: NodeId,
        expanded: &BTreeSet<NodeId>,
        filter: &str,
    ) -> Arc<Vec<Row>> {
        // A filter makes the rows independent of what is expanded, so its result is cached on the
        // filter alone: typing a character doesn't re-walk a ten-thousand-layer tree.
        let ex = if filter.is_empty() {
            hash_expanded(expanded)
        } else {
            0
        };
        let fresh = self.doc.as_ref().is_some_and(|d| Arc::ptr_eq(d, doc))
            && self.page == Some(page)
            && self.expanded == ex
            && self.filter == filter;
        if !fresh {
            let rows = listed_rows(doc, page, expanded, filter);
            *self = RowCache {
                doc: Some(doc.clone()),
                page: Some(page),
                expanded: ex,
                filter: filter.to_string(),
                rows: Arc::new(rows),
            };
        }
        self.rows.clone()
    }
}

/// The rows to list: the expanded tree, or the layers matching `filter` (lowercase).
pub fn listed_rows(
    doc: &Document,
    page: NodeId,
    expanded: &BTreeSet<NodeId>,
    filter: &str,
) -> Vec<Row> {
    let mut rows = Vec::new();
    if filter.is_empty() {
        collect(doc, page, 0, false, false, expanded, &mut rows);
    } else {
        matching(doc, page, filter, &mut rows);
    }
    rows
}

/// Whether the children of `n` don't show on the canvas (given whether `n` itself doesn't).
fn hides_children(n: &figcraft_doc::Node, dimmed: bool) -> bool {
    dimmed || !n.visible || n.hides_contents()
}

fn collect(
    doc: &Document,
    id: NodeId,
    depth: usize,
    dimmed: bool,
    locked: bool,
    expanded: &BTreeSet<NodeId>,
    out: &mut Vec<Row>,
) {
    let Some(n) = doc.node(id) else { return };
    let dim_kids = !n.is_page() && hides_children(n, dimmed);
    let lock_kids = locked || n.locked;
    // Layers panel lists top of the stack first.
    for c in n.children.iter().rev() {
        let Some(cn) = doc.node(*c) else { continue };
        let has_kids = !cn.children.is_empty() || matches!(cn.kind, NodeKind::BooleanOp { .. });
        out.push(Row {
            id: *c,
            depth,
            has_kids,
            dimmed: dim_kids,
            locked_above: lock_kids,
        });
        if expanded.contains(c) && depth < MAX_DEPTH {
            collect(doc, *c, depth + 1, dim_kids, lock_kids, expanded, out);
        }
    }
}

/// Layers whose name contains `q` (lowercase), top of the stack first, as a flat list.
fn matching(doc: &Document, id: NodeId, q: &str, out: &mut Vec<Row>) {
    let Some(n) = doc.node(id) else { return };
    for c in n.children.iter().rev() {
        let Some(cn) = doc.node(*c) else { continue };
        let name = if cn.name.is_empty() {
            cn.kind.label()
        } else {
            cn.name.as_str()
        };
        if name.to_lowercase().contains(q) {
            let above: Vec<&figcraft_doc::Node> = doc
                .ancestors(*c)
                .iter()
                .filter_map(|a| doc.node(*a))
                .filter(|a| !a.is_page())
                .collect();
            let dimmed = above.iter().any(|a| hides_children(a, false));
            out.push(Row {
                id: *c,
                depth: 0,
                has_kids: false,
                dimmed,
                locked_above: above.iter().any(|a| a.locked),
            });
        }
        if out.len() < SEARCH_HIT_CAP {
            matching(doc, *c, q, out);
        }
    }
}

/// The rows `first..last` of the list that the viewport at `offset` shows.
///
/// figcraft's `show_viewport` computes this from the clip rect; GPUI has no clip rect to read
/// during `render`, so the same arithmetic runs on the viewport box measured by the panel's probe.
/// The trailing `+ 1` and the outer clamps are figcraft's: they keep a row that starts inside the
/// viewport and one that ends inside it, and they keep the arithmetic total on an empty list.
pub fn visible_window(len: usize, offset: f32, height: f32, pitch: f32) -> Range<usize> {
    if len == 0 || pitch <= 0.0 || height <= 0.0 || !offset.is_finite() {
        return 0..0;
    }
    let offset = offset.max(0.0);
    let first = ((offset / pitch).floor().max(0.0) as usize).min(len);
    let last = (((offset + height) / pitch).ceil().max(0.0) as usize)
        .saturating_add(1)
        .min(len);
    first..last.max(first)
}

/// The scroll offset that puts row `index` in the middle of a `height`-tall viewport, clamped to
/// the list's extent (the last page of a short list sits flush, not past it).
pub fn scroll_to_row(index: usize, height: f32, pitch: f32, len: usize) -> f32 {
    let total = len as f32 * pitch;
    let top = index as f32 * pitch;
    (top - height / 2.0).max(0.0).min((total - height).max(0.0))
}

// ---------------------------------------------------------------------------
// Row interaction: pure
// ---------------------------------------------------------------------------

/// Which control of a row the pointer went down on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowHit {
    /// The disclosure triangle.
    Triangle,
    /// The name area: selects.
    Name,
    /// The eye: toggles `visible`.
    Eye,
    /// The padlock: toggles `locked`.
    Lock,
}

/// Which control of a row a pointer at `inner_x` (measured from the row's left inner edge) went
/// down on, given how this panel lays the row out: `INDENT` per depth, the disclosure triangle at
/// the indent, and the padlock and eye as the last 40 points of the row.
pub fn row_hit_geometry(inner_x: f32, inner_w: f32, depth: usize, has_kids: bool) -> RowHit {
    if inner_w <= 0.0 || !inner_x.is_finite() {
        return RowHit::Name;
    }
    let x0 = 4.0 + depth as f32 * INDENT;
    if has_kids && (x0 - 1.0..=x0 + 13.0).contains(&inner_x) {
        return RowHit::Triangle;
    }
    if inner_x >= inner_w - 20.0 {
        return RowHit::Eye;
    }
    if inner_x >= inner_w - 40.0 {
        return RowHit::Lock;
    }
    RowHit::Name
}

/// How a click on a row changes the selection.
#[derive(Clone, Debug, PartialEq)]
pub enum SelectMode {
    /// Only the clicked row.
    Replace,
    /// It is added to, or taken from, the selection.
    Toggle,
    /// Every listed row from the anchor to the clicked one.
    Range,
}

/// What a press and release on a row did.
#[derive(Clone, Debug, PartialEq)]
pub enum RowAction {
    /// Nothing: the press was on a control, or the row isn't listed.
    None,
    /// The row was selected.
    Select { id: NodeId, mode: SelectMode },
    /// The row's name is now open for editing.
    Rename { id: NodeId },
    /// The row's visibility was toggled.
    ToggleVisible { id: NodeId },
    /// The row's lock was toggled.
    ToggleLock { id: NodeId },
    /// The row's disclosure flipped; `deep` means Alt, which takes the whole subtree with it.
    ToggleExpanded { id: NodeId, deep: bool },
}

/// What a click on a row means, from where it went down and with what held.
///
/// figcraft spreads this across its row loop and the `clicked` block at the bottom of `layers()`.
/// It is arithmetic over the row order, so it is written out once here and tested. `anchor_listed`
/// says whether the range anchor is a row the list still shows.
pub fn row_action(
    hit: RowHit,
    click_count: usize,
    shift: bool,
    command: bool,
    listed: bool,
    anchor_listed: bool,
) -> RowAction {
    if !listed {
        return RowAction::None;
    }
    match hit {
        RowHit::Triangle => RowAction::ToggleExpanded {
            id: NodeId(0),
            deep: false,
        },
        RowHit::Eye => RowAction::ToggleVisible { id: NodeId(0) },
        RowHit::Lock => RowAction::ToggleLock { id: NodeId(0) },
        RowHit::Name => {
            if click_count >= 2 {
                return RowAction::Rename { id: NodeId(0) };
            }
            // Shift with a listed anchor extends the range from it; Shift alone and Cmd toggle.
            let mode = if shift && anchor_listed {
                SelectMode::Range
            } else if shift || command {
                SelectMode::Toggle
            } else {
                SelectMode::Replace
            };
            RowAction::Select {
                id: NodeId(0),
                mode,
            }
        }
    }
}

/// Where the page at `from` lands when dropped over row `over` of an `n`-row list, in its bottom
/// half: its new index, or `None` when it would stay put.
///
/// figcraft's `list_drag::drop_slot`, with the rects replaced by the row the pointer is over and
/// which half of it: the same "how many rows sit above the pointer, and is that where it already
/// is" test.
pub fn drop_slot(from: usize, over: usize, below_half: bool, n: usize) -> Option<usize> {
    if n == 0 || from >= n || over >= n {
        return None;
    }
    // The gap under the pointer: how many rows sit above it.
    let gap = if below_half { over + 1 } else { over };
    if gap == from || gap == from + 1 {
        return None;
    }
    Some(if gap > from { gap - 1 } else { gap })
}

/// What an arrow key in the list does.
#[derive(Clone, Debug, PartialEq)]
pub enum NavOutcome {
    /// Set the selection to these ids, with this anchor.
    Select { ids: Vec<NodeId>, anchor: NodeId },
    /// Flip this row open (Right) or shut (Left).
    Toggle(NodeId),
    /// Nothing to do: the arrow had nowhere to go.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKey {
    Up,
    Down,
    Left,
    Right,
}

/// The list as shown, and where the keyboard is: figcraft's `layers::key_nav`, split from the
/// `FigApp` it reads.
pub fn nav_step(
    rows: &[Row],
    selection: &[NodeId],
    anchor: Option<NodeId>,
    key: NavKey,
    shift: bool,
    expanded: &BTreeSet<NodeId>,
    filtering: bool,
    parent_of: impl Fn(NodeId) -> Option<NodeId>,
    page: NodeId,
) -> NavOutcome {
    let selected = selection
        .last()
        .and_then(|c| rows.iter().position(|r| r.id == *c));
    let Some((i, row)) = selected.map(|i| (i, &rows[i])) else {
        // Nothing listed is selected: Down starts at the top, Up at the bottom.
        let first = match key {
            NavKey::Up => rows.last(),
            NavKey::Down => rows.first(),
            _ => None,
        };
        return match first {
            Some(r) => NavOutcome::Select {
                ids: vec![r.id],
                anchor: r.id,
            },
            None => NavOutcome::None,
        };
    };
    let cur = row.id;
    match key {
        NavKey::Up | NavKey::Down => {
            let j = if key == NavKey::Up {
                i.checked_sub(1)
            } else {
                Some(i + 1).filter(|j| *j < rows.len())
            };
            let Some(next) = j.and_then(|j| rows.get(j)).map(|r| r.id) else {
                return NavOutcome::None;
            };
            if shift {
                // The rows from the anchor to the new one, with the moving end last.
                let a = anchor
                    .filter(|a| selection.contains(a))
                    .and_then(|a| rows.iter().position(|r| r.id == a))
                    .unwrap_or(i);
                let j = rows.iter().position(|r| r.id == next).unwrap_or(i);
                let ids = if a <= j {
                    rows[a..=j].iter().map(|r| r.id).collect()
                } else {
                    rows[j..=a].iter().rev().map(|r| r.id).collect()
                };
                NavOutcome::Select {
                    ids,
                    anchor: anchor.filter(|a| selection.contains(a)).unwrap_or(cur),
                }
            } else {
                NavOutcome::Select {
                    ids: vec![next],
                    anchor: next,
                }
            }
        }
        NavKey::Left => {
            if row.has_kids && !filtering && expanded.contains(&cur) {
                NavOutcome::Toggle(cur)
            } else if let Some(p) = parent_of(cur).filter(|p| *p != page) {
                NavOutcome::Select {
                    ids: vec![p],
                    anchor: p,
                }
            } else {
                NavOutcome::None
            }
        }
        NavKey::Right => {
            if row.has_kids && !filtering {
                if !expanded.contains(&cur) {
                    NavOutcome::Toggle(cur)
                } else {
                    match rows.get(i + 1).filter(|r| r.depth == row.depth + 1) {
                        Some(c) => NavOutcome::Select {
                            ids: vec![c.id],
                            anchor: c.id,
                        },
                        None => NavOutcome::None,
                    }
                }
            } else {
                NavOutcome::None
            }
        }
    }
}

/// Expand or collapse `id` and everything inside it (Alt-click on a disclosure triangle).
///
/// figcraft's `layers::set_expanded_deep`. Iterative rather than recursive, so a deep tree can't
/// blow the stack.
pub fn set_expanded_deep(doc: &Document, id: NodeId, open: bool, expanded: &mut BTreeSet<NodeId>) {
    let mut stack = vec![id];
    while let Some(n) = stack.pop() {
        if open {
            expanded.insert(n);
        } else {
            expanded.remove(&n);
        }
        if let Some(node) = doc.node(n) {
            stack.extend(node.children.iter().copied());
        }
    }
}

/// The rows a selection implies being open: every ancestor of a selected layer, so a reveal
/// actually shows it. figcraft does this in `layers()` under `reveal`.
pub fn ancestors_to_expand(doc: &Document, page: NodeId, selection: &[NodeId]) -> Vec<NodeId> {
    let mut out: Vec<NodeId> = Vec::new();
    for s in selection {
        for a in doc.ancestors(*s) {
            if a != page && !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out
}

/// What a drag of row `dragging` moves: the selection when the row is in it, else the row.
///
/// figcraft's `layers::dragged_layers`.
pub fn dragged_layers(dragging: Option<NodeId>, selection: &[NodeId]) -> Vec<NodeId> {
    match dragging {
        Some(dg) if selection.contains(&dg) => selection.to_vec(),
        Some(dg) => vec![dg],
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Text editing
// ---------------------------------------------------------------------------

/// The one-line editor behind the search box, the inline rename and the file name.
///
/// There is no `Input` element in the pinned GPUI revision, so this is the buffer and the handful
/// of keys the three fields need, kept as pure functions so it can be tested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextField {
    pub text: String,
    /// Caret as a char index into `text`.
    pub caret: usize,
    /// The existing text is selected: the next keystroke replaces it.
    pub select_all: bool,
}

impl TextField {
    /// An editor over `text` with its caret at the end. figcraft selects the whole name when a
    /// rename opens, so a rename starts with `select_all`.
    pub fn new(text: impl Into<String>, select_all: bool) -> Self {
        let text = text.into();
        let caret = text.chars().count();
        Self {
            text,
            caret,
            select_all,
        }
    }

    /// Insert a typed character, replacing the selection if there is one.
    pub fn insert(&mut self, c: char) {
        self.replace_selection();
        let at = self.byte_at(self.caret);
        self.text.insert(at, c);
        self.caret += 1;
    }

    /// Delete the character before the caret, or the selection.
    pub fn backspace(&mut self) {
        if self.select_all {
            self.replace_selection();
            return;
        }
        if self.caret == 0 {
            return;
        }
        let at = self.byte_at(self.caret - 1);
        self.text.remove(at);
        self.caret -= 1;
    }

    /// Delete the character at the caret, or the selection.
    pub fn delete(&mut self) {
        if self.select_all {
            self.replace_selection();
            return;
        }
        let at = self.byte_at(self.caret);
        if at < self.text.len() {
            self.text.remove(at);
        }
    }

    pub fn move_caret(&mut self, delta: isize) {
        let n = self.text.chars().count() as isize;
        self.select_all = false;
        self.caret = (self.caret as isize + delta).clamp(0, n) as usize;
    }

    pub fn home(&mut self) {
        self.select_all = false;
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.select_all = false;
        self.caret = self.text.chars().count();
    }

    /// Put the caret where a click landed, at `char_pos` characters into the visible text.
    pub fn click_at(&mut self, char_pos: usize) {
        self.select_all = false;
        self.caret = char_pos.min(self.text.chars().count());
    }

    fn byte_at(&self, caret: usize) -> usize {
        self.text
            .char_indices()
            .nth(caret)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }

    fn replace_selection(&mut self) {
        if !self.select_all {
            return;
        }
        self.select_all = false;
        self.text.clear();
        self.caret = 0;
    }
}

// ---------------------------------------------------------------------------
// Panel state
// ---------------------------------------------------------------------------

/// Which rail item the left panel is showing. figcraft's `LeftTab`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeftTab {
    File,
    Assets,
    Variables,
}

/// A layer being renamed in its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rename {
    pub id: NodeId,
    /// The rename just started: its field takes the keyboard next frame.
    pub start: bool,
}

/// A press on a row's eye or padlock dragged across other rows: every row passed gets the state
/// the first one switched to, in one step on release.
#[derive(Clone, Debug, PartialEq)]
pub struct Sweep {
    /// `true` for the padlock, `false` for the eye.
    pub lock: bool,
    /// Hidden for the eye, locked for the padlock.
    pub on: bool,
    pub ids: Vec<NodeId>,
}

/// What is being dragged out of the panel.
#[derive(Clone, Debug, PartialEq)]
pub enum Dragging {
    /// Layer rows: the ids that move, the row the press started on, and the press point.
    Layers { ids: Vec<NodeId>, from: usize },
    /// A page row being reordered.
    Pages { id: NodeId, from: usize },
}

/// A press that has not yet travelled far enough to be a drag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PendingPress {
    pub origin: (f32, f32),
}

/// Where a list's viewport was last measured, in window coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Viewport {
    /// The row index under a pointer at window position `(x, y)`, given the list's scroll offset.
    pub fn row_at(&self, x: f32, y: f32, offset: f32, len: usize) -> Option<usize> {
        if self.width <= 0.0 || x < self.x || x > self.x + self.width {
            return None;
        }
        let rel = y - self.y + offset;
        if rel < 0.0 {
            return None;
        }
        let i = (rel / PITCH) as usize;
        if i < len {
            Some(i)
        } else {
            None
        }
    }

    /// Where in its row a pointer at window `y` is, as 0 at the row's top edge and 1 at its bottom.
    pub fn row_rel(&self, y: f32, offset: f32) -> f32 {
        let rel = y - self.y + offset;
        if !rel.is_finite() || rel < 0.0 {
            return 0.0;
        }
        (rel / PITCH).fract().clamp(0.0, 1.0)
    }
}

/// One row of the pages list.
#[derive(Clone, Debug, PartialEq)]
pub struct PageRow {
    pub id: NodeId,
    pub name: String,
    /// A divider is a line between pages: it opens nothing and takes no layers.
    pub divider: bool,
}

/// The panel's own state, split out from the view so the windowing and the row projection can be
/// exercised without a `Window`. Everything in here is the editor's, not the document's.
#[derive(Default)]
pub struct PanelState {
    pub left_tab: Option<LeftTab>,
    pub pages_open: bool,
    pub layer_search: bool,
    pub expanded: BTreeSet<NodeId>,
    /// The page whose top-level layers were last opened, so a new document
    /// opens open.
    pub expanded_page: Option<NodeId>,
    pub rename: Option<Rename>,
    /// The rename buffer, for the row being renamed.
    pub rename_field: Option<TextField>,
    /// The search buffer.
    pub filter_field: Option<TextField>,
    /// The file-name buffer.
    pub file_rename: Option<TextField>,
    /// The page being renamed, and its buffer.
    pub rename_page: Option<NodeId>,
    pub page_rename_field: Option<TextField>,
    /// Where a range selection runs from.
    pub anchor: Option<NodeId>,
    /// The selection the rows were last revealed for.
    pub seen_sel: Vec<NodeId>,
    /// `None` until the first probe reports; then the measured box.
    pub viewport: Option<Viewport>,
    pub pages_viewport: Option<Viewport>,
    pub offset: f32,
    pub pages_offset: f32,
    pub drag: Option<Dragging>,
    pub pending: Option<PendingPress>,
    pub sweep: Option<Sweep>,
    pub hover: Option<NodeId>,
    /// A collapsed container held over during a drag, and when the hold started.
    pub spring_loaded: Option<(NodeId, Instant)>,
    /// The row the drag is currently pointed into, when it takes the layer.
    pub drop_into: Option<NodeId>,
    /// The drag is over a row that takes no drop: the not-allowed cursor shows.
    pub refused: bool,
    /// A timer is running (a spring-loaded hold), so the panel needs another frame.
    pub hold_refresh: bool,
    /// What a context menu was opened on.
    pub context: Option<ContextTarget>,
}

/// What a context menu is for.
#[derive(Clone, Debug, PartialEq)]
pub enum ContextTarget {
    Layer(NodeId),
    Page(NodeId),
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// Icons rendered to SVG bytes so far, keyed by icon. GPUI keys the rendered tile by a hash of
/// the bytes, so an icon is rasterised once and then tinted per element.
#[derive(Default)]
pub struct IconCache {
    bytes: HashMap<LayerIcon, Arc<[u8]>>,
}

/// The Layers panel: pages on top, the virtualised layer tree below, an icon rail down the side.
///
/// The engine is held as `Rc<RefCell<Session>>`, the same handle
/// [`EngineCanvas`](crate::engine::canvas::EngineCanvas) hands out through
/// [`session_handle`](crate::engine::canvas::EngineCanvas::session_handle), so one editor has one
/// session. Every borrow here is statement-scoped: read the value out, drop the borrow, then act,
/// so no borrow is ever held across a call that borrows again.
pub struct LayersPanel {
    /// The engine, shared with the canvas. Read through `borrow()`, changed through `borrow_mut()`.
    session: Rc<RefCell<Session>>,
    state: PanelState,
    icons: IconCache,
    cache: RowCache,
    /// The rows of the frame being rendered, kept so the event handlers can map a pointer back
    /// to a layer without walking the tree again.
    cached_rows: Arc<Vec<Row>>,
    focus: FocusHandle,
    /// Where the layers viewport was, written by the probe element during the last frame's paint.
    probe_layers: Rc<Cell<Option<Viewport>>>,
    probe_pages: Rc<Cell<Option<Viewport>>>,
}

impl LayersPanel {
    /// A panel over a session handle something else owns — normally
    /// [`EngineCanvas::session_handle`](crate::engine::canvas::EngineCanvas::session_handle).
    /// Everything it shows comes from the session; everything it changes goes through it.
    pub fn new(session: Rc<RefCell<Session>>, cx: &mut Context<Self>) -> Self {
        Self {
            session,
            state: PanelState {
                left_tab: Some(LeftTab::File),
                pages_open: true,
                ..PanelState::default()
            },
            icons: IconCache::default(),
            cache: RowCache::default(),
            cached_rows: Arc::new(Vec::new()),
            focus: cx.focus_handle(),
            probe_layers: Rc::new(Cell::new(None)),
            probe_pages: Rc::new(Cell::new(None)),
        }
    }

    /// The session handle the panel reads and acts through. Same shape as
    /// [`EngineCanvas::session_handle`](crate::engine::canvas::EngineCanvas::session_handle), so
    /// the two agree on which engine they are talking to.
    pub fn session(&self) -> Rc<RefCell<Session>> {
        Rc::clone(&self.session)
    }

    /// The engine's own count of edits to the open document.
    ///
    /// Read from the shared session, so it agrees with
    /// [`crate::engine::canvas::EngineCanvas::document_revision`] by
    /// construction rather than by the two panels agreeing to stay in step.
    pub fn document_revision(&self) -> u64 {
        let session = self.session.borrow();
        session
            .docs
            .get(session.active)
            .map_or(0, |doc| doc.revision)
    }

    /// Replace the session handle, e.g. when a project is opened.
    pub fn set_session(&mut self, session: Rc<RefCell<Session>>, cx: &mut Context<Self>) {
        self.session = session;
        self.cache = RowCache::default();
        self.state.seen_sel.clear();
        self.state.offset = 0.0;
        cx.notify();
    }

    /// The panel's keyboard focus handle.
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The layers the panel is listing, the box it is listing them in, and how
    /// far the list is scrolled.
    ///
    /// For the window-level tests: the rows are what the panel has built, and
    /// the viewport is the box it drew them in, so a test can ask which layer
    /// a pointer at a given place would land on.
    #[cfg(test)]
    pub fn test_listing(&self) -> (Vec<NodeId>, Option<Viewport>, f32) {
        (
            self.cached_rows.iter().map(|row| row.id).collect(),
            self.state.viewport,
            self.state.offset,
        )
    }

    /// Run an engine command. The only way this panel changes the document.
    fn run(&mut self, id: &str, params: Value) -> bool {
        self.session.borrow_mut().execute(id, &params).is_ok()
    }

    /// The active document, cloned so it can be read without holding the session borrowed.
    fn doc_arc(&self) -> Option<Arc<Document>> {
        self.session.borrow().ds().ok().map(|ds| ds.doc.clone())
    }

    /// The rows as listed. Cached: nothing here re-walks the tree when nothing changed.
    fn current_rows(&mut self) -> Arc<Vec<Row>> {
        let Some(doc) = self.doc_arc() else {
            return Arc::new(Vec::new());
        };
        let Ok(page) = self.session.borrow().page() else {
            return Arc::new(Vec::new());
        };
        // A NEW DOCUMENT OPENS WITH ITS TOP-LEVEL LAYERS OPEN.
        //
        // The rows are collected from the page downwards, and a layer's
        // children are listed only if that layer is in the expanded set. A
        // freshly opened project had an empty expanded set, so the panel listed
        // the page's top-level frames and nothing inside them: the project's
        // own layers were missing, and the panel looked broken.
        //
        // This fires on a PAGE CHANGE and not on every frame, so "Collapse
        // layers" still works -- that command clears the set, and the panel
        // must not immediately undo it.
        if self.state.expanded_page != Some(page) {
            self.state.expanded_page = Some(page);
            self.state.expanded.clear();
            if let Some(node) = doc.node(page) {
                for child in &node.children {
                    self.state.expanded.insert(*child);
                }
            }
        }
        let filter = self.filter();
        let expanded = std::mem::take(&mut self.state.expanded);
        let rows = self.cache.rows(&doc, page, &expanded, &filter);
        self.state.expanded = expanded;
        rows
    }

    /// The lowercased search text.
    fn filter(&self) -> String {
        self.state
            .filter_field
            .as_ref()
            .map(|f| f.text.trim().to_lowercase())
            .unwrap_or_default()
    }

    /// The box the pages list is drawn in, measured by its probe.
    ///
    /// The fallback is the box the list is given before the probe has measured
    /// one, so that the first frame has a list to draw rather than none.
    fn pages_viewport(&self) -> Viewport {
        self.state.pages_viewport.unwrap_or(Viewport {
            x: RAIL_W,
            y: 0.0,
            width: 240.0,
            height: 96.0,
        })
    }

    /// The height of everything above the layer list: the file header, the
    /// pages section, the layers header and the search box.
    ///
    /// The layer list is laid out as an absolute box under the chrome rather
    /// than as a flexible child of it, so this is the number that says where
    /// the list starts. It is the height the chrome is actually drawn at, so
    /// the list starts exactly where the chrome ends.
    ///
    /// It is measured rather than flexed because a flexible child does not
    /// grow: `flex: 1` asks the layout for the space that is left over, and
    /// this layout does not hand it over, so a list laid out that way is drawn
    /// with no height at all and its rows are clipped away.
    fn chrome_height(&self) -> f32 {
        let pages = self.pages_viewport();
        let search = if self.state.layer_search {
            SEARCH_H
        } else {
            0.0
        };
        FILE_HEADER_H + (HEADER_H + pages.height + 1.0) + HEADER_H + search
    }

    /// The pages as listed: `(id, name, divider)`.
    pub fn pages(&self) -> Vec<PageRow> {
        // The one place the borrow is held across a block rather than a statement: every row is
        // read out of the document, so nothing here borrows the session again.
        let session = self.session.borrow();
        let Ok(doc) = session.doc() else {
            return Vec::new();
        };
        doc.pages
            .iter()
            .filter_map(|p| {
                doc.node(*p)
                    .map(|n| (*p, n.name.clone(), doc.is_divider(*p)))
            })
            .map(|(id, name, divider)| PageRow { id, name, divider })
            .collect()
    }

    // --- engine UI requests -------------------------------------------------

    /// Consume the engine's requests for this panel: `layers.rename` (a rename asked for from the
    /// keyboard or a menu) and `layers.collapseAll`.
    fn drain_ui_requests(&mut self) {
        // Taken in one statement so the mutable borrow is gone before any of the handlers below
        // read or change the session again.
        let requests = std::mem::take(&mut self.session.borrow_mut().ui_requests);
        for r in requests {
            match r.kind.as_str() {
                "layers.rename" => {
                    let ids: Vec<NodeId> = r
                        .data
                        .get("ids")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_u64()).map(NodeId).collect())
                        .unwrap_or_default();
                    self.start_rename(&ids);
                }
                "layers.collapseAll" => self.collapse_all(),
                // Requests for other surfaces (dialogs, files) are put back untouched rather than
                // dropped: another panel owns them and losing one would hang the engine.
                _ => self.session.borrow_mut().ui_requests.push(r),
            }
        }
    }

    /// Start renaming the topmost listed of `ids`, which becomes the selection. Shows the panel
    /// and opens the rows above the layer. figcraft's `layers::start_rename`.
    pub fn start_rename(&mut self, ids: &[NodeId]) {
        let Some(doc) = self.doc_arc() else { return };
        let Ok(page) = self.session.borrow().page() else {
            return;
        };
        for a in ancestors_to_expand(&doc, page, ids) {
            self.state.expanded.insert(a);
        }
        let rows = self.current_rows();
        let Some(id) = rows.iter().map(|r| r.id).find(|r| ids.contains(r)) else {
            return;
        };
        self.state.left_tab = Some(LeftTab::File);
        self.rename_layer(id);
    }

    /// Collapse layers: every row closes except those leading to the selection. figcraft's
    /// `layers::collapse_all`.
    pub fn collapse_all(&mut self) {
        self.state.expanded.clear();
        let Some(doc) = self.doc_arc() else { return };
        let Ok(page) = self.session.borrow().page() else {
            return;
        };
        let sel = self.session.borrow().selection();
        for a in ancestors_to_expand(&doc, page, &sel) {
            self.state.expanded.insert(a);
        }
    }

    /// Open `id`'s row for renaming, selected. figcraft's `layers::rename_layer`.
    fn rename_layer(&mut self, id: NodeId) {
        let Some(doc) = self.doc_arc() else { return };
        let Some(name) = doc.node(id).map(|n| n.name.clone()) else {
            return;
        };
        self.state.rename = Some(Rename { id, start: true });
        self.state.rename_field = Some(TextField::new(name, true));
        if self.session.borrow().selection() != [id] {
            self.run("select.set", json!({"ids": [id]}));
        }
        self.state.anchor = Some(id);
        // The rename scrolls its own row into view.
        self.state.seen_sel = self.session.borrow().selection();
    }

    /// Open a page's name for editing (double click or "Rename page").
    fn start_page_rename(&mut self, id: NodeId) {
        let name = self
            .session
            .borrow()
            .doc()
            .ok()
            .and_then(|d| d.node(id))
            .map(|n| n.name.clone())
            .unwrap_or_default();
        self.state.rename_page = Some(id);
        self.state.page_rename_field = Some(TextField::new(name, true));
    }

    // --- selection ----------------------------------------------------------

    /// Apply a selection mode to `id` and record the anchor.
    fn select(&mut self, id: NodeId, mode: SelectMode) {
        let sel = self.session.borrow().selection();
        match mode {
            SelectMode::Replace => {
                self.state.anchor = Some(id);
                self.run("select.set", json!({"ids": [id]}));
            }
            SelectMode::Toggle => {
                let mut s = sel;
                if let Some(pos) = s.iter().position(|x| *x == id) {
                    s.remove(pos);
                } else {
                    s.push(id);
                }
                self.state.anchor = Some(id);
                self.run("select.set", json!({"ids": s}));
            }
            SelectMode::Range => {
                let rows = self.current_rows();
                let order: Vec<NodeId> = rows.iter().map(|r| r.id).collect();
                let pos = |x: NodeId| order.iter().position(|o| *o == x);
                let (Some(ai), Some(bi)) = (self.state.anchor.and_then(pos), pos(id)) else {
                    self.select(id, SelectMode::Toggle);
                    return;
                };
                let (lo, hi) = (ai.min(bi), ai.max(bi));
                let mut s = sel;
                for x in order.get(lo..=hi).unwrap_or_default() {
                    if !s.contains(x) {
                        s.push(*x);
                    }
                }
                self.run("select.set", json!({"ids": s}));
            }
        }
        // A click in the panel needs no reveal scroll.
        self.state.seen_sel = self.session.borrow().selection();
        // Editing text on the canvas ends when a layer is picked out of the panel.
        let editing = self
            .session
            .borrow()
            .ds()
            .map(|ds| ds.text_edit.is_some())
            .unwrap_or(false);
        if editing {
            let _ = figcraft_engine::textedit::end(&mut self.session.borrow_mut());
        }
    }

    // --- presses ------------------------------------------------------------

    /// Handle a press on a row. `hit` says which control.
    fn press_row(
        &mut self,
        id: NodeId,
        hit: RowHit,
        click_count: usize,
        m: gpui::Modifiers,
        deep: bool,
    ) {
        let rows = self.current_rows();
        let listed = rows.iter().any(|r| r.id == id);
        let anchor_listed = self
            .state
            .anchor
            .is_some_and(|a| rows.iter().any(|r| r.id == a));
        let action = row_action(
            hit,
            click_count,
            m.shift,
            m.platform || m.control,
            listed,
            anchor_listed,
        );
        self.state.context = None;
        match action {
            RowAction::None => {}
            RowAction::Select { mode, .. } => self.select(id, mode),
            RowAction::Rename { .. } => self.rename_layer(id),
            RowAction::ToggleVisible { .. } => {
                let visible = self
                    .session
                    .borrow()
                    .doc()
                    .ok()
                    .and_then(|d| d.node(id))
                    .map(|n| n.visible)
                    .unwrap_or(true);
                self.run(
                    "object.toggleVisible",
                    json!({"ids": [id], "visible": !visible}),
                );
            }
            RowAction::ToggleLock { .. } => {
                let locked = self
                    .session
                    .borrow()
                    .doc()
                    .ok()
                    .and_then(|d| d.node(id))
                    .map(|n| n.locked)
                    .unwrap_or(false);
                self.run("object.toggleLock", json!({"ids": [id], "locked": !locked}));
            }
            RowAction::ToggleExpanded { deep: d, .. } => {
                let open = self.state.expanded.contains(&id);
                let deep = deep || d;
                if deep {
                    if let Some(doc) = self.doc_arc() {
                        set_expanded_deep(&doc, id, !open, &mut self.state.expanded);
                    }
                } else if open {
                    self.state.expanded.remove(&id);
                } else {
                    self.state.expanded.insert(id);
                }
            }
        }
    }

    /// A press on a page row.
    fn press_page(&mut self, id: NodeId, click_count: usize) {
        if click_count >= 2 {
            self.start_page_rename(id);
        }
    }

    // --- drag and drop ------------------------------------------------------

    /// Pointer moved over the panel: start a drag, track the drop, sweep the eye or padlock,
    /// scroll at an edge.
    fn pointer_move(&mut self, x: f32, y: f32) {
        // A sweep across the eyes or padlocks takes in the rows the pointer passes.
        if self.state.sweep.is_some() {
            self.extend_sweep(x, y);
            return;
        }
        // A press that has travelled far enough becomes a drag.
        if let Some(p) = self.state.pending {
            let travelled = ((x - p.origin.0).powi(2) + (y - p.origin.1).powi(2)).sqrt();
            if self.state.drag.is_none() && travelled >= DRAG_THRESHOLD {
                self.begin_drag(x, y);
            }
        }
        if self.state.drag.is_some() {
            self.track_drag(x, y);
        }
    }

    /// Turn the pending press into a drag of whatever it started on.
    fn begin_drag(&mut self, x: f32, y: f32) {
        let vp = self.state.viewport.unwrap_or_default();
        let rows = self.current_rows();
        if let Some(i) = vp.row_at(x, y, self.state.offset, rows.len()) {
            let Some(row) = rows.get(i) else { return };
            let sel = self.session.borrow().selection();
            let ids = dragged_layers(Some(row.id), &sel);
            self.state.drag = Some(Dragging::Layers { ids, from: i });
            self.state.pending = None;
            return;
        }
        let pov = self.state.pages_viewport.unwrap_or_default();
        let pages = self.pages();
        if let Some((i, p)) = pov
            .row_at(x, y, self.state.pages_offset, pages.len())
            .and_then(|i| pages.get(i).map(|p| (i, p)))
        {
            self.state.drag = Some(Dragging::Pages { id: p.id, from: i });
            self.state.pending = None;
        }
    }

    /// Follow the pointer: the drop zone under it, the spring-loaded expand, the edge scroll.
    fn track_drag(&mut self, x: f32, y: f32) {
        if !matches!(self.state.drag, Some(Dragging::Layers { .. })) {
            return;
        }
        let vp = self.state.viewport.unwrap_or_default();
        self.auto_scroll(y, vp);
        let rows = self.current_rows();
        let Some(i) = vp.row_at(x, y, self.state.offset, rows.len()) else {
            self.state.drop_into = None;
            self.state.spring_loaded = None;
            return;
        };
        let Some(row) = rows.get(i).cloned() else {
            return;
        };
        let Some(doc) = self.doc_arc() else { return };
        let Some(node) = doc.node(row.id) else { return };
        // A layer can't go into itself or its own descendants.
        let moving = match &self.state.drag {
            Some(Dragging::Layers { ids, .. }) => ids.clone(),
            _ => Vec::new(),
        };
        if moving.contains(&row.id) || doc.ancestors(row.id).iter().any(|a| moving.contains(a)) {
            self.state.drop_into = None;
            self.state.spring_loaded = None;
            self.state.refused = true;
            return;
        }
        self.state.refused = false;
        let rel = vp.row_rel(y, self.state.offset);
        let zone = layer_drop_zone(
            rel,
            node.kind.is_container(),
            matches!(node.kind, NodeKind::Instance { .. }),
        );
        self.state.drop_into = if zone == 0 { Some(row.id) } else { None };
        // Holding over a collapsed group or frame opens it, to drop deeper inside.
        if zone == 0 && row.has_kids && !self.state.expanded.contains(&row.id) {
            match self.state.spring_loaded {
                Some((id, since)) if id == row.id => {
                    if since.elapsed().as_secs_f32() > SPRING_LOADED_SECS {
                        self.state.expanded.insert(row.id);
                        self.state.spring_loaded = None;
                    } else {
                        self.state.hold_refresh = true;
                    }
                }
                _ => {
                    self.state.spring_loaded = Some((row.id, Instant::now()));
                    self.state.hold_refresh = true;
                }
            }
        } else {
            self.state.spring_loaded = None;
        }
    }

    /// Scroll the layers list by `delta` points, clamped to the list.
    fn scroll_by(&mut self, delta: f32) {
        let vp = self.state.viewport.unwrap_or_default();
        let total = self.current_rows().len() as f32 * PITCH;
        self.state.offset = (self.state.offset + delta).clamp(0.0, (total - vp.height).max(0.0));
    }

    /// The edge auto-scroll a drag near the top or bottom of the viewport gets.
    fn auto_scroll(&mut self, y: f32, vp: Viewport) {
        let dy = edge_scroll_delta(y - vp.y, vp.height);
        if dy != 0.0 {
            self.scroll_by(dy);
            self.state.hold_refresh = true;
        }
    }

    /// Take in the rows a sweeping eye or padlock passes over.
    fn extend_sweep(&mut self, x: f32, y: f32) {
        let vp = self.state.viewport.unwrap_or_default();
        let rows = self.current_rows();
        let Some(i) = vp.row_at(x, y, self.state.offset, rows.len()) else {
            return;
        };
        let Some(id) = rows.get(i).map(|r| r.id) else {
            return;
        };
        if let Some(sw) = self.state.sweep.as_mut() {
            if !sw.ids.contains(&id) {
                sw.ids.push(id);
            }
        }
        self.auto_scroll(y, vp);
    }

    /// The pointer was let go: finish the sweep, the drag, or nothing.
    fn pointer_up(&mut self, x: f32, y: f32) {
        if let Some(sw) = self.state.sweep.take() {
            let (cmd, key) = if sw.lock {
                ("object.toggleLock", "locked")
            } else {
                ("object.toggleVisible", "visible")
            };
            let value = if sw.lock { sw.on } else { !sw.on };
            self.run(cmd, json!({"ids": sw.ids, key: value}));
            self.state.pending = None;
            return;
        }
        let Some(drag) = self.state.drag.take() else {
            self.state.pending = None;
            return;
        };
        self.state.spring_loaded = None;
        self.state.refused = false;
        self.state.drop_into = None;
        self.state.pending = None;
        match drag {
            Dragging::Layers { ids, .. } => self.drop_layers(&ids, x, y),
            Dragging::Pages { id, .. } => self.drop_page(id, x, y),
        }
    }

    /// Where the dragged layers land.
    fn drop_layers(&mut self, ids: &[NodeId], x: f32, y: f32) {
        // A release over a page row moves the layers to that page.
        let pov = self.state.pages_viewport.unwrap_or_default();
        let pages = self.pages();
        let cur = self.session.borrow().page().ok();
        if let Some(p) = pov
            .row_at(x, y, self.state.pages_offset, pages.len())
            .and_then(|i| pages.get(i))
            .filter(|p| !p.divider && Some(p.id) != cur)
        {
            self.run("object.moveToPage", json!({"ids": ids, "page": p.id}));
            return;
        }
        let vp = self.state.viewport.unwrap_or_default();
        let rows = self.current_rows();
        let Some(i) = vp.row_at(x, y, self.state.offset, rows.len()) else {
            return;
        };
        let Some(row) = rows.get(i).cloned() else {
            return;
        };
        let Some(doc) = self.doc_arc() else { return };
        let Some(node) = doc.node(row.id) else { return };
        let rel = vp.row_rel(y, self.state.offset);
        let zone = layer_drop_zone(
            rel,
            node.kind.is_container(),
            matches!(node.kind, NodeKind::Instance { .. }),
        );
        match zone {
            0 => {
                self.run("node.reparent", json!({"ids": ids, "parent": row.id}));
            }
            z => {
                // Above in the list = higher z = after the target among siblings.
                let Some(parent) = node.parent else { return };
                let idx = doc.index_in_parent(row.id).unwrap_or(0);
                let index = if z < 0 { idx + 1 } else { idx };
                self.run(
                    "node.reparent",
                    json!({"ids": ids, "parent": parent, "index": index}),
                );
            }
        }
    }

    /// Where the dragged page lands.
    fn drop_page(&mut self, id: NodeId, x: f32, y: f32) {
        let pov = self.state.pages_viewport.unwrap_or_default();
        let pages = self.pages();
        let Some(i) = pov.row_at(x, y, self.state.pages_offset, pages.len()) else {
            return;
        };
        let n = pages.len();
        let from = pages.iter().position(|p| p.id == id).unwrap_or(0);
        let over = pov.row_rel(y, self.state.pages_offset) >= 0.5;
        if let Some(to) = drop_slot(from, i, over, n) {
            self.run("page.move", json!({"id": id, "index": to}));
        }
    }

    // --- keyboard -----------------------------------------------------------

    /// Handle a key. Returns whether it was used.
    ///
    /// figcraft dispatches these from the app's key handler, where they live next to the canvas
    /// shortcuts. Here the panel owns them, because the panel owns the focus.
    pub fn handle_keystroke(&mut self, ks: &Keystroke, cx: &mut Context<Self>) -> bool {
        let key = ks.key.clone();
        let shift = ks.modifiers.shift;
        let cmd = ks.modifiers.control || ks.modifiers.platform;
        // An open field has the keyboard.
        if self.state.rename_field.is_some() {
            return self.handle_rename_key(&key, shift, ks, cx);
        }
        if self.state.page_rename_field.is_some() {
            return self.handle_page_rename_key(&key, ks, cx);
        }
        if self.state.filter_field.is_some() {
            return self.handle_filter_key(&key, shift, cmd, ks, cx);
        }
        if self.state.file_rename.is_some() {
            return self.handle_file_rename_key(&key, ks, cx);
        }
        // Rename the selection: Cmd/Ctrl+R, as figcraft's shortcut.
        if cmd && key == "r" {
            let ids = self.session.borrow().selection();
            self.start_rename(&ids);
            cx.notify();
            return true;
        }
        let nav = match key.as_str() {
            "up" => Some(NavKey::Up),
            "down" => Some(NavKey::Down),
            "left" => Some(NavKey::Left),
            "right" => Some(NavKey::Right),
            _ => None,
        };
        let Some(nav) = nav else { return false };
        self.navigate(nav, shift, cx);
        true
    }

    fn handle_rename_key(
        &mut self,
        key: &str,
        shift: bool,
        ks: &Keystroke,
        cx: &mut Context<Self>,
    ) -> bool {
        match key {
            "enter" | "escape" | "tab" => {
                let Some(rn) = self.state.rename.take() else {
                    return false;
                };
                let field = self.state.rename_field.take();
                if key != "escape" {
                    if let Some(field) = field {
                        self.run("object.rename", json!({"ids": [rn.id], "name": field.text}));
                    }
                }
                // Tab / Shift+Tab commit and go on to the next / previous row.
                if key == "tab" {
                    let rows = self.current_rows();
                    let i = rows.iter().position(|r| r.id == rn.id);
                    let target = match (shift, i) {
                        (true, Some(i)) if i > 0 => rows.get(i - 1).map(|r| r.id),
                        (false, Some(i)) => rows.get(i + 1).map(|r| r.id),
                        _ => None,
                    };
                    if let Some(t) = target {
                        self.rename_layer(t);
                    }
                }
                cx.notify();
                return true;
            }
            "backspace" => {
                if let Some(f) = self.state.rename_field.as_mut() {
                    f.backspace();
                }
            }
            "delete" => {
                if let Some(f) = self.state.rename_field.as_mut() {
                    f.delete();
                }
            }
            "left" => {
                if let Some(f) = self.state.rename_field.as_mut() {
                    f.move_caret(-1);
                }
            }
            "right" => {
                if let Some(f) = self.state.rename_field.as_mut() {
                    f.move_caret(1);
                }
            }
            "home" => {
                if let Some(f) = self.state.rename_field.as_mut() {
                    f.home();
                }
            }
            "end" => {
                if let Some(f) = self.state.rename_field.as_mut() {
                    f.end();
                }
            }
            _ => {
                if !ks.modifiers.control && !ks.modifiers.platform {
                    if let (Some(text), Some(f)) =
                        (ks.key_char.as_deref(), self.state.rename_field.as_mut())
                    {
                        for c in text.chars() {
                            f.insert(c);
                        }
                    }
                }
            }
        }
        cx.notify();
        true
    }

    fn handle_page_rename_key(
        &mut self,
        key: &str,
        ks: &Keystroke,
        cx: &mut Context<Self>,
    ) -> bool {
        match key {
            "enter" | "escape" => {
                let id = self.state.rename_page.take();
                let field = self.state.page_rename_field.take();
                if key == "enter" {
                    if let (Some(id), Some(field)) = (id, field) {
                        self.run("page.rename", json!({"id": id, "name": field.text}));
                    }
                }
                cx.notify();
                return true;
            }
            "backspace" => {
                if let Some(f) = self.state.page_rename_field.as_mut() {
                    f.backspace();
                }
            }
            _ => {
                if !ks.modifiers.control && !ks.modifiers.platform {
                    if let (Some(text), Some(f)) = (
                        ks.key_char.as_deref(),
                        self.state.page_rename_field.as_mut(),
                    ) {
                        for c in text.chars() {
                            f.insert(c);
                        }
                    }
                }
            }
        }
        cx.notify();
        true
    }

    fn handle_filter_key(
        &mut self,
        key: &str,
        _shift: bool,
        cmd: bool,
        ks: &Keystroke,
        cx: &mut Context<Self>,
    ) -> bool {
        if cmd && key == "r" {
            return false;
        }
        match key {
            "escape" | "enter" => {
                self.state.filter_field = None;
                self.state.layer_search = false;
                cx.notify();
                return true;
            }
            "backspace" => {
                if let Some(f) = self.state.filter_field.as_mut() {
                    f.backspace();
                }
            }
            "left" => {
                if let Some(f) = self.state.filter_field.as_mut() {
                    f.move_caret(-1);
                }
            }
            "right" => {
                if let Some(f) = self.state.filter_field.as_mut() {
                    f.move_caret(1);
                }
            }
            "down" | "up" => return false,
            _ => {
                if !ks.modifiers.control && !ks.modifiers.platform {
                    if let (Some(text), Some(f)) =
                        (ks.key_char.as_deref(), self.state.filter_field.as_mut())
                    {
                        for c in text.chars() {
                            f.insert(c);
                        }
                    }
                }
            }
        }
        cx.notify();
        true
    }

    fn handle_file_rename_key(
        &mut self,
        key: &str,
        ks: &Keystroke,
        cx: &mut Context<Self>,
    ) -> bool {
        match key {
            "enter" | "escape" => {
                let field = self.state.file_rename.take();
                if key == "enter" {
                    if let Some(field) = field {
                        self.run("file.rename", json!({"name": field.text}));
                    }
                }
                cx.notify();
                return true;
            }
            "backspace" => {
                if let Some(f) = self.state.file_rename.as_mut() {
                    f.backspace();
                }
            }
            _ => {
                if !ks.modifiers.control && !ks.modifiers.platform {
                    if let (Some(text), Some(f)) =
                        (ks.key_char.as_deref(), self.state.file_rename.as_mut())
                    {
                        for c in text.chars() {
                            f.insert(c);
                        }
                    }
                }
            }
        }
        cx.notify();
        true
    }

    /// Move the selection with the arrow keys.
    fn navigate(&mut self, nav: NavKey, shift: bool, cx: &mut Context<Self>) {
        let Some(doc) = self.doc_arc() else { return };
        let Ok(page) = self.session.borrow().page() else {
            return;
        };
        let rows = self.current_rows();
        let filtering = !self.filter().is_empty();
        let expanded = std::mem::take(&mut self.state.expanded);
        let selection = self.session.borrow().selection();
        let anchor = self.state.anchor;
        let outcome = nav_step(
            &rows,
            &selection,
            anchor,
            nav,
            shift,
            &expanded,
            filtering,
            |id| doc.node(id).and_then(|n| n.parent),
            page,
        );
        self.state.expanded = expanded;
        match outcome {
            NavOutcome::Select { ids, anchor } => {
                self.state.anchor = Some(anchor);
                self.run("select.set", json!({"ids": ids}));
                self.state.seen_sel = self.session.borrow().selection();
            }
            NavOutcome::Toggle(id) => {
                if nav == NavKey::Right {
                    self.state.expanded.insert(id);
                } else {
                    self.state.expanded.remove(&id);
                }
            }
            NavOutcome::None => {}
        }
        cx.notify();
    }

    // --- rendering helpers --------------------------------------------------

    /// An icon element, rendered from the SVG bytes cached for `icon`.
    fn icon(&mut self, icon: LayerIcon, size_px: f32, color: u32) -> Svg {
        let bytes = self
            .icons
            .bytes
            .entry(icon)
            .or_insert_with(|| Arc::from(icon_svg(icon).into_bytes()))
            .clone();
        svg()
            .data(&bytes)
            .w(px(size_px))
            .h(px(size_px))
            .text_color(rgb(color))
            .flex_shrink_0()
    }

    /// The probe that records where a viewport was, so the next frame can window against it.
    ///
    /// GPUI gives a view no way to read its own laid-out box during `render`, and the row list
    /// cannot lay out rows without knowing how many of them fit. The probe is a zero-size canvas
    /// whose `prepaint` receives the box of the list it sits in; the next frame reads it back
    /// through the shared cell. One frame of lag, which is what every virtualised GPUI list does.
    fn probe(&self, slot: &Rc<Cell<Option<Viewport>>>) -> impl gpui::IntoElement {
        let slot = slot.clone();
        canvas(
            move |bounds, _window, _cx| {
                slot.set(Some(Viewport {
                    x: bounds.origin.x.into(),
                    y: bounds.origin.y.into(),
                    width: bounds.size.width.into(),
                    height: bounds.size.height.into(),
                }));
            },
            |_bounds, _t, _window, _cx| {},
        )
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        // THE LIST'S OWN SIZE, not zero.
        //
        // This probe exists to measure the box the rows are drawn in, so that
        // the rows can be windowed against it: only the rows on screen are
        // built, and a long tree does not cost a long list of elements.
        //
        // It measured a box of zero by zero, so the window was computed
        // against a height of zero and NO ROWS WERE DRAWN AT ALL. The panel
        // showed its chrome and an empty tree, and the rows were there --
        // `rows.len()` was 1 for a project with one frame in it.
        //
        // It is absolutely positioned so it does not take part in the list's
        // layout, and it is the list's FIRST child so the rows paint over it
        // and keep their own hit testing.
        .size_full()
    }

    /// Which control of a row a pointer at window `x` went down on, given how the panel lays the
    /// row out: an 8pt inset, then `INDENT` per depth, with the eye and padlock on the right.
    fn hit_for(&self, x: f32, depth: usize, has_kids: bool) -> RowHit {
        let vp = self.state.viewport.unwrap_or_default();
        row_hit_geometry(x - vp.x - 8.0, vp.width - 16.0, depth, has_kids)
    }
}

impl Render for LayersPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        self.drain_ui_requests();

        let (Some(doc), Ok(page)) = (self.doc_arc(), self.session.borrow().page()) else {
            return div()
                .flex_col()
                .size_full()
                .bg(rgb(theme::SURFACE))
                .into_any_element();
        };
        let selection = self.session.borrow().selection();
        let hover = self
            .state
            .hover
            .or_else(|| self.session.borrow().ds().ok().and_then(|ds| ds.hover));

        // Reveal a new selection: expand its ancestors and scroll its row into view, once, so
        // "Collapse layers" and manual collapsing stick. figcraft's `reveal`.
        let cached = self
            .cache
            .rows(&doc, page, &self.state.expanded, &self.filter());
        let rows_position = |id: NodeId| cached.iter().position(|r| r.id == id);
        let reveal = if selection != self.state.seen_sel {
            for a in ancestors_to_expand(&doc, page, &selection) {
                self.state.expanded.insert(a);
            }
            self.state.seen_sel = selection.clone();
            selection.first().and_then(|id| rows_position(*id))
        } else {
            None
        };

        let filter = self.filter();
        let rows = self.cache.rows(&doc, page, &self.state.expanded, &filter);
        self.cached_rows = rows.clone();
        let len = rows.len();

        // One repaint when a probe reports a box we had not seen.
        if self.probe_layers.get() != self.state.viewport {
            self.state.viewport = self.probe_layers.get();
            cx.notify();
        }
        if self.probe_pages.get() != self.state.pages_viewport {
            self.state.pages_viewport = self.probe_pages.get();
            cx.notify();
        }

        let layers_vp = self.state.viewport.unwrap_or(Viewport {
            x: RAIL_W,
            y: 0.0,
            width: 240.0,
            height: 480.0,
        });
        let total = len as f32 * PITCH;
        let offset = self
            .state
            .offset
            .clamp(0.0, (total - layers_vp.height).max(0.0));
        self.state.offset = offset;
        let band = visible_window(len, offset, layers_vp.height, PITCH);

        // A revealed selection, and an open rename, both want their row on screen.
        let want = reveal.or_else(|| {
            self.state
                .rename
                .and_then(|r| rows.iter().position(|x| x.id == r.id))
        });
        if let Some(i) = want.filter(|i| *i < len && !(band.start..band.end).contains(i)) {
            self.state.offset = scroll_to_row(i, layers_vp.height, PITCH, len);
        }
        // A spring-loaded hold or a held edge scroll needs another frame.
        let hold = self.state.hold_refresh;
        self.state.hold_refresh = false;

        let body = div()
            .flex_col()
            .flex_1()
            .min_w_0()
            .relative()
            .child(self.render_file_header(cx))
            .child(self.render_pages(page, cx))
            .child(self.render_layers_header(cx))
            .child(if self.state.layer_search {
                self.render_search(cx)
            } else {
                div().into_any_element()
            })
            .child(self.render_layers(&doc, page, &rows, &selection, hover, band, cx));

        // The panel fills the box it is given.
        //
        // The panel is mounted in the shell's left sidebar, after the shell's
        // own chrome, so it is laid out in the flow: the shell's chrome is
        // above it, and the panel takes the rest.
        let root = div()
            .id("layers_panel")
            .flex()
            .size_full()
            .bg(rgb(theme::SURFACE))
            .text_color(rgb(theme::TEXT))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _w, cx| {
                if this.handle_keystroke(&event.keystroke, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseUpEvent, _w, cx| {
                    this.pointer_up(e.position.x.into(), e.position.y.into());
                    cx.notify();
                }),
            )
            .child(self.render_rail(cx))
            .child(body);
        if hold {
            cx.notify();
        }
        root.into_any_element()
    }
}

impl LayersPanel {
    /// The slim icon rail at the far left: main menu, File, Assets, Variables.
    fn render_rail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut column = div()
            .flex_col()
            .w(px(RAIL_W))
            .h_full()
            .flex_shrink_0()
            .items_center();
        // Main menu (logo).
        let logo = self.icon(LayerIcon::Logo, 18.0, theme::TEXT);
        column = column.child(
            div()
                .id("rail-logo")
                .w(px(36.0))
                .h(px(32.0))
                .mt(px(8.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .cursor_pointer()
                .child(logo),
        );
        column = column.child(
            div()
                .w(px(RAIL_W - 36.0))
                .h(px(1.0))
                .mt(px(12.0))
                .mb(px(4.0))
                .bg(rgb(theme::BORDER)),
        );
        let items = [
            (LayerIcon::Page, "File", Some(LeftTab::File)),
            (LayerIcon::Assets, "Assets", Some(LeftTab::Assets)),
            (LayerIcon::Variables, "Variables", Some(LeftTab::Variables)),
        ];
        for (icon, label, tab) in items {
            let active = self.state.left_tab == tab;
            let color = if active {
                theme::ACCENT
            } else {
                theme::TEXT_SECONDARY
            };
            let chip = div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(36.0))
                .h(px(28.0))
                .rounded(px(6.0))
                .bg(rgb(if active {
                    theme::ACCENT_WASH
                } else {
                    theme::SURFACE
                }))
                .child(self.icon(icon, 16.0, color));
            column = column.child(
                div()
                    .id(SharedString::from(format!("rail-{label}")))
                    .w(px(RAIL_W))
                    .h(px(48.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        // Leaving the tab collapses it; arriving shows it.
                        this.state.left_tab = if active { None } else { tab };
                        cx.notify();
                    }))
                    .child(chip)
                    .child(
                        div()
                            .mt(px(2.0))
                            .text_xs()
                            .text_color(rgb(theme::TEXT_SECONDARY))
                            .child(label),
                    ),
            );
        }
        column.into_any_element()
    }

    /// Header: the file name (or "Assets"), and the collapse control.
    fn render_file_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let assets = self.state.left_tab == Some(LeftTab::Assets);
        let name = if assets {
            "Assets".to_string()
        } else {
            self.state
                .file_rename
                .as_ref()
                .map(|f| f.text.clone())
                .unwrap_or_else(|| {
                    self.session
                        .borrow()
                        .doc()
                        .map(|d| d.name.clone())
                        .unwrap_or_default()
                })
        };
        let icon = self.icon(LayerIcon::Panels, 14.0, theme::TEXT_SECONDARY);
        div()
            .h(px(FILE_HEADER_H))
            .flex_shrink_0()
            .border_b_1()
            .border_color(rgb(theme::BORDER))
            .relative()
            .child(
                div()
                    .id("file-name")
                    .absolute()
                    .left(px(14.0))
                    .top(px(14.0))
                    .right(px(48.0))
                    .h(px(20.0))
                    .flex()
                    .items_center()
                    .cursor_text()
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        let name = this
                            .session
                            .borrow()
                            .doc()
                            .map(|d| d.name.clone())
                            .unwrap_or_default();
                        this.state.rename = None;
                        this.state.rename_field = None;
                        this.state.filter_field = None;
                        this.state.file_rename = Some(TextField::new(name, true));
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(name),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left(px(16.0))
                    .top(px(42.0))
                    .text_sm()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child("Drafts"),
            )
            .child(
                div()
                    .id("hide-ui")
                    .absolute()
                    .right(px(12.0))
                    .top(px(12.0))
                    .w(px(24.0))
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        this.state.left_tab = None;
                        cx.notify();
                    }))
                    .child(icon),
            )
            .into_any_element()
    }

    /// The Pages header and list: add, rename, reorder, duplicate, delete, dividers.
    fn render_pages(&mut self, page: NodeId, cx: &mut Context<Self>) -> AnyElement {
        let pages = self.pages();
        let open = self.state.pages_open;
        let pov = self.pages_viewport();
        let ptotal = pages.len() as f32 * ROW;
        let poffset = self
            .state
            .pages_offset
            .clamp(0.0, (ptotal - pov.height).max(0.0));
        self.state.pages_offset = poffset;
        let pband = visible_window(pages.len(), poffset, pov.height, ROW);
        let dragging_layers = matches!(self.state.drag, Some(Dragging::Layers { .. }));
        let drag_id = match &self.state.drag {
            Some(Dragging::Pages { id, .. }) => Some(*id),
            _ => None,
        };

        let plus = self.icon(LayerIcon::Plus, 14.0, theme::TEXT_SECONDARY);
        let header = div()
            .h(px(HEADER_H))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .pl(px(16.0))
            .pr(px(8.0))
            .child(
                div()
                    .id("pages-toggle")
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        this.state.pages_open = !this.state.pages_open;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Pages"),
                    ),
            )
            .child(
                div()
                    .id("add-page")
                    .w(px(24.0))
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        this.run("page.add", json!({}));
                        cx.notify();
                    }))
                    .child(plus),
            );

        if !open {
            return div()
                .flex_col()
                .flex_shrink_0()
                .child(header)
                .child(div().h(px(1.0)).bg(rgb(theme::BORDER)))
                .into_any_element();
        }

        let probe = self.probe(&self.probe_pages.clone());
        let mut list = div()
            .id("pages-view")
            .relative()
            .h(px(pov.height))
            .flex_shrink_0()
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(|this, e: &gpui::ScrollWheelEvent, _w, cx| {
                let dy = scroll_delta_y(&e.delta);
                let vp = this.state.pages_viewport.unwrap_or_default();
                let total = this.pages().len() as f32 * ROW;
                this.state.pages_offset =
                    (this.state.pages_offset + dy).clamp(0.0, (total - vp.height).max(0.0));
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _w, cx| {
                this.pointer_move(e.position.x.into(), e.position.y.into());
                cx.notify();
            }))
            .child(probe)
            // The full-height spacer: everything outside the band is empty scroll space.
            .child(div().absolute().left(px(0.0)).top(px(0.0)).h(px(ptotal)));

        for (k, p) in pages.iter().enumerate().skip(pband.start).take(pband.len()) {
            let id = p.id;
            let divider = p.divider;
            let current = id == page;
            let name = p.name.clone();
            let renaming = self.state.rename_page == Some(id);
            // Layers dragged from the list onto another page move there.
            let is_drop_target = dragging_layers && !divider && !current;
            let dragging_this = drag_id == Some(id);
            let weight = if current {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            };
            let mut inner = div()
                .flex_1()
                .min_w_0()
                .h(px(ROW - 8.0))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .px(px(10.0));
            if is_drop_target {
                inner = inner
                    .border_2()
                    .border_color(rgb(theme::ACCENT))
                    .rounded(px(5.0));
            }
            if dragging_this {
                inner = inner.opacity(0.5);
            }
            if !divider {
                inner = inner.cursor_default();
            }
            inner = inner.child(if renaming {
                div().text_xs().font_weight(weight).truncate().child(
                    self.state
                        .page_rename_field
                        .as_ref()
                        .map(|f| f.text.clone())
                        .unwrap_or_default(),
                )
            } else if divider {
                // A divider is a line between pages: it opens nothing and takes no layers.
                div().h(px(1.0)).flex_1().bg(rgb(theme::BORDER))
            } else {
                div().text_xs().font_weight(weight).truncate().child(name)
            });
            list = list.child(
                div()
                    .id(SharedString::from(format!("page-{k}")))
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .top(px(poffset - pband.start as f32 * ROW))
                    .h(px(ROW))
                    .flex()
                    .items_center()
                    .px(px(8.0))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                            if divider {
                                return;
                            }
                            let (x, y): (f32, f32) = (e.position.x.into(), e.position.y.into());
                            this.state.pending = Some(PendingPress { origin: (x, y) });
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
                        if divider || this.state.drag.is_some() {
                            return;
                        }
                        this.press_page(id, e.click_count());
                        // Read the current page out and drop the borrow before `run` mutates.
                        let cur = this.session.borrow().page().ok();
                        if cur.is_some() && cur != Some(id) {
                            this.run("page.select", json!({"id": id}));
                        }
                        cx.notify();
                    }))
                    .child(inner),
            );
        }

        div()
            .flex_col()
            .flex_shrink_0()
            .child(header)
            .child(list)
            .child(div().h(px(1.0)).bg(rgb(theme::BORDER)))
            .into_any_element()
    }

    /// The Layers header, with the search toggle.
    fn render_layers_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let search_on = self.state.layer_search;
        let icon = self.icon(LayerIcon::Search, 14.0, theme::TEXT_SECONDARY);
        div()
            .h(px(HEADER_H))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .pl(px(16.0))
            .pr(px(8.0))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Layers"),
            )
            .child(
                div()
                    .id("layer-search")
                    .w(px(24.0))
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.0))
                    .bg(rgb(if search_on {
                        theme::SURFACE_HOVER
                    } else {
                        theme::SURFACE
                    }))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _e, _w, cx| {
                        this.state.layer_search = !this.state.layer_search;
                        this.state.filter_field = if this.state.layer_search {
                            Some(TextField::new("", false))
                        } else {
                            None
                        };
                        cx.notify();
                    }))
                    .child(icon),
            )
            .into_any_element()
    }

    /// The live filter under the Layers header.
    fn render_search(&mut self, _cx: &mut Context<Self>) -> AnyElement {
        let text = self
            .state
            .filter_field
            .as_ref()
            .map(|f| f.text.clone())
            .unwrap_or_default();
        let empty = text.is_empty();
        let glyph = self.icon(LayerIcon::Search, 12.0, theme::TEXT_MUTED);
        div()
            .h(px(SEARCH_H))
            .flex_shrink_0()
            .px(px(12.0))
            .flex()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .h(px(24.0))
                    .rounded(px(5.0))
                    .bg(rgb(theme::SURFACE_RAISED))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .child(glyph)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .child(if empty {
                                div()
                                    .text_color(rgb(theme::TEXT_MUTED))
                                    .child("Find layers")
                            } else {
                                div().child(text)
                            }),
                    ),
            )
            .into_any_element()
    }

    /// The layer rows, windowed against the measured viewport.
    fn render_layers(
        &mut self,
        doc: &Document,
        page: NodeId,
        rows: &[Row],
        selection: &[NodeId],
        hover: Option<NodeId>,
        band: Range<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let total = rows.len() as f32 * PITCH;
        let offset = self.state.offset;
        let dragging = self.state.drag.clone();
        let filter_empty = self.filter().is_empty();
        let probe = self.probe(&self.probe_layers.clone());
        let list = div()
            .id("layers-view")
            .debug_selector(|| "layers-list".to_string())
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(self.chrome_height()))
            .bottom(px(0.0))
            .min_h(px(0.0))
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(|this, e: &gpui::ScrollWheelEvent, _w, cx| {
                this.scroll_by(scroll_delta_y(&e.delta));
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _w, cx| {
                let (x, y): (f32, f32) = (e.position.x.into(), e.position.y.into());
                this.pointer_move(x, y);
                this.state.hover = this
                    .state
                    .viewport
                    .and_then(|v| v.row_at(x, y, this.state.offset, this.cached_rows.len()))
                    .and_then(|i| this.cached_rows.get(i).map(|r| r.id));
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseUpEvent, _w, cx| {
                    this.pointer_up(e.position.x.into(), e.position.y.into());
                    cx.notify();
                }),
            )
            .child(probe)
            // The full-height spacer: everything outside the band is empty scroll space.
            .child(div().absolute().left(px(0.0)).top(px(0.0)).h(px(total)));

        if rows.is_empty() {
            let hint = if filter_empty {
                "Add a frame (F) or a shape (R) to get started"
            } else {
                "No matching layers"
            };
            // The hint lives inside the list, not around it: the list is the
            // box the rows are drawn in, and a box around it would have to be
            // given a height of its own.
            return list
                .child(
                    div()
                        .absolute()
                        .left(px(16.0))
                        .top(px(8.0))
                        .text_xs()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(hint),
                )
                .into_any_element();
        }

        // Only the rows in view are built; the rest is the spacer above.
        let mut band_div = div()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(offset - band.start as f32 * PITCH));
        for (k, row) in rows.iter().enumerate().skip(band.start).take(band.len()) {
            let Some(n) = doc.node(row.id) else { continue };
            band_div = band_div.child(self.render_row(
                doc,
                page,
                row,
                n,
                k,
                selection,
                hover,
                dragging.as_ref(),
                cx,
            ));
        }
        list.child(band_div).into_any_element()
    }

    /// One row: disclosure, icon, name, eye, padlock.
    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &mut self,
        doc: &Document,
        page: NodeId,
        row: &Row,
        n: &figcraft_doc::Node,
        index: usize,
        selection: &[NodeId],
        hover: Option<NodeId>,
        dragging: Option<&Dragging>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.id;
        let depth = row.depth;
        let has_kids = row.has_kids;
        let selected = selection.contains(&row.id);
        let in_selected = !selected && doc.ancestors(row.id).iter().any(|a| selection.contains(a));
        let is_comp = matches!(
            n.kind,
            NodeKind::Component { .. } | NodeKind::ComponentSet { .. } | NodeKind::Instance { .. }
        );
        let shown = n.visible && !row.dimmed;
        let icon = node_icon(doc, row.id);
        let open = self.state.expanded.contains(&row.id);
        let renaming = self.state.rename.is_some_and(|rn| rn.id == row.id);
        let hidden = !n.visible;
        let locked = n.locked;
        let hovered = hover == Some(row.id) || dragging.is_some();
        let drop_into = self.state.drop_into == Some(row.id);

        let mut row_base = div()
            .h(px(ROW))
            .flex()
            .items_center()
            .px(px(8.0))
            .pl(px(8.0 + depth as f32 * INDENT))
            .cursor_default();
        // A selected row takes the accent wash, or the component tint when it is a component.
        if selected {
            row_base = row_base.bg(rgb(if is_comp {
                COMPONENT_ROW
            } else {
                theme::ACCENT_WASH
            }));
        } else if in_selected {
            row_base = row_base.bg(rgb(theme::ACCENT_WASH));
        }
        if drop_into {
            row_base = row_base
                .border_2()
                .border_color(rgb(theme::ACCENT))
                .rounded(px(5.0));
        } else if !selected && !in_selected && hovered {
            row_base = row_base
                .border_1()
                .border_color(rgb(if is_comp { COMPONENT } else { theme::ACCENT }))
                .rounded(px(5.0));
        }
        let mut row_div = row_base
            .id(SharedString::from(format!("row-{index}")))
            // The selector names the LAYER, not the position: a test can then
            // say which layer it is looking at, and the same layer keeps its
            // name when the list above it changes.
            .debug_selector(|| format!("layers-row-{}", row.id.0))
            .h(px(ROW))
            .flex()
            .items_center()
            .px(px(8.0))
            .pl(px(8.0 + depth as f32 * INDENT))
            .cursor_default()
            .on_mouse_move(cx.listener(move |this, _e, _w, cx| {
                if let Ok(ds) = this.session.borrow_mut().ds_mut() {
                    if ds.hover != Some(id) {
                        ds.hover = Some(id);
                    }
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                    let (x, y): (f32, f32) = (e.position.x.into(), e.position.y.into());
                    let hit = this.hit_for(x, depth, has_kids);
                    if hit == RowHit::Eye || hit == RowHit::Lock {
                        // A press on the eye or the padlock starts a sweep across the rows the
                        // pointer passes, so a drag over ten rows is one command.
                        let on = this
                            .session
                            .borrow()
                            .doc()
                            .ok()
                            .and_then(|d| d.node(id))
                            .map(|n| {
                                if hit == RowHit::Lock {
                                    !n.locked
                                } else {
                                    n.visible
                                }
                            })
                            .unwrap_or(true);
                        this.state.sweep = Some(Sweep {
                            lock: hit == RowHit::Lock,
                            on,
                            ids: vec![id],
                        });
                        this.state.pending = None;
                    } else {
                        // Nothing acts on the press: the release decides whether it was a click or
                        // a drag, and the triangle owns its own click. figcraft splits these the
                        // same way (`clicked` against `drag_started`).
                        this.state.pending = Some(PendingPress { origin: (x, y) });
                    }
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
                if this.state.drag.is_some() || this.state.sweep.is_some() {
                    return;
                }
                let x: f32 = e.position().x.into();
                let hit = this.hit_for(x, depth, has_kids);
                let count = e.click_count();
                let m = e.modifiers();
                // Alt on the disclosure takes the whole subtree with it.
                this.press_row(id, hit, count, m, m.alt);
                cx.notify();
            }));

        // The disclosure triangle.
        if has_kids {
            let glyph = self.icon(
                if open {
                    LayerIcon::ChevronDown
                } else {
                    LayerIcon::ChevronRight
                },
                14.0,
                theme::TEXT_MUTED,
            );
            row_div = row_div.child(
                div()
                    .id(SharedString::from(format!("tri-{index}")))
                    .w(px(14.0))
                    .h(px(14.0))
                    .flex_shrink_0()
                    .mr(px(4.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    // The click is handled once, by the row: this element is the glyph's box, and
                    // `hit_for` resolves a press on it to `RowHit::Triangle`.
                    .child(glyph),
            );
        } else {
            row_div = row_div.child(div().w(px(18.0)).flex_shrink_0());
        }

        let icon_color = if is_comp || icon == LayerIcon::Slot {
            COMPONENT
        } else if shown {
            theme::TEXT_SECONDARY
        } else {
            theme::TEXT_MUTED
        };
        let glyph = self.icon(icon, 14.0, icon_color);
        row_div = row_div.child(glyph).child(div().w(px(6.0)).flex_shrink_0());

        if renaming {
            let value = self
                .state
                .rename_field
                .as_ref()
                .map(|f| f.text.clone())
                .unwrap_or_default();
            row_div = row_div.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(ROW - 8.0))
                    .rounded(px(5.0))
                    .bg(rgb(theme::SURFACE_RAISED))
                    .px(px(6.0))
                    .flex()
                    .items_center()
                    .child(div().text_xs().truncate().child(value)),
            );
        } else {
            let color = if is_comp {
                COMPONENT
            } else if shown {
                theme::TEXT
            } else {
                theme::TEXT_MUTED
            };
            let parent_is_page = doc
                .node(n.parent.unwrap_or(page))
                .is_some_and(|p| p.is_page());
            let weight = if parent_is_page
                && (n.kind.is_frame_like() || matches!(n.kind, NodeKind::Section { .. }))
            {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            };
            let name = if n.name.is_empty() {
                n.kind.label().to_string()
            } else {
                n.name.clone()
            };
            row_div = row_div.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .mr(px(56.0))
                    .text_xs()
                    .font_weight(weight)
                    .text_color(rgb(color))
                    .truncate()
                    .child(name),
            );
        }

        // Lock / eye: shown on hover, or when set. A layer in a locked one shows a faint padlock.
        let show_lock = locked || row.locked_above || self.state.hover == Some(row.id);
        let show_eye = hidden || self.state.hover == Some(row.id);
        if show_eye {
            let glyph = self.icon(
                if hidden {
                    LayerIcon::EyeOff
                } else {
                    LayerIcon::Eye
                },
                14.0,
                theme::TEXT_MUTED,
            );
            row_div = row_div.child(
                div()
                    .id(SharedString::from(format!("eye-{index}")))
                    .w(px(20.0))
                    .h(px(20.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        let visible = this
                            .session
                            .borrow()
                            .doc()
                            .ok()
                            .and_then(|d| d.node(id))
                            .map(|n| n.visible)
                            .unwrap_or(true);
                        this.run(
                            "object.toggleVisible",
                            json!({"ids": [id], "visible": !visible}),
                        );
                        cx.notify();
                    }))
                    .child(glyph),
            );
        }
        if show_lock {
            let glyph = self.icon(
                if locked || row.locked_above {
                    LayerIcon::Lock
                } else {
                    LayerIcon::Unlock
                },
                14.0,
                if locked || !row.locked_above {
                    theme::TEXT_MUTED
                } else {
                    theme::BORDER_SOFT
                },
            );
            row_div = row_div.child(
                div()
                    .id(SharedString::from(format!("lock-{index}")))
                    .w(px(20.0))
                    .h(px(20.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        let locked_now = this
                            .session
                            .borrow()
                            .doc()
                            .ok()
                            .and_then(|d| d.node(id))
                            .map(|n| n.locked)
                            .unwrap_or(false);
                        this.run(
                            "object.toggleLock",
                            json!({"ids": [id], "locked": !locked_now}),
                        );
                        cx.notify();
                    }))
                    .child(glyph),
            );
        }
        row_div.into_any_element()
    }
}

/// A vertical scroll delta in points.
fn scroll_delta_y(delta: &ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Pixels(p) => p.y.into(),
        ScrollDelta::Lines(p) => p.y * 24.0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    /// A page with `n` rectangles on it, each named `Layer {i}`.
    fn doc_with(n: usize) -> (Document, NodeId, Vec<NodeId>) {
        let mut doc = Document::new("T");
        let page = doc.pages[0];
        let ids = (0..n)
            .map(|i| {
                doc.create(
                    page,
                    NodeKind::Rectangle,
                    &format!("Layer {i}"),
                    0.0,
                    0.0,
                    10.0,
                    10.0,
                )
                .unwrap_or(page)
            })
            .collect();
        (doc, page, ids)
    }

    fn group(doc: &mut Document, parent: NodeId, name: &str) -> NodeId {
        doc.create(parent, NodeKind::Group, name, 0.0, 0.0, 10.0, 10.0)
            .unwrap_or(parent)
    }

    fn rect(doc: &mut Document, parent: NodeId, name: &str) -> NodeId {
        doc.create(parent, NodeKind::Rectangle, name, 0.0, 0.0, 10.0, 10.0)
            .unwrap_or(parent)
    }

    // --- the visible window ------------------------------------------------

    #[test]
    fn visible_window_covers_the_viewport() {
        // A hundred 32pt rows in a 320pt box: ten fit, plus the one that ends inside it.
        assert_eq!(visible_window(100, 0.0, 320.0, PITCH), 0..11);
        assert_eq!(visible_window(100, 320.0, 320.0, PITCH), 10..21);
    }

    #[test]
    fn visible_window_clamps_to_the_row_count() {
        assert_eq!(visible_window(3, 0.0, 320.0, PITCH), 0..3);
        assert_eq!(visible_window(0, 0.0, 320.0, PITCH), 0..0);
        assert_eq!(visible_window(3, 100_000.0, 320.0, PITCH), 3..3);
    }

    #[test]
    fn visible_window_is_total_on_nonsense() {
        // No division by zero, no panic on a list scrolled before it was laid out.
        assert_eq!(visible_window(10, 0.0, 0.0, PITCH), 0..0);
        assert_eq!(visible_window(10, 0.0, 320.0, 0.0), 0..0);
        assert_eq!(visible_window(10, f32::NAN, 320.0, PITCH), 0..0);
        assert_eq!(visible_window(10, -50.0, 320.0, PITCH), 0..10);
    }

    #[test]
    fn visible_window_is_bounded_for_a_ten_thousand_layer_file() {
        // The point of the port: the work is proportional to the viewport, not the document.
        let range = visible_window(10_000, 5_000.0 * PITCH, 400.0, PITCH);
        assert!(range.len() < 32, "{range:?}");
        assert_eq!(range.start, 5_000);
    }

    #[test]
    fn scroll_to_row_centres_and_clamps() {
        assert_eq!(scroll_to_row(0, 320.0, PITCH, 100), 0.0);
        assert_eq!(scroll_to_row(50, 320.0, PITCH, 100), 50.0 * PITCH - 160.0);
        // A list shorter than the viewport can't scroll at all.
        assert_eq!(scroll_to_row(1, 320.0, PITCH, 2), 0.0);
    }

    #[test]
    fn edge_scroll_only_near_the_edges() {
        assert_eq!(edge_scroll_delta(200.0, 400.0), 0.0);
        assert!(edge_scroll_delta(0.0, 400.0) > 0.0);
        assert!(edge_scroll_delta(400.0, 400.0) < 0.0);
        assert_eq!(edge_scroll_delta(10.0, 0.0), 0.0);
        assert_eq!(edge_scroll_delta(f32::NAN, 400.0), 0.0);
    }

    // --- the row projection ------------------------------------------------

    #[test]
    fn rows_are_listed_top_of_stack_first() {
        let (doc, page, ids) = doc_with(3);
        let rows = listed_rows(&doc, page, &BTreeSet::new(), "");
        let got: Vec<NodeId> = rows.iter().map(|r| r.id).collect();
        let mut want = ids;
        want.reverse();
        assert_eq!(got, want);
        assert!(rows.iter().all(|r| r.depth == 0 && !r.has_kids));
    }

    /// A layer's children are listed only when that layer is in the expanded
    /// set, so a document that opens with its top-level layers closed shows
    /// the frames and nothing inside them.
    ///
    /// This is the rule the panel's own open-on-load behaviour depends on: the
    /// top-level layers are what gets opened, so the rows inside them are
    /// listed.
    #[test]
    fn a_layers_children_are_listed_only_when_it_is_expanded() {
        // A GROUP, because a rectangle cannot have children -- the engine
        // refuses the create and the helper hands back the parent, which would
        // make this test pass for the wrong reason.
        let (mut doc, page, _) = doc_with(0);
        let top = group(&mut doc, page, "G");
        let kid = rect(&mut doc, top, "K");
        assert_ne!(top, kid, "the child is a different layer");

        let closed = listed_rows(&doc, page, &BTreeSet::new(), "");
        assert_eq!(
            closed.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![top],
            "a closed layer's children are not listed"
        );

        let mut ex = BTreeSet::new();
        ex.insert(top);
        let open = listed_rows(&doc, page, &ex, "");
        assert_eq!(
            open.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![top, kid],
            "an expanded layer's children are listed"
        );
    }

    #[test]
    fn expanded_rows_carry_their_depth() {
        let (mut doc, page, _) = doc_with(2);
        let g = group(&mut doc, page, "G");
        let kid = rect(&mut doc, g, "K");
        let mut ex = BTreeSet::new();
        ex.insert(g);
        let rows = listed_rows(&doc, page, &ex, "");
        // The group is topmost on the page, so it and its open child lead the list.
        assert_eq!(
            rows[..2].iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![g, kid]
        );
        assert!(rows[0].has_kids);
        assert_eq!(rows[1].depth, 1);
        // Collapsed, the group's child is not listed at all.
        assert_eq!(listed_rows(&doc, page, &BTreeSet::new(), "").len(), 3);
    }

    #[test]
    fn a_hidden_ancestor_dims_its_children() {
        let (mut doc, page, _) = doc_with(1);
        let g = group(&mut doc, page, "G");
        let kid = rect(&mut doc, g, "K");
        let mut ex = BTreeSet::new();
        ex.insert(g);
        if let Some(n) = doc.node_mut(g) {
            n.visible = false;
        }
        let rows = listed_rows(&doc, page, &ex, "");
        assert!(!rows.iter().find(|r| r.id == g).unwrap().dimmed);
        assert!(rows.iter().find(|r| r.id == kid).unwrap().dimmed);
    }

    #[test]
    fn a_locked_ancestor_marks_its_children_locked_above() {
        let (mut doc, page, _) = doc_with(1);
        let g = group(&mut doc, page, "G");
        let kid = rect(&mut doc, g, "K");
        if let Some(n) = doc.node_mut(g) {
            n.locked = true;
        }
        let mut ex = BTreeSet::new();
        ex.insert(g);
        let rows = listed_rows(&doc, page, &ex, "");
        assert!(rows.iter().find(|r| r.id == kid).unwrap().locked_above);
        assert!(!rows.iter().find(|r| r.id == g).unwrap().locked_above);
    }

    #[test]
    fn search_lists_matches_flat_and_caps_the_hits() {
        let (doc, page, _) = doc_with(3);
        let rows = listed_rows(&doc, page, &BTreeSet::new(), "layer 1");
        assert_eq!(rows.len(), 1);
        // Results are flat: no depth, no disclosure.
        assert_eq!(rows[0].depth, 0);
        assert!(!rows[0].has_kids);

        // The cap stops the walk on a file far bigger than it: the list is bounded well below the
        // node count, even though every one of those layers matches.
        let (doc, page, _) = doc_with(SEARCH_HIT_CAP + 5_000);
        let rows = listed_rows(&doc, page, &BTreeSet::new(), "layer");
        assert!(rows.len() >= SEARCH_HIT_CAP, "{}", rows.len());
        assert!(
            rows.len() < doc.node_count(),
            "{} hits from {} nodes",
            rows.len(),
            doc.node_count()
        );
    }

    #[test]
    fn search_is_case_insensitive_and_falls_back_to_the_kind_label() {
        let mut doc = Document::new("T");
        let page = doc.pages[0];
        let unnamed = doc
            .create(
                page,
                NodeKind::Ellipse {
                    start: 0.0,
                    sweep: std::f64::consts::TAU,
                    inner: 0.0,
                },
                "",
                0.0,
                0.0,
                1.0,
                1.0,
            )
            .unwrap_or(page);
        let rows = listed_rows(&doc, page, &BTreeSet::new(), "ellipse");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, unnamed);
    }

    #[test]
    fn row_cache_rebuilds_only_when_a_dependency_changed() {
        let (doc, page, _) = doc_with(3);
        let doc = Arc::new(doc);
        let mut cache = RowCache::default();
        let ex = BTreeSet::new();
        let first = cache.rows(&doc, page, &ex, "");
        assert_eq!(first.len(), 3);
        // Nothing changed: the same allocation comes back.
        assert!(Arc::ptr_eq(&first, &cache.rows(&doc, page, &ex, "")));
        // The filter changed.
        assert_eq!(cache.rows(&doc, page, &ex, "layer 0").len(), 1);
        // So did the document.
        let other = Arc::new(doc_with(4).0);
        assert_eq!(cache.rows(&other, page, &ex, "").len(), 4);
    }

    // --- the click / drag decision -----------------------------------------

    #[test]
    fn the_row_geometry_finds_the_control_under_the_pointer() {
        // 240pt panel: the row's inner box is 224 wide, so the eye is the last 20 and the padlock
        // the 20 before it.
        assert_eq!(row_hit_geometry(210.0, 224.0, 0, false), RowHit::Eye);
        assert_eq!(row_hit_geometry(190.0, 224.0, 0, false), RowHit::Lock);
        assert_eq!(row_hit_geometry(100.0, 224.0, 0, false), RowHit::Name);
        // The triangle only exists on a row with children, and it moves with the indent.
        assert_eq!(row_hit_geometry(8.0, 224.0, 0, true), RowHit::Triangle);
        assert_eq!(row_hit_geometry(8.0, 224.0, 0, false), RowHit::Name);
        assert_eq!(row_hit_geometry(24.0, 224.0, 1, true), RowHit::Triangle);
        assert_eq!(row_hit_geometry(8.0, 224.0, 1, true), RowHit::Name);
    }

    #[test]
    fn the_row_geometry_is_total_on_an_unmeasured_viewport() {
        assert_eq!(row_hit_geometry(10.0, 0.0, 0, true), RowHit::Name);
        assert_eq!(row_hit_geometry(f32::NAN, 224.0, 0, true), RowHit::Name);
        assert_eq!(row_hit_geometry(-10.0, 224.0, 0, false), RowHit::Name);
    }

    #[test]
    fn a_scroll_delta_is_points_whatever_the_device_sends() {
        assert_eq!(
            scroll_delta_y(&ScrollDelta::Pixels(point(px(0.0), px(-24.0)))),
            -24.0
        );
        assert_eq!(scroll_delta_y(&ScrollDelta::Lines(point(0.0, -2.0))), -48.0);
        assert_eq!(scroll_delta_y(&ScrollDelta::default()), 0.0);
    }

    #[test]
    fn a_plain_click_replaces_the_selection() {
        assert_eq!(
            row_action(RowHit::Name, 1, false, false, true, false),
            RowAction::Select {
                id: NodeId(0),
                mode: SelectMode::Replace
            }
        );
    }

    #[test]
    fn shift_with_an_anchor_extends_the_range() {
        assert_eq!(
            row_action(RowHit::Name, 1, true, false, true, true),
            RowAction::Select {
                id: NodeId(0),
                mode: SelectMode::Range
            }
        );
        // Shift with nothing to extend from is a toggle.
        assert_eq!(
            row_action(RowHit::Name, 1, true, false, true, false),
            RowAction::Select {
                id: NodeId(0),
                mode: SelectMode::Toggle
            }
        );
        assert_eq!(
            row_action(RowHit::Name, 1, false, true, true, false),
            RowAction::Select {
                id: NodeId(0),
                mode: SelectMode::Toggle
            }
        );
    }

    #[test]
    fn a_double_click_opens_the_rename() {
        assert_eq!(
            row_action(RowHit::Name, 2, false, false, true, false),
            RowAction::Rename { id: NodeId(0) }
        );
    }

    #[test]
    fn the_controls_never_change_the_selection() {
        assert!(matches!(
            row_action(RowHit::Eye, 1, false, false, true, false),
            RowAction::ToggleVisible { .. }
        ));
        assert!(matches!(
            row_action(RowHit::Lock, 1, false, false, true, false),
            RowAction::ToggleLock { .. }
        ));
        assert!(matches!(
            row_action(RowHit::Triangle, 1, false, false, true, false),
            RowAction::ToggleExpanded { .. }
        ));
    }

    #[test]
    fn a_click_on_an_unlisted_row_does_nothing() {
        assert_eq!(
            row_action(RowHit::Name, 1, false, false, false, false),
            RowAction::None
        );
    }

    #[test]
    fn the_middle_of_a_container_drops_into_it() {
        assert_eq!(layer_drop_zone(0.5, true, false), 0);
        // The edges always reorder, even over a container.
        assert_eq!(layer_drop_zone(0.1, true, false), -1);
        assert_eq!(layer_drop_zone(0.9, true, false), 1);
        // A plain row only ever reorders.
        assert_eq!(layer_drop_zone(0.5, false, false), 1);
        // An instance's structure is fixed, so its middle quarter is a reorder too.
        assert_eq!(layer_drop_zone(0.5, true, true), 1);
    }

    #[test]
    fn a_page_drop_lands_between_rows() {
        // Five rows; dropping in the bottom half of row 2 leaves three rows above the gap, so it
        // takes index 3.
        assert_eq!(drop_slot(4, 2, true, 5), Some(3));
        // Dropping it where it already is does nothing, either side of its own row.
        assert_eq!(drop_slot(2, 2, true, 5), None);
        assert_eq!(drop_slot(3, 2, true, 5), None);
        assert_eq!(drop_slot(0, 0, false, 5), None);
        // The bottom half of the last row is the only place it lands last.
        assert_eq!(drop_slot(0, 4, true, 5), Some(4));
        assert_eq!(drop_slot(0, 4, false, 5), Some(3));
        // Out of range on either side is nothing.
        assert_eq!(drop_slot(0, 0, true, 0), None);
        assert_eq!(drop_slot(9, 0, true, 5), None);
        assert_eq!(drop_slot(0, 9, true, 5), None);
    }

    // --- keyboard navigation ----------------------------------------------

    #[test]
    fn down_and_up_walk_the_list() {
        let (doc, page, _) = doc_with(3);
        let rows = listed_rows(&doc, page, &BTreeSet::new(), "");
        let top = rows[0].id;
        assert_eq!(
            nav_step(
                &rows,
                &[top],
                Some(top),
                NavKey::Down,
                false,
                &BTreeSet::new(),
                false,
                |_| None,
                page
            ),
            NavOutcome::Select {
                ids: vec![rows[1].id],
                anchor: rows[1].id
            }
        );
        // Nothing selected yet: Down starts at the top.
        assert_eq!(
            nav_step(
                &rows,
                &[],
                None,
                NavKey::Down,
                false,
                &BTreeSet::new(),
                false,
                |_| None,
                page
            ),
            NavOutcome::Select {
                ids: vec![top],
                anchor: top
            }
        );
        // ... and Up at the bottom.
        assert_eq!(
            nav_step(
                &rows,
                &[],
                None,
                NavKey::Up,
                false,
                &BTreeSet::new(),
                false,
                |_| None,
                page
            ),
            NavOutcome::Select {
                ids: vec![rows[2].id],
                anchor: rows[2].id
            }
        );
        // Off the end does nothing.
        assert_eq!(
            nav_step(
                &rows,
                &[rows[2].id],
                None,
                NavKey::Down,
                false,
                &BTreeSet::new(),
                false,
                |_| None,
                page
            ),
            NavOutcome::None
        );
        // Neither does the empty list.
        assert_eq!(
            nav_step(
                &[],
                &[],
                None,
                NavKey::Down,
                false,
                &BTreeSet::new(),
                false,
                |_| None,
                page
            ),
            NavOutcome::None
        );
    }

    #[test]
    fn shift_down_extends_from_the_anchor() {
        let (doc, page, _) = doc_with(5);
        let rows = listed_rows(&doc, page, &BTreeSet::new(), "");
        let anchor = rows[1].id;
        assert_eq!(
            nav_step(
                &rows,
                &[anchor],
                Some(anchor),
                NavKey::Down,
                true,
                &BTreeSet::new(),
                false,
                |_| None,
                page
            ),
            NavOutcome::Select {
                ids: vec![rows[1].id, rows[2].id],
                anchor
            }
        );
    }

    #[test]
    fn left_collapses_then_walks_to_the_parent() {
        let (mut doc, page, _) = doc_with(1);
        let g = group(&mut doc, page, "G");
        let _kid = rect(&mut doc, g, "K");
        let parent = |id: NodeId| doc.node(id).and_then(|n| n.parent);
        let mut ex = BTreeSet::new();
        ex.insert(g);
        let rows = listed_rows(&doc, page, &ex, "");
        // Expanded: Left collapses it.
        assert_eq!(
            nav_step(
                &rows,
                &[g],
                None,
                NavKey::Left,
                false,
                &ex,
                false,
                parent,
                page
            ),
            NavOutcome::Toggle(g)
        );
        // Collapsed: Left goes to the parent, unless that is the page itself.
        assert_eq!(
            nav_step(
                &rows,
                &[g],
                None,
                NavKey::Left,
                false,
                &BTreeSet::new(),
                false,
                parent,
                page
            ),
            NavOutcome::None
        );
        // From the child, Left walks out to the group.
        assert_eq!(
            nav_step(
                &rows,
                &[_kid],
                None,
                NavKey::Left,
                false,
                &ex,
                false,
                parent,
                page
            ),
            NavOutcome::Select {
                ids: vec![g],
                anchor: g
            }
        );
    }

    #[test]
    fn right_opens_a_row_then_enters_it() {
        let (mut doc, page, _) = doc_with(0);
        let g = group(&mut doc, page, "G");
        let kid = rect(&mut doc, g, "K");
        let parent = |id: NodeId| doc.node(id).and_then(|n| n.parent);
        let ex = BTreeSet::new();
        let rows = listed_rows(&doc, page, &ex, "");
        // Collapsed: Right opens it.
        assert_eq!(
            nav_step(
                &rows,
                &[g],
                None,
                NavKey::Right,
                false,
                &ex,
                false,
                parent,
                page
            ),
            NavOutcome::Toggle(g)
        );
        // Open: Right goes into the first child.
        let mut ex = BTreeSet::new();
        ex.insert(g);
        let rows = listed_rows(&doc, page, &ex, "");
        assert_eq!(
            nav_step(
                &rows,
                &[g],
                None,
                NavKey::Right,
                false,
                &ex,
                false,
                parent,
                page
            ),
            NavOutcome::Select {
                ids: vec![kid],
                anchor: kid
            }
        );
    }

    #[test]
    fn the_arrows_do_nothing_while_filtering() {
        let (mut doc, page, _) = doc_with(0);
        let g = group(&mut doc, page, "G");
        let _kid = rect(&mut doc, g, "K");
        let mut ex = BTreeSet::new();
        ex.insert(g);
        let rows = listed_rows(&doc, page, &ex, "");
        // A flat search list has nothing to expand.
        assert_eq!(
            nav_step(
                &rows,
                &[g],
                None,
                NavKey::Right,
                false,
                &ex,
                true,
                |_| None,
                page
            ),
            NavOutcome::None
        );
    }

    // --- expand helpers ----------------------------------------------------

    #[test]
    fn deep_expand_walks_the_whole_subtree() {
        let (mut doc, page, _) = doc_with(0);
        let a = group(&mut doc, page, "A");
        let b = group(&mut doc, a, "B");
        let mut ex = BTreeSet::new();
        set_expanded_deep(&doc, a, true, &mut ex);
        assert!(ex.contains(&a) && ex.contains(&b));
        set_expanded_deep(&doc, a, false, &mut ex);
        assert!(!ex.contains(&a) && !ex.contains(&b));
    }

    #[test]
    fn ancestors_to_expand_leaves_the_page_out() {
        let (mut doc, page, _) = doc_with(0);
        let a = group(&mut doc, page, "A");
        let b = rect(&mut doc, a, "B");
        assert_eq!(ancestors_to_expand(&doc, page, &[b]), vec![a]);
        assert!(ancestors_to_expand(&doc, page, &[]).is_empty());
    }

    #[test]
    fn a_drag_moves_the_selection_when_the_row_is_in_it() {
        let (_, _, ids) = doc_with(3);
        assert_eq!(
            dragged_layers(Some(ids[0]), &[ids[0], ids[1]]),
            vec![ids[0], ids[1]]
        );
        assert_eq!(dragged_layers(Some(ids[0]), &[ids[1]]), vec![ids[0]]);
        assert!(dragged_layers(None, &[ids[0]]).is_empty());
    }

    // --- icons -------------------------------------------------------------

    #[test]
    fn every_icon_emits_one_well_formed_svg() {
        for icon in LayerIcon::ALL {
            let markup = icon_svg(*icon);
            assert!(
                markup
                    .starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\""),
                "{icon:?}"
            );
            assert!(markup.ends_with("</svg>"), "{icon:?}");
            // Every icon has to paint something: GPUI's SVG path keeps only the alpha.
            assert!(
                markup.contains("#000"),
                "{icon:?} draws nothing in the alpha mask"
            );
            assert_eq!(
                markup.matches('<').count(),
                markup.matches('>').count(),
                "{icon:?}"
            );
            assert!(!markup.contains("NaN"), "{icon:?}");
        }
    }

    #[test]
    fn node_icon_follows_the_kind_and_the_layout() {
        let mut doc = Document::new("T");
        let page = doc.pages[0];
        let r = rect(&mut doc, page, "R");
        assert_eq!(node_icon(&doc, r), LayerIcon::Rectangle);
        let frame = doc
            .create(page, NodeKind::Frame, "F", 0.0, 0.0, 1.0, 1.0)
            .unwrap_or(page);
        assert_eq!(node_icon(&doc, frame), LayerIcon::Frame);
        if let Some(n) = doc.node_mut(frame) {
            let mut layout = n.layout.clone().unwrap_or_default();
            layout.mode = LayoutMode::Vertical;
            n.layout = Some(layout);
        }
        assert_eq!(node_icon(&doc, frame), LayerIcon::LayoutV);
        if let Some(n) = doc.node_mut(frame) {
            let mut layout = n.layout.clone().unwrap_or_default();
            layout.mode = LayoutMode::Grid;
            n.layout = Some(layout);
        }
        assert_eq!(node_icon(&doc, frame), LayerIcon::LayoutGrid);
        if let Some(n) = doc.node_mut(frame) {
            let mut layout = n.layout.clone().unwrap_or_default();
            layout.mode = LayoutMode::Horizontal;
            n.layout = Some(layout);
        }
        assert_eq!(node_icon(&doc, frame), LayerIcon::LayoutH);
        let t = doc
            .create(
                page,
                NodeKind::Text(figcraft_doc::TextData::default()),
                "T",
                0.0,
                0.0,
                1.0,
                1.0,
            )
            .unwrap_or(page);
        assert_eq!(node_icon(&doc, t), LayerIcon::Text);
        let g = group(&mut doc, page, "G");
        assert_eq!(node_icon(&doc, g), LayerIcon::Group);
    }

    #[test]
    fn a_missing_node_does_not_panic() {
        let doc = Document::new("T");
        assert_eq!(node_icon(&doc, NodeId(9_999)), LayerIcon::Rectangle);
    }

    #[test]
    fn boolean_operations_map_to_their_icons() {
        assert_eq!(op_icon(BoolOp::Union), LayerIcon::Union);
        assert_eq!(op_icon(BoolOp::Subtract), LayerIcon::Subtract);
        assert_eq!(op_icon(BoolOp::Intersect), LayerIcon::Intersect);
        assert_eq!(op_icon(BoolOp::Exclude), LayerIcon::Exclude);
    }

    // --- the text buffer ---------------------------------------------------

    #[test]
    fn a_rename_opens_with_the_name_selected() {
        let mut f = TextField::new("Header", true);
        assert_eq!(f.caret, 6);
        f.insert('X');
        assert_eq!(f.text, "X");
        f.end();
        f.insert('!');
        assert_eq!(f.text, "X!");
    }

    #[test]
    fn the_buffer_edits_at_the_caret() {
        let mut f = TextField::new("abc", false);
        f.move_caret(-1);
        f.backspace();
        assert_eq!(f.text, "ac");
        f.delete();
        assert_eq!(f.text, "a");
        f.home();
        f.insert('z');
        assert_eq!(f.text, "za");
    }

    #[test]
    fn the_caret_clamps_at_both_ends() {
        let mut f = TextField::new("ab", false);
        f.move_caret(-100);
        assert_eq!(f.caret, 0);
        f.backspace();
        assert_eq!(f.text, "ab");
        f.move_caret(100);
        f.delete();
        assert_eq!(f.text, "ab");
        f.end();
        f.move_caret(100);
        assert_eq!(f.caret, 2);
    }

    #[test]
    fn the_buffer_works_on_char_boundaries() {
        let mut f = TextField::new("héllo", false);
        f.home();
        f.insert('é');
        // The caret sits after the inserted character, so one backspace takes it.
        f.backspace();
        assert_eq!(f.text, "héllo");
        f.click_at(99);
        assert_eq!(f.caret, 5);
        f.backspace();
        assert_eq!(f.text, "héll");
    }

    // --- the viewport ------------------------------------------------------

    #[test]
    fn a_pointer_maps_to_a_row() {
        let vp = Viewport {
            x: 48.0,
            y: 100.0,
            width: 240.0,
            height: 320.0,
        };
        assert_eq!(vp.row_at(100.0, 100.0, 0.0, 10), Some(0));
        assert_eq!(vp.row_at(100.0, 132.0, 0.0, 10), Some(1));
        // Scrolled: the pointer at the top of the box is row 5.
        assert_eq!(vp.row_at(100.0, 100.0, 5.0 * PITCH, 10), Some(5));
        // Outside the box, or past the end, is nothing.
        assert_eq!(vp.row_at(10.0, 100.0, 0.0, 10), None);
        assert_eq!(vp.row_at(100.0, 90.0, 0.0, 10), None);
        assert_eq!(vp.row_at(100.0, 100.0 + 2.0 * PITCH, 0.0, 2), None);
    }

    #[test]
    fn row_rel_is_the_position_inside_the_row() {
        let vp = Viewport {
            x: 48.0,
            y: 100.0,
            width: 240.0,
            height: 320.0,
        };
        assert!(vp.row_rel(100.0, 0.0) < 0.1);
        assert!(vp.row_rel(114.0, 0.0) > 0.4);
        // The middle of a row is the "into it" band.
        assert_eq!(layer_drop_zone(vp.row_rel(116.0, 0.0), true, false), 0);
        // Scrolled, the position inside the row is the same.
        assert!((vp.row_rel(116.0, 3.0 * PITCH) - vp.row_rel(116.0, 0.0)).abs() < 1e-4);
    }

    // --- the panel against a real session ----------------------------------

    #[test]
    fn the_canvas_and_the_panel_share_one_session() {
        // The panel cannot be built without a GPUI context, so what is checked here is the thing
        // the shared handle buys: one engine, two holders, one set of changes.
        let (doc, _, ids) = doc_with(2);
        let canvas = Rc::new(RefCell::new(Session::with_document(doc)));
        let panel = Rc::clone(&canvas);
        panel
            .borrow_mut()
            .execute("select.set", &json!({"ids": [ids[0]]}))
            .unwrap();
        assert_eq!(canvas.borrow().selection(), vec![ids[0]]);
        canvas
            .borrow_mut()
            .execute(
                "object.rename",
                &json!({"ids": [ids[1]], "name": "Renamed"}),
            )
            .unwrap();
        // The panel's own read: the document cloned out so nothing is held borrowed.
        let doc = panel.borrow().ds().ok().map(|ds| ds.doc.clone());
        assert_eq!(doc.unwrap().node(ids[1]).unwrap().name, "Renamed");
    }

    #[test]
    fn taking_the_ui_requests_holds_no_borrow_across_the_handlers() {
        // `drain_ui_requests` shape: take with `borrow_mut`, drop it, act, then put back with a
        // fresh `borrow_mut`. A borrow held across either would panic at runtime, not at compile.
        let (doc, _, ids) = doc_with(1);
        let session = Rc::new(RefCell::new(Session::with_document(doc)));
        session
            .borrow_mut()
            .execute("layers.collapseAll", &json!({}))
            .unwrap();
        let requests = std::mem::take(&mut session.borrow_mut().ui_requests);
        assert!(requests.iter().any(|r| r.kind == "layers.collapseAll"));
        let _ = session.borrow().selection();
        let _ = session.borrow().page();
        session
            .borrow_mut()
            .execute("select.set", &json!({"ids": [ids[0]]}))
            .unwrap();
        for r in requests {
            session.borrow_mut().ui_requests.push(r);
        }
        assert_eq!(session.borrow().selection(), vec![ids[0]]);
    }

    #[test]
    fn the_rows_come_from_the_session() {
        let (doc, page, _) = doc_with(3);
        let session = Session::with_document(doc);
        let doc = session.ds().map(|ds| ds.doc.clone());
        let rows = listed_rows(&doc.as_ref().unwrap(), page, &BTreeSet::new(), "");
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn every_command_the_panel_sends_exists_and_runs() {
        // The panel's whole contract with the engine is these ids and these params; a rename in
        // the engine that dropped one would turn a click into a silent no-op.
        let (mut doc, page, ids) = doc_with(2);
        let spare = doc.add_page("Page 2");
        let target = ids[0];
        let mut session = Session::with_document(doc);
        for (id, params) in [
            ("select.set", json!({"ids": [target]})),
            ("select.none", json!({})),
            ("object.rename", json!({"ids": [target], "name": "x"})),
            (
                "object.toggleVisible",
                json!({"ids": [target], "visible": true}),
            ),
            (
                "object.toggleLock",
                json!({"ids": [target], "locked": false}),
            ),
            ("object.moveToPage", json!({"ids": [ids[1]], "page": page})),
            (
                "node.reparent",
                json!({"ids": [ids[1]], "parent": page, "index": 0}),
            ),
            ("page.add", json!({})),
            ("page.select", json!({"id": page})),
            ("page.rename", json!({"id": page, "name": "Renamed"})),
            ("page.duplicate", json!({"id": page})),
            ("page.move", json!({"id": page, "index": 0})),
            ("page.delete", json!({"id": spare})),
            ("layers.collapseAll", json!({})),
        ] {
            let err = session.execute(id, &params);
            assert!(err.is_ok(), "{id} failed: {err:?}");
        }
    }

    #[test]
    fn rename_and_toggle_go_through_the_engine() {
        let (doc, _, ids) = doc_with(2);
        let mut session = Session::with_document(doc);
        session
            .execute(
                "object.rename",
                &json!({"ids": [ids[0]], "name": "Renamed"}),
            )
            .unwrap();
        assert_eq!(session.doc().unwrap().node(ids[0]).unwrap().name, "Renamed");
        session
            .execute(
                "object.toggleVisible",
                &json!({"ids": [ids[0]], "visible": false}),
            )
            .unwrap();
        assert!(!session.doc().unwrap().node(ids[0]).unwrap().visible);
        session
            .execute(
                "object.toggleLock",
                &json!({"ids": [ids[1]], "locked": true}),
            )
            .unwrap();
        assert!(session.doc().unwrap().node(ids[1]).unwrap().locked);
    }

    #[test]
    fn a_reparent_through_the_engine_lands_where_the_drop_zone_said() {
        let (mut doc, page, ids) = doc_with(2);
        let group = group(&mut doc, page, "G");
        let mut session = Session::with_document(doc);
        session
            .execute("node.reparent", &json!({"ids": [ids[0]], "parent": group}))
            .unwrap();
        assert_eq!(
            session.doc().unwrap().node(ids[0]).unwrap().parent,
            Some(group)
        );
    }

    #[test]
    fn collapse_all_is_a_ui_request_not_a_mutation() {
        let mut session = Session::new();
        session.execute("layers.collapseAll", &json!({})).unwrap();
        assert!(session
            .ui_requests
            .iter()
            .any(|r| r.kind == "layers.collapseAll"));
    }

    #[test]
    fn rename_without_a_name_requests_the_panel() {
        let (doc, _, ids) = doc_with(1);
        let mut session = Session::with_document(doc);
        session
            .execute("object.rename", &json!({"ids": [ids[0]]}))
            .unwrap();
        assert!(session.ui_requests.iter().any(|r| r.kind == "layers.rename"
            && r.data["ids"]
                .as_array()
                .map(|a| a.len() == 1)
                .unwrap_or(false)));
    }

    #[test]
    fn pages_are_listed_with_their_dividers() {
        let mut doc = Document::new("T");
        doc.add_page("Page 2");
        doc.add_page("- Divider");
        let session = Session::with_document(doc);
        let mut panel = PanelState {
            pages_open: true,
            ..PanelState::default()
        };
        let _ = &mut panel;
        // The panel reads pages off the live document; `is_divider` is the engine's rule.
        assert_eq!(session.doc().unwrap().pages.len(), 3);
        let third = session.doc().unwrap().pages[2];
        assert!(session.doc().unwrap().is_divider(third));
    }

    #[test]
    fn a_ten_thousand_layer_file_projects_and_windows() {
        let (doc, page, _) = doc_with(10_000);
        let doc = Arc::new(doc);
        let expanded = BTreeSet::new();
        let mut cache = RowCache::default();
        let rows = cache.rows(&doc, page, &expanded, "");
        assert_eq!(rows.len(), 10_000);
        // The second call is the cached allocation, not a fresh walk.
        assert!(Arc::ptr_eq(&rows, &cache.rows(&doc, page, &expanded, "")));
        let range = visible_window(rows.len(), 0.0, 400.0, PITCH);
        assert!(range.len() < 32, "{range:?}");
    }
}
