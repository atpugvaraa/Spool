mod canvas_render;
mod cascade;
mod commands;
mod diagnostics;
pub mod engine;
mod engine_project;
mod inspector;
#[cfg(test)]
mod interaction_window_tests;
mod lifecycle;
pub mod project_bundle;
mod project_open;
mod project_save;
mod shell;
pub mod source_binding;
pub mod source_document;
mod spool_project;
mod theme;
mod tools;

use gpui::{px, size, App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions};
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
        // INDEX THE SYSTEM FONTS, so text can be shaped at all.
        //
        // The engine's font database is empty until it is scanned, and an empty
        // database means no face to shape a text layer with -- the layer draws
        // nothing, and the typography panel's family list is empty. The scan is
        // ON DEMAND, so it happens the first time a font is actually needed
        // rather than at startup.
        figcraft_text::scan_system_fonts_on_demand();
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
