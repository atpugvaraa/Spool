//! The Styles section: figcraft's style browser, ported to GPUI.
//!
//! # What this module owns
//!
//! Two things, in one file because they are one thing:
//!
//! 1. **The model.** [`StylesModel`] reads the engine's [`Document`] and turns
//!    the selected layers' style links, the document's styles and the
//!    document's colour variables into what the panel shows.
//!    [`StylesAction`] turns a change into the engine's own command.
//! 2. **The panel.** [`styles_section`] draws that model and asks the engine
//!    for the changes the user makes.
//!
//! # The reference
//!
//! figcraft's style browser — `crates/ui-egui/src/style_picker.rs` — is the
//! specification. Its rows, its order, its labels, its actions, its empty
//! state and its enable/disable behaviour are mirrored here. The engine's own
//! model is the arbiter: [`figcraft_doc::StyleKind`] says what a style is, and
//! the engine's commands say what a change is.
//!
//! # Where this differs from figcraft, and why
//!
//! - **The chip is always shown.** figcraft draws the chip only when a style
//!   is applied; Spool's Inspector shows three states for every property, so an
//!   unset property says "Not set" rather than disappearing.
//! - **One section, not five popovers.** figcraft opens the browser as a
//!   popover from the Fill, Stroke, Effects, Layout and Typography section
//!   headers. Spool's Inspector has no surface that owns a popover, so the
//!   five chips and the browser live in one section.
//! - **The edit button is always shown.** figcraft reveals it on hover; a
//!   hover-only control cannot be reached without a pointer.
//! - **A typed field commits on Enter.** figcraft commits the typed name and
//!   description when the browser closes; Spool's Inspector commits every
//!   typed field on Enter and abandons it on Escape, like every other field.
//!
//! # The save path
//!
//! The metadata can carry a style — `lamine.yaml` has a `styles:` list and
//! every node record has a `styles:` of its own — and the engine reads both.
//! The save path does not write them back yet, so the section says so where
//! the change is made rather than leaving the user to find out by saving.
//!
//! # Commands, not mutations
//!
//! Every change leaves through the engine's command surface: `style.create`,
//! `style.apply`, `style.detach`, `style.update`, `style.bind`,
//! `style.delete` and `variable.bind`. Nothing here writes to a [`Document`]
//! directly, so an edit made from this panel, from the CLI and from an agent
//! are the same edit and land in the same history.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use figcraft_doc::color::{Color, Paint, PaintKind};
use figcraft_doc::{Document, Node, NodeId, StyleKind, TextStyle, VariableType, VariableValue};
use gpui::{div, prelude::*, px, rgb, rgba, AnyElement, Context, Div, SharedString};
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

/// The word for a style the document no longer holds.
///
/// figcraft's word: a link to a style that is gone is not "Not set" — the
/// layer does link to something, and that something cannot be found.
pub const MISSING_STYLE: &str = "Missing style";

/// What the save path does with a shared style today.
///
/// The metadata can carry a style — `lamine.yaml` has a `styles:` list and
/// every node record has a `styles:` of its own — and the engine reads both.
/// The save path does not write them back yet: it writes the editor's own
/// version 1 metadata, and it leaves a version 2 file alone rather than
/// re-encoding it. So a style made or edited here stays in the editor's
/// document. Said where the change is made, rather than left for the user to
/// find out by saving.
const SAVE_PATH_NOTE: &str =
    "Shared styles are not written back to lamine.yaml yet, so a style made or edited here \
     stays in the editor's document.";

// ═══════════════════════════════════════════════════════════════════════════
// The slot
// ═══════════════════════════════════════════════════════════════════════════

/// A style slot on a layer: what the browser lists and what a chip shows.
///
/// The engine's five, in the engine's order: a layer links to one style per
/// slot, and the slot's name is the one `style.detach` and `style.apply` use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StyleSlot {
    /// The layer's fill.
    Fill,
    /// The layer's stroke.
    Stroke,
    /// The layer's effects.
    Effect,
    /// The layer's layout grids.
    Grid,
    /// The layer's text.
    Text,
}

impl StyleSlot {
    /// Every slot, in the order the panel draws them.
    pub const ALL: [StyleSlot; 5] = [
        StyleSlot::Fill,
        StyleSlot::Stroke,
        StyleSlot::Effect,
        StyleSlot::Grid,
        StyleSlot::Text,
    ];

    /// The slot's name, as the engine's command parameter spells it.
    ///
    /// The same word `style.apply {target}`, `style.detach {which}` and
    /// `variable.bind {field}` use.
    pub fn key(self) -> &'static str {
        match self {
            StyleSlot::Fill => "fill",
            StyleSlot::Stroke => "stroke",
            StyleSlot::Effect => "effect",
            StyleSlot::Grid => "grid",
            StyleSlot::Text => "text",
        }
    }

    /// The slot's title, as the browser's header says it.
    pub fn title(self) -> &'static str {
        match self {
            StyleSlot::Fill | StyleSlot::Stroke => "Color styles",
            StyleSlot::Effect => "Effect styles",
            StyleSlot::Grid => "Grid styles",
            StyleSlot::Text => "Text styles",
        }
    }

    /// The slot's row label, as the panel's chip shows it.
    pub fn label(self) -> &'static str {
        match self {
            StyleSlot::Fill => "Fill",
            StyleSlot::Stroke => "Stroke",
            StyleSlot::Effect => "Effects",
            StyleSlot::Grid => "Grid",
            StyleSlot::Text => "Text",
        }
    }

    /// The type `style.create {type}` takes for this slot.
    ///
    /// The engine's own spelling: a colour style is a "paint", a stroke style
    /// is a "stroke" — the engine takes the layer's strokes for it — and the
    /// rest are named after what they hold.
    pub fn create_type(self) -> &'static str {
        match self {
            StyleSlot::Fill => "paint",
            StyleSlot::Stroke => "stroke",
            StyleSlot::Effect => "effect",
            StyleSlot::Grid => "grid",
            StyleSlot::Text => "text",
        }
    }

    /// The base of a new style's name ("Color 3").
    pub fn default_name(self) -> &'static str {
        match self {
            StyleSlot::Fill | StyleSlot::Stroke => "Color",
            StyleSlot::Effect => "Effect",
            StyleSlot::Grid => "Grid",
            StyleSlot::Text => "Text",
        }
    }

    /// Whether the slot holds styles of this kind.
    ///
    /// The engine's own kinds: a colour slot holds paint styles, the effect
    /// slot holds effect styles, and so on.
    pub fn holds(self, kind: &StyleKind) -> bool {
        matches!(
            (kind, self),
            (StyleKind::Paint { .. }, StyleSlot::Fill | StyleSlot::Stroke)
                | (StyleKind::Effect { .. }, StyleSlot::Effect)
                | (StyleKind::Grid { .. }, StyleSlot::Grid)
                | (StyleKind::Text { .. }, StyleSlot::Text)
        )
    }

    /// Whether the slot's styles are paints — the ones that carry a colour.
    pub fn paints(self) -> bool {
        matches!(self, StyleSlot::Fill | StyleSlot::Stroke)
    }

    /// Whether the slot's styles also show as a grid (swatches, "Ag" tiles).
    pub fn has_grid(self) -> bool {
        self.paints() || self == StyleSlot::Text
    }

    /// The style the layer links to in this slot, if it links to one.
    pub fn linked(self, node: &Node) -> Option<&str> {
        match self {
            StyleSlot::Fill => node.styles.fill.as_deref(),
            StyleSlot::Stroke => node.styles.stroke.as_deref(),
            StyleSlot::Effect => node.styles.effect.as_deref(),
            StyleSlot::Grid => node.styles.grid.as_deref(),
            StyleSlot::Text => node.styles.text.as_deref(),
        }
    }

    /// The slot's link on one layer.
    ///
    /// A text layer's link is a fact about its *characters*, not about the
    /// layer: the engine's own `text_style_link` says which style every
    /// character is linked to, and that the characters may disagree. So a
    /// text layer whose characters link to different styles is a layer that
    /// disagrees with itself, and that is a state of its own.
    fn reading(self, node: &Node) -> SlotReading {
        match self {
            StyleSlot::Text => match node.kind.text().and_then(|text| text.text_style_link()) {
                Some(Some(id)) => SlotReading::Style(id.to_owned()),
                Some(None) => SlotReading::None,
                // The characters disagree.
                None => SlotReading::Mixed,
            },
            _ => match self.linked(node) {
                Some(id) => SlotReading::Style(id.to_owned()),
                None => SlotReading::None,
            },
        }
    }

    /// The slot's index, for the model's array of links.
    fn index(self) -> usize {
        match self {
            StyleSlot::Fill => 0,
            StyleSlot::Stroke => 1,
            StyleSlot::Effect => 2,
            StyleSlot::Grid => 3,
            StyleSlot::Text => 4,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The three states
// ═══════════════════════════════════════════════════════════════════════════

/// One layer's reading of a slot.
///
/// The layer-level reading, which the selection-level fold turns into a
/// [`StyleValue`]. Three answers: the layer links to no style, it links to
/// one, or it disagrees with itself.
#[derive(Clone, Debug, PartialEq)]
enum SlotReading {
    /// The layer links to no style.
    None,
    /// The layer links to this style.
    Style(String),
    /// The layer's own parts disagree.
    Mixed,
}

/// The state of a slot across a selection.
///
/// The fold of the layers' readings: every layer links to the same style, no
/// layer links to one, or they do not agree.
fn across_slots(readings: impl IntoIterator<Item = SlotReading>) -> StyleValue<String> {
    let mut readings = readings.into_iter();
    // An empty selection has no reading at all. `Unset` is the honest answer:
    // nothing is set, and nothing claims to be.
    let Some(first) = readings.next() else {
        return StyleValue::Unset;
    };
    readings.fold(slot_value(first), |state, next| {
        let next = slot_value(next);
        match (state, next) {
            // A disagreement can only ever widen.
            (StyleValue::Mixed, _) | (_, StyleValue::Mixed) => StyleValue::Mixed,
            (state, next) if state == next => state,
            _ => StyleValue::Mixed,
        }
    })
}

/// One layer's reading, as the state the fold starts from.
fn slot_value(reading: SlotReading) -> StyleValue<String> {
    match reading {
        SlotReading::None => StyleValue::Unset,
        SlotReading::Style(id) => StyleValue::Set(id),
        SlotReading::Mixed => StyleValue::Mixed,
    }
}

/// One property's state across a selection.
///
/// Three states, never two: a value every selected layer agrees on, agreement
/// that nobody set it, and the absence of agreement. The Inspector's own
/// properties have the same three, and the same words, so "Mixed" means one
/// thing in the panel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum StyleValue<T> {
    /// Every selected layer has this value, and they agree on it.
    Set(T),
    /// No selected layer has this value.
    #[default]
    Unset,
    /// The selected layers disagree.
    Mixed,
}

impl<T: PartialEq> StyleValue<T> {
    /// Fold a selection's readings into the one state that is honest for all
    /// of them.
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

/// One style the document offers.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleEntry {
    /// The engine's id for the style.
    pub id: String,
    /// The style's name, folders and all: "Brand/Blue/500".
    pub name: String,
    /// The style's description.
    pub description: String,
    /// What the style holds.
    pub kind: StyleKind,
    /// The variables bound to fields of the style's value, by the engine's own
    /// field key (`fills/0/color`), with the variable's name.
    pub bound: Vec<(String, String)>,
    /// The library the style was copied from, when it is a copy.
    ///
    /// A copy is read-only: the engine refuses to edit it, so the panel does
    /// not offer to.
    pub library: Option<String>,
    /// The style's id in that library, when it is a copy.
    pub library_key: Option<String>,
}

impl StyleEntry {
    /// Whether the style can be edited here.
    ///
    /// A local style can; a copy of a library's cannot — the engine refuses
    /// the change, so the panel does not offer it.
    pub fn editable(&self) -> bool {
        self.library.is_none()
    }

    /// The folder part of the style's name ("Brand/Blue/500" → "Brand/Blue"),
    /// empty at the top level.
    pub fn folder(&self) -> &str {
        folder_of(&self.name)
    }

    /// The last part of the style's name ("Brand/Blue/500" → "500").
    pub fn leaf(&self) -> &str {
        leaf_of(&self.name)
    }

    /// The variable bound to a field of the style's value, if one is.
    pub fn binding(&self, field: &str) -> Option<&str> {
        self.bound
            .iter()
            .find(|(key, _)| key == field)
            .map(|(_, variable)| variable.as_str())
    }
}

/// One folder of styles.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleFolder {
    /// The folder's own name: "Brand/Blue", empty at the top level.
    pub name: String,
    /// The folder's label, as the panel shows it: "Brand / Blue".
    pub label: String,
    /// The styles in the folder.
    pub styles: Vec<StyleEntry>,
}

