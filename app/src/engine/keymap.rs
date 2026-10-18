//! figcraft's default keyboard shortcuts, as GPUI key bindings.
//!
//! The bindings are data, not code: [`figcraft_engine::keymap::KEYS`] is one table of
//! (group, command id, macOS keys, Windows/Linux keys, context) that figcraft's menus, its
//! shortcuts panel, its canvas, the control channel and its MCP `key` tool all read. Nothing here
//! redefines a key — this module only translates between two spellings of the same chord, so a
//! shortcut changed upstream changes here too.
//!
//! # The `cmd-` / `super-` / `win-` mismatch
//!
//! figcraft writes the platform modifier as `Cmd` in *every* spec, whatever the platform:
//! `Shift+Cmd+K` is ⇧⌘K on macOS and Ctrl+Shift+K elsewhere, and figcraft resolves that itself in
//! `Platform::Other`, where Cmd and Ctrl are one key.
//!
//! GPUI does the opposite. [`gpui::Keystroke::parse`] accepts `cmd-`, `super-` and `win-` as
//! synonyms for its `platform` modifier, so a binding *parses* on every OS — but
//! [`gpui::Keystroke::unparse`] writes back whatever this OS calls that key: `cmd-` on macOS,
//! `super-` on Linux and FreeBSD, `win-` on Windows. Comparing that string against figcraft's
//! `Cmd` spelling fails on two of the three platforms, which is the kind of failure that only
//! shows up on the machine you are not building on.
//!
//! So every comparison here goes through [`normalized`] (or [`normalized_keystroke`]), which
//! rewrites a leading `win-`/`super-` to `cmd-` before anything compares it, and the tests assert
//! the normalized spelling on both platforms for every row of the table.
//!
//! # The letter-key trap
//!
//! GPUI's parser reads an upper-case single character as that key with Shift held (`K` becomes
//! `shift-k`), because that is how a keyboard reports it. figcraft writes `Cmd+K` for the chord
//! with no Shift at all, so emitting figcraft's spelling verbatim would silently add Shift to
//! every letter shortcut. [`keystroke`] therefore lower-cases single characters and writes
//! `shift-` only when figcraft asked for Shift.

use std::borrow::Cow;

use figcraft_engine::keymap::{self, Binding, Context, Group, Mods, Platform, KEYS};
use gpui::{Action, DummyKeyboardMapper, KeyBinding, Keystroke};
use serde_json::Value;

/// The action a menu item, a shortcut or the Actions palette dispatches.
///
/// One action carries any command id, because the commands are data: figcraft registers hundreds
/// of them in one table and every surface (menus, shortcuts, palette, CLI, control channel, MCP)
/// dispatches the same ids. A GPUI action type per command would mean generating hundreds of
/// types from that table, and a keymap nobody could read without the generation step.
///
/// This is deliberately not registered with GPUI's action registry: registration exists so a
/// keymap can be loaded from JSON by action name, and Spool's keymap is code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunCommand {
    /// A figcraft command id, or one of the ids the UI layer implements itself
    /// (`crate::engine::menus::UI_COMMANDS`).
    pub id: String,
    /// The params to run it with; `{}` for the ones that take none.
    pub params: Value,
}

impl RunCommand {
    pub fn new(id: impl Into<String>, params: Value) -> Self {
        Self {
            id: id.into(),
            params,
        }
    }
}

impl Action for RunCommand {
    fn boxed_clone(&self) -> Box<dyn Action> {
        Box::new(self.clone())
    }

    fn partial_eq(&self, action: &dyn Action) -> bool {
        action.as_any().downcast_ref::<Self>() == Some(self)
    }

    fn name(&self) -> &'static str {
        "spool::RunCommand"
    }

    fn name_for_type() -> &'static str {
        "spool::RunCommand"
    }

    /// Spool's keymap is code, so this action is never built from JSON by name.
    fn build(_: Value) -> gpui::Result<Box<dyn Action>> {
        Err(gpui::private::anyhow::anyhow!(
            "spool::RunCommand carries a command id, so it is not built from JSON"
        ))
    }
}

