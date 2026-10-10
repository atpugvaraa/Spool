//! Interaction tests that need a live GPUI window.
//!
//! The behaviours in here exist only once there is a real `Window` behind them:
//! which element holds keyboard focus, which nodes a key event is dispatched to,
//! and whether a click's hit test reaches the canvas at all. A plain unit test
//! cannot observe any of it, because a plain unit test holds the `Window` that
//! would do the work and then never lets it.
//!
//! So these use GPUI's own `VisualTestContext`: a real window, the real shell
//! rendered into it, and `simulate_click` / `simulate_keystrokes` driving the
//! same dispatch code the packaged app uses. A test that passes here is evidence
//! about the app, not about a stand-in for it.
//!
//! The split this file maintains:
//!
//!   * `canvas::tests` — logic that needs no window: geometry, default sizes,
//!     history depth, document and source effects. Fast, and most of the suite.
//!   * this file — the seams between that logic and GPUI. Few, and each one
//!     bought by a behaviour that was otherwise untestable.
//!
//! Two rules for adding to this file:
//!
//!   1. Assert what the user would see, not which function ran. "The object is
//!      gone and the document says so" is a test; "`delete_selected_objects` was
//!      called once" is not.
//!   2. If a test can be written without a window, it belongs in `canvas::tests`.
//!      Windows are slow, and a window test that proves something a plain test
//!      could prove is a test that will be slow for no reason.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use gpui::{point, px, size, Entity, IntoElement, Modifiers, TestAppContext, VisualTestContext};

use crate::canvas::CanvasView;
use crate::shell::AppShell;

/// The size the real app opens at.
///
/// Same size as the app so that a point which lands on an object in a test is a
/// point that lands on that object in the app, rather than a coincidence of the
/// test's own layout.
fn window_size() -> gpui::Size<gpui::Pixels> {
    size(px(1480.0), px(960.0))
}

/// Render the shell into the window.
///
/// `draw` is what produces the rendered frame that hit testing reads. Skipping it
/// means every `simulate_click` lands on empty space and fails in a way that
/// looks exactly like "the canvas ignores clicks" — so this is not optional
/// setup, it is part of what is under test.
fn redraw(shell: &Entity<AppShell>, window: &mut VisualTestContext) {
    window.draw(point(px(0.0), px(0.0)), window_size(), |_, _| {
        shell.clone().into_any_element()
    });
    window.run_until_parked();
}

/// Read one value out of the canvas, from outside its module.
fn read<R>(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
    f: impl Fn(&CanvasView) -> R,
) -> R {
    window.update(|_, cx| shell.read_with(cx, |shell, _| f(shell.canvas().read(cx))))
}