/// One colour variable the document offers.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleVariable {
    /// The engine's id for the variable.
    pub id: String,
    /// The variable's name.
    pub name: String,
    /// The variable's value, as the panel shows it.
    pub value: String,
    /// The variable's colour, when it has one.
    pub color: Option<Color>,
}

/// What the Styles section shows about the selection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StylesModel {
    /// The layers the model was read for.
    pub ids: Vec<NodeId>,
    /// The style each slot links to, across the selection.
    ///
    /// One reading per slot, folded on its own: a disagreement about the fill
    /// says "Mixed" in the fill's row and leaves the stroke's row alone.
    pub links: [StyleValue<String>; 5],
    /// The styles the document offers, in the engine's order.
    pub styles: Vec<StyleEntry>,
    /// The colour variables the document offers.
    pub variables: Vec<StyleVariable>,
}

impl StylesModel {
    /// The styles and the links of a selection, as the panel shows them.
    ///
    /// A layer the document does not hold is skipped rather than shown as an
    /// empty one, and a selection with nothing selected links to nothing.
    pub fn read(doc: &Document, selection: &[NodeId]) -> Self {
        let nodes: Vec<&Node> = selection.iter().filter_map(|id| doc.node(*id)).collect();
        let links =
            StyleSlot::ALL.map(|slot| across_slots(nodes.iter().map(|node| slot.reading(node))));
        let styles = doc
            .styles
            .iter()
            .map(|style| {
                let imported = doc.libraries.style_import(&style.id);
                StyleEntry {
                    id: style.id.clone(),
                    name: style.name.clone(),
                    description: style.description.clone(),
                    kind: style.kind.clone(),
                    bound: style
                        .bound
                        .iter()
                        .map(|(field, variable)| {
                            let name = doc
                                .variables
                                .get(variable)
                                .map(|variable| variable.name.clone())
                                .unwrap_or_else(|| variable.clone());
                            (field.clone(), name)
                        })
                        .collect(),
                    library: imported.map(|imported| imported.library.clone()),
                    library_key: imported.map(|imported| imported.key.clone()),
                }
            })
            .collect();
        let variables = doc
            .variables
            .variables
            .iter()
            // A copy of a library's variable is the library's, offered in the
            // library's own picker; this file's are the ones it owns.
            .filter(|variable| doc.libraries.variable_import(&variable.id).is_none())
            .filter(|variable| variable.kind == VariableType::Color)
            .map(|variable| StyleVariable {
                id: variable.id.clone(),
                name: variable.name.clone(),
                value: variable
                    .values
                    .values()
                    .next()
                    .map(VariableValue::display)
                    .unwrap_or_default(),
                color: match variable.values.values().next() {
                    Some(VariableValue::Color(color)) => Some(*color),
                    _ => None,
                },
            })
            .collect();
        Self {
            ids: selection.to_vec(),
            links,
            styles,
            variables,
        }
    }

    /// The style a slot links to, across the selection.
    pub fn link(&self, slot: StyleSlot) -> &StyleValue<String> {
        &self.links[slot.index()]
    }

    /// One style, by its id.
    pub fn style(&self, id: &str) -> Option<&StyleEntry> {
        self.styles.iter().find(|style| style.id == id)
    }

    /// Whether the document holds any style the slot would offer.
    ///
    /// This is what tells "no matching styles" from "no styles yet": a search
    /// that found nothing is not the same as a file with no styles in it.
    pub fn has_any(&self, slot: StyleSlot) -> bool {
        self.styles.iter().any(|style| slot.holds(&style.kind))
    }

    /// The styles the slot offers, by folder.
    ///
    /// The search matches the whole name, in lower case, as figcraft's does:
    /// "brand" finds "Brand/Blue/500".
    ///
    /// Folders come in the order they first appear, and the styles that are
    /// not in a folder come first.
    pub fn folders_for(&self, slot: StyleSlot, query: &str) -> Vec<StyleFolder> {
        let query = query.trim().to_lowercase();
        let mut groups: Vec<(String, Vec<StyleEntry>)> = Vec::new();
        for style in self
            .styles
            .iter()
            .filter(|style| slot.holds(&style.kind))
            .filter(|style| query.is_empty() || style.name.to_lowercase().contains(&query))
        {
            let folder = style.folder().to_owned();
            match groups.iter_mut().find(|(name, _)| *name == folder) {
                Some((_, list)) => list.push(style.clone()),
                None => groups.push((folder, vec![style.clone()])),
            }
        }
        // The top level first, then the folders in the order they appeared.
        groups.sort_by_key(|(name, _)| !name.is_empty());
        groups
            .into_iter()
            .map(|(name, styles)| StyleFolder {
                label: name
                    .split('/')
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join(" / "),
                name,
                styles,
            })
            .collect()
    }

    /// The colour variables the search matches.
    pub fn variables_for(&self, query: &str) -> Vec<&StyleVariable> {
        let query = query.trim().to_lowercase();
        self.variables
            .iter()
            .filter(|variable| query.is_empty() || variable.name.to_lowercase().contains(&query))
            .collect()
    }

    /// The name a new style of this slot gets: the first "Color 1" that no
    /// style in the document already answers to.
    pub fn next_name(&self, slot: StyleSlot) -> String {
        let base = slot.default_name();
        (1..)
            .map(|number| format!("{base} {number}"))
            .find(|name| self.styles.iter().all(|style| style.name != *name))
            .unwrap_or_else(|| base.to_owned())
    }
}

/// The folder part of a style name ("Brand/Blue/500" → "Brand/Blue").
fn folder_of(name: &str) -> &str {
    name.rsplit_once('/')
        .map_or("", |(folder, _)| folder.trim())
}

/// The last part of a style name ("Brand/Blue/500" → "500").
fn leaf_of(name: &str) -> &str {
    name.rsplit_once('/').map_or(name, |(_, leaf)| leaf.trim())
}

// ═══════════════════════════════════════════════════════════════════════════
// The commands
// ═══════════════════════════════════════════════════════════════════════════

/// A change the Styles section can ask for.
///
/// One variant per thing the section can change, so a change the engine has no
/// command for cannot be written down as though it had one.
#[derive(Clone, Debug, PartialEq)]
pub enum StylesAction {
    /// Apply a style to the selection.
    Apply {
        /// The layers to apply it to.
        ids: Vec<NodeId>,
        /// The style: its id, or the key it has in its library.
        style: String,
        /// The library the style came from, when it is a library's.
        ///
        /// A library's style is copied in and applied with
        /// `library.applyStyle`; this file's own is applied with
        /// `style.apply`.
        library: Option<String>,
        /// The slot the style goes on, when the slot says which one.
        ///
        /// Only a colour style says: the engine takes a paint style for the
        /// fill unless it is told otherwise.
        target: Option<&'static str>,
    },
    /// Unlink the style from the selection's slot.
    Detach {
        /// The layers to unlink it from.
        ids: Vec<NodeId>,
        /// Which slot.
        which: &'static str,
    },
    /// Create a style from the selection.
    Create {
        /// The layers the style is made from.
        ids: Vec<NodeId>,
        /// The slot the style is made for.
        slot: StyleSlot,
        /// The style's name.
        name: String,
    },
    /// Change a style's name.
    Rename {
        /// The style to rename.
        style: String,
        /// Its new name.
        name: String,
    },
    /// Change a style's description.
    Describe {
        /// The style to describe.
        style: String,
        /// Its new description.
        description: String,
    },
    /// Redefine a style from a layer.
    UpdateFromSelection {
        /// The style to redefine.
        style: String,
        /// The layer to take the values from.
        from: NodeId,
    },
    /// Delete a style.
    Delete {
        /// The style to delete.
        style: String,
    },
    /// Bind a colour variable to the selection's fill or stroke.
    BindVariable {
        /// The layers to bind it on.
        ids: Vec<NodeId>,
        /// Which slot: "fill" or "stroke".
        which: &'static str,
        /// The variable's id.
        variable: String,
    },
    /// Unbind a variable from a field of a style.
    Unbind {
        /// The style.
        style: String,
        /// The field, in the engine's own spelling (`fills/0/color`).
        field: String,
    },
    /// Replace a text style's definition.
    SetText {
        /// The style to redefine.
        style: String,
        /// Its new definition.
        value: TextStyle,
    },
}

