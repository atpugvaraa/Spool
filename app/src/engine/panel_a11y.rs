//! The Accessibility section: what a screen reader reads of a layer, and the
//! keyboard's way round the window.
//!
//! # What this module owns
//!
//! Three things, in one file because they are one thing:
//!
//! 1. **The model.** [`A11yModel`] reads the engine's [`Document`] and turns the
//!    selected layers into the accessible metadata Spool can honestly show: the
//!    name a screen reader reads, the role the layer is exposed with, the
//!    description it says besides the name, and how deep the layer sits in the
//!    layer tree. [`A11yAction`] turns a change into the engine's own command.
//! 2. **The panel.** [`a11y_section`] draws that model and asks the engine for
//!    the changes the user makes.
//! 3. **The keyboard's way round the window.** [`Region`] and [`Nav`] are
//!    figcraft's F6 region navigation, ported: which region has the keyboard,
//!    and which region the keyboard goes to next.
//!
//! # The reference
//!
//! figcraft's `crates/ui-egui/src/a11y.rs` is the specification. It is the
//! accessibility surface of the whole editor, and it has two halves:
//!
//! - **The names, roles and states.** figcraft gives every control it paints
//!   itself an accessible node: a role, a label, a description, and the states
//!   that go with them. `a11y::tree_item` is the one that is about a *design
//!   node* — a layer row — and it is what this section shows:
//!
//!   | figcraft's `a11y::tree_item` | here |
//!   |---|---|
//!   | the label: the layer's name | the **Name** row |
//!   | the label: the layer's kind, when the name is empty | the **Reads as** row |
//!   | `n.set_role(Role::TreeItem)` | the **Role** row |
//!   | `n.set_description(kind, ", hidden", ", locked")` | the **Description** row |
//!   | `n.set_level(depth + 1)` | the **Level** row |
//!
//! - **The keyboard navigation.** F6 and Shift+F6 move the keyboard between the
//!   window's regions — the toolbar, the layers panel, the canvas and the
//!   properties panel — and Tab moves between the controls of a region. The
//!   engine owns the two commands (`view.nextRegion`, `view.previousRegion`)
//!   and the UI does the moving.
//!
//! # The source boundary
//!
//! **Spool's source can express an accessibility label — the layer's name.**
//! A name is an Owner B fact: the design-tool fact HTML and CSS cannot state,
//! and `lamine.yaml` v2 has the place for it (`NodeRecord::name`). The compile
//! path reads it into the engine's document (`spool_source::reader`), and the
//! save path writes it back (`app/src/engine/source_edits.rs` keeps the
//! engine's name over the manifest's). So the name here is a real, editable,
//! saveable fact.
//!
//! **The role and the description are derived, not authored.** A role follows
//! from the layer's kind, and a description follows from the kind and from what
//! is true of the layer (hidden, locked). They are Owner C facts: nothing is
//! written for them, so nothing needs a spelling for them, and they cannot
//! change without the layer changing.
//!
//! **Spool's source has no separate ARIA-style spelling, and this section does
//! not invent one.** HTML can carry `aria-label`, `role` and
//! `aria-describedby`, and `spool-source` does not read them:
//! `crates/spool-source/src/reader.rs` resolves the stylesheets and the box
//! model, and no accessibility attribute is among the properties it resolves.
//! Adding one would be a change to the source contract — a decision for a
//! human, not for a panel — so the rows here are the ones the engine holds,
//! and the note under them says so.
//!
//! # Where this differs from figcraft, and why
//!
//! - **A section, not a tree.** figcraft's accessible nodes live on the layer
//!   rows it paints. Spool's Inspector is a column of sections about the
//!   selection, so the same metadata is a section, and the layers panel keeps
//!   its rows.
//! - **Per property, not per layer.** figcraft reads a row as one value. Spool's
//!   Inspector folds the selection per property, so two layers that disagree
//!   about their kind say "Mixed" about the description and still agree about
//!   the role.
//! - **The name is edited here.** figcraft renames a layer in the layers panel.
//!   Spool's Inspector renames a layer in its header, and this section says
//!   where the name a screen reader reads comes from.
//! - **The focus mechanics are not ported.** figcraft finds the controls of a
//!   region by reading egui's widget registry; GPUI has no such registry to
//!   read, so the Tab order, the arrow keys and the focus ring stay with the
//!   shell. What is ported is the part that is the engine's and the panel's:
//!   which regions exist, which one has the keyboard, and which one the
//!   keyboard goes to next.
//!
//! # Commands, not mutations
//!
//! Every change leaves through the engine's command surface: `object.rename`
//! for the name, and `view.nextRegion` / `view.previousRegion` for the
//! keyboard. Nothing here writes to a [`Document`] directly, so an edit made
//! from this panel, from the CLI and from an agent are the same edit and land
//! in the same history.

use std::cell::RefCell;
use std::rc::Rc;

use figcraft_doc::{Document, Node, NodeId};
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Point, SharedString};
use serde_json::{json, Value};

use crate::theme;

// ═══════════════════════════════════════════════════════════════════════════
// The words
// ═══════════════════════════════════════════════════════════════════════════

/// The word for a property the selected layers disagree about.
///
/// The Inspector's own word, and figcraft's: a value and a nothing would leave
/// a panel free to invent a third thing at every row that faced a
/// disagreement.
pub const MIXED: &str = "Mixed";

/// The word for a property nobody has set.
///
/// figcraft's word, not Spool's: a property that is not there is "Not set",
/// which says that there is nothing to read rather than that the answer is
/// zero.
pub const NOT_SET: &str = "Not set";

