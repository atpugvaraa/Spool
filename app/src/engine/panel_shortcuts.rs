//! The keyboard shortcut sheet.
//!
//! Ported from figcraft's `ui-egui/src/shortcuts.rs` and
//! `ui-egui/src/shortcuts_panel.rs`.
//!
//! # What figcraft's sheet shows
//!
//! A panel along the bottom of the window, with:
//!
//! - **a tab per group of the keymap** — Essential, Tools, View, Zoom, Text,
//!   Shape, Selection, Edit, Transform, Arrange, Components, Prototype — which
//!   is figcraft's own twelve, in its own order;
//! - **a row per shortcut**: the command's label and its keys as keycaps, at
//!   most two of them, with the alternatives that read the same shown once;
//! - **the pointer gestures and held keys** the canvas answers itself, listed
//!   under the group they belong to;
//! - **a search across every group**, so a query that matches something in
//!   another tab still finds it;
//! - **an empty state** — figcraft's words, "No shortcuts match".
//!
//! # The sheet is read from the live table, not written out
//!
//! **This is the whole point of the slice.** A hand-written list of shortcuts
//! is a list that starts lying the moment a binding changes, and a shortcut
//! sheet that lies teaches the user the wrong thing.
//!
//! So the rows are built at runtime from [`figcraft_engine::keymap::KEYS`] and
//! [`figcraft_engine::keymap::GESTURES`] — the same table figcraft's menus,
//! its canvas, its control channel and its MCP `key` tool all read, and the
//! same table Spool installs as GPUI key bindings (see
//! [`crate::engine::keymap`]). The engine's table is plain data with no egui
//! in it, which is exactly why it could be reused here.
//!
//! Three things about Spool are applied on top of it, and each of them is a
//! fact about Spool rather than about the table:
//!
//! - **Escape is the shell's ladder, not a binding.** figcraft binds Escape
//!   to `select.none`; Spool does not install that binding, because the
//!   shell's ladder walks down from the topmost thing the user entered and
//!   gives up one rung at a time. The sheet says so rather than implying that
//!   Escape always deselects.
//! - **The save is Spool's own.** figcraft's `file.save` writes the engine's
//!   own document format to a path of its own; Spool's save writes the
//!   document back to the HTML and CSS the project was authored in. The
//!   engine's chord is therefore not installed, and the sheet says which save
//!   it is showing.
//! - **The canvas answers the chords while it holds the keyboard.** A gesture
//!   is not a command at all: the canvas handles it, and it only does so
//!   while the keyboard is its own. The sheet marks those rows as gestures.
//!   The canvas's command chords are its in the same way: the shell's own
//!   table steps aside while the canvas holds the keyboard, so a canvas
//!   chord is live then and not otherwise.
//!
//! # Who answers a chord
//!
//! Every row says two things: **where the chord is live** — the canvas, only
//! while editing text, or both, which is figcraft's own table — and **who
//! answers it**: the keymap, the shell, the canvas, or nobody yet.
//!
//! The second is derived, not written out. The keymap's own function
//! ([`crate::engine::keymap::bindings`]) is the list of chords Spool
//! actually installs, and a chord it does not install, and that is not a
//! text chord, is a chord the shell answers itself. That is what makes
//! Escape and the save the shell's: the keymap deliberately leaves them
//! out, and the sheet reads that decision rather than restating it. A test
//! pins the resulting set, so a third chord joining the shell's fails a
//! test rather than quietly changing what the sheet teaches.
//!
//! # The chord spelling
//!
//! A chord is shown in **the spelling Spool's keymap installs it in**, read
//! back through the keymap's own normalisation
//! ([`crate::engine::keymap::normalized`]). GPUI's [`gpui::Keystroke::unparse`]
//! writes the platform modifier as this OS names it — `cmd-`, `super-` or
//! `win-` — while figcraft's table always writes `Cmd`. Unnormalised, the
//! sheet would read `super-s` on Linux and `win-s` on Windows, and neither
//! would compare against the table anywhere. Normalised, every chord reads
//! `cmd-s` on every platform, and the note under the sheet says that `cmd` is
//! the platform's command key: ⌘ on macOS, Ctrl on Windows and Linux.
//!
//! # The source boundary
//!
//! **There is none, and that is a real property of this slice rather than an
//! oversight.** The sheet reads the keymap table and draws it. It changes
//! nothing in the document, so there is no property to own, nothing to write
//! back to the source, and no round trip to break. A panel that edits a
//! property has to decide whether that property is authored in the HTML/CSS,
//! held in `lamine.yaml`, derived, or session state; this one has no
//! property at all.
//!
//! # Where it mounts
//!
//! It is a panel, not an Inspector section: it is about the whole editor
//! rather than about the selection, so it has a home of its own. figcraft
//! anchors it to the bottom of the window as a dialog, and so does this: the
//! shell draws it over the canvas, and the engine's own `help.shortcuts`
//! command opens it.
//!
//! # The search field and the keyboard
//!
//! The search is a text field like any other, and the keyboard has to know:
//! a character typed into it is a character and not an editor shortcut, so
//! the shortcut has to be skipped while it is open.
//!
//! **This is the shell's job, not the sheet's.** GPUI dispatches a key
//! binding *before* any `on_key_down` runs, and an action that is answered
//! stops the key there — so a letter typed into the field would run the
//! tool shortcut and never reach the field at all. The sheet cannot stop
//! that from the inside: the binding is dispatched on the window, above
//! the field.
//!
//! So the sheet says when it is searching — [`ShortcutsState::is_searching`]
//! — and the shell reads it and skips the shortcuts, exactly as it already
//! does for the Inspector's own fields. The sheet cannot answer the
//! question for itself, because only the shell knows what else has the
//! keyboard.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use figcraft_engine::keymap::{Binding, Context as KeyContext, Platform, GESTURES, KEYS};
use gpui::{
    div, prelude::*, px, rgb, AnyElement, Context, Div, DummyKeyboardMapper, FocusHandle,
    KeyBinding, SharedString, Stateful, Window,
};
use serde_json::Value;

use crate::engine::keymap::{self, RunCommand};
use crate::theme;

pub use figcraft_engine::keymap::Group;

// ═══════════════════════════════════════════════════════════════════════════
// The words
// ═══════════════════════════════════════════════════════════════════════════