impl StylesAction {
    /// The engine's command id and parameters for this change.
    ///
    /// The engine's own spellings, so a change made from the panel is the
    /// same change the CLI, the keymap and an agent would make.
    pub fn command(&self) -> (String, Value) {
        match self {
            Self::Apply {
                ids,
                style,
                library,
                target,
            } => {
                let mut params = json!({ "ids": ids, "style": style });
                // A library's style is copied in and applied with the
                // library's own command; this file's own is applied with the
                // plain one.
                let command = match library {
                    Some(library) => {
                        params["library"] = json!(library);
                        "library.applyStyle"
                    }
                    None => "style.apply",
                };
                // Only a colour style says which slot it goes on: the engine
                // takes a paint style for the fill unless it is told otherwise.
                if let Some(target) = target {
                    params["target"] = json!(target);
                }
                (command.to_owned(), params)
            }
            Self::Detach { ids, which } => (
                "style.detach".to_owned(),
                json!({ "ids": ids, "which": which }),
            ),
            Self::Create { ids, slot, name } => {
                let mut params = json!({ "type": slot.create_type(), "name": name });
                if let Some(from) = ids.first() {
                    params["from"] = json!(from.0);
                }
                ("style.create".to_owned(), params)
            }
            Self::Rename { style, name } => (
                "style.update".to_owned(),
                json!({ "style": style, "name": name }),
            ),
            Self::Describe { style, description } => (
                "style.update".to_owned(),
                json!({ "style": style, "description": description }),
            ),
            Self::UpdateFromSelection { style, from } => (
                "style.update".to_owned(),
                json!({ "style": style, "from": from.0 }),
            ),
            Self::Delete { style } => ("style.delete".to_owned(), json!({ "style": style })),
            Self::BindVariable {
                ids,
                which,
                variable,
            } => (
                "variable.bind".to_owned(),
                json!({ "ids": ids, "field": which, "variable": variable }),
            ),
            Self::Unbind { style, field } => (
                "style.bind".to_owned(),
                json!({ "style": style, "field": field }),
            ),
            Self::SetText { style, value } => {
                let mut params = json!({ "style": style });
                // The engine's own `TextStyle`, serialized by the engine, so
                // the panel cannot spell a field the engine does not have.
                params["value"] = serde_json::to_value(value).unwrap_or(Value::Null);
                ("style.update".to_owned(), params)
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel's own state
// ═══════════════════════════════════════════════════════════════════════════

/// Which field is being typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleField {
    /// The browser's search field.
    Search,
    /// The edited style's name.
    Name,
    /// The edited style's description.
    Description,
    /// The edited text style's font family.
    FontFamily,
    /// The edited text style's font style.
    FontStyle,
    /// The edited text style's size.
    FontSize,
    /// The edited text style's line height.
    LineHeight,
    /// The edited text style's letter spacing.
    LetterSpacing,
}

/// A value being typed into a field.
///
/// A buffer, not a second copy of the style: the value becomes real only when
/// Enter hands it to the engine as one semantic operation.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleDraft {
    /// Which field.
    pub field: StyleField,
    /// The text as typed.
    pub text: String,
    /// Whether the text is selected, so the first keystroke replaces it.
    pub select_all: bool,
    /// The style the field belongs to, when it is one of the editor's.
    pub style: Option<String>,
    /// The text style the field changes, when it is one of the typography
    /// fields.
    ///
    /// The definition as it was when the field was opened, so a change to one
    /// field does not lose the others.
    pub base: Option<TextStyle>,
}

/// The style being edited, with its name and description as typed.
#[derive(Clone, Debug, PartialEq)]
pub struct Editing {
    /// The style's id.
    pub id: String,
    /// Its name, as typed.
    pub name: String,
    /// Its description, as typed.
    pub description: String,
}

/// The panel's own state.
///
/// Presentation only: never in the document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StylesPanelState {
    /// Which slot's list is open, if any.
    pub slot: Option<StyleSlot>,
    /// The search text the list is filtered by.
    pub query: String,
    /// Whether the list is shown as a grid rather than as a list.
    pub grid: bool,
    /// The folders that are collapsed.
    pub folds: BTreeSet<String>,
    /// The style being edited, if any.
    pub editing: Option<Editing>,
    /// The field being typed into, if any.
    pub draft: Option<StyleDraft>,
}

impl StylesPanelState {
    /// The state a section starts in.
    pub fn new() -> Self {
        Self::default()
    }

    /// The text the list is filtered by right now.
    ///
    /// While the search field is open the list follows what has been typed,
    /// which is how figcraft's search behaves: the list narrows as the query
    /// is written, not when it is committed.
    pub fn filter(&self) -> &str {
        match self.draft.as_ref() {
            Some(draft) if draft.field == StyleField::Search => &draft.text,
            _ => &self.query,
        }
    }

    /// Whether the section is part-way through something.
    ///
    /// Read-only, and kept in step with [`Self::cancel`]: the editor's Escape
    /// ladder reads this to decide which rung is on top, and only then asks
    /// the section to act on it.
    pub fn is_busy(&self) -> bool {
        self.draft.is_some()
    }

    /// Give up whatever the section was part-way through.
    ///
    /// Returns whether anything was given up, so the ladder knows the key was
    /// consumed.
    pub fn cancel(&mut self) -> bool {
        self.draft.take().is_some()
    }

    /// Handle one keystroke.
    ///
    /// Returns the change the key asks for, if the key was the section's own.
    /// Escape is not claimed: it belongs to the editor's ladder, which walks
    /// down from the topmost thing, and this draft is one of the rungs the
    /// ladder knows about.
    pub fn handle_key(&mut self, key: &str) -> Option<StylesAction> {
        let draft = self.draft.as_mut()?;
        match key {
            "enter" | "return" => {
                let draft = self.draft.take()?;
                self.commit_draft(draft)
            }
            "backspace" => {
                // With the text selected, Backspace deletes the selection.
                if draft.select_all {
                    draft.text.clear();
                    draft.select_all = false;
                } else {
                    draft.text.pop();
                }
                None
            }
            // Arrows are not characters. The section has no nudge to make, so
            // the key is swallowed rather than typed into the field.
            "up" | "down" | "left" | "right" => None,
            other if other.chars().count() == 1 => {
                if draft.select_all {
                    draft.text.clear();
                    draft.select_all = false;
                }
                draft.text.push_str(other);
                None
            }
            // A modifier combination is an editor shortcut, not a character.
            _ => None,
        }
    }

    /// Commit a typed value: what it means, if it means anything.
    fn commit_draft(&mut self, draft: StyleDraft) -> Option<StylesAction> {
        match draft.field {
            // The search is not an edit of the document: it is the list's own
            // filter, so it lands in the state.
            StyleField::Search => {
                self.query = draft.text;
                None
            }
            StyleField::Name => {
                let style = draft.style?;
                let name = draft.text.trim();
                if name.is_empty() {
                    return None;
                }
                Some(StylesAction::Rename {
                    style,
                    name: name.to_owned(),
                })
            }
            StyleField::Description => {
                let style = draft.style?;
                Some(StylesAction::Describe {
                    style,
                    description: draft.text,
                })
            }
            StyleField::FontFamily
            | StyleField::FontStyle
            | StyleField::FontSize
            | StyleField::LineHeight
            | StyleField::LetterSpacing => {
                let style = draft.style?;
                let mut value = draft.base?;
                match draft.field {
                    StyleField::FontFamily => value.font_family = draft.text,
                    StyleField::FontStyle => value.font_style = draft.text,
                    StyleField::FontSize => {
                        value.font_size = draft.text.trim().parse().ok()?;
                    }
                    StyleField::LineHeight => {
                        value.line_height = parse_line_height(&draft.text)?;
                    }
                    StyleField::LetterSpacing => {
                        value.letter_spacing = parse_letter_spacing(&draft.text)?;
                    }
                    _ => return None,
                }
                Some(StylesAction::SetText { style, value })
            }
        }
    }
}

/// A line height, as the field spells it.
///
/// "auto", a number of pixels, or a percentage — the engine's own three.
fn parse_line_height(text: &str) -> Option<figcraft_doc::LineHeight> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("auto") {
        return Some(figcraft_doc::LineHeight::Auto);
    }
    if let Some(percent) = text.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(figcraft_doc::LineHeight::Percent);
    }
    let pixels = text.strip_suffix("px").unwrap_or(text);
    pixels
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(figcraft_doc::LineHeight::Pixels)
}

/// A letter spacing, as the field spells it.
///
/// A number of pixels, or a percentage of the font size.
fn parse_letter_spacing(text: &str) -> Option<figcraft_doc::LetterSpacing> {
    let text = text.trim();
    if let Some(percent) = text.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(figcraft_doc::LetterSpacing::Percent);
    }
    let pixels = text.strip_suffix("px").unwrap_or(text);
    pixels
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(figcraft_doc::LetterSpacing::Pixels)
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// What a surface that hosts the Styles section must be able to do.
///
/// The panel draws and asks; the surface owns the engine and runs the changes.
/// The panel never touches the document itself, so the engine's command
/// surface stays the one operation boundary.
pub trait StylesHost: Sized + 'static {
    /// Run a change through the engine.
    fn run_styles_action(&mut self, action: StylesAction, cx: &mut Context<Self>);
}

