//! The Spool menu bar, built from figcraft's menu catalog.
//!
//! [`figcraft_engine::catalog::MENU`] is already the menu tree as data: 216 rows of
//! (menu path, label, command id), where `/` separates submenu levels and a label of `-` is a
//! separator. There is no second menu definition to keep in step, and no per-item code: this module
//! groups the rows into a tree, turns that tree into GPUI [`gpui::Menu`]s, and says where each
//! item's command goes when it is chosen.
//!
//! # Where a chosen item goes
//!
//! Two kinds of item end a click, and pretending they are the same kind is how a design editor
//! ends up with a menu item that does nothing:
//!
//! - **Engine commands** go to [`figcraft_engine::Session::execute`]. Every one of them is
//!   registered, documented and undoable, and Spool runs them by id.
//! - **UI-implemented ids** — [`UI_COMMANDS`] — are the ones figcraft's own egui layer
//!   implements itself: a file dialog, the system clipboard, an export preview, the plugin
//!   importer. The engine has no command for them, and inventing one would be a fake. They come
//!   back as a [`UiRequest::Shell`] for the shell to resolve, or [`UiRequest::Unknown`] when an
//!   id is neither, which is upstream drift rather than something to paper over.
//!
//! [`coverage`] measures the split, and the tests assert on it, so a catalog row that stops
//! resolving is a failing test rather than a dead menu item.

use figcraft_engine::catalog::{MENU, SEP};
use figcraft_engine::{find_command, Session};
use serde_json::Value;

use super::keymap;

/// The command ids the UI layer implements itself.
///
/// figcraft's own `ui-egui` lists these in its `menus.rs`; the list is the same here, and each
/// one needs something only a shell can do — a native dialog, the clipboard, a preview window.
/// Not one of them is an engine command, so each one reaches the shell as a
/// [`UiRequest::Shell`].
pub const UI_COMMANDS: &[&str] = &[
    "edit.copyAsText",
    "edit.copyAsCss",
    "edit.copyAsSvg",
    "edit.copyAsPng",
    "edit.copyLink",
    "file.open",
    "file.import",
    "file.placeImage",
    "file.save",
    "file.saveAs",
    "file.export",
    "file.exportSvg",
    "plugin.import",
];

/// Whether an id is one the UI implements rather than one the engine runs.
pub fn is_ui_command(id: &str) -> bool {
    UI_COMMANDS.contains(&id)
}

/// Whether Spool can do something with this id at all.
pub fn available(id: &str) -> bool {
    find_command(id).is_some() || is_ui_command(id)
}

/// What the caller has to do with a chosen menu item.
#[derive(Clone, Debug, PartialEq)]
pub enum UiRequest {
    /// An engine command: run it on the session with these params.
    Execute { id: &'static str, params: Value },
    /// A command the shell implements (a dialog, the clipboard, a preview).
    Shell { id: &'static str, params: Value },
    /// Neither an engine command nor one of [`UI_COMMANDS`]: the catalog has drifted.
    Unknown { id: String, params: Value },
}

impl UiRequest {
    /// The command id, whichever kind this is.
    pub fn id(&self) -> &str {
        match self {
            UiRequest::Execute { id, .. } | UiRequest::Shell { id, .. } => id,
            UiRequest::Unknown { id, .. } => id,
        }
    }

    /// Whether the engine can run this one.
    pub fn is_engine(&self) -> bool {
        matches!(self, UiRequest::Execute { .. })
    }
}

/// Sort an id into the kind of thing it is.
///
/// [`UI_COMMANDS`] wins over the registry, which is the order figcraft's own UI uses and the one
/// that has to hold here: several of them *are* engine commands as well — `file.open` takes a
/// `path`, `file.saveAs` one of its own — because the CLI and the control channel need a
/// programmatic way in. The UI intercepts the id first and opens the dialog; the engine command
/// is what a caller that already knows the path gets.
///
/// Ids come from a static table, so a row that resolves to [`UiRequest::Unknown`] is a catalog
/// row no layer implements. That is a bug worth seeing, so it is a value rather than a panic:
/// the menu still shows, and the caller decides what to do about it.
pub fn request(id: &str, params: Value) -> UiRequest {
    let Some(static_id) = static_id(id) else {
        return UiRequest::Unknown {
            id: id.to_string(),
            params,
        };
    };
    if is_ui_command(id) {
        return UiRequest::Shell {
            id: static_id,
            params,
        };
    }
    if find_command(id).is_some() {
        return UiRequest::Execute {
            id: static_id,
            params,
        };
    }
    UiRequest::Unknown {
        id: id.to_string(),
        params,
    }
}

/// The catalog's own copy of an id.
///
/// Menu ids are `&'static str` in the catalog table, so a matching row hands its own reference
/// back rather than allocating one per click. `None` for an id the catalog does not have, which
/// is how a programmatic caller (`file.openRecent` from the shell, say) is handled.
pub fn static_id(id: &str) -> Option<&'static str> {
    MENU.iter().find(|m| m.id == id).map(|m| m.id)
}

/// What happened when a menu item was chosen.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The engine ran it and returned this.
    Ran(Value),
    /// The engine has the command but refused it: nothing selected, a bad param, no document.
    ///
    /// The error is kept as its message because that is what a menu has room for, and because
    /// [`figcraft_engine::EngineError`] carries an [`std::error::Error`] rather than a value.
    Refused { message: String },
    /// The shell has to run it (a dialog, the clipboard, a preview), or nothing does: the catalog
    /// has drifted past both the registry and [`UI_COMMANDS`].
    Deferred(UiRequest),
}

