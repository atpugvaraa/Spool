//! The layers panel and the keyboard, in a real window.
//!
//! A panel that owns the keyboard is a keyboard thing, and a keyboard thing is
//! only visible in a window: whether a keystroke reaches the tree depends on
//! which node holds the focus, and on whether a key binding claimed the key
//! before any `on_key_down` ran at all. A unit test holds the `Window` that
//! would do that work and never lets it, so it cannot see the defect.
//!
//! So these drive a real window through GPUI's own test facility: the same
//! shell the app opens, the same dispatch path the packaged app uses, and
//! keystrokes delivered as events rather than as calls.

use gpui::{point, px, size, Entity, IntoElement, Modifiers, TestAppContext, VisualTestContext};

use crate::shell::AppShell;

/// The size the real app opens at.
fn window_size() -> gpui::Size<gpui::Pixels> {
    size(px(1480.0), px(960.0))
}

/// Render the shell into the window.
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

/// Put two named frames in the document, so the tree has something to walk.
fn two_frames(shell: &Entity<AppShell>, window: &mut VisualTestContext) {
    window.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            shell.engine_canvas().update(cx, |canvas, cx| {
                for name in ["Row", "Card"] {
                    canvas.run_engine_command(
                        "node.create",
                        serde_json::json!({
                            "type": "frame",
                            "x": 40.0,
                            "y": 40.0,
                            "width": 320.0,
                            "height": 200.0,
                            "name": name
                        }),
                        cx,
                    );
                }
                canvas.zoom_to_fit(cx);
            });
        });
    });
    window.run_until_parked();
    redraw(shell, window);
}

/// The rows the panel is listing, in the order it lists them.
fn listed_rows(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
) -> Vec<figcraft_doc::NodeId> {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            shell.engine_layers().read(cx).test_listing().0
        })
    })
}

/// The engine's selection.
fn selection(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
) -> Vec<figcraft_doc::NodeId> {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            shell
                .engine_canvas()
                .read(cx)
                .with_session(|session| session.selection())
        })
    })
}

/// The engine's tool in effect.
fn tool(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> &'static str {
    window.update(|_, cx| shell.read_with(cx, |shell, _| shell.test_engine_tool(cx)))
}

/// Whether the layers panel holds the keyboard.
fn layers_hold_focus(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> bool {
    window.update(|window, cx| {
        shell.read_with(cx, |shell, _| {
            let layers = shell.engine_layers().clone();
            layers.read(cx).focus_handle().is_focused(window)
        })
    })
}

/// The text in the panel's search field.
fn search_text(shell: &Entity<AppShell>, window: &mut VisualTestContext) -> String {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| shell.engine_layers().read(cx).test_filter())
    })
}

/// The layer the panel is renaming, and the text in its field.
fn renaming(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
) -> (Option<figcraft_doc::NodeId>, Option<String>) {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            let layers = shell.engine_layers().read(cx);
            (layers.test_renaming(), layers.test_rename_text())
        })
    })
}

/// The name the document gives a layer.
fn layer_name(
    shell: &Entity<AppShell>,
    window: &mut VisualTestContext,
    id: figcraft_doc::NodeId,
) -> Option<String> {
    window.update(|_, cx| {
        shell.read_with(cx, |shell, _| {
            shell.engine_canvas().read(cx).with_session(|session| {
                session
                    .doc()
                    .ok()
                    .and_then(|doc| doc.node(id))
                    .map(|n| n.name.clone())
            })
        })
    })
}

/// Click a layer row, by the layer's name.
fn click_row(shell: &Entity<AppShell>, window: &mut VisualTestContext, id: figcraft_doc::NodeId) {
    let selector: &'static str =
        Box::leak(format!("layers-row-{}", id.0).into_boxed_str()) as &'static str;
    let row = window.debug_bounds(selector).expect("the row is drawn");
    window.simulate_click(row.center(), Modifiers::none());
    window.run_until_parked();
    redraw(shell, window);
}

