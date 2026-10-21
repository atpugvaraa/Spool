//! The Inspector's text fields, in a real window.
//!
//! A field is a keyboard thing, and a keyboard thing is only visible in a
//! window: whether a keystroke reaches the field depends on which node holds
//! the focus, and on whether a key binding claimed the key before any
//! `on_key_down` ran at all. A unit test holds the `Window` that would do
//! that work and never lets it, so it cannot see the defect.
//!
//! So these drive a real window through GPUI's own test facility: the same
//! shell the app opens, the same dispatch path the packaged app uses, and
//! keystrokes delivered as events rather than as calls.

use gpui::{point, px, size, Entity, IntoElement, Modifiers, TestAppContext, VisualTestContext};

use crate::shell::AppShell;

/// The size the real app opens at.
///
/// The same size as the app, so a point that lands on a control in a test is
/// a point that lands on that control in the app.
fn window_size() -> gpui::Size<gpui::Pixels> {
    size(px(1480.0), px(960.0))
}

/// Render the shell into the window.
///
/// `draw` is what produces the rendered frame that hit testing reads, so it is
/// part of what is under test rather than setup.
fn redraw(shell: &Entity<AppShell>, window: &mut VisualTestContext) {
    window.draw(point(px(0.0), px(0.0)), window_size(), |_, _| {
        shell.clone().into_any_element()
    });
    window.run_until_parked();
}

/// A shell in a real window, with the engine's keymap installed.
///
/// The application installs the engine's keymap in `lifecycle::install`, which
/// a test does not run. Without it a shortcut would go nowhere, and the test
/// would be measuring the absence of a binding rather than the behaviour it
/// claims to test.
fn open_shell(cx: &mut TestAppContext) -> (Entity<AppShell>, &mut VisualTestContext) {
    cx.update(|app| {
        app.bind_keys(crate::engine::keymap::bindings(
            figcraft_engine::keymap::Platform::current(),
        ));
    });
    let (shell, window) = cx.add_window_view(|_, cx| AppShell::new_with_project(None, cx));
    redraw(&shell, window);
    (shell, window)
}

/// One frame in the engine's document, selected by a click on it.
///
/// The engine's own command, so the document is one the editor would really
/// hold, and `node.create` leaves the new layer selected — which is what the
/// Inspector reads. The click matters as much as the command: the canvas takes
/// the keyboard when it is clicked, and a key event only reaches the nodes on
/// the path from the focused node up to the root. A test that never clicked
/// the canvas would be testing a window in which no key reaches anything.
fn select_a_frame(shell: &Entity<AppShell>, window: &mut VisualTestContext) {
    window.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            shell.engine_canvas().update(cx, |canvas, cx| {
                canvas.run_engine_command(
                    "node.create",
                    serde_json::json!({
                        "type": "frame",
                        "x": 40.0,
                        "y": 40.0,
                        "width": 320.0,
                        "height": 200.0,
                        "name": "Row"
                    }),
                    cx,
                );
                canvas.zoom_to_fit(cx);
            });
        });
    });
    window.run_until_parked();
    redraw(shell, window);
    let centre = frame_centre(shell, window);
    window.simulate_click(centre, Modifiers::none());
    window.run_until_parked();
    redraw(shell, window);
}

/// The middle of the frame, on the screen.
///
/// The engine's own geometry: the node's corners and the camera that puts
/// them on the screen. Nothing here re-derives the camera.
fn frame_centre(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
) -> gpui::Point<gpui::Pixels> {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            let canvas = shell.engine_canvas().read(cx);
            let session = canvas.session_handle();
            let session = session.borrow();
            let Ok(doc) = session.doc() else {
                panic!("the engine should have a document");
            };
            let id = doc
                .nodes()
                .find(|node| node.name == "Row")
                .map(|node| node.id)
                .unwrap_or_else(|| panic!("the frame should be in the document"));
            let quad = crate::engine::canvas::node_quad(doc, id)
                .unwrap_or_else(|| panic!("the frame should have a quad"));
            let centre = quad
                .iter()
                .fold((0.0, 0.0), |(x, y), point| (x + point.x, y + point.y));
            let centre = (centre.0 / 4.0, centre.1 / 4.0);
            let at = crate::engine::canvas::page_to_screen(
                &canvas.view(),
                canvas.viewport(),
                centre.0,
                centre.1,
            );
            gpui::Point::new(px(at.x), px(at.y))
        })
    })
}

/// Open the fill colour picker and click into its hex field.
///
/// Two clicks, because two things happen: the trigger opens the picker, and
/// the field starts a draft. The draft is the field's buffer, and the buffer
/// is what the user types into.
fn open_the_hex_field(shell: &Entity<AppShell>, window: &mut VisualTestContext) {
    let trigger = window
        .debug_bounds("fill-color-trigger")
        .expect("the fill row offers a colour");
    window.simulate_click(trigger.center(), Modifiers::none());
    window.run_until_parked();
    redraw(shell, window);
    let field = window
        .debug_bounds("color-picker-field-hex")
        .expect("the picker shows its hex field");
    window.simulate_click(field.center(), Modifiers::none());
    window.run_until_parked();
    redraw(shell, window);
}