/// What the sheet says when a search finds nothing.
///
/// figcraft's own words, word for word.
pub const NO_MATCHES: &str = "No shortcuts match";

/// The hint on the search field.
pub const SEARCH_SHORTCUTS: &str = "Search shortcuts";

/// The title of the sheet.
pub const TITLE: &str = "Keyboard shortcuts";

/// The notes under the sheet.
///
/// The sheet is a reference, and a reference that leaves a chord's meaning out
/// is worse than no sheet: these say what the words on the rows mean, and
/// which chords are Spool's own rather than the table's.
pub const NOTES: &[&str] = &[
    "Canvas: the canvas's own chord — the canvas answers it while it holds the keyboard, and it is not live while editing text.  Text: only while editing text.  Both: in both.",
    "\"Not wired\" means the chord is in the table and nothing in Spool answers it yet.",
    "Escape is Spool's ladder: it steps out one step at a time — panel, inspector, gesture, text, selection, tool.",
    "The save is Spool's own: it writes the document back to the HTML and CSS the project was authored in.",
    "Gestures are answered by the canvas while it holds the keyboard.",
    "The platform's command key is written `cmd`: it is ⌘ on macOS and Ctrl on Windows and Linux.",
];

// ═══════════════════════════════════════════════════════════════════════════
// The model
// ═══════════════════════════════════════════════════════════════════════════

/// Where a shortcut is live.
///
/// figcraft's table says this for every binding — the canvas, only while
/// editing text, or both — and Spool keeps the same three words. The canvas's
/// own gestures are a fourth thing: they are not commands at all, and the
/// canvas answers them itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// On the canvas: not while editing text.
    Canvas,
    /// Only while editing text.
    Text,
    /// On the canvas and while editing text.
    Both,
    /// A gesture the canvas answers itself.
    Gesture,
}

impl Scope {
    /// The word the sheet shows.
    pub fn label(self) -> &'static str {
        match self {
            Scope::Canvas => "Canvas",
            Scope::Text => "Text",
            Scope::Both => "Both",
            Scope::Gesture => "Gesture",
        }
    }
}

/// Whether Spool answers a shortcut, and who.
///
/// The answer comes from the live table and from the keymap Spool installs,
/// never from a list written out here. The one that is derived rather than
/// declared is [`Wired::Shell`]: a chord the keymap does not install, and
/// that is not a text chord, is a chord the shell answers itself — which is
/// Escape (the ladder) and the save (Spool's own), today.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wired {
    /// The keymap installs it: a GPUI key binding dispatches the command.
    Keymap,
    /// The shell answers it: Escape is the shell's ladder, and the save is
    /// Spool's own.
    ///
    /// Derived from the keymap's own table: the keymap does not install this
    /// chord, so the shell must. A test pins the exact set, so a third chord
    /// joining the shell's is a failing test rather than a silent change.
    Shell,
    /// The canvas answers it: a gesture, not a command.
    Canvas,
    /// Nothing in Spool answers it yet.
    ///
    /// The table has the chord and Spool's text editing layer does not
    /// implement it, so the sheet says so rather than teaching a chord that
    /// does nothing.
    NotYet,
}

impl Wired {
    /// The word the sheet adds to the scope, or nothing when there is none to
    /// add.
    pub fn label(self) -> &'static str {
        match self {
            Wired::Keymap | Wired::Canvas => "",
            Wired::Shell => "Shell",
            Wired::NotYet => "Not wired",
        }
    }
}

/// One row of the sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The command the row runs. Empty for a gesture.
    pub id: &'static str,
    /// The group — the tab — the row is listed under.
    pub group: Group,
    /// What the row says: the command's own label, from the table.
    pub label: String,
    /// The chords, in the spelling Spool's keymap installs them in.
    ///
    /// A gesture's row holds the gesture's own words instead, because a
    /// gesture is not a chord.
    pub keys: Vec<String>,
    /// Where the row is live.
    pub scope: Scope,
    /// Who answers it.
    pub wired: Wired,
}

impl Row {
    /// Whether the row matches a search.
    ///
    /// The label is what the sheet shows and the command id is what the
    /// engine calls it, so `edit.undo` finds the row that says "Undo".
    pub fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.label.to_lowercase().contains(&query) || self.id.to_lowercase().contains(&query)
    }

    /// The name the row is drawn under, for a test to find it by.
    pub fn selector(&self) -> String {
        if self.id.is_empty() {
            format!("shortcuts-gesture-{}", self.label.replace(' ', "-"))
        } else {
            format!("shortcuts-row-{}", self.id)
        }
    }
}

/// The sheet's model: every shortcut, read from the live keymap table.
///
/// The rows are in the table's own order, which is figcraft's order, which is
/// the order the menus are in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutsModel {
    /// The rows: the table's bindings first, then the table's gestures.
    pub rows: Vec<Row>,
}

impl ShortcutsModel {
    /// Read the model from the live keymap table.
    ///
    /// `platform` is the platform whose keys the sheet shows: figcraft's table
    /// carries the macOS keys and the Windows/Linux keys side by side, and the
    /// two differ for a few dozen bindings.
    pub fn read(platform: Platform) -> Self {
        let installed = installed_ids(platform);
        let mut rows = Vec::with_capacity(KEYS.len() + GESTURES.len());
        for binding in KEYS {
            rows.push(Row {
                id: binding.id,
                group: binding.group,
                label: binding.title().to_owned(),
                keys: keys_of(binding, platform),
                scope: scope_of(binding.context),
                wired: wired_of(binding, &installed),
            });
        }
        for (group, label, mac, other) in GESTURES {
            rows.push(Row {
                id: "",
                group: *group,
                label: (*label).to_owned(),
                keys: vec![if platform == Platform::Mac {
                    (*mac).to_owned()
                } else {
                    (*other).to_owned()
                }],
                scope: Scope::Gesture,
                wired: Wired::Canvas,
            });
        }
        Self { rows }
    }

    /// The rows a tab shows, or the rows a search finds.
    ///
    /// `group` is `None` for a search, because a search crosses every group:
    /// figcraft's own search does, and a shortcut the user cannot find
    /// because it is under another tab is a shortcut the sheet has failed to
    /// show.
    pub fn visible(&self, group: Option<Group>, query: &str) -> Vec<&Row> {
        let query = query.trim().to_lowercase();
        self.rows
            .iter()
            .filter(|row| group.is_none_or(|group| group == row.group))
            .filter(|row| query.is_empty() || row.matches(&query))
            .collect()
    }
}