/// Whether the canvas holds keyboard focus right now.
///
/// Separate from `read` because it needs the `Window`, which only exists inside
/// `VisualTestContext::update`.
fn holds_focus(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> bool {
    window.update(|window, cx| {
        shell.read_with(cx, |shell, _| {
            shell.canvas().read(cx).holds_keyboard_focus(window)
        })
    })
}

/// A window-coordinate point that lands on the middle of an object.
///
/// Derived from the camera rather than written down. A hard-coded coordinate
/// stops pointing at its object the moment the layout, zoom, or starter scene
/// changes, and the test then goes on passing while clicking empty canvas —
/// which is the worst way for a test like this to fail.
fn centre_of(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
    index: usize,
) -> gpui::Point<gpui::Pixels> {
    let origin = window
        .debug_bounds("canvas-viewport")
        .expect("the canvas viewport should have been laid out")
        .origin;
    let local = read(shell, window, |canvas| canvas.object_screen_center(index))
        .unwrap_or_else(|| panic!("the starter scene should have an object at index {index}"));
    origin + gpui::Point::new(px(local.x), px(local.y))
}

/// Keystroke spelling note, since it is easy to get wrong.
///
/// `Keystroke::parse` splits modifiers on `-`, not `+`. `"cmd-z"` is the Command
/// key and the letter z; `"cmd+z"` parses as a single key literally named
/// `cmd+z`, which matches nothing and silently does nothing. The app's own
/// bindings in `main.rs` use hyphens for the same reason.
const UNDO: &str = "cmd-z";
const REDO: &str = "cmd-shift-z";

/// Click the middle of an object, then let the app settle.
fn click_object(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
    index: usize,
    modifiers: Modifiers,
) {
    let target = centre_of(shell, window, index);
    window.simulate_click(target, modifiers);
    window.run_until_parked();
}

/// A shell in a real window, already rendered, ready to receive events.
///
/// Takes the context and returns the shell plus a reborrowable handle, because
/// `add_window_view` lends out the window for as long as the test lives and a
/// helper cannot hold that borrow across the calls that follow.
fn open_shell(cx: &mut TestAppContext) -> (Entity<AppShell>, &mut VisualTestContext) {
    let (shell, window) = cx.add_window_view(|_, cx| AppShell::new_with_project(None, cx));
    redraw(&shell, window);
    (shell, window)
}

#[gpui::test]
fn the_shell_renders_and_lays_out_a_window(cx: &mut TestAppContext) {
    // The smallest possible proof that this facility works for Spool at all, and
    // the reason the tests below can be believed: if the real shell could not be
    // constructed and drawn here, every other test in this file would be
    // measuring nothing at all.
    let (shell, window) = open_shell(cx);
    assert!(
        window.debug_bounds("canvas-viewport").is_some(),
        "the canvas viewport must be laid out for a click to have anywhere to land"
    );
    assert!(
        read(&shell, window, |canvas| canvas.document_objects().len()) > 0,
        "the starter scene should be present"
    );
}

#[gpui::test]
fn clicking_a_canvas_object_gives_the_canvas_keyboard_focus(cx: &mut TestAppContext) {
    // This is the whole reason editor shortcuts were dead on the canvas. GPUI
    // dispatches a key event only to the nodes on the path from the focused node
    // up to the root. With nothing focused that path is the dispatch tree's
    // synthetic root alone, which carries none of the shell's listeners, so
    // Delete and Backspace were silently discarded — while the same keys worked
    // from Layers, because a Layers row focuses itself when clicked.
    let (shell, window) = open_shell(cx);

    assert!(
        !holds_focus(&shell, window),
        "the canvas starts without keyboard focus"
    );

    click_object(&shell, window, 0, Modifiers::none());

    assert!(
        holds_focus(&shell, window),
        "a pointer interaction on the canvas must leave it holding keyboard focus"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.selection().ids().len()),
        1,
        "and the object under the pointer should be selected"
    );
}

/// The shared body of the two deletion tests below.
///
/// Both spellings get their own `#[gpui::test]` because they are two keystrokes
/// arriving at one command, and either could stop being routed on its own. They
/// share this body so the two cannot drift apart in what they check.
fn a_pressed_key_deletes_the_clicked_object(cx: &mut TestAppContext, key: &str) {
    // The full chain, driven by real events and nothing stubbed at the seams:
    //
    //   click -> canvas takes focus -> the key event is dispatched to the shell's
    //   on_key_down -> Command::Delete -> the same semantic deletion Layers uses
    //   -> one history entry
    let (shell, window) = open_shell(cx);
    let before = read(&shell, window, |canvas| canvas.document_objects().len());

    click_object(&shell, window, 0, Modifiers::none());
    assert!(
        holds_focus(&shell, window),
        "{key}: without focus this key would go nowhere, which is the bug"
    );

    window.simulate_keystrokes(key);
    window.run_until_parked();

    assert_eq!(
        read(&shell, window, |canvas| canvas.document_objects().len()),
        before - 1,
        "{key} must remove the selected object"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.test_history().undo_len()),
        1,
        "{key} must record exactly one history entry"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.selection().ids().len()),
        0,
        "{key} must leave nothing selected"
    );
}

#[gpui::test]
fn delete_key_removes_the_clicked_object(cx: &mut TestAppContext) {
    a_pressed_key_deletes_the_clicked_object(cx, "delete");
}

#[gpui::test]
fn backspace_removes_the_clicked_object(cx: &mut TestAppContext) {
    a_pressed_key_deletes_the_clicked_object(cx, "backspace");
}

