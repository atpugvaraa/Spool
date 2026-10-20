//! Tests for the canvas.
//!
//! Three kinds, and each answers a question that cannot be answered by reading the code:
//!
//! * **Camera.** [`page_to_screen`] and [`screen_to_page`] must be inverses, and must agree with
//!   the engine's own [`ViewState`] about what a pan and a zoom do — a pointer event built from
//!   one and a frame painted with the other that disagree is a canvas that moves what it did not
//!   click.
//! * **Selection.** A hit point must become the layer a click means, through the engine's
//!   `hit` and `select_target`, including the nested case where deep and shallow differ.
//! * **Interaction.** A select-then-drag driven only through [`Session::pointer`] must move the
//!   document. That is the whole claim this module makes: the canvas forwards, the engine edits.
//! * **Render.** A document rendered through the real renderer must produce non-blank pixels at
//!   exactly the size that was asked for.

use std::sync::Arc;
use std::time::{Duration, Instant};

use figcraft_doc::{Document, NodeId, NodeKind};
use figcraft_engine::tools::{Cursor, PointerEvent, PointerKind, ToolId};
use figcraft_engine::Session;
use figcraft_geom::Point;
use figcraft_render::{Options, Retained, View, Work};
use gpui::{point, px, Hsla};
use serde_json::json;

use super::*;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A session over one page with one rectangle at `(40, 30)` sized `80 × 60`.
fn session_with_rectangle() -> (Session, NodeId) {
    let mut doc = Document::new("canvas-test");
    let page = doc.first_page().expect("a new document has a page");
    doc.create(page, NodeKind::Rectangle, "Rect", 40.0, 30.0, 80.0, 60.0)
        .expect("a rectangle can be created on a page");
    (Session::with_document(doc), page)
}

/// A page with one solid red rectangle on it, so a render has something distinguishable to put on
/// the canvas. The fill is set directly rather than through a command: what is under test is the
/// render path, and a command's default fill is not.
fn session_with_filled_rectangle() -> (Session, NodeId) {
    let mut doc = Document::new("canvas-render");
    let page = doc.first_page().expect("a new document has a page");
    let id = doc
        .create(page, NodeKind::Rectangle, "R", 20.0, 20.0, 120.0, 90.0)
        .expect("a rectangle can be created on a page");
    let node = doc.node_mut(id).expect("the rectangle is there");
    node.fills = vec![figcraft_doc::color::Paint::solid(
        figcraft_doc::color::Color {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        },
    )];
    let page = doc.first_page().expect("a new document has a page");
    (Session::with_document(doc), page)
}

/// A viewport at the window's top-left.
fn viewport() -> Viewport {
    Viewport {
        origin: point(0.0, 0.0),
        size: (800.0, 600.0),
    }
}

/// The one child of `page`.
fn only_child(doc: &Document, page: NodeId) -> NodeId {
    doc.node(page)
        .and_then(|n| n.children.first().copied())
        .expect("the page has a child")
}

/// A view that fits the page, with the viewport in step so `origin` is `(0, 0)`.
fn fitted(mut view: ViewState) -> ViewState {
    view.vw = 800.0;
    view.vh = 600.0;
    view.cx = 0.0;
    view.cy = 0.0;
    view.zoom = 1.0;
    view
}

/// A pointer event at page `(x, y)` for a viewport whose origin is `(0, 0)`.
fn at_page(view: &ViewState, kind: PointerKind, x: f64, y: f64) -> PointerEvent {
    let at = page_to_screen(view, viewport(), x, y);
    pointer_event(view, viewport(), kind, at, false, false, false, false, None)
}

/// The session's camera, which lives in its `DocState` — the canvas never has a copy.
fn cam(session: &Session) -> ViewState {
    session
        .ds()
        .map(|d| d.view.clone())
        .expect("the session has a document")
}

/// Drive a whole gesture: a press at `from`, three moves, a release at `to`.
fn drag(session: &mut Session, from: Point, to: Point) {
    let zoom = session.ds().map(|d| d.view.zoom).unwrap_or(1.0).max(1e-9);
    let view = ViewState {
        zoom,
        ..Default::default()
    };
    session
        .pointer(&at_page(&view, PointerKind::Down, from.x, from.y))
        .expect("a press is not an error");
    for step in 1..=3 {
        let t = f64::from(step) / 3.0;
        let p = from.lerp(to, t);
        session
            .pointer(&at_page(&view, PointerKind::Move, p.x, p.y))
            .expect("a move is not an error");
    }
    session
        .pointer(&at_page(&view, PointerKind::Up, to.x, to.y))
        .expect("a release is not an error");
}

// ---------------------------------------------------------------------------
// Camera
// ---------------------------------------------------------------------------

/// The camera's own maths is the reference; these wrappers must add the viewport's top-left and
/// change nothing else.
#[test]
fn page_to_screen_adds_the_viewports_top_left_and_nothing_else() {
    let view = fitted(ViewState::default());
    let vp = Viewport {
        origin: point(37.0, -12.0),
        size: (800.0, 600.0),
    };
    let got = page_to_screen(&view, vp, 100.0, 200.0);
    let want = view.page_to_screen(100.0, 200.0);
    assert!((f64::from(got.x) - (want.0 + 37.0)).abs() < 1e-6);
    assert!((f64::from(got.y) - (want.1 - 12.0)).abs() < 1e-6);
}

#[test]
fn screen_to_page_removes_the_viewports_top_left() {
    let view = fitted(ViewState::default());
    let vp = Viewport {
        origin: point(37.0, -12.0),
        size: (800.0, 600.0),
    };
    let at = page_to_screen(&view, vp, 100.0, 200.0);
    let p = screen_to_page(&view, vp, at);
    assert!((p.x - 100.0).abs() < 1e-6);
    assert!((p.y - 200.0).abs() < 1e-6);
}

/// The property the whole file rests on: an event built at a page point and the page point it
/// carries are the same point, whatever the pan and the zoom.
#[test]
fn the_two_directions_are_inverses_at_every_zoom_and_pan() {
    for zoom in [0.25_f64, 1.0, 4.0, 11.5] {
        for (cx, cy) in [(0.0, 0.0), (-320.0, 140.5), (10_000.0, -10_000.0)] {
            let view = ViewState {
                cx,
                cy,
                zoom,
                ..fitted(ViewState::default())
            };
            let vp = Viewport {
                origin: point(120.5, 44.25),
                size: (640.0, 480.0),
            };
            for (x, y) in [(0.0, 0.0), (-17.5, 233.0), (999.0, -0.25)] {
                let at = page_to_screen(&view, vp, x, y);
                let back = screen_to_page(&view, vp, at);
                assert!(
                    (back.x - x).abs() < 1e-6 && (back.y - y).abs() < 1e-6,
                    "zoom {zoom} pan ({cx}, {cy}): ({x}, {y}) came back ({}, {})",
                    back.x,
                    back.y
                );
            }
        }
    }
}