/// The characters in the colour picker's open field.
fn hex_draft(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> Option<String> {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| shell.inspector().read(cx).test_color_draft())
    })
}

/// The engine's tool in effect.
fn tool(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> &'static str {
    window.update(|_, cx| shell.read_with(cx, |shell, _| shell.test_engine_tool(cx)))
}

/// Whether the canvas has been told that a field owns the keyboard.
fn canvas_claims_the_keyboard(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> bool {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            shell.engine_canvas().read(cx).keyboard_claimed()
        })
    })
}

/// How many layers the engine's document holds.
fn node_count(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> usize {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            shell.engine_canvas().read(cx).with_session(|session| {
                let Ok(doc) = session.doc() else {
                    return 0;
                };
                doc.nodes().count()
            })
        })
    })
}

/// The engine's identity for the layer the tests use.
fn frame_id(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
) -> Option<figcraft_doc::NodeId> {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            let canvas = shell.engine_canvas().read(cx);
            canvas.with_session(|session| {
                let Ok(doc) = session.doc() else {
                    return None;
                };
                doc.nodes()
                    .find(|node| node.name == "Row")
                    .map(|node| node.id)
            })
        })
    })
}

/// The name the engine's document gives the layer.
fn layer_name(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
    id: figcraft_doc::NodeId,
) -> Option<String> {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            let canvas = shell.engine_canvas().read(cx);
            canvas.with_session(|session| {
                let Ok(doc) = session.doc() else {
                    return None;
                };
                doc.node(id).map(|node| node.name.clone())
            })
        })
    })
}

/// The X of the layer, in the document.
fn layer_x(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
    id: figcraft_doc::NodeId,
) -> Option<f32> {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            let canvas = shell.engine_canvas().read(cx);
            canvas.with_session(|session| {
                let Ok(doc) = session.doc() else {
                    return None;
                };
                let geometry = crate::engine::inspector_model::geometry(doc, id)?;
                Some(geometry.position.x)
            })
        })
    })
}

/// A colour field takes every character the user types.
///
/// The hex field is the one everybody meets first: six characters of a colour
/// code, typed. Every one of them has to arrive, in order, or the field
/// commits a colour the user did not choose.
#[gpui::test]
fn a_colour_field_takes_every_character(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    select_a_frame(&shell, window);
    open_the_hex_field(&shell, window);

    window.simulate_keystrokes("d 9 7 7 0 6");

    assert_eq!(
        hex_draft(&shell, window).as_deref(),
        Some("d97706"),
        "the field holds every character, in order"
    );
}

/// A colour field takes the letters that are also tool shortcuts.
///
/// `a` is the frame tool, `f` is the frame tool, `c` is the comment tool and
/// `t` is the text tool: every hex code is likely to contain one of them, and
/// a code that loses its last letter is a colour the user did not choose.
#[gpui::test]
fn a_colour_field_takes_the_letters_that_are_also_shortcuts(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    select_a_frame(&shell, window);
    open_the_hex_field(&shell, window);

    window.simulate_keystrokes("e 8 8 3 3 a");

    assert_eq!(
        hex_draft(&shell, window).as_deref(),
        Some("e8833a"),
        "the field holds the whole code, the last letter included"
    );
}

/// A letter typed into a field is a letter, not a shortcut.
///
/// `f` is the frame tool and `a` is the frame tool too, so a hex code that
/// contains either of them is the code that breaks first.
#[gpui::test]
fn a_letter_typed_into_a_field_does_not_switch_the_tool(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    select_a_frame(&shell, window);
    open_the_hex_field(&shell, window);

    window.simulate_keystrokes("f a");

    assert_eq!(
        tool(&shell, window),
        "move",
        "typing into a field does not reach the tool shortcuts"
    );
    assert_eq!(
        hex_draft(&shell, window).as_deref(),
        Some("fa"),
        "and the letters are the letters that were typed"
    );
}

/// Enter commits the field.
///
/// Enter is a canvas chord — it descends the selection — so a field that is
/// open has to take it, or the chord lands on the canvas and puts a shape
/// there instead of a colour in the document.
#[gpui::test]
fn enter_commits_the_field(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    select_a_frame(&shell, window);
    open_the_hex_field(&shell, window);
    let before = node_count(&shell, window);

    window.simulate_keystrokes("d 9 7 7 0 6 enter");

    assert_eq!(hex_draft(&shell, window), None, "Enter closed the field");
    assert_eq!(
        node_count(&shell, window),
        before,
        "and it did not put a shape on the canvas"
    );
}