/// One default shortcut, resolved for a platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    /// The command it runs.
    pub id: &'static str,
    /// The tab of the shortcuts panel it is listed under.
    pub group: Group,
    /// Where it is live.
    pub context: Context,
    /// The platform its keys were read for.
    pub platform: Platform,
    /// figcraft's spelling, `Shift+Cmd+K`.
    pub spec: &'static str,
    /// GPUI's spelling, `cmd-shift-k`.
    pub keystroke: String,
    /// What a menu shows beside the label, `⇧⌘K`.
    pub display: String,
}

/// A chord in GPUI's spelling: lower-case keys, modifiers in GPUI's own order.
///
/// `None` for a spec figcraft's parser rejects, or a key GPUI cannot name. Nothing here panics:
/// the table is data, and an upstream key this mapping has never seen must not take the editor
/// down. [`unresolvable`] reports those instead of dropping them silently.
pub fn keystroke(spec: &str) -> Option<String> {
    let (mods, key) = keymap::parse(spec)?;
    let mut out = String::new();
    if mods.ctrl {
        out.push_str("ctrl-");
    }
    if mods.alt {
        out.push_str("alt-");
    }
    if mods.cmd {
        out.push_str("cmd-");
    }
    if mods.shift {
        out.push_str("shift-");
    }
    out.push_str(&gpui_key(key)?);
    Some(out)
}

/// figcraft's canonical key name as GPUI names it, or `None` when GPUI has no name for it.
///
/// A single character comes back lower-cased: [`gpui::Keystroke::parse`] reads an upper-case
/// letter as that key with Shift held. Named keys are the names GPUI stores, all lower-case —
/// its parser lower-cases them so they compare case-insensitively.
pub fn gpui_key(key: &str) -> Option<Cow<'static, str>> {
    Some(match key {
        "Enter" => Cow::Borrowed("enter"),
        "Escape" => Cow::Borrowed("escape"),
        "Tab" => Cow::Borrowed("tab"),
        "Delete" => Cow::Borrowed("delete"),
        "Space" => Cow::Borrowed("space"),
        "PageUp" => Cow::Borrowed("pageup"),
        "PageDown" => Cow::Borrowed("pagedown"),
        "Home" => Cow::Borrowed("home"),
        "End" => Cow::Borrowed("end"),
        "ArrowLeft" => Cow::Borrowed("left"),
        "ArrowRight" => Cow::Borrowed("right"),
        "ArrowUp" => Cow::Borrowed("up"),
        "ArrowDown" => Cow::Borrowed("down"),
        "F1" => Cow::Borrowed("f1"),
        "F2" => Cow::Borrowed("f2"),
        "F3" => Cow::Borrowed("f3"),
        "F4" => Cow::Borrowed("f4"),
        "F5" => Cow::Borrowed("f5"),
        "F6" => Cow::Borrowed("f6"),
        "F7" => Cow::Borrowed("f7"),
        "F8" => Cow::Borrowed("f8"),
        "F9" => Cow::Borrowed("f9"),
        "F10" => Cow::Borrowed("f10"),
        "F11" => Cow::Borrowed("f11"),
        "F12" => Cow::Borrowed("f12"),
        single if single.chars().count() == 1 => Cow::Owned(single.to_lowercase()),
        _ => return None,
    })
}

/// A GPUI key name as figcraft's parser needs it, for the other direction: a keystroke arriving
/// from the platform, handed to [`figcraft_engine::keymap::lookup`] so one table decides what runs.
///
/// Empty for a key figcraft has no name for, which no binding can match.
pub fn figcraft_key(key: &str) -> Cow<'static, str> {
    match key {
        "enter" => Cow::Borrowed("Enter"),
        "escape" => Cow::Borrowed("Escape"),
        "tab" => Cow::Borrowed("Tab"),
        "delete" => Cow::Borrowed("Delete"),
        "pageup" => Cow::Borrowed("PageUp"),
        "pagedown" => Cow::Borrowed("PageDown"),
        "left" => Cow::Borrowed("ArrowLeft"),
        "right" => Cow::Borrowed("ArrowRight"),
        "up" => Cow::Borrowed("ArrowUp"),
        "down" => Cow::Borrowed("ArrowDown"),
        // figcraft's parser matches its own named keys case-insensitively, so the ones already
        // spelled the same way — `home`, `end`, `space`, `f1`…`f12` — are passed through.
        "home" => Cow::Borrowed("home"),
        "end" => Cow::Borrowed("end"),
        "space" => Cow::Borrowed("space"),
        f if is_function_key(f) => Cow::Owned(f.to_lowercase()),
        single if single.len() == 1 => Cow::Owned(single.to_lowercase()),
        _ => Cow::Borrowed(""),
    }
}