/// The commands Spool installs as GPUI key bindings.
///
/// Read from [`crate::engine::keymap::bindings`] — the very function the
/// application installs — so the sheet cannot claim a chord is live when the
/// keymap does not install it.
fn installed_ids(platform: Platform) -> HashSet<String> {
    keymap::bindings(platform)
        .iter()
        .filter_map(|binding| {
            binding
                .action()
                .as_any()
                .downcast_ref::<RunCommand>()
                .map(|action| action.id.clone())
        })
        .collect()
}

/// The chords a command answers to, in the spelling Spool installs them in.
///
/// The chord is built as a GPUI key binding and then read back with the
/// keymap's own normalisation, so the sheet says `cmd-shift-k` on every
/// platform. At most [`MAX_KEYS`] are shown: the rest stay in the menus'
/// alternates, which is figcraft's own rule.
fn keys_of(binding: &Binding, platform: Platform) -> Vec<String> {
    let mut keys: Vec<String> = Vec::with_capacity(MAX_KEYS);
    for spec in binding.keys(platform) {
        for chord in keymap::gpui_keystrokes(spec) {
            let Some(spelling) = normalised(&chord, binding.id) else {
                continue;
            };
            // Alternatives that read the same show once.
            if keys.contains(&spelling) {
                continue;
            }
            keys.push(spelling);
            if keys.len() == MAX_KEYS {
                return keys;
            }
        }
    }
    keys
}

/// A chord in the spelling the keymap compares against.
///
/// The chord is loaded as a GPUI key binding — the same way the keymap loads
/// it — and read back through the keymap's normalisation. A chord this
/// platform's parser rejects is dropped rather than fatal: the table is data,
/// and an upstream key this mapping has never seen must not take the sheet
/// down.
fn normalised(chord: &str, id: &str) -> Option<String> {
    let binding = KeyBinding::load(
        chord,
        Box::new(RunCommand::new(id, Value::Object(Default::default()))),
        None,
        false,
        None,
        &DummyKeyboardMapper,
    )
    .ok()?;
    Some(keymap::normalized(&binding))
}

/// Where figcraft's table says a binding is live.
fn scope_of(context: KeyContext) -> Scope {
    match context {
        KeyContext::Canvas => Scope::Canvas,
        KeyContext::Text => Scope::Text,
        KeyContext::Both => Scope::Both,
    }
}

/// Who answers a binding.
///
/// The keymap's own table is the authority for the chords it installs, and the
/// shell is the authority for the two it answers itself. Nothing here is
/// written out by hand: a chord the keymap does not install, and that is not
/// a text chord, is a chord the shell answers.
fn wired_of(binding: &Binding, installed: &HashSet<String>) -> Wired {
    if binding.context == KeyContext::Text {
        // The text editing layer owns these, and Spool's does not implement
        // them yet: the keymap does not install them, and nothing else
        // answers them either.
        return Wired::NotYet;
    }
    if installed.contains(binding.id) {
        // The keymap installs it, so a GPUI key binding dispatches it. If
        // Spool cannot run the command at all, then the chord is dead
        // whatever the keymap says, and the sheet says that too.
        return if crate::engine::menus::available(binding.id) {
            Wired::Keymap
        } else {
            Wired::NotYet
        };
    }
    // NOT INSTALLED, AND NOT A TEXT CHORD: the shell answers it.
    //
    // Escape is the shell's ladder rather than a binding, and the save is
    // Spool's own. Both are chords the keymap deliberately does not install,
    // and both are chords the shell does answer — so "not installed" is the
    // fact that decides it, and the set that follows from that fact is
    // checked by a test rather than written out here.
    Wired::Shell
}

/// Rows a column of the fullest tab holds.
///
/// Counted from the live table, so switching tabs does not resize the sheet.
fn tallest_column() -> usize {
    Group::ALL
        .iter()
        .map(|group| {
            let count = KEYS.iter().filter(|b| b.group == *group).count()
                + GESTURES.iter().filter(|(g, ..)| g == group).count();
            count.div_ceil(COLUMNS)
        })
        .max()
        .unwrap_or(1)
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// Most keys a row shows.
///
/// figcraft's own rule: a command's first bindings, and the rest stay in the
/// menus' alternates.
pub const MAX_KEYS: usize = 2;

/// The height of a row.
pub const ROW_H: f32 = 26.0;

/// How many columns the body has.
pub const COLUMNS: usize = 3;

/// How many rows a column shows before the body scrolls.
pub const MAX_ROWS: usize = 9;

/// The gap between the columns.
pub const COL_GAP: f32 = 24.0;

/// The width of the sheet.
///
/// figcraft clamps its dialog to the window's width; this is the same width,
/// fixed, because the sheet is anchored to the window rather than to a
/// column.
pub const PANEL_W: f32 = 1040.0;

/// The width of the search field.
///
/// figcraft's own field asks for 180 points.
pub const SEARCH_W: f32 = 180.0;

/// The sheet's own state.
///
/// Presentation only, and never in the document: the sheet reads the keymap
/// table and draws it, and there is nothing here for the source to own.
pub struct ShortcutsState {
    /// The rows, read from the live keymap table.
    ///
    /// Shared rather than owned, so the panel can read the rows without
    /// holding the state borrowed while it draws them.
    model: Rc<ShortcutsModel>,
    /// The tab the sheet shows.
    ///
    /// figcraft opens on Essential.
    tab: Group,
    /// The search text.
    query: String,
    /// The keyboard focus the search field takes.
    focus: Option<FocusHandle>,
}

impl Default for ShortcutsState {
    fn default() -> Self {
        Self::new(Platform::current())
    }
}

impl ShortcutsState {
    /// A sheet for this platform.
    pub fn new(platform: Platform) -> Self {
        Self {
            model: Rc::new(ShortcutsModel::read(platform)),
            tab: Group::Essential,
            query: String::new(),
            focus: None,
        }
    }

    /// The tab the sheet shows.
    pub fn tab(&self) -> Group {
        self.tab
    }

    /// The search text the sheet is filtered by.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether the search field is in use.
    ///
    /// The search is a text field like any other: while it has the keyboard, a
    /// character typed into it is a character and not an editor shortcut, so
    /// the shortcut has to be skipped. A search with text in it counts as in
    /// use even if the field has lost the focus, because the text is still
    /// there to be edited.
    pub fn is_searching(&self, window: &Window) -> bool {
        !self.query.is_empty()
            || self
                .focus
                .as_ref()
                .is_some_and(|focus| focus.is_focused(window))
    }
}

/// The sheet, as an element.
///
/// Everything it needs is passed in: the state it owns, and the surface that
/// draws it. It returns an element, so the shell can put it over the canvas
/// and a test can put it in a window on its own.
pub fn shortcuts_panel<T: 'static>(
    state: &Rc<RefCell<ShortcutsState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let focus = focus_of(state, cx);
    let model = state.borrow().model.clone();
    let (tab, query) = {
        let state = state.borrow();
        (state.tab, state.query.clone())
    };
    // A search crosses every group; a tab shows its own.
    let searching = !query.trim().is_empty();
    let visible = model.visible(if searching { None } else { Some(tab) }, &query);

    let mut panel = div()
        .id("shortcuts-panel")
        .debug_selector(|| "shortcuts-panel".to_owned())
        .flex()
        .flex_col()
        .w(px(PANEL_W))
        .rounded_lg()
        .bg(rgb(theme::SURFACE_RAISED))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .shadow_lg();
    panel = panel.child(header(state, tab, focus.as_ref(), cx));
    panel = panel.child(body(&visible));
    panel = panel.child(legend());
    panel.into_any_element()
}