/// The Styles section, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own state,
/// and the surface that runs the changes. It returns an element, so the
/// Inspector can put it in a section of its own and a test can put it in a
/// window on its own.
pub fn styles_section<T: StylesHost>(
    model: &StylesModel,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2().px(px(14.0)).py(px(12.0));
    for slot in StyleSlot::ALL {
        column = column.child(slot_row(model, slot, state, cx));
    }
    let open = state.borrow().slot;
    if let Some(slot) = open {
        column = column.child(browser(model, slot, state, cx));
    }
    column = column.child(caption(SAVE_PATH_NOTE));
    column.into_any_element()
}

/// The chip: the style a slot links to, and the way to unlink it.
///
/// figcraft's chip: the style's name, click opens the browser, and a detach
/// button unlinks it. Spool shows the chip even when nothing is linked, and
/// says so.
fn slot_row<T: StylesHost>(
    model: &StylesModel,
    slot: StyleSlot,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let link = model.link(slot);
    let (shown, has_style) = match link {
        StyleValue::Set(id) => {
            let name = model
                .style(id)
                .map(|style| style.name.clone())
                .unwrap_or_else(|| MISSING_STYLE.to_owned());
            (name, true)
        }
        StyleValue::Unset => (NOT_SET.to_owned(), false),
        StyleValue::Mixed => (MIXED.to_owned(), true),
    };
    let open = state.borrow().slot == Some(slot);
    let label = slot.label();
    let value_selector = format!("styles-slot-{}-value-{shown}", slot.key());
    let slot_key = slot.key();
    let mut row = div()
        .id(SharedString::from(format!("styles-slot-{slot_key}")))
        .debug_selector(move || format!("styles-slot-{slot_key}"))
        .flex()
        .items_center()
        .gap_2()
        .px(px(8.0))
        .py(px(7.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(if open {
            theme::ACCENT
        } else {
            theme::BORDER_SOFT
        }))
        .text_xs()
        .child(div().text_color(rgb(theme::TEXT_MUTED)).child(label))
        .child(
            div()
                .id(SharedString::from(value_selector.clone()))
                .debug_selector(move || value_selector.clone())
                .flex_1()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(shown),
        );
    let shared = Rc::clone(state);
    row = row.on_click(cx.listener(move |_host, _event, _window, cx| {
        {
            let mut state = shared.borrow_mut();
            state.slot = if state.slot == Some(slot) {
                None
            } else {
                Some(slot)
            };
            state.editing = None;
            state.draft = None;
        }
        cx.notify();
    }));
    // The detach button is only there when there is something to detach.
    if has_style {
        let ids = model.ids.clone();
        let which = slot.key();
        row = row.child(
            div()
                .id(SharedString::from(format!("styles-slot-{which}-detach")))
                .debug_selector(move || format!("styles-slot-{which}-detach"))
                .flex()
                .items_center()
                .justify_center()
                .w(px(20.0))
                .h(px(20.0))
                .rounded_sm()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("×")
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    cx.stop_propagation();
                    host.run_styles_action(
                        StylesAction::Detach {
                            ids: ids.clone(),
                            which,
                        },
                        cx,
                    );
                })),
        );
    }
    row.into_any_element()
}

/// The browser: the list of styles the slot offers, or the editor of one.
fn browser<T: StylesHost>(
    model: &StylesModel,
    slot: StyleSlot,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let editing = state.borrow().editing.clone();
    match editing {
        Some(editing) => editor(model, &editing, state, cx),
        None => listing(model, slot, state, cx),
    }
}

/// The list: the header, the search, the styles by folder, the variables.
fn listing<T: StylesHost>(
    model: &StylesModel,
    slot: StyleSlot,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2();
    column = column.child(listing_header(model, slot, state, cx));
    column = column.child(search_field(state, cx));
    let query = state.borrow().filter().to_owned();
    let folders = model.folders_for(slot, &query);
    if folders.is_empty() {
        column = column.child(empty_state(model, slot));
    }
    for folder in &folders {
        if !folder.name.is_empty() {
            column = column.child(folder_row(folder, state, cx));
        }
        if state.borrow().grid && slot.has_grid() {
            column = column.child(style_grid(model, slot, &folder.styles, cx));
        } else {
            for style in &folder.styles {
                column = column.child(style_row(model, slot, style, state, cx));
            }
        }
    }
    // The colour variables a colour slot can take.
    if slot.paints() {
        let variables = model.variables_for(&query);
        if !variables.is_empty() {
            column = column.child(caption("Color variables"));
            for variable in variables {
                column = column.child(variable_row(model, slot, variable, cx));
            }
        }
    }
    column.into_any_element()
}

/// The header: the slot's title, and the way to create, detach and to change
/// the view.
fn listing_header<T: StylesHost>(
    model: &StylesModel,
    slot: StyleSlot,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let title = slot.title();
    let mut header = div().flex().items_center().justify_between().gap_2().child(
        div()
            .id(SharedString::from("styles-picker-title"))
            .debug_selector(|| "styles-picker-title".to_owned())
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgb(theme::TEXT))
            .child(title),
    );
    // The create button. A style is made from a layer, so the button is only
    // there when there is one: figcraft's browser is only reachable with a
    // selection, and a button that can only fail is not a button.
    let ids = model.ids.clone();
    if !ids.is_empty() {
        let name = model.next_name(slot);
        let clicked = ids.clone();
        header = header.child(
            div()
                .id(SharedString::from("styles-picker-create"))
                .debug_selector(|| "styles-picker-create".to_owned())
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("＋")
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    host.run_styles_action(
                        StylesAction::Create {
                            ids: clicked.clone(),
                            slot,
                            name: name.clone(),
                        },
                        cx,
                    );
                })),
        );
    }
    // The detach button, when there is something to detach.
    let linked = !model.link(slot).is_unset();
    if linked {
        let ids = model.ids.clone();
        let which = slot.key();
        header = header.child(
            div()
                .id(SharedString::from("styles-picker-detach"))
                .debug_selector(|| "styles-picker-detach".to_owned())
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("⊘")
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    host.run_styles_action(
                        StylesAction::Detach {
                            ids: ids.clone(),
                            which,
                        },
                        cx,
                    );
                })),
        );
    }
    // The view toggle, for the slots whose styles show as a grid.
    if slot.has_grid() {
        let shared = Rc::clone(state);
        let grid = state.borrow().grid;
        header = header.child(
            div()
                .id(SharedString::from("styles-picker-view"))
                .debug_selector(|| "styles-picker-view".to_owned())
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child(if grid { "☰" } else { "▦" })
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.grid = !state.grid;
                    }
                    cx.notify();
                })),
        );
    }
    header.into_any_element()
}

/// The search field.
fn search_field<T: StylesHost>(
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let draft = state
        .borrow()
        .draft
        .as_ref()
        .filter(|draft| draft.field == StyleField::Search)
        .map(|draft| draft.text.clone());
    let shown = draft.unwrap_or_else(|| {
        let query = state.borrow().query.clone();
        if query.is_empty() {
            "Search".to_owned()
        } else {
            query
        }
    });
    let shared = Rc::clone(state);
    div()
        .id(SharedString::from("styles-search"))
        .debug_selector(|| "styles-search".to_owned())
        .flex()
        .items_center()
        .px(px(8.0))
        .py(px(7.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .cursor_text()
        .child(shown)
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.draft = Some(StyleDraft {
                    field: StyleField::Search,
                    text: state.query.clone(),
                    select_all: true,
                    style: None,
                    base: None,
                });
            }
            cx.notify();
        }))
        .into_any_element()
}

/// The empty state, word for word as figcraft says it.
fn empty_state(model: &StylesModel, slot: StyleSlot) -> AnyElement {
    let hint = if model.has_any(slot) {
        "No matching styles"
    } else if slot == StyleSlot::Text {
        "No text styles yet. Click + to create one from the selected text."
    } else {
        "No styles yet. Click + to create one from the selection."
    };
    let selector = format!("styles-empty-{hint}");
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(hint.to_owned())
        .into_any_element()
}

/// A folder row: the folder's name, which folds the folder away.
fn folder_row<T: StylesHost>(
    folder: &StyleFolder,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let closed = state.borrow().folds.contains(&folder.name);
    let shared = Rc::clone(state);
    let name = folder.name.clone();
    let selector = if folder.name.is_empty() {
        "styles-folder-top".to_owned()
    } else {
        format!("styles-folder-{}", folder.name)
    };
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_2()
        .px(px(6.0))
        .py(px(6.0))
        .rounded_md()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer()
        .child(if closed { "▸" } else { "▾" })
        .child(folder.label.clone())
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                if !state.folds.remove(&name) {
                    state.folds.insert(name.clone());
                }
            }
            cx.notify();
        }))
        .into_any_element()
}