/// The role figcraft exposes a layer row with.
///
/// `a11y::tree_item` sets `Role::TreeItem` on every layer row, so a layer is a
/// tree item to a screen reader whatever it is. The role is derived from that
/// rule and not from the layer, so it is the same for every layer and cannot be
/// changed.
pub const ROLE_TREE_ITEM: &str = "Tree item";

/// The word figcraft appends to a layer row's description when the layer is
/// hidden.
pub const HIDDEN: &str = "hidden";

/// The word figcraft appends to a layer row's description when the layer is
/// locked.
pub const LOCKED: &str = "locked";

/// What the section says when nothing is selected.
///
/// Not a blank panel: an accessibility section with nothing to say should say
/// that it has nothing to say.
pub const NOTHING_SELECTED: &str = "Select a layer to see how a screen reader reads it.";

/// Why the name cannot be changed when more than one layer is selected.
///
/// Said out loud, because a control that cannot be used must say why.
pub const ONE_LAYER_AT_A_TIME: &str = "A name is one layer's own. Select one layer to change it.";

/// The note under the rows: where the name comes from, and what the source
/// cannot carry.
///
/// This is the honest half of the section. The rows above are what the engine
/// holds; this says where they come from and what a designer cannot change
/// here.
pub const SOURCE_NOTE: &str = "The name is the layer's own, and it is saved in lamine.yaml. \
The role and the description are read from the layer, so they change when the layer does. \
Spool's source has no separate accessibility label: it has no spelling for one yet, so \
this section does not offer one.";

// ═══════════════════════════════════════════════════════════════════════════
// The three states
// ═══════════════════════════════════════════════════════════════════════════

/// One property's state across a selection.
///
/// Three states, never two: a value every selected layer agrees on, agreement
/// that nobody set it, and the absence of agreement. The Inspector's own
/// properties have the same three, and the same words, so "Mixed" means one
/// thing in the panel.
///
/// Named `A11yValue` and not `A11yState` because it holds the value when
/// there is one, and the state when there is not.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum A11yValue<T> {
    /// Every selected layer has this value, and they agree on it.
    Set(T),
    /// No selected layer has this value.
    #[default]
    Unset,
    /// The selected layers disagree.
    Mixed,
}

impl<T: PartialEq> A11yValue<T> {
    /// Fold a selection's readings into the one state that is honest for all of
    /// them.
    ///
    /// `None` for a layer means "this property is not set on that layer",
    /// which is an answer rather than missing data — so the fold is over
    /// `Option<T>` and never has to invent a default.
    pub fn across(readings: impl IntoIterator<Item = Option<T>>) -> Self {
        let mut readings = readings.into_iter();
        // An empty selection has no reading at all. `Unset` is the honest
        // answer: nothing is set, and nothing claims to be.
        let Some(first) = readings.next() else {
            return Self::Unset;
        };
        readings.fold(Self::from(first), |state, next| {
            let next = Self::from(next);
            match (state, next) {
                // A disagreement can only ever widen. Once two layers differ,
                // no later agreement can make the selection uniform again.
                (Self::Mixed, _) | (_, Self::Mixed) => Self::Mixed,
                (state, next) if state == next => state,
                _ => Self::Mixed,
            }
        })
    }

    fn from(reading: Option<T>) -> Self {
        match reading {
            Some(value) => Self::Set(value),
            None => Self::Unset,
        }
    }

    /// The value, when the selection agrees on one.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Set(value) => Some(value),
            _ => None,
        }
    }

    /// Whether the selection disagrees about this property.
    pub fn is_mixed(&self) -> bool {
        matches!(self, Self::Mixed)
    }

    /// Whether no selected layer has this property set.
    pub fn is_unset(&self) -> bool {
        matches!(self, Self::Unset)
    }

    /// The word the panel shows for a state that is not a value.
    ///
    /// Empty for a value: the value says its own name.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Set(_) => "",
            Self::Unset => NOT_SET,
            Self::Mixed => MIXED,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The model
// ═══════════════════════════════════════════════════════════════════════════

/// One layer's accessibility, as the engine holds it.
///
/// Every field here is the engine's own: the name and the kind are the
/// document's, the hidden and locked flags are the document's, and the depth
/// is the document's tree. Nothing is invented, and nothing is authored in
/// this file.
#[derive(Clone, Debug, PartialEq)]
pub struct A11yItem {
    /// The layer.
    pub id: NodeId,
    /// The layer's own name. Empty when the layer has none.
    pub name: String,
    /// The layer's kind, in the engine's own words ("Frame", "Text").
    pub kind: String,
    /// The role the layer is exposed with.
    pub role: String,
    /// What a screen reader says about the layer besides its name.
    pub description: String,
    /// How deep the layer sits in the layer tree, as AccessKit counts it: a
    /// layer on the page is 1.
    pub level: usize,
    /// The layer is hidden.
    pub hidden: bool,
    /// The layer is locked.
    pub locked: bool,
    /// The layer has children, so a screen reader could open it.
    pub has_children: bool,
}

/// What the Accessibility section shows.
///
/// The rows are folded per property, so a selection that disagrees about one
/// of them says so about that one only.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct A11yModel {
    /// The layers the model was read for.
    pub ids: Vec<NodeId>,
    /// The layer's own name. Unset when the layer has none.
    pub name: A11yValue<String>,
    /// What a screen reader reads: the name, or the kind when the name is
    /// empty.
    pub reads_as: A11yValue<String>,
    /// The role the layers are exposed with.
    pub role: A11yValue<String>,
    /// What a screen reader says besides the name.
    pub description: A11yValue<String>,
    /// How deep the layers sit in the layer tree.
    pub level: A11yValue<usize>,
    /// One entry per selected layer, in the order the selection holds them.
    pub items: Vec<A11yItem>,
}