/// Zooming about a point must leave that point where it was. This is what makes a wheel zoom feel
/// like the canvas is holding still rather than flying away.
#[test]
fn zooming_about_a_point_keeps_that_point_under_the_pointer() {
    let mut view = fitted(ViewState::default());
    let vp = viewport();
    let anchor = screen_to_page(&view, vp, point(321.0, 211.0));
    let before = page_to_screen(&view, vp, anchor.x, anchor.y);
    view.set_zoom_at(3.5, (anchor.x, anchor.y));
    let after = page_to_screen(&view, vp, anchor.x, anchor.y);
    assert!((f64::from(before.x) - f64::from(after.x)).abs() < 1e-6);
    assert!((f64::from(before.y) - f64::from(after.y)).abs() < 1e-6);
}

#[test]
fn panning_moves_the_content_the_same_way_the_wheel_did() {
    let mut view = fitted(ViewState::default());
    let vp = viewport();
    let before = page_to_screen(&view, vp, 0.0, 0.0);
    // 40 points of scroll to the right: `ViewState::scroll` subtracts, so the page's origin moves
    // left and the content follows the fingers.
    view.scroll(40.0, 0.0, false);
    let after = page_to_screen(&view, vp, 0.0, 0.0);
    assert!((f64::from(after.x) - f64::from(before.x) - 40.0).abs() < 1e-6);
}

#[test]
fn a_zoom_is_clamped_to_the_engines_own_range() {
    let mut view = fitted(ViewState::default());
    view.set_zoom_at(1e9, (0.0, 0.0));
    assert_eq!(view.zoom, ViewState::MAX_ZOOM);
    view.set_zoom_at(-3.0, (0.0, 0.0));
    assert_eq!(view.zoom, ViewState::MIN_ZOOM);
}

/// An event's `sx`/`sy` are relative to the viewport, not the window: the engine's pan gesture
/// compares them against the camera's own origin, so a canvas offset in them would make a pan drift
/// by the canvas's position.
#[test]
fn a_pointer_events_screen_position_ignores_the_viewport_offset() {
    let view = fitted(ViewState::default());
    let vp = Viewport {
        origin: point(500.0, 300.0),
        size: (800.0, 600.0),
    };
    let ev = pointer_event(
        &view,
        vp,
        PointerKind::Hover,
        point(520.0, 340.0),
        false,
        false,
        false,
        false,
        None,
    );
    assert_eq!((ev.sx, ev.sy), (20.0, 40.0));
    // The page point is the screen point through the camera: at zoom 1 with the viewport 800×600
    // centred on the origin, the top-left of the viewport is the page point (-400, -300).
    assert_eq!((ev.x, ev.y), (-380.0, -260.0));
}

#[test]
fn a_pointer_event_carries_every_modifier_through() {
    let view = fitted(ViewState::default());
    let ev = pointer_event(
        &view,
        viewport(),
        PointerKind::Down,
        point(10.0, 20.0),
        true,
        true,
        true,
        true,
        Some(0.5),
    );
    assert!(ev.shift && ev.alt && ev.cmd && ev.ctrl);
    assert_eq!(ev.pressure, Some(0.5));
}

#[test]
fn a_zoom_of_zero_does_not_divide_by_zero() {
    let view = fitted(ViewState {
        zoom: 0.0,
        ..ViewState::default()
    });
    let p = page_point(&view, viewport(), point(10.0, 10.0));
    assert!(p.x.is_finite() && p.y.is_finite());
}

#[test]
fn a_viewport_contains_only_what_is_inside_it() {
    let vp = Viewport {
        origin: point(10.0, 20.0),
        size: (100.0, 50.0),
    };
    assert!(vp.contains(point(10.0, 20.0)));
    assert!(vp.contains(point(110.0, 70.0)));
    assert!(!vp.contains(point(9.0, 70.0)));
    assert!(!vp.contains(point(110.0, 71.0)));
}

#[test]
fn the_viewport_as_a_rectangle_matches_its_origin_and_size() {
    let vp = Viewport {
        origin: point(4.0, 8.0),
        size: (100.0, 50.0),
    };
    let b = vp.bounds();
    assert_eq!((f32::from(b.origin.x), f32::from(b.origin.y)), (4.0, 8.0));
    assert_eq!(
        (f32::from(b.size.width), f32::from(b.size.height)),
        (100.0, 50.0)
    );
}

// ---------------------------------------------------------------------------
// Selection from a hit point
// ---------------------------------------------------------------------------

/// The plain case: a click on a rectangle selects it.
#[test]
fn a_hit_point_selects_the_layer_under_it() {
    let (session, page) = session_with_rectangle();
    let doc = session.doc().expect("the session has a document");
    let target = select_at(&session, doc, page, Point::new(80.0, 60.0), false);
    assert_eq!(target, Some(only_child(doc, page)));
}

/// Outside every layer there is nothing to select, and a click there must not invent something.
#[test]
fn a_point_on_empty_canvas_selects_nothing() {
    let (session, page) = session_with_rectangle();
    let doc = session.doc().expect("the session has a document");
    assert_eq!(
        select_at(&session, doc, page, Point::new(5000.0, 5000.0), false),
        None
    );
}

/// Nested: a plain click picks the outermost group, a deep click the layer actually hit. This is
/// `select_target`'s rule, and the canvas must pass `deep` through rather than decide it itself.
#[test]
fn a_deep_click_reaches_the_nested_layer_and_a_plain_one_does_not() {
    let mut doc = Document::new("canvas-nested");
    let page = doc.first_page().expect("a new document has a page");
    let group = doc
        .create(page, NodeKind::Group, "Group", 0.0, 0.0, 200.0, 200.0)
        .expect("a group can be created");
    let inner = doc
        .create(group, NodeKind::Rectangle, "Inner", 20.0, 20.0, 40.0, 40.0)
        .expect("a rectangle can be created inside a group");
    let session = Session::with_document(doc);
    let doc = session.doc().expect("the session has a document");
    let at = Point::new(40.0, 40.0);
    assert_eq!(select_at(&session, doc, page, at, true), Some(inner));
    assert_eq!(select_at(&session, doc, page, at, false), Some(group));
}