/// Whether a GPUI key name is one of the function keys figcraft names (`F1`…`F12`).
fn is_function_key(key: &str) -> bool {
    let Some(digits) = key.strip_prefix('f') else {
        return false;
    };
    matches!(digits.parse::<u8>(), Ok(1..=12))
}

/// The bindings that are live in `context` on `platform`, in [`KEYS`] order.
pub fn bindings_in(platform: Platform, context: Context) -> Vec<&'static Binding> {
    KEYS.iter()
        .filter(|b| b.context == context || b.context == Context::Both)
        .filter(|b| !b.keys(platform).is_empty())
        .collect()
}

/// Every default shortcut for `platform`: one entry per chord, so a command with two keys has
/// two entries.
pub fn shortcuts(platform: Platform) -> Vec<Shortcut> {
    KEYS.iter()
        .flat_map(|b| b.keys(platform).iter().map(move |spec| (*spec, b)))
        .filter_map(|(spec, b)| {
            Some(Shortcut {
                id: b.id,
                group: b.group,
                context: b.context,
                platform,
                spec,
                keystroke: keystroke(spec)?,
                display: keymap::display(spec, platform),
            })
        })
        .collect()
}

/// The GPUI key bindings to install for `platform`.
///
/// Only the canvas-side table: a `Text`-only binding belongs to the text editing layer, which
/// knows whether text is being edited and this does not. `Context::Both` bindings belong to both
/// tables, which is what "both" means.
///
/// Built with [`KeyBinding::load`] rather than [`KeyBinding::new`], because `new` unwraps the
/// parse and a library must not. Every key here comes from figcraft's table and [`keystroke`]
/// has already accepted it, so a failure is a bug rather than bad input; [`unresolvable`] counts
/// it instead of aborting the editor.
pub fn bindings(platform: Platform) -> Vec<KeyBinding> {
    KEYS.iter()
        .filter(|b| b.context != Context::Text)
        .flat_map(|b| b.keys(platform).iter().map(move |spec| (*spec, b)))
        .filter_map(|(spec, b)| {
            KeyBinding::load(
                &keystroke(spec)?,
                Box::new(RunCommand::new(b.id, Value::Object(Default::default()))),
                None,
                false,
                None,
                &DummyKeyboardMapper,
            )
            .ok()
        })
        .collect()
}

/// The chords in figcraft's table that Spool cannot spell as GPUI key bindings.
///
/// Empty today, and a test asserts it: this is where an upstream key this mapping has never seen
/// would surface.
pub fn unresolvable(platform: Platform) -> Vec<(&'static str, &'static str)> {
    KEYS.iter()
        .filter(|b| b.context != Context::Text)
        .flat_map(|b| b.keys(platform).iter().map(move |spec| (*spec, b)))
        .filter(|(spec, _)| keystroke(spec).is_none())
        .map(|(spec, b)| (spec, b.id))
        .collect()
}

/// A binding's first keystroke in figcraft's spelling.
///
/// [`gpui::Keystroke::unparse`] writes the platform modifier as this OS names it — `cmd-`,
/// `super-` or `win-` — while figcraft's table always writes `Cmd`. Rewriting that prefix is the
/// whole difference between a comparison that works on one platform and one that works on three.
pub fn normalized(binding: &KeyBinding) -> String {
    let key = binding
        .keystrokes()
        .first()
        .map(|k| k.unparse())
        .unwrap_or_default();
    ["win-", "super-"]
        .iter()
        .find_map(|prefix| key.strip_prefix(prefix))
        .map_or(key.clone(), |rest| format!("cmd-{rest}"))
}