impl A11yModel {
    /// Read the selected layers' accessibility out of the engine's document.
    ///
    /// A layer the document does not hold is skipped rather than shown as an
    /// empty one, and a selection with nothing in it reads nothing.
    pub fn read(doc: &Document, selection: &[NodeId]) -> Self {
        let nodes: Vec<&Node> = selection.iter().filter_map(|id| doc.node(*id)).collect();
        let items: Vec<A11yItem> = nodes.iter().map(|node| item_of(doc, node)).collect();

        Self {
            ids: items.iter().map(|item| item.id).collect(),
            name: A11yValue::across(
                items
                    .iter()
                    .map(|item| Some(item.name.clone()).filter(|name| !name.is_empty())),
            ),
            reads_as: A11yValue::across(items.iter().map(|item| Some(item.reads_as()))),
            role: A11yValue::across(items.iter().map(|item| Some(item.role.clone()))),
            description: A11yValue::across(items.iter().map(|item| Some(item.description.clone()))),
            level: A11yValue::across(items.iter().map(|item| Some(item.level))),
            items,
        }
    }

    /// One layer, by its id.
    pub fn item(&self, id: NodeId) -> Option<&A11yItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// The one layer the selection holds, when it holds exactly one.
    ///
    /// The name is one layer's own, so it can only be changed when the
    /// selection is one layer.
    pub fn single(&self) -> Option<&A11yItem> {
        match self.items.as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }

