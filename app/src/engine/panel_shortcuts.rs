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
    "Canvas: on the canvas, not while editing text.  Text: only while editing text.  Both: in both.",
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
/// never from a list written out here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wired {
    /// The keymap installs it: a GPUI key binding dispatches the command.
    Keymap,
    /// The shell answers it: Escape is the shell's ladder, and the save is
    /// Spool's own.
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

/// The chords the shell answers rather than the keymap.
///
/// Escape is the shell's ladder — it walks down from the topmost thing the
/// user entered — and the save is Spool's own, because the engine's
/// `file.save` writes a different file. The keymap installs neither, and a
/// test says so: this list and the keymap's own table cannot drift apart
/// without a test failing.
const SHELL_OWNS: &[&str] = &["select.none", "file.save"];

/// Who answers a binding.
///
/// The keymap's own table is the authority for the chords it installs, and the
/// shell is the authority for the two it answers itself.
fn wired_of(binding: &Binding, installed: &HashSet<String>) -> Wired {
    if SHELL_OWNS.contains(&binding.id) {
        return Wired::Shell;
    }
    if binding.context == KeyContext::Text {
        // The text editing layer owns these, and Spool's does not implement
        // them yet: the keymap does not install them, and nothing else
        // answers them either.
        return Wired::NotYet;
    }
    if installed.contains(binding.id) && crate::engine::menus::available(binding.id) {
        return Wired::Keymap;
    }
    Wired::NotYet
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
        .items_center()
        .justify_between()
        .gap_2()
        .px(px(12.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .child(strip)
        .child(search_field(state, focus, cx))
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
    /// If a third chord joins the shell's, or one of these two leaves it, the
    /// sheet would be teaching a chord that nothing answers — so this fails
    /// rather than letting the two lists drift apart.
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
}
