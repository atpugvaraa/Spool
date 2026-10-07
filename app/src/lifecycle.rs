//! Application and window lifecycle.
//!
//! Spool installs no menu bar, so AppKit never sees the two key equivalents a
//! Mac user reaches for without thinking. GPUI supplies neither of them by
//! itself: `cmd-q` and `cmd-w` are ordinary keybindings, and until something
//! binds them they fall through to nothing. So `⌘Q` did not quit and `⌘W` did
//! not close the window.
//!
//! The native close button needed no work. GPUI's macOS backend implements
//! `windowShouldClose:`, which returns `YES` when no `on_window_should_close`
//! callback is registered, and the platform window's `on_close` calls
//! `Window::remove_window`. The red button was already wired; only the two key
//! equivalents were missing.

//! # MUTATION HARNESS
//!
//! `app/mutate_spool_project.sh` breaks one rule at a time in this file — the
//! application menu that keeps `⌘Q` alive with no windows, the deferred window
//! removal, the action each key dispatches — and requires the suite to notice.
//! The script refuses to run unless it sees this marker.

use gpui::{App, KeyBinding, Menu, MenuItem};

gpui::actions!(
    spool_app,
    [
        /// Quits the application.
        Quit,
        /// Closes the current window.
        ///
        /// This is not a quit, and must not become one. On macOS `QuitMode::Default`
        /// resolves to `QuitMode::Explicit`, so removing a window does not terminate
        /// the process; that is the platform's own choice and this action stays
        /// inside it.
        CloseWindow,
    ]
);

/// The lifecycle key equivalents, kept separate from the editor's bindings so
/// this table can be asserted on its own.
pub fn bindings() -> [KeyBinding; 2] {
    [
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
    ]
}

/// The application menu.
///
/// This exists for exactly one reason, and it is not decoration.
///
/// Key dispatch in GPUI is *window*-scoped: a keystroke is matched against the
/// key bindings inside the window that receives it. Close the last window and
/// there is no window left to receive one, so `cmd-q` is never even looked at
/// and the application cannot be quit. That is not a bug in the `Quit` handler —
/// it is simply never reached. On macOS the state is reachable and normal: a
/// document-based app whose last window is closed stays running and must still
/// answer `⌘Q`.
///
/// A menu fixes this the way AppKit intends. A key equivalent belongs to the
/// application menu rather than to a window, so AppKit matches it while routing
/// the event and hands it to `NSApplication`, which reaches GPUI's
/// `on_app_menu_action` → `App::dispatch_action`. That path does not consult the
/// window list, so it fires with or without windows. GPUI reads the key
/// equivalent for this item out of the keymap, which is why [`bindings`] still
/// has to register `cmd-q`.
///
/// Only `Quit` appears here. `⌘W` needs no menu entry: it is pressed while a
/// window exists, so the ordinary window-scoped binding already reaches it, and
/// adding a Window menu would be UI this milestone has no reason to add.
pub fn menus() -> Vec<Menu> {
    vec![Menu::new("Spool").items([MenuItem::action("Quit Spool", Quit)])]
}

/// Bind the lifecycle keys, install the application menu, and handle the actions
/// they dispatch.
///
/// Both handlers register globally, and that is not a shortcut around a better
/// option. An action dispatched to an element only reaches listeners along the
/// dispatch path, and GPUI builds that path from the focused node — falling back
/// to the dispatch tree's synthetic root when nothing is focused, a path that
/// contains no editor element at all. App-level keys would then go dead whenever
/// focus happened to be nowhere, which is exactly the state the app is in between
/// launching and the first click. Zed binds `Quit` globally for the same reason.
///
/// `CloseWindow` defers the window update. The action is dispatched from inside
/// the window update it wants to end, and GPUI takes that window out of
/// `App::windows` for the duration of an update, so updating it from in here
/// finds no window and is silently dropped. Deferring runs once that update has
/// finished and put it back.
pub fn install(cx: &mut App) {
    cx.bind_keys(bindings());
    cx.set_menus(menus());
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &CloseWindow, cx| {
        let Some(window) = cx.active_window() else {
            return;
        };
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `unparse` spells the platform modifier per OS (`cmd-`, `win-`, `super-`).
    /// The binding is `cmd-*` everywhere, so compare on the neutral spelling.
    fn keystroke(binding: &KeyBinding) -> String {
        let key = binding.keystrokes()[0].unparse();
        ["win-", "super-"]
            .iter()
            .find_map(|prefix| key.strip_prefix(prefix))
            .map_or(key.clone(), |rest| format!("cmd-{rest}"))
    }

    #[test]
    fn cmd_q_dispatches_quit() {
        let binding = bindings()
            .into_iter()
            .find(|b| b.action().name() == "spool_app::Quit");
        let binding = binding.expect("cmd-q is bound to Quit");
        assert_eq!(keystroke(&binding), "cmd-q");
    }

    #[test]
    fn cmd_w_dispatches_close_window() {
        let binding = bindings()
            .into_iter()
            .find(|b| b.action().name() == "spool_app::CloseWindow");
        let binding = binding.expect("cmd-w is bound to CloseWindow");
        assert_eq!(keystroke(&binding), "cmd-w");
    }

    /// Neither key is conditional. A `when` context would let the editor's own
    /// listeners claim the keystroke first, which is how `⌘W` would quietly turn
    /// into a canvas action.
    #[test]
    fn lifecycle_keys_are_always_active() {
        for binding in bindings() {
            assert!(
                binding.predicate().is_none(),
                "{} must not be gated on a key context",
                binding.action().name()
            );
        }
    }

    /// The lifecycle table must not grow keys the editor already owns, or a
    /// collision would silently shadow `cmd-a`/`cmd-c`/`cmd-v`/`cmd-x`.
    #[test]
    fn lifecycle_keys_do_not_collide_with_editor_keys() {
        for binding in bindings() {
            assert!(
                !matches!(
                    keystroke(&binding).as_str(),
                    "cmd-a" | "cmd-c" | "cmd-v" | "cmd-x"
                ),
                "{} collides with an editor shortcut",
                keystroke(&binding)
            );
        }
    }

    /// `⌘Q` has to survive the last window closing, and the application menu is
    /// the only reason it does. If the Quit item is dropped, the app silently
    /// becomes unquittable once its only window is gone — with no failing test
    /// and no error anywhere, because key dispatch is window-scoped and simply
    /// never looks at the binding.
    #[test]
    fn the_application_menu_carries_quit() {
        let menus = menus();
        let app_menu = menus.first().expect("an application menu is installed");
        assert_eq!(app_menu.name, "Spool");
        let quit = app_menu
            .items
            .iter()
            .find(|item| matches!(item, MenuItem::Action { name, .. } if *name == "Quit Spool"));
        let quit = quit.expect("the application menu offers Quit");
        match quit {
            MenuItem::Action { action, .. } => {
                assert_eq!(action.name(), "spool_app::Quit")
            }
            _ => unreachable!("Quit is an action item"),
        }
    }

    /// `⌘W` must not gain a menu entry. A Window-menu Close item would be a
    /// second path to the same verb, and the distinction between closing a window
    /// and quitting the application is the thing this module exists to keep.
    #[test]
    fn only_quit_is_in_the_menu() {
        for menu in menus() {
            for item in &menu.items {
                match item {
                    MenuItem::Action { name, .. } => {
                        assert_eq!(*name, "Quit Spool", "no other menu action is expected")
                    }
                    MenuItem::Separator => {}
                    _ => panic!("unexpected menu item in the application menu"),
                }
            }
        }
    }
}