#[gpui::test]
fn deleting_several_selected_objects_is_one_history_entry(cx: &mut TestAppContext) {
    // Shift-click multi-selection, because a deletion that looped per object
    // would pass every single-object test above while writing N history entries.
    let (shell, window) = open_shell(cx);
    let before = read(&shell, window, |canvas| canvas.document_objects().len());
    assert!(before >= 2, "this test needs two objects to select");

    click_object(&shell, window, 0, Modifiers::none());
    click_object(&shell, window, 1, Modifiers::shift());
    assert_eq!(
        read(&shell, window, |canvas| canvas.selection().ids().len()),
        2,
        "shift-click should add to the selection"
    );

    window.simulate_keystrokes("delete");
    window.run_until_parked();

    assert_eq!(
        read(&shell, window, |canvas| canvas.document_objects().len()),
        before - 2,
        "both selected objects should be gone"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.test_history().undo_len()),
        1,
        "two objects, one gesture, one history entry"
    );
}

#[gpui::test]
fn undo_and_redo_restore_and_reapply_a_keyboard_deletion(cx: &mut TestAppContext) {
    // Undo and redo are key-driven too, so they travel the same dispatch path and
    // are only as reachable as Delete was. Asserting Delete without these would
    // leave half the history contract unproven on the canvas.
    let (shell, window) = open_shell(cx);
    let before = read(&shell, window, |canvas| canvas.document_objects().len());
    let identity = read(&shell, window, |canvas| {
        canvas.document_objects()[0].spool_id.clone()
    });

    click_object(&shell, window, 0, Modifiers::none());
    window.simulate_keystrokes("delete");
    window.run_until_parked();
    assert_eq!(
        read(&shell, window, |canvas| canvas.document_objects().len()),
        before - 1
    );

    window.simulate_keystrokes(UNDO);
    window.run_until_parked();
    assert_eq!(
        read(&shell, window, |canvas| canvas.document_objects().len()),
        before,
        "undo must put the object back"
    );
    assert!(
        read(&shell, window, |canvas| canvas
            .document_objects()
            .iter()
            .any(|object| object.spool_id == identity)),
        "and bring back the same authored identity, not a new object"
    );

    window.simulate_keystrokes(REDO);
    window.run_until_parked();
    assert_eq!(
        read(&shell, window, |canvas| canvas.document_objects().len()),
        before - 1,
        "redo must delete it again"
    );
}
// ── Gap B: the Text tool's handover into a text session ──────────────────────
//
// Everything here used to be untestable, because all of it happens in the
// pointer-up handler: the tool has to still be Text when that handler runs, so
// that it can tell a fresh Text object from anything else that happens to be
// selected, and the session it opens has to survive the tool changing
// immediately afterwards.

/// A point on empty canvas, far enough from everything that a click lands on no
/// object.
///
/// Found by scanning the viewport in screen space, not by writing a coordinate
/// down and not by scanning the world: a fixed point stops being empty the first
/// time the starter scene changes, and a world-space scan happily returns points
/// that are nowhere on screen, so the click lands on the shell instead of the
/// canvas. Screen space is the only frame in which "somewhere empty on the canvas"
/// is a question with an answer.
fn empty_canvas_point(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
) -> gpui::Point<gpui::Pixels> {
    let bounds = window
        .debug_bounds("canvas-viewport")
        .expect("the canvas viewport should have been laid out");
    let width = bounds.size.width;
    let height = bounds.size.height;

    let found = read(shell, window, |canvas| {
        let objects: Vec<(gpui::Point<f32>, gpui::Size<f32>)> = canvas
            .document_objects()
            .iter()
            .map(|object| {
                (
                    canvas.world_to_canvas_screen(object.position),
                    gpui::size(object.size.width, object.size.height),
                )
            })
            .collect();
        let step = 25.0_f32;
        let (limit_x, limit_y) = (f32::from(width), f32::from(height));
        let mut x = step;
        while x < limit_x - step {
            let mut y = step;
            while y < limit_y - step {
                let candidate = gpui::point(x, y);
                // A margin, so the click cannot land on an edge.
                let clear = objects.iter().all(|(origin, extent)| {
                    !(candidate.x > origin.x - 20.0
                        && candidate.x < origin.x + extent.width + 20.0
                        && candidate.y > origin.y - 20.0
                        && candidate.y < origin.y + extent.height + 20.0)
                });
                if clear {
                    return Some(candidate);
                }
                y += step;
            }
            x += step;
        }
        None
    })
    .expect("the scene should leave some empty canvas");
    bounds.origin + gpui::Point::new(px(found.x), px(found.y))
}

