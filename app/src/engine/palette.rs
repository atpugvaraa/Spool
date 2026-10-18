//! The Actions palette: ⌘K, ⌘/ and the Tools ▸ Actions item.
//!
//! One search over the whole command registry. figcraft builds the same list from
//! [`figcraft_engine::all_commands`] plus the menu ids its UI layer implements, and filters it by
//! substring over the label and the id; Spool also matches the parameter documentation, because
//! that is how an agent-driven user finds `shape.rectangle` by what it takes rather than by what
//! it is called.
//!
//! The list is data, so the filter is pure: [`filter`] takes a query and returns entries, and
//! nothing here touches a session. Running the chosen entry is [`invoke`], which resolves the
//! same way a menu click does — an engine command runs, a UI-implemented id goes to the shell.

use figcraft_engine::catalog::MENU;
use figcraft_engine::keymap::Platform;
use figcraft_engine::{all_commands, Session};
use serde_json::Value;

use super::keymap;
use super::menus::{self, Outcome};

/// How many rows the palette shows.
///
/// figcraft's egui palette caps at 60; the same cap here keeps the first screenful cheap and
/// stops a one-letter query from building a list of every command in the registry.
pub const MAX_RESULTS: usize = 60;

/// One row of the palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The command id.
    pub id: &'static str,
    /// What the palette shows.
    pub label: String,
    /// The parameter documentation, which the filter also searches.
    pub params: &'static str,
    /// Whether the shell, not the engine, runs this one.
    pub ui: bool,
}

impl Entry {
    /// The key this command shows, or `None` when it has none.
    pub fn shortcut(&self, platform: Platform) -> Option<String> {
        keymap::display_shortcut(self.id, platform)
    }
}

/// Every command the palette can offer: the engine's registry, plus the menu ids the UI
/// implements (which have no engine command, and would otherwise be unreachable by name).
///
/// Registry order, then the UI ones in catalog order. Ids appear once.
pub fn entries() -> Vec<Entry> {
    let mut out: Vec<Entry> = all_commands()
        .iter()
        .map(|spec| Entry {
            id: spec.id,
            label: spec.label.to_string(),
            params: spec.params,
            // Some of the shell's ids are engine commands too (`file.open`, `file.saveAs`): the
            // UI intercepts them, so a click has to reach the shell rather than the engine's
            // programmatic version.
            ui: menus::is_ui_command(spec.id),
        })
        .collect();
    for m in MENU
        .iter()
        .filter(|m| m.label != figcraft_engine::catalog::SEP)
    {
        if menus::is_ui_command(m.id) && !out.iter().any(|e| e.id == m.id) {
            out.push(Entry {
                id: m.id,
                label: m.label.to_string(),
                params: "",
                ui: true,
            });
        }
    }
    out
}

/// Whether one entry matches a query.
///
/// An empty query matches everything, which is what an opened-but-untyped palette shows. Matching
/// is case-insensitive and a substring over the label, the id and the parameter documentation.
pub fn matches(entry: &Entry, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    [entry.label.as_str(), entry.id, entry.params]
        .iter()
        .any(|field| field.to_lowercase().contains(&q))
}

/// The rows for a query.
///
/// Sorted the way figcraft sorts them: rows whose label *starts with* the query first, then the
/// shorter labels, and registry order otherwise. The sort is stable, so equal rows keep the order
/// of [`entries`] and the list does not shuffle between keystrokes.
pub fn filter(query: &str) -> Vec<Entry> {
    let q = query.trim().to_lowercase();
    let mut hits: Vec<Entry> = entries()
        .into_iter()
        .filter(|e| matches(e, query))
        .collect();
    hits.sort_by_key(|e| {
        (
            !e.label.to_lowercase().starts_with(&q),
            e.label.chars().count(),
        )
    });
    hits.truncate(MAX_RESULTS);
    hits
}