impl Outcome {
    /// Whether the engine ran the command.
    pub fn ran(&self) -> bool {
        matches!(self, Outcome::Ran(_))
    }

    /// The request left for the caller, when there is one.
    pub fn deferred(&self) -> Option<&UiRequest> {
        match self {
            Outcome::Deferred(request) => Some(request),
            _ => None,
        }
    }

    /// The engine's complaint, when it refused.
    pub fn message(&self) -> Option<&str> {
        match self {
            Outcome::Refused { message } => Some(message),
            _ => None,
        }
    }
}

/// Run a menu item on the engine.
///
/// The three outcomes are kept apart on purpose. A command that refuses (nothing selected, no
/// document open) is the engine answering, and is not the same thing as a command the engine does
/// not have — which is a hole in the catalog, and is [`Outcome::Deferred`] with
/// [`UiRequest::Unknown`] rather than a silent no-op.
pub fn invoke(session: &mut Session, id: &str, params: &Value) -> Outcome {
    match request(id, params.clone()) {
        UiRequest::Execute { id, params } => match session.execute(id, &params) {
            Ok(value) => Outcome::Ran(value),
            Err(error) => Outcome::Refused {
                message: error.to_string(),
            },
        },
        other => Outcome::Deferred(other),
    }
}

/// One row of the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    /// The catalog row, and with it the menu path (`Edit/Copy as`).
    pub menu: &'static str,
    pub label: &'static str,
    /// The command id; empty for a separator.
    pub id: &'static str,
}

impl Item {
    /// Whether Spool can run this one, or the shell can.
    pub fn is_available(&self) -> bool {
        available(self.id)
    }

    /// The key a menu shows beside the label, or `None` when it has none.
    pub fn shortcut(&self, platform: figcraft_engine::keymap::Platform) -> Option<String> {
        keymap::display_shortcut(self.id, platform)
    }
}

/// A node of the menu tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// A submenu (`Edit/Copy as`).
    Submenu { title: String, items: Vec<Node> },
    /// A separator between items.
    Separator,
    /// A command.
    Item(Item),
}

impl Node {
    /// The rows under this node, if it is a submenu.
    pub fn items(&self) -> &[Node] {
        match self {
            Node::Submenu { items, .. } => items,
            _ => &[],
        }
    }

    /// The title, if this node has one (a submenu, or the menu itself).
    pub fn title(&self) -> Option<&str> {
        match self {
            Node::Submenu { title, .. } => Some(title),
            _ => None,
        }
    }
}

/// The whole menu tree: one [`Node::Submenu`] per top-level menu, in catalog order.
pub fn tree() -> Vec<Node> {
    levels()
        .into_iter()
        .map(|(title, items)| Node::Submenu { title, items })
        .collect()
}