    /// Whether the selection is empty, so the section has nothing to say.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// One layer's accessibility, read out of the document.
fn item_of(doc: &Document, node: &Node) -> A11yItem {
    let kind = node.kind.label();
    let hidden = !node.visible;
    let locked = node.locked;
    A11yItem {
        id: node.id,
        name: node.name.clone(),
        kind: kind.to_owned(),
        role: ROLE_TREE_ITEM.to_owned(),
        description: description_of(kind, hidden, locked),
        level: level_of(doc, node.id),
        hidden,
        locked,
        has_children: !node.children.is_empty(),
    }
}

/// What a screen reader says about a layer besides its name.
///
/// figcraft's rule, word for word: the layer's kind, then ", hidden" when the
/// layer is hidden and ", locked" when it is locked.
pub fn description_of(kind: &str, hidden: bool, locked: bool) -> String {
    let mut description = kind.to_owned();
    for (on, word) in [(hidden, HIDDEN), (locked, LOCKED)] {
        if on {
            description.push_str(", ");
            description.push_str(word);
        }
    }
    description
}

/// How deep a layer sits in the layer tree, as AccessKit counts it.
///
/// figcraft's `depth + 1`: a layer on the page is 1, a layer inside a frame is
/// 2. The engine's ancestors run up to and including the page, so the page
/// itself is not counted.
pub fn level_of(doc: &Document, id: NodeId) -> usize {
    doc.ancestors(id).len()
}

impl A11yItem {
    /// What a screen reader reads for this layer.
    ///
    /// figcraft's rule, word for word: the layer's name, or its kind when the
    /// name is empty. A layer with no name is still announced — as what it is.
    pub fn reads_as(&self) -> String {
        if self.name.is_empty() {
            self.kind.clone()
        } else {
            self.name.clone()
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The change
// ═══════════════════════════════════════════════════════════════════════════

/// A change the Accessibility section can ask for.
///
/// One variant per thing the section can change, so a change the engine has no
/// command for cannot be written down as though it had one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum A11yAction {
    /// Change a layer's name, which is the name a screen reader reads.
    Rename {
        /// The layer.
        id: NodeId,
        /// Its new name.
        name: String,
    },
}

impl A11yAction {
    /// The engine's command id and parameters for this change.
    ///
    /// The engine's own spellings, so a change made from the panel is the same
    /// change the CLI, the keymap and an agent would make: `object.rename`
    /// takes `ids` (a list of node ids) and `name`.
    pub fn command(&self) -> (String, Value) {
        match self {
            Self::Rename { id, name } => (
                "object.rename".to_owned(),
                json!({ "ids": [id.0], "name": name }),
            ),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel's own state
// ═══════════════════════════════════════════════════════════════════════════

/// The field being typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum A11yField {
    /// The layer's name.
    Name,
}

/// A value being typed into a field.
///
/// A buffer, not a second name store: the name becomes real only when Enter
/// hands it to the engine as one semantic operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct A11yDraft {
    /// The field being typed into.
    pub field: A11yField,
    /// The layer being renamed.
    pub id: NodeId,
    /// The text so far.
    pub text: String,
    /// The text is selected, so the next keystroke replaces it.
    pub select_all: bool,
}

/// The panel's own state. Presentation only: never in the document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct A11yPanelState {
    /// The field being typed into, if one is.
    pub draft: Option<A11yDraft>,
}

impl A11yPanelState {
    /// The state a section starts in.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the section is part-way through something.
    pub fn is_busy(&self) -> bool {
        self.draft.is_some()
    }

    /// Give up whatever the section was part-way through.
    ///
    /// Returns whether anything was given up, so the editor's Escape ladder
    /// knows the key was consumed.
    pub fn cancel(&mut self) -> bool {
        self.draft.take().is_some()
    }

    /// Handle one keystroke.
    ///
    /// Returns the change the key asks for, if the key was the section's own.
    /// Escape is not claimed: it belongs to the editor's ladder, which walks
    /// down from the topmost thing, and a text buffer that answered it first
    /// would make the rung it belongs to depend on which listener saw the key
    /// first.
    pub fn handle_key(&mut self, key: &str) -> Option<A11yAction> {
        let draft = self.draft.as_mut()?;
        match key {
            "enter" | "return" => {
                let draft = self.draft.take()?;
                let name = draft.text.trim().to_owned();
                if name.is_empty() {
                    // An empty name is not a name. The engine would take it and
                    // the layer would lose the only thing a screen reader had.
                    return None;
                }
                Some(A11yAction::Rename { id: draft.id, name })
            }
            "backspace" => {
                if draft.select_all {
                    draft.text.clear();
                    draft.select_all = false;
                } else {
                    draft.text.pop();
                }
                None
            }
            // Arrows are not characters, and the section has nothing to nudge.
            "up" | "down" | "left" | "right" => None,
            other if other.chars().count() == 1 => {
                if draft.select_all {
                    draft.text.clear();
                    draft.select_all = false;
                }
                draft.text.push_str(other);
                None
            }
            _ => None,
        }
    }
}

/// The surface the section draws on and asks for changes.
///
/// The Inspector implements it, and so does a test's host: the section draws
/// itself and hands every change back, and nothing here knows what a window
/// is.
pub trait A11yHost: Sized + 'static {
    /// Run a change through the engine.
    fn run_a11y_action(&mut self, action: A11yAction, cx: &mut Context<Self>);
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// The Accessibility section, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own state,
/// and the surface that runs the changes. It returns an element, so the
/// Inspector can put it in a section of its own and a test can put it in a
/// window on its own.
pub fn a11y_section<T: A11yHost>(
    model: &A11yModel,
    state: &Rc<RefCell<A11yPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2().px(px(14.0)).py(px(12.0));
    if model.is_empty() {
        column = column.child(empty_state());
        return column.into_any_element();
    }
    column = column.child(name_row(model, state, cx));
    column = column.child(value_row("Reads as", &model.reads_as, "a11y-reads-as"));
    column = column.child(value_row("Role", &model.role, "a11y-role"));
    column = column.child(value_row(
        "Description",
        &model.description,
        "a11y-description",
    ));
    column = column.child(level_row(model));
    column = column.child(note(SOURCE_NOTE, "a11y-note"));
    column.into_any_element()
}

/// The empty state, word for word.
fn empty_state() -> AnyElement {
    div()
        .id(SharedString::from("a11y-empty"))
        .debug_selector(|| "a11y-empty".to_owned())
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(NOTHING_SELECTED.to_owned())
        .into_any_element()
}

/// The layer's own name, and the field that changes it.
fn name_row<T: A11yHost>(
    model: &A11yModel,
    state: &Rc<RefCell<A11yPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let draft = state.borrow().draft.clone();
    let open = draft
        .as_ref()
        .filter(|draft| draft.field == A11yField::Name)
        .cloned();
    let single = model.single().cloned();

    let value: AnyElement = match (&open, &single) {
        // A rename is open: the buffer is the truth, and the caret is in it.
        // The buffer holds the layer it belongs to, so it survives a selection
        // that changed underneath it.
        (Some(draft), _) => div()
            .id(SharedString::from("a11y-name"))
            .debug_selector(|| "a11y-name".to_owned())
            .text_sm()
            .text_color(rgb(theme::ACCENT))
            .cursor_text()
            .child(format!("{}|", draft.text))
            .into_any_element(),
        // No rename is open, and the selection is one layer: the name is a
        // field that can be opened.
        (None, Some(item)) => {
            let shared = Rc::clone(state);
            let id = item.id;
            let shown = if item.name.is_empty() {
                NOT_SET.to_owned()
            } else {
                item.name.clone()
            };
            div()
                .id(SharedString::from("a11y-name"))
                .debug_selector(|| "a11y-name".to_owned())
                .text_sm()
                .text_color(rgb(if item.name.is_empty() {
                    theme::TEXT_MUTED
                } else {
                    theme::TEXT
                }))
                .cursor_text()
                .child(shown)
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.draft = Some(A11yDraft {
                            field: A11yField::Name,
                            id,
                            text: String::new(),
                            select_all: true,
                        });
                    }
                    cx.notify();
                }))
                .into_any_element()
        }
        // More than one layer: a name is one layer's own, so the control says
        // why it cannot be used rather than being quietly dead.
        (None, None) => div()
            .id(SharedString::from("a11y-name"))
            .debug_selector(|| "a11y-name".to_owned())
            .text_sm()
            .text_color(rgb(theme::TEXT_MUTED))
            .child(ONE_LAYER_AT_A_TIME.to_owned())
            .into_any_element(),
    };

    row("Name", value)
}

/// A row whose value is text: one of the folded properties.
fn value_row(label: &str, value: &A11yValue<String>, selector: &str) -> AnyElement {
    let shown = match value {
        A11yValue::Set(set) => set.clone(),
        _ => value.word().to_owned(),
    };
    let muted = !matches!(value, A11yValue::Set(_));
    row(
        label,
        div()
            .id(SharedString::from(selector.to_owned()))
            .debug_selector(move || selector.to_owned())
            .text_sm()
            .text_color(rgb(if muted {
                theme::TEXT_MUTED
            } else {
                theme::TEXT
            }))
            .child(shown)
            .into_any_element(),
    )
}