/// Every tool the toolbar renders, with the label a test can click by.
///
/// The selector strings are built once and deliberately leaked: `debug_bounds`
/// takes a `&'static str`, and a handful of short strings held for the life of
/// the test process is a smaller cost than threading a lifetime through every
/// helper that has to name a button.
fn toolbar_tools() -> &'static [(&'static str, crate::canvas::Tool, &'static str)] {
    static TOOLS: OnceLock<Vec<(&'static str, crate::canvas::Tool, &'static str)>> =
        OnceLock::new();
    TOOLS.get_or_init(|| {
        crate::shell::TOOLS
            .iter()
            .map(|(_, label, _, tool)| {
                (
                    *label,
                    *tool,
                    Box::leak(format!("tool-{label}").into_boxed_str()) as &'static str,
                )
            })
            .collect()
    })
}

/// The `debug_bounds` selector for a tool button.
fn tool_selector(label: &str) -> &'static str {
    toolbar_tools()
        .iter()
        .find(|(candidate, _, _)| *candidate == label)
        .map(|(_, _, selector)| *selector)
        .unwrap_or_else(|| panic!("the toolbar should offer {label}"))
}

/// Put the canvas on a tool by clicking the real toolbar button.
///
/// Through the button rather than `set_tool`, so the test also proves the
/// toolbar is wired to the canvas at all.
fn choose_tool(window: &mut VisualTestContext, label: &str) {
    let button = window
        .debug_bounds(tool_selector(label))
        .unwrap_or_else(|| panic!("the toolbar should have a {label} button"));
    window.simulate_click(button.center(), Modifiers::none());
    window.run_until_parked();
}

/// Get the pending frame painted, so a freshly registered input handler is live.
///
/// Typing does not go through the mouse path. A mouse event makes GPUI redraw if
/// the window is dirty before dispatching it, which is why clicks work without
/// ceremony; a text input goes straight to whatever handler the last painted
/// frame offered. Opening a session registers the canvas's input handler during
/// paint, so that frame has to land before anything can be typed into it.
///
/// A mouse move is used because it is the cheapest event that forces the
/// redraw, and because it has no other effect worth asserting here.
fn flush_frame(window: &mut VisualTestContext, at: &gpui::Point<gpui::Pixels>) {
    window.simulate_mouse_move(*at, None, Modifiers::none());
    window.run_until_parked();
}

/// The text of the most recently created object, if it is a Text.
fn last_text(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> Option<String> {
    read(shell, window, |canvas| {
        canvas
            .document_objects()
            .last()
            .and_then(|object| object.text_content.clone())
    })
}

#[gpui::test]
fn a_text_tool_click_opens_a_session_and_hands_the_tool_back(cx: &mut TestAppContext) {
    // The handover, end to end. Both halves matter and they are adjacent: if the
    // tool changed before the pointer-up handler ran, the handler would not
    // recognise the object it had just made and no session would open; if the
    // tool changed by going through `set_tool`, that call commits an open text
    // edit and the session would close again the instant it opened.
    let (shell, window) = open_shell(cx);
    choose_tool(window, "Text");
    let before = read(&shell, window, |canvas| canvas.document_objects().len());
    let undo_before = read(&shell, window, |canvas| canvas.test_history().undo_len());

    let spot = empty_canvas_point(&shell, window);
    window.simulate_click(spot, Modifiers::none());
    window.run_until_parked();

    assert_eq!(
        read(&shell, window, |canvas| canvas.document_objects().len()),
        before + 1,
        "a Text-tool click should create a text object"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.selection().ids().len()),
        1,
        "and select it"
    );
    assert!(
        read(&shell, window, |canvas| canvas.is_text_editing()),
        "and open a session on it, so the caret can be typed into"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.tool()),
        crate::canvas::Tool::Select,
        "the creation is over, so the tool goes back to Selection"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.test_history().undo_len()),
        undo_before + 1,
        "and the whole thing is one history entry"
    );
}