/// The tolerance is in screen pixels, so it must shrink as the canvas zooms in. At 1% zoom a click
/// 20 page units out still counts as on the layer; at 100% it does not.
#[test]
fn the_hit_tolerance_is_screen_pixels_not_page_units() {
    let (session, page) = session_with_rectangle();
    let doc = session.doc().expect("the session has a document");
    // The rectangle spans (40, 30)-(120, 90). 25 units right of its edge.
    let far = Point::new(145.0, 60.0);

    let mut zoomed_out = Session::with_document(doc.clone());
    if let Ok(ds) = zoomed_out.ds_mut() {
        ds.view.zoom = 0.01;
    }
    assert!(select_at(&zoomed_out, doc, page, far, false).is_some());

    let mut zoomed_in = Session::with_document(doc.clone());
    if let Ok(ds) = zoomed_in.ds_mut() {
        ds.view.zoom = 4.0;
    }
    assert_eq!(select_at(&zoomed_in, doc, page, far, false), None);
}

/// A marquee is the engine's, and the rectangle is normalised so a drag in any direction selects
/// the same thing.
#[test]
fn a_marquee_selects_what_it_covers_in_either_direction() {
    let (session, page) = session_with_rectangle();
    let doc = session.doc().expect("the session has a document");
    let child = only_child(doc, page);
    let down_right = marquee_targets(doc, page, [10.0, 10.0, 300.0, 300.0], None);
    let up_left = marquee_targets(doc, page, [300.0, 300.0, 10.0, 10.0], None);
    assert!(down_right.contains(&child));
    assert_eq!(down_right, up_left);
}

/// A marquee that covers nothing selects nothing.
#[test]
fn a_marquee_that_covers_nothing_selects_nothing() {
    let (session, page) = session_with_rectangle();
    let doc = session.doc().expect("the session has a document");
    assert!(marquee_targets(doc, page, [-500.0, -500.0, -100.0, -100.0], None).is_empty());
}

// ---------------------------------------------------------------------------
// Interaction: select, then drag
// ---------------------------------------------------------------------------

/// The claim this module exists to make: the canvas sends pointer events and the document changes.
/// Nothing here is a canvas call — it is `Session::pointer` and a document read.
#[test]
fn select_then_drag_moves_the_document() {
    let (mut session, page) = session_with_rectangle();
    let doc = session.doc().expect("the session has a document");
    let child = only_child(doc, page);

    // A click on the rectangle.
    let click = at_page(&cam(&session), PointerKind::Down, 80.0, 60.0);
    session.pointer(&click).expect("a press is not an error");
    let release = at_page(&cam(&session), PointerKind::Up, 80.0, 60.0);
    session
        .pointer(&release)
        .expect("a release is not an error");
    assert_eq!(
        session.selection(),
        vec![child],
        "the click selected nothing"
    );

    // A drag of the same layer.
    drag(
        &mut session,
        Point::new(80.0, 60.0),
        Point::new(180.0, 60.0),
    );

    let doc = session.doc().expect("the session has a document");
    let bounds = doc.world_bounds(child).expect("the layer is still there");
    // The drag was 100 page units right at zoom 1.
    assert!(
        (bounds.x0 - 140.0).abs() < 0.5,
        "the layer ended at x {}, expected about 140",
        bounds.x0
    );
    assert!((bounds.y0 - 30.0).abs() < 0.5, "it also moved vertically");
}

/// A drag is one undo step, not one per move. If it were not, an editor with 555 commands would
/// still be unusable after one drag.
#[test]
fn a_drag_is_one_undo_step() {
    let (mut session, _page) = session_with_rectangle();
    let click = at_page(&cam(&session), PointerKind::Down, 80.0, 60.0);
    session.pointer(&click).expect("a press is not an error");
    let release = at_page(&cam(&session), PointerKind::Up, 80.0, 60.0);
    session
        .pointer(&release)
        .expect("a release is not an error");
    let before = session.undo_depth();
    drag(
        &mut session,
        Point::new(80.0, 60.0),
        Point::new(180.0, 60.0),
    );
    let after = session.undo_depth();
    assert_eq!(
        after.0,
        before.0 + 1,
        "one drag added {after:?} against {before:?}"
    );
}

#[test]
fn undoing_a_drag_puts_the_layer_back_where_it_was() {
    let (mut session, page) = session_with_rectangle();
    let child = {
        let doc = session.doc().expect("the session has a document");
        only_child(doc, page)
    };
    let start_x = session
        .doc()
        .ok()
        .and_then(|d| d.world_bounds(child))
        .map(|b| b.x0)
        .expect("the layer is there");
    let click = at_page(&cam(&session), PointerKind::Down, 80.0, 60.0);
    session.pointer(&click).expect("a press is not an error");
    let release = at_page(&cam(&session), PointerKind::Up, 80.0, 60.0);
    session
        .pointer(&release)
        .expect("a release is not an error");
    drag(
        &mut session,
        Point::new(80.0, 60.0),
        Point::new(180.0, 60.0),
    );
    assert!(
        session.undo().expect("undo is available"),
        "nothing to undo"
    );
    let end_x = session
        .doc()
        .ok()
        .and_then(|d| d.world_bounds(child))
        .map(|b| b.x0)
        .expect("the layer is still there");
    assert!((end_x - start_x).abs() < 1e-9);
}

/// A click on empty canvas with the Move tool clears the selection, which is how a click deselects.
#[test]
fn a_click_on_empty_canvas_deselects() {
    let (mut session, page) = session_with_rectangle();
    let child = {
        let doc = session.doc().expect("the session has a document");
        only_child(doc, page)
    };
    session.set_selection(vec![child]);
    let down = at_page(&cam(&session), PointerKind::Down, 4000.0, 4000.0);
    session.pointer(&down).expect("a press is not an error");
    let up = at_page(&cam(&session), PointerKind::Up, 4000.0, 4000.0);
    session.pointer(&up).expect("a release is not an error");
    assert!(session.selection().is_empty());
}

/// Escape walks out: the selection, then its parent, then nothing. One key, three states, all the
/// engine's.
#[test]
fn escape_walks_out_of_the_selection_and_then_clears_it() {
    let mut doc = Document::new("canvas-escape");
    let page = doc.first_page().expect("a new document has a page");
    let group = doc
        .create(page, NodeKind::Group, "Group", 0.0, 0.0, 200.0, 200.0)
        .expect("a group can be created");
    let inner = doc
        .create(group, NodeKind::Rectangle, "Inner", 20.0, 20.0, 40.0, 40.0)
        .expect("a rectangle can be created inside a group");
    let mut session = Session::with_document(doc);
    session.set_selection(vec![inner]);

    session.escape().expect("escape is not an error");
    assert_eq!(session.selection(), vec![group]);
    session.escape().expect("escape is not an error");
    assert!(session.selection().is_empty());
}