/// The level row: how deep the layers sit in the layer tree.
fn level_row(model: &A11yModel) -> AnyElement {
    let shown = match &model.level {
        A11yValue::Set(level) => level.to_string(),
        _ => model.level.word().to_owned(),
    };
    let muted = !matches!(model.level, A11yValue::Set(_));
    row(
        "Level",
        div()
            .id(SharedString::from("a11y-level"))
            .debug_selector(|| "a11y-level".to_owned())
            .text_sm()
            .text_color(rgb(if muted {
                theme::TEXT_MUTED
            } else {
                theme::TEXT
            }))
            .child(shown)
            .into_any_element(),
    )
}

/// A row: a label on the left, the value on the right.
///
/// The row's own name comes from the label, so a test can look a row up by
/// what it is called rather than by a number.
fn row(label: &str, value: AnyElement) -> AnyElement {
    let selector = format!("a11y-row-{}", label.to_lowercase().replace(' ', "-"));
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(label.to_owned()),
        )
        .child(value)
        .into_any_element()
}

/// A note under the rows.
fn note(text: &str, selector: &str) -> AnyElement {
    let selector = selector.to_owned();
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
        .into_any_element()
}

// ═══════════════════════════════════════════════════════════════════════════
// The keyboard's way round the window
// ═══════════════════════════════════════════════════════════════════════════

/// The commands F6 and Shift+F6 are bound to.
///
/// The engine's own ids, read from the command registry rather than spelled
/// here: `view.nextRegion` and `view.previousRegion` ask the UI to move the
/// keyboard, and the document does not change.
pub const NEXT_REGION: &str = "view.nextRegion";
pub const PREVIOUS_REGION: &str = "view.previousRegion";

/// A part of the window F6 moves the keyboard to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Region {
    Toolbar,
    Layers,
    #[default]
    Canvas,
    Properties,
}

impl Region {
    /// Every region, in F6 order.
    pub const ALL: [Region; 4] = [
        Region::Toolbar,
        Region::Layers,
        Region::Canvas,
        Region::Properties,
    ];

    /// The region's own name, as the control channel spells it.
    pub fn id(self) -> &'static str {
        match self {
            Region::Toolbar => "toolbar",
            Region::Layers => "layers",
            Region::Canvas => "canvas",
            Region::Properties => "properties",
        }
    }

    /// The region a name stands for.
    ///
    /// The name is matched without regard to case, as the control channel
    /// spells it.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|r| r.id().eq_ignore_ascii_case(s))
    }

    /// The region's place in F6 order.
    pub fn index(self) -> usize {
        self as usize
    }

    /// This region's entry of a per-region array.
    fn of<T: Copy>(self, a: &[T; 4]) -> T {
        let [t, l, c, p] = a;
        match self {
            Region::Toolbar => *t,
            Region::Layers => *l,
            Region::Canvas => *c,
            Region::Properties => *p,
        }
    }
}

/// Where the keyboard goes on the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    /// By this many regions (F6, Shift+F6).
    Step(i32),
    /// To this region.
    To(Region),
}

/// Keyboard navigation state.
///
/// figcraft's `Nav`, minus the parts that need egui's widget registry: the
/// regions and where they are, the region with the keyboard, and the move the
/// keyboard has asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Nav {
    /// The region with the keyboard.
    pub region: Region,
    /// Keyboard mode: the keyboard moved the focus since the last pointer
    /// press, so the focus ring shows.
    pub keyboard: bool,
    /// Where each region is. `None` is a region that is not shown.
    pub rects: [Option<gpui::Bounds<gpui::Pixels>>; 4],
    /// The move the keyboard has asked for, not carried out yet.
    pub pending: Option<Move>,
}

impl Nav {
    /// Move the keyboard `step` regions on (`view.nextRegion` /
    /// `view.previousRegion`).
    pub fn step(&mut self, step: i32) {
        self.pending = Some(match self.pending {
            Some(Move::Step(s)) => Move::Step(s.saturating_add(step)),
            _ => Move::Step(step),
        });
    }

    /// Give `region` the keyboard.
    pub fn go(&mut self, region: Region) {
        self.pending = Some(Move::To(region));
    }

    /// Where a region is.
    pub fn rect(&self, region: Region) -> Option<gpui::Bounds<gpui::Pixels>> {
        region.of(&self.rects)
    }

    /// Whether a region is on screen.
    ///
    /// The canvas always is: it is the middle of the window, and a window
    /// without it is not a window.
    pub fn shown(&self, region: Region) -> bool {
        region == Region::Canvas || self.rect(region).is_some()
    }

    /// The region the point `p` is in.
    ///
    /// The toolbar floats over the canvas, so it is asked first. The side
    /// panels scroll, so a control scrolled below a panel's bottom still
    /// belongs to it: only the top edge is checked.
    pub fn region_at(&self, p: Point<gpui::Pixels>) -> Option<Region> {
        if self.rect(Region::Toolbar).is_some_and(|r| r.contains(&p)) {
            return Some(Region::Toolbar);
        }
        for region in [Region::Layers, Region::Properties] {
            if let Some(rect) = self.rect(region) {
                let within_x = p.x >= rect.origin.x && p.x <= rect.origin.x + rect.size.width;
                if within_x && p.y >= rect.origin.y {
                    return Some(region);
                }
            }
        }
        self.rect(Region::Canvas)
            .filter(|r| r.contains(&p))
            .map(|_| Region::Canvas)
    }
}

/// The region `step` regions on from the current one, skipping regions not
/// shown.
///
/// figcraft's rule, word for word: a full turn at most each way, and a region
/// that is not on screen is stepped over rather than landed on.
pub fn next_region(nav: &Nav, step: i32) -> Region {
    // A full turn at most each way.
    let step = step.clamp(-4, 4);
    let n = Region::ALL.len() as i32;
    let cur = nav.region.index() as i32;
    let dir = step.signum();
    let mut i = cur;
    let mut left = step.abs();
    for _ in 0..(n * step.abs().max(1)) {
        if left == 0 {
            break;
        }
        i = (i + dir).rem_euclid(n);
        if Region::ALL.get(i as usize).is_some_and(|r| nav.shown(*r)) {
            left -= 1;
        }
    }
    Region::ALL.get(i as usize).copied().unwrap_or_default()
}