/// The layers panel takes the keyboard when it is clicked.
///
/// The panel tracks a focus handle and nothing ever focused it, so the tree's
/// arrow keys went to the canvas and its Cmd-R went to the Inspector's rename.
#[gpui::test]
fn the_layers_panel_takes_the_keyboard_when_it_is_clicked(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    two_frames(&shell, window);
    let rows = listed_rows(&shell, window);
    assert!(!rows.is_empty(), "the panel is listing something");

    assert!(
        !layers_hold_focus(&shell, window),
        "the panel starts without the keyboard"
    );

    click_row(&shell, window, rows[0]);

    assert!(
        layers_hold_focus(&shell, window),
        "clicking a row must leave the tree holding the keyboard"
    );
}

/// An arrow key moves the selection in the tree.
#[gpui::test]
fn an_arrow_key_moves_the_selection_in_the_tree(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    two_frames(&shell, window);
    let rows = listed_rows(&shell, window);
    assert!(rows.len() >= 2, "the tree has two rows: {rows:?}");
    click_row(&shell, window, rows[0]);
    assert_eq!(selection(&shell, window), vec![rows[0]]);

    window.simulate_keystrokes("down");
    window.run_until_parked();

    assert_eq!(
        selection(&shell, window),
        vec![rows[1]],
        "the arrow key moved the selection down the tree"
    );
}

/// The search field takes the letter the user types.
///
/// The search field is a text field like any other, and the letters the
/// engine's keymap binds as tool shortcuts -- `v`, `a`, `c`, `f`, `o`, `p`,
/// `r`, `t` -- are letters in a layer name. GPUI dispatches a key BINDING
/// before any `on_key_down` handler runs, and an action that is answered stops
/// the key there, so a shortcut that ran while the field was open would switch
/// the tool and the character would never reach the field.
#[gpui::test]
fn a_letter_typed_in_the_layers_search_reaches_the_field(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    two_frames(&shell, window);
    let search = window
        .debug_bounds("layer-search")
        .expect("the layers header offers a search");
    window.simulate_click(search.center(), Modifiers::none());
    window.run_until_parked();
    redraw(&shell, window);

    window.simulate_keystrokes("f");

    assert_eq!(
        search_text(&shell, window),
        "f",
        "the field holds the letter the user typed"
    );
}

/// ...and the letter is a letter, not a tool shortcut.
#[gpui::test]
fn a_letter_typed_in_the_layers_search_does_not_switch_the_tool(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    two_frames(&shell, window);
    let search = window
        .debug_bounds("layer-search")
        .expect("the layers header offers a search");
    window.simulate_click(search.center(), Modifiers::none());
    window.run_until_parked();
    redraw(&shell, window);

    window.simulate_keystrokes("f");

    assert_eq!(
        tool(&shell, window),
        "move",
        "typing into the tree's field does not reach the tool shortcuts"
    );
}

/// The rename chord renames the layer in the tree.
///
/// The chord belongs to the surface the user is on: in the tree it renames the
/// row, and the name is typed into the row. The engine's keymap binds the same
/// chord to `object.rename`, which the shell answers with the Inspector's own
/// rename -- so the tree has to claim the chord before the binding runs.
#[gpui::test]
fn the_rename_chord_renames_the_layer_in_the_tree(cx: &mut TestAppContext) {
    let (shell, window) = open_shell(cx);
    two_frames(&shell, window);
    let rows = listed_rows(&shell, window);
    click_row(&shell, window, rows[0]);

    window.simulate_keystrokes("cmd-r");
    window.run_until_parked();

    let (renaming, text) = renaming(&shell, window);
    assert_eq!(
        renaming,
        Some(rows[0]),
        "the tree opened its own rename on the selected layer"
    );
    assert_eq!(
        text.as_deref(),
        layer_name(&shell, window, rows[0]).as_deref(),
        "and the field holds the name the layer has"
    );

    window.simulate_keystrokes("h e r o enter");
    window.run_until_parked();

    assert_eq!(
        layer_name(&shell, window, rows[0]).as_deref(),
        Some("hero"),
        "the name the user typed is the name in the document"
    );
}