/// One style in the list: its preview, its name, and the marks it carries.
fn style_row<T: StylesHost>(
    model: &StylesModel,
    slot: StyleSlot,
    style: &StyleEntry,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let applied = model.link(slot).value().map(String::as_str) == Some(style.id.as_str());
    let mut row = div()
        .id(SharedString::from(format!("styles-row-{}", style.id)))
        .debug_selector(move || format!("styles-row-{}", style.id))
        .flex()
        .items_center()
        .gap_2()
        .px(px(6.0))
        .py(px(6.0))
        .rounded_md()
        .text_xs();
    match &style.kind {
        StyleKind::Text { style: definition } => {
            let size = definition.font_size.clamp(6.0, 16.0) as f32;
            let meta = format!(
                "{}/{}",
                figcraft_doc::trim_num(definition.font_size),
                definition.line_height.label()
            );
            row = row.child(
                div()
                    .flex_1()
                    .text_size(px(size))
                    .text_color(rgb(theme::TEXT))
                    .child(style.leaf().to_owned()),
            );
            let meta_selector = format!("styles-row-{}-{}", style.id, meta);
            row = row.child(
                div()
                    .id(SharedString::from(meta_selector.clone()))
                    .debug_selector(move || meta_selector.clone())
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(meta),
            );
        }
        kind => {
            row = row.child(preview(kind));
            row = row.child(
                div()
                    .flex_1()
                    .text_color(rgb(theme::TEXT))
                    .child(style.leaf().to_owned()),
            );
        }
    }
    // A style with a variable bound inside it is marked.
    if !style.bound.is_empty() {
        row = row.child(
            div()
                .id(SharedString::from(format!(
                    "styles-row-{}-variables",
                    style.id
                )))
                .debug_selector(move || format!("styles-row-{}-variables", style.id))
                .text_color(rgb(theme::TEXT_MUTED))
                .child("var"),
        );
    }
    // The check, when the style is the one that is applied.
    if applied {
        row = row.child(
            div()
                .id(SharedString::from(format!(
                    "styles-row-{}-applied",
                    style.id
                )))
                .debug_selector(move || format!("styles-row-{}-applied", style.id))
                .text_color(rgb(theme::ACCENT))
                .child("✓"),
        );
    }
    // The edit button. figcraft reveals it on hover; Spool shows it, because a
    // hover-only control cannot be reached without a pointer.
    if style.editable() {
        let shared = Rc::clone(state);
        let id = style.id.clone();
        let name = style.name.clone();
        let description = style.description.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!("styles-row-{}-edit", style.id)))
                .debug_selector(move || format!("styles-row-{}-edit", style.id))
                .flex()
                .items_center()
                .justify_center()
                .w(px(20.0))
                .h(px(20.0))
                .rounded_sm()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("✎")
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    // The button is inside the row, and the row applies the
                    // style: without this the click would do both.
                    cx.stop_propagation();
                    {
                        let mut state = shared.borrow_mut();
                        state.editing = Some(Editing {
                            id: id.clone(),
                            name: name.clone(),
                            description: description.clone(),
                        });
                    }
                    cx.notify();
                })),
        );
    }
    // The click that applies the style.
    let ids = model.ids.clone();
    let style_id = style.id.clone();
    let library = style.library.clone();
    let library_key = style.library_key.clone();
    let target = if slot.paints() {
        Some(slot.key())
    } else {
        None
    };
    row.on_click(cx.listener(move |host, _event, _window, cx| {
        host.run_styles_action(
            StylesAction::Apply {
                ids: ids.clone(),
                style: library_key.clone().unwrap_or_else(|| style_id.clone()),
                library: library.clone(),
                target,
            },
            cx,
        );
    }))
    .into_any_element()
}

/// The styles of a folder as a grid: swatches for colour styles, "Ag" tiles
/// for text styles.
fn style_grid<T: StylesHost>(
    model: &StylesModel,
    slot: StyleSlot,
    styles: &[StyleEntry],
    cx: &mut Context<T>,
) -> AnyElement {
    let text = styles
        .first()
        .is_some_and(|style| matches!(style.kind, StyleKind::Text { .. }));
    let per_row = if text { 3 } else { 7 };
    let mut column = div().flex().flex_col().gap_1();
    for chunk in styles.chunks(per_row) {
        let mut row = div().flex().gap_1();
        for style in chunk {
            row = row.child(grid_cell(model, slot, style, cx));
        }
        column = column.child(row);
    }
    column.into_any_element()
}

/// One cell of the grid.
fn grid_cell<T: StylesHost>(
    model: &StylesModel,
    slot: StyleSlot,
    style: &StyleEntry,
    cx: &mut Context<T>,
) -> AnyElement {
    let applied = model.link(slot).value().map(String::as_str) == Some(style.id.as_str());
    let cell = match &style.kind {
        StyleKind::Text { style: definition } => {
            let size = definition.font_size.clamp(6.0, 24.0) as f32;
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(54.0))
                .h(px(44.0))
                .rounded_md()
                .bg(rgb(theme::SURFACE))
                .border_1()
                .border_color(rgb(if applied {
                    theme::ACCENT
                } else {
                    theme::BORDER_SOFT
                }))
                .text_size(px(size))
                .text_color(rgb(theme::TEXT))
                .child("Ag")
        }
        kind => div()
            .w(px(24.0))
            .h(px(24.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(if applied {
                theme::ACCENT
            } else {
                theme::BORDER_SOFT
            }))
            .child(preview(kind)),
    };
    let ids = model.ids.clone();
    let style_id = style.id.clone();
    let library = style.library.clone();
    let library_key = style.library_key.clone();
    let target = if slot.paints() {
        Some(slot.key())
    } else {
        None
    };
    cell.id(SharedString::from(format!("styles-grid-{}", style.id)))
        .debug_selector(move || format!("styles-grid-{}", style.id))
        .cursor_pointer()
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_styles_action(
                StylesAction::Apply {
                    ids: ids.clone(),
                    style: library_key.clone().unwrap_or_else(|| style_id.clone()),
                    library: library.clone(),
                    target,
                },
                cx,
            );
        }))
        .into_any_element()
}

/// One colour variable, and the way to bind it to the slot.
fn variable_row<T: StylesHost>(
    model: &StylesModel,
    slot: StyleSlot,
    variable: &StyleVariable,
    cx: &mut Context<T>,
) -> AnyElement {
    let ids = model.ids.clone();
    let which = slot.key();
    let variable_id = variable.id.clone();
    div()
        .id(SharedString::from(format!(
            "styles-variable-{}",
            variable.id
        )))
        .debug_selector(move || format!("styles-variable-{}", variable.id))
        .flex()
        .items_center()
        .gap_2()
        .px(px(6.0))
        .py(px(6.0))
        .rounded_md()
        .text_xs()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer()
        .child(swatch_color(variable.color))
        .child(
            div()
                .flex_1()
                .text_color(rgb(theme::TEXT))
                .child(variable.name.clone()),
        )
        .child(
            div()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(variable.value.clone()),
        )
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_styles_action(
                StylesAction::BindVariable {
                    ids: ids.clone(),
                    which,
                    variable: variable_id.clone(),
                },
                cx,
            );
        }))
        .into_any_element()
}

/// The editor of one style: its name, its description, its values, and the
/// way to take the values from the selection or to delete it.
fn editor<T: StylesHost>(
    model: &StylesModel,
    editing: &Editing,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2();
    column = column.child(editor_header(editing, state, cx));
    column = column.child(label_field(
        "Name",
        editing.name.clone(),
        StyleField::Name,
        editing.id.clone(),
        None,
        state,
        cx,
    ));
    column = column.child(label_field(
        "Description",
        editing.description.clone(),
        StyleField::Description,
        editing.id.clone(),
        None,
        state,
        cx,
    ));
    column = column.child(caption("Properties"));
    column = column.child(properties(model, editing, state, cx));
    column = column.child(editor_actions(model, editing, state, cx));
    column.into_any_element()
}

/// The editor's header: the way back, and the title.
fn editor_header<T: StylesHost>(
    editing: &Editing,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let shared = Rc::clone(state);
    let id = editing.id.clone();
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .id(SharedString::from("styles-editor-back"))
                .debug_selector(|| "styles-editor-back".to_owned())
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("‹")
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.editing = None;
                        state.draft = None;
                    }
                    cx.notify();
                })),
        )
        .child(
            div()
                .id(SharedString::from("styles-editor-title"))
                .debug_selector(|| "styles-editor-title".to_owned())
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(rgb(theme::TEXT))
                .child("Edit style"),
        )
        .child(div().flex_1())
        .child(
            div()
                .id(SharedString::from(format!("styles-editor-id-{id}")))
                .debug_selector(move || format!("styles-editor-id-{id}"))
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(editing.name.clone()),
        )
        .into_any_element()
}

/// The style's own values, as the slot's kind shows them.
fn properties<T: StylesHost>(
    model: &StylesModel,
    editing: &Editing,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let Some(style) = model.style(&editing.id) else {
        return caption("This style is gone.").into_any_element();
    };
    let mut column = div().flex().flex_col().gap_1();
    match &style.kind {
        StyleKind::Paint { paints } if paints.is_empty() => {
            column = column.child(caption("No paints"));
        }
        StyleKind::Paint { paints } => {
            // The top of the list paints on top: show them in reverse, the way
            // the Fill section does.
            for (index, paint) in paints.iter().enumerate().rev() {
                let label = format!("{}  {:.0}%", paint.label(), paint.opacity * 100.0);
                let mut row = property_row(&label, Some(paint));
                // A solid paint's colour takes a variable.
                let field = format!("fills/{index}/color");
                if matches!(paint.kind, PaintKind::Solid { .. }) {
                    row = row.child(binding_cell(style, &field, cx));
                }
                column = column.child(row);
            }
        }
        StyleKind::Effect { effects } => {
            for (index, effect) in effects.iter().enumerate().rev() {
                let mut row = property_row(effect.label(), None);
                if let Some((field, _kind)) = effect_binding(effect, index) {
                    row = row.child(binding_cell(style, &field, cx));
                }
                column = column.child(row);
            }
        }
        StyleKind::Grid { grids } => {
            for (index, grid) in grids.iter().enumerate() {
                let mut row = property_row(&grid_label(grid), None);
                let field = format!("layoutGrids/{index}/size");
                row = row.child(binding_cell(style, &field, cx));
                column = column.child(row);
            }
        }
        StyleKind::Text { style: definition } => {
            column = column.child(text_properties(editing, definition, state, cx));
        }
    }
    column.into_any_element()
}

/// The field of an effect a variable binds from its row, and its type.
fn effect_binding(effect: &figcraft_doc::Effect, index: usize) -> Option<(String, VariableType)> {
    use figcraft_doc::EffectKind as Kind;
    match effect.kind {
        Kind::DropShadow(_) | Kind::InnerShadow(_) => {
            Some((format!("effects/{index}/color"), VariableType::Color))
        }
        Kind::LayerBlur(_) | Kind::BackgroundBlur(_) => {
            Some((format!("effects/{index}/radius"), VariableType::Number))
        }
        _ => None,
    }
}

/// A layout grid's summary ("Columns 12", "Grid 8").
fn grid_label(grid: &figcraft_doc::LayoutGrid) -> String {
    match grid.kind {
        figcraft_doc::GridKind::Grid => {
            format!("Grid {}", figcraft_doc::trim_num(grid.size))
        }
        figcraft_doc::GridKind::Columns => format!("Columns {}", grid.count),
        figcraft_doc::GridKind::Rows => format!("Rows {}", grid.count),
    }
}