/// Escape abandons the field, and the canvas is none the wiser.
///
/// Escape is the editor's ladder, and the field is the top rung of it: the
/// typed colour is given up and the document is left as it was.
#[gpui::test]
fn escape_abandons_the_field(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    select_a_frame(&shell, window);
    open_the_hex_field(&shell, window);
    let before = node_count(&shell, window);

    window.simulate_keystrokes("d 9 7 7 0 6 escape");

    assert_eq!(hex_draft(&shell, window), None, "Escape closed the field");
    assert_eq!(
        node_count(&shell, window),
        before,
        "and it did not put a shape on the canvas either"
    );
}

/// The canvas steps aside while a field is open.
///
/// The canvas is the engine's own key handling, and the engine's keys are
/// gestures. A field that is open owns the keyboard, and the canvas has to
/// know that: a `z` typed into a colour code is not the zoom tool, and a
/// click on the canvas does not take the field's keyboard away.
#[gpui::test]
fn the_canvas_steps_aside_while_a_field_is_open(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    select_a_frame(&shell, window);
    assert!(
        !canvas_claims_the_keyboard(&shell, window),
        "nothing has claimed the keyboard yet"
    );

    open_the_hex_field(&shell, window);

    assert!(
        canvas_claims_the_keyboard(&shell, window),
        "the field has claimed the keyboard, and the canvas knows it"
    );
}

/// The canvas does not take the keyboard from whoever has it.
///
/// The field is open, so the keyboard is the field's. A click on the canvas
/// must not take it back: the canvas is the surface the user is clicking on,
/// but the keyboard is not the canvas's while a field has it.
#[gpui::test]
fn the_canvas_does_not_take_the_keyboard_from_a_field(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    select_a_frame(&shell, window);
    // The layers panel takes the keyboard, the way a field does.
    window.update(|window, cx| {
        let layers = shell.read_with(cx, |shell, _| shell.engine_layers().clone());
        let focus = layers.read(cx).focus_handle().clone();
        focus.focus(window, cx);
    });
    window.run_until_parked();

    // A field opens, and the keyboard is the field's.
    open_the_hex_field(&shell, window);
    let canvas = window
        .debug_bounds("engine-canvas")
        .expect("the engine canvas is on screen");
    window.simulate_click(canvas.center(), Modifiers::none());
    window.run_until_parked();

    window.update(|window, cx| {
        let layers = shell.read_with(cx, |shell, _| shell.engine_layers().clone());
        assert!(
            layers.read(cx).focus_handle().is_focused(window),
            "the keyboard is still where the field left it"
        );
    });
}

/// A shortcut still works when no field is open.
///
/// The fix is a gate, and a gate that never opens is a fix that broke the
/// editor: `v` is the select tool, and it has to stay the select tool.
#[gpui::test]
fn a_tool_shortcut_still_works_when_no_field_is_open(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    select_a_frame(&shell, window);

    window.simulate_keystrokes("f");
    assert_eq!(tool(&shell, window), "frame", "`f` is the frame tool");

    window.simulate_keystrokes("v");
    assert_eq!(tool(&shell, window), "move", "`v` is the select tool");
}

/// A double click, which is how a number field opens for typing.
///
/// A single click is a drag, so the field opens on the second click — and the
/// test facility's own `simulate_click` always reports one click, so the
/// events are dispatched here.
fn double_click(window: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>) {
    window.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers: Modifiers::none(),
        button: gpui::MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    window.simulate_event(gpui::MouseUpEvent {
        position: at,
        modifiers: Modifiers::none(),
        button: gpui::MouseButton::Left,
        click_count: 2,
    });
}

/// A number field takes the number the user types.
///
/// The position fields are the other half of the complaint: a corner radius
/// that can only be dragged is a corner radius nobody can set exactly.
#[gpui::test]
fn a_number_field_takes_the_number_the_user_types(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    select_a_frame(&shell, window);
    let id = frame_id(&shell, window).expect("the frame is in the document");
    // The lookup takes a `'static` name, and the id is only known now.
    let selector = format!("inspector-{}-x", id.0).leak();
    let field = window
        .debug_bounds(selector)
        .expect("the position section shows the X field");
    double_click(window, field.center());
    window.run_until_parked();
    redraw(&shell, window);

    window.simulate_keystrokes("1 2 0 enter");

    assert_eq!(
        layer_x(&shell, window, id),
        Some(120.0),
        "the typed number is the number in the document"
    );
}

/// A layer can be renamed, and the letters of the name are letters.
///
/// `F2` is the rename chord, and `r` and `o` are the rectangle and ellipse
/// tools: a name that cannot be typed is a name nobody can give a layer.
#[gpui::test]
fn a_layer_can_be_renamed(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    select_a_frame(&shell, window);
    let id = frame_id(&shell, window).expect("the frame is in the document");

    window.simulate_keystrokes("f2");
    window.simulate_keystrokes("h e r o enter");

    assert_eq!(
        layer_name(&shell, window, id).as_deref(),
        Some("hero"),
        "the name the user typed is the name in the document"
    );
    assert_eq!(
        tool(&shell, window),
        "move",
        "and typing the name did not reach the tool shortcuts"
    );
}