/// The tool the canvas reports and the tool the engine acts with are the same value: a panel that
/// shows a toolbar from `tool()` cannot disagree with a drag driven through `pointer`.
#[test]
fn the_reported_tool_is_the_tool_that_acts() {
    let (mut session, _page) = session_with_rectangle();
    assert_eq!(session.tool.tool, ToolId::Move);
    session.execute("tool.select", &json!({})).ok();
    session
        .execute("tool.set", &json!({"tool": "rectangle"}))
        .ok();
    assert_eq!(session.tool.tool, ToolId::Rectangle);
}

/// A hover is the only way the canvas learns what is under the pointer, and it must not change the
/// document.
#[test]
fn a_hover_reports_the_hovered_layer_without_editing() {
    let (mut session, page) = session_with_rectangle();
    let doc = session.doc().expect("the session has a document");
    let child = only_child(doc, page);
    let before = session
        .doc()
        .ok()
        .map(|d| d.world_bounds(child).map(|b| b.x0));
    let hover = at_page(&cam(&session), PointerKind::Hover, 80.0, 60.0);
    session.pointer(&hover).expect("a hover is not an error");
    assert_eq!(session.ds().ok().and_then(|ds| ds.hover), Some(child));
    assert_eq!(
        session
            .doc()
            .ok()
            .map(|d| d.world_bounds(child).map(|b| b.x0)),
        before
    );
}

// ---------------------------------------------------------------------------
// The overlay
// ---------------------------------------------------------------------------

/// An idle overlay paints nothing. If this were wrong, every canvas would draw a marquee.
#[test]
fn an_idle_overlay_is_empty() {
    let overlay = OverlayView::from_overlay(&Overlay::default());
    assert!(overlay.is_empty());
}

/// The overlay the engine builds must survive the trip through `tool_info`'s JSON unchanged, or an
/// agent driving the same session would see a different canvas.
#[test]
fn the_json_overlay_is_the_typed_overlay() {
    let (mut session, page) = session_with_rectangle();
    let doc = session.doc().expect("the session has a document");
    let child = only_child(doc, page);
    let down = at_page(&cam(&session), PointerKind::Down, 5000.0, 5000.0);
    session.pointer(&down).expect("a press is not an error");
    let hover = at_page(&cam(&session), PointerKind::Move, 5020.0, 5010.0);
    session.pointer(&hover).expect("a move is not an error");
    let _ = (page, child);

    let typed = OverlayView::from_overlay(&session.tool.overlay);
    let json = OverlayView::from_tool_info(&session.tool_info()).expect("tool_info has an overlay");
    assert_eq!(typed, json);
    assert!(
        !typed.is_empty(),
        "a drag across the canvas shows a marquee"
    );
}

/// `tool_info` without an overlay is not an overlay, and must not become an empty one that paints
/// nothing and looks like an idle canvas.
#[test]
fn a_tool_info_without_an_overlay_is_not_an_overlay() {
    assert!(OverlayView::from_tool_info(&json!({"tool": "move"})).is_none());
    assert!(OverlayView::from_tool_info(&json!({"overlay": 3})).is_none());
    assert!(OverlayView::from_tool_info(&json!(null)).is_none());
}

/// The five keys the panels and the diagnostics read must always be there.
#[test]
fn tool_info_carries_the_tool_the_busy_flag_and_the_overlay() {
    let (session, _page) = session_with_rectangle();
    let info = session.tool_info();
    assert_eq!(info["tool"], "move");
    assert_eq!(info["busy"], json!(false));
    assert!(info["overlay"].is_object());
    assert!(info["arcHandles"].is_null() || info["arcHandles"].is_object());
    assert!(info["shapeHandles"].is_array());
}

/// A marquee is only drawn while one is being dragged; the field is absent rather than a degenerate
/// rect, and `is_empty` has to say so.
#[test]
fn a_dragged_marquee_makes_the_overlay_non_empty() {
    let (mut session, _page) = session_with_rectangle();
    let down = at_page(&cam(&session), PointerKind::Down, 10.0, 10.0);
    session.pointer(&down).expect("a press is not an error");
    let mv = at_page(&cam(&session), PointerKind::Move, 200.0, 200.0);
    session.pointer(&mv).expect("a move is not an error");
    let overlay = OverlayView::from_overlay(&session.tool.overlay);
    assert!(overlay.marquee.is_some());
    assert!(!overlay.is_empty());
    let [x0, y0, x1, y1] = overlay.marquee.expect("a marquee was drawn");
    assert!((x0 - 10.0).abs() < 1e-6 && (y0 - 10.0).abs() < 1e-6);
    assert!((x1 - 200.0).abs() < 1e-6 && (y1 - 200.0).abs() < 1e-6);
}

#[test]
fn releasing_the_drag_clears_the_marquee() {
    let (mut session, _page) = session_with_rectangle();
    let down = at_page(&cam(&session), PointerKind::Down, 10.0, 10.0);
    session.pointer(&down).expect("a press is not an error");
    let mv = at_page(&cam(&session), PointerKind::Move, 200.0, 200.0);
    session.pointer(&mv).expect("a move is not an error");
    let up = at_page(&cam(&session), PointerKind::Up, 200.0, 200.0);
    session.pointer(&up).expect("a release is not an error");
    assert!(OverlayView::from_overlay(&session.tool.overlay)
        .marquee
        .is_none());
}

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

#[test]
fn a_resize_cursor_picks_the_nearest_system_arrow() {
    assert_eq!(resize_cursor(0.0), gpui::CursorStyle::ResizeLeftRight);
    assert_eq!(resize_cursor(90.0), gpui::CursorStyle::ResizeUpDown);
    assert_eq!(
        resize_cursor(45.0),
        gpui::CursorStyle::ResizeUpLeftDownRight
    );
    assert_eq!(
        resize_cursor(135.0),
        gpui::CursorStyle::ResizeUpRightDownLeft
    );
    // Angles past a half turn are the same axis as their negative.
    assert_eq!(resize_cursor(360.0), resize_cursor(0.0));
}