/// One property row in the editor: a paint, an effect, a grid.
fn property_row(label: &str, paint: Option<&Paint>) -> Div {
    let mut row = div()
        .flex()
        .items_center()
        .gap_2()
        .px(px(6.0))
        .py(px(6.0))
        .rounded_md()
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY));
    row = match paint {
        Some(paint) => row.child(swatch(Some(paint))),
        None => row.child(div().text_color(rgb(theme::TEXT_MUTED)).child("◆")),
    };
    row.child(label.to_owned())
}

/// The variable bound to a field of the style, or the way to bind one.
///
/// A bound variable is named, and a click on the name unbinds it.
fn binding_cell<T: StylesHost>(style: &StyleEntry, field: &str, cx: &mut Context<T>) -> AnyElement {
    match style.binding(field) {
        Some(variable) => {
            let id = style.id.clone();
            let field = field.to_owned();
            let selector = format!("styles-binding-{field}");
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap_1()
                .px(px(6.0))
                .py(px(3.0))
                .rounded_sm()
                .border_1()
                .border_color(rgb(theme::BORDER_SOFT))
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("var")
                .child(variable.to_owned())
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    host.run_styles_action(
                        StylesAction::Unbind {
                            style: id.clone(),
                            field: field.clone(),
                        },
                        cx,
                    );
                }))
                .into_any_element()
        }
        None => div()
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .child("")
            .into_any_element(),
    }
}

/// A text style's typography: a sample, and the fields that make it.
fn text_properties<T: StylesHost>(
    editing: &Editing,
    definition: &TextStyle,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let size = definition.font_size.clamp(6.0, 28.0) as f32;
    let summary = format!(
        "{} {}\n{}/{}",
        definition.font_family,
        definition.font_style,
        figcraft_doc::trim_num(definition.font_size),
        definition.line_height.label()
    );
    let mut column = div().flex().flex_col().gap_2();
    column = column.child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .px(px(8.0))
            .py(px(6.0))
            .rounded_md()
            .bg(rgb(theme::SURFACE))
            .child(
                div()
                    .text_size(px(size))
                    .text_color(rgb(theme::TEXT))
                    .child("Ag"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(summary),
            ),
    );
    let id = editing.id.clone();
    let base = definition.clone();
    column = column.child(label_field(
        "Font family",
        definition.font_family.clone(),
        StyleField::FontFamily,
        id.clone(),
        Some(base.clone()),
        state,
        cx,
    ));
    column = column.child(label_field(
        "Font style",
        definition.font_style.clone(),
        StyleField::FontStyle,
        id.clone(),
        Some(base.clone()),
        state,
        cx,
    ));
    column = column.child(label_field(
        "Size",
        figcraft_doc::trim_num(definition.font_size),
        StyleField::FontSize,
        id.clone(),
        Some(base.clone()),
        state,
        cx,
    ));
    column = column.child(label_field(
        "Line height",
        definition.line_height.label(),
        StyleField::LineHeight,
        id.clone(),
        Some(base.clone()),
        state,
        cx,
    ));
    column = column.child(label_field(
        "Letter spacing",
        definition.letter_spacing.label(),
        StyleField::LetterSpacing,
        id,
        Some(base),
        state,
        cx,
    ));
    column.into_any_element()
}

/// A labelled field: the label, and the value that is being typed into.
fn label_field<T: StylesHost>(
    label: &str,
    value: String,
    field: StyleField,
    style: String,
    base: Option<TextStyle>,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let draft = state
        .borrow()
        .draft
        .as_ref()
        .filter(|draft| draft.field == field && draft.style.as_deref() == Some(style.as_str()))
        .map(|draft| draft.text.clone());
    let shown = draft.unwrap_or(value);
    let shared = Rc::clone(state);
    let seed = shown.clone();
    let base_for_draft = base.clone();
    div()
        .id(SharedString::from(format!(
            "styles-field-{}",
            field_name(field)
        )))
        .debug_selector(move || format!("styles-field-{}", field_name(field)))
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px(px(8.0))
        .py(px(7.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .child(
            div()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(label.to_owned()),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "styles-field-{}-value",
                    field_name(field)
                )))
                .debug_selector(move || format!("styles-field-{}-value", field_name(field)))
                .text_color(rgb(theme::TEXT_SECONDARY))
                .cursor_text()
                .child(shown),
        )
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.draft = Some(StyleDraft {
                    field,
                    text: seed.clone(),
                    select_all: true,
                    style: Some(style.clone()),
                    base: base_for_draft.clone(),
                });
            }
            cx.notify();
        }))
        .into_any_element()
}

/// The name of a field, for a selector.
fn field_name(field: StyleField) -> &'static str {
    match field {
        StyleField::Search => "search",
        StyleField::Name => "name",
        StyleField::Description => "description",
        StyleField::FontFamily => "font-family",
        StyleField::FontStyle => "font-style",
        StyleField::FontSize => "font-size",
        StyleField::LineHeight => "line-height",
        StyleField::LetterSpacing => "letter-spacing",
    }
}

/// The editor's actions: take the values from the selection, or delete it.
fn editor_actions<T: StylesHost>(
    model: &StylesModel,
    editing: &Editing,
    state: &Rc<RefCell<StylesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut row = div().flex().gap_2();
    // "Update from selection" needs a layer to take the values from.
    if let Some(from) = model.ids.first() {
        let from = *from;
        let id = editing.id.clone();
        row = row.child(
            div()
                .id(SharedString::from("styles-editor-update"))
                .debug_selector(|| "styles-editor-update".to_owned())
                .flex_1()
                .items_center()
                .justify_center()
                .px(px(8.0))
                .py(px(7.0))
                .rounded_md()
                .border_1()
                .border_color(rgb(theme::BORDER))
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("Update from selection")
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    host.run_styles_action(
                        StylesAction::UpdateFromSelection {
                            style: id.clone(),
                            from,
                        },
                        cx,
                    );
                })),
        );
    }
    let shared = Rc::clone(state);
    let id = editing.id.clone();
    row = row.child(
        div()
            .id(SharedString::from("styles-editor-delete"))
            .debug_selector(|| "styles-editor-delete".to_owned())
            .flex_1()
            .items_center()
            .justify_center()
            .px(px(8.0))
            .py(px(7.0))
            .rounded_md()
            .border_1()
            .border_color(rgb(theme::BORDER))
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("Delete")
            .on_click(cx.listener(move |host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.editing = None;
                }
                host.run_styles_action(StylesAction::Delete { style: id.clone() }, cx);
            })),
    );
    row.into_any_element()
}

// ── Shared chrome ───────────────────────────────────────────────────────────

/// A caption: the dim text that names a group.
fn caption(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// The preview of a style: its first paint, or the slot's own mark.
fn preview(kind: &StyleKind) -> Div {
    match kind {
        StyleKind::Paint { paints } => {
            let paint = paints
                .last()
                .cloned()
                .unwrap_or_else(|| Paint::solid(figcraft_doc::color::Color::TRANSPARENT));
            swatch(Some(&paint))
        }
        StyleKind::Effect { .. } => div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(14.0))
            .h(px(14.0))
            .text_color(rgb(theme::TEXT_MUTED))
            .child("✦"),
        StyleKind::Grid { .. } => div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(14.0))
            .h(px(14.0))
            .text_color(rgb(theme::TEXT_MUTED))
            .child("⊞"),
        StyleKind::Text { .. } => div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(14.0))
            .h(px(14.0))
            .text_color(rgb(theme::TEXT_MUTED))
            .child("T"),
    }
}

/// A paint's swatch.
fn swatch(paint: Option<&Paint>) -> Div {
    match paint {
        Some(paint) => {
            let [r, g, b, _] = paint.preview_color().to_rgba8();
            div()
                .size(px(14.0))
                .rounded_sm()
                .bg(rgb((u32::from(r) << 16)
                    | (u32::from(g) << 8)
                    | u32::from(b)))
        }
        None => div().size(px(14.0)).rounded_sm().bg(rgba(0x00000000)),
    }
}