/// A keystroke in figcraft's spelling, built from the modifiers the platform reported.
///
/// The counterpart of [`keystroke`] for a chord that actually arrived: the spelling both sides
/// agree on, whatever this OS calls the platform modifier. Key names come back lower-cased, the
/// way [`keystroke`] writes them, so the two are comparable.
pub fn normalized_keystroke(keystroke: &Keystroke) -> String {
    let key = figcraft_key(&keystroke.key);
    if key.is_empty() {
        return String::new();
    }
    let mut mods = String::new();
    if keystroke.modifiers.control {
        mods.push_str("ctrl-");
    }
    if keystroke.modifiers.alt {
        mods.push_str("alt-");
    }
    if keystroke.modifiers.platform {
        mods.push_str("cmd-");
    }
    if keystroke.modifiers.shift {
        mods.push_str("shift-");
    }
    format!("{mods}{}", key.to_lowercase())
}

/// The command a chord runs in `context` on `platform`.
///
/// Delegated to figcraft's own [`keymap::lookup`] rather than resolved against a second table
/// here, so a chord can only ever mean one thing in Spool and in figcraft.
pub fn lookup(platform: Platform, context: Context, key: &str, mods: Mods) -> Option<&'static str> {
    keymap::lookup(platform, context, key, mods).map(|b| b.id)
}

/// [`lookup`], for a keystroke the platform handed us.
pub fn from_keystroke(
    platform: Platform,
    context: Context,
    keystroke: &Keystroke,
) -> Option<&'static str> {
    lookup(
        platform,
        context,
        &figcraft_key(&keystroke.key),
        Mods {
            shift: keystroke.modifiers.shift,
            alt: keystroke.modifiers.alt,
            // figcraft's `Cmd` is the platform modifier on every OS; on Windows and Linux figcraft
            // resolves Cmd and Ctrl to the same key, and GPUI reports that key as `control`.
            cmd: keystroke.modifiers.platform,
            ctrl: keystroke.modifiers.control,
        },
    )
}

/// The key a menu shows for a command on `platform`, or `None` when it has none.
pub fn display_shortcut(id: &str, platform: Platform) -> Option<String> {
    keymap::primary(id, platform).map(|spec| keymap::display(spec, platform))
}

