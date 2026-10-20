//! Editor-wide commands: the one place a keystroke becomes an intent.
//!
//! Three surfaces can read a key, and they are not peers:
//!
//! | Surface | Owns | Where |
//! |---|---|---|
//! | A text buffer | characters, caret keys, its own Escape | [`crate::inspector`] for a field or a name, the engine's canvas for an object's text |
//! | A panel | its own navigation over its own rows | [`crate::engine::layers`] |
//! | This module | everything the whole editor answers to | [`crate::shell`] |
//!
//! So this file resolves keys into [`Command`]s and knows nothing about how one
//! is carried out: it is a pure function of the key, the modifiers, and
//! [`Scope`]. The shell is the only caller that executes the result, which is
//! why the answer to "which surface owns this key" has exactly one answer.
//!
//! Two rules the corpus in `docs/research/interaction/keyboard.md` settled, and
//! which the shape of this module is built around:
//!
//! 1. **A text buffer is above the editor.** Typing `f` into a layer name must
//!    not also switch to the frame tool, so [`Scope::TextEditing`] removes the
//!    commands that would move or replace objects — and the commands the text
//!    session itself owns: while a session is open, `⌘Z` undoes the text,
//!    `⌘A` selects the text and `⌘D` does nothing, because the engine's own
//!    keymap says the document's history, select-all and duplicate are canvas
//!    chords and the canvas answers them while the session is open.
//! 2. **Modifiers are context, not decoration.** `⌘D` duplicates in three
//!    products and deselects in the fourth; the arrow keys nudge a selection and
//!    pan the view with nothing selected. A keymap cannot express either, which
//!    is why this is a function with a scope argument and not a table of
//!    strings.
//!
//! Escape is deliberately *not* in the table. It does one job — leave the most
//! recent thing you entered — and it does it by walking a ladder, so it lives
//! in [`EscapeState`] instead and the shell asks that ladder first.

use gpui::Modifiers;

use crate::tools::{Tool, Traversal};

/// The command modifier: `⌘` on macOS, `Ctrl` on Windows and Linux.
///
/// Both spellings are accepted on both platforms, and that is a researched
/// choice rather than laziness. tldraw binds `'cmd+z,ctrl+z'` and documents that
/// "every combination is active on every platform"; Figma, Affinity and Canva
/// each bind only their own. Accepting both costs nothing, breaks nothing — a
/// text buffer has already claimed `⌃C` before this function is reached — and
/// means a keyboard shortcut read out of the corpus works wherever the editor
/// runs. GPUI reports the two in different fields, so this is the one place that
/// has to know about it; there is no `cfg(target_os)` anywhere in the editor.
fn command(modifiers: Modifiers) -> bool {
    modifiers.platform || modifiers.control
}

/// Whether a key was pressed with no modifier that changes what it means.
///
/// `function` is excluded on purpose. `fn` is a hardware modifier: `fn`+`←` is
/// `Home` in the system's own terms and `fn`+`V` is still the letter V, so
/// treating it as intent would make the arrow keys stop nudging on a Mac
/// keyboard and the tool letters stop switching tools.
fn plain(modifiers: Modifiers) -> bool {
    !modifiers.platform && !modifiers.control && !modifiers.alt && !modifiers.shift
}

/// An arrow key, as an intent rather than as a character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// One editor-wide command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Undo the last committed semantic operation.
    Undo,
    /// Replay the next one.
    Redo,
    /// Write the open project back to its authored source.
    Save,
    /// Delete the selection.
    Delete,
    /// Duplicate the selection in place.
    Duplicate,
    /// Select every top-level object in the document.
    SelectAll,
    /// Open a rename on the selection.
    Rename,
    /// Make a tool current.
    Tool(Tool),
    /// Move the selection by one step. `coarse` is `⇧`, ten steps.
    Nudge { direction: Direction, coarse: bool },
    /// Walk the selection through the hierarchy.
    ///
    /// Figma's traversal ladder, recorded in
    /// `docs/research/interaction/selection.md`: `Tab` / `⇧Tab` for siblings,
    /// `Enter` or `⌘↓` to descend and `⌘↑` or `⇧Enter` to ascend. One verb
    /// rather than four because the keys are one ladder, and a caller that has
    /// to reconstruct the ladder from four names will get one of them wrong.
    ///
    /// `Enter` is descend, not [`Command::Rename`]. Rename is `F2`, which is
    /// also what a layer list reaches it with: one key meaning two things on
    /// the same surface is worse than a longer descent, and every product in
    /// the corpus puts `Enter` on traversal.
    Traverse(Traversal),
    /// One zoom step in.
    ZoomIn,
    /// One zoom step out.
    ZoomOut,
    /// Frame the whole document.
    ZoomToFit,
    /// Frame the selection.
    ZoomToSelection,
    /// Return to 100%.
    ZoomToActualSize,
}