/// The keyboard focus the search field takes.
///
/// Made once, when the sheet is first drawn: a focus handle belongs to the
/// window it was made in, so a new one on every frame would lose the focus
/// the user had.
fn focus_of<T: 'static>(
    state: &Rc<RefCell<ShortcutsState>>,
    cx: &Context<T>,
) -> Option<FocusHandle> {
    let mut state = state.borrow_mut();
    if state.focus.is_none() {
        state.focus = Some(cx.focus_handle());
    }
    state.focus.clone()
}

/// The header: the tab strip, and the search field.
fn header<T: 'static>(
    state: &Rc<RefCell<ShortcutsState>>,
    tab: Group,
    focus: Option<&FocusHandle>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut strip = div()
        .id("shortcuts-tabs")
        .debug_selector(|| "shortcuts-tabs".to_owned())
        .flex()
        .items_center()
        .gap_1()
        .flex_1()
        .min_w_0()
        .overflow_x_scroll();
    for group in Group::ALL {
        strip = strip.child(tab_button(group, group == tab, state, cx));
    }
    div()
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(rgb(theme::BORDER_SOFT))
        // The title, and the search beside it.
        //
        // figcraft's dialog names itself, and a sheet that does not say what
        // it is leaves the user reading a list of chords with nothing to
        // call it.
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .px(px(12.0))
                .pt(px(10.0))
                .pb(px(8.0))
                .child(
                    div()
                        .id("shortcuts-title")
                        .debug_selector(|| "shortcuts-title".to_owned())
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(theme::TEXT))
                        .child(TITLE),
                )
                .child(search_field(state, focus, cx)),
        )
        // The tab strip, under the title: twelve tabs and a search field do
        // not share one line without one of them being squeezed.
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .px(px(12.0))
                .pb(px(8.0))
                .child(strip),
        )
        .into_any_element()
}

/// One tab of the strip.
fn tab_button<T: 'static>(
    group: Group,
    selected: bool,
    state: &Rc<RefCell<ShortcutsState>>,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    let shared = Rc::clone(state);
    div()
        .id(SharedString::from(format!("shortcuts-tab-{}", group.label())))
        .debug_selector(move || format!("shortcuts-tab-{}", group.label()))
        .px(px(7.0))
        .py(px(4.0))
        .rounded_sm()
        .text_xs()
        .text_color(rgb(if selected {
            theme::TEXT
        } else {
            theme::TEXT_SECONDARY
        }))
        .bg(rgb(if selected {
            theme::SURFACE_HOVER
        } else {
            theme::SURFACE_RAISED
        }))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            shared.borrow_mut().tab = group;
            cx.notify();
        }))
        .child(group.label().to_owned())
}