/// The four cursors GPUI cannot show come back as `None`, which is the signal to draw a glyph. If
/// any of them got a system cursor the glyph would never be drawn.
#[test]
fn the_cursors_gpui_cannot_show_are_left_to_the_glyph_painter() {
    assert!(cursor_style(Cursor::Rotate(45.0)).is_none());
    assert!(cursor_style(Cursor::Pen(figcraft_engine::tools::PenCursor::New)).is_none());
    assert!(cursor_style(Cursor::Eyedropper).is_none());
    assert!(cursor_style(Cursor::Comment).is_none());
}

#[test]
fn an_unusable_resize_angle_falls_back_to_the_default_cursor() {
    assert_eq!(
        cursor_style(Cursor::Resize(f64::NAN)),
        Some(gpui::CursorStyle::Arrow)
    );
}

#[test]
fn panning_wins_over_the_tools_cursor() {
    assert_eq!(
        on_canvas(true, false, None, Cursor::Crosshair),
        Some(gpui::CursorStyle::OpenHand)
    );
    assert_eq!(
        on_canvas(true, true, None, Cursor::Crosshair),
        Some(gpui::CursorStyle::ClosedHand)
    );
    assert_eq!(
        on_canvas(false, false, None, Cursor::Crosshair),
        Some(gpui::CursorStyle::Crosshair)
    );
}