impl Command {
    /// Whether this command moves the camera rather than the document.
    ///
    /// The canvas notifies itself when it changes, so the shell would repaint on
    /// its own for the document commands. A camera command is different: the
    /// zoom readout beside the tools is the shell's, so it has to be told.
    pub fn is_camera(self) -> bool {
        matches!(
            self,
            Command::ZoomIn
                | Command::ZoomOut
                | Command::ZoomToFit
                | Command::ZoomToSelection
                | Command::ZoomToActualSize
        )
    }
}

/// What is listening for a key.
///
/// The single piece of state that changes what a keystroke means. There is no
/// other: not the tool, not the selection, not the camera. A tool changes which
/// gesture the pointer produces, and the selection changes whether an arrow
/// nudges or pans, but neither changes which *key* is a command — so neither
/// belongs in the resolver, and both are read by the shell when it carries a
/// command out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// No text buffer is open, so every editor-wide command is reachable.
    Editor,
    /// An object's text is being edited on the canvas.
    ///
    /// The text session is ABOVE the editor, and it owns history, duplicate
    /// and select-all while it is open: `⌘Z` undoes the text, `⌘A` selects
    /// the text, and `⌘D` is not a text verb at all. Those chords are the
    /// canvas's while the session is open — see
    /// [`crate::engine::canvas::EngineCanvas`] — and the editor-wide table
    /// must not answer them as well, or one chord would have two owners.
    ///
    /// The engine's own keymap is the authority for that: `edit.undo`,
    /// `edit.redo`, `select.all` and `edit.duplicate` are canvas chords, so
    /// they are not live while text is being edited.
    ///
    /// Save stays reachable, because the engine's table says `file.save` is
    /// live in both contexts: it commits the open session and writes, rather
    /// than silently doing nothing because a caret happened to be in a text
    /// box.
    ///
    /// Everything that would move or replace objects is unreachable: the caret
    /// keys and the delete keys belong to the buffer, and the tool letters are
    /// characters in it.
    TextEditing,
}