/// One top-level menu and its rows.
pub fn menu(name: &str) -> Vec<Node> {
    levels()
        .into_iter()
        .find(|(title, _)| title == name)
        .map(|(_, items)| items)
        .unwrap_or_default()
}

/// The menu bar: every top-level menu, with its rows.
///
/// Each submenu is built by prefix rather than by a fixed depth, so `Edit/Copy as`,
/// `View/Outlines` and `Object/Boolean groups` all work without this module knowing any of them.
fn levels() -> Vec<(String, Vec<Node>)> {
    let mut menus: Vec<(String, Vec<Node>)> = Vec::new();
    for m in MENU {
        let Some((head, _)) = m.menu.split_once('/') else {
            // A row of a top-level menu. The menu list is built from the rows themselves, so a
            // menu with no rows of its own still appears.
            if !menus.iter().any(|(name, _)| name == m.menu) {
                menus.push((m.menu.to_string(), level(m.menu)));
            }
            continue;
        };
        if !menus.iter().any(|(name, _)| name == head) {
            menus.push((head.to_string(), level(head)));
        }
    }
    menus
}

/// The rows of one menu path, in catalog order.
///
/// A row belongs to this level when its path *is* the path; a deeper row shows up as its first
/// extra segment, which is how a submenu appears here at the point the catalog first mentions it.
/// Separators collapse: two in a row are one, and a trailing one is dropped.
fn level(prefix: &str) -> Vec<Node> {
    let mut nodes: Vec<Node> = Vec::new();
    let mut last_separator = true;
    for m in MENU.iter().filter(|m| m.menu == prefix) {
        if m.label == SEP {
            if !last_separator {
                nodes.push(Node::Separator);
                last_separator = true;
            }
            continue;
        }
        last_separator = false;
        nodes.push(Node::Item(Item {
            menu: m.menu,
            label: m.label,
            id: m.id,
        }));
    }
    let mut seen: Vec<String> = Vec::new();
    for m in MENU {
        let Some(rest) = m
            .menu
            .strip_prefix(prefix)
            .and_then(|r| r.strip_prefix('/'))
        else {
            continue;
        };
        let sub = rest.split('/').next().unwrap_or(rest).to_string();
        if seen.contains(&sub) {
            continue;
        }
        seen.push(sub.clone());
        let path = format!("{prefix}/{sub}");
        nodes.push(Node::Submenu {
            title: sub,
            items: level(&path),
        });
    }
    // A separator that ends the menu once the submenus have been added is noise.
    while matches!(nodes.last(), Some(Node::Separator)) {
        nodes.pop();
    }
    nodes
}

/// The menu bar as GPUI menus.
///
/// Every row becomes a [`keymap::RunCommand`] action, so a click and the matching keystroke reach
/// the same dispatch — [`request`] decides which, and the shell resolves what is left.
///
/// Submenus and separators map onto GPUI's own; a `Text`-only context has no say here because a
/// menu is not scoped to one.
pub fn gpui_menus() -> Vec<gpui::Menu> {
    levels()
        .into_iter()
        .map(|(name, items)| gpui::Menu::new(name).items(gpui_items(&items)))
        .collect()
}

fn gpui_items(nodes: &[Node]) -> Vec<gpui::MenuItem> {
    nodes
        .iter()
        .map(|node| match node {
            Node::Separator => gpui::MenuItem::separator(),
            Node::Submenu { title, items } => {
                gpui::MenuItem::submenu(gpui::Menu::new(title.clone()).items(gpui_items(items)))
            }
            Node::Item(item) => gpui::MenuItem::action(
                item.label,
                keymap::RunCommand::new(item.id, Value::Object(Default::default())),
            ),
        })
        .collect()
}

/// How much of the catalog Spool can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coverage {
    /// Catalog rows that are not separators.
    pub items: usize,
    /// Rows the engine runs.
    pub engine: usize,
    /// Rows the shell runs ([`UI_COMMANDS`]).
    pub ui: usize,
    /// Rows nothing implements, with the menus they sit in.
    pub missing: Vec<(&'static str, &'static str)>,
}