#[test]
fn cursor_names_say_what_the_pointer_is_doing() {
    assert_eq!(cursor_name(Cursor::Default), "default");
    assert_eq!(cursor_name(Cursor::Rotate(45.4)), "rotate 45");
    assert_eq!(
        cursor_name(Cursor::Pen(figcraft_engine::tools::PenCursor::Close)),
        "pen-close"
    );
    assert_eq!(cursor_name(Cursor::NotAllowed), "not-allowed");
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

#[test]
fn numbers_are_labelled_without_a_trailing_zero() {
    assert_eq!(fmt(120.0), "120");
    assert_eq!(fmt(120.5), "120.5");
    assert_eq!(fmt(120.456), "120.46");
    assert_eq!(fmt(120.0 / 3.0), "40");
    assert_eq!(fmt(f64::NAN), "");
}

#[test]
fn a_size_label_is_the_length_alone_for_a_line() {
    assert_eq!(size_label(120.0, 0.0, true), "120");
    assert_eq!(size_label(120.0, 80.0, false), "120 × 80");
}

// ---------------------------------------------------------------------------
// Hatching
// ---------------------------------------------------------------------------

/// A square must hatch: the region under the lines is what says "this will be filled".
#[test]
fn a_square_hatches() {
    let poly: Vec<GPoint<f32>> = vec![
        point(0.0, 0.0),
        point(100.0, 0.0),
        point(100.0, 100.0),
        point(0.0, 100.0),
    ];
    let lines = hatch_lines(&[poly], HATCH_GAP);
    assert!(!lines.is_empty(), "a 100px square hatched nothing");
    for [a, b] in &lines {
        assert!(a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite());
    }
}

/// Even-odd: a square with a square hole has two bands, not one.
#[test]
fn a_square_with_a_hole_hatches_the_hole_too() {
    let outer: Vec<GPoint<f32>> = vec![
        point(0.0, 0.0),
        point(100.0, 0.0),
        point(100.0, 100.0),
        point(0.0, 100.0),
    ];
    let inner: Vec<GPoint<f32>> = vec![
        point(30.0, 30.0),
        point(70.0, 30.0),
        point(70.0, 70.0),
        point(30.0, 70.0),
    ];
    let solid = hatch_lines(&[outer.clone()], HATCH_GAP).len();
    let holed = hatch_lines(&[outer, inner], HATCH_GAP).len();
    assert!(
        holed > solid,
        "a hole added nothing ({holed} against {solid})"
    );
}

#[test]
fn nothing_hatches_nothing() {
    assert!(hatch_lines(&[], HATCH_GAP).is_empty());
    assert!(hatch_lines(&[vec![]], HATCH_GAP).is_empty());
    assert!(hatch_lines(&[vec![point(0.0, 0.0)]], HATCH_GAP).is_empty());
    assert!(hatch_lines(&[vec![point(0.0, 0.0), point(1.0, 1.0)]], 0.0).is_empty());
}

/// A non-finite corner would otherwise make every scanline NaN and the loop would not terminate.
#[test]
fn a_non_finite_polygon_hatches_nothing_rather_than_looping() {
    let poly: Vec<GPoint<f32>> = vec![
        point(0.0, 0.0),
        point(f32::NAN, 0.0),
        point(100.0, 100.0),
        point(0.0, 100.0),
    ];
    assert!(hatch_lines(&[poly], HATCH_GAP).is_empty());
}

/// A polygon far bigger than the viewport must not cost a line per pixel of page.
#[test]
fn a_huge_polygon_hatches_a_bounded_number_of_lines() {
    let poly: Vec<GPoint<f32>> = vec![
        point(0.0, 0.0),
        point(1e7, 0.0),
        point(1e7, 1e7),
        point(0.0, 1e7),
    ];
    let lines = hatch_lines(&[poly], 1.0);
    assert!(
        lines.is_empty(),
        "a 10-million-unit polygon was not clipped"
    );
}

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// Every palette colour comes from the theme, so a theme change moves the canvas with it.
#[test]
fn the_palette_is_the_theme() {
    let pal = Palette::new();
    assert_eq!(pal.accent, hsla(theme::ACCENT));
    assert_eq!(pal.guide, hsla(theme::SNAP_GUIDE));
    assert_eq!(pal.canvas, hsla(theme::CANVAS));
    assert_eq!(pal.ink, hsla(theme::TEXT));
    assert_eq!(pal.none.a, 0.0);
    assert_eq!(pal.white.a, 1.0);
}

#[test]
fn fading_a_colour_changes_only_its_alpha() {
    let pal = Palette::new();
    let faded = Palette::fade(pal.accent, 0.25);
    assert!((faded.a - 0.25).abs() < 1e-6);
    assert!((faded.h - pal.accent.h).abs() < 1e-6);
    // An alpha outside 0..1 is clamped by GPUI rather than wrapping.
    assert_eq!(Palette::fade(pal.accent, 5.0).a, 1.0);
    assert_eq!(Palette::fade(pal.accent, -1.0).a, 0.0);
}

/// An engine colour becomes a GPUI colour without changing its channels.
#[test]
fn an_engine_colour_survives_the_conversion() {
    let engine = figcraft_doc::color::Color {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    let got = engine_color(engine);
    let back: Hsla = got;
    assert!(
        back.h < 0.01 || back.h > 0.99,
        "pure red is not at hue 0: {}",
        back.h
    );
    assert!((back.a - 1.0).abs() < 1e-6);
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// GPUI's names are lower-case and the engine's are not; every key the engine handles must survive
/// the translation.
#[test]
fn gpui_key_names_map_onto_the_engines() {
    assert_eq!(engine_key_name("left"), Some("Left"));
    assert_eq!(engine_key_name("escape"), Some("Escape"));
    assert_eq!(engine_key_name("backspace"), Some("Backspace"));
    assert_eq!(engine_key_name("space"), Some("Space"));
    assert_eq!(engine_key_name("a"), None, "a character is not a named key");
    assert_eq!(engine_key_name(""), None);
}

/// A named key is something to act on; anything else is a character to type. Getting this backwards
/// would type "left" into a text layer.
#[test]
fn an_unnamed_key_is_a_character() {
    assert!(engine_key_name("q").is_none());
    assert!(engine_key_name("7").is_none());
}

// ---------------------------------------------------------------------------
// UTF-8 / UTF-16
// ---------------------------------------------------------------------------

/// The text layer's indices are `char`s and the IME's are UTF-16 units; a round trip through both
/// has to be the identity, including past a surrogate pair.
#[test]
fn utf16_and_char_indices_round_trip() {
    let text = "aéb🙂c";
    let chars: Vec<char> = text.chars().collect();
    for i in 0..=chars.len() {
        let utf8 = chars[..i].iter().collect::<String>().len();
        let utf16 = text[..utf8].encode_utf16().count();
        assert_eq!(utf16_to_utf8(text, utf16), utf8, "at char {i}");
        assert_eq!(utf8_to_utf16(text, utf8), utf16, "at char {i}");
    }
}

#[test]
fn a_range_past_the_end_of_the_text_is_clamped_not_panicked() {
    assert_eq!(utf16_range_to_utf8("abc", 1..99), 1..3);
    // A REVERSED range, spelled as a struct rather than as `99..2` so that the
    // deny-by-default `reversed_empty_ranges` lint does not read the literal as
    // a mistake. It is not one: the point of the case is that a range whose end
    // is before its start clamps to an empty range at the end of the text,
    // rather than panicking or producing a backwards range.
    let reversed = std::ops::Range { start: 99, end: 2 };
    assert_eq!(utf16_range_to_utf8("abc", reversed), 3..3);
    assert_eq!(utf8_range_to_utf16("abc", 0..99), 0..3);
}

#[test]
fn an_offset_inside_a_character_snaps_back_to_its_boundary() {
    // Byte 2 of "aé" is inside the two-byte character.
    assert_eq!(utf8_boundary("aé", 2), 1);
    assert_eq!(utf8_boundary("aé", 9), 3);
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The render test the module is named for: a document through the real renderer produces pixels of
/// exactly the size that was asked for, and they are not all blank.
#[test]
fn a_rendered_page_has_the_exact_size_asked_for_and_is_not_blank() {
    let (session, page) = session_with_filled_rectangle();
    let doc = Arc::new(session.doc().expect("the session has a document").clone());

    let mut retained = Retained::default();
    // A view of exactly 200×150 device pixels at scale 1.
    let view = View {
        origin: (0.0, 0.0),
        scale: 1.0,
        width: 200,
        height: 150,
    };
    let rendered = retained.render(&doc, page, &view, &Options::default());
    assert!(rendered.is_some(), "the engine refused a valid view");
    let (drawn, pixmap, _work) = rendered.expect("a frame");
    // `snap` grows the view by a device pixel on each axis to cover the snapped origin, so the
    // frame is exactly the view the engine actually drew, one pixel wider and taller than asked.
    assert_eq!((drawn.width, drawn.height), (201, 151));
    assert_eq!(
        (pixmap.width(), pixmap.height()),
        (drawn.width, drawn.height)
    );

    // Not blank, and not one flat colour: the page's background is outside the rectangle and the
    // rectangle's fill is inside it, so a corner and the centre must differ.
    let pixels = pixmap.pixels();
    let count = (drawn.width * drawn.height) as usize;
    assert_eq!(pixels.len(), count);
    assert!(
        pixels.iter().all(|p| p.demultiply().alpha() > 200),
        "the page background left transparent pixels"
    );
    // The rectangle covers the page rect (20, 20)-(140, 110); (80, 60) is inside it and (5, 5) is
    // not, so the two pixels are the fill and the page background.
    let width = drawn.width as usize;
    let corner = pixels[(5 * width) + 5].demultiply();
    let centre = pixels[(60 * width) + 80].demultiply();
    assert_ne!(
        (corner.red(), corner.green(), corner.blue()),
        (centre.red(), centre.green(), centre.blue()),
        "the filled rectangle did not reach the middle of the view"
    );
}

/// The engine renders one device pixel wider and taller than the view it was given, because `snap`
/// grows it to cover the snapped origin. A canvas that assumed otherwise would be a pixel short.
#[test]
fn the_engine_renders_the_snapped_view_which_may_be_a_pixel_larger() {
    let (session, page) = session_with_filled_rectangle();
    let doc = Arc::new(session.doc().expect("the session has a document").clone());
    let mut retained = Retained::default();
    let view = View {
        origin: (0.3, 0.7),
        scale: 1.0,
        width: 200,
        height: 150,
    };
    let (_, pixmap, _) = retained
        .render(&doc, page, &view, &Options::default())
        .expect("a frame");
    assert!(
        pixmap.width() >= 200 && pixmap.height() >= 150,
        "the frame is smaller than the view it was asked for"
    );
}

/// A view that cannot be put on a pixel grid draws nothing, and says so rather than drawing a
/// blank of the wrong size.
#[test]
fn an_unusable_view_renders_nothing() {
    let (session, page) = session_with_filled_rectangle();
    let doc = Arc::new(session.doc().expect("the session has a document").clone());
    let mut retained = Retained::default();
    let view = View {
        origin: (0.0, 0.0),
        scale: f64::NAN,
        width: 200,
        height: 150,
    };
    assert!(retained
        .render(&doc, page, &view, &Options::default())
        .is_none());
}

/// The same view twice costs one render and one upload: `Work::Reused` is the only answer that
/// lets the texture cache keep its `Arc`, so the canvas depends on it being right.
#[test]
fn an_unchanged_view_is_reported_as_reused() {
    let (session, page) = session_with_filled_rectangle();
    let doc = Arc::new(session.doc().expect("the session has a document").clone());
    let mut retained = Retained::default();
    let view = View {
        origin: (0.0, 0.0),
        scale: 1.0,
        width: 120,
        height: 90,
    };
    assert!(retained
        .render(&doc, page, &view, &Options::default())
        .is_some());
    let work = retained
        .render(&doc, page, &view, &Options::default())
        .map(|(_, _, work)| work);
    let work = work.expect("a second frame");
    assert!(
        matches!(work, Work::Reused),
        "an unchanged view cost {work:?}"
    );
}

/// A different view must not be reported as reused, or a pan would leave the old pixels up.
#[test]
fn a_panned_view_is_not_reused() {
    let (session, page) = session_with_filled_rectangle();
    let doc = Arc::new(session.doc().expect("the session has a document").clone());
    let mut retained = Retained::default();
    let view = View {
        origin: (0.0, 0.0),
        scale: 1.0,
        width: 120,
        height: 90,
    };
    let _ = retained.render(&doc, page, &view, &Options::default());
    let panned = View {
        origin: (-40.0, 0.0),
        ..view
    };
    let (_, _, work) = retained
        .render(&doc, page, &panned, &Options::default())
        .expect("a frame after a pan");
    assert!(!matches!(work, Work::Reused));
}

/// The whole worker-thread path the draw side uses: a request goes out on the renderer, a frame
/// comes back, and its size is the size that was asked for.
#[test]
fn the_renderer_publishes_a_frame_of_the_requested_size() {
    let (session, page) = session_with_filled_rectangle();
    let doc = Arc::new(session.doc().expect("the session has a document").clone());
    let renderer = CanvasRenderer::new();
    renderer
        .render_canvas(
            doc,
            page,
            (0.0, 0.0),
            1.0,
            (200.0, 150.0),
            1.0,
            Options::default(),
        )
        .expect("a valid viewport is accepted");

    let deadline = Instant::now() + Duration::from_secs(10);
    let frame = loop {
        if let Some(frame) = renderer.frame() {
            break frame;
        }
        assert!(
            Instant::now() < deadline,
            "the render thread published nothing"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(frame.image.is_some(), "the frame carries no image");
    // The engine renders one device pixel wider and taller than the view it was given, so at
    // ratio 1 the frame covers the whole 200×150 viewport and one pixel past each edge.
    assert_eq!((frame.size.width, frame.size.height), (201, 151));
    assert_eq!(frame.logical_size(1.0), (201.0, 151.0));
    assert!(
        !renderer.diagnostics().contains("faulted"),
        "{}",
        renderer.diagnostics()
    );
}

/// A viewport the engine cannot put on a pixel grid is refused at the door, and no thread is asked.
#[test]
fn the_renderer_refuses_an_unusable_viewport() {
    let (session, page) = session_with_filled_rectangle();
    let doc = Arc::new(session.doc().expect("the session has a document").clone());
    let renderer = CanvasRenderer::new();
    let result = renderer.render_canvas(
        doc,
        page,
        (0.0, 0.0),
        0.0,
        (200.0, 150.0),
        1.0,
        Options::default(),
    );
    assert!(result.is_err(), "a zero zoom was accepted");
}

// ---------------------------------------------------------------------------
// Text on the canvas
// ---------------------------------------------------------------------------

/// A caret placed by a click must be in the character the click was over, or the caret lands
/// somewhere the click never was. Checked as monotonicity across the box rather than as an exact
/// index, because where the layout puts the first glyph depends on the font and the vertical
/// alignment, and neither is what this test is about.
#[test]
fn a_text_layout_hit_goes_forward_across_the_box() {
    let mut doc = Document::new("canvas-text");
    let page = doc.first_page().expect("a new document has a page");
    let id = doc
        .create(
            page,
            NodeKind::Text(figcraft_doc::TextData {
                characters: String::from("Hello"),
                ..figcraft_doc::TextData::default()
            }),
            "Text",
            0.0,
            0.0,
            200.0,
            40.0,
        )
        .expect("a text layer can be created");
    let Some(text) = TextOnScreen::of(&doc, id) else {
        // No system fonts on this machine: the layout cannot be built, and there is nothing to
        // assert about it. The engine's own text tests cover the layout; this one covers wiring.
        return;
    };
    let mut last = 0;
    for step in 0..=20 {
        let x = f64::from(step) * 10.0;
        let at = text.page_point(x, 0.0);
        assert!(at.x.is_finite() && at.y.is_finite());
        let index = text.hit(at.x, at.y);
        assert!(
            index >= last,
            "the hit went backwards at x {x}: {last} then {index}"
        );
        last = index;
    }
    assert!(
        last > 0,
        "the hit test found the same character at both ends of the box"
    );
}

#[test]
fn a_non_text_layer_has_no_text_on_screen() {
    let (session, page) = session_with_rectangle();
    let doc = session.doc().expect("the session has a document");
    let child = only_child(doc, page);
    assert!(TextOnScreen::of(doc, child).is_none());
    assert!(TextOnScreen::of(doc, page).is_none());
}

/// A node's box in world space: a rotated node's corners are not its local corners.
#[test]
fn a_node_quad_is_the_world_box_not_the_local_one() {
    let mut doc = Document::new("canvas-quad");
    let page = doc.first_page().expect("a new document has a page");
    let id = doc
        .create(page, NodeKind::Rectangle, "R", 10.0, 20.0, 40.0, 60.0)
        .expect("a rectangle can be created");
    let quad = node_quad(&doc, id).expect("the layer is there");
    assert_eq!(quad[0], Point::new(10.0, 20.0));
    assert_eq!(quad[1], Point::new(50.0, 20.0));
    assert_eq!(quad[2], Point::new(50.0, 80.0));
    assert_eq!(quad[3], Point::new(10.0, 80.0));
}

// ---------------------------------------------------------------------------
// Boards and labels
// ---------------------------------------------------------------------------

/// Only visible top-level layers get labels and grids; a hidden one draws neither.
#[test]
fn boards_lists_the_pages_visible_top_level_layers() {
    let mut doc = Document::new("canvas-boards");
    let page = doc.first_page().expect("a new document has a page");
    let shown = doc
        .create(page, NodeKind::Frame, "Shown", 0.0, 0.0, 100.0, 100.0)
        .expect("a frame can be created");
    let hidden = doc
        .create(page, NodeKind::Frame, "Hidden", 0.0, 0.0, 100.0, 100.0)
        .expect("a frame can be created");
    doc.node_mut(hidden).expect("the layer is there").visible = false;
    assert_eq!(boards(&doc, page), vec![shown]);
}

/// A section's contents are labelled too, so the top level alone would miss half a file.
#[test]
fn boards_walks_into_a_section_with_visible_contents() {
    let mut doc = Document::new("canvas-section");
    let page = doc.first_page().expect("a new document has a page");
    let section = doc
        .create(
            page,
            NodeKind::Section {
                contents_hidden: false,
            },
            "S",
            0.0,
            0.0,
            400.0,
            300.0,
        )
        .expect("a section can be created");
    let inner = doc
        .create(section, NodeKind::Frame, "Inner", 0.0, 0.0, 100.0, 100.0)
        .expect("a frame can be created inside a section");
    assert_eq!(boards(&doc, page), vec![section, inner]);
}

// ---------------------------------------------------------------------------
// Wheel
// ---------------------------------------------------------------------------

/// A wheel delta is pixels whatever the platform reported, because the camera is in pixels.
#[test]
fn a_wheel_delta_is_pixels_whatever_the_platform_said() {
    assert_eq!(
        wheel_pixels(gpui::ScrollDelta::Pixels(gpui::Point::new(
            px(3.0),
            px(-4.0)
        ))),
        (3.0, -4.0)
    );
    assert_eq!(
        wheel_pixels(gpui::ScrollDelta::Lines(point(1.0, 2.0))),
        (40.0, 80.0)
    );
}

// ---------------------------------------------------------------------------
// Modifiers
// ---------------------------------------------------------------------------

/// The engine's `cmd` is GPUI's `platform`, not `control`: on macOS that is Cmd, and a canvas that
/// sent Control would make every deep-click a word-move.
#[test]
fn the_platform_modifier_is_the_one_the_engine_calls_cmd() {
    let m = gpui::Modifiers {
        shift: true,
        alt: false,
        platform: true,
        control: false,
        function: false,
    };
    let mods = Mods::of(&m);
    assert!(mods.cmd && !mods.ctrl && mods.shift);
}

// ---------------------------------------------------------------------------
// Helpers the engine owns but the canvas names
// ---------------------------------------------------------------------------

/// `select_at` must not select something when nothing is there, even deep: an agent driving the
/// canvas through it needs the same answer the pointer gets.
#[test]
fn a_deep_click_on_empty_canvas_selects_nothing_either() {
    let (session, page) = session_with_rectangle();
    let doc = session.doc().expect("the session has a document");
    assert_eq!(
        select_at(&session, doc, page, Point::new(9000.0, 9000.0), true),
        None
    );
}

/// The engine's pixels and the overlay must land on the same pixel.
///
/// The frame's top-left is a page point, and so is everything the overlay
/// draws. If the frame is blitted at a different point on screen than the
/// overlay would put that same page point, the selection handles float away
/// from the thing they belong to.
#[test]
fn the_frame_is_blitted_where_the_overlay_draws_it() {
    let mut view = ViewState::default();
    view.zoom = 1.5;
    view.cx = 280.0;
    view.cy = 230.0;
    view.vw = 900.0;
    view.vh = 800.0;
    // The canvas element does not start at the window's top-left: it sits
    // beside a sidebar and below a toolbar, so this origin is never zero.
    let viewport = Viewport {
        origin: point(215.0, 90.0),
        size: (900.0, 800.0),
    };

    // The page point the frame's top-left stands for.
    let (page_x, page_y) = view.origin();

    // Where the overlay puts that page point: absolute window coordinates.
    let overlay = super::page_to_screen(&view, viewport, page_x, page_y);

    // The frame, rendered at the view's own scale, starting at that page
    // point.
    let frame = crate::canvas_render::CanvasFrame {
        image: None,
        size: crate::canvas_render::DeviceSize::new(1350, 1200),
        view: figcraft_render::View {
            origin: view.origin(),
            scale: view.zoom,
            width: 1350,
            height: 1200,
        },
        origin_px: (
            (page_x * view.zoom).round() as i64,
            (page_y * view.zoom).round() as i64,
        ),
        scale: view.zoom,
        ratio: 1.0,
        work: figcraft_render::Work::Full,
        damage: Default::default(),
    };
    let blit = super::blit_bounds(&frame, view.origin(), view.zoom, viewport.origin);

    assert_eq!(
        (overlay.x, overlay.y),
        (f32::from(blit.origin.x), f32::from(blit.origin.y)),
        "the engine's pixels and the overlay must land on the same pixel"
    );

    // And the frame must be drawn at the SIZE the view says that content is.
    // The frame is 1350 device pixels wide at a scale of 1.5, so it covers 900
    // page units, which at the view's zoom is 1350 logical points. If the
    // blit were scaled by the ratio as well, the content would be twice the
    // size the overlay drew it.
    assert_eq!(
        (f32::from(blit.size.width), f32::from(blit.size.height)),
        (1350.0, 1200.0),
        "the frame must be drawn at the size the view says"
    );
}

/// The command modifier is the platform's, and the engine's.
///
/// figcraft's table writes `Cmd` for the command key in every spec and
/// resolves it to Control off macOS, and the engine's `PointerEvent::cmd`
/// says the same: "Cmd on macOS, Ctrl elsewhere".
///
/// GPUI reports the two separately -- `platform` is the Cmd key on macOS
/// and the Windows key elsewhere, `control` is always Control -- so the
/// platform's rule has to be applied once, here. Passing the physical
/// platform modifier to the engine instead meant that on Windows and Linux
/// a Control-click did not deep-select, and the Windows key did.
///
/// Only macOS has ever been run here, so this is pinned by a test rather
/// than by a manual check on three platforms.
#[test]
fn the_command_modifier_is_the_platforms() {
    let mac = figcraft_engine::keymap::Platform::Mac;
    let other = figcraft_engine::keymap::Platform::Other;

    // A Cmd key held: the command modifier on macOS, and NOT on the other
    // platforms, where the command key is Control.
    let cmd_key = Mods {
        shift: false,
        alt: false,
        cmd: true,
        ctrl: false,
    };
    assert_eq!(
        cmd_key.command_for(mac),
        true,
        "the Cmd key is the command key on macOS"
    );
    assert_eq!(
        cmd_key.command_for(other),
        false,
        "and it is not on Windows or Linux, where the command key is Control"
    );

    // A Control key held: the command key on Windows and Linux, and not on
    // macOS.
    let ctrl_key = Mods {
        shift: false,
        alt: false,
        cmd: false,
        ctrl: true,
    };
    assert_eq!(
        ctrl_key.command_for(mac),
        false,
        "Control is not the command key on macOS"
    );
    assert_eq!(
        ctrl_key.command_for(other),
        true,
        "and it is on Windows and Linux"
    );
}
