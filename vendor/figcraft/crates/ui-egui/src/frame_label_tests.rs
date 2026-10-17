//! Frame name labels on the canvas, driven through egui headless: an upright frame's label sits
//! just above its top-left corner, a rotated frame's runs along its turned top edge, and pressing
//! either label selects and drags its frame.

use egui::{Event, Pos2, pos2, vec2};
use figcraft_doc::NodeId;
use figcraft_engine::Session;
use figcraft_geom::Point;
use serde_json::json;

use crate::layers_drag_tests::Ui;

fn id(v: serde_json::Value) -> NodeId {
    figcraft_engine::params::id_of(&v["id"]).unwrap()
}

/// Frames "Upright" at (0, 0) and "Turned" at (400, 0), the second rotated 30°.
fn setup() -> (Ui, NodeId, NodeId) {
    let mut s = Session::new();
    let a = id(s.execute("frame.create", &json!({"x": 0, "y": 0, "width": 300, "height": 200, "name": "Upright"})).unwrap());
    let b = id(s.execute("frame.create", &json!({"x": 400, "y": 0, "width": 300, "height": 200, "name": "Turned"})).unwrap());
    s.execute("node.set", &json!({"id": b, "rotation": 30})).unwrap();
    let mut u = Ui::new(s);
    u.frame(vec![]);
    (u, a, b)
}

/// Frame `f`'s label on screen (corners) as the canvas published it.
fn label(u: &Ui, f: NodeId) -> [Pos2; 4] {
    let (_, q) = u.app.session.tool.labels.iter().find(|(id, _)| *id == f).expect("the frame has a label");
    q.map(|p| u.app.to_screen(p.x, p.y))
}

/// Frame `f`'s local point `(x, y)` on screen.
fn local(u: &Ui, f: NodeId, x: f64, y: f64) -> Pos2 {
    let p = u.app.session.doc().unwrap().world_transform(f) * Point::new(x, y);
    u.app.to_screen(p.x, p.y)
}

fn mid(q: &[Pos2; 4]) -> Pos2 {
    pos2(q.iter().map(|p| p.x).sum::<f32>() / 4.0, q.iter().map(|p| p.y).sum::<f32>() / 4.0)
}

#[test]
fn labels_run_along_the_frames_top_edge() {
    let (u, a, b) = setup();
    // Upright: a strip right above the top-left corner.
    let q = label(&u, a);
    let tl = local(&u, a, 0.0, 0.0);
    assert!((q[0].x - tl.x).abs() < 0.5 && q[0].y < tl.y && q[3].y < tl.y + 0.5, "{q:?} vs {tl:?}");
    assert!((q[0].y - q[1].y).abs() < 1e-3, "level");
    // Turned: the strip starts at the turned top-left corner and runs along the top edge.
    let q = label(&u, b);
    let (tl, tr) = (local(&u, b, 0.0, 0.0), local(&u, b, 300.0, 0.0));
    let along = (tr - tl).normalized();
    let top = (q[1] - q[0]).normalized();
    assert!((top - along).length() < 1e-3, "{top:?} along {along:?}");
    // Outside the frame: on the far side of the top edge from the frame's centre.
    let up = vec2(along.y, -along.x);
    assert!((mid(&q) - tl).dot(up) > 2.0);
    assert!((local(&u, b, 150.0, 100.0) - tl).dot(up) < 0.0);
    // Never longer than the edge.
    assert!((q[1] - q[0]).length() <= (tr - tl).length() + 0.5);
}

#[test]
fn pressing_a_rotated_label_selects_and_drags_the_frame() {
    let (mut u, _, b) = setup();
    let at = mid(&label(&u, b));
    let before = u.app.session.doc().unwrap().world_bounds(b).unwrap();
    let undo = u.app.session.ds().unwrap().history.undo.len();
    u.button(at, true);
    assert_eq!(u.app.session.selection(), vec![b]);
    for k in 1..=10 {
        u.frame(vec![Event::PointerMoved(at + vec2(4.0 * k as f32, 2.0 * k as f32))]);
    }
    u.button(at + vec2(40.0, 20.0), false);
    let after = u.app.session.doc().unwrap().world_bounds(b).unwrap();
    assert!(after.x0 > before.x0 + 1.0 && after.y0 > before.y0 + 1.0, "{before:?} → {after:?}");
    assert_eq!(u.app.session.ds().unwrap().history.undo.len(), undo + 1);
}