#[gpui::test]
fn typing_after_a_text_click_is_committed_when_the_session_ends(cx: &mut TestAppContext) {
    // Typing does not write to the object: it edits the session's buffer, and the
    // object changes when the session ends. Both halves are asserted, because
    // only checking the object would pass with a session that silently threw
    // every keystroke away, and only checking the session would pass with text
    // that never reached the document.
    //
    // The caret lands at the end of the placeholder text a new Text object is
    // created with, so typed characters append to it.
    let (shell, window) = open_shell(cx);
    choose_tool(window, "Text");

    let spot = empty_canvas_point(&shell, window);
    window.simulate_click(spot, Modifiers::none());
    window.run_until_parked();
    flush_frame(window, &spot);

    let undo_before_typing = read(&shell, window, |canvas| canvas.test_history().undo_len());
    window.simulate_input("Hello");
    window.run_until_parked();
    assert_eq!(
        last_text(&shell, window).as_deref(),
        Some("Type something"),
        "typing edits the open session, not the document yet"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.test_history().undo_len()),
        undo_before_typing,
        "and records nothing while the session is open"
    );

    // Click an existing object, which ends the session.
    click_object(&shell, window, 0, Modifiers::none());

    assert!(
        !read(&shell, window, |canvas| canvas.is_text_editing()),
        "clicking away should end the session"
    );
    assert_eq!(
        last_text(&shell, window).as_deref(),
        Some("Type somethingHello"),
        "and the typed text should have been committed to the object"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.test_history().undo_len()),
        undo_before_typing + 1,
        "committing the session is its own history entry"
    );
}

#[gpui::test]
fn escape_discards_a_text_session_and_leaves_the_object_alone(cx: &mut TestAppContext) {
    // The other end of the session: Escape is a cancel, not a commit. Asserted
    // through the real event path so that the rung that answers it — the text
    // rung, ahead of the tool rung — is what is being tested.
    let (shell, window) = open_shell(cx);
    choose_tool(window, "Text");

    let spot = empty_canvas_point(&shell, window);
    window.simulate_click(spot, Modifiers::none());
    window.run_until_parked();
    flush_frame(window, &spot);
    window.simulate_input("Hello");
    window.run_until_parked();

    let undo_before = read(&shell, window, |canvas| canvas.test_history().undo_len());
    window.simulate_keystrokes("escape");
    window.run_until_parked();

    assert!(
        !read(&shell, window, |canvas| canvas.is_text_editing()),
        "Escape should end the session"
    );
    assert_eq!(
        last_text(&shell, window).as_deref(),
        Some("Type something"),
        "and discard what was typed rather than commit it"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.test_history().undo_len()),
        undo_before,
        "a discarded session is not a history entry"
    );
}

// ── Gap C: one tool authority, and the toolbar that reports it ────────────────

/// Which tools the shell would show as active right now.
///
/// Reads the same expression the two toolbars render with, so this is the
/// toolbar's answer and not a re-derivation of it.
fn highlighted_tools(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> Vec<String> {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            toolbar_tools()
                .iter()
                .filter(|(_, tool, _)| shell.test_tool_is_highlighted(*tool, cx))
                .map(|(label, _, _)| label.to_string())
                .collect()
        })
    })
}

#[gpui::test]
fn every_toolbar_button_selects_its_tool_and_highlights_only_itself(cx: &mut TestAppContext) {
    // Clicking the real buttons, for each tool the toolbar offers. The point is
    // not that the click works — it is that afterwards exactly one button claims
    // to be active, and it is the one the canvas is actually using. Two
    // independently mutable tool states cannot satisfy that.
    for (label, tool, _) in toolbar_tools().iter().copied() {
        let (shell, window) = open_shell(cx);
        choose_tool(window, label);

        assert_eq!(
            read(&shell, window, |canvas| canvas.tool()),
            tool,
            "clicking {label} should put the canvas on that tool"
        );
        assert_eq!(
            highlighted_tools(&shell, window),
            vec![label.to_string()],
            "and {label} should be the only highlighted tool"
        );
    }
}

#[gpui::test]
fn the_toolbar_starts_on_selection(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    assert_eq!(
        read(&shell, window, |canvas| canvas.tool()),
        crate::canvas::Tool::Select
    );
    assert_eq!(
        highlighted_tools(&shell, window),
        vec!["Select".to_string()],
        "a fresh window should highlight Selection and nothing else"
    );
}