/// Which rows resolve, and which do not.
pub fn coverage() -> Coverage {
    let mut c = Coverage {
        items: 0,
        engine: 0,
        ui: 0,
        missing: Vec::new(),
    };
    for m in MENU.iter().filter(|m| m.label != SEP) {
        c.items += 1;
        // The same order as `request`: the UI intercepts its own ids, whether or not the engine
        // also has a command for them.
        if is_ui_command(m.id) {
            c.ui += 1;
        } else if find_command(m.id).is_some() {
            c.engine += 1;
        } else {
            c.missing.push((m.menu, m.id));
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use figcraft_engine::keymap::Platform;

    /// Every command the catalog names, once.
    fn ids() -> Vec<&'static str> {
        MENU.iter()
            .filter(|m| m.label != SEP)
            .map(|m| m.id)
            .collect()
    }

    /// The catalog tree, walked with its paths.
    fn walk(prefix: &str, nodes: &[Node], out: &mut Vec<String>) {
        for node in nodes {
            match node {
                Node::Separator => {}
                Node::Item(item) => out.push(item.menu.to_string()),
                Node::Submenu { title, items } => walk(&format!("{prefix}/{title}"), items, out),
            }
        }
    }

    #[test]
    fn every_menu_id_is_either_an_engine_command_or_a_ui_command() {
        // The test that catches upstream drift: a catalog row that names a command nothing
        // implements would be a menu item that silently does nothing.
        let c = coverage();
        assert!(
            c.items > 200,
            "the catalog has {} rows, which is not figcraft's",
            c.items
        );
        assert_eq!(
            c.missing,
            Vec::<(&str, &str)>::new(),
            "these catalog rows resolve to nothing"
        );
        assert_eq!(c.engine + c.ui, c.items);
    }

    #[test]
    fn coverage_counts_what_it_says() {
        let c = coverage();
        let rows = MENU.iter().filter(|m| m.label != SEP).count();
        assert_eq!(c.items, rows);
        assert_eq!(
            c.engine,
            MENU.iter()
                .filter(|m| m.label != SEP && !is_ui_command(m.id) && find_command(m.id).is_some())
                .count()
        );
        // 13 ids, each named exactly once in the catalog, and the shell answers for all of them.
        assert_eq!(c.ui, 13);
        assert_eq!(UI_COMMANDS.len(), 13);
        for id in UI_COMMANDS {
            assert_eq!(
                ids().iter().filter(|i| *i == id).count(),
                1,
                "`{id}` appears twice in the catalog"
            );
        }
        // Some of them are engine commands as well: the UI intercepts the id and opens a dialog,
        // and the engine command is what the CLI and the control channel use when they already
        // have a path. The UI still wins for a click.
        let both: Vec<&str> = UI_COMMANDS
            .iter()
            .copied()
            .filter(|id| find_command(id).is_some())
            .collect();
        assert!(
            !both.is_empty(),
            "some UI ids are programmatic engine commands too"
        );
        for id in &both {
            assert!(
                !request(id, Value::Object(Default::default())).is_engine(),
                "`{id}` is the shell's, whatever the registry also has"
            );
        }
    }

    #[test]
    fn the_ui_ids_are_the_documented_ones() {
        for id in [
            "edit.copyAsText",
            "edit.copyAsCss",
            "edit.copyAsSvg",
            "edit.copyAsPng",
            "edit.copyLink",
            "file.open",
            "file.import",
            "file.placeImage",
            "file.save",
            "file.saveAs",
            "file.export",
            "file.exportSvg",
            "plugin.import",
        ] {
            assert!(is_ui_command(id), "`{id}` is missing from UI_COMMANDS");
            assert!(available(id), "`{id}` should be available");
        }
        assert!(!is_ui_command("edit.copy"));
        assert!(!available("edit.nonesuch"));
    }

    #[test]
    fn requests_are_sorted_into_the_layer_that_answers_them() {
        let engine = request("edit.copy", serde_json::json!({}));
        assert!(engine.is_engine());
        assert_eq!(engine.id(), "edit.copy");
        assert_eq!(
            engine,
            UiRequest::Execute {
                id: "edit.copy",
                params: serde_json::json!({})
            }
        );

        let shell = request("file.open", serde_json::json!({"index": 2}));
        assert_eq!(
            shell,
            UiRequest::Shell {
                id: "file.open",
                params: serde_json::json!({"index": 2})
            }
        );
        assert!(!shell.is_engine());

        let unknown = request("edit.nonesuch", serde_json::json!({}));
        assert_eq!(
            unknown,
            UiRequest::Unknown {
                id: "edit.nonesuch".to_string(),
                params: serde_json::json!({})
            }
        );
        assert_eq!(unknown.id(), "edit.nonesuch");
    }

    #[test]
    fn an_engine_command_runs_and_a_ui_command_is_handed_back() {
        let mut session = Session::new();
        let ran = invoke(&mut session, "view.toggleUi", &serde_json::json!({}));
        assert!(ran.ran(), "an engine command should run, got {ran:?}");

        for id in UI_COMMANDS {
            let outcome = invoke(&mut session, id, &serde_json::json!({}));
            assert_eq!(
                outcome,
                Outcome::Deferred(UiRequest::Shell {
                    id,
                    params: serde_json::json!({})
                }),
                "`{id}` should go to the shell"
            );
        }
        assert_eq!(
            invoke(&mut session, "edit.nonesuch", &serde_json::json!({})),
            Outcome::Deferred(UiRequest::Unknown {
                id: "edit.nonesuch".to_string(),
                params: serde_json::json!({})
            })
        );
    }

    /// The engine refusing a command is not the same as the command not being ours.
    #[test]
    fn a_refused_command_is_an_engine_error_not_a_ui_request() {
        let mut session = Session::new();
        // `file.open` needs a path. Through the menu it is the shell's, so this has to be a
        // command the engine alone owns: `file.rename` has the same shape and no dialog.
        let outcome = invoke(&mut session, "file.rename", &serde_json::json!({}));
        assert!(!outcome.ran(), "file.rename with no name should not run");
        assert!(
            outcome.deferred().is_none(),
            "the engine owns file.rename, so this is not a UI request"
        );
        assert!(
            outcome.message().is_some_and(|m| !m.is_empty()),
            "the engine should say why, got {outcome:?}"
        );
    }

    #[test]
    fn the_tree_has_every_top_level_menu_in_catalog_order() {
        let expected: Vec<String> = {
            let mut seen: Vec<String> = Vec::new();
            for m in MENU {
                let head = m.menu.split('/').next().unwrap_or(m.menu).to_string();
                if !seen.contains(&head) {
                    seen.push(head);
                }
            }
            seen
        };
        let tree = tree();
        let titles: Vec<&str> = tree.iter().filter_map(Node::title).collect();
        assert_eq!(
            titles,
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );
        // File, Edit, Object, View, Text, Arrange, Vector, Plugins, Preferences, Help, Libraries, Tools.
        assert_eq!(titles.first(), Some(&"File"));
        assert_eq!(titles.len(), 12);
    }

    #[test]
    fn every_catalog_row_appears_once_in_the_tree() {
        let mut rows = Vec::new();
        for (title, items) in levels() {
            walk(&title, &items, &mut rows);
        }
        let mut catalog: Vec<&str> = MENU
            .iter()
            .filter(|m| m.label != SEP)
            .map(|m| m.menu)
            .collect();
        catalog.sort_unstable();
        rows.sort();
        assert_eq!(rows, catalog);
    }

    #[test]
    fn submenus_nest_by_prefix() {
        let copy_as = menu("Edit").into_iter().find_map(|n| match n {
            Node::Submenu { title, items } if title == "Copy as" => Some(items),
            _ => None,
        });
        let items = copy_as.unwrap_or_else(|| panic!("Edit has no Copy as submenu"));
        assert_eq!(items.len(), 5);
        let labels: Vec<&str> = items
            .iter()
            .filter_map(|n| match n {
                Node::Item(i) => Some(i.label),
                _ => None,
            })
            .collect();
        assert_eq!(
            labels,
            [
                "Copy as text",
                "Copy as CSS",
                "Copy as SVG",
                "Copy as PNG",
                "Copy link to selection"
            ]
        );

        // A submenu whose title contains a space and reads like a prefix of another.
        let booleans = menu("Object").into_iter().find_map(|n| match n {
            Node::Submenu { title, items } if title == "Boolean groups" => Some(items),
            _ => None,
        });
        assert!(
            booleans.is_some_and(|items| items.len() == 5),
            "Union, Subtract, Intersect, Exclude, Flatten"
        );

        let outlines = menu("View").into_iter().find_map(|n| match n {
            Node::Submenu { title, items } if title == "Outlines" => Some(items),
            _ => None,
        });
        assert!(outlines.is_some_and(|items| !items.is_empty()));
    }

    #[test]
    fn separators_collapse_and_never_end_a_menu() {
        for (title, items) in levels() {
            assert!(
                !matches!(items.last(), Some(Node::Separator)),
                "{title} ends in a separator"
            );
            for (i, node) in items.iter().enumerate() {
                if matches!(node, Node::Separator) {
                    assert!(i > 0, "{title} starts with a separator");
                    assert!(
                        !matches!(items[i - 1], Node::Separator),
                        "{title} has two separators in a row"
                    );
                }
            }
        }
        // The File menu's separators survive as separators, not as rows.
        assert!(menu("File").iter().any(|n| matches!(n, Node::Separator)));
    }

    #[test]
    fn items_carry_their_menu_label_and_shortcut() {
        let file = menu("File");
        let first = file
            .iter()
            .find_map(|n| match n {
                Node::Item(i) if i.id == "file.new" => Some(*i),
                _ => None,
            })
            .unwrap_or_else(|| panic!("File has no New design file"));
        assert_eq!(first.label, "New design file");
        assert_eq!(first.menu, "File");
        assert_eq!(first.shortcut(Platform::Mac).as_deref(), Some("⌘N"));
        assert_eq!(first.shortcut(Platform::Other).as_deref(), Some("Ctrl+N"));
        assert!(first.is_available());

        // An item with no key shows nothing rather than an empty gap.
        let unnamed = menu("Help")
            .into_iter()
            .filter_map(|n| match n {
                Node::Item(i) if i.id == "help.page" => Some(i),
                _ => None,
            })
            .next()
            .unwrap_or_else(|| panic!("Help has no Help page"));
        assert_eq!(unnamed.shortcut(Platform::Mac), None);
    }

    #[test]
    fn gpui_menus_cover_the_same_tree() {
        let menus = gpui_menus();
        assert_eq!(menus.len(), levels().len());
        assert_eq!(
            menus.first().map(|m| m.name.to_string()),
            Some("File".to_string())
        );
        assert_eq!(
            menus.last().map(|m| m.name.to_string()),
            Some("Tools".to_string())
        );

        // File has separators and rows; Edit has the submenus.
        let has = |name: &str, want: fn(&gpui::MenuItem) -> bool| {
            menus
                .iter()
                .find(|m| m.name == name)
                .is_some_and(|m| m.items.iter().any(want))
        };
        assert!(has("File", |i| matches!(i, gpui::MenuItem::Separator)));
        assert!(has("Edit", |i| matches!(i, gpui::MenuItem::Submenu(_))));

        // Every action row is a RunCommand carrying the catalog id.
        let mut seen = 0;
        fn count(nodes: &[gpui::MenuItem], seen: &mut usize) {
            for node in nodes {
                match node {
                    gpui::MenuItem::Submenu(menu) => count(&menu.items, seen),
                    gpui::MenuItem::Action { action, .. } => {
                        let action = action.as_any().downcast_ref::<keymap::RunCommand>();
                        assert!(
                            action.is_some_and(|a| !a.id.is_empty()),
                            "a menu row has no command"
                        );
                        *seen += 1;
                    }
                    _ => {}
                }
            }
        }
        for menu in &menus {
            count(&menu.items, &mut seen);
        }
        assert_eq!(seen, MENU.iter().filter(|m| m.label != SEP).count());
    }

    #[test]
    fn a_submenu_is_nested_one_level_deeper_than_its_catalog_path() {
        /// Depth-check one subtree against the path its rows carry.
        fn check(prefix: &str, nodes: &[Node]) {
            for node in nodes {
                let Node::Submenu { title, items } = node else {
                    continue;
                };
                let path = format!("{prefix}/{title}");
                assert_eq!(
                    path.matches('/').count(),
                    prefix.matches('/').count() + 1,
                    "{path}"
                );
                assert!(!items.is_empty(), "{path} is empty");
                check(&path, items);
            }
        }
        for (title, items) in levels() {
            check(&title, &items);
        }
    }
}