/// A colour's swatch.
fn swatch_color(color: Option<Color>) -> Div {
    match color {
        Some(color) => {
            let [r, g, b, _] = color.to_rgba8();
            div()
                .size(px(14.0))
                .rounded_sm()
                .bg(rgb((u32::from(r) << 16)
                    | (u32::from(g) << 8)
                    | u32::from(b)))
        }
        None => div().size(px(14.0)).rounded_sm().bg(rgba(0x00000000)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use figcraft_doc::{NodeKind, Style};
    use figcraft_engine::Session;
    use gpui::{point, size, Entity, VisualTestContext};

    // ── The engine's own model ───────────────────────────────────────────────

    /// A document with one rectangle on it.
    fn document_with(styles: Vec<Style>) -> (Document, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(page, NodeKind::Rectangle, "Card", 0.0, 0.0, 100.0, 100.0)
            .expect("a rectangle");
        doc.styles = styles;
        (doc, id)
    }

    /// A colour style, of the engine's own kind.
    fn paint_style(id: &str, name: &str) -> Style {
        Style {
            id: id.to_owned(),
            name: name.to_owned(),
            description: String::new(),
            kind: StyleKind::Paint {
                paints: vec![Paint::solid(Color::from_rgb8_const(0x22, 0x66, 0xcc))],
            },
            bound: BTreeMap::new(),
        }
    }

    /// A text style, of the engine's own kind.
    fn text_style(id: &str, name: &str) -> Style {
        Style {
            id: id.to_owned(),
            name: name.to_owned(),
            description: "The body text".to_owned(),
            kind: StyleKind::Text {
                style: TextStyle {
                    font_family: "Inter".to_owned(),
                    font_style: "Regular".to_owned(),
                    font_size: 16.0,
                    ..TextStyle::default()
                },
            },
            bound: BTreeMap::new(),
        }
    }

    #[test]
    fn the_model_reads_the_engines_styles_for_a_node() {
        // The style the layer links to, and the styles the document offers.
        let (mut doc, id) = document_with(vec![paint_style("s1", "Brand/Blue/500")]);
        doc.get_mut(id).expect("the layer").styles.fill = Some("s1".to_owned());
        let model = StylesModel::read(&doc, &[id]);

        assert_eq!(
            model.link(StyleSlot::Fill),
            &StyleValue::Set("s1".to_owned()),
            "the fill links to the style the layer carries"
        );
        assert_eq!(
            model.link(StyleSlot::Stroke),
            &StyleValue::Unset,
            "the stroke links to nothing, and that is a state"
        );
        assert_eq!(model.styles.len(), 1);
        assert_eq!(model.styles[0].name, "Brand/Blue/500");
        assert_eq!(model.styles[0].leaf(), "500");
        assert_eq!(model.styles[0].folder(), "Brand/Blue");
        assert!(model.styles[0].editable(), "a local style can be edited");

        // The slot offers the style, under the folder its name says.
        let folders = model.folders_for(StyleSlot::Fill, "");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "Brand/Blue");
        assert_eq!(folders[0].label, "Brand / Blue");
        assert_eq!(folders[0].styles.len(), 1);
        // And the effect slot offers nothing: a paint style is not an effect
        // style.
        assert!(model.folders_for(StyleSlot::Effect, "").is_empty());
    }

    #[test]
    fn the_search_matches_the_whole_name() {
        let (doc, id) = document_with(vec![
            paint_style("s1", "Brand/Blue/500"),
            paint_style("s2", "Brand/Grey/100"),
        ]);
        let model = StylesModel::read(&doc, &[id]);
        let folders = model.folders_for(StyleSlot::Fill, "grey");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].styles.len(), 1);
        assert_eq!(folders[0].styles[0].id, "s2");
        // A search that finds nothing is not the same as a file with no
        // styles in it.
        assert!(model.has_any(StyleSlot::Fill));
        assert!(model
            .folders_for(StyleSlot::Fill, "nothing at all")
            .is_empty());
    }

    #[test]
    fn a_new_style_gets_the_first_name_nobody_has() {
        let (doc, id) = document_with(vec![paint_style("s1", "Color 1")]);
        let model = StylesModel::read(&doc, &[id]);
        assert_eq!(model.next_name(StyleSlot::Fill), "Color 2");
        assert_eq!(model.next_name(StyleSlot::Effect), "Effect 1");
        assert_eq!(model.next_name(StyleSlot::Text), "Text 1");
    }

    #[test]
    fn a_mixed_selection_reads_mixed_for_the_slot_that_differs() {
        // Two layers: one links to a style, the other does not.
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let first = doc
            .create(page, NodeKind::Rectangle, "One", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let second = doc
            .create(page, NodeKind::Rectangle, "Two", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        doc.styles = vec![paint_style("s1", "Brand/Blue/500")];
        doc.get_mut(first).expect("the layer").styles.fill = Some("s1".to_owned());

        let model = StylesModel::read(&doc, &[first, second]);
        assert_eq!(
            model.link(StyleSlot::Fill),
            &StyleValue::Mixed,
            "one layer has a fill style and the other has none"
        );
        // The fold is per property: the stroke agrees on nothing, and that is
        // not the same as disagreeing.
        assert_eq!(model.link(StyleSlot::Stroke), &StyleValue::Unset);
        assert_eq!(model.link(StyleSlot::Fill).word(), MIXED);
        assert_eq!(model.link(StyleSlot::Stroke).word(), NOT_SET);
    }

    #[test]
    fn two_layers_that_link_to_different_styles_are_mixed() {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let first = doc
            .create(page, NodeKind::Rectangle, "One", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let second = doc
            .create(page, NodeKind::Rectangle, "Two", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        doc.styles = vec![
            paint_style("s1", "Brand/Blue/500"),
            paint_style("s2", "Brand/Grey/100"),
        ];
        doc.get_mut(first).expect("the layer").styles.fill = Some("s1".to_owned());
        doc.get_mut(second).expect("the layer").styles.fill = Some("s2".to_owned());

        let model = StylesModel::read(&doc, &[first, second]);
        assert_eq!(model.link(StyleSlot::Fill), &StyleValue::Mixed);
    }

    #[test]
    fn a_text_layers_link_is_a_fact_about_its_characters() {
        // The engine's own reading: which style every character is linked to.
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(
                page,
                NodeKind::Text(figcraft_doc::TextData::new("abcdef")),
                "Note",
                0.0,
                0.0,
                100.0,
                20.0,
            )
            .expect("a text node");
        doc.styles = vec![
            text_style("t1", "Body/Regular"),
            text_style("t2", "Body/Bold"),
        ];
        {
            let node = doc.get_mut(id).expect("the layer");
            let NodeKind::Text(text) = &mut node.kind else {
                panic!("the layer is a text node");
            };
            text.characters = "abcdef".to_owned();
            let regular = TextStyle::default();
            let bold = TextStyle {
                font_style: "Bold".to_owned(),
                ..TextStyle::default()
            };
            text.link_text_style(0, 3, &regular, "t1");
            text.link_text_style(3, 6, &bold, "t2");
        }

        let model = StylesModel::read(&doc, &[id]);
        assert_eq!(
            model.link(StyleSlot::Text),
            &StyleValue::Mixed,
            "the characters link to different styles, so the layer disagrees with itself"
        );

        // Every character linked to the same style: the layer agrees.
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(
                page,
                NodeKind::Text(figcraft_doc::TextData::new("abcdef")),
                "Note",
                0.0,
                0.0,
                100.0,
                20.0,
            )
            .expect("a text node");
        doc.styles = vec![text_style("t1", "Body/Regular")];
        {
            let node = doc.get_mut(id).expect("the layer");
            let NodeKind::Text(text) = &mut node.kind else {
                panic!("the layer is a text node");
            };
            text.characters = "abcdef".to_owned();
            text.link_text_style(0, 6, &TextStyle::default(), "t1");
        }
        let model = StylesModel::read(&doc, &[id]);
        assert_eq!(
            model.link(StyleSlot::Text),
            &StyleValue::Set("t1".to_owned()),
            "every character links to the same style"
        );
    }

    // ── The commands ─────────────────────────────────────────────────────────

    #[test]
    fn a_command_for_a_change_carries_the_engines_own_id_and_params() {
        // The engine's command ids and the engine's parameter names.
        let (id, params) = StylesAction::Apply {
            ids: vec![NodeId(7)],
            style: "s1".to_owned(),
            library: None,
            target: Some("fill"),
        }
        .command();
        assert_eq!(id, "style.apply");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["style"], json!("s1"));
        assert_eq!(params["target"], json!("fill"));

        let (id, params) = StylesAction::Detach {
            ids: vec![NodeId(7)],
            which: "effect",
        }
        .command();
        assert_eq!(id, "style.detach");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["which"], json!("effect"));

        let (id, params) = StylesAction::Create {
            ids: vec![NodeId(7)],
            slot: StyleSlot::Text,
            name: "Text 1".to_owned(),
        }
        .command();
        assert_eq!(id, "style.create");
        assert_eq!(params["type"], json!("text"));
        assert_eq!(params["name"], json!("Text 1"));
        assert_eq!(params["from"], json!(7));

        let (id, params) = StylesAction::Rename {
            style: "s1".to_owned(),
            name: "Brand/Blue/600".to_owned(),
        }
        .command();
        assert_eq!(id, "style.update");
        assert_eq!(params["style"], json!("s1"));
        assert_eq!(params["name"], json!("Brand/Blue/600"));

        let (id, params) = StylesAction::Describe {
            style: "s1".to_owned(),
            description: "The brand blue".to_owned(),
        }
        .command();
        assert_eq!(id, "style.update");
        assert_eq!(params["description"], json!("The brand blue"));

        let (id, params) = StylesAction::UpdateFromSelection {
            style: "s1".to_owned(),
            from: NodeId(3),
        }
        .command();
        assert_eq!(id, "style.update");
        assert_eq!(params["from"], json!(3));

        let (id, params) = StylesAction::Delete {
            style: "s1".to_owned(),
        }
        .command();
        assert_eq!(id, "style.delete");
        assert_eq!(params["style"], json!("s1"));

        let (id, params) = StylesAction::Unbind {
            style: "s1".to_owned(),
            field: "fills/0/color".to_owned(),
        }
        .command();
        assert_eq!(id, "style.bind");
        assert_eq!(params["field"], json!("fills/0/color"));

        let (id, params) = StylesAction::BindVariable {
            ids: vec![NodeId(7)],
            which: "fill",
            variable: "v1".to_owned(),
        }
        .command();
        assert_eq!(id, "variable.bind");
        assert_eq!(params["field"], json!("fill"));
        assert_eq!(params["variable"], json!("v1"));

        // A library's style is applied with the library's own command.
        let (id, params) = StylesAction::Apply {
            ids: vec![NodeId(7)],
            style: "k1".to_owned(),
            library: Some("lib1".to_owned()),
            target: Some("stroke"),
        }
        .command();
        assert_eq!(id, "library.applyStyle");
        assert_eq!(params["library"], json!("lib1"));
        assert_eq!(params["style"], json!("k1"));
        assert_eq!(params["target"], json!("stroke"));
    }

    #[test]
    fn a_text_style_is_written_with_the_engines_own_spelling() {
        // The style is the engine's own `TextStyle`, serialized by the
        // engine, so the panel cannot spell a field the engine does not have.
        let (id, params) = StylesAction::SetText {
            style: "s1".to_owned(),
            value: TextStyle {
                font_family: "Inter".to_owned(),
                font_style: "Semi Bold".to_owned(),
                font_size: 24.0,
                ..TextStyle::default()
            },
        }
        .command();
        assert_eq!(id, "style.update");
        assert_eq!(params["style"], json!("s1"));
        let value = &params["value"];
        assert_eq!(value["font_family"], json!("Inter"));
        assert_eq!(value["font_style"], json!("Semi Bold"));
        assert_eq!(value["font_size"], json!(24.0));
        assert_eq!(value["line_height"], json!({"unit": "auto"}));
    }

    #[test]
    fn the_engine_accepts_the_commands_the_panel_builds() {
        // The write path, end to end: the panel's command, the engine's
        // command, the engine's document.
        let (doc, id) = document_with(vec![paint_style("s1", "Brand/Blue/500")]);
        let mut session = Session::with_document(doc);
        session
            .execute("select.set", &json!({ "ids": [id.0] }))
            .expect("the layer is selected");

        let (command, params) = StylesAction::Apply {
            ids: vec![id],
            style: "s1".to_owned(),
            library: None,
            target: Some("fill"),
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the style is applied");

        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(
            node.styles.fill.as_deref(),
            Some("s1"),
            "the layer links to the style"
        );
        assert_eq!(node.fills.len(), 1, "and carries the style's paint");

        // Detaching unlinks it and keeps the values.
        let (command, params) = StylesAction::Detach {
            ids: vec![id],
            which: "fill",
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the style is detached");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(node.styles.fill, None, "the link is gone");
        assert_eq!(node.fills.len(), 1, "the layer keeps its values");
    }

    #[test]
    fn the_engine_accepts_a_style_created_from_the_selection() {
        let (doc, id) = document_with(Vec::new());
        let mut session = Session::with_document(doc);
        session
            .execute("select.set", &json!({ "ids": [id.0] }))
            .expect("the layer is selected");
        let (command, params) = StylesAction::Create {
            ids: vec![id],
            slot: StyleSlot::Fill,
            name: "Color 1".to_owned(),
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the style is created");
        let doc = session.doc().expect("a document");
        assert_eq!(doc.styles.len(), 1, "the document holds the style");
        assert_eq!(doc.styles[0].name, "Color 1");
        let node = doc.node(id).expect("the layer");
        assert_eq!(
            node.styles.fill.as_deref(),
            Some(doc.styles[0].id.as_str()),
            "and the layer it was made from links to it"
        );
    }

    // ── The panel's own state ────────────────────────────────────────────────

    #[test]
    fn the_search_follows_what_has_been_typed() {
        let mut state = StylesPanelState::new();
        assert!(!state.is_busy(), "nothing is being typed");
        state.draft = Some(StyleDraft {
            field: StyleField::Search,
            text: String::new(),
            select_all: true,
            style: None,
            base: None,
        });
        assert!(state.is_busy(), "a field is open");
        state.handle_key("b");
        state.handle_key("r");
        assert_eq!(state.filter(), "br", "the list follows the query");
        assert!(
            state.handle_key("enter").is_none(),
            "the search is not an edit"
        );
        assert_eq!(state.query, "br", "and it is committed");
        assert!(!state.is_busy(), "the field closed");
    }

    #[test]
    fn typing_a_name_commits_it_and_typing_a_nothing_does_not() {
        let mut state = StylesPanelState::new();
        state.draft = Some(StyleDraft {
            field: StyleField::Name,
            text: "Brand/Blue/600".to_owned(),
            select_all: true,
            style: Some("s1".to_owned()),
            base: None,
        });
        let action = state
            .handle_key("enter")
            .expect("the typed name is a change");
        assert_eq!(
            action,
            StylesAction::Rename {
                style: "s1".to_owned(),
                name: "Brand/Blue/600".to_owned()
            }
        );

        // An empty name is not a name: the engine would refuse it.
        let mut state = StylesPanelState::new();
        state.draft = Some(StyleDraft {
            field: StyleField::Name,
            text: String::new(),
            select_all: true,
            style: Some("s1".to_owned()),
            base: None,
        });
        assert!(state.handle_key("enter").is_none());
    }

    #[test]
    fn typing_a_size_commits_it_and_typing_a_nonsense_size_does_not() {
        let mut state = StylesPanelState::new();
        state.draft = Some(StyleDraft {
            field: StyleField::FontSize,
            text: "24".to_owned(),
            select_all: true,
            style: Some("s1".to_owned()),
            base: Some(TextStyle::default()),
        });
        let action = state
            .handle_key("enter")
            .expect("the typed size is a change");
        match action {
            StylesAction::SetText { style, value } => {
                assert_eq!(style, "s1");
                assert_eq!(value.font_size, 24.0);
            }
            other => panic!("expected a text style, got {other:?}"),
        }

        let mut state = StylesPanelState::new();
        state.draft = Some(StyleDraft {
            field: StyleField::FontSize,
            text: "big".to_owned(),
            select_all: true,
            style: Some("s1".to_owned()),
            base: Some(TextStyle::default()),
        });
        assert!(
            state.handle_key("enter").is_none(),
            "a size that is not a number commits nothing"
        );
    }

    #[test]
    fn the_first_key_replaces_the_text_that_was_there() {
        let mut state = StylesPanelState::new();
        state.draft = Some(StyleDraft {
            field: StyleField::Description,
            text: "The body text".to_owned(),
            select_all: true,
            style: Some("s1".to_owned()),
            base: None,
        });
        state.handle_key("x");
        let action = state
            .handle_key("enter")
            .expect("the description is a change");
        assert_eq!(
            action,
            StylesAction::Describe {
                style: "s1".to_owned(),
                description: "x".to_owned()
            }
        );
    }

    // ── The panel, in a window ───────────────────────────────────────────────

    /// A host for the section in a window: it draws the section, remembers
    /// what the section asked for, and does what it is asked.
    struct Host {
        model: StylesModel,
        state: Rc<RefCell<StylesPanelState>>,
        asked: Rc<RefCell<Vec<StylesAction>>>,
    }

    impl Host {
        fn new(model: StylesModel) -> Self {
            Self {
                model,
                state: Rc::new(RefCell::new(StylesPanelState::new())),
                asked: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl StylesHost for Host {
        fn run_styles_action(&mut self, action: StylesAction, cx: &mut Context<Self>) {
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
            styles_section(&model, &state, cx)
        }
    }

    /// Open a window with the section in it, over the given model.
    fn open_section(
        cx: &mut gpui::TestAppContext,
        model: StylesModel,
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
    fn the_section_draws_the_style_a_node_is_bound_to(cx: &mut gpui::TestAppContext) {
        // The render test: the section, on its own, in a real window, showing
        // the style the layer is bound to.
        let (mut doc, id) = document_with(vec![paint_style("s1", "Brand/Blue/500")]);
        doc.get_mut(id).expect("the layer").styles.fill = Some("s1".to_owned());
        let model = StylesModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        // The chip carries the style's name, and it is laid out with a real
        // size.
        let selector: &'static str = Box::leak(
            "styles-slot-fill-value-Brand/Blue/500"
                .to_owned()
                .into_boxed_str(),
        );
        let chip = bounds(window, selector);
        assert!(
            chip.size.height > px(0.0),
            "the style's name is on screen: {chip:?}"
        );
        assert!(
            chip.size.width > px(0.0),
            "the style's name is on screen: {chip:?}"
        );
        // The other slots say that nothing is set, rather than saying nothing.
        let selector: &'static str = Box::leak(
            "styles-slot-stroke-value-Not set"
                .to_owned()
                .into_boxed_str(),
        );
        assert!(
            window.debug_bounds(selector).is_some(),
            "an unset slot says so"
        );
        // And the way to unlink the style is there.
        assert!(
            window.debug_bounds("styles-slot-fill-detach").is_some(),
            "the chip offers the way to detach the style"
        );
    }

    #[gpui::test]
    fn the_section_shows_the_empty_state_when_there_are_no_styles(cx: &mut gpui::TestAppContext) {
        // figcraft's empty state, word for word.
        let (doc, id) = document_with(Vec::new());
        let model = StylesModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        // Open the browser for the fill slot.
        let chip = bounds(window, "styles-slot-fill");
        window.simulate_click(chip.center(), gpui::Modifiers::none());
        window.run_until_parked();

        let selector: &'static str = Box::leak(
            "styles-empty-No styles yet. Click + to create one from the selection."
                .to_owned()
                .into_boxed_str(),
        );
        assert!(
            window.debug_bounds(selector).is_some(),
            "the section says figcraft's own words"
        );
        assert!(
            window.debug_bounds("styles-picker-create").is_some(),
            "and offers the way to create one"
        );
    }

    #[gpui::test]
    fn clicking_a_style_asks_the_engine_to_apply_it(cx: &mut gpui::TestAppContext) {
        let (doc, id) = document_with(vec![paint_style("s1", "Brand/Blue/500")]);
        let model = StylesModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        // Open the browser for the fill slot.
        let chip = bounds(window, "styles-slot-fill");
        window.simulate_click(chip.center(), gpui::Modifiers::none());
        window.run_until_parked();

        // The style is in the list.
        let row = bounds(window, "styles-row-s1");
        window.simulate_click(row.center(), gpui::Modifiers::none());
        window.run_until_parked();

        let asked = root.read_with(cx, |host, _| host.asked.borrow().clone());
        assert_eq!(asked.len(), 1, "the section asked for one change");
        match &asked[0] {
            StylesAction::Apply {
                style,
                library,
                target,
                ..
            } => {
                assert_eq!(style, "s1");
                assert!(
                    library.is_none(),
                    "a local style is applied with style.apply"
                );
                assert_eq!(*target, Some("fill"));
            }
            other => panic!("expected an apply, got {other:?}"),
        }
        // And the command it would run is the engine's own.
        let (command, params) = asked[0].command();
        assert_eq!(command, "style.apply");
        assert_eq!(params["ids"], json!([id.0]));
        assert_eq!(params["style"], json!("s1"));
        assert_eq!(params["target"], json!("fill"));
    }

    #[gpui::test]
    fn a_text_style_is_shown_in_its_own_size(cx: &mut gpui::TestAppContext) {
        let (doc, id) = document_with(vec![text_style("t1", "Body/Regular")]);
        let model = StylesModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        // Open the browser for the text slot.
        let chip = bounds(window, "styles-slot-text");
        window.simulate_click(chip.center(), gpui::Modifiers::none());
        window.run_until_parked();

        let row = bounds(window, "styles-row-t1");
        assert!(
            row.size.height > px(0.0),
            "the text style is on screen: {row:?}"
        );
        // The meta says the size and the line height, as figcraft's does.
        let selector: &'static str = Box::leak("styles-row-t1-16/Auto".to_owned().into_boxed_str());
        assert!(
            window.debug_bounds(selector).is_some(),
            "the style's size and line height are on screen"
        );
    }
}