#[gpui::test]
fn a_successful_creation_returns_the_toolbar_to_selection(cx: &mut TestAppContext) {
    // Stale toolbar state is the failure this guards: the click ends a creation,
    // the canvas changes tool, and if the toolbar were reading anything else the
    // button would still be lit for a tool the user is no longer holding.
    for label in ["Frame", "Rectangle", "Ellipse"] {
        let (shell, window) = open_shell(cx);
        choose_tool(window, label);

        let spot = empty_canvas_point(&shell, window);
        window.simulate_click(spot, Modifiers::none());
        window.run_until_parked();

        assert_eq!(
            read(&shell, window, |canvas| canvas.tool()),
            crate::canvas::Tool::Select,
            "creating with {label} should end up on Selection"
        );
        assert_eq!(
            highlighted_tools(&shell, window),
            vec!["Select".to_string()],
            "so the {label} highlight should have gone with it"
        );
    }
}

#[gpui::test]
fn the_text_handover_leaves_the_toolbar_on_selection(cx: &mut TestAppContext) {
    // The Text case is separate because it changes tool by a different route:
    // the session opens first and the tool changes after, so the toolbar could
    // easily be left showing Text.
    let (shell, window) = open_shell(cx);
    choose_tool(window, "Text");

    let spot = empty_canvas_point(&shell, window);
    window.simulate_click(spot, Modifiers::none());
    window.run_until_parked();

    assert!(
        read(&shell, window, |canvas| canvas.is_text_editing()),
        "precondition: a session is open"
    );
    assert_eq!(
        highlighted_tools(&shell, window),
        vec!["Select".to_string()],
        "Text should not stay lit once its creation is over"
    );
}

#[gpui::test]
fn escape_walks_back_through_the_rungs_to_the_tool(cx: &mut TestAppContext) {
    // Escape answers the topmost thing with something to give up, in order. With
    // an object selected the selection rung answers first and the tool stays put;
    // a second Escape then reaches the tool. Asserting only the end state would
    // hide that ordering, which is the part worth protecting.
    //
    // Order matters here. The object is clicked first, while the Selection tool
    // is still in effect, because clicking with a creation tool in effect would
    // create rather than select. Choosing the tool afterwards is safe because a
    // toolbar button is not a focus target, so the canvas keeps the focus its
    // click gave it and the key still has somewhere to arrive.
    let (shell, window) = open_shell(cx);
    click_object(&shell, window, 0, Modifiers::none());
    choose_tool(window, "Rectangle");

    assert!(
        holds_focus(&shell, window),
        "picking a tool from the toolbar must not take focus away from the canvas"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.tool()),
        crate::canvas::Tool::Rectangle,
        "choosing the tool should change it"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.selection().ids().len()),
        1,
        "and leave the selection from the click alone"
    );

    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert_eq!(
        read(&shell, window, |canvas| canvas.selection().ids().len()),
        0,
        "the first Escape should give up the selection"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.tool()),
        crate::canvas::Tool::Rectangle,
        "and stop there, because the tool rung is below the selection rung"
    );

    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert_eq!(
        read(&shell, window, |canvas| canvas.tool()),
        crate::canvas::Tool::Select,
        "the second Escape should give up the tool"
    );
    assert_eq!(
        highlighted_tools(&shell, window),
        vec!["Select".to_string()],
        "and the toolbar highlight should have followed"
    );
}

// ── Persistence: the whole chain, from a keypress to the file on disk ──────────
//
// Everything above proves the interaction reaches the document. This proves the
// document reaches the source, because that is the part that is easy to believe
// and easy to get wrong: an editor can delete perfectly from the user's point of
// view and still write nothing, and then the object comes back on reopen.
//
// The chain under test, all of it through real events and real files:
//
//   open project -> click an object -> Delete -> Save -> reopen -> object gone