/// A move the keyboard can ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavAction {
    /// Move the keyboard one region on.
    Next,
    /// Move the keyboard one region back.
    Previous,
}

impl NavAction {
    /// The engine's command id and parameters for this move.
    ///
    /// The engine's own ids, read from the command registry: both commands ask
    /// the UI to move the keyboard and change nothing in the document.
    pub fn command(&self) -> (String, Value) {
        match self {
            Self::Next => (NEXT_REGION.to_owned(), json!({})),
            Self::Previous => (PREVIOUS_REGION.to_owned(), json!({})),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use figcraft_doc::NodeKind;
    use gpui::{point, size, Entity, VisualTestContext};

    // ── The engine's own model ───────────────────────────────────────────────

    /// A document with one rectangle on it.
    fn document_with() -> (Document, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(page, NodeKind::Rectangle, "Card", 0.0, 0.0, 100.0, 100.0)
            .expect("a rectangle");
        (doc, id)
    }

    /// A document with a frame and a rectangle inside it.
    fn document_with_a_frame() -> (Document, NodeId, NodeId) {
        let (mut doc, card) = document_with();
        let page = doc.first_page().expect("a page");
        let frame = doc
            .create(page, NodeKind::Frame, "Hero", 0.0, 0.0, 200.0, 200.0)
            .expect("a frame");
        doc.reparent(card, frame, None).expect("the card moves");
        (doc, frame, card)
    }

    // ── The three states ────────────────────────────────────────────────────

    #[test]
    fn a_property_nobody_set_is_not_set_and_a_disagreement_is_mixed() {
        assert_eq!(A11yValue::<String>::across([]).word(), NOT_SET);
        assert_eq!(
            A11yValue::across([Some("a".to_owned()), Some("b".to_owned())]).word(),
            MIXED
        );
        assert_eq!(
            A11yValue::across([Some("a".to_owned()), Some("a".to_owned())]).value(),
            Some(&"a".to_owned())
        );
    }

    #[test]
    fn a_disagreement_can_only_widen() {
        // The first two agree, the third does not: the fold is Mixed and
        // stays Mixed even when a later layer agrees with the first again.
        let state = A11yValue::across([
            Some("a".to_owned()),
            Some("a".to_owned()),
            Some("b".to_owned()),
            Some("a".to_owned()),
        ]);
        assert!(state.is_mixed());
    }

    // ── The model ───────────────────────────────────────────────────────────

    #[test]
    fn the_model_reads_the_engines_accessibility_for_a_node() {
        let (doc, id) = document_with();
        let model = A11yModel::read(&doc, &[id]);

        assert_eq!(model.items.len(), 1);
        let item = model.item(id).expect("the layer");
        assert_eq!(item.name, "Card");
        assert_eq!(item.kind, "Rectangle");
        assert_eq!(item.role, ROLE_TREE_ITEM);
        assert_eq!(item.description, "Rectangle");
        assert_eq!(item.level, 1, "a layer on the page is level 1");
        assert!(!item.hidden);
        assert!(!item.locked);
    }

    #[test]
    fn a_node_with_a_label_set_shows_that_label() {
        let (doc, id) = document_with();
        let model = A11yModel::read(&doc, &[id]);
        assert_eq!(
            model.name.value(),
            Some(&"Card".to_owned()),
            "the name is the layer's own"
        );
        assert_eq!(
            model.reads_as.value(),
            Some(&"Card".to_owned()),
            "and it is what a screen reader reads"
        );
    }

    #[test]
    fn a_layer_with_no_name_is_read_as_its_kind() {
        // figcraft's rule: the label is the name, or the kind when the name is
        // empty.
        let (mut doc, id) = document_with();
        doc.get_mut(id).expect("the layer").name = String::new();
        let model = A11yModel::read(&doc, &[id]);
        assert_eq!(model.name.value(), None, "the layer has no name of its own");
        assert_eq!(model.name.word(), NOT_SET);
        assert_eq!(
            model.reads_as.value(),
            Some(&"Rectangle".to_owned()),
            "but a screen reader still reads what it is"
        );
    }

    #[test]
    fn a_hidden_or_locked_layer_says_so_in_its_description() {
        let (mut doc, id) = document_with();
        {
            let node = doc.get_mut(id).expect("the layer");
            node.visible = false;
            node.locked = true;
        }
        let model = A11yModel::read(&doc, &[id]);
        let item = model.item(id).expect("the layer");
        assert_eq!(item.description, "Rectangle, hidden, locked");
    }

    #[test]
    fn the_level_counts_the_layer_tree() {
        let (doc, frame, card) = document_with_a_frame();
        let model = A11yModel::read(&doc, &[frame, card]);
        assert_eq!(
            model.item(card).expect("the card").level,
            2,
            "a layer inside a frame is level 2"
        );
        assert_eq!(model.item(frame).expect("the frame").level, 1);
    }

    #[test]
    fn a_selection_that_disagrees_is_mixed_per_property() {
        let (mut doc, card) = document_with();
        let page = doc.first_page().expect("a page");
        let frame = doc
            .create(page, NodeKind::Frame, "Hero", 0.0, 0.0, 200.0, 200.0)
            .expect("a frame");
        let model = A11yModel::read(&doc, &[card, frame]);

        assert!(model.name.is_mixed(), "the two layers have different names");
        assert!(model.description.is_mixed(), "and different kinds");
        assert_eq!(
            model.role.value(),
            Some(&ROLE_TREE_ITEM.to_owned()),
            "but both are tree items, so the role is not a disagreement"
        );
    }

    #[test]
    fn an_empty_selection_reads_nothing() {
        let (doc, _) = document_with();
        let model = A11yModel::read(&doc, &[]);
        assert!(model.is_empty());
        assert_eq!(model.name.word(), NOT_SET);
        assert_eq!(model.reads_as.word(), NOT_SET);
    }

    #[test]
    fn a_layer_the_document_does_not_hold_is_skipped() {
        let (doc, id) = document_with();
        let model = A11yModel::read(&doc, &[id, NodeId(9999)]);
        assert_eq!(model.items.len(), 1);
    }

    #[test]
    fn the_name_can_only_be_changed_when_one_layer_is_selected() {
        let (doc, id) = document_with();
        let model = A11yModel::read(&doc, &[id]);
        assert_eq!(model.single().map(|item| item.id), Some(id));

        let (doc, _) = document_with();
        let model = A11yModel::read(&doc, &[]);
        assert!(model.single().is_none());
    }

    // ── The change ──────────────────────────────────────────────────────────

    #[test]
    fn a_rename_carries_the_engines_command_id_and_params() {
        let action = A11yAction::Rename {
            id: NodeId(7),
            name: "Headline".to_owned(),
        };
        let (command, params) = action.command();
        assert_eq!(command, "object.rename");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["name"], json!("Headline"));
    }

    #[test]
    fn the_engine_accepts_the_rename_the_panel_asks_for() {
        use figcraft_engine::Session;
        let (doc, id) = document_with();
        let mut session = Session::with_document(doc);
        let action = A11yAction::Rename {
            id,
            name: "Headline".to_owned(),
        };
        let (command, params) = action.command();
        session.execute(&command, &params).expect("the rename runs");
        assert_eq!(
            session
                .doc()
                .expect("a document")
                .node(id)
                .expect("the layer")
                .name,
            "Headline"
        );
    }

    // ── The panel's own state ───────────────────────────────────────────────

    #[test]
    fn typing_a_name_and_pressing_enter_asks_for_the_rename() {
        let mut state = A11yPanelState::new();
        state.draft = Some(A11yDraft {
            field: A11yField::Name,
            id: NodeId(3),
            text: String::new(),
            select_all: true,
        });
        assert!(state.handle_key("H").is_none());
        assert!(state.handle_key("i").is_none());
        let action = state.handle_key("enter").expect("the name is a change");
        assert_eq!(
            action,
            A11yAction::Rename {
                id: NodeId(3),
                name: "Hi".to_owned()
            }
        );
        assert!(!state.is_busy());
    }

    #[test]
    fn an_empty_name_asks_for_nothing() {
        let mut state = A11yPanelState::new();
        state.draft = Some(A11yDraft {
            field: A11yField::Name,
            id: NodeId(3),
            text: String::new(),
            select_all: true,
        });
        assert!(
            state.handle_key("enter").is_none(),
            "a layer with no name is not renamed to nothing"
        );
    }

    #[test]
    fn the_first_key_replaces_the_text_that_was_there() {
        let mut state = A11yPanelState::new();
        state.draft = Some(A11yDraft {
            field: A11yField::Name,
            id: NodeId(3),
            text: "Card".to_owned(),
            select_all: true,
        });
        state.handle_key("x");
        let action = state.handle_key("enter").expect("the name is a change");
        assert_eq!(
            action,
            A11yAction::Rename {
                id: NodeId(3),
                name: "x".to_owned()
            }
        );
    }

    #[test]
    fn escape_is_not_claimed_by_the_field() {
        let mut state = A11yPanelState::new();
        state.draft = Some(A11yDraft {
            field: A11yField::Name,
            id: NodeId(3),
            text: "Card".to_owned(),
            select_all: true,
        });
        assert!(
            state.handle_key("escape").is_none(),
            "escape belongs to the editor's ladder"
        );
        assert!(state.cancel(), "and the ladder gives the field up");
        assert!(!state.is_busy());
    }

    // ── The keyboard's way round the window ─────────────────────────────────

    #[test]
    fn the_regions_are_the_ones_figcraft_moves_the_keyboard_between() {
        assert_eq!(Region::ALL.len(), 4);
        assert_eq!(Region::Toolbar.id(), "toolbar");
        assert_eq!(Region::Layers.id(), "layers");
        assert_eq!(Region::Canvas.id(), "canvas");
        assert_eq!(Region::Properties.id(), "properties");
        assert_eq!(Region::parse("Canvas"), Some(Region::Canvas));
        assert_eq!(Region::parse("canvas"), Some(Region::Canvas));
        assert_eq!(Region::parse("nowhere"), None);
    }

    #[test]
    fn f6_moves_the_keyboard_on_and_shift_f6_moves_it_back() {
        assert_eq!(
            NavAction::Next.command(),
            ("view.nextRegion".to_owned(), json!({}))
        );
        assert_eq!(
            NavAction::Previous.command(),
            ("view.previousRegion".to_owned(), json!({}))
        );
    }

    #[test]
    fn the_keyboard_cycles_through_the_regions_that_are_shown() {
        let nav = Nav {
            rects: [
                Some(gpui::Bounds::new(
                    point(px(0.0), px(0.0)),
                    size(px(100.0), px(40.0)),
                )),
                Some(gpui::Bounds::new(
                    point(px(0.0), px(0.0)),
                    size(px(240.0), px(600.0)),
                )),
                Some(gpui::Bounds::new(
                    point(px(240.0), px(0.0)),
                    size(px(800.0), px(600.0)),
                )),
                Some(gpui::Bounds::new(
                    point(px(1040.0), px(0.0)),
                    size(px(320.0), px(600.0)),
                )),
            ],
            ..Default::default()
        };
        assert_eq!(nav.region, Region::Canvas);
        assert_eq!(next_region(&nav, 1), Region::Properties);
        assert_eq!(next_region(&nav, -1), Region::Layers);
        assert_eq!(next_region(&nav, 2), Region::Toolbar);
    }

    #[test]
    fn a_region_that_is_not_shown_is_stepped_over() {
        // The layers panel and the properties panel are not on screen.
        let mut nav = Nav {
            rects: [
                Some(gpui::Bounds::new(
                    point(px(0.0), px(0.0)),
                    size(px(100.0), px(40.0)),
                )),
                None,
                Some(gpui::Bounds::new(
                    point(px(0.0), px(0.0)),
                    size(px(800.0), px(600.0)),
                )),
                None,
            ],
            region: Region::Toolbar,
            ..Default::default()
        };
        assert_eq!(
            next_region(&nav, 1),
            Region::Canvas,
            "the layers panel is not landed on"
        );
        nav.region = Region::Canvas;
        assert_eq!(
            next_region(&nav, 1),
            Region::Toolbar,
            "and the properties panel is not either"
        );
    }

    #[test]
    fn the_point_in_the_toolbar_belongs_to_the_toolbar() {
        let nav = Nav {
            rects: [
                Some(gpui::Bounds::new(
                    point(px(0.0), px(0.0)),
                    size(px(100.0), px(40.0)),
                )),
                None,
                Some(gpui::Bounds::new(
                    point(px(0.0), px(0.0)),
                    size(px(800.0), px(600.0)),
                )),
                None,
            ],
            ..Default::default()
        };
        assert_eq!(
            nav.region_at(point(px(10.0), px(10.0))),
            Some(Region::Toolbar),
            "the toolbar floats over the canvas"
        );
        assert_eq!(
            nav.region_at(point(px(400.0), px(300.0))),
            Some(Region::Canvas)
        );
    }

    // ── The panel, in a window ───────────────────────────────────────────────

    /// A host for the section in a window: it draws the section, remembers
    /// what the section asked for, and does what it is asked.
    struct Host {
        model: A11yModel,
        state: Rc<RefCell<A11yPanelState>>,
        asked: Rc<RefCell<Vec<A11yAction>>>,
    }

    impl Host {
        fn new(model: A11yModel) -> Self {
            Self {
                model,
                state: Rc::new(RefCell::new(A11yPanelState::new())),
                asked: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl A11yHost for Host {
        fn run_a11y_action(&mut self, action: A11yAction, cx: &mut Context<Self>) {
            self.asked.borrow_mut().push(action);
            cx.notify();
        }
    }

    impl Render for Host {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            cx: &mut Context<Self>,
        ) -> impl IntoElement {
            let model = self.model.clone();
            let state = Rc::clone(&self.state);
            a11y_section(&model, &state, cx)
        }
    }

    /// Open a window with the section in it, over the given model.
    fn open_section(
        cx: &mut gpui::TestAppContext,
        model: A11yModel,
    ) -> (Entity<Host>, &mut VisualTestContext) {
        cx.add_window_view(|_, _| Host::new(model))
    }

    /// Draw the section in the window and settle.
    fn draw(root: &Entity<Host>, window: &mut VisualTestContext) {
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(320.0), px(900.0)),
            |_, _| root.clone().into_any_element(),
        );
        window.run_until_parked();
    }

    /// The bounds of an element the section draws.
    fn bounds(
        window: &mut VisualTestContext,
        selector: &'static str,
    ) -> gpui::Bounds<gpui::Pixels> {
        window
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("the section draws {selector}"))
    }

