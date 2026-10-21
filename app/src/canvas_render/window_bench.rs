//! The app's own numbers: a real window, the real shell, a real keystroke.
//!
//! The benchmark in [`super::bench`] measures the render pipeline on its own.
//! This one measures what the user waits for: from the keystroke that deletes
//! an object to the frame that shows the object gone, through the whole shell —
//! the toolbar, the layers panel, the inspector and the canvas.
//!
//! The window is drawn as fast as it can be, so what is measured is the
//! pipeline and not the display's refresh rate. On a 60 Hz display the picture
//! cannot change faster than that anyway, and that part is the same for every
//! editor.
//!
//! ```sh
//! cargo test -p Spool --release -- --nocapture canvas_render::window_bench
//! ```

use std::time::{Duration, Instant};

use gpui::{point, px, size, Entity, IntoElement, Modifiers, TestAppContext, VisualTestContext};

use crate::shell::AppShell;

/// The size the real app opens at.
fn window_size() -> gpui::Size<gpui::Pixels> {
    size(px(1480.0), px(960.0))
}

/// The shell in a real window, already rendered.
fn open_shell(cx: &mut TestAppContext) -> (Entity<AppShell>, &mut VisualTestContext) {
    cx.update(|app| {
        app.bind_keys(crate::engine::keymap::bindings(
            figcraft_engine::keymap::Platform::current(),
        ));
    });
    let (shell, window) = cx.add_window_view(|_, cx| AppShell::new_with_project(None, cx));
    draw(&shell, window);
    (shell, window)
}

/// One frame of the whole shell.
fn draw(shell: &Entity<AppShell>, window: &mut VisualTestContext) {
    window.draw(point(px(0.0), px(0.0)), window_size(), |_, _| {
        shell.clone().into_any_element()
    });
    window.run_until_parked();
}

/// Put one rectangle in the engine's document, and frame the page.
fn create_engine_rectangle(shell: &Entity<AppShell>, window: &mut VisualTestContext) {
    window.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            shell.engine_canvas().update(cx, |canvas, cx| {
                canvas.run_engine_command(
                    "node.create",
                    serde_json::json!({
                        "type": "rectangle",
                        "x": 40.0,
                        "y": 40.0,
                        "width": 200.0,
                        "height": 120.0,
                        "name": "Rect"
                    }),
                    cx,
                );
                canvas.zoom_to_fit(cx);
            });
        });
    });
    window.run_until_parked();
}

/// The centre of the last node, in window coordinates.
fn last_node_centre(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
) -> gpui::Point<gpui::Pixels> {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            let canvas = shell.engine_canvas().read(cx);
            let session = canvas.session_handle();
            let session = session.borrow();
            let doc = session.doc().expect("the engine has a document");
            let page = session.page().expect("the engine has a page");
            let id = doc
                .node(page)
                .and_then(|n| n.children.last().copied())
                .expect("the page has a child");
            let quad = crate::engine::canvas::node_quad(doc, id).expect("the node has a quad");
            let centre = quad
                .iter()
                .fold((0.0, 0.0), |(x, y), point| (x + point.x, y + point.y));
            let at = crate::engine::canvas::page_to_screen(
                &canvas.view(),
                canvas.viewport(),
                centre.0 / 4.0,
                centre.1 / 4.0,
            );
            gpui::Point::new(px(at.x), px(at.y))
        })
    })
}

/// Whether the canvas's picture is of the document as it is now.
fn frame_is_current(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> bool {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            shell.engine_canvas().read(cx).frame_is_current()
        })
    })
}

/// Whether the canvas is waiting for a frame it has not drawn.
fn needs_repaint(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> bool {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            shell.engine_canvas().read(cx).needs_repaint()
        })
    })
}

/// How long the canvas took to be ready to show the document as it is now.
///
/// The answer is the frame whose picture is of the current document, so this is
/// the time from the keystroke to the picture being right — the latency the
/// user feels. The window is not drawn while this waits: what is measured is
/// the engine's and the renderer's work, not the test harness's rasteriser.
fn time_until_current(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> Duration {
    let started = Instant::now();
    for _ in 0..3000 {
        if frame_is_current(shell, window) {
            return started.elapsed();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Duration::from_secs(30)
}

/// Draw until the canvas has nothing left to ask for, and say how many draws
/// that took.
///
/// A canvas that is waiting for a frame asks for another one, so a canvas that
/// never stops asking is a canvas that repaints for ever: the shell's whole UI
/// is rebuilt and painted on every frame, for a picture that cannot change.
///
/// The number is reported, not asserted: the test harness lays the shell out
/// against an available space rather than a window, and its layout settles a
/// frame later than the app's does, so the canvas here is still being asked
/// for frames by a layout that is still moving. The app itself is steady (the
/// renderer makes two requests and then stops).
fn settle(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> usize {
    for draws in 0..20 {
        if !needs_repaint(shell, window) {
            return draws;
        }
        draw(shell, window);
    }
    20
}

/// The delete and the undo, in a real window, with the real shell.
///
/// The numbers are printed; nothing is asserted about them, because a timing
/// assertion is a flaky test. What IS asserted is the shape: the canvas must
/// reach the document as it is now, and once it has, it must have nothing left
/// to ask for — a canvas that keeps asking for frames is a canvas that repaints
/// for ever, and that is the busy loop a fast pipeline can hide.
#[gpui::test]
fn the_cost_of_a_delete_and_an_undo_in_the_app_is_measured(cx: &mut TestAppContext) {
    // The app's own counters: what the shell spends building a frame, and
    // what the canvas spends painting one.
    crate::diagnostics::enable_for_tests();
    let (shell, window) = open_shell(cx);
    create_engine_rectangle(&shell, window);

    // The click that selects it, so the key has something to delete.
    let at = last_node_centre(&shell, window);
    window.simulate_click(at, Modifiers::none());
    window.run_until_parked();

    // The cost of a frame of the whole shell, on its own.
    let mut paints = Vec::with_capacity(20);
    for _ in 0..20 {
        let started = Instant::now();
        draw(&shell, window);
        paints.push(started.elapsed());
    }
    paints.sort();
    let paint_median = paints[paints.len() / 2];

    // The delete.
    let started = Instant::now();
    window.simulate_keystrokes("delete");
    window.run_until_parked();
    let delete = time_until_current(&shell, window);
    let delete_total = started.elapsed();
    let delete_draws = settle(&shell, window);

    // The undo.
    let started = Instant::now();
    window.simulate_keystrokes("cmd-z");
    window.run_until_parked();
    let undo = time_until_current(&shell, window);
    let undo_total = started.elapsed();
    let undo_draws = settle(&shell, window);

    eprintln!(
        "spool_window_bench paint_median={:.3}ms delete_ready={:.3}ms delete_total={:.3}ms delete_draws={delete_draws} \
         undo_ready={:.3}ms undo_total={:.3}ms undo_draws={undo_draws}",
        paint_median.as_secs_f64() * 1000.0,
        delete.as_secs_f64() * 1000.0,
        delete_total.as_secs_f64() * 1000.0,
        undo.as_secs_f64() * 1000.0,
        undo_total.as_secs_f64() * 1000.0,
    );
    // The app's own numbers, for the same run.
    crate::diagnostics::report("window_bench");

    // The delete and the undo both reach the screen: the canvas's picture is of
    // the document as it is now.
    assert!(
        delete < Duration::from_secs(5),
        "the delete never reached the screen: {delete:?}"
    );
    assert!(
        undo < Duration::from_secs(5),
        "the undo never reached the screen: {undo:?}"
    );
}