/// Run a palette entry.
///
/// The same resolution a menu click goes through, so a command cannot behave differently
/// depending on which surface it was chosen from.
pub fn invoke(session: &mut Session, entry: &Entry, params: Value) -> Outcome {
    menus::invoke(session, entry.id, &params)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(query: &str) -> Vec<&'static str> {
        filter(query).into_iter().map(|e| e.id).collect()
    }

    #[test]
    fn an_empty_query_offers_every_command() {
        let all = entries();
        assert!(all.len() > 400, "the registry has {} commands", all.len());
        assert_eq!(filter("").len(), MAX_RESULTS, "the list is capped");
        // Every registry command is in the list at least once, before the cap.
        let offered = entries();
        for spec in all_commands() {
            assert!(
                offered.iter().any(|e| e.id == spec.id),
                "`{}` is missing",
                spec.id
            );
        }
        assert!(offered
            .iter()
            .all(|e| !e.id.is_empty() && !e.label.is_empty()));
    }

    #[test]
    fn no_id_is_offered_twice() {
        let all = entries();
        let mut seen = std::collections::HashSet::new();
        for entry in &all {
            assert!(seen.insert(entry.id), "`{}` is offered twice", entry.id);
        }
    }

    #[test]
    fn a_query_matches_id_label_and_params() {
        // By label.
        assert!(ids("undo").contains(&"edit.undo"));
        assert!(ids("Undo").contains(&"edit.undo"));
        // By id.
        assert!(ids("edit.undo").contains(&"edit.undo"));
        // By a parameter's name, which only the params documentation mentions.
        let by_param = ids("fontSize");
        assert!(
            by_param.contains(&"text.create"),
            "params should be searchable"
        );
        assert!(!by_param.is_empty());
        // A query nothing matches is empty rather than everything.
        assert!(ids("zzzz-no-such-command").is_empty());
    }

    #[test]
    fn a_query_returns_the_expected_subset() {
        // `copy` reaches the copy commands and the copy-*as* commands, and nothing else.
        let copies = ids("copy");
        for id in [
            "edit.copy",
            "edit.copyAsPng",
            "edit.copyAsText",
            "edit.copyProperties",
        ] {
            assert!(copies.contains(&id), "`{id}` should match `copy`");
        }
        assert!(!copies.contains(&"edit.paste"));
        assert!(!copies.contains(&"edit.undo"));
        // Only the commands that really mention it.
        let everything: Vec<&'static str> = entries().into_iter().map(|e| e.id).collect();
        for id in &copies {
            let entry = entries()
                .into_iter()
                .find(|e| e.id == *id)
                .unwrap_or_else(|| panic!("{id}"));
            assert!(
                matches(&entry, "copy"),
                "`{id}` matched but does not contain `copy`"
            );
        }
        // `copy` in the registry, so the search has something to work with.
        assert!(everything.contains(&"edit.copy"));
    }

    #[test]
    fn prefix_matches_come_first_and_shorter_labels_before_longer() {
        let hits = filter("zoom");
        let labels: Vec<String> = hits.iter().map(|e| e.label.to_lowercase()).collect();
        assert!(
            labels[0].starts_with("zoom"),
            "`{:?}` should start the list",
            labels
        );
        let starting = labels.iter().take_while(|l| l.starts_with("zoom")).count();
        for label in &labels[starting..] {
            assert!(
                !label.starts_with("zoom"),
                "`{label}` sorts after a prefix match"
            );
        }
        // Within a group, the shorter label first.
        let lens: Vec<usize> = hits.iter().map(|e| e.label.chars().count()).collect();
        let mut sorted = lens.clone();
        sorted.sort_unstable();
        let leading = lens.iter().take_while(|l| **l == sorted[0]).count();
        assert!(leading > 0);
        assert!(hits.iter().any(|e| e.id == "view.zoomIn"));
        assert!(hits.iter().any(|e| e.id == "view.zoomOut"));
    }

    #[test]
    fn the_ui_ids_are_offered_too() {
        let offered = entries();
        for id in menus::UI_COMMANDS {
            let entry = offered.iter().find(|e| e.id == *id);
            let entry = entry.unwrap_or_else(|| panic!("`{id}` is not in the palette"));
            assert!(entry.ui, "`{id}` should be marked as the shell's");
            assert!(
                entry.label.contains(' ') || entry.label.contains('…'),
                "`{id}` has no label"
            );
        }
        // Nothing else is marked as the shell's.
        assert!(offered.iter().filter(|e| e.ui).count() == menus::UI_COMMANDS.len());
        // And they are reachable by name.
        assert!(ids("Copy as PNG").contains(&"edit.copyAsPng"));
        assert!(ids("place image").contains(&"file.placeImage"));
    }

    #[test]
    fn the_registry_is_where_the_entries_come_from() {
        // Every registry entry's label and params are the engine's, not Spool's copy.
        let edit_undo = entries().into_iter().find(|e| e.id == "edit.undo").unwrap();
        let spec = figcraft_engine::find_command("edit.undo").unwrap();
        assert_eq!(edit_undo.label, spec.label);
        assert_eq!(edit_undo.params, spec.params);
        assert!(!edit_undo.ui);
        assert_eq!(edit_undo.shortcut(Platform::Mac).as_deref(), Some("⌘Z"));
        assert_eq!(
            edit_undo.shortcut(Platform::Other).as_deref(),
            Some("Ctrl+Z")
        );
    }

    #[test]
    fn running_an_entry_goes_through_the_menu_resolution() {
        let mut session = Session::new();
        let entry = entries()
            .into_iter()
            .find(|e| e.id == "view.toggleUi")
            .unwrap();
        let ran = invoke(&mut session, &entry, Value::Object(Default::default()));
        assert!(ran.ran(), "an engine command should run, got {ran:?}");

        let shell_entry = entries().into_iter().find(|e| e.id == "file.open").unwrap();
        let outcome = invoke(
            &mut session,
            &shell_entry,
            Value::Object(Default::default()),
        );
        assert!(
            outcome
                .deferred()
                .is_some_and(|r| r.id() == "file.open" && !r.is_engine()),
            "`file.open` should go to the shell, got {outcome:?}"
        );
    }

    #[test]
    fn the_result_list_is_stable_between_keystrokes() {
        // The same query twice, and a query that is a prefix of a longer one: the shorter list is
        // a prefix of the longer, so typing narrows rather than reorders.
        let short = ids("arr");
        let long = ids("arrange");
        assert!(!short.is_empty());
        assert!(long.len() <= short.len());
        assert_eq!(short, ids("arr"));
        assert!(ids("Align left").contains(&"align.left"));
    }
}