    #[gpui::test]
    fn the_section_draws_a_layers_accessibility(cx: &mut gpui::TestAppContext) {
        // The render test: the section, on its own, in a real window, showing
        // what a screen reader would read of the layer.
        let (doc, id) = document_with();
        let model = A11yModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let name = bounds(window, "a11y-name");
        assert!(
            name.size.height > px(0.0),
            "the name is on screen: {name:?}"
        );
        let reads_as = bounds(window, "a11y-reads-as");
        assert!(
            reads_as.size.height > px(0.0),
            "what it reads as is on screen: {reads_as:?}"
        );
        let role = bounds(window, "a11y-role");
        assert!(
            role.size.height > px(0.0),
            "the role is on screen: {role:?}"
        );
        let description = bounds(window, "a11y-description");
        assert!(
            description.size.height > px(0.0),
            "the description is on screen: {description:?}"
        );
        let level = bounds(window, "a11y-level");
        assert!(
            level.size.height > px(0.0),
            "the level is on screen: {level:?}"
        );
        assert!(
            window.debug_bounds("a11y-note").is_some(),
            "and the note says where the name comes from"
        );
    }

    #[gpui::test]
    fn the_section_draws_its_empty_state(cx: &mut gpui::TestAppContext) {
        let (doc, _) = document_with();
        let model = A11yModel::read(&doc, &[]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let empty = bounds(window, "a11y-empty");
        assert!(
            empty.size.height > px(0.0),
            "the empty state is on screen: {empty:?}"
        );
        assert!(
            window.debug_bounds("a11y-name").is_none(),
            "and there is no name row to say nothing about"
        );
    }
}