/// Resolve one keystroke into the editor-wide command it names.
///
/// Returns `None` for a key no editor-wide command claims, which is the common
/// case: most keys a user presses are either text, a panel's business, or
/// nothing at all.
pub fn resolve(key: &str, modifiers: Modifiers, scope: Scope) -> Option<Command> {
    // THE COMMAND-MODIFIER CHORDS, and which scope answers them.
    //
    // They are asked first and they never fall through: a command-modifier key
    // is either one of these or nothing, so the reader never has to know which
    // block a `⌘` key stopped in. `⌥` is excluded from all of them — `⌥⌘D` is
    // a shortcut in some other product, not this one, and quietly duplicating
    // for it would be worse than doing nothing.
    if command(modifiers) && !modifiers.alt && !modifiers.function {
        // A TEXT SESSION OWNS HISTORY, SELECT-ALL AND DUPLICATE while it is
        // open, so the editor-wide table does not answer them there: `⌘Z`
        // undoes the text, `⌘A` selects the text, and `⌘D` is not a text
        // verb. The canvas answers those chords while the session is open
        // (see `engine::canvas`), and two owners of one chord is the defect,
        // not the fix.
        //
        // SAVE IS THE EXCEPTION, and the engine's own keymap says why:
        // `file.save` is live in both contexts, so it answers here too.
        if scope == Scope::TextEditing {
            return match key {
                "s" => Some(Command::Save),
                _ => None,
            };
        }
        // THE EDITOR SCOPE answers the command modifier's chords, and it
        // answers them first: a command-modifier key is either one of these
        // or nothing, so the reader never has to know which block a `⌘` key
        // stopped in. `⌥` is excluded from all of them — `⌥⌘D` is a shortcut
        // in some other product, not this one, and quietly duplicating for it
        // would be worse than doing nothing.
        return match key {
            "z" => Some(if modifiers.shift {
                Command::Redo
            } else {
                Command::Undo
            }),
            // `⌘Y` is Canva's second redo binding. Redo is the one verb in the
            // corpus where the products genuinely disagree, so both spellings
            // answer rather than picking a side.
            "y" => Some(Command::Redo),
            "d" if !modifiers.shift => Some(Command::Duplicate),
            "s" => Some(Command::Save),
            // `⌘⇧A` is inverse selection in Figma, which Spool does not have.
            // Letting it through would quietly do something the user did not
            // ask for.
            "a" if !modifiers.shift => Some(Command::SelectAll),
            // `⌘↑` / `⌘↓` are select-parent and descend in Figma, tldraw and
            // Canva alike, and the document now has a hierarchy for them to act
            // on. No product anywhere treats `⌘`+arrow as a nudge, so taking
            // them here cannot take a nudge away from anyone.
            "up" => Some(Command::Traverse(Traversal::Ascend)),
            "down" => Some(Command::Traverse(Traversal::Descend)),
            _ => None,
        };
    }

    // Everything below is a character of something the user is in the middle of,
    // and a caret in a text box outranks all of it: `←` is a character there,
    // not a nudge, and `f` is a character rather than the frame tool.
    if scope != Scope::Editor {
        return None;
    }

    if plain(modifiers) {
        let bare_key = match key {
            // `⌫` and `⌦` are the same verb. Both spellings are bound so a
            // keyboard without a dedicated forward-delete key is not missing
            // half of it.
            "delete" | "backspace" => Some(Command::Delete),
            // `Enter` descends and `F2` renames. Figma reads `Enter` as rename
            // inside a layer list, but the panel here no longer does, so the
            // two surfaces no longer disagree about what the key means.
            "enter" => Some(Command::Traverse(Traversal::Descend)),
            "f2" => Some(Command::Rename),
            // `⇧Tab` steps back. Bound in the plain table so a forward `Tab`
            // below can own the bare key without a shifted duplicate.
            "tab" => Some(Command::Traverse(Traversal::Sibling(true))),
            "v" => Some(Command::Tool(Tool::Select)),
            "f" => Some(Command::Tool(Tool::Frame)),
            "r" => Some(Command::Tool(Tool::Rectangle)),
            "o" => Some(Command::Tool(Tool::Ellipse)),
            "t" => Some(Command::Tool(Tool::Text)),
            "p" => Some(Command::Tool(Tool::Pen)),
            "c" => Some(Command::Tool(Tool::Comment)),
            _ => None,
        };
        if bare_key.is_some() {
            return bare_key;
        }
    }

    // The camera reads a shifted character rather than a bare key, so these sit
    // outside the bare-key table: on a US layout `+` *is* `⇧`+`=`, and a
    // platform that clears the shift flag for it still has to reach zoom-in.
    // `⌥` is excluded so `⌥`+`-` stays a character on a keyboard that has one.
    if !modifiers.alt && !modifiers.function {
        match key {
            "=" | "+" => return Some(Command::ZoomIn),
            "-" | "_" => return Some(Command::ZoomOut),
            _ => {}
        }
    }

    if let Some(direction) = direction(key) {
        return Some(Command::Nudge {
            direction,
            coarse: modifiers.shift,
        });
    }

    // The shifted half of the traversal ladder, after the direction table so a
    // shifted arrow keeps meaning nudge.
    //
    // Guarded on `⇧` specifically rather than on [`plain`], which requires the
    // shift to be *absent*: this branch exists precisely to read the keys that
    // carry one.
    if modifiers.shift && !modifiers.platform && !modifiers.control && !modifiers.alt {
        let shifted = match key {
            "tab" => Some(Traversal::Sibling(false)),
            "enter" => Some(Traversal::Ascend),
            _ => None,
        };
        if let Some(step) = shifted {
            return Some(Command::Traverse(step));
        }
    }
    shifted_number(key, modifiers.shift).and_then(|digit| match digit {
        "1" => Some(Command::ZoomToFit),
        "2" => Some(Command::ZoomToSelection),
        "0" => Some(Command::ZoomToActualSize),
        _ => None,
    })
}

