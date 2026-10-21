//! What this CLI deliberately does not expose: the WebAssembly plugin sandbox.
//!
//! `Agents.md` ("Assigned on 2026-10-17") assigns the CLI, the MCP server and core design
//! parity, and says of figcraft's extras:
//!
//! > Still deferred, despite figcraft shipping them: the WebAssembly plugin sandbox, the
//! > browser/wasm build, and the collaboration server app. Do not start them.
//!
//! The engine registers the sandbox's commands anyway — vendoring an engine adopts its registry —
//! so `spool-cli` would otherwise be a shell an agent can drive straight into a subsystem nobody
//! in Spool has designed, reviewed or tested. Two things follow, and both live here:
//!
//! * `list-commands` does not list a deferred command, and
//! * `run`/`script` **refuse** one, by name and by policy, rather than reporting "unknown
//!   command". The engine does register it, so calling it unknown would be a lie, and letting it
//!   through would be the thing the policy forbids.
//!
//! ## Where the boundary is drawn
//!
//! The deferred set is **read out of the engine's own registration lists**, not guessed from an
//! id prefix:
//!
//! * [`figcraft_engine::plugins::specs`] — `plugin.*`, `plugins.*`: import, run, panels, events,
//! * [`figcraft_engine::plugin_directory::specs`] — `plugin.directory.*`: a server's directory,
//!   install, publish, unpublish,
//! * [`figcraft_engine::widgets::specs`] — `widget.*`: layers a plugin draws, a click that calls
//!   the plugin back. These are the sandbox's widget API, and every one of them exists only to
//!   host a wasm module (`widget.insert` names a plugin and runs it), so they are part of the same
//!   deferred subsystem rather than a separate feature.
//!
//! Everything else is in scope and stays reachable — **collaboration above all**, which
//! `Agents.md` explicitly assigns ("Collaboration and the server"), and which shares the
//! `collab.*` prefix, a plugin-facing permission table and a server with the plugin directory. The
//! plugin *directory* is deferred; the plugin *server's* presence in figcraft is not a reason to
//! hide `collab.*`. `crates/spool-cli/src/main.rs` tests both halves of that line.
//!
//! Reading the lists from the engine also means a future `git subtree pull` that adds a sandbox
//! command extends the deferred set with no edit here — and the tests fail loudly if a new id
//! appears outside the three lists.

use std::sync::OnceLock;

/// The policy, named where it is written down.
pub const POLICY: &str =
    "Agents.md (\"Assigned on 2026-10-17\": \"Still deferred, despite figcraft \
                           shipping them: the WebAssembly plugin sandbox, the browser/wasm build, \
                           and the collaboration server app. Do not start them.\")";