/// The search field.
///
/// The list narrows as the text is written, the way figcraft's search does, so
/// there is nothing to commit.
fn search_field<T: 'static>(
    state: &Rc<RefCell<ShortcutsState>>,
    focus: Option<&FocusHandle>,
    cx: &mut Context<T>,
) -> AnyElement {
    let query = state.borrow().query.clone();
    let empty = query.is_empty();
    let shared = Rc::clone(state);
    let mut field = div()
        .id(SharedString::from("shortcuts-search"))
        .debug_selector(|| "shortcuts-search".to_owned())
        .flex()
        .items_center()
        // A fixed width, so the field does not grow as the query does and
        // push the title across the sheet. figcraft's own field is 180 wide.
        .w(px(SEARCH_W))
        .flex_shrink_0()
        .px(px(8.0))
        .py(px(6.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .text_color(rgb(if empty {
            theme::TEXT_MUTED
        } else {
            theme::TEXT_SECONDARY
        }))
        .cursor_text()
        .overflow_hidden()
        .truncate()
        .child(if empty {
            SEARCH_SHORTCUTS.to_owned()
        } else {
            query
        });
    if let Some(focus) = focus {
        field = field.track_focus(focus);
        field = field.on_key_down(cx.listener(
            move |_host, event: &gpui::KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.as_str();
                // A chord is not a character: `⌘Z` is undo, and a search that
                // took it as text would search for "z".
                let typed = if event.keystroke.modifiers.control
                    || event.keystroke.modifiers.platform
                {
                    None
                } else {
                    // The character the key produced, when the platform says
                    // which one it was. A key that is a single character and
                    // carries none is that character: the named keys all have
                    // longer names.
                    event.keystroke.key_char.clone().or_else(|| {
                        if key.chars().count() == 1 {
                            Some(key.to_owned())
                        } else {
                            None
                        }
                    })
                };
                let mut state = shared.borrow_mut();
                let handled = match key {
                    "backspace" => {
                        state.query.pop();
                        true
                    }
                    // Escape clears the search, and only while there is one to
                    // clear: an empty search is the shell's Escape to answer.
                    "escape" if !state.query.is_empty() => {
                        state.query.clear();
                        true
                    }
                    _ => match typed {
                        Some(typed) if typed.chars().count() == 1 => {
                            state.query.push_str(&typed);
                            true
                        }
                        _ => false,
                    },
                };
                drop(state);
                if handled {
                    cx.notify();
                    cx.stop_propagation();
                }
            },
        ));
    }
    field.into_any_element()
}

/// The body: the rows, in three columns, or the empty state.
fn body(visible: &[&Row]) -> AnyElement {
    let height = px((ROW_H + 2.0) * tallest_column().min(MAX_ROWS) as f32);
    if visible.is_empty() {
        return div()
            .id("shortcuts-empty")
            .debug_selector(|| "shortcuts-empty".to_owned())
            .flex()
            .items_center()
            .px(px(14.0))
            .h(height)
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .child(NO_MATCHES)
            .into_any_element();
    }
    let per = visible.len().div_ceil(COLUMNS).max(1);
    let width = (PANEL_W - 24.0 - COL_GAP * (COLUMNS as f32 - 1.0)) / COLUMNS as f32;
    let mut columns = div()
        .id("shortcuts-body")
        .debug_selector(|| "shortcuts-body".to_owned())
        .flex()
        .flex_row()
        .gap(px(COL_GAP))
        .px(px(12.0))
        .h(height)
        .overflow_y_scroll();
    for chunk in visible.chunks(per) {
        let mut column = div().flex().flex_col().w(px(width));
        for row in chunk.iter().copied() {
            column = column.child(row_element(row));
        }
        columns = columns.child(column);
    }
    columns.into_any_element()
}

/// One row: the label, and the chords as keycaps.
fn row_element(row: &Row) -> Stateful<Div> {
    let selector = row.selector();
    let mut keys = div().flex().items_center().gap_1();
    for (position, key) in row.keys.iter().enumerate() {
        if position > 0 {
            keys = keys.child(
                div()
                    .text_size(px(9.0))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child("or"),
            );
        }
        keys = keys.child(keycap(key));
    }
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_2()
        .h(px(ROW_H))
        .child(scope_chip(row))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(rgb(theme::TEXT))
                .child(row.label.clone()),
        )
        .child(keys)
}

/// The chip that says where the row is live, and who answers it.
fn scope_chip(row: &Row) -> Div {
    let mut word = row.scope.label().to_owned();
    let owner = row.wired.label();
    if !owner.is_empty() {
        word.push_str(" · ");
        word.push_str(owner);
    }
    div()
        .flex_shrink_0()
        .px(px(5.0))
        .py(px(1.0))
        .rounded_sm()
        .bg(rgb(theme::SURFACE))
        .text_size(px(9.0))
        .text_color(rgb(if row.wired == Wired::NotYet {
            theme::TEXT_MUTED
        } else {
            theme::TEXT_SECONDARY
        }))
        .child(word)
}

/// One keycap.
fn keycap(text: &str) -> Div {
    div()
        .flex_shrink_0()
        .px(px(5.0))
        .py(px(2.0))
        .rounded_sm()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_size(px(10.0))
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(text.to_owned())
}