fn direction(key: &str) -> Option<Direction> {
    match key {
        "left" => Some(Direction::Left),
        "right" => Some(Direction::Right),
        "up" => Some(Direction::Up),
        "down" => Some(Direction::Down),
        _ => None,
    }
}

/// The US keyboard's shifted number row.
///
/// Only the three rows Spool has a camera command on are listed. Matching these
/// *positionally* rather than by symbol is tldraw's third documented matching
/// strategy, and it is the only form that works across GPUI's two conventions:
/// macOS normalises `⇧1` to the symbol `!` and clears the shift flag, while the
/// X11 path reports the digit and keeps the flag. A table that accepted only
/// `⇧`+`1` would be dead code on the platform it was written on.
const SHIFTED_NUMBER: [(&str, &str); 3] = [("1", "!"), ("2", "@"), ("0", ")")];

/// The digit the user meant on the number row, or `None` if this is not it.
///
/// The digit comes back for the unshifted symbol *and* for the digit with
/// `⇧` held, so a bare `1` stays a character rather than becoming zoom-to-fit.
fn shifted_number(key: &str, shift: bool) -> Option<&'static str> {
    SHIFTED_NUMBER
        .iter()
        .find(|(_, symbol)| *symbol == key)
        .or_else(|| shift.then(|| SHIFTED_NUMBER.iter().find(|(digit, _)| *digit == key))?)
        .map(|(digit, _)| *digit)
}

/// One rung of the Escape ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rung {
    /// An open panel: a modal is above everything.
    Panel,
    /// An Inspector text buffer, or a drag in flight inside the Inspector.
    Inspector,
    /// A canvas gesture: a move, a resize, a creation drag, a marquee, a pan.
    Gesture,
    /// An open text session on the canvas.
    TextEditing,
    /// The current selection.
    Selection,
    /// A non-Select tool, which returns to Select.
    Tool,
}

/// Everything Escape has to work through, topmost first.
///
/// The order is the finding, not the implementation. Figma, tldraw and Affinity
/// all treat Escape as "up one level" — a selection scope, a state chart, a tool
/// mode — and Canva treats it as cancel; the order below is the single reading
/// of both that the corpus supports, and it is why what Escape cancels cannot
/// depend on which listener happened to see the key first.
///
/// A rung is a *state*, not an action, so this is a value the shell reads rather
/// than a callback it registers. That is what makes the ladder testable without
/// a window: [`EscapeState::next`] is the whole policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EscapeState {
    pub panel: bool,
    pub inspector: bool,
    pub gesture: bool,
    pub text_editing: bool,
    pub selection: bool,
    pub tool: bool,
}