/// The tabs of the Keyboard shortcuts panel, in figcraft's order.
pub fn groups() -> [Group; 12] {
    Group::ALL
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    const PLATFORMS: [Platform; 2] = [Platform::Mac, Platform::Other];

    fn parse(spec: &str) -> Keystroke {
        Keystroke::parse(spec).unwrap_or_else(|_| panic!("`{spec}` doesn't parse"))
    }

    fn mods_of(spec: &str) -> (Mods, String) {
        keymap::parse(spec)
            .map(|(mods, key)| (mods, key.to_string()))
            .unwrap_or_else(|| panic!("`{spec}` doesn't parse"))
    }

    /// figcraft's spelling, rebuilt from the keystroke the platform actually produced — which is
    /// how a shortcut arrives at runtime, and how `unparse` spells the platform modifier.
    fn round_trip(spec: &str) -> String {
        normalized_keystroke(&parse(&keystroke(spec).unwrap_or_default()))
    }

    /// Chords in the canvas-side table, as GPUI would install them. Pinned so a change upstream
    /// that silently drops or duplicates a row is visible here.
    fn expected_count() -> usize {
        KEYS.iter()
            .filter(|b| b.context != Context::Text)
            .map(|b| b.keys(Platform::Mac).len())
            .sum()
    }

    #[test]
    fn every_chord_in_the_table_can_be_spelled() {
        for platform in PLATFORMS {
            assert_eq!(
                unresolvable(platform),
                Vec::new(),
                "{platform:?} has a chord Spool cannot spell"
            );
        }
    }

    #[test]
    fn bindings_have_the_expected_count() {
        assert_eq!(expected_count(), 165);
        for platform in PLATFORMS {
            let built = bindings(platform);
            assert_eq!(built.len(), expected_count(), "{platform:?}");
            assert!(
                built.iter().all(|b| !b.keystrokes().is_empty()),
                "{platform:?}"
            );
        }
    }

    /// `KeyBinding::new` panics on a bad keystroke; building the table must not, whatever the
    /// table says.
    #[test]
    fn building_the_table_never_panics() {
        assert!(!bindings(Platform::Mac).is_empty());
        assert!(!bindings(Platform::Other).is_empty());
        // A chord no key names at all is dropped rather than fatal.
        assert!(keystroke("Cmd+Fn+Hyper").is_none());
    }

    #[test]
    fn every_binding_carries_its_command() {
        for binding in bindings(Platform::Mac) {
            let action = binding.action().as_any().downcast_ref::<RunCommand>();
            let action = action.unwrap_or_else(|| panic!("a menu binding is not a RunCommand"));
            let id = action.id.clone();
            assert!(!id.is_empty());
            assert_eq!(action.params, serde_json::json!({}));
            // The action's command is one the keymap table itself binds.
            assert!(
                KEYS.iter().any(|b| b.id == id),
                "`{id}` is bound but is not in the keymap"
            );
        }
    }

    /// Every chord runs its own command, on the platform it is read for, in the context it is
    /// live in.
    ///
    /// Not in both contexts: figcraft's table is ordered, and `lookup` answers with the first
    /// binding that matches, so a text-only binding legitimately shadows a canvas one while text
    /// is being edited — `Cmd+K` is Actions on the canvas and a text link while editing. Upstream's
    /// own parity test asserts the same thing (`fires(..).contains(&Some(id))`), for the same
    /// reason.
    #[test]
    fn every_binding_fires_its_command() {
        for platform in PLATFORMS {
            for b in KEYS.iter().filter(|b| b.context != Context::Text) {
                for spec in b.keys(platform) {
                    let (mods, key) = mods_of(spec);
                    let fires: [Option<&'static str>; 2] = [Context::Canvas, Context::Text]
                        .map(|context| lookup(platform, context, &key, mods));
                    assert!(
                        fires.contains(&Some(b.id)),
                        "{platform:?} `{spec}` should run {}, runs {fires:?}",
                        b.id
                    );
                }
            }
        }
    }

    /// A chord never resolves to a command that does not bind it: the whole point of reading the
    /// chord out of a keystroke rather than trusting the string it came from.
    #[test]
    fn a_chord_only_ever_resolves_to_a_command_that_binds_it() {
        for platform in PLATFORMS {
            for b in KEYS {
                for spec in b.keys(platform) {
                    let (mods, key) = mods_of(spec);
                    for context in [Context::Canvas, Context::Text] {
                        let Some(id) = lookup(platform, context, &key, mods) else {
                            continue;
                        };
                        let resolved: Vec<_> = KEYS.iter().filter(|r| r.id == id).collect();
                        assert!(
                            resolved.iter().any(|r| r
                                .keys(platform)
                                .iter()
                                .any(|s| keymap::matches(s, platform, &key, mods))),
                            "{platform:?} `{spec}` in {context:?} ran {id}, which does not bind it"
                        );
                    }
                }
            }
        }
    }

    /// Where figcraft says the two platforms agree, they agree here too.
    ///
    /// A binding with no Windows/Linux column means "the macOS keys with `Cmd` read as Ctrl", so
    /// the same chord must reach the same command on both. Off macOS, Control is that key: a chord
    /// written with `Cmd` answers to Control, and one written with neither must not.
    #[test]
    fn platforms_agree_where_figcraft_says_they_do() {
        for b in KEYS {
            if b.context == Context::Text || !b.other.is_empty() {
                continue;
            }
            for spec in b.keys(Platform::Mac) {
                let (mods, key) = mods_of(spec);
                assert_eq!(
                    lookup(Platform::Other, Context::Canvas, &key, mods),
                    Some(b.id),
                    "`{spec}` should run {} on Windows and Linux too",
                    b.id
                );
                let as_ctrl = Mods {
                    ctrl: true,
                    cmd: false,
                    ..mods
                };
                let with_ctrl = lookup(Platform::Other, Context::Canvas, &key, as_ctrl);
                let writes_cmd = b
                    .keys(Platform::Mac)
                    .iter()
                    .any(|s| keymap::parse(s).is_some_and(|(m, _)| m.cmd));
                if mods.cmd && !mods.shift && !mods.alt {
                    // Off macOS `Cmd` *is* `Ctrl`: the same chord under the key the platform has,
                    // which is the whole reason figcraft resolves Cmd there at all.
                    assert_eq!(
                        with_ctrl,
                        lookup(Platform::Other, Context::Canvas, &key, mods),
                        "`{spec}` should answer to Control as well as Cmd"
                    );
                } else if mods.ctrl {
                    // The spec names Control itself: the same chord, either way.
                    assert_eq!(with_ctrl, Some(b.id), "`{spec}`");
                } else if !writes_cmd {
                    assert_ne!(
                        with_ctrl,
                        Some(b.id),
                        "`{spec}` writes no Cmd anywhere, so Control must not reach {} there",
                        b.id
                    );
                }
            }
        }
    }

    /// Where figcraft says they differ, both platforms' chords reach the same command on their
    /// own platform.
    #[test]
    fn platforms_differ_where_figcraft_says_they_do() {
        let differing: Vec<_> = KEYS.iter().filter(|b| !b.other.is_empty()).collect();
        assert!(
            !differing.is_empty(),
            "the table has platform-specific bindings"
        );
        for b in differing {
            for (platform, specs) in [
                (Platform::Mac, b.keys(Platform::Mac)),
                (Platform::Other, b.keys(Platform::Other)),
            ] {
                for spec in specs {
                    let (mods, key) = mods_of(spec);
                    assert_eq!(
                        lookup(platform, Context::Canvas, &key, mods),
                        Some(b.id),
                        "{platform:?} `{spec}` should run {}",
                        b.id
                    );
                }
            }
        }
    }

    /// The point of the normalisation: a chord written with figcraft's `Cmd` comes back out of
    /// GPUI with `cmd-`, whichever OS is running the test.
    #[test]
    fn the_platform_modifier_always_reads_as_cmd() {
        for platform in PLATFORMS {
            for binding in bindings(platform) {
                let key = normalized(&binding);
                assert!(
                    !key.contains("super-") && !key.contains("win-"),
                    "`{key}` keeps this OS's spelling of the platform modifier"
                );
                // Whatever the OS calls it, the modifier is still the platform modifier.
                let parsed = parse(&key);
                assert_eq!(
                    parsed.modifiers.platform,
                    key.contains("cmd-"),
                    "`{key}` should say cmd- exactly when the platform modifier is held"
                );
            }
        }
        for b in KEYS {
            for platform in PLATFORMS {
                for spec in b.keys(platform) {
                    let Some(expected) = keystroke(spec) else {
                        continue;
                    };
                    assert_eq!(
                        round_trip(spec),
                        expected,
                        "{platform:?} `{spec}` should survive the round trip"
                    );
                }
            }
        }
    }

    /// A letter key must not gain a Shift it never had: GPUI's parser reads `K` as `shift-k`.
    #[test]
    fn letter_keys_do_not_gain_a_shift() {
        assert_eq!(keystroke("Cmd+K").as_deref(), Some("cmd-k"));
        assert_eq!(keystroke("Shift+Cmd+K").as_deref(), Some("cmd-shift-k"));
        assert_eq!(keystroke("Ctrl+C").as_deref(), Some("ctrl-c"));
        assert_eq!(keystroke("Alt+Shift+S").as_deref(), Some("alt-shift-s"));
        assert_eq!(keystroke("Cmd+Shift+Z").as_deref(), Some("cmd-shift-z"));
        for b in KEYS {
            for platform in PLATFORMS {
                for spec in b.keys(platform) {
                    let Some((mods, _key)) = keymap::parse(spec) else {
                        continue;
                    };
                    let got = keystroke(spec).unwrap_or_default();
                    assert_eq!(
                        got.contains("shift-"),
                        mods.shift,
                        "`{spec}` should {} Shift",
                        if mods.shift { "carry" } else { "not carry" }
                    );
                }
            }
        }
    }

    /// Named keys, and the two characters GPUI's parser treats specially.
    #[test]
    fn named_keys_are_spelled_the_way_gpui_spells_them() {
        assert_eq!(keystroke("Enter").as_deref(), Some("enter"));
        assert_eq!(keystroke("Escape").as_deref(), Some("escape"));
        assert_eq!(keystroke("Tab").as_deref(), Some("tab"));
        assert_eq!(keystroke("Delete").as_deref(), Some("delete"));
        assert_eq!(keystroke("PageUp").as_deref(), Some("pageup"));
        assert_eq!(keystroke("PageDown").as_deref(), Some("pagedown"));
        assert_eq!(keystroke("ArrowLeft").as_deref(), Some("left"));
        assert_eq!(keystroke("F6").as_deref(), Some("f6"));
        // A shifted symbol is the unshifted key with Shift held, never the symbol itself.
        assert_eq!(keystroke("?").as_deref(), Some("shift-/"));
        assert_eq!(keystroke("Shift+.").as_deref(), Some("shift-."));
        // A bare minus: GPUI's parser reads a trailing `-` as the key.
        assert_eq!(keystroke("-").as_deref(), Some("-"));
        assert_eq!(keystroke("Cmd+-").as_deref(), Some("cmd--"));
        assert_eq!(parse("cmd--").key, "-");
    }

    #[test]
    fn key_names_round_trip() {
        for key in [
            "Enter",
            "Escape",
            "Tab",
            "Delete",
            "PageUp",
            "PageDown",
            "ArrowLeft",
        ] {
            assert_eq!(
                figcraft_key(&gpui_key(key).unwrap_or_default()),
                key,
                "{key}"
            );
        }
        // The named keys figcraft's parser matches case-insensitively survive either spelling.
        for key in ["Home", "End", "Space", "F6"] {
            let gpui = gpui_key(key).unwrap_or_default();
            assert_eq!(
                figcraft_key(&gpui).to_lowercase(),
                key.to_lowercase(),
                "{key}"
            );
        }
        for key in [
            "a", "z", "0", "9", "=", "-", "[", "]", "\\", "'", ",", ".", "/", ";",
        ] {
            assert_eq!(keystroke(key).as_deref(), Some(key), "{key}");
        }
        // A key neither side names resolves to nothing rather than to a wrong command.
        assert!(figcraft_key("f13").is_empty());
        assert!(gpui_key("Hyper").is_none());
    }

    /// A keystroke arriving from the platform reaches the same command as the spec it was built
    /// from, in both contexts.
    ///
    /// Not necessarily the binding it was built from: `Cmd+K` is Actions on the canvas and a text
    /// link while editing text, because figcraft's table is ordered and `Cmd+K` binds both.
    #[test]
    fn a_platform_keystroke_resolves_the_same_way() {
        for platform in PLATFORMS {
            for b in KEYS {
                for spec in b.keys(platform) {
                    let Some(ks) = keystroke(spec).and_then(|k| Keystroke::parse(&k).ok()) else {
                        continue;
                    };
                    let (mods, key) = mods_of(spec);
                    for context in [Context::Canvas, Context::Text] {
                        assert_eq!(
                            from_keystroke(platform, context, &ks),
                            lookup(platform, context, &key, mods),
                            "{platform:?} `{spec}` in {context:?}"
                        );
                    }
                }
            }
        }
    }

    /// The chord a binding installs is one the keymap resolves back to a command that binds it,
    /// which is what makes the GPUI table and figcraft's table the same table.
    #[test]
    fn an_installed_chord_resolves_to_its_own_command_or_to_one_that_binds_it() {
        for platform in PLATFORMS {
            for binding in bindings(platform) {
                let spelling = normalized(&binding);
                let Some(ks) = Keystroke::parse(&spelling).ok() else {
                    panic!("`{spelling}` is not a keystroke");
                };
                let Some(id) = from_keystroke(platform, Context::Canvas, &ks) else {
                    panic!("`{spelling}` is installed but resolves to nothing");
                };
                let resolved = KEYS.iter().find(|b| b.id == id);
                assert!(
                    resolved.is_some_and(|b| b.context != Context::Text),
                    "`{spelling}` is installed but {id} is not a canvas binding"
                );
            }
        }
    }

    /// The platform modifier is figcraft's `Cmd`, and Control stays Control.
    #[test]
    fn modifiers_map_to_the_platform_key() {
        let platform = Platform::Mac;
        let cmd_c = parse("cmd-c");
        let ctrl_c = parse("ctrl-c");
        assert!(cmd_c.modifiers.platform && !cmd_c.modifiers.control);
        assert!(ctrl_c.modifiers.control && !ctrl_c.modifiers.platform);
        assert_eq!(
            from_keystroke(platform, Context::Canvas, &cmd_c),
            Some("edit.copy")
        );
        assert_eq!(
            from_keystroke(platform, Context::Canvas, &ctrl_c),
            Some("edit.pickColor")
        );
        // ⌥⇧S: subtract. (⇧S alone is the section tool, ⇧⌘S is Save as.)
        let alt_shift_s = parse("alt-shift-s");
        assert!(
            alt_shift_s.modifiers.alt
                && alt_shift_s.modifiers.shift
                && !alt_shift_s.modifiers.platform
        );
        assert_eq!(
            from_keystroke(platform, Context::Canvas, &alt_shift_s),
            Some("bool.subtract")
        );
        // Off macOS both spellings are Control, and figcraft resolves them to one chord there.
        assert_eq!(
            from_keystroke(Platform::Other, Context::Canvas, &ctrl_c),
            Some("edit.copy")
        );
        assert_eq!(
            from_keystroke(Platform::Other, Context::Canvas, &cmd_c),
            Some("edit.copy")
        );
    }

    #[test]
    fn a_keystroke_nobody_bound_runs_nothing() {
        let f13 = Keystroke {
            modifiers: Modifiers::none(),
            key: "f13".into(),
            key_char: None,
        };
        assert_eq!(from_keystroke(Platform::Mac, Context::Canvas, &f13), None);
        assert_eq!(
            from_keystroke(Platform::Mac, Context::Canvas, &parse("q")),
            None
        );
    }

    #[test]
    fn shortcuts_carry_what_a_menu_shows() {
        let mac = shortcuts(Platform::Mac);
        let other = shortcuts(Platform::Other);
        assert_eq!(mac.len(), other.len());
        let shown = |list: &[Shortcut], id: &str| {
            list.iter().find(|s| s.id == id).map(|s| s.display.clone())
        };
        assert_eq!(shown(&mac, "edit.copy").as_deref(), Some("⌘C"));
        assert_eq!(shown(&other, "edit.copy").as_deref(), Some("Ctrl+C"));
        assert_eq!(
            display_shortcut("file.save", Platform::Mac).as_deref(),
            Some("⌘S")
        );
        assert_eq!(
            display_shortcut("file.save", Platform::Other).as_deref(),
            Some("Ctrl+S")
        );
        assert_eq!(
            display_shortcut("select.none", Platform::Mac).as_deref(),
            Some("Esc")
        );
        assert_eq!(
            display_shortcut("app.actions", Platform::Mac).as_deref(),
            Some("⌘K")
        );
        assert_eq!(
            display_shortcut("app.actions", Platform::Other).as_deref(),
            Some("Ctrl+K")
        );
        // Every entry carries both spellings of its chord.
        assert!(mac
            .iter()
            .all(|s| !s.keystroke.is_empty() && !s.display.is_empty()));
        assert!(mac.iter().any(|s| s.context == Context::Text));
        assert!(mac.iter().any(|s| s.group == Group::Essential));
        assert!(mac.iter().all(|s| s.platform == Platform::Mac));
        assert!(other.iter().all(|s| s.platform == Platform::Other));
    }

    #[test]
    fn the_run_command_action_carries_the_command() {
        let action = RunCommand::new("edit.copy", serde_json::json!({}));
        assert_eq!(action.id, "edit.copy");
        assert_eq!(action.name(), "spool::RunCommand");
        assert_eq!(RunCommand::name_for_type(), "spool::RunCommand");
        assert_eq!(action, RunCommand::new("edit.copy", serde_json::json!({})));
        assert_ne!(action, RunCommand::new("edit.cut", serde_json::json!({})));
        assert!(RunCommand::build(serde_json::json!({"id": "edit.copy"})).is_err());
    }

    #[test]
    fn groups_are_the_ones_the_panel_has() {
        assert_eq!(groups().len(), 12);
        assert_eq!(groups()[0], Group::Essential);
        assert_eq!(groups()[11], Group::Prototype);
        assert_eq!(bindings_in(Platform::Mac, Context::Text).len(), 26);
    }
}