/// The notes under the sheet.
fn legend() -> AnyElement {
    let mut notes = div()
        .id("shortcuts-legend")
        .debug_selector(|| "shortcuts-legend".to_owned())
        .flex()
        .flex_col()
        .gap(px(2.0))
        .px(px(14.0))
        .py(px(8.0))
        .border_t_1()
        .border_color(rgb(theme::BORDER_SOFT));
    for note in NOTES {
        notes = notes.child(
            div()
                .text_size(px(9.0))
                .text_color(rgb(theme::TEXT_MUTED))
                .child((*note).to_owned()),
        );
    }
    notes.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        point, size, Entity, IntoElement, Modifiers, Render, TestAppContext, VisualTestContext,
    };

    const PLATFORMS: [Platform; 2] = [Platform::Mac, Platform::Other];

    /// The chords the shell answers itself, as the sheet should report them.
    ///
    /// Escape is the shell's ladder and the save is Spool's own. The sheet
    /// **derives** this set from the keymap's own table — a chord the keymap
    /// does not install, and that is not a text chord, is the shell's — and
    /// this is the expected result of that derivation, pinned so that a
    /// change on either side fails a test rather than quietly changing what
    /// the sheet teaches.
    const SHELL_OWNS: &[&str] = &["file.save", "select.none"];

    fn row<'a>(model: &'a ShortcutsModel, id: &str) -> &'a Row {
        model
            .rows
            .iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("the sheet should have a row for `{id}`"))
    }

    /// The sheet is the table, not a copy of it.
    ///
    /// Every binding in the live table is a row, in the table's order, with
    /// the table's own label; and so is every gesture.
    #[test]
    fn the_sheet_is_built_from_the_live_keymap_table() {
        let model = ShortcutsModel::read(Platform::Mac);
        assert_eq!(model.rows.len(), KEYS.len() + GESTURES.len());
        for (index, binding) in KEYS.iter().enumerate() {
            let row = &model.rows[index];
            assert_eq!(row.id, binding.id);
            assert_eq!(row.group, binding.group);
            assert_eq!(row.label, binding.title());
        }
        for (index, (group, label, _, _)) in GESTURES.iter().enumerate() {
            let row = &model.rows[KEYS.len() + index];
            assert_eq!(row.group, *group);
            assert_eq!(row.label, *label);
            assert_eq!(row.scope, Scope::Gesture);
            assert_eq!(row.wired, Wired::Canvas);
        }
    }

    /// A chord is shown in the spelling the keymap installs it in.
    #[test]
    fn the_chords_are_spelled_the_way_the_keymap_installs_them() {
        let model = ShortcutsModel::read(Platform::Mac);
        assert_eq!(row(&model, "edit.copy").keys, vec!["cmd-c".to_string()]);
        assert_eq!(row(&model, "file.save").keys, vec!["cmd-s".to_string()]);
        assert_eq!(
            row(&model, "text.strikethrough").keys,
            vec!["cmd-shift-x".to_string()]
        );
        assert_eq!(row(&model, "select.none").keys, vec!["escape".to_string()]);
        // The engine's `Delete` means Backspace or forward-delete, and Spool
        // installs both, so the sheet shows both.
        assert_eq!(
            row(&model, "edit.delete").keys,
            vec!["delete".to_string(), "backspace".to_string()]
        );
        // And a row never shows more than figcraft's own two.
        for row in &model.rows {
            assert!(row.keys.len() <= MAX_KEYS, "{}: {:?}", row.id, row.keys);
        }
    }

    /// The platform modifier reads as `cmd` on every platform.
    ///
    /// GPUI writes it as this OS names it, so an unnormalised chord would read
    /// `super-s` on Linux and `win-s` on Windows — and would compare against
    /// nothing.
    #[test]
    fn the_platform_modifier_is_never_written_as_this_oss_name() {
        for platform in PLATFORMS {
            for row in ShortcutsModel::read(platform).rows {
                for key in &row.keys {
                    assert!(
                        !key.contains("super-") && !key.contains("win-"),
                        "`{key}` keeps this OS's spelling of the platform modifier"
                    );
                }
            }
        }
    }

    /// The sheet says which chords are Spool's own.
    ///
    /// Escape is the shell's ladder, the save is Spool's own, and the chords
    /// the text editing layer does not implement yet say so.
    #[test]
    fn the_sheet_says_which_chords_the_shell_owns() {
        let model = ShortcutsModel::read(Platform::Mac);
        let escape = row(&model, "select.none");
        assert_eq!(escape.wired, Wired::Shell);
        assert_eq!(escape.scope, Scope::Canvas);
        let save = row(&model, "file.save");
        assert_eq!(save.wired, Wired::Shell);
        assert_eq!(save.scope, Scope::Both);
        // The text-only chords are in the table, and nothing answers them.
        for id in ["text.link", "text.listIndent", "text.listOutdent"] {
            let row = row(&model, id);
            assert_eq!(row.scope, Scope::Text, "{id}");
            assert_eq!(row.wired, Wired::NotYet, "{id}");
        }
        // And the rest are the keymap's.
        assert_eq!(row(&model, "edit.copy").wired, Wired::Keymap);
        assert_eq!(row(&model, "tool.move").wired, Wired::Keymap);
    }

    /// The shell's two chords are exactly the ones the keymap does not
    /// install.
    ///
    /// The sheet derives the set from the keymap's own table, so the two
    /// cannot drift apart. The result is pinned here: if a third chord joins
    /// the shell's, or one of these two leaves it, the sheet would be
    /// teaching a chord that nothing answers — so this fails rather than
    /// letting the two lists drift apart.
    #[test]
    fn the_shells_chords_are_exactly_the_ones_the_keymap_does_not_install() {
        for platform in PLATFORMS {
            let installed = installed_ids(platform);
            for binding in KEYS.iter().filter(|b| b.context != KeyContext::Text) {
                assert_eq!(
                    installed.contains(binding.id),
                    !SHELL_OWNS.contains(&binding.id),
                    "{platform:?}: `{}`",
                    binding.id
                );
            }
            // And the sheet says the same thing, from the same table.
            let model = ShortcutsModel::read(platform);
            let mut shell: Vec<&str> = model
                .rows
                .iter()
                .filter(|row| row.wired == Wired::Shell)
                .map(|row| row.id)
                .collect();
            shell.sort_unstable();
            assert_eq!(shell, SHELL_OWNS, "{platform:?}");
        }
    }

    /// Every row the sheet calls live names something Spool can run.
    #[test]
    fn every_live_row_names_a_command_spool_can_run() {
        for row in ShortcutsModel::read(Platform::Mac).rows {
            if row.id.is_empty() || row.wired == Wired::NotYet {
                continue;
            }
            assert!(
                crate::engine::menus::available(row.id),
                "`{}` is bound but nothing runs it",
                row.id
            );
        }
    }

    /// The live rows are the keymap's commands, exactly.
    ///
    /// Both directions, because one direction alone would let the sheet
    /// quietly stop offering a chord the keymap has, or quietly offer one
    /// the keymap does not have. The first is a sheet that understates what
    /// Spool can do; the second is a sheet that teaches a chord which does
    /// nothing.
    #[test]
    fn the_live_rows_are_the_keymaps_commands_exactly() {
        for platform in PLATFORMS {
            let installed = installed_ids(platform);
            let model = ShortcutsModel::read(platform);
            let live: HashSet<&str> = model
                .rows
                .iter()
                .filter(|row| row.wired == Wired::Keymap)
                .map(|row| row.id)
                .collect();
            for id in &installed {
                assert!(
                    live.contains(id.as_str()),
                    "{platform:?}: `{id}` is installed but the sheet does not call it live"
                );
            }
            for id in &live {
                assert!(
                    installed.contains(*id),
                    "{platform:?}: the sheet calls `{id}` live but the keymap does not install it"
                );
            }
        }
    }

    /// The tabs are figcraft's groups, and every one of them has rows.
    #[test]
    fn the_tabs_are_the_groups_figcraft_has() {
        assert_eq!(Group::ALL.len(), 12);
        assert_eq!(Group::ALL[0], Group::Essential);
        assert_eq!(Group::ALL[11], Group::Prototype);
        let model = ShortcutsModel::read(Platform::Mac);
        for group in Group::ALL {
            assert!(
                !model.visible(Some(group), "").is_empty(),
                "{group:?} has no rows"
            );
        }
    }

    /// A tab shows its own group; a search crosses every group.
    #[test]
    fn the_search_narrows_the_rows_and_crosses_every_group() {
        let model = ShortcutsModel::read(Platform::Mac);
        assert_eq!(model.visible(None, "").len(), model.rows.len());
        let tools = model.visible(Some(Group::Tools), "");
        assert!(!tools.is_empty());
        assert!(tools.iter().all(|row| row.group == Group::Tools));
        // A search finds a row by its label...
        assert!(model
            .visible(None, "undo")
            .iter()
            .any(|row| row.id == "edit.undo"));
        // ...and by its command id.
        assert!(model
            .visible(None, "edit.undo")
            .iter()
            .any(|row| row.id == "edit.undo"));
        // A search crosses every group, so a row under another tab is found.
        let toggles = model.visible(None, "toggle");
        let groups: HashSet<Group> = toggles.iter().map(|row| row.group).collect();
        assert!(groups.len() > 1, "a search should cross the groups: {groups:?}");
        // And a search that matches nothing finds nothing.
        assert!(model.visible(None, "no such shortcut").is_empty());
    }

    /// A row says where the table says the chord is live.
    ///
    /// The scope is figcraft's own context, read from the table rather than
    /// written out here: the canvas, only while editing text, or both.
    #[test]
    fn a_row_says_where_the_table_says_it_is_live() {
        for platform in PLATFORMS {
            let model = ShortcutsModel::read(platform);
            for (index, binding) in KEYS.iter().enumerate() {
                let expected = match binding.context {
                    KeyContext::Canvas => Scope::Canvas,
                    KeyContext::Text => Scope::Text,
                    KeyContext::Both => Scope::Both,
                };
                assert_eq!(
                    model.rows[index].scope,
                    expected,
                    "{}: {platform:?}",
                    binding.id
                );
            }
        }
    }

    /// The chords on a row are the table's own chords.
    ///
    /// Read back out of the same place the keymap reads them, and spelled
    /// the way the keymap spells them: a row cannot show a chord the table
    /// does not have, and it cannot hide one it does.
    ///
    /// By position, not by id: the table binds `view.assets` twice — once
    /// under View and once under Components, with different chords — and the
    /// sheet has a row for each, the way figcraft's own panel does.
    #[test]
    fn the_chords_on_a_row_are_the_table_s_own() {
        for platform in PLATFORMS {
            let model = ShortcutsModel::read(platform);
            for (index, binding) in KEYS.iter().enumerate() {
                // Every chord the table's specs stand for, in the table's
                // order, with the alternatives that read the same collapsed.
                let mut expected: Vec<String> = Vec::new();
                for spec in binding.keys(platform) {
                    for chord in keymap::gpui_keystrokes(spec) {
                        let Some(spelling) = normalised(&chord, binding.id) else {
                            continue;
                        };
                        if !expected.contains(&spelling) {
                            expected.push(spelling);
                        }
                    }
                }
                let shown = &model.rows[index].keys;
                let wanted = &expected[..expected.len().min(MAX_KEYS)];
                assert_eq!(shown, wanted, "{}: {platform:?}", binding.id);
            }
        }
    }

    /// Every chord the sheet calls live is one the keymap installed.
    ///
    /// The sheet says a chord is live; the keymap is what makes it live. If
    /// the two disagree then the sheet is teaching a chord that does
    /// nothing — so this reads the keymap's own installed chords and
    /// compares, rather than trusting the sheet's own word for it.
    #[test]
    fn every_chord_the_sheet_calls_live_is_one_the_keymap_installed() {
        for platform in PLATFORMS {
            let bindings = keymap::bindings(platform);
            let installed: HashSet<String> =
                bindings.iter().map(keymap::normalized).collect();
            let model = ShortcutsModel::read(platform);
            for row in &model.rows {
                if row.wired != Wired::Keymap {
                    continue;
                }
                for key in &row.keys {
                    assert!(
                        installed.contains(key),
                        "{platform:?}: `{}` shows `{key}`, which the keymap does not install",
                        row.id
                    );
                }
            }
        }
    }

    /// The gestures are in the sheet, under the group the table gives them.
    ///
    /// A gesture is not a command: the canvas answers it itself. They are
    /// listed after the bindings, in the table's order, and a search finds
    /// them by their own words.
    #[test]
    fn the_gestures_are_in_the_sheet_under_their_own_group() {
        for platform in PLATFORMS {
            let model = ShortcutsModel::read(platform);
            assert!(
                model
                    .rows
                    .iter()
                    .take(KEYS.len())
                    .all(|row| !row.id.is_empty()),
                "the bindings come first"
            );
            let gestures: Vec<&Row> = model
                .rows
                .iter()
                .filter(|row| row.id.is_empty())
                .collect();
            assert_eq!(gestures.len(), GESTURES.len());
            for (index, (group, label, mac, other)) in GESTURES.iter().enumerate() {
                let row = &model.rows[KEYS.len() + index];
                assert_eq!(row.group, *group);
                assert_eq!(row.label, *label);
                assert_eq!(row.scope, Scope::Gesture);
                assert_eq!(row.wired, Wired::Canvas);
                let expected = if platform == Platform::Mac { mac } else { other };
                assert_eq!(row.keys, vec![(*expected).to_string()]);
            }
            // And a search finds a gesture by its words.
            assert!(model
                .visible(None, "pan the canvas")
                .iter()
                .any(|row| row.id.is_empty()));
        }
    }

    /// A row never shows the same chord twice.
    ///
    /// The alternatives that read the same show once — figcraft's own rule —
    /// and a row shows at most figcraft's two.
    #[test]
    fn a_row_never_shows_the_same_chord_twice() {
        for platform in PLATFORMS {
            for row in ShortcutsModel::read(platform).rows {
                let mut seen = HashSet::new();
                for key in &row.keys {
                    assert!(seen.insert(key.clone()), "{}: `{key}` twice", row.id);
                }
                assert!(row.keys.len() <= MAX_KEYS, "{}: {:?}", row.id, row.keys);
            }
        }
    }

    // ── In a real window ─────────────────────────────────────────────────────

    /// The sheet on its own, in a window.
    struct Host {
        state: Rc<RefCell<ShortcutsState>>,
    }

    impl Render for Host {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            shortcuts_panel(&self.state, cx)
        }
    }

    /// A sheet in a real window.
    fn open_sheet(
        cx: &mut TestAppContext,
        platform: Platform,
    ) -> (Entity<Host>, &mut VisualTestContext) {
        cx.add_window_view(|_, _| Host {
            state: Rc::new(RefCell::new(ShortcutsState::new(platform))),
        })
    }

    /// Draw the sheet in the window and settle.
    fn draw(host: &Entity<Host>, window: &mut VisualTestContext) {
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(1200.0), px(700.0)),
            |_, _| host.clone().into_any_element(),
        );
        window.run_until_parked();
    }

    /// The bounds of an element the sheet draws.
    fn bounds(
        window: &mut VisualTestContext,
        selector: &'static str,
    ) -> gpui::Bounds<gpui::Pixels> {
        window
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("the sheet draws {selector}"))
    }

    /// Type into the search field, the way a user does.
    fn type_text(window: &mut VisualTestContext, text: &str) {
        for character in text.chars() {
            let keystroke = gpui::Keystroke::parse(&character.to_string()).expect("a keystroke");
            window.update(|window, cx| window.dispatch_keystroke(keystroke, cx));
        }
        window.run_until_parked();
    }

    /// The sheet draws itself: the tabs, the search field, the rows and the
    /// notes that say what the words mean.
    #[gpui::test]
    fn the_sheet_draws_the_shortcuts(cx: &mut TestAppContext) {
        let (host, window) = open_sheet(cx, Platform::Mac);
        draw(&host, window);

        let panel = bounds(window, "shortcuts-panel");
        assert!(
            panel.size.height > px(0.0),
            "the sheet is on screen: {panel:?}"
        );
        // It says what it is. The width matters as much as the height: an
        // empty title would still be a line tall.
        let title = bounds(window, "shortcuts-title");
        assert!(
            title.size.height > px(0.0) && title.size.width > px(0.0),
            "the sheet names itself: {title:?}"
        );
        let tabs = bounds(window, "shortcuts-tabs");
        assert!(
            tabs.size.height > px(0.0),
            "the tab strip is on screen: {tabs:?}"
        );
        assert!(
            window.debug_bounds("shortcuts-tab-Essential").is_some(),
            "the strip has figcraft's tabs"
        );
        assert!(window.debug_bounds("shortcuts-tab-Prototype").is_some());
        let search = bounds(window, "shortcuts-search");
        assert!(
            search.size.height > px(0.0),
            "the search field is on screen: {search:?}"
        );
        // The Essential tab's own rows are drawn.
        let actions = bounds(window, "shortcuts-row-app.actions");
        assert!(
            actions.size.height > px(0.0),
            "a shortcut is drawn: {actions:?}"
        );
        // And the notes under the sheet are there.
        assert!(window.debug_bounds("shortcuts-legend").is_some());
    }

    /// A tab shows its own group, and a search crosses every group.
    #[gpui::test]
    fn the_search_narrows_what_the_sheet_draws(cx: &mut TestAppContext) {
        let (host, window) = open_sheet(cx, Platform::Mac);
        draw(&host, window);

        // The Essential tab, so a row of another group is not on screen.
        assert!(window.debug_bounds("shortcuts-row-app.actions").is_some());
        assert!(window.debug_bounds("shortcuts-row-edit.copy").is_none());

        let search = bounds(window, "shortcuts-search");
        window.simulate_click(search.center(), Modifiers::none());
        window.run_until_parked();
        type_text(window, "copy");
        draw(&host, window);

        assert!(
            window.debug_bounds("shortcuts-row-edit.copy").is_some(),
            "the row that matches is on screen, from a group the tab did not show"
        );
        assert!(
            window.debug_bounds("shortcuts-row-app.actions").is_none(),
            "and the row that does not match is gone"
        );
    }

    /// A search that finds nothing says so, in figcraft's words.
    #[gpui::test]
    fn the_sheet_draws_its_empty_state(cx: &mut TestAppContext) {
        let (host, window) = open_sheet(cx, Platform::Mac);
        draw(&host, window);

        let search = bounds(window, "shortcuts-search");
        window.simulate_click(search.center(), Modifiers::none());
        window.run_until_parked();
        type_text(window, "nosuchshortcut");
        draw(&host, window);

        let empty = bounds(window, "shortcuts-empty");
        assert!(
            empty.size.height > px(0.0),
            "the empty state is on screen: {empty:?}"
        );
        assert!(
            window.debug_bounds("shortcuts-body").is_none(),
            "and there are no rows to draw"
        );
    }

    /// Clicking a tab shows that group's rows.
    ///
    /// The tabs are the table's groups, and a tab that did not change what is
    /// on screen would be a picture of a tab rather than a tab.
    #[gpui::test]
    fn clicking_a_tab_shows_that_groups_rows(cx: &mut TestAppContext) {
        let (host, window) = open_sheet(cx, Platform::Mac);
        draw(&host, window);

        // The Essential tab is what figcraft opens on.
        assert!(window.debug_bounds("shortcuts-row-app.actions").is_some());

        let tools = bounds(window, "shortcuts-tab-Tools");
        window.simulate_click(tools.center(), Modifiers::none());
        window.run_until_parked();
        draw(&host, window);

        assert!(
            window.debug_bounds("shortcuts-row-tool.move").is_some(),
            "the Tools tab shows its own rows"
        );
        assert!(
            window.debug_bounds("shortcuts-row-app.actions").is_none(),
            "and not the tab that was showing"
        );
    }

    /// The sheet says when its search field has the keyboard.
    ///
    /// While it does, a character typed into it is a character and not an
    /// editor shortcut, so the shortcut has to be skipped — and only the
    /// sheet can say that, because only the sheet knows where its own field
    /// is. This is the hook the shell reads.
    #[gpui::test]
    fn the_sheet_says_when_the_search_field_has_the_keyboard(cx: &mut TestAppContext) {
        let (host, window) = open_sheet(cx, Platform::Mac);
        draw(&host, window);

        assert!(!is_searching(&host, window), "the search starts unused");

        let search = bounds(window, "shortcuts-search");
        window.simulate_click(search.center(), Modifiers::none());
        window.run_until_parked();
        draw(&host, window);

        assert!(
            is_searching(&host, window),
            "the field has the keyboard, so the shell has to skip the shortcuts"
        );

        // And a query in it counts as in use even once the field has lost the
        // focus: the text is still there to be edited.
        type_text(window, "copy");
        let title = bounds(window, "shortcuts-title");
        window.simulate_click(title.center(), Modifiers::none());
        window.run_until_parked();
        draw(&host, window);

        assert!(
            is_searching(&host, window),
            "a query in the field is still a search in progress"
        );
    }

    /// Whether the sheet's search field has the keyboard.
    fn is_searching(host: &Entity<Host>, window: &mut VisualTestContext) -> bool {
        window.update(|window, cx| {
            host.read_with(cx, |host, _| host.state.borrow().is_searching(window))
        })
    }
}
