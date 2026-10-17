mod canvas;
mod canvas_render;
mod commands;
mod diagnostics;
mod document_runtime_bridge;
mod hierarchy;
mod inspector;
#[cfg(test)]
mod interaction_window_tests;
mod layers;
mod lifecycle;
mod operations;
pub mod project_bundle;
mod project_open;
mod project_save;
mod shell;
mod snap;
pub mod source_binding;
pub mod source_document;
mod spool_project;
mod style;
mod theme;
mod visual;

use gpui::{
    px, size, App, AppContext, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions,
};
use gpui_platform::application;

fn main() {
    // Resolved before the application starts, because it is the one piece of
    // launch state that decides what the window shows. Two projects on the
    // command line is a mistake worth reporting rather than resolving by taking
    // the first one.
    let requested = match spool_project::requested_from_args(std::env::args()) {
        Ok(requested) => requested,
        Err(error) => {
            eprintln!("spool_project_request failed: {error}");
            None
        }
    };

    application().run(|cx: &mut App| {
        lifecycle::install(cx);
        cx.bind_keys([
            KeyBinding::new("backspace", canvas::Backspace, None),
            KeyBinding::new("delete", canvas::Delete, None),
            KeyBinding::new("left", canvas::Left, None),
            KeyBinding::new("right", canvas::Right, None),
            KeyBinding::new("shift-left", canvas::SelectLeft, None),
            KeyBinding::new("shift-right", canvas::SelectRight, None),
            KeyBinding::new("cmd-a", canvas::SelectAll, None),
            KeyBinding::new("ctrl-a", canvas::SelectAll, None),
            KeyBinding::new("home", canvas::Home, None),
            KeyBinding::new("end", canvas::End, None),
            KeyBinding::new("cmd-v", canvas::Paste, None),
            KeyBinding::new("ctrl-v", canvas::Paste, None),
            KeyBinding::new("cmd-c", canvas::Copy, None),
            KeyBinding::new("ctrl-c", canvas::Copy, None),
            KeyBinding::new("cmd-x", canvas::Cut, None),
            KeyBinding::new("ctrl-x", canvas::Cut, None),
        ]);
        let bounds = Bounds::centered(None, size(px(1480.0), px(960.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Spool".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |_, cx| cx.new(|cx| shell::AppShell::new_with_project(requested.clone(), cx)),
        )
        .expect("failed to open Spool window");
        cx.activate(true);
    });
}