/// Every command id the deferred subsystem registers, sorted and deduplicated.
///
/// Taken from the engine's own `specs()` lists, so it cannot drift from the registry it filters.
pub fn deferred() -> &'static [&'static str] {
    static IDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    IDS.get_or_init(|| {
        let mut ids: Vec<&'static str> = figcraft_engine::plugins::specs()
            .iter()
            .chain(figcraft_engine::plugin_directory::specs().iter())
            .chain(figcraft_engine::widgets::specs().iter())
            .map(|s| s.id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    })
}

/// Whether `id` belongs to the deferred plugin sandbox.
pub fn is_deferred(id: &str) -> bool {
    deferred().contains(&id)
}

/// The refusal for `run <id>`: what it is, why it is refused, and where the policy is.
pub fn refusal(id: &str) -> String {
    format!(
        "`{id}` is deferred by Spool's scope policy and spool-cli will not run it: \
         the WebAssembly plugin sandbox is not built in Spool yet. The engine registers the \
         command, so this is not an unknown command — it is a real path to a subsystem that is \
         deliberately not started. Policy: {POLICY}"
    )
}

/// The refusal for a script step, which names the step so the agent can find it in the file.
pub fn script_refusal(step: usize, id: &str) -> String {
    format!("script step {step}: {}", refusal(id))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// The ids the three engine lists register today, spelled out in sorted order. If a subtree
    /// pull adds or renames one, this test fails and the boundary is re-decided on purpose.
    const KNOWN_DEFERRED: &[&str] = &[
        "plugin.close",
        "plugin.directory.install", // plugin_directory::specs()
        "plugin.directory.list",
        "plugin.directory.publish",
        "plugin.directory.unpublish",
        "plugin.event",
        "plugin.import",
        "plugin.list",
        "plugin.remove",
        "plugin.run",
        "plugin.ui",
        "plugins.manage",
        "plugins.runLast",
        "widget.click", // widgets::specs()
        "widget.insert",
        "widget.list",
    ];

    /// In scope, and each of them for a reason: collaboration is assigned by the policy, the rest
    /// are ordinary design-editor commands. None may be swept up by the filter.
    const KNOWN_ALLOWED: &[&str] = &[
        "node.create",
        "node.set",
        "shape.rectangle",
        "align.left",
        "layers.collapseAll",
        "doc.list",
        // Collaboration is explicitly assigned ("Collaboration and the server"), including the
        // commands that share infrastructure with the deferred plugin directory.
        "collab.host",
        "collab.join",
        "collab.peers",
        "collab.stop",
        "collab.share",
        "collab.publish",
        "collab.diff",
        "collab.serverInfo",
    ];

    #[test]
    fn the_deferred_set_is_exactly_the_three_engine_lists() {
        assert_eq!(deferred(), KNOWN_DEFERRED, "{:#?}", deferred());
    }

    #[test]
    fn every_deferred_id_is_a_registered_command() {
        for id in deferred() {
            assert!(
                figcraft_engine::find_command(id).is_some(),
                "`{id}` is deferred but the engine does not register it: the boundary is stale"
            );
        }
    }

    /// The filter must not reach past the sandbox, in either direction.
    #[test]
    fn the_boundary_is_the_sandbox_and_nothing_else() {
        for (id, allowed) in KNOWN_DEFERRED
            .iter()
            .map(|i| (*i, false))
            .chain(KNOWN_ALLOWED.iter().map(|i| (*i, true)))
        {
            assert_eq!(
                is_deferred(id),
                !allowed,
                "`{id}` should {} be deferred",
                if allowed { "not" } else { "" }
            );
        }
    }

    /// A prefix-shaped filter would have caught `collab.*` too. Prove it does not.
    #[test]
    fn collaboration_is_not_deferred_even_though_it_is_prefix_shaped_like_plugins() {
        for spec in figcraft_engine::all_commands() {
            if spec.id.starts_with("collab.") {
                assert!(!is_deferred(spec.id), "`{}` is collaboration", spec.id);
            }
        }
        assert!(figcraft_engine::find_command("collab.host").is_some());
        assert!(!is_deferred("collab.host"));
    }

    /// And no listed id anywhere in the registry is a deferred one.
    #[test]
    fn no_registered_command_outside_the_three_lists_is_deferred() {
        for spec in figcraft_engine::all_commands() {
            let from_a_list = KNOWN_DEFERRED.contains(&spec.id);
            assert_eq!(
                from_a_list,
                is_deferred(spec.id),
                "`{}` disagrees with the boundary lists",
                spec.id
            );
        }
    }

    #[test]
    fn the_refusal_names_the_command_the_policy_and_the_document() {
        let message = refusal("plugin.run");
        assert!(message.contains("`plugin.run`"));
        assert!(message.contains("deferred"));
        assert!(message.contains("WebAssembly plugin sandbox"));
        assert!(message.contains("Agents.md"));
        assert!(
            message.contains("not an unknown command"),
            "it is registered, so saying otherwise would be a lie: {message}"
        );

        let step = script_refusal(2, "widget.click");
        assert!(step.contains("script step 2"));
        assert!(step.contains("`widget.click`"));
        assert!(step.contains("Agents.md"));
    }

    #[test]
    fn an_id_that_is_not_deferred_has_no_refusal() {
        assert!(!is_deferred("node.create"));
        assert!(!is_deferred("pluginx"));
        assert!(!is_deferred(""));
        assert!(!is_deferred("plugin."));
    }
}