impl EscapeState {
    /// The topmost rung that has something to give up, or `None` when Escape
    /// has nothing left to cancel and belongs to nobody.
    pub fn next(&self) -> Option<Rung> {
        // Written as an ordered chain rather than a loop over a table so that
        // adding a rung is a visible edit to the order, not a silent one.
        if self.panel {
            Some(Rung::Panel)
        } else if self.inspector {
            Some(Rung::Inspector)
        } else if self.gesture {
            Some(Rung::Gesture)
        } else if self.text_editing {
            Some(Rung::TextEditing)
        } else if self.selection {
            Some(Rung::Selection)
        } else if self.tool {
            Some(Rung::Tool)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No modifiers at all.
    fn bare() -> Modifiers {
        Modifiers::none()
    }

    fn cmd() -> Modifiers {
        Modifiers {
            platform: true,
            ..Modifiers::none()
        }
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            control: true,
            ..Modifiers::none()
        }
    }

    fn resolve_editor(key: &str, modifiers: Modifiers) -> Option<Command> {
        resolve(key, modifiers, Scope::Editor)
    }

    fn resolve_text(key: &str, modifiers: Modifiers) -> Option<Command> {
        resolve(key, modifiers, Scope::TextEditing)
    }

    // ── history ──────────────────────────────────────────────────────────

    #[test]
    fn undo_and_redo_work_from_either_command_spelling() {
        for modifiers in [cmd(), ctrl()] {
            assert_eq!(resolve_editor("z", modifiers), Some(Command::Undo));
            assert_eq!(
                resolve_editor(
                    "z",
                    Modifiers {
                        shift: true,
                        ..modifiers
                    }
                ),
                Some(Command::Redo)
            );
            assert_eq!(
                resolve_editor("y", Modifiers { ..modifiers }),
                Some(Command::Redo)
            );
        }
    }

    #[test]
    fn redo_also_answers_the_canva_spelling() {
        // Redo is the one verb where the four products genuinely disagree:
        // `⌘⇧Z` everywhere, and Canva also takes `⌘Y`. Both answer.
        assert_eq!(
            resolve_editor(
                "y",
                Modifiers {
                    platform: true,
                    ..Modifiers::none()
                }
            ),
            Some(Command::Redo)
        );
    }

    #[test]
    fn a_bare_letter_is_never_history() {
        // `z` and `y` on their own are characters, not commands.
        assert_eq!(resolve_editor("z", bare()), None);
        assert_eq!(resolve_editor("y", bare()), None);
    }

    // ── object verbs ─────────────────────────────────────────────────────

    #[test]
    fn both_delete_spellings_are_the_same_command() {
        for key in ["delete", "backspace"] {
            assert_eq!(resolve_editor(key, bare()), Some(Command::Delete));
        }
    }

    #[test]
    fn duplicate_uses_the_command_modifier_and_nothing_else() {
        for modifiers in [cmd(), ctrl()] {
            assert_eq!(resolve_editor("d", modifiers), Some(Command::Duplicate));
        }
        assert_eq!(resolve_editor("d", bare()), None);
        // `d` is also the frame tool's neighbour on the keyboard but not its
        // letter, and it must not become one.
        assert_eq!(resolve_editor("d", Modifiers { alt: true, ..cmd() }), None);
    }

    #[test]
    fn select_all_is_reachable_and_does_not_swallow_the_inverse_selection_chord() {
        for modifiers in [cmd(), ctrl()] {
            assert_eq!(resolve_editor("a", modifiers), Some(Command::SelectAll));
        }
        // `⌘⇧A` is inverse selection in Figma. Spool has no inverse selection,
        // so the chord must resolve to nothing rather than to select-all.
        assert_eq!(
            resolve_editor(
                "a",
                Modifiers {
                    shift: true,
                    ..cmd()
                }
            ),
            None
        );
    }

    #[test]
    fn rename_is_f2_and_enter_descends() {
        // `Enter` is the traversal key in every product in the corpus, and the
        // layers panel no longer claims it as rename, so the editor-wide table
        // spends it on descend. Rename keeps one unambiguous spelling.
        assert_eq!(resolve_editor("f2", bare()), Some(Command::Rename));
        assert_eq!(
            resolve_editor("enter", bare()),
            Some(Command::Traverse(Traversal::Descend))
        );
        // `⇧Enter` was left unclaimed here as "ascend-one-level in Figma, which
        // Spool has no scope for". It now has one: hierarchy traversal reached
        // the document, so ascend is the same verb one rung down the same ladder.
        assert_eq!(
            resolve_editor(
                "enter",
                Modifiers {
                    shift: true,
                    ..Modifiers::none()
                }
            ),
            Some(Command::Traverse(Traversal::Ascend))
        );
        // `⌘↓` / `⌘↑` stay the same two rungs under their own spelling.
        for modifiers in [cmd(), ctrl()] {
            assert_eq!(
                resolve_editor("down", modifiers),
                Some(Command::Traverse(Traversal::Descend))
            );
            assert_eq!(
                resolve_editor("up", modifiers),
                Some(Command::Traverse(Traversal::Ascend))
            );
        }
    }

    #[test]
    fn save_is_reachable_from_the_keyboard_on_both_platform_conventions() {
        for modifiers in [cmd(), ctrl()] {
            assert_eq!(resolve_editor("s", modifiers), Some(Command::Save));
        }
        assert_eq!(resolve_editor("s", bare()), None);
    }

    // ── tools ────────────────────────────────────────────────────────────

    #[test]
    fn every_tool_shortcut_resolves_its_own_tool() {
        for (key, tool) in [
            ("v", Tool::Select),
            ("f", Tool::Frame),
            ("r", Tool::Rectangle),
            ("o", Tool::Ellipse),
            ("t", Tool::Text),
            ("p", Tool::Pen),
            ("c", Tool::Comment),
        ] {
            assert_eq!(resolve_editor(key, bare()), Some(Command::Tool(tool)));
        }
    }

    #[test]
    fn a_tool_letter_with_any_modifier_is_not_a_tool_letter() {
        // `⌥V` selecting the Select tool was a real defect: the guard listed
        // three of the four intent modifiers and forgot `alt`.
        for modifiers in [
            Modifiers {
                alt: true,
                ..bare()
            },
            Modifiers {
                shift: true,
                ..bare()
            },
            cmd(),
            ctrl(),
        ] {
            for key in ["v", "f", "r", "o", "t", "p", "c"] {
                assert_eq!(
                    resolve_editor(key, modifiers),
                    None,
                    "{key} with {modifiers:?} must not pick a tool"
                );
            }
        }
    }

    #[test]
    fn the_function_modifier_does_not_disarm_a_tool_letter() {
        // `fn` is hardware. `fn`+`V` types a `V`, so it still picks a tool.
        assert_eq!(
            resolve_editor(
                "v",
                Modifiers {
                    function: true,
                    ..Modifiers::none()
                }
            ),
            Some(Command::Tool(Tool::Select))
        );
    }

    // ── movement ─────────────────────────────────────────────────────────

    #[test]
    fn arrows_nudge_by_one_step_and_shift_nudges_by_ten() {
        for (key, direction) in [
            ("left", Direction::Left),
            ("right", Direction::Right),
            ("up", Direction::Up),
            ("down", Direction::Down),
        ] {
            assert_eq!(
                resolve_editor(key, bare()),
                Some(Command::Nudge {
                    direction,
                    coarse: false
                })
            );
            assert_eq!(
                resolve_editor(
                    key,
                    Modifiers {
                        shift: true,
                        ..Modifiers::none()
                    }
                ),
                Some(Command::Nudge {
                    direction,
                    coarse: true
                })
            );
        }
    }

    #[test]
    fn the_command_modifier_disarms_the_arrow_keys() {
        // `⌘↑` is select-parent in Figma, tldraw and Canva alike. It answered
        // `None` here for as long as the document had no hierarchy to change
        // scope within; it now answers [`Command::Traverse`], which is the same
        // verb the corpus documents rather than a nudge invented to fill a gap.
        // No product treats `⌘`+arrow as a nudge, and none of these keys moves
        // an object when the user asked to change scope.
        for modifiers in [cmd(), ctrl()] {
            assert_eq!(
                resolve_editor("up", modifiers),
                Some(Command::Traverse(Traversal::Ascend))
            );
            assert_eq!(
                resolve_editor("down", modifiers),
                Some(Command::Traverse(Traversal::Descend))
            );
            // The horizontal pair has no traversal meaning, so it stays disarmed
            // rather than being given one to round the set out.
            for key in ["left", "right"] {
                assert_eq!(resolve_editor(key, modifiers), None);
            }
        }
    }

    // ── camera ───────────────────────────────────────────────────────────

    #[test]
    fn zoom_step_keys_answer_in_both_spellings() {
        for modifiers in [
            bare(),
            Modifiers {
                shift: true,
                ..Modifiers::none()
            },
        ] {
            assert_eq!(resolve_editor("+", modifiers), Some(Command::ZoomIn));
            assert_eq!(resolve_editor("=", modifiers), Some(Command::ZoomIn));
            assert_eq!(resolve_editor("-", modifiers), Some(Command::ZoomOut));
        }
    }

    #[test]
    fn the_camera_chords_match_by_number_row_position() {
        // macOS reports `⇧1` as `!` with the shift flag cleared; other
        // platforms report `1` with the flag held. Both have to mean fit.
        assert_eq!(
            resolve_editor("!", Modifiers::none()),
            Some(Command::ZoomToFit)
        );
        assert_eq!(
            resolve_editor(
                "1",
                Modifiers {
                    shift: true,
                    ..Modifiers::none()
                }
            ),
            Some(Command::ZoomToFit)
        );
        assert_eq!(
            resolve_editor("@", Modifiers::none()),
            Some(Command::ZoomToSelection)
        );
        assert_eq!(
            resolve_editor(
                "2",
                Modifiers {
                    shift: true,
                    ..Modifiers::none()
                }
            ),
            Some(Command::ZoomToSelection)
        );
        assert_eq!(
            resolve_editor(")", Modifiers::none()),
            Some(Command::ZoomToActualSize)
        );
        assert_eq!(
            resolve_editor(
                "0",
                Modifiers {
                    shift: true,
                    ..Modifiers::none()
                }
            ),
            Some(Command::ZoomToActualSize)
        );
    }

    #[test]
    fn an_unshifted_digit_stays_a_character() {
        // The whole point of matching the number row positionally is that it
        // can tell `1` from `⇧1`. If a bare digit resolved to zoom-to-fit,
        // typing a number into a field would reframe the canvas.
        for key in ["1", "2", "0", "3"] {
            assert_eq!(resolve_editor(key, bare()), None, "{key} must be text");
        }
    }

    #[test]
    fn the_camera_chords_stay_away_from_the_command_modifier() {
        for key in ["!", "@", ")", "1", "2", "0"] {
            assert_eq!(resolve_editor(key, cmd()), None);
            assert_eq!(resolve_editor(key, ctrl()), None);
        }
    }

    // ── text editing outranks the editor ─────────────────────────────────

    #[test]
    fn a_text_buffer_outranks_the_commands_that_would_replace_objects() {
        // The point of `Scope`: typing `f` into a layer name must not also
        // switch to the frame tool, and typing `⌫` must not delete the object.
        assert_eq!(resolve_text("delete", bare()), None);
        assert_eq!(resolve_text("backspace", bare()), None);
        assert_eq!(resolve_text("enter", bare()), None);
        for key in ["v", "f", "r", "o", "t", "p", "c"] {
            assert_eq!(resolve_text(key, bare()), None);
        }
        assert_eq!(resolve_text("+", bare()), None);
        assert_eq!(resolve_text("!", bare()), None);
    }

    #[test]
    fn the_text_session_owns_history_duplicate_and_select_all() {
        // The engine's keymap is the authority: `edit.undo`, `edit.redo`,
        // `edit.duplicate` and `select.all` are CANVAS chords, so they are
        // not live while text is being edited — the text session is above the
        // canvas and owns them. The canvas answers them there, so the
        // editor-wide table must not answer them as well: one chord, one
        // owner.
        //
        // This is the opposite of what this table used to say, and the
        // change is deliberate: the old rule let `⌘Z` reach the document's
        // history from inside a text session, which made one chord mean two
        // things and left the text itself with no undo at all.
        for modifiers in [cmd(), ctrl()] {
            assert_eq!(resolve_text("z", modifiers), None, "⌘Z is the text's");
            assert_eq!(
                resolve_text(
                    "z",
                    Modifiers {
                        shift: true,
                        ..modifiers
                    }
                ),
                None,
                "and so is ⌘⇧Z"
            );
            assert_eq!(resolve_text("y", modifiers), None, "⌘Y is the text's");
            assert_eq!(resolve_text("d", modifiers), None, "⌘D is not a text verb");
            assert_eq!(resolve_text("a", modifiers), None, "⌘A is the text's");
            // Save is live in both contexts: the engine's own table says so.
            assert_eq!(resolve_text("s", modifiers), Some(Command::Save));
        }
    }

    #[test]
    fn the_traversal_chords_belong_to_the_text_session_too() {
        // `⌘↑` and `⌘↓` are the caret's line ends and the text's ends while a
        // session is open (the engine's own text rules), so the editor-wide
        // table must not spend them on the selection's hierarchy.
        for modifiers in [cmd(), ctrl()] {
            assert_eq!(resolve_text("up", modifiers), None);
            assert_eq!(resolve_text("down", modifiers), None);
        }
    }

    #[test]
    fn arrow_keys_are_the_caret_inside_a_text_buffer_and_a_nudge_outside_it() {
        for (key, direction) in [
            ("left", Direction::Left),
            ("right", Direction::Right),
            ("up", Direction::Up),
            ("down", Direction::Down),
        ] {
            assert_eq!(resolve_text(key, bare()), None);
            assert_eq!(
                resolve_editor(key, bare()),
                Some(Command::Nudge {
                    direction,
                    coarse: false
                })
            );
        }
    }

    // ── the escape ladder ────────────────────────────────────────────────

    #[test]
    fn escape_walks_one_rung_at_a_time_from_the_top() {
        let every = EscapeState {
            panel: true,
            inspector: true,
            gesture: true,
            text_editing: true,
            selection: true,
            tool: true,
        };
        assert_eq!(every.next(), Some(Rung::Panel));

        let mut state = every;
        state.panel = false;
        assert_eq!(state.next(), Some(Rung::Inspector));
        state.inspector = false;
        assert_eq!(state.next(), Some(Rung::Gesture));
        state.gesture = false;
        assert_eq!(state.next(), Some(Rung::TextEditing));
        state.text_editing = false;
        assert_eq!(state.next(), Some(Rung::Selection));
        state.selection = false;
        assert_eq!(state.next(), Some(Rung::Tool));
        state.tool = false;
        assert_eq!(state.next(), None);
    }

    #[test]
    fn a_buffer_outranks_the_gesture_below_it() {
        // What the user typed into is above what they are dragging, in every
        // product that documents an Escape ladder.
        let state = EscapeState {
            inspector: true,
            gesture: true,
            ..EscapeState::default()
        };
        assert_eq!(state.next(), Some(Rung::Inspector));
    }

    #[test]
    fn a_gesture_outranks_the_text_session_it_may_have_started() {
        let state = EscapeState {
            gesture: true,
            text_editing: true,
            ..EscapeState::default()
        };
        assert_eq!(state.next(), Some(Rung::Gesture));
    }

    #[test]
    fn the_selection_outranks_the_tool_and_nothing_outranks_the_panel() {
        assert_eq!(
            EscapeState {
                selection: true,
                tool: true,
                ..EscapeState::default()
            }
            .next(),
            Some(Rung::Selection)
        );
        assert_eq!(
            EscapeState {
                panel: true,
                selection: true,
                tool: true,
                ..EscapeState::default()
            }
            .next(),
            Some(Rung::Panel)
        );
        assert_eq!(EscapeState::default().next(), None);
    }

    // ── the shape of the table ───────────────────────────────────────────

    #[test]
    fn escape_is_never_resolved_as_a_command() {
        // It has to reach the ladder instead, or the ladder becomes unreachable
        // from the key path.
        for scope in [Scope::Editor, Scope::TextEditing] {
            for modifiers in [bare(), cmd(), ctrl()] {
                assert_eq!(resolve("escape", modifiers, scope), None);
            }
        }
    }

    #[test]
    fn the_space_bar_is_not_a_command_but_stays_the_pan_modifier() {
        // It is held rather than pressed, so it is deliberately absent here and
        // handled by the shell's key-down/key-up pair. Asserted so a future
        // "just put it in the table" change is caught.
        assert_eq!(resolve_editor("space", bare()), None);
    }
}