/// A scratch copy of the landing fixture, named like a project so `open` accepts
/// it, and removed on the way out so a failing test leaves nothing behind for
/// the next one to trip over.
fn scratch_project(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "spool-interaction-{name}-{}-{:?}.spool",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/landing.spool"),
        &root,
    )
    .expect("the fixture project should copy");
    root
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The identities of the objects the canvas is holding.
fn identities(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> Vec<String> {
    read(shell, window, |canvas| {
        canvas
            .document_objects()
            .iter()
            .map(|object| object.spool_id.as_str().to_owned())
            .collect()
    })
}

#[gpui::test]
fn a_keyboard_deletion_survives_a_save_and_a_reopen(cx: &mut TestAppContext) {
    // The chain, all of it through real events and real files:
    //
    //   open project -> choose Rectangle -> click canvas to create -> Save ->
    //   click the new object -> Delete -> Save -> reopen
    //
    // Creation is in the chain on purpose. Deleting an object this session
    // created is the one deletion whose expected result is unambiguous, and it
    // exercises the remove path end to end: an object that exists in the source,
    // disappears from the source, and does not come back.
    let root = scratch_project("delete-round-trip");

    let (shell, window) = cx.add_window_view({
        let root = root.clone();
        move |_, cx| AppShell::new_with_project(Some(root), cx)
    });
    redraw(&shell, window);
    let original = identities(&shell, window);

    // Create with a click, which also selects the new object. No second click is
    // needed to select it, and deliberately none is made: objects overlap, so a
    // click aimed at a particular object can land on the frame around it, and
    // "the thing I just made is already selected" is both simpler and closer to
    // what a person would actually do.
    choose_tool(window, "Rectangle");
    let spot = empty_canvas_point(&shell, window);
    window.simulate_click(spot, Modifiers::none());
    window.run_until_parked();

    let created = identities(&shell, window);
    let created_id = created.last().expect("a created object").clone();
    assert_eq!(
        created.len(),
        original.len() + 1,
        "the click should have created an object"
    );
    assert!(
        !original.contains(&created_id),
        "and it should be a new one, not one of the fixture's"
    );
    assert_eq!(
        read(&shell, window, |canvas| canvas.selection().ids().len()),
        1,
        "the created object should be selected and ready for Delete"
    );

    // Put it in the source before deleting it, so the deletion has something real
    // to remove rather than undoing an unsaved creation.
    window.simulate_keystrokes("cmd-s");
    window.run_until_parked();

    window.simulate_keystrokes("delete");
    window.run_until_parked();
    assert!(
        !identities(&shell, window).contains(&created_id),
        "the object should be gone from the open document"
    );
    window.simulate_keystrokes("cmd-s");
    window.run_until_parked();

    let (reopened, second) = cx.add_window_view({
        let root = root.clone();
        move |_, cx| AppShell::new_with_project(Some(root), cx)
    });
    redraw(&reopened, second);
    let after_reopen = identities(&reopened, second);

    assert!(
        !after_reopen.contains(&created_id),
        "the deleted object must not come back from the source; reopened {after_reopen:?}"
    );
    assert_eq!(
        after_reopen.len(),
        original.len(),
        "and the objects that were never touched should all still be there"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A window-coordinate point on the object with this authored identity.
///
/// Objects overlap — a frame covers the objects inside it — so "click index 0" is
/// not a reliable way to say which object a click means. Naming the identity is.
fn object_centre(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
    spool_id: &str,
) -> gpui::Point<gpui::Pixels> {
    let origin = window
        .debug_bounds("canvas-viewport")
        .expect("the canvas viewport should have been laid out")
        .origin;
    let local = read(shell, window, |canvas| {
        canvas
            .document_objects()
            .iter()
            .position(|object| object.spool_id.as_str() == spool_id)
            .and_then(|index| canvas.object_screen_center(index))
    })
    .unwrap_or_else(|| panic!("{spool_id} should be on the canvas"));
    origin + gpui::Point::new(px(local.x), px(local.y))
}

#[gpui::test]
fn deleting_a_container_takes_its_contents_with_it_and_still_saves(cx: &mut TestAppContext) {
    // Deleting a container deletes what is inside it.
    //
    // The rule is settled by the authored source rather than by taste: the
    // fixture nests two elements inside the container's `<main>`, so removing
    // that element removes them whether or not anyone says so. Leaving their
    // records behind would mean a `lamine.yaml` entry for an element no longer in
    // the source — a dangling binding, and the same class of corruption as the
    // dangling `parent` that used to make this save fail outright.
    //
    // Before the cascade this test was ignored and asserted the failure: nothing
    // was written, and because a failed save left the structure corrupt, every
    // later save failed the same way too.
    let root = scratch_project("container-delete");

    let (shell, window) = cx.add_window_view({
        let root = root.clone();
        move |_, cx| AppShell::new_with_project(Some(root), cx)
    });
    redraw(&shell, window);

    let all = identities(&shell, window);
    let container = all
        .first()
        .cloned()
        .expect("the fixture should have objects");
    // The landing fixture nests two objects inside the root frame, which is what
    // makes this the interesting case: they are children in lamine.yaml *and*
    // elements nested inside the container's element in the HTML.
    let (child_one, child_two) = (
        all.get(1).cloned().expect("a nested child"),
        all.get(2).cloned().expect("another nested child"),
    );
    let target = object_centre(&shell, window, &container);
    window.simulate_click(target, Modifiers::none());
    window.run_until_parked();
    window.simulate_keystrokes("delete");
    window.run_until_parked();
    assert!(
        !identities(&shell, window).contains(&container),
        "precondition: the container is gone from the open document"
    );

    // The save is the point of the test. Before the cascade, this failed
    // validation with a dangling `parent`, wrote nothing, and every later save
    // failed the same way, so the user's deletion was silently lost.
    let outcome = window.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            shell.canvas().update(cx, |canvas, _| canvas.save_project())
        })
    });
    assert!(
        outcome.is_ok(),
        "deleting a container must leave a project that can still be saved: {:?}",
        outcome.err()
    );

    // And the cascade has to be real, not just saveable: the objects inside the
    // container went with it rather than being stranded.
    let after_delete = identities(&shell, window);
    assert!(
        !after_delete.contains(&container),
        "the container itself is gone"
    );
    for child in [&child_one, &child_two] {
        assert!(
            !after_delete.contains(child),
            "{child} was inside the deleted container and goes with it"
        );
    }

    // Reopening proves it reached the source rather than only the document.
    window.simulate_keystrokes("cmd-s");
    window.run_until_parked();
    let (reopened, second) = cx.add_window_view({
        let root = root.clone();
        move |_, cx| AppShell::new_with_project(Some(root), cx)
    });
    redraw(&reopened, second);
    // The assertion is that the deleted subtree does not come back, rather than
    // that the reopened project is empty. A project with no nodes cannot be
    // opened — there is nothing with a canvas representation — so the shell falls
    // back to the starter scene. That fallback is its own behaviour; what this
    // test is about is that none of the three deleted identities is in the source
    // any more.
    let after_reopen = identities(&reopened, second);
    for gone in [&container, &child_one, &child_two] {
        assert!(
            !after_reopen.contains(gone),
            "{gone} was deleted and must not reappear from the source; reopened \
             {after_reopen:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[gpui::test]
fn text_typed_after_a_click_survives_a_save_and_a_reopen(cx: &mut TestAppContext) {
    // The same chain for the Text handover, because a text object has content the
    // shape tools do not, and content is the part a source-backed save is most
    // likely to lose.
    let root = scratch_project("text-round-trip");

    let (shell, window) = cx.add_window_view({
        let root = root.clone();
        move |_, cx| AppShell::new_with_project(Some(root), cx)
    });
    redraw(&shell, window);

    choose_tool(window, "Text");
    let spot = empty_canvas_point(&shell, window);
    window.simulate_click(spot, Modifiers::none());
    window.run_until_parked();
    flush_frame(window, &spot);
    window.simulate_input("Typed");
    window.run_until_parked();

    // End the session so the typed text is committed before saving.
    click_object(&shell, window, 0, Modifiers::none());
    window.run_until_parked();
    window.simulate_keystrokes("cmd-s");
    window.run_until_parked();

    let expected: Vec<String> = read(&shell, window, |canvas| {
        canvas
            .document_objects()
            .iter()
            .map(|object| object.text_content.clone().unwrap_or_default())
            .collect()
    });

    let (reopened, second) = cx.add_window_view({
        let root = root.clone();
        move |_, cx| AppShell::new_with_project(Some(root), cx)
    });
    redraw(&reopened, second);

    let after_reopen: Vec<String> = read(&reopened, second, |canvas| {
        canvas
            .document_objects()
            .iter()
            .map(|object| object.text_content.clone().unwrap_or_default())
            .collect()
    });
    assert!(
        after_reopen.iter().any(|text| text.contains("Typed")),
        "typed text should survive the round trip; reopened had {after_reopen:?}"
    );
    assert_eq!(
        after_reopen.len(),
        expected.len(),
        "and the object count should match what was saved"
    );

    let _ = std::fs::remove_dir_all(&root);
}
