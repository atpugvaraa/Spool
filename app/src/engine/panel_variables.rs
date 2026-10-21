//! The Variables section: figcraft's variables editor, ported to GPUI.
//!
//! # What this module owns
//!
//! Two things, in one file because they are one thing:
//!
//! 1. **The model.** [`VariablesModel`] reads the engine's [`Document`] and turns
//!    the document's variable collections, their modes and their variables, and
//!    the selected layers' variable bindings and modes, into what the panel
//!    shows. [`VariablesAction`] turns a change into the engine's own command.
//! 2. **The panel.** [`variables_section`] draws that model and asks the engine
//!    for the changes the user makes.
//!
//! # The reference
//!
//! figcraft's variables editor — `crates/ui-egui/src/variables_panel.rs` — is
//! the specification. Its collections, its variables, its modes, its rows, its
//! labels, its actions, its empty state and its enable/disable behaviour are
//! mirrored here. The engine's own model is the arbiter:
//! [`figcraft_doc::Collection`], [`figcraft_doc::Variable`] and
//! [`figcraft_doc::VariableValue`] say what a variable is, and the engine's
//! commands say what a change is.
//!
//! # The source boundary
//!
//! **Spool's source can express a variable.** A variable is an Owner B fact —
//! the design-tool fact HTML and CSS cannot state — and `lamine.yaml` v2 has
//! the place for it:
//!
//! | what | where in `lamine.yaml` |
//! |---|---|
//! | a variable | the document-level `variables:` list |
//! | a collection and its modes | the document-level `collections:` list |
//! | a layer's binding | the node record's `bound:` map (`fills/0/color` → id) |
//! | a layer's mode for a collection | the node record's `var_modes:` map |
//!
//! The compile path reads all four into the engine's document
//! (`spool_source::reader`), so a project that carries them opens with its
//! variables, its modes and its bindings intact.
//!
//! **The save path does not write them back yet.** It writes the editor's own
//! version 1 metadata, which has no variables section, and it leaves a version 2
//! file alone rather than re-encoding it. So a variable made or edited here
//! stays in the editor's document. That is said where the change is made, rather
//! than left for the user to find out by saving — the same note the Styles
//! section carries for shared styles.
//!
//! # Where this differs from figcraft, and why
//!
//! - **One column, not a window.** figcraft opens the editor as an 880×480
//!   window with a sidebar and a table. Spool's Inspector is a narrow column of
//!   sections, so the collections are a list at the top and the table follows.
//! - **The value cell names its mode.** figcraft's table has one column per
//!   mode; a narrow column cannot hold columns, so each value says which mode
//!   it is the value in. The modes themselves are a strip above the variables,
//!   where the header row's mode names were.
//! - **The layers' own variables are here too.** figcraft keeps the bindings in
//!   its right panel and the editor is document-only. Spool's Inspector is about
//!   the selected layers, so the section also shows what the selection binds
//!   and which mode it is in — with the Inspector's three states, folded per
//!   property.
//! - **The edit button is always shown.** figcraft reveals it on hover; a
//!   hover-only control cannot be reached without a pointer.
//! - **A typed field commits on Enter.** figcraft commits a typed name and
//!   description when the browser closes; Spool's Inspector commits every typed
//!   field on Enter and abandons it on Escape, like every other field.
//!
//! # Commands, not mutations
//!
//! Every change leaves through the engine's command surface:
//! `variable.createCollection`, `variable.renameCollection`,
//! `variable.deleteCollection`, `variable.addMode`, `variable.renameMode`,
//! `variable.removeMode`, `variable.moveMode`, `variable.create`,
//! `variable.set`, `variable.update`, `variable.duplicate`, `variable.delete`,
//! `variable.resetOverride`, `variable.bind`, `variable.setMode` and
//! `command.batch` for a group's rename and delete. Nothing here writes to a
//! [`Document`] directly, so an edit made from this panel, from the CLI and
//! from an agent are the same edit and land in the same history.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use figcraft_doc::color::Color;
use figcraft_doc::{Document, Node, NodeId, Variable, VariableType, VariableValue};
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

/// The word for a layer that follows its parent for a collection's mode.
///
/// The engine's own word (`variable.setMode {mode: null}` is Auto), and the
/// Inspector's: the gap is made up by the parent, so the row says so rather
/// than saying nothing is set.
pub const AUTO: &str = "Auto";

/// figcraft's own sentence, word for word, for a document with no collections.
///
/// The variables editor explains itself here rather than showing an empty
/// table: a panel that only said "no variables" would not say what a variable
/// is for.
pub const NO_COLLECTIONS: &str = "Variables store reusable values (colours, numbers, strings, \
                                  booleans) with modes such as Light and Dark.";

/// figcraft's own words, word for word, for a search that finds nothing.
pub const NO_MATCHING_VARIABLES: &str = "No matching variables";

/// What the save path does with a variable today.
///
/// The metadata can carry one — `lamine.yaml` has a `variables:` list, a
/// `collections:` list and a `bound:` map on every node record — and the
/// compile path reads all of them. The save path does not write them back: it
/// writes the editor's own version 1 metadata, and it leaves a version 2 file
/// alone rather than re-encoding it. Said where the change is made, rather
/// than left for the user to find out by saving.
const SAVE_PATH_NOTE: &str =
    "Variables are not written back to lamine.yaml yet, so a variable made or edited here stays \
     in the editor's document.";

/// The word for a variable that is not there any more.
///
/// figcraft's word: a binding to a variable that has gone is not "Not set" —
/// the layer does bind to something, and that something cannot be found.
pub const MISSING_VARIABLE: &str = "Missing variable";

/// The variable types, in the order figcraft offers them.
///
/// The engine's own four, in the engine's own order: colour, number, string,
/// boolean.
pub const VARIABLE_TYPES: [VariableType; 4] = [
    VariableType::Color,
    VariableType::Number,
    VariableType::String,
    VariableType::Boolean,
];

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
/// Named `VariableState` and not `VariableValue`, because the engine's own
/// [`VariableValue`] is the value a variable holds, and the two are different
/// things.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum VariableState<T> {
    /// Every selected layer has this value, and they agree on it.
    Set(T),
    /// No selected layer has this value.
    #[default]
    Unset,
    /// The selected layers disagree.
    Mixed,
}

impl<T: PartialEq> VariableState<T> {
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

impl<T: Clone + PartialEq> VariableState<T> {
    /// Fold readings that are all present.
    ///
    /// A layer that follows its parent for a mode has a reading — "Auto" — and
    /// that reading is an answer, not a missing one. So this fold is over
    /// values, and an empty selection is the only thing that is `Unset`.
    pub fn across_present(readings: impl IntoIterator<Item = T>) -> Self {
        let mut readings = readings.into_iter();
        let Some(first) = readings.next() else {
            return Self::Unset;
        };
        let state = Self::Set(first.clone());
        for next in readings {
            if next != first {
                return Self::Mixed;
            }
        }
        state
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The model
// ═══════════════════════════════════════════════════════════════════════════

/// One mode of a collection.
#[derive(Clone, Debug, PartialEq)]
pub struct VariableMode {
    /// The engine's id for the mode.
    pub id: String,
    /// The mode's name: "Light", "Dark".
    pub name: String,
}

/// One variable.
#[derive(Clone, Debug, PartialEq)]
pub struct VariableEntry {
    /// The engine's id for the variable.
    pub id: String,
    /// The variable's name, groups and all: "color/brand/blue".
    pub name: String,
    /// The collection the variable is in.
    pub collection: String,
    /// What the variable holds.
    pub kind: VariableType,
    /// The variable's value in each mode of its collection, by the mode's id.
    ///
    /// The engine's own value, so the panel cannot spell a value the engine
    /// does not have.
    pub values: BTreeMap<String, VariableValue>,
    /// The value each mode resolves to, following an alias.
    ///
    /// The engine's own resolution: an alias points at another variable, and
    /// the value the layer shows is the one that variable holds.
    pub resolved: BTreeMap<String, VariableValue>,
    /// The variable's description.
    pub description: String,
    /// The properties pickers offer the variable for.
    ///
    /// `None` is every supported property, an empty list is none of them.
    pub scopes: Option<Vec<String>>,
    /// Whether the variable is left out when the file is published.
    pub hidden_from_publishing: bool,
    /// The variable it inherits, in an extended collection.
    pub extends: Option<String>,
    /// The modes whose value it overrides.
    pub overrides: BTreeSet<String>,
    /// The name code uses for the variable per platform.
    pub code_syntax: BTreeMap<String, String>,
}

impl VariableEntry {
    /// The last part of the variable's name ("color/brand/blue" → "blue").
    ///
    /// The short name, the way figcraft's table shows it under its group's
    /// header.
    pub fn leaf(&self) -> &str {
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }

    /// The group the variable is in ("color/brand/blue" → "color/brand").
    pub fn group(&self) -> &str {
        group_of(&self.name)
    }

    /// The variable's value in one mode, if it has one.
    pub fn value_in(&self, mode: &str) -> Option<&VariableValue> {
        self.values.get(mode)
    }

    /// Whether the variable inherits another one.
    ///
    /// An inherited variable is named and deleted in the collection it
    /// extends, so the panel does not offer to do either.
    pub fn inherited(&self) -> bool {
        self.extends.is_some()
    }

    /// Whether the variable overrides the inherited value in one mode.
    pub fn overridden(&self, mode: &str) -> bool {
        self.overrides.contains(mode)
    }
}

/// One collection.
#[derive(Clone, Debug, PartialEq)]
pub struct VariableCollection {
    /// The engine's id for the collection.
    pub id: String,
    /// The collection's name.
    pub name: String,
    /// The collection's modes, in the engine's order. The first is the default.
    pub modes: Vec<VariableMode>,
    /// The collection it extends, by name.
    pub extends: Option<String>,
    /// The collection's variables, in the engine's order.
    pub variables: Vec<VariableEntry>,
}

impl VariableCollection {
    /// The collection's default mode: the first one.
    pub fn default_mode(&self) -> Option<&VariableMode> {
        self.modes.first()
    }

    /// The mode with this id.
    pub fn mode(&self, id: &str) -> Option<&VariableMode> {
        self.modes.iter().find(|mode| mode.id == id)
    }

    /// Whether the collection's modes can be changed here.
    ///
    /// An extended collection has the modes of the collection it extends, and
    /// the engine refuses a mode edit on one.
    pub fn modes_editable(&self) -> bool {
        self.extends.is_none()
    }
}

/// One group of variables: a name's path.
#[derive(Clone, Debug, PartialEq)]
pub struct VariableGroup {
    /// The group's path: "color/brand".
    pub path: String,
    /// The group's label, as the panel shows it: the last part of the path.
    pub label: String,
    /// How deep the group sits, for the indent.
    pub depth: usize,
    /// How many variables are in the group.
    pub count: usize,
}

/// The mode a collection is set to on the selection.
#[derive(Clone, Debug, PartialEq)]
pub enum ModeChoice {
    /// The layer follows its parent.
    Auto,
    /// The layer is set to this mode.
    Mode(String),
}

impl ModeChoice {
    /// The mode's name, as the panel shows it.
    pub fn label(&self, model: &VariablesModel) -> String {
        match self {
            Self::Auto => AUTO.to_owned(),
            Self::Mode(id) => model.mode_name(id).unwrap_or_else(|| id.clone()),
        }
    }
}

/// One field the selection binds a variable to.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundVariable {
    /// The engine's own field name: "fills/0/color", "width".
    pub field: String,
    /// The variable bound to it, across the selection.
    pub value: VariableState<String>,
}

/// The mode one collection is set to on the selection.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectionMode {
    /// The collection's id.
    pub collection: String,
    /// The mode the selection is in, across the selection.
    pub value: VariableState<ModeChoice>,
}

/// One field a variable can be bound to.
#[derive(Clone, Debug, PartialEq)]
pub struct BindableField {
    /// The engine's own field name.
    pub field: String,
    /// The kinds of variable the field takes, by the engine's own rules.
    pub kinds: Vec<VariableType>,
}

impl BindableField {
    /// Whether a variable of this kind can be bound to the field.
    pub fn takes(&self, kind: VariableType) -> bool {
        self.kinds.contains(&kind)
    }
}

/// What the Variables section shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VariablesModel {
    /// The layers the model was read for.
    pub ids: Vec<NodeId>,
    /// The document's own collections, in the engine's order.
    pub collections: Vec<VariableCollection>,
    /// The fields the selection binds, and what it binds them to.
    pub bindings: Vec<BoundVariable>,
    /// The mode each collection is set to on the selection.
    pub modes: Vec<CollectionMode>,
    /// The fields a variable can be bound to on the selection.
    pub fields: Vec<BindableField>,
}

impl VariablesModel {
    /// The collections, the variables and the selected layers' own variables.
    ///
    /// A layer the document does not hold is skipped rather than shown as an
    /// empty one, and a selection with nothing selected binds nothing.
    pub fn read(doc: &Document, selection: &[NodeId]) -> Self {
        let nodes: Vec<&Node> = selection.iter().filter_map(|id| doc.node(*id)).collect();
        let collections = read_collections(doc);
        let bindings = read_bindings(&nodes);
        let modes = read_modes(&collections, &nodes);
        let fields = read_fields(doc, &nodes);
        Self {
            ids: selection.to_vec(),
            collections,
            bindings,
            modes,
            fields,
        }
    }

    /// One collection, by its id.
    pub fn collection(&self, id: &str) -> Option<&VariableCollection> {
        self.collections
            .iter()
            .find(|collection| collection.id == id)
    }

    /// One variable, by its id.
    pub fn variable(&self, id: &str) -> Option<&VariableEntry> {
        self.collections
            .iter()
            .flat_map(|collection| collection.variables.iter())
            .find(|variable| variable.id == id)
    }

    /// A mode's name, by its id.
    pub fn mode_name(&self, id: &str) -> Option<String> {
        self.collections
            .iter()
            .flat_map(|collection| collection.modes.iter())
            .find(|mode| mode.id == id)
            .map(|mode| mode.name.clone())
    }

    /// The binding on one field, if the selection has one.
    pub fn binding(&self, field: &str) -> Option<&BoundVariable> {
        self.bindings.iter().find(|binding| binding.field == field)
    }

    /// The mode one collection is set to on the selection.
    pub fn collection_mode(&self, collection: &str) -> Option<&CollectionMode> {
        self.modes.iter().find(|mode| mode.collection == collection)
    }

    /// The variables of one collection, in the engine's order.
    pub fn variables_in(&self, collection: &str) -> Vec<&VariableEntry> {
        self.collection(collection)
            .map(|collection| collection.variables.iter().collect())
            .unwrap_or_default()
    }

    /// The variables of one kind, in the engine's order.
    ///
    /// What an alias can point at: an alias names a variable of the same type.
    pub fn variables_of_kind(&self, kind: VariableType) -> Vec<&VariableEntry> {
        self.collections
            .iter()
            .flat_map(|collection| collection.variables.iter())
            .filter(|variable| variable.kind == kind)
            .collect()
    }

    /// The groups of one collection, with their counts.
    ///
    /// Every group path in the collection's names with its ancestors
    /// ("a/b/c" → "a", "a/b"), sorted, and the count of the variables under
    /// each. figcraft's own grouping, from its own `groups`.
    pub fn groups(&self, collection: &str) -> Vec<VariableGroup> {
        let variables = self.variables_in(collection);
        let names: Vec<&str> = variables
            .iter()
            .map(|variable| variable.name.as_str())
            .collect();
        let mut paths: Vec<String> = Vec::new();
        for name in &names {
            let mut path = group_of(name);
            while !path.is_empty() {
                if !paths.iter().any(|existing| existing == path) {
                    paths.push(path.to_owned());
                }
                path = group_of(path);
            }
        }
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let depth = path.matches('/').count();
                let count = names
                    .iter()
                    .filter(|name| name.starts_with(&format!("{path}/")))
                    .count();
                VariableGroup {
                    label: path.rsplit('/').next().unwrap_or(&path).to_owned(),
                    path,
                    depth,
                    count,
                }
            })
            .collect()
    }

    /// The variables of one collection the table shows.
    ///
    /// The group filter and the search, in figcraft's order: the group's
    /// variables first, then the ones whose name holds the query in lower case.
    /// The list is sorted by group, so the groups come out in one run.
    pub fn visible_variables(
        &self,
        collection: &str,
        group: Option<&str>,
        query: &str,
    ) -> Vec<&VariableEntry> {
        let query = query.trim().to_lowercase();
        let mut list: Vec<&VariableEntry> = self
            .variables_in(collection)
            .into_iter()
            .filter(|variable| match group {
                Some(group) => variable.name.starts_with(&format!("{group}/")),
                None => true,
            })
            .filter(|variable| query.is_empty() || variable.name.to_lowercase().contains(&query))
            .collect();
        list.sort_by(|a, b| a.group().cmp(b.group()));
        list
    }

    /// The name a new collection gets: the first "Collection" no collection
    /// already answers to.
    pub fn next_collection_name(&self) -> String {
        unique_name("Collection", |name| {
            self.collections
                .iter()
                .any(|collection| collection.name == name)
        })
    }

    /// The name a new variable of this kind gets.
    ///
    /// The kind's own name, the way figcraft names it: a new colour variable is
    /// "Color", a new number variable is "Number".
    pub fn next_variable_name(&self, collection: &str, kind: VariableType) -> String {
        let base = kind_label(kind);
        unique_name(base, |name| {
            self.variables_in(collection)
                .iter()
                .any(|variable| variable.name == name)
        })
    }

    /// The name a new mode gets: the first "Mode" no mode already answers to.
    pub fn next_mode_name(&self, collection: &str) -> String {
        let Some(collection) = self.collection(collection) else {
            return "Mode".to_owned();
        };
        unique_name("Mode", |name| {
            collection.modes.iter().any(|mode| mode.name == name)
        })
    }
}

/// The document's own collections, with their variables.
fn read_collections(doc: &Document) -> Vec<VariableCollection> {
    doc.variables
        .collections
        .iter()
        // A copy of a library's collection is the library's, and figcraft shows
        // it in the library's own list rather than in this file's.
        .filter(|collection| doc.libraries.collection_import(&collection.id).is_none())
        .map(|collection| VariableCollection {
            id: collection.id.clone(),
            name: collection.name.clone(),
            modes: collection
                .modes
                .iter()
                .map(|mode| VariableMode {
                    id: mode.id.clone(),
                    name: mode.name.clone(),
                })
                .collect(),
            extends: collection
                .extends
                .as_ref()
                .and_then(|parent| doc.variables.collection(parent))
                .map(|parent| parent.name.clone()),
            variables: doc
                .variables
                .variables
                .iter()
                .filter(|variable| variable.collection == collection.id)
                .filter(|variable| doc.libraries.variable_import(&variable.id).is_none())
                .map(|variable| read_variable(doc, variable))
                .collect(),
        })
        .collect()
}

/// One variable, as the panel shows it.
fn read_variable(doc: &Document, variable: &Variable) -> VariableEntry {
    let mut resolved = BTreeMap::new();
    for mode in variable.values.keys() {
        let modes = BTreeMap::from([(variable.collection.clone(), mode.clone())]);
        let value = doc
            .variables
            .resolve(&variable.id, &modes)
            .or_else(|| variable.values.get(mode).cloned());
        if let Some(value) = value {
            resolved.insert(mode.clone(), value);
        }
    }
    VariableEntry {
        id: variable.id.clone(),
        name: variable.name.clone(),
        collection: variable.collection.clone(),
        kind: variable.kind,
        values: variable.values.clone(),
        resolved,
        description: variable.description.clone(),
        scopes: variable.scopes.clone(),
        hidden_from_publishing: variable.hidden_from_publishing,
        extends: variable.extends.clone(),
        overrides: variable.overrides.clone(),
        code_syntax: variable.code_syntax.clone(),
    }
}

/// The fields the selection binds, folded per field.
fn read_bindings(nodes: &[&Node]) -> Vec<BoundVariable> {
    let mut fields: BTreeSet<&str> = BTreeSet::new();
    for node in nodes {
        fields.extend(node.bound.keys().map(String::as_str));
    }
    fields
        .into_iter()
        .map(|field| BoundVariable {
            field: field.to_owned(),
            value: VariableState::across(nodes.iter().map(|node| node.bound.get(field).cloned())),
        })
        .collect()
}

/// The mode each collection is set to on the selection.
fn read_modes(collections: &[VariableCollection], nodes: &[&Node]) -> Vec<CollectionMode> {
    collections
        .iter()
        .map(|collection| CollectionMode {
            collection: collection.id.clone(),
            value: VariableState::across_present(nodes.iter().map(|node| {
                node.var_modes
                    .get(&collection.id)
                    .map(|mode| ModeChoice::Mode(mode.clone()))
                    .unwrap_or(ModeChoice::Auto)
            })),
        })
        .collect()
}

/// The fields a variable can be bound to on the selection.
///
/// The engine's own rules: `field_takes` says whether a property takes a
/// variable of a kind, and the list fields are the ones the selection
/// actually has. A kind is offered only when *every* selected layer takes it,
/// because the engine refuses a bind that one of them does not accept.
fn read_fields(doc: &Document, nodes: &[&Node]) -> Vec<BindableField> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(node) = nodes.first() {
        candidates.extend(scalar_fields());
        candidates.extend(list_fields(node));
    }
    candidates
        .into_iter()
        .map(|field| BindableField {
            kinds: VARIABLE_TYPES
                .into_iter()
                .filter(|kind| {
                    nodes.iter().all(|node| {
                        figcraft_engine::cmd::variables::field_takes(doc, node.id, &field, *kind)
                    })
                })
                .collect(),
            field,
        })
        .filter(|field| !field.kinds.is_empty())
        .collect()
}

/// The properties the engine's own rules name a variable for.
fn scalar_fields() -> Vec<String> {
    [
        "width",
        "height",
        "opacity",
        "cornerRadius",
        "gap",
        "paddingHorizontal",
        "characters",
        "visible",
        "fontSize",
        "fontWeight",
        "lineHeight",
        "letterSpacing",
        "fontFamily",
        "fontStyle",
        "paragraphSpacing",
        "paragraphIndent",
    ]
    .iter()
    .map(|field| (*field).to_owned())
    .collect()
}

/// The list fields the layer has, in the engine's own spelling.
///
/// The engine's lists are stored bottom first, so `fills/0/color` is the
/// bottom fill: the index here is the engine's index, not the panel's.
fn list_fields(node: &Node) -> Vec<String> {
    let mut fields = Vec::new();
    for (index, paint) in node.fills.iter().enumerate() {
        fields.extend(paint_fields("fills", index, paint));
    }
    for (index, paint) in node.strokes.iter().enumerate() {
        fields.extend(paint_fields("strokes", index, paint));
    }
    for (index, effect) in node.effects.iter().enumerate() {
        match effect.kind {
            figcraft_doc::EffectKind::DropShadow(_) | figcraft_doc::EffectKind::InnerShadow(_) => {
                fields.push(format!("effects/{index}/color"));
                fields.push(format!("effects/{index}/offsetX"));
                fields.push(format!("effects/{index}/offsetY"));
                fields.push(format!("effects/{index}/radius"));
                fields.push(format!("effects/{index}/spread"));
            }
            figcraft_doc::EffectKind::LayerBlur(_)
            | figcraft_doc::EffectKind::BackgroundBlur(_) => {
                fields.push(format!("effects/{index}/radius"));
                fields.push(format!("effects/{index}/startRadius"));
            }
            _ => {}
        }
    }
    for (index, _grid) in node.grids.iter().enumerate() {
        fields.push(format!("layoutGrids/{index}/size"));
        fields.push(format!("layoutGrids/{index}/count"));
        fields.push(format!("layoutGrids/{index}/gutter"));
        fields.push(format!("layoutGrids/{index}/offset"));
    }
    fields
}

/// The binding fields one paint has.
fn paint_fields(prefix: &str, index: usize, paint: &figcraft_doc::color::Paint) -> Vec<String> {
    match &paint.kind {
        figcraft_doc::color::PaintKind::Solid { .. } => {
            vec![format!("{prefix}/{index}/color")]
        }
        figcraft_doc::color::PaintKind::Gradient(gradient) => gradient
            .stops
            .iter()
            .enumerate()
            .map(|(stop, _)| format!("{prefix}/{index}/stops/{stop}/color"))
            .collect(),
        _ => Vec::new(),
    }
}

/// The group part of a variable's name ("color/brand" → "color"; "" when
/// ungrouped).
fn group_of(name: &str) -> &str {
    name.rsplit_once('/').map_or("", |(group, _)| group)
}

/// The first of `base`, `base 2`, `base 3`… that `taken` does not hold.
fn unique_name(base: &str, taken: impl Fn(&str) -> bool) -> String {
    (1..10_000)
        .map(|number| {
            if number == 1 {
                base.to_owned()
            } else {
                format!("{base} {number}")
            }
        })
        .find(|name| !taken(name))
        .unwrap_or_else(|| base.to_owned())
}

/// A variable type's name, as the panel shows it and as a new variable is
/// named.
///
/// The engine's own spelling for the command (`color`, `number`, `string`,
/// `boolean`) and figcraft's own label for the menu ("Color", "Number",
/// "String", "Boolean").
pub fn kind_label(kind: VariableType) -> &'static str {
    match kind {
        VariableType::Color => "Color",
        VariableType::Number => "Number",
        VariableType::String => "String",
        VariableType::Boolean => "Boolean",
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The commands
// ═══════════════════════════════════════════════════════════════════════════

/// A change the Variables section can ask for.
///
/// One variant per thing the section can change, so a change the engine has no
/// command for cannot be written down as though it had one.
#[derive(Clone, Debug, PartialEq)]
pub enum VariablesAction {
    /// Create a collection with one mode.
    CreateCollection {
        /// The collection's name.
        name: String,
    },
    /// Change a collection's name.
    RenameCollection {
        /// The collection.
        collection: String,
        /// Its new name.
        name: String,
    },
    /// Delete a collection, with the variables in it.
    DeleteCollection {
        /// The collection.
        collection: String,
    },
    /// Add a mode to a collection.
    AddMode {
        /// The collection.
        collection: String,
        /// The mode's name.
        name: String,
        /// The mode the new one copies, when it copies one.
        from: Option<String>,
    },
    /// Change a mode's name.
    RenameMode {
        /// The collection the mode is in.
        collection: String,
        /// The mode.
        mode: String,
        /// Its new name.
        name: String,
    },
    /// Delete a mode.
    RemoveMode {
        /// The collection the mode is in.
        collection: String,
        /// The mode.
        mode: String,
    },
    /// Move a mode to another column.
    MoveMode {
        /// The collection the mode is in.
        collection: String,
        /// The mode.
        mode: String,
        /// The column it goes to. Zero is the default mode.
        to: usize,
    },
    /// Create a variable.
    CreateVariable {
        /// The collection it goes in.
        collection: String,
        /// Its name.
        name: String,
        /// What it holds.
        kind: VariableType,
        /// Its value in the default mode.
        value: VariableValue,
    },
    /// Set a variable's value in one mode.
    SetValue {
        /// The variable.
        variable: String,
        /// The mode, by its id. `None` is the collection's default mode.
        mode: Option<String>,
        /// The value.
        value: VariableValue,
    },
    /// Delete a variable.
    DeleteVariable {
        /// The variable.
        variable: String,
    },
    /// Duplicate a variable.
    DuplicateVariable {
        /// The variable.
        variable: String,
    },
    /// Change a variable's name.
    RenameVariable {
        /// The variable.
        variable: String,
        /// Its new name.
        name: String,
    },
    /// Change a variable's description.
    DescribeVariable {
        /// The variable.
        variable: String,
        /// Its new description.
        description: String,
    },
    /// Change the properties pickers offer a variable for.
    SetScopes {
        /// The variable.
        variable: String,
        /// The scope ids. `None` is every supported property.
        scopes: Option<Vec<String>>,
    },
    /// Leave a variable out of a published library, or not.
    SetHiddenFromPublishing {
        /// The variable.
        variable: String,
        /// Whether it is left out.
        hidden: bool,
    },
    /// Change the name code uses for a variable on one platform.
    SetCodeSyntax {
        /// The variable.
        variable: String,
        /// The platform: "WEB", "iOS" or "ANDROID".
        platform: String,
        /// The name code uses. Empty clears it.
        name: String,
    },
    /// Drop an override, so the value follows the inherited one again.
    ResetOverride {
        /// The variable.
        variable: String,
        /// The mode, by its id. `None` is every overridden mode.
        mode: Option<String>,
    },
    /// Bind a variable to a field of the selection, or unbind it.
    Bind {
        /// The layers to bind it on.
        ids: Vec<NodeId>,
        /// The engine's own field name.
        field: String,
        /// The variable's id. `None` unbinds.
        variable: Option<String>,
    },
    /// Set the mode a collection is in on the selection.
    SetMode {
        /// The layers to set it on.
        ids: Vec<NodeId>,
        /// The collection.
        collection: String,
        /// The mode, by its id. `None` is Auto.
        mode: Option<String>,
    },
    /// Rename a group: every variable under it moves with it.
    RenameGroup {
        /// The collection the group is in.
        collection: String,
        /// The group's path.
        group: String,
        /// The variables under it, with the names they get.
        renames: Vec<(String, String)>,
    },
    /// Delete a group: every variable under it goes.
    DeleteGroup {
        /// The collection the group is in.
        collection: String,
        /// The group's path.
        group: String,
        /// The variables under it.
        variables: Vec<String>,
    },
}

impl VariablesAction {
    /// The engine's command id and parameters for this change.
    ///
    /// The engine's own spellings, so a change made from the panel is the
    /// same change the CLI, the keymap and an agent would make.
    pub fn command(&self) -> (String, Value) {
        match self {
            Self::CreateCollection { name } => (
                "variable.createCollection".to_owned(),
                json!({ "name": name, "modes": ["Mode 1"] }),
            ),
            Self::RenameCollection { collection, name } => (
                "variable.renameCollection".to_owned(),
                json!({ "collection": collection, "name": name }),
            ),
            Self::DeleteCollection { collection } => (
                "variable.deleteCollection".to_owned(),
                json!({ "collection": collection }),
            ),
            Self::AddMode {
                collection,
                name,
                from,
            } => {
                let mut params = json!({ "collection": collection, "name": name });
                if let Some(from) = from {
                    params["from"] = json!(from);
                }
                ("variable.addMode".to_owned(), params)
            }
            Self::RenameMode {
                collection,
                mode,
                name,
            } => (
                "variable.renameMode".to_owned(),
                json!({ "collection": collection, "mode": mode, "name": name }),
            ),
            Self::RemoveMode { collection, mode } => (
                "variable.removeMode".to_owned(),
                json!({ "collection": collection, "mode": mode }),
            ),
            Self::MoveMode {
                collection,
                mode,
                to,
            } => (
                "variable.moveMode".to_owned(),
                json!({ "collection": collection, "mode": mode, "to": to }),
            ),
            Self::CreateVariable {
                collection,
                name,
                kind,
                value,
            } => (
                "variable.create".to_owned(),
                json!({
                    "collection": collection,
                    "name": name,
                    "type": kind.id(),
                    "value": value_json(value),
                }),
            ),
            Self::SetValue {
                variable,
                mode,
                value,
            } => {
                let mut params = json!({ "variable": variable, "value": value_json(value) });
                if let Some(mode) = mode {
                    params["mode"] = json!(mode);
                }
                ("variable.set".to_owned(), params)
            }
            Self::DeleteVariable { variable } => (
                "variable.delete".to_owned(),
                json!({ "variable": variable }),
            ),
            Self::DuplicateVariable { variable } => (
                "variable.duplicate".to_owned(),
                json!({ "variable": variable }),
            ),
            Self::RenameVariable { variable, name } => (
                "variable.update".to_owned(),
                json!({ "variable": variable, "name": name }),
            ),
            Self::DescribeVariable {
                variable,
                description,
            } => (
                "variable.update".to_owned(),
                json!({ "variable": variable, "description": description }),
            ),
            Self::SetScopes { variable, scopes } => {
                let mut params = json!({ "variable": variable });
                match scopes {
                    Some(scopes) => params["scopes"] = json!(scopes),
                    None => params["scopes"] = Value::Null,
                }
                ("variable.update".to_owned(), params)
            }
            Self::SetHiddenFromPublishing { variable, hidden } => (
                "variable.update".to_owned(),
                json!({ "variable": variable, "hiddenFromPublishing": hidden }),
            ),
            Self::SetCodeSyntax {
                variable,
                platform,
                name,
            } => (
                "variable.update".to_owned(),
                json!({ "variable": variable, "codeSyntax": { platform.clone(): name } }),
            ),
            Self::ResetOverride { variable, mode } => {
                let mut params = json!({ "variable": variable });
                if let Some(mode) = mode {
                    params["mode"] = json!(mode);
                }
                ("variable.resetOverride".to_owned(), params)
            }
            Self::Bind {
                ids,
                field,
                variable,
            } => {
                let mut params = json!({ "field": field });
                if !ids.is_empty() {
                    params["ids"] = json!(ids.iter().map(|id| id.0).collect::<Vec<_>>());
                }
                match variable {
                    Some(variable) => params["variable"] = json!(variable),
                    None => params["variable"] = Value::Null,
                }
                ("variable.bind".to_owned(), params)
            }
            Self::SetMode {
                ids,
                collection,
                mode,
            } => {
                let mut params = json!({ "collection": collection });
                if !ids.is_empty() {
                    params["ids"] = json!(ids.iter().map(|id| id.0).collect::<Vec<_>>());
                }
                match mode {
                    Some(mode) => params["mode"] = json!(mode),
                    None => params["mode"] = Value::Null,
                }
                ("variable.setMode".to_owned(), params)
            }
            Self::RenameGroup {
                collection,
                group,
                renames,
            } => {
                let commands: Vec<Value> = renames
                    .iter()
                    .map(|(variable, name)| {
                        json!({
                            "command": "variable.update",
                            "params": { "variable": variable, "name": name },
                        })
                    })
                    .collect();
                (
                    "command.batch".to_owned(),
                    json!({
                        "label": format!("Rename group {group}"),
                        "collection": collection,
                        "commands": commands,
                    }),
                )
            }
            Self::DeleteGroup {
                collection,
                group,
                variables,
            } => {
                let commands: Vec<Value> = variables
                    .iter()
                    .map(|variable| {
                        json!({
                            "command": "variable.delete",
                            "params": { "variable": variable },
                        })
                    })
                    .collect();
                (
                    "command.batch".to_owned(),
                    json!({
                        "label": format!("Delete group {group}"),
                        "collection": collection,
                        "commands": commands,
                    }),
                )
            }
        }
    }
}

/// A value, as a `variable.set` value.
///
/// The engine's own spellings: a colour is a CSS hex, an alias names the
/// variable it points at.
fn value_json(value: &VariableValue) -> Value {
    match value {
        VariableValue::Color(color) => json!(color.to_css_hex()),
        VariableValue::Number(number) => json!(number),
        VariableValue::String(text) => json!(text),
        VariableValue::Boolean(flag) => json!(flag),
        VariableValue::Alias(target) => json!({ "alias": target }),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel's own state
// ═══════════════════════════════════════════════════════════════════════════

/// Which field is being typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VariableField {
    /// The table's search field.
    Search,
    /// A collection's name.
    CollectionName,
    /// A mode's name.
    ModeName,
    /// A variable's name.
    VariableName,
    /// A variable's description.
    VariableDescription,
    /// The name code uses for a variable on one platform.
    CodeSyntax,
    /// A variable's value in one mode.
    Value,
    /// A group's name.
    GroupName,
}

/// A value being typed into a field.
///
/// A buffer, not a second copy of the variable: the value becomes real only
/// when Enter hands it to the engine as one semantic operation.
#[derive(Clone, Debug, PartialEq)]
pub struct VariableDraft {
    /// Which field.
    pub field: VariableField,
    /// The text as typed.
    pub text: String,
    /// Whether the text is selected, so the first keystroke replaces it.
    pub select_all: bool,
    /// The collection the field belongs to.
    pub collection: Option<String>,
    /// The mode the field belongs to.
    pub mode: Option<String>,
    /// The variable the field belongs to.
    pub variable: Option<String>,
    /// What the field holds, when its kind is needed to read it back.
    pub kind: Option<VariableType>,
    /// The value the field held when it was opened.
    ///
    /// A colour keeps its alpha when it is typed over: six digits change the
    /// colour, eight change the alpha too.
    pub current: Option<VariableValue>,
    /// The variables a group rename moves, with the names they get.
    pub renames: Vec<(String, String)>,
    /// The variables a group delete removes.
    pub removals: Vec<String>,
}

impl Default for VariableDraft {
    fn default() -> Self {
        Self {
            field: VariableField::Search,
            text: String::new(),
            select_all: true,
            collection: None,
            mode: None,
            variable: None,
            kind: None,
            current: None,
            renames: Vec::new(),
            removals: Vec::new(),
        }
    }
}

/// Which menu of the panel is open.
///
/// One menu at a time: a second one opening closes the first, which is what a
/// menu that opens another does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VariablesMenu {
    /// The actions on a collection.
    Collection(String),
    /// The actions on a mode.
    Mode(String, String),
    /// The actions on a variable.
    Variable(String),
    /// The kinds of variable a new one can be.
    CreateVariable,
    /// The variables a value can become an alias of.
    Alias(String, String),
    /// The actions on a group.
    Group(String, String),
    /// The fields a variable can be bound to.
    BindField,
    /// The variables that fit a field.
    BindVariable(String),
    /// The modes a collection can be set to on the selection.
    SetMode(String),
}

/// The panel's own state.
///
/// Presentation only: never in the document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VariablesPanelState {
    /// The collection the table shows.
    pub collection: Option<String>,
    /// The search text the table is filtered by.
    pub query: String,
    /// The group the table is filtered by.
    pub group: Option<String>,
    /// The variable whose details are open.
    pub details: Option<String>,
    /// Which menu is open, if any.
    pub menu: Option<VariablesMenu>,
    /// The field being typed into, if any.
    pub draft: Option<VariableDraft>,
}

impl VariablesPanelState {
    /// The state a section starts in.
    pub fn new() -> Self {
        Self::default()
    }

    /// The text the table is filtered by right now.
    ///
    /// While the search field is open the table follows what has been typed,
    /// which is how figcraft's search behaves: the list narrows as the query
    /// is written, not when it is committed.
    pub fn filter(&self) -> &str {
        match self.draft.as_ref() {
            Some(draft) if draft.field == VariableField::Search => &draft.text,
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
    pub fn handle_key(&mut self, key: &str) -> Option<VariablesAction> {
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
    fn commit_draft(&mut self, draft: VariableDraft) -> Option<VariablesAction> {
        match draft.field {
            // The search is not an edit of the document: it is the table's own
            // filter, so it lands in the state.
            VariableField::Search => {
                self.query = draft.text;
                None
            }
            VariableField::CollectionName => {
                let collection = draft.collection?;
                let name = draft.text.trim();
                if name.is_empty() {
                    return None;
                }
                Some(VariablesAction::RenameCollection {
                    collection,
                    name: name.to_owned(),
                })
            }
            VariableField::ModeName => {
                let collection = draft.collection?;
                let mode = draft.mode?;
                let name = draft.text.trim();
                if name.is_empty() {
                    return None;
                }
                Some(VariablesAction::RenameMode {
                    collection,
                    mode,
                    name: name.to_owned(),
                })
            }
            VariableField::VariableName => {
                let variable = draft.variable?;
                let name = draft.text.trim();
                if name.is_empty() {
                    return None;
                }
                Some(VariablesAction::RenameVariable {
                    variable,
                    name: name.to_owned(),
                })
            }
            VariableField::VariableDescription => {
                let variable = draft.variable?;
                Some(VariablesAction::DescribeVariable {
                    variable,
                    description: draft.text,
                })
            }
            VariableField::CodeSyntax => {
                let variable = draft.variable?;
                let platform = draft.mode.clone()?;
                Some(VariablesAction::SetCodeSyntax {
                    variable,
                    platform,
                    name: draft.text.trim().to_owned(),
                })
            }
            VariableField::Value => {
                let variable = draft.variable?;
                let kind = draft.kind?;
                let value = parse_value(kind, &draft.text, draft.current.as_ref())?;
                Some(VariablesAction::SetValue {
                    variable,
                    mode: draft.mode.clone(),
                    value,
                })
            }
            VariableField::GroupName => {
                let collection = draft.collection?;
                let group = draft.mode.clone()?;
                // The group's last part changes, and every variable under it
                // moves with it. A name with a `/` in it cannot be a group.
                let new = draft.text.trim().replace('/', "-");
                if new.is_empty() {
                    return None;
                }
                let renamed = match group.rsplit_once('/') {
                    Some((parent, _)) => format!("{parent}/{new}"),
                    None => new.clone(),
                };
                let renames = draft
                    .renames
                    .iter()
                    .map(|(variable, rest)| (variable.clone(), format!("{renamed}/{rest}")))
                    .collect();
                Some(VariablesAction::RenameGroup {
                    collection,
                    group,
                    renames,
                })
            }
        }
    }
}

/// The value a typed text means, in a variable's own kind.
///
/// `None` when the text is not a value of that kind, so a field that cannot
/// be read commits nothing rather than committing zero.
fn parse_value(
    kind: VariableType,
    text: &str,
    current: Option<&VariableValue>,
) -> Option<VariableValue> {
    match kind {
        VariableType::Color => {
            let color = Color::from_hex_or_name(text.trim())?;
            // Six digits (or a colour name) keep the alpha; eight digits set
            // it. The engine's own rule, from the colour field.
            let digits = text.trim().trim_start_matches('#');
            let color = if digits.len() == 8 && digits.chars().all(|ch| ch.is_ascii_hexdigit()) {
                color
            } else {
                let alpha = match current {
                    Some(VariableValue::Color(color)) => color.a,
                    _ => color.a,
                };
                color.with_alpha(alpha)
            };
            Some(VariableValue::Color(color))
        }
        VariableType::Number => {
            let number = text.trim().parse::<f64>().ok()?;
            if !number.is_finite() {
                return None;
            }
            Some(VariableValue::Number(number))
        }
        VariableType::String => Some(VariableValue::String(text.to_owned())),
        VariableType::Boolean => Some(VariableValue::Boolean(text.trim() == "true")),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// What a surface that hosts the Variables section must be able to do.
///
/// The panel draws and asks; the surface owns the engine and runs the changes.
/// The panel never touches the document itself, so the engine's command
/// surface stays the one operation boundary.
pub trait VariablesHost: Sized + 'static {
    /// Run a change through the engine.
    fn run_variables_action(&mut self, action: VariablesAction, cx: &mut Context<Self>);
}

/// The Variables section, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own state,
/// and the surface that runs the changes. It returns an element, so the
/// Inspector can put it in a section of its own and a test can put it in a
/// window on its own.
pub fn variables_section<T: VariablesHost>(
    model: &VariablesModel,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2().px(px(14.0)).py(px(12.0));
    column = column.child(collections_block(model, state, cx));
    match current_collection(model, state) {
        Some(collection) => column = column.child(table(model, collection, state, cx)),
        None => column = column.child(empty_state(model, cx)),
    }
    column = column.child(selection_block(model, state, cx));
    column = column.child(caption(SAVE_PATH_NOTE));
    column.into_any_element()
}

/// The collection the table shows.
///
/// The one the panel was told to show, or the first one: figcraft's editor
/// shows a collection from the moment it opens.
fn current_collection<'a>(
    model: &'a VariablesModel,
    state: &Rc<RefCell<VariablesPanelState>>,
) -> Option<&'a VariableCollection> {
    let chosen = state.borrow().collection.clone();
    match chosen {
        Some(id) => model.collection(&id),
        None => model.collections.first(),
    }
}

/// The collections: the list, the way to make one, and the groups of the
/// collection that is shown.
fn collections_block<T: VariablesHost>(
    model: &VariablesModel,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_1();
    let mut header = div().flex().items_center().justify_between().gap_2().child(
        div()
            .id(SharedString::from("variables-collections-title"))
            .debug_selector(|| "variables-collections-title".to_owned())
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgb(theme::TEXT))
            .child("Collections"),
    );
    let new_name = model.next_collection_name();
    header = header.child(
        div()
            .id(SharedString::from("variables-create-collection"))
            .debug_selector(|| "variables-create-collection".to_owned())
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("＋")
            .on_click(cx.listener(move |host, _event, _window, cx| {
                host.run_variables_action(
                    VariablesAction::CreateCollection {
                        name: new_name.clone(),
                    },
                    cx,
                );
            })),
    );
    column = column.child(header);
    for collection in &model.collections {
        column = column.child(collection_row(model, collection, state, cx));
        if current_collection(model, state).map(|c| c.id.as_str()) == Some(collection.id.as_str()) {
            column = column.child(group_rows(model, collection, state, cx));
        }
    }
    column.into_any_element()
}

/// One collection: its name, how many variables it holds, and the way to
/// change it.
fn collection_row<T: VariablesHost>(
    model: &VariablesModel,
    collection: &VariableCollection,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let selected =
        current_collection(model, state).map(|c| c.id.as_str()) == Some(collection.id.as_str());
    let count = collection.variables.len();
    let mut row = div()
        .id(SharedString::from(format!(
            "variables-collection-{}",
            collection.id
        )))
        .debug_selector(move || format!("variables-collection-{}", collection.id))
        .flex()
        .items_center()
        .gap_2()
        .px(px(6.0))
        .py(px(6.0))
        .rounded_md()
        .bg(if selected {
            rgb(theme::SURFACE_HOVER)
        } else {
            rgba(0x00000000)
        })
        .text_xs()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer();
    let name = collection.name.clone();
    let shared = Rc::clone(state);
    let id = collection.id.clone();
    row = row
        .child(
            div()
                .flex_1()
                .text_color(rgb(theme::TEXT))
                .child(name.clone()),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "variables-collection-{}-count",
                    collection.id
                )))
                .debug_selector(move || format!("variables-collection-{}-count", collection.id))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(count.to_string()),
        )
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                if state.collection.as_deref() != Some(id.as_str()) {
                    state.group = None;
                    state.query.clear();
                }
                state.collection = Some(id.clone());
                state.menu = None;
            }
            cx.notify();
        }));
    // The way to change the collection: rename it, or delete it.
    row = row.child(menu_button(
        &format!("variables-collection-{}-menu", collection.id),
        &format!("variables-collection-{}-menu", collection.id),
        state,
        VariablesMenu::Collection(collection.id.clone()),
        cx,
    ));
    let collection_menu_open =
        state.borrow().menu == Some(VariablesMenu::Collection(collection.id.clone()));
    if collection_menu_open {
        row = row.child(collection_menu(collection, state, cx));
    }
    row.into_any_element()
}

/// The actions on a collection: rename it, or delete it.
fn collection_menu<T: VariablesHost>(
    collection: &VariableCollection,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut menu = menu_box();
    let id = collection.id.clone();
    let name = collection.name.clone();
    let shared = Rc::clone(state);
    menu = menu.child(
        div()
            .id(SharedString::from(format!(
                "variables-collection-{}-rename",
                collection.id
            )))
            .debug_selector(move || format!("variables-collection-{}-rename", collection.id))
            .px(px(6.0))
            .py(px(5.0))
            .rounded_sm()
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("Rename")
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.menu = None;
                    state.draft = Some(VariableDraft {
                        field: VariableField::CollectionName,
                        text: name.clone(),
                        collection: Some(id.clone()),
                        ..VariableDraft::default()
                    });
                }
                cx.notify();
            })),
    );
    let id = collection.id.clone();
    menu = menu.child(menu_item(
        &format!("variables-collection-{}-delete", collection.id),
        "Delete collection",
        VariablesAction::DeleteCollection { collection: id },
        cx,
    ));
    menu.into_any_element()
}

/// The actions on a group: rename it, or delete it.
fn group_menu<T: VariablesHost>(
    model: &VariablesModel,
    collection: &VariableCollection,
    group: &str,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let members: Vec<&VariableEntry> = model
        .variables_in(&collection.id)
        .into_iter()
        .filter(|variable| variable.name.starts_with(&format!("{group}/")))
        .collect();
    let renames: Vec<(String, String)> = members
        .iter()
        .map(|variable| {
            let rest = variable
                .name
                .strip_prefix(&format!("{group}/"))
                .unwrap_or(&variable.name)
                .to_owned();
            (variable.id.clone(), rest)
        })
        .collect();
    let variables: Vec<String> = members.iter().map(|variable| variable.id.clone()).collect();
    let mut menu = menu_box();
    // Renaming a group renames every variable under it, in one undo step.
    let shared = Rc::clone(state);
    let collection_id = collection.id.clone();
    let group = group.to_owned();
    let label = group.rsplit('/').next().unwrap_or(&group).to_owned();
    let selector_group = group.clone();
    let renamed_group = group.clone();
    menu = menu.child(
        div()
            .id(SharedString::from(format!(
                "variables-group-{group}-rename"
            )))
            .debug_selector(move || format!("variables-group-{selector_group}-rename"))
            .px(px(6.0))
            .py(px(5.0))
            .rounded_sm()
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("Rename group")
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.menu = None;
                    state.draft = Some(VariableDraft {
                        field: VariableField::GroupName,
                        text: label.clone(),
                        collection: Some(collection_id.clone()),
                        mode: Some(renamed_group.clone()),
                        renames: renames.clone(),
                        ..VariableDraft::default()
                    });
                }
                cx.notify();
            })),
    );
    menu = menu.child(menu_item(
        &format!("variables-group-{group}-delete"),
        "Delete group",
        VariablesAction::DeleteGroup {
            collection: collection.id.clone(),
            group: group.clone(),
            variables,
        },
        cx,
    ));
    menu.into_any_element()
}

/// The groups of the collection that is shown, and the way to filter by one.
fn group_rows<T: VariablesHost>(
    model: &VariablesModel,
    collection: &VariableCollection,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let groups = model.groups(&collection.id);
    if groups.is_empty() {
        return div().into_any_element();
    }
    let mut column = div().flex().flex_col().gap_1();
    let total = collection.variables.len();
    column = column.child(group_row(
        model,
        collection,
        None,
        "All variables",
        total,
        state,
        cx,
    ));
    for group in groups {
        column = column.child(group_row(
            model,
            collection,
            Some(group.path.clone()),
            &group.label,
            group.count,
            state,
            cx,
        ));
    }
    column.into_any_element()
}

/// One group row: the group's name, how many variables are in it, and the way
/// to change it.
#[allow(clippy::too_many_arguments)]
fn group_row<T: VariablesHost>(
    model: &VariablesModel,
    collection: &VariableCollection,
    group: Option<String>,
    label: &str,
    count: usize,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let selected = state.borrow().group.as_deref() == group.as_deref();
    let key = group.clone().unwrap_or_default();
    let row_key = key.clone();
    let count_key = key.clone();
    let menu_key = key.clone();
    let mut row = div()
        .id(SharedString::from(format!("variables-group-{key}")))
        .debug_selector(move || format!("variables-group-{row_key}"))
        .flex()
        .items_center()
        .gap_2()
        .pl(px(6.0
            + 12.0
                * group
                    .as_deref()
                    .map(|g| g.matches('/').count())
                    .unwrap_or(0) as f32))
        .pr(px(6.0))
        .py(px(4.0))
        .rounded_md()
        .bg(if selected {
            rgb(theme::SURFACE_HOVER)
        } else {
            rgba(0x00000000)
        })
        .text_xs()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer();
    let shared = Rc::clone(state);
    let chosen = group.clone();
    row = row
        .child(
            div()
                .flex_1()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(label.to_owned()),
        )
        .child(
            div()
                .id(SharedString::from(format!("variables-group-{key}-count")))
                .debug_selector(move || format!("variables-group-{count_key}-count"))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(count.to_string()),
        )
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.group = chosen.clone();
                state.menu = None;
            }
            cx.notify();
        }));
    // A group can be renamed and deleted; "All variables" is not a group.
    if let Some(path) = group.as_deref() {
        let shared = Rc::clone(state);
        let collection_id = collection.id.clone();
        let group_path = path.to_owned();
        row = row.child(
            div()
                .id(SharedString::from(format!("variables-group-{key}-menu")))
                .debug_selector(move || format!("variables-group-{menu_key}-menu"))
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("⋯")
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.menu = Some(VariablesMenu::Group(
                            collection_id.clone(),
                            group_path.clone(),
                        ));
                        state.draft = None;
                    }
                    cx.notify();
                })),
        );
        let group_menu_open = state.borrow().menu
            == Some(VariablesMenu::Group(collection.id.clone(), path.to_owned()));
        if group_menu_open {
            row = row.child(group_menu(model, collection, path, state, cx));
        }
    }
    row.into_any_element()
}

/// The table: the search, the modes, and the variables.
fn table<T: VariablesHost>(
    model: &VariablesModel,
    collection: &VariableCollection,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2();
    column = column.child(search_field(state, cx));
    column = column.child(modes_strip(model, collection, state, cx));
    let query = state.borrow().filter().to_owned();
    let group = state.borrow().group.clone();
    let variables = model.visible_variables(&collection.id, group.as_deref(), &query);
    if variables.is_empty() && !query.trim().is_empty() {
        let selector = format!("variables-empty-{NO_MATCHING_VARIABLES}");
        column = column.child(
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(NO_MATCHING_VARIABLES.to_owned()),
        );
    }
    let mut group_seen: Option<&str> = None;
    for variable in variables {
        let group = variable.group();
        if group_seen != Some(group) {
            group_seen = Some(group);
            if !group.is_empty() {
                let selector = format!("variables-group-header-{group}");
                column = column.child(
                    div()
                        .id(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .px(px(6.0))
                        .py(px(4.0))
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(theme::TEXT_SECONDARY))
                        .child(group.to_owned()),
                );
            }
        }
        column = column.child(variable_card(model, collection, variable, state, cx));
    }
    column = column.child(create_variable_row(model, collection, state, cx));
    column.into_any_element()
}

/// The search field.
fn search_field<T: VariablesHost>(
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let draft = state
        .borrow()
        .draft
        .as_ref()
        .filter(|draft| draft.field == VariableField::Search)
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
        .id(SharedString::from("variables-search"))
        .debug_selector(|| "variables-search".to_owned())
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
                state.draft = Some(VariableDraft {
                    field: VariableField::Search,
                    text: state.query.clone(),
                    ..VariableDraft::default()
                });
            }
            cx.notify();
        }))
        .into_any_element()
}

/// The modes: figcraft's header row, as a strip of the collection's modes.
fn modes_strip<T: VariablesHost>(
    model: &VariablesModel,
    collection: &VariableCollection,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut row = div().flex().flex_wrap().items_center().gap_1();
    for (index, mode) in collection.modes.iter().enumerate() {
        let mut chip = div()
            .id(SharedString::from(format!("variables-mode-{}", mode.id)))
            .debug_selector(move || format!("variables-mode-{}", mode.id))
            .flex()
            .items_center()
            .gap_1()
            .px(px(6.0))
            .py(px(4.0))
            .rounded_md()
            .bg(rgb(theme::WINDOW))
            .border_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY));
        let name = mode.name.clone();
        let shared = Rc::clone(state);
        let mode_id = mode.id.clone();
        let collection_id = collection.id.clone();
        let draft_text = name.clone();
        chip = chip
            .child(name)
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.draft = Some(VariableDraft {
                        field: VariableField::ModeName,
                        text: draft_text.clone(),
                        collection: Some(collection_id.clone()),
                        mode: Some(mode_id.clone()),
                        ..VariableDraft::default()
                    });
                    state.menu = None;
                }
                cx.notify();
            }));
        // The way to change the mode: rename it, duplicate it, move it, delete
        // it, or make it the default.
        chip = chip.child(menu_button(
            &format!("variables-mode-{}-menu", mode.id),
            &format!("variables-mode-{}-menu", mode.id),
            state,
            VariablesMenu::Mode(collection.id.clone(), mode.id.clone()),
            cx,
        ));
        let mode_menu_open = state.borrow().menu
            == Some(VariablesMenu::Mode(collection.id.clone(), mode.id.clone()));
        if mode_menu_open {
            chip = chip.child(mode_menu(collection, mode, index, state, cx));
        }
        row = row.child(chip);
    }
    // The way to add a mode. An extended collection has the modes of the
    // collection it extends, and the engine refuses a mode edit on one.
    if collection.modes_editable() {
        let collection_id = collection.id.clone();
        let new_name = model.next_mode_name(&collection_id);
        row = row.child(
            div()
                .id(SharedString::from("variables-add-mode"))
                .debug_selector(|| "variables-add-mode".to_owned())
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("＋")
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    host.run_variables_action(
                        VariablesAction::AddMode {
                            collection: collection_id.clone(),
                            name: new_name.clone(),
                            from: None,
                        },
                        cx,
                    );
                })),
        );
    }
    row.into_any_element()
}

/// The actions on a mode: rename it, duplicate it, move it, make it the
/// default, or delete it.
///
/// The enable/disable behaviour is figcraft's: the first column is the default
/// mode, so it cannot be moved left and cannot be made the default again; the
/// last column cannot be moved right; and a collection keeps at least one
/// mode, so the last one cannot be deleted.
fn mode_menu<T: VariablesHost>(
    collection: &VariableCollection,
    mode: &VariableMode,
    index: usize,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut menu = menu_box();
    // An extended collection has the modes of the collection it extends, and
    // the engine refuses a mode edit on one.
    if !collection.modes_editable() {
        return menu.into_any_element();
    }
    let id = mode.id.clone();
    let name = mode.name.clone();
    let shared = Rc::clone(state);
    let collection_id = collection.id.clone();
    let selector_mode = mode.id.clone();
    menu = menu.child(
        div()
            .id(SharedString::from(format!(
                "variables-mode-{}-rename",
                mode.id
            )))
            .debug_selector(move || format!("variables-mode-{selector_mode}-rename"))
            .px(px(6.0))
            .py(px(5.0))
            .rounded_sm()
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("Rename mode")
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.menu = None;
                    state.draft = Some(VariableDraft {
                        field: VariableField::ModeName,
                        text: name.clone(),
                        collection: Some(collection_id.clone()),
                        mode: Some(id.clone()),
                        ..VariableDraft::default()
                    });
                }
                cx.notify();
            })),
    );
    // Duplicating a mode copies its values, the way the engine's own
    // `variable.addMode {from}` does.
    let copy = unique_name(&format!("{} copy", mode.name), |name| {
        collection.modes.iter().any(|other| other.name == name)
    });
    menu = menu.child(menu_item(
        &format!("variables-mode-{}-duplicate", mode.id),
        "Duplicate mode",
        VariablesAction::AddMode {
            collection: collection.id.clone(),
            name: copy,
            from: Some(mode.id.clone()),
        },
        cx,
    ));
    let last = collection.modes.len().saturating_sub(1);
    if index > 0 {
        menu = menu.child(menu_item(
            &format!("variables-mode-{}-default", mode.id),
            "Set as default",
            VariablesAction::MoveMode {
                collection: collection.id.clone(),
                mode: mode.id.clone(),
                to: 0,
            },
            cx,
        ));
        menu = menu.child(menu_item(
            &format!("variables-mode-{}-left", mode.id),
            "Move column left",
            VariablesAction::MoveMode {
                collection: collection.id.clone(),
                mode: mode.id.clone(),
                to: index - 1,
            },
            cx,
        ));
    }
    if index < last {
        menu = menu.child(menu_item(
            &format!("variables-mode-{}-right", mode.id),
            "Move column right",
            VariablesAction::MoveMode {
                collection: collection.id.clone(),
                mode: mode.id.clone(),
                to: index + 1,
            },
            cx,
        ));
    }
    if collection.modes.len() > 1 {
        menu = menu.child(menu_item(
            &format!("variables-mode-{}-delete", mode.id),
            "Delete mode",
            VariablesAction::RemoveMode {
                collection: collection.id.clone(),
                mode: mode.id.clone(),
            },
            cx,
        ));
    }
    menu.into_any_element()
}

/// One variable: its name, its type, and its value in each mode.
fn variable_card<T: VariablesHost>(
    model: &VariablesModel,
    collection: &VariableCollection,
    variable: &VariableEntry,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut card = div()
        .id(SharedString::from(format!("variables-row-{}", variable.id)))
        .debug_selector(move || format!("variables-row-{}", variable.id))
        .flex()
        .flex_col()
        .gap_1()
        .px(px(8.0))
        .py(px(7.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs();
    let mut header = div().flex().items_center().gap_2();
    header = header.child(kind_glyph(variable));
    let name = variable.leaf().to_owned();
    let shared = Rc::clone(state);
    let variable_id = variable.id.clone();
    let collection_id = collection.id.clone();
    header = header.child(
        div()
            .id(SharedString::from(format!(
                "variables-row-{}-name",
                variable.id
            )))
            .debug_selector(move || format!("variables-row-{}-name", variable.id))
            .flex_1()
            .text_color(rgb(theme::TEXT))
            .child(name.clone())
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.draft = Some(VariableDraft {
                        field: VariableField::VariableName,
                        text: name.clone(),
                        variable: Some(variable_id.clone()),
                        collection: Some(collection_id.clone()),
                        ..VariableDraft::default()
                    });
                    state.menu = None;
                }
                cx.notify();
            })),
    );
    // The variable's actions: edit it, rename it, reset its overrides,
    // duplicate it, delete it.
    header = header.child(menu_button(
        &format!("variables-row-{}-menu", variable.id),
        &format!("variables-row-{}-menu", variable.id),
        state,
        VariablesMenu::Variable(variable.id.clone()),
        cx,
    ));
    let variable_menu_open =
        state.borrow().menu == Some(VariablesMenu::Variable(variable.id.clone()));
    if variable_menu_open {
        header = header.child(variable_menu(variable, state, cx));
    }
    card = card.child(header);
    for mode in &collection.modes {
        card = card.child(value_cell(model, variable, mode, state, cx));
    }
    // The variable's details, when they are open.
    let details_open = state.borrow().details.as_deref() == Some(variable.id.as_str());
    if details_open {
        card = card.child(details(variable, state, cx));
    }
    card.into_any_element()
}

/// The actions on a variable.
///
/// The enable/disable behaviour is figcraft's: an inherited variable is named
/// and deleted in the collection it extends, so the panel does not offer
/// either; and "Reset overrides" is only there when there is an override to
/// reset.
fn variable_menu<T: VariablesHost>(
    variable: &VariableEntry,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut menu = menu_box();
    // The variable's details.
    let id = variable.id.clone();
    let shared = Rc::clone(state);
    menu = menu.child(
        div()
            .id(SharedString::from(format!(
                "variables-row-{}-details",
                variable.id
            )))
            .debug_selector(move || format!("variables-row-{}-details", variable.id))
            .px(px(6.0))
            .py(px(5.0))
            .rounded_sm()
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("Edit variable")
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.menu = None;
                    state.details = Some(id.clone());
                }
                cx.notify();
            })),
    );
    // Renaming: an inherited variable's name is the inherited one's.
    if !variable.inherited() {
        let name = variable.name.clone();
        let shared = Rc::clone(state);
        let id = variable.id.clone();
        menu = menu.child(
            div()
                .id(SharedString::from(format!(
                    "variables-row-{}-rename",
                    variable.id
                )))
                .debug_selector(move || format!("variables-row-{}-rename", variable.id))
                .px(px(6.0))
                .py(px(5.0))
                .rounded_sm()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("Rename")
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.menu = None;
                        state.draft = Some(VariableDraft {
                            field: VariableField::VariableName,
                            text: name.clone(),
                            variable: Some(id.clone()),
                            ..VariableDraft::default()
                        });
                    }
                    cx.notify();
                })),
        );
    }
    if !variable.overrides.is_empty() {
        menu = menu.child(menu_item(
            &format!("variables-row-{}-reset", variable.id),
            "Reset overrides",
            VariablesAction::ResetOverride {
                variable: variable.id.clone(),
                mode: None,
            },
            cx,
        ));
    }
    menu = menu.child(menu_item(
        &format!("variables-row-{}-duplicate", variable.id),
        "Duplicate",
        VariablesAction::DuplicateVariable {
            variable: variable.id.clone(),
        },
        cx,
    ));
    if !variable.inherited() {
        menu = menu.child(menu_item(
            &format!("variables-row-{}-delete", variable.id),
            "Delete variable",
            VariablesAction::DeleteVariable {
                variable: variable.id.clone(),
            },
            cx,
        ));
    }
    menu.into_any_element()
}

/// The type marker before a variable's name.
fn kind_glyph(variable: &VariableEntry) -> Div {
    match variable.kind {
        VariableType::Color => {
            let color = variable
                .resolved
                .values()
                .next()
                .and_then(|value| match value {
                    VariableValue::Color(color) => Some(*color),
                    _ => None,
                });
            swatch(color)
        }
        VariableType::Number => div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(14.0))
            .h(px(14.0))
            .text_color(rgb(theme::TEXT_MUTED))
            .child("#"),
        VariableType::String => div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(14.0))
            .h(px(14.0))
            .text_color(rgb(theme::TEXT_MUTED))
            .child("T"),
        VariableType::Boolean => div()
            .flex()
            .items_center()
            .justify_center()
            .w(px(14.0))
            .h(px(14.0))
            .text_color(rgb(theme::TEXT_MUTED))
            .child("◐"),
    }
}

/// One value cell: the value, in one mode.
fn value_cell<T: VariablesHost>(
    model: &VariablesModel,
    variable: &VariableEntry,
    mode: &VariableMode,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut row = div().flex().items_center().gap_2().pl(px(4.0)).py(px(2.0));
    // An overridden value is marked, the way figcraft marks it: a bar down the
    // side of the cell, and the way to put the inherited value back.
    if variable.overridden(&mode.id) {
        let selector = format!("variables-value-{}-{}-override", variable.id, mode.id);
        row = row.child(
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .w(px(2.0))
                .h(px(14.0))
                .rounded_sm()
                .bg(rgb(theme::ACCENT)),
        );
    }
    row = row.child(
        div()
            .w(px(72.0))
            .flex_none()
            .text_color(rgb(theme::TEXT_MUTED))
            .child(mode.name.clone()),
    );
    let Some(value) = variable.value_in(&mode.id) else {
        // The variable has no value in this mode: figcraft leaves the cell
        // empty, and so does this.
        return row.into_any_element();
    };
    match value {
        VariableValue::Alias(target) => {
            let name = model
                .variable(target)
                .map(|variable| variable.name.clone())
                .unwrap_or_else(|| MISSING_VARIABLE.to_owned());
            let selector = format!("variables-value-{}-{}", variable.id, mode.id);
            row = row.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px(px(6.0))
                    .py(px(4.0))
                    .rounded_md()
                    .bg(rgb(theme::SURFACE))
                    .border_1()
                    .border_color(rgb(theme::BORDER_SOFT))
                    .text_color(rgb(theme::TEXT))
                    .child("var")
                    .child(name),
            );
            // Detach the alias: keep the value it resolves to now.
            let resolved = variable.resolved.get(&mode.id).cloned();
            if let Some(resolved) = resolved {
                let variable_id = variable.id.clone();
                let mode_id = mode.id.clone();
                row = row.child(
                    div()
                        .id(SharedString::from(format!(
                            "variables-value-{}-{}-detach",
                            variable.id, mode.id
                        )))
                        .debug_selector(move || {
                            format!("variables-value-{}-{}-detach", variable.id, mode.id)
                        })
                        .text_xs()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                        .cursor_pointer()
                        .child("⊘")
                        .on_click(cx.listener(move |host, _event, _window, cx| {
                            host.run_variables_action(
                                VariablesAction::SetValue {
                                    variable: variable_id.clone(),
                                    mode: Some(mode_id.clone()),
                                    value: resolved.clone(),
                                },
                                cx,
                            );
                        })),
                );
            }
        }
        VariableValue::Color(color) => {
            let selector = format!("variables-value-{}-{}", variable.id, mode.id);
            let hex = color.to_hex();
            let alpha = format!("{}%", (color.a * 100.0).round());
            let draft = state
                .borrow()
                .draft
                .as_ref()
                .filter(|draft| {
                    draft.field == VariableField::Value
                        && draft.variable.as_deref() == Some(variable.id.as_str())
                        && draft.mode.as_deref() == Some(mode.id.as_str())
                })
                .map(|draft| draft.text.clone());
            let shown = draft.unwrap_or_else(|| hex.clone());
            let shared = Rc::clone(state);
            let variable_id = variable.id.clone();
            let mode_id = mode.id.clone();
            let current = VariableValue::Color(*color);
            let alpha_variable = variable.id.clone();
            let alpha_mode = mode.id.clone();
            row = row
                .child(swatch(Some(*color)))
                .child(
                    div()
                        .id(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .flex_1()
                        .px(px(6.0))
                        .py(px(4.0))
                        .rounded_md()
                        .bg(rgb(theme::SURFACE))
                        .border_1()
                        .border_color(rgb(theme::BORDER_SOFT))
                        .text_color(rgb(theme::TEXT_SECONDARY))
                        .cursor_text()
                        .child(shown.clone())
                        .on_click(cx.listener(move |_host, _event, _window, cx| {
                            {
                                let mut state = shared.borrow_mut();
                                state.draft = Some(VariableDraft {
                                    field: VariableField::Value,
                                    text: shown.clone(),
                                    variable: Some(variable_id.clone()),
                                    mode: Some(mode_id.clone()),
                                    kind: Some(VariableType::Color),
                                    current: Some(current.clone()),
                                    ..VariableDraft::default()
                                });
                            }
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "variables-value-{}-{}-alpha",
                            variable.id, mode.id
                        )))
                        .debug_selector(move || {
                            format!("variables-value-{}-{}-alpha", alpha_variable, alpha_mode)
                        })
                        .flex_none()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(alpha),
                );
        }
        VariableValue::Number(number) => {
            let selector = format!("variables-value-{}-{}", variable.id, mode.id);
            let text = figcraft_doc::trim_num(*number);
            let draft = state
                .borrow()
                .draft
                .as_ref()
                .filter(|draft| {
                    draft.field == VariableField::Value
                        && draft.variable.as_deref() == Some(variable.id.as_str())
                        && draft.mode.as_deref() == Some(mode.id.as_str())
                })
                .map(|draft| draft.text.clone());
            let shown = draft.unwrap_or_else(|| text.clone());
            let shared = Rc::clone(state);
            let variable_id = variable.id.clone();
            let mode_id = mode.id.clone();
            let current = VariableValue::Number(*number);
            row = row.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .flex_1()
                    .px(px(6.0))
                    .py(px(4.0))
                    .rounded_md()
                    .bg(rgb(theme::SURFACE))
                    .border_1()
                    .border_color(rgb(theme::BORDER_SOFT))
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .cursor_text()
                    .child(shown.clone())
                    .on_click(cx.listener(move |_host, _event, _window, cx| {
                        {
                            let mut state = shared.borrow_mut();
                            state.draft = Some(VariableDraft {
                                field: VariableField::Value,
                                text: shown.clone(),
                                variable: Some(variable_id.clone()),
                                mode: Some(mode_id.clone()),
                                kind: Some(VariableType::Number),
                                current: Some(current.clone()),
                                ..VariableDraft::default()
                            });
                        }
                        cx.notify();
                    })),
            );
        }
        VariableValue::String(text) => {
            let selector = format!("variables-value-{}-{}", variable.id, mode.id);
            let draft = state
                .borrow()
                .draft
                .as_ref()
                .filter(|draft| {
                    draft.field == VariableField::Value
                        && draft.variable.as_deref() == Some(variable.id.as_str())
                        && draft.mode.as_deref() == Some(mode.id.as_str())
                })
                .map(|draft| draft.text.clone());
            let shown = draft.unwrap_or_else(|| text.clone());
            let shared = Rc::clone(state);
            let variable_id = variable.id.clone();
            let mode_id = mode.id.clone();
            let current = VariableValue::String(text.clone());
            row = row.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .flex_1()
                    .px(px(6.0))
                    .py(px(4.0))
                    .rounded_md()
                    .bg(rgb(theme::SURFACE))
                    .border_1()
                    .border_color(rgb(theme::BORDER_SOFT))
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .cursor_text()
                    .child(shown.clone())
                    .on_click(cx.listener(move |_host, _event, _window, cx| {
                        {
                            let mut state = shared.borrow_mut();
                            state.draft = Some(VariableDraft {
                                field: VariableField::Value,
                                text: shown.clone(),
                                variable: Some(variable_id.clone()),
                                mode: Some(mode_id.clone()),
                                kind: Some(VariableType::String),
                                current: Some(current.clone()),
                                ..VariableDraft::default()
                            });
                        }
                        cx.notify();
                    })),
            );
        }
        VariableValue::Boolean(flag) => {
            let selector = format!("variables-value-{}-{}", variable.id, mode.id);
            let variable_id = variable.id.clone();
            let mode_id = mode.id.clone();
            let next = !*flag;
            row = row.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .flex_1()
                    .px(px(6.0))
                    .py(px(4.0))
                    .rounded_md()
                    .bg(rgb(theme::SURFACE))
                    .border_1()
                    .border_color(rgb(theme::BORDER_SOFT))
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .cursor_pointer()
                    .child(if *flag { "True" } else { "False" })
                    .on_click(cx.listener(move |host, _event, _window, cx| {
                        host.run_variables_action(
                            VariablesAction::SetValue {
                                variable: variable_id.clone(),
                                mode: Some(mode_id.clone()),
                                value: VariableValue::Boolean(next),
                            },
                            cx,
                        );
                    })),
            );
        }
    }
    // The way to make the value an alias of another variable. figcraft reveals
    // it on hover; Spool shows it, because a hover-only control cannot be
    // reached without a pointer.
    let shared = Rc::clone(state);
    let alias_variable = variable.id.clone();
    let alias_mode = mode.id.clone();
    let alias_selector = alias_variable.clone();
    let alias_selector_mode = alias_mode.clone();
    row = row.child(
        div()
            .id(SharedString::from(format!(
                "variables-value-{}-{}-alias",
                variable.id, mode.id
            )))
            .debug_selector(move || {
                format!(
                    "variables-value-{}-{}-alias",
                    alias_selector, alias_selector_mode
                )
            })
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("var")
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.menu = Some(VariablesMenu::Alias(
                        alias_variable.clone(),
                        alias_mode.clone(),
                    ));
                    state.draft = None;
                }
                cx.notify();
            })),
    );
    let alias_menu_open =
        state.borrow().menu == Some(VariablesMenu::Alias(variable.id.clone(), mode.id.clone()));
    if alias_menu_open {
        row = row.child(alias_menu(model, variable, mode, cx));
    }
    // The way to put the inherited value back.
    if variable.overridden(&mode.id) {
        let variable_id = variable.id.clone();
        let mode_id = mode.id.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!(
                    "variables-value-{}-{}-reset",
                    variable.id, mode.id
                )))
                .debug_selector(move || {
                    format!("variables-value-{}-{}-reset", variable.id, mode.id)
                })
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("↺")
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    host.run_variables_action(
                        VariablesAction::ResetOverride {
                            variable: variable_id.clone(),
                            mode: Some(mode_id.clone()),
                        },
                        cx,
                    );
                })),
        );
    }
    row.into_any_element()
}

/// The variables a value can become an alias of.
///
/// An alias names a variable of the same type, and the engine refuses an alias
/// that leads back to itself, so the variable itself is not offered.
fn alias_menu<T: VariablesHost>(
    model: &VariablesModel,
    variable: &VariableEntry,
    mode: &VariableMode,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut menu = menu_box();
    for target in model.variables_of_kind(variable.kind) {
        if target.id == variable.id {
            continue;
        }
        let action = VariablesAction::SetValue {
            variable: variable.id.clone(),
            mode: Some(mode.id.clone()),
            value: VariableValue::Alias(target.id.clone()),
        };
        menu = menu.child(menu_item(
            &format!("variables-alias-{}", target.id),
            &target.name,
            action,
            cx,
        ));
    }
    menu.into_any_element()
}

/// The way to make a new variable, and the kinds it can be.
fn create_variable_row<T: VariablesHost>(
    model: &VariablesModel,
    collection: &VariableCollection,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_1();
    let shared = Rc::clone(state);
    let collection_id = collection.id.clone();
    let mut button = div()
        .id(SharedString::from("variables-create-variable"))
        .debug_selector(|| "variables-create-variable".to_owned())
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer()
        .child("+ Create variable")
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.menu = Some(VariablesMenu::CreateVariable);
                state.draft = None;
            }
            cx.notify();
        }));
    let create_menu_open = state.borrow().menu == Some(VariablesMenu::CreateVariable);
    if create_menu_open {
        let mut menu = menu_box();
        for kind in VARIABLE_TYPES {
            let action = VariablesAction::CreateVariable {
                collection: collection_id.clone(),
                name: model.next_variable_name(&collection_id, kind),
                kind,
                value: VariableValue::default_for(kind),
            };
            let selector = format!("variables-create-variable-{}", kind.id());
            menu = menu.child(menu_item(&selector, kind_label(kind), action, cx));
        }
        button = button.child(menu);
    }
    column = column.child(button);
    column.into_any_element()
}

/// The empty state, word for word as figcraft says it.
fn empty_state<T: VariablesHost>(model: &VariablesModel, cx: &mut Context<T>) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2();
    let selector = format!("variables-empty-{NO_COLLECTIONS}");
    column = column.child(
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .child(NO_COLLECTIONS.to_owned()),
    );
    let new_name = model.next_collection_name();
    column = column.child(
        div()
            .id(SharedString::from("variables-empty-create-collection"))
            .debug_selector(|| "variables-empty-create-collection".to_owned())
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("Create collection")
            .on_click(cx.listener(move |host, _event, _window, cx| {
                host.run_variables_action(
                    VariablesAction::CreateCollection {
                        name: new_name.clone(),
                    },
                    cx,
                );
            })),
    );
    column.into_any_element()
}

/// The selected layers' own variables: what they bind, and which mode they
/// are in.
fn selection_block<T: VariablesHost>(
    model: &VariablesModel,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2();
    column = column.child(caption("Selection"));
    if model.ids.is_empty() {
        column = column.child(
            div()
                .id(SharedString::from("variables-selection-empty"))
                .debug_selector(|| "variables-selection-empty".to_owned())
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child("Select a layer to bind variables to."),
        );
        return column.into_any_element();
    }
    column = column.child(caption("Bound variables"));
    for binding in &model.bindings {
        column = column.child(binding_row(model, binding, state, cx));
    }
    let shared = Rc::clone(state);
    let mut bind = div()
        .id(SharedString::from("variables-bind-variable"))
        .debug_selector(|| "variables-bind-variable".to_owned())
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer()
        .child("+ Bind a variable")
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.menu = Some(VariablesMenu::BindField);
                state.draft = None;
            }
            cx.notify();
        }));
    let bind_field_menu_open = state.borrow().menu == Some(VariablesMenu::BindField);
    if bind_field_menu_open {
        bind = bind.child(bind_field_menu(model, state, cx));
    }
    column = column.child(bind);
    column = column.child(caption("Mode"));
    for mode in &model.modes {
        column = column.child(collection_mode_row(model, mode, state, cx));
    }
    column.into_any_element()
}

/// The fields a variable can be bound to on the selection.
///
/// The engine's own rules: a field the selection does not take a variable for
/// is not offered, because the engine would refuse the bind. Choosing a field
/// opens the variables that fit it.
fn bind_field_menu<T: VariablesHost>(
    model: &VariablesModel,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut menu = menu_box();
    for field in &model.fields {
        let shared = Rc::clone(state);
        let name = field.field.clone();
        menu = menu.child(
            div()
                .id(SharedString::from(format!(
                    "variables-bind-field-{}",
                    field.field
                )))
                .debug_selector(move || format!("variables-bind-field-{}", field.field))
                .px(px(6.0))
                .py(px(5.0))
                .rounded_sm()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child(name.clone())
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.menu = Some(VariablesMenu::BindVariable(name.clone()));
                    }
                    cx.notify();
                })),
        );
    }
    menu.into_any_element()
}

/// The variables that fit a field.
///
/// The engine's own rules again: a variable of a kind the field does not take
/// would be refused, so it is not offered.
fn bind_variable_menu<T: VariablesHost>(
    model: &VariablesModel,
    field: &str,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut menu = menu_box();
    let Some(bindable) = model.fields.iter().find(|entry| entry.field == field) else {
        return menu.into_any_element();
    };
    for collection in &model.collections {
        for variable in &collection.variables {
            if !bindable.takes(variable.kind) {
                continue;
            }
            let action = VariablesAction::Bind {
                ids: model.ids.clone(),
                field: field.to_owned(),
                variable: Some(variable.id.clone()),
            };
            menu = menu.child(menu_item(
                &format!("variables-bind-variable-{}", variable.id),
                &variable.name,
                action,
                cx,
            ));
        }
    }
    menu.into_any_element()
}

/// One bound field: what it is bound to, and the way to change it.
fn binding_row<T: VariablesHost>(
    model: &VariablesModel,
    binding: &BoundVariable,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let shown = match &binding.value {
        VariableState::Set(id) => model
            .variable(id)
            .map(|variable| variable.name.clone())
            .unwrap_or_else(|| MISSING_VARIABLE.to_owned()),
        VariableState::Unset => NOT_SET.to_owned(),
        VariableState::Mixed => MIXED.to_owned(),
    };
    let mut row = div()
        .id(SharedString::from(format!(
            "variables-binding-{}",
            binding.field
        )))
        .debug_selector(move || format!("variables-binding-{}", binding.field))
        .flex()
        .items_center()
        .gap_2()
        .px(px(6.0))
        .py(px(6.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs();
    row = row.child(
        div()
            .flex_1()
            .text_color(rgb(theme::TEXT_MUTED))
            .child(binding.field.clone()),
    );
    row = row.child(
        div()
            .id(SharedString::from(format!(
                "variables-binding-{}-value",
                binding.field
            )))
            .debug_selector(move || format!("variables-binding-{}-value", binding.field))
            .text_color(rgb(theme::TEXT_SECONDARY))
            .child(shown),
    );
    // The way to unbind the field.
    if let VariableState::Set(id) = &binding.value {
        let ids = model.ids.clone();
        let field = binding.field.clone();
        let variable = id.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!(
                    "variables-binding-{}-unbind",
                    binding.field
                )))
                .debug_selector(move || format!("variables-binding-{}-unbind", binding.field))
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("×")
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    host.run_variables_action(
                        VariablesAction::Bind {
                            ids: ids.clone(),
                            field: field.clone(),
                            variable: Some(variable.clone()),
                        },
                        cx,
                    );
                })),
        );
    }
    // The way to bind a different variable to the field.
    row = row.child(menu_button(
        &format!("variables-binding-{}-menu", binding.field),
        &format!("variables-binding-{}-menu", binding.field),
        state,
        VariablesMenu::BindVariable(binding.field.clone()),
        cx,
    ));
    let bind_variable_menu_open =
        state.borrow().menu == Some(VariablesMenu::BindVariable(binding.field.clone()));
    if bind_variable_menu_open {
        row = row.child(bind_variable_menu(model, &binding.field, cx));
    }
    row.into_any_element()
}

/// One collection's mode on the selection, and the way to change it.
fn collection_mode_row<T: VariablesHost>(
    model: &VariablesModel,
    mode: &CollectionMode,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let collection = model
        .collection(&mode.collection)
        .map(|collection| collection.name.clone())
        .unwrap_or_else(|| mode.collection.clone());
    let shown = match &mode.value {
        VariableState::Set(choice) => choice.label(model),
        VariableState::Unset => NOT_SET.to_owned(),
        VariableState::Mixed => MIXED.to_owned(),
    };
    let mut row = div()
        .id(SharedString::from(format!(
            "variables-collection-mode-{}",
            mode.collection
        )))
        .debug_selector(move || format!("variables-collection-mode-{}", mode.collection))
        .flex()
        .items_center()
        .gap_2()
        .px(px(6.0))
        .py(px(6.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs();
    row = row.child(
        div()
            .flex_1()
            .text_color(rgb(theme::TEXT_MUTED))
            .child(collection),
    );
    row = row.child(
        div()
            .id(SharedString::from(format!(
                "variables-collection-mode-{}-value",
                mode.collection
            )))
            .debug_selector(move || format!("variables-collection-mode-{}-value", mode.collection))
            .text_color(rgb(theme::TEXT_SECONDARY))
            .child(shown),
    );
    row = row.child(menu_button(
        &format!("variables-collection-mode-{}-menu", mode.collection),
        &format!("variables-collection-mode-{}-menu", mode.collection),
        state,
        VariablesMenu::SetMode(mode.collection.clone()),
        cx,
    ));
    let set_mode_menu_open =
        state.borrow().menu == Some(VariablesMenu::SetMode(mode.collection.clone()));
    if set_mode_menu_open {
        row = row.child(set_mode_menu(model, &mode.collection, cx));
    }
    row.into_any_element()
}

/// The modes a collection can be set to on the selection.
///
/// "Auto" is the first choice: it is the mode the engine's own
/// `variable.setMode {mode: null}` takes, and it means the layer follows its
/// parent.
fn set_mode_menu<T: VariablesHost>(
    model: &VariablesModel,
    collection: &str,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut menu = menu_box();
    let ids = model.ids.clone();
    menu = menu.child(menu_item(
        &format!("variables-set-mode-{collection}-auto"),
        AUTO,
        VariablesAction::SetMode {
            ids: ids.clone(),
            collection: collection.to_owned(),
            mode: None,
        },
        cx,
    ));
    let Some(chosen) = model.collection(collection) else {
        return menu.into_any_element();
    };
    for mode in &chosen.modes {
        menu = menu.child(menu_item(
            &format!("variables-set-mode-{collection}-{}", mode.id),
            &mode.name,
            VariablesAction::SetMode {
                ids: ids.clone(),
                collection: chosen.id.clone(),
                mode: Some(mode.id.clone()),
            },
            cx,
        ));
    }
    menu.into_any_element()
}

/// A variable's details: its name, its description, and where it is offered.
fn details<T: VariablesHost>(
    variable: &VariableEntry,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(caption("Edit variable"));
    column = column.child(label_field(
        "Name",
        variable.name.clone(),
        VariableField::VariableName,
        Some(variable.id.clone()),
        Some(variable.collection.clone()),
        None,
        None,
        None,
        state,
        cx,
    ));
    column = column.child(label_field(
        "Description",
        variable.description.clone(),
        VariableField::VariableDescription,
        Some(variable.id.clone()),
        Some(variable.collection.clone()),
        None,
        None,
        None,
        state,
        cx,
    ));
    // "Hide from publishing": the variable is left out when the file is
    // published as a library.
    let variable_id = variable.id.clone();
    let hidden = variable.hidden_from_publishing;
    column = column.child(
        div()
            .id(SharedString::from(format!(
                "variables-details-{}-hidden",
                variable.id
            )))
            .debug_selector(move || format!("variables-details-{}-hidden", variable.id))
            .flex()
            .items_center()
            .gap_2()
            .px(px(6.0))
            .py(px(5.0))
            .rounded_md()
            .bg(rgb(theme::SURFACE))
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .cursor_pointer()
            .child(if hidden { "☑" } else { "☐" })
            .child("Hide from publishing")
            .on_click(cx.listener(move |host, _event, _window, cx| {
                host.run_variables_action(
                    VariablesAction::SetHiddenFromPublishing {
                        variable: variable_id.clone(),
                        hidden: !hidden,
                    },
                    cx,
                );
            })),
    );
    // The names code uses for it, per platform.
    column = column.child(caption("Code syntax"));
    for platform in figcraft_doc::CODE_PLATFORMS {
        let current = variable
            .code_syntax
            .get(platform)
            .cloned()
            .unwrap_or_default();
        column = column.child(label_field(
            platform,
            current,
            VariableField::CodeSyntax,
            Some(variable.id.clone()),
            Some(variable.collection.clone()),
            None,
            Some(platform.to_owned()),
            None,
            state,
            cx,
        ));
    }
    // Where pickers offer it.
    let scopes = figcraft_doc::scopes_for(variable.kind);
    if !scopes.is_empty() {
        column = column.child(caption("Scoping: where pickers offer it"));
        let all = variable.scopes.is_none();
        let variable_id = variable.id.clone();
        column = column.child(
            div()
                .id(SharedString::from(format!(
                    "variables-details-{}-scopes-all",
                    variable.id
                )))
                .debug_selector(move || format!("variables-details-{}-scopes-all", variable.id))
                .flex()
                .items_center()
                .gap_2()
                .px(px(6.0))
                .py(px(5.0))
                .rounded_md()
                .bg(rgb(theme::SURFACE))
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .cursor_pointer()
                .child(if all { "☑" } else { "☐" })
                .child("All supported properties")
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    let scopes = if all { Some(Vec::new()) } else { None };
                    host.run_variables_action(
                        VariablesAction::SetScopes {
                            variable: variable_id.clone(),
                            scopes,
                        },
                        cx,
                    );
                })),
        );
        for (scope, label) in scopes {
            let on = variable
                .scopes
                .as_ref()
                .is_none_or(|scopes| scopes.iter().any(|id| id == scope));
            let variable_id = variable.id.clone();
            let scope = (*scope).to_owned();
            let selector_variable = variable.id.clone();
            let selector_scope = scope.clone();
            // The scope list the panel starts from: every scope the variable's
            // kind supports, or the ones it already names.
            let current: Vec<String> = variable.scopes.clone().unwrap_or_else(|| {
                figcraft_doc::scopes_for(variable.kind)
                    .iter()
                    .map(|(id, _)| (*id).to_owned())
                    .collect()
            });
            column = column.child(
                div()
                    .id(SharedString::from(format!(
                        "variables-details-{}-scope-{}",
                        variable.id, scope
                    )))
                    .debug_selector(move || {
                        format!(
                            "variables-details-{}-scope-{}",
                            selector_variable, selector_scope
                        )
                    })
                    .pl(px(16.0))
                    .pr(px(6.0))
                    .py(px(4.0))
                    .rounded_md()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .cursor_pointer()
                    .child(if on { "☑" } else { "☐" })
                    .child(label.to_owned())
                    .on_click(cx.listener(move |host, _event, _window, cx| {
                        let mut list = current.clone();
                        if on {
                            list.retain(|id| id != &scope);
                        } else {
                            list.push(scope.clone());
                        }
                        host.run_variables_action(
                            VariablesAction::SetScopes {
                                variable: variable_id.clone(),
                                scopes: Some(list),
                            },
                            cx,
                        );
                    })),
            );
        }
    }
    column.into_any_element()
}

/// A labelled field: the label, and the value that is being typed into.
#[allow(clippy::too_many_arguments)]
fn label_field<T: VariablesHost>(
    label: &str,
    value: String,
    field: VariableField,
    variable: Option<String>,
    collection: Option<String>,
    mode: Option<String>,
    platform: Option<String>,
    kind: Option<VariableType>,
    state: &Rc<RefCell<VariablesPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    // The code-syntax field's target is its platform, not a mode.
    let target = platform.clone().or_else(|| mode.clone());
    let draft = state
        .borrow()
        .draft
        .as_ref()
        .filter(|draft| {
            draft.field == field
                && draft.variable == variable
                && draft.collection == collection
                && draft.mode == target
        })
        .map(|draft| draft.text.clone());
    let shown = draft.unwrap_or(value);
    let shared = Rc::clone(state);
    let seed = shown.clone();
    let field_name = field_name(field);
    div()
        .id(SharedString::from(format!("variables-field-{field_name}")))
        .debug_selector(move || format!("variables-field-{field_name}"))
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px(px(8.0))
        .py(px(6.0))
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
                    "variables-field-{field_name}-value"
                )))
                .debug_selector(move || format!("variables-field-{field_name}-value"))
                .text_color(rgb(theme::TEXT_SECONDARY))
                .cursor_text()
                .child(shown),
        )
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.draft = Some(VariableDraft {
                    field,
                    text: seed.clone(),
                    select_all: true,
                    collection: collection.clone(),
                    mode: target.clone(),
                    variable: variable.clone(),
                    kind,
                    current: None,
                    renames: Vec::new(),
                    removals: Vec::new(),
                });
            }
            cx.notify();
        }))
        .into_any_element()
}

/// The name of a field, for a selector.
fn field_name(field: VariableField) -> &'static str {
    match field {
        VariableField::Search => "search",
        VariableField::CollectionName => "collection-name",
        VariableField::ModeName => "mode-name",
        VariableField::VariableName => "variable-name",
        VariableField::VariableDescription => "variable-description",
        VariableField::CodeSyntax => "code-syntax",
        VariableField::Value => "value",
        VariableField::GroupName => "group-name",
    }
}

/// The button that opens a menu.
fn menu_button<T: VariablesHost>(
    selector: &str,
    debug: &str,
    state: &Rc<RefCell<VariablesPanelState>>,
    menu: VariablesMenu,
    cx: &mut Context<T>,
) -> AnyElement {
    let shared = Rc::clone(state);
    let open = state.borrow().menu.as_ref() == Some(&menu);
    div()
        .id(SharedString::from(selector.to_owned()))
        .debug_selector(move || debug.to_owned())
        .flex()
        .items_center()
        .justify_center()
        .w(px(20.0))
        .h(px(20.0))
        .rounded_sm()
        .text_color(rgb(theme::TEXT_MUTED))
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer()
        .child(if open { "▾" } else { "⋯" })
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.menu = if open { None } else { Some(menu.clone()) };
                state.draft = None;
            }
            cx.notify();
        }))
        .into_any_element()
}

/// One choice in a menu.
fn menu_item<T: VariablesHost>(
    selector: &str,
    label: &str,
    action: VariablesAction,
    cx: &mut Context<T>,
) -> AnyElement {
    div()
        .id(SharedString::from(selector.to_owned()))
        .debug_selector(move || selector.to_owned())
        .px(px(6.0))
        .py(px(5.0))
        .rounded_sm()
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer()
        .child(label.to_owned())
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_variables_action(action.clone(), cx);
        }))
        .into_any_element()
}

/// The box every menu is drawn in.
fn menu_box() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p(px(6.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER))
}

/// A caption: the dim text that names a group.
fn caption(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A colour's swatch.
fn swatch(color: Option<Color>) -> Div {
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

    use figcraft_doc::{Collection, CollectionId, ModeId, NodeKind, Variables};
    use figcraft_engine::Session;
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

    /// A collection with one mode, in the engine's own model.
    fn collection(id: &str, name: &str, modes: &[&str]) -> Collection {
        Collection {
            id: id.to_owned(),
            name: name.to_owned(),
            modes: modes
                .iter()
                .enumerate()
                .map(|(index, mode)| figcraft_doc::Mode {
                    id: format!("{id}/mode{}", index + 1),
                    name: (*mode).to_owned(),
                })
                .collect(),
            extends: None,
        }
    }

    /// A variable, in the engine's own model.
    fn variable(
        id: &str,
        name: &str,
        collection: &str,
        kind: VariableType,
        values: &[(&str, VariableValue)],
    ) -> Variable {
        Variable {
            id: id.to_owned(),
            name: name.to_owned(),
            collection: collection.to_owned(),
            kind,
            values: values
                .iter()
                .map(|(mode, value)| ((*mode).to_owned(), value.clone()))
                .collect(),
            description: String::new(),
            scopes: None,
            hidden_from_publishing: false,
            extends: None,
            overrides: BTreeSet::new(),
            code_syntax: BTreeMap::new(),
        }
    }

    /// A document with a collection, a variable and a layer.
    fn document_with_variables() -> (Document, NodeId) {
        let (mut doc, id) = document_with();
        doc.variables = Variables {
            collections: vec![collection("c1", "Theme", &["Light", "Dark"])],
            variables: vec![variable(
                "v1",
                "color/brand",
                "c1",
                VariableType::Color,
                &[
                    (
                        "c1/mode1",
                        VariableValue::Color(Color::from_rgb8_const(0x22, 0x66, 0xcc)),
                    ),
                    (
                        "c1/mode2",
                        VariableValue::Color(Color::from_rgb8_const(0x11, 0x22, 0x33)),
                    ),
                ],
            )],
        };
        (doc, id)
    }

    // ── The model ────────────────────────────────────────────────────────────

    #[test]
    fn the_model_reads_the_engines_variables_for_a_node() {
        // The document's collections, their modes and their variables.
        let (doc, id) = document_with_variables();
        let model = VariablesModel::read(&doc, &[id]);

        assert_eq!(model.collections.len(), 1);
        assert_eq!(model.collections[0].name, "Theme");
        assert_eq!(model.collections[0].modes.len(), 2);
        assert_eq!(model.collections[0].modes[0].name, "Light");
        assert_eq!(model.collections[0].modes[1].name, "Dark");
        assert_eq!(model.collections[0].variables.len(), 1);
        assert_eq!(model.collections[0].variables[0].name, "color/brand");
        assert_eq!(model.collections[0].variables[0].leaf(), "brand");
        assert_eq!(model.collections[0].variables[0].group(), "color");

        // And the layer binds nothing, which is a state and not a missing one.
        assert_eq!(model.bindings.len(), 0);
        assert_eq!(model.modes.len(), 1);
        assert_eq!(
            model.collection_mode("c1").map(|mode| &mode.value),
            Some(&VariableState::Set(ModeChoice::Auto)),
            "a layer with no explicit mode follows its parent"
        );
    }

    #[test]
    fn a_node_with_a_variable_bound_reads_the_binding() {
        let (mut doc, id) = document_with_variables();
        doc.get_mut(id)
            .expect("the layer")
            .bound
            .insert("fills/0/color".to_owned(), "v1".to_owned());
        let model = VariablesModel::read(&doc, &[id]);

        assert_eq!(model.bindings.len(), 1);
        assert_eq!(model.bindings[0].field, "fills/0/color");
        assert_eq!(
            model.bindings[0].value,
            VariableState::Set("v1".to_owned()),
            "the layer binds the variable it carries"
        );
        // The bound variable is the engine's own, so the panel can name it.
        assert_eq!(
            model.variable("v1").map(|variable| variable.name.as_str()),
            Some("color/brand")
        );
    }

    #[test]
    fn a_mixed_selection_reads_mixed_for_the_binding_that_differs() {
        // Two layers: one binds a variable, the other binds nothing.
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let first = doc
            .create(page, NodeKind::Rectangle, "One", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let second = doc
            .create(page, NodeKind::Rectangle, "Two", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        doc.variables = Variables {
            collections: vec![collection("c1", "Theme", &["Light"])],
            variables: vec![variable(
                "v1",
                "brand",
                "c1",
                VariableType::Color,
                &[("c1/mode1", VariableValue::Color(Color::BLACK))],
            )],
        };
        doc.get_mut(first)
            .expect("the layer")
            .bound
            .insert("fills/0/color".to_owned(), "v1".to_owned());

        let model = VariablesModel::read(&doc, &[first, second]);
        assert_eq!(
            model.bindings[0].value,
            VariableState::Mixed,
            "one layer binds a variable and the other binds nothing"
        );
        assert_eq!(model.bindings[0].value.word(), MIXED);
    }

    #[test]
    fn two_layers_that_bind_different_variables_are_mixed() {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let first = doc
            .create(page, NodeKind::Rectangle, "One", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let second = doc
            .create(page, NodeKind::Rectangle, "Two", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        doc.variables = Variables {
            collections: vec![collection("c1", "Theme", &["Light"])],
            variables: vec![
                variable(
                    "v1",
                    "one",
                    "c1",
                    VariableType::Color,
                    &[("c1/mode1", VariableValue::Color(Color::BLACK))],
                ),
                variable(
                    "v2",
                    "two",
                    "c1",
                    VariableType::Color,
                    &[("c1/mode1", VariableValue::Color(Color::WHITE))],
                ),
            ],
        };
        doc.get_mut(first)
            .expect("the layer")
            .bound
            .insert("fills/0/color".to_owned(), "v1".to_owned());
        doc.get_mut(second)
            .expect("the layer")
            .bound
            .insert("fills/0/color".to_owned(), "v2".to_owned());

        let model = VariablesModel::read(&doc, &[first, second]);
        assert_eq!(model.bindings[0].value, VariableState::Mixed);
    }

    #[test]
    fn the_layers_mode_is_read_from_the_engine() {
        let (mut doc, id) = document_with_variables();
        doc.get_mut(id)
            .expect("the layer")
            .var_modes
            .insert(CollectionId::from("c1"), ModeId::from("c1/mode2"));
        let model = VariablesModel::read(&doc, &[id]);
        assert_eq!(
            model.collection_mode("c1").map(|mode| &mode.value),
            Some(&VariableState::Set(ModeChoice::Mode("c1/mode2".to_owned())))
        );
    }

    #[test]
    fn the_groups_of_a_collection_are_read_with_their_ancestors() {
        let (mut doc, id) = document_with();
        doc.variables = Variables {
            collections: vec![collection("c1", "Theme", &["Light"])],
            variables: vec![
                variable("v1", "space/sm", "c1", VariableType::Number, &[]),
                variable("v2", "space/md", "c1", VariableType::Number, &[]),
                variable("v3", "color/brand/blue", "c1", VariableType::Color, &[]),
                variable("v4", "color/text", "c1", VariableType::Color, &[]),
                variable("v5", "plain", "c1", VariableType::String, &[]),
            ],
        };
        let model = VariablesModel::read(&doc, &[id]);
        let groups = model.groups("c1");
        let paths: Vec<&str> = groups.iter().map(|group| group.path.as_str()).collect();
        assert_eq!(paths, ["color", "color/brand", "space"]);
        assert_eq!(groups[0].depth, 0);
        assert_eq!(groups[1].depth, 1);
        assert_eq!(groups[0].count, 2, "the two colour variables");
        assert_eq!(groups[1].count, 1, "the one brand variable");
        assert_eq!(groups[2].count, 2, "the two space variables");
    }

    #[test]
    fn the_search_matches_the_whole_name() {
        let (mut doc, id) = document_with();
        doc.variables = Variables {
            collections: vec![collection("c1", "Theme", &["Light"])],
            variables: vec![
                variable("v1", "color/brand/blue", "c1", VariableType::Color, &[]),
                variable("v2", "color/text", "c1", VariableType::Color, &[]),
            ],
        };
        let model = VariablesModel::read(&doc, &[id]);
        let found = model.visible_variables("c1", None, "brand");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "v1");
        assert!(
            model
                .visible_variables("c1", None, "nothing at all")
                .is_empty(),
            "a search that finds nothing is not the same as a collection with no variables"
        );
    }

    #[test]
    fn the_group_filter_narrows_the_table() {
        let (mut doc, id) = document_with();
        doc.variables = Variables {
            collections: vec![collection("c1", "Theme", &["Light"])],
            variables: vec![
                variable("v1", "color/brand/blue", "c1", VariableType::Color, &[]),
                variable("v2", "space/sm", "c1", VariableType::Number, &[]),
            ],
        };
        let model = VariablesModel::read(&doc, &[id]);
        let found = model.visible_variables("c1", Some("color"), "");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "v1");
    }

    #[test]
    fn a_new_collection_and_variable_get_the_first_name_nobody_has() {
        let (mut doc, id) = document_with();
        doc.variables = Variables {
            collections: vec![collection("c1", "Collection", &["Light"])],
            variables: vec![variable("v1", "Color", "c1", VariableType::Color, &[])],
        };
        let model = VariablesModel::read(&doc, &[id]);
        assert_eq!(model.next_collection_name(), "Collection 2");
        assert_eq!(
            model.next_variable_name("c1", VariableType::Color),
            "Color 2",
            "a new colour variable is named after its kind"
        );
        assert_eq!(
            model.next_variable_name("c1", VariableType::Number),
            "Number"
        );
        assert_eq!(model.next_mode_name("c1"), "Mode");
    }

    // ── The commands ─────────────────────────────────────────────────────────

    #[test]
    fn a_command_for_a_change_carries_the_engines_own_id_and_params() {
        // The engine's command ids and the engine's parameter names.
        let (id, params) = VariablesAction::CreateCollection {
            name: "Theme".to_owned(),
        }
        .command();
        assert_eq!(id, "variable.createCollection");
        assert_eq!(params["name"], json!("Theme"));
        assert_eq!(params["modes"], json!(["Mode 1"]));

        let (id, params) = VariablesAction::RenameCollection {
            collection: "c1".to_owned(),
            name: "Brand".to_owned(),
        }
        .command();
        assert_eq!(id, "variable.renameCollection");
        assert_eq!(params["collection"], json!("c1"));
        assert_eq!(params["name"], json!("Brand"));

        let (id, params) = VariablesAction::DeleteCollection {
            collection: "c1".to_owned(),
        }
        .command();
        assert_eq!(id, "variable.deleteCollection");
        assert_eq!(params["collection"], json!("c1"));

        let (id, params) = VariablesAction::AddMode {
            collection: "c1".to_owned(),
            name: "High contrast".to_owned(),
            from: Some("c1/mode1".to_owned()),
        }
        .command();
        assert_eq!(id, "variable.addMode");
        assert_eq!(params["collection"], json!("c1"));
        assert_eq!(params["name"], json!("High contrast"));
        assert_eq!(params["from"], json!("c1/mode1"));

        let (id, params) = VariablesAction::RenameMode {
            collection: "c1".to_owned(),
            mode: "c1/mode2".to_owned(),
            name: "Night".to_owned(),
        }
        .command();
        assert_eq!(id, "variable.renameMode");
        assert_eq!(params["mode"], json!("c1/mode2"));
        assert_eq!(params["name"], json!("Night"));

        let (id, params) = VariablesAction::RemoveMode {
            collection: "c1".to_owned(),
            mode: "c1/mode2".to_owned(),
        }
        .command();
        assert_eq!(id, "variable.removeMode");
        assert_eq!(params["mode"], json!("c1/mode2"));

        let (id, params) = VariablesAction::MoveMode {
            collection: "c1".to_owned(),
            mode: "c1/mode2".to_owned(),
            to: 0,
        }
        .command();
        assert_eq!(id, "variable.moveMode");
        assert_eq!(params["to"], json!(0));

        let (id, params) = VariablesAction::CreateVariable {
            collection: "c1".to_owned(),
            name: "brand".to_owned(),
            kind: VariableType::Color,
            value: VariableValue::Color(Color::from_rgb8_const(0x22, 0x66, 0xcc)),
        }
        .command();
        assert_eq!(id, "variable.create");
        assert_eq!(params["collection"], json!("c1"));
        assert_eq!(params["name"], json!("brand"));
        assert_eq!(params["type"], json!("color"));
        assert_eq!(params["value"], json!("#2266CC"));

        let (id, params) = VariablesAction::SetValue {
            variable: "v1".to_owned(),
            mode: Some("c1/mode2".to_owned()),
            value: VariableValue::Number(12.0),
        }
        .command();
        assert_eq!(id, "variable.set");
        assert_eq!(params["variable"], json!("v1"));
        assert_eq!(params["mode"], json!("c1/mode2"));
        assert_eq!(params["value"], json!(12.0));

        // An alias names the variable it points at.
        let (id, params) = VariablesAction::SetValue {
            variable: "v1".to_owned(),
            mode: None,
            value: VariableValue::Alias("v2".to_owned()),
        }
        .command();
        assert_eq!(id, "variable.set");
        assert_eq!(params["value"], json!({ "alias": "v2" }));
        assert!(
            params.get("mode").is_none(),
            "the default mode is the one the engine picks"
        );

        let (id, params) = VariablesAction::DeleteVariable {
            variable: "v1".to_owned(),
        }
        .command();
        assert_eq!(id, "variable.delete");
        assert_eq!(params["variable"], json!("v1"));

        let (id, params) = VariablesAction::DuplicateVariable {
            variable: "v1".to_owned(),
        }
        .command();
        assert_eq!(id, "variable.duplicate");
        assert_eq!(params["variable"], json!("v1"));

        let (id, params) = VariablesAction::RenameVariable {
            variable: "v1".to_owned(),
            name: "brand/blue".to_owned(),
        }
        .command();
        assert_eq!(id, "variable.update");
        assert_eq!(params["variable"], json!("v1"));
        assert_eq!(params["name"], json!("brand/blue"));

        let (id, params) = VariablesAction::DescribeVariable {
            variable: "v1".to_owned(),
            description: "The brand blue".to_owned(),
        }
        .command();
        assert_eq!(id, "variable.update");
        assert_eq!(params["description"], json!("The brand blue"));

        let (id, params) = VariablesAction::SetScopes {
            variable: "v1".to_owned(),
            scopes: Some(vec!["FRAME_FILL".to_owned()]),
        }
        .command();
        assert_eq!(id, "variable.update");
        assert_eq!(params["scopes"], json!(["FRAME_FILL"]));

        let (id, params) = VariablesAction::SetHiddenFromPublishing {
            variable: "v1".to_owned(),
            hidden: true,
        }
        .command();
        assert_eq!(id, "variable.update");
        assert_eq!(params["hiddenFromPublishing"], json!(true));

        let (id, params) = VariablesAction::SetCodeSyntax {
            variable: "v1".to_owned(),
            platform: "WEB".to_owned(),
            name: "var(--brand)".to_owned(),
        }
        .command();
        assert_eq!(id, "variable.update");
        assert_eq!(params["codeSyntax"], json!({ "WEB": "var(--brand)" }));

        let (id, params) = VariablesAction::ResetOverride {
            variable: "v1".to_owned(),
            mode: Some("c1/mode2".to_owned()),
        }
        .command();
        assert_eq!(id, "variable.resetOverride");
        assert_eq!(params["mode"], json!("c1/mode2"));

        let (id, params) = VariablesAction::Bind {
            ids: vec![NodeId(7)],
            field: "fills/0/color".to_owned(),
            variable: Some("v1".to_owned()),
        }
        .command();
        assert_eq!(id, "variable.bind");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["field"], json!("fills/0/color"));
        assert_eq!(params["variable"], json!("v1"));

        // Unbinding is the same command with a null variable.
        let (id, params) = VariablesAction::Bind {
            ids: vec![NodeId(7)],
            field: "fills/0/color".to_owned(),
            variable: None,
        }
        .command();
        assert_eq!(id, "variable.bind");
        assert!(params["variable"].is_null());

        let (id, params) = VariablesAction::SetMode {
            ids: vec![NodeId(7)],
            collection: "c1".to_owned(),
            mode: Some("c1/mode2".to_owned()),
        }
        .command();
        assert_eq!(id, "variable.setMode");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["collection"], json!("c1"));
        assert_eq!(params["mode"], json!("c1/mode2"));

        // Auto is the same command with a null mode.
        let (id, params) = VariablesAction::SetMode {
            ids: vec![NodeId(7)],
            collection: "c1".to_owned(),
            mode: None,
        }
        .command();
        assert_eq!(id, "variable.setMode");
        assert!(params["mode"].is_null());
    }

    #[test]
    fn a_group_rename_and_delete_are_one_undo_step() {
        let (id, params) = VariablesAction::RenameGroup {
            collection: "c1".to_owned(),
            group: "color".to_owned(),
            renames: vec![("v1".to_owned(), "brand/blue".to_owned())],
        }
        .command();
        assert_eq!(id, "command.batch");
        assert_eq!(params["commands"][0]["command"], json!("variable.update"));
        assert_eq!(params["commands"][0]["params"]["variable"], json!("v1"));
        assert_eq!(params["commands"][0]["params"]["name"], json!("brand/blue"));

        let (id, params) = VariablesAction::DeleteGroup {
            collection: "c1".to_owned(),
            group: "color".to_owned(),
            variables: vec!["v1".to_owned(), "v2".to_owned()],
        }
        .command();
        assert_eq!(id, "command.batch");
        assert_eq!(params["commands"].as_array().map(Vec::len), Some(2));
        assert_eq!(params["commands"][0]["command"], json!("variable.delete"));
    }

    #[test]
    fn the_engine_accepts_the_commands_the_panel_builds() {
        // The write path, end to end: the panel's command, the engine's
        // command, the engine's document.
        let (doc, id) = document_with();
        let mut session = Session::with_document(doc);
        session
            .execute("select.set", &json!({ "ids": [id.0] }))
            .expect("the layer is selected");

        let (command, params) = VariablesAction::CreateCollection {
            name: "Theme".to_owned(),
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the collection is created");
        let doc = session.doc().expect("a document");
        assert_eq!(doc.variables.collections.len(), 1);
        assert_eq!(doc.variables.collections[0].name, "Theme");
        assert_eq!(
            doc.variables.collections[0].modes.len(),
            1,
            "a new collection has one mode"
        );
        let collection = doc.variables.collections[0].id.clone();

        let (command, params) = VariablesAction::CreateVariable {
            collection: collection.clone(),
            name: "brand".to_owned(),
            kind: VariableType::Color,
            value: VariableValue::Color(Color::from_rgb8_const(0x22, 0x66, 0xcc)),
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the variable is created");
        let doc = session.doc().expect("a document");
        assert_eq!(doc.variables.variables.len(), 1);
        let variable = doc.variables.variables[0].id.clone();

        // The layer binds it, and the panel reads the binding back.
        let (command, params) = VariablesAction::Bind {
            ids: vec![id],
            field: "fills/0/color".to_owned(),
            variable: Some(variable.clone()),
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the variable is bound");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(
            node.bound.get("fills/0/color").map(String::as_str),
            Some(variable.as_str())
        );

        // And the layer's mode for the collection.
        let mode = doc.variables.collections[0].modes[0].id.clone();
        let (command, params) = VariablesAction::SetMode {
            ids: vec![id],
            collection: collection.clone(),
            mode: Some(mode.clone()),
        }
        .command();
        session.execute(&command, &params).expect("the mode is set");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(
            node.var_modes.get(&collection).map(String::as_str),
            Some(mode.as_str())
        );
    }

    // ── The panel's own state ────────────────────────────────────────────────

    #[test]
    fn the_search_follows_what_has_been_typed() {
        let mut state = VariablesPanelState::new();
        assert!(!state.is_busy(), "nothing is being typed");
        state.draft = Some(VariableDraft {
            field: VariableField::Search,
            text: String::new(),
            ..VariableDraft::default()
        });
        assert!(state.is_busy(), "a field is open");
        state.handle_key("b");
        state.handle_key("r");
        assert_eq!(state.filter(), "br", "the table follows the query");
        assert!(
            state.handle_key("enter").is_none(),
            "the search is not an edit of the document"
        );
        assert_eq!(state.query, "br", "and it is committed");
        assert!(!state.is_busy(), "the field closed");
    }

    #[test]
    fn typing_a_name_commits_it_and_typing_a_nothing_does_not() {
        let mut state = VariablesPanelState::new();
        state.draft = Some(VariableDraft {
            field: VariableField::VariableName,
            text: "brand/blue".to_owned(),
            variable: Some("v1".to_owned()),
            ..VariableDraft::default()
        });
        let action = state
            .handle_key("enter")
            .expect("the typed name is a change");
        assert_eq!(
            action,
            VariablesAction::RenameVariable {
                variable: "v1".to_owned(),
                name: "brand/blue".to_owned()
            }
        );

        let mut state = VariablesPanelState::new();
        state.draft = Some(VariableDraft {
            field: VariableField::VariableName,
            text: String::new(),
            variable: Some("v1".to_owned()),
            ..VariableDraft::default()
        });
        assert!(state.handle_key("enter").is_none());
    }

    #[test]
    fn typing_a_colour_commits_it_and_typing_a_nonsense_colour_does_not() {
        let mut state = VariablesPanelState::new();
        state.draft = Some(VariableDraft {
            field: VariableField::Value,
            text: "#2266CC".to_owned(),
            variable: Some("v1".to_owned()),
            mode: Some("c1/mode1".to_owned()),
            kind: Some(VariableType::Color),
            ..VariableDraft::default()
        });
        let action = state
            .handle_key("enter")
            .expect("the typed colour is a change");
        match action {
            VariablesAction::SetValue {
                variable,
                mode,
                value,
            } => {
                assert_eq!(variable, "v1");
                assert_eq!(mode.as_deref(), Some("c1/mode1"));
                match value {
                    VariableValue::Color(color) => {
                        assert_eq!(color.to_hex(), "2266CC");
                    }
                    other => panic!("expected a colour, got {other:?}"),
                }
            }
            other => panic!("expected a value, got {other:?}"),
        }

        let mut state = VariablesPanelState::new();
        state.draft = Some(VariableDraft {
            field: VariableField::Value,
            text: "not a colour".to_owned(),
            variable: Some("v1".to_owned()),
            mode: Some("c1/mode1".to_owned()),
            kind: Some(VariableType::Color),
            ..VariableDraft::default()
        });
        assert!(
            state.handle_key("enter").is_none(),
            "a colour that is not a colour commits nothing"
        );
    }

    #[test]
    fn the_first_key_replaces_the_text_that_was_there() {
        let mut state = VariablesPanelState::new();
        state.draft = Some(VariableDraft {
            field: VariableField::VariableDescription,
            text: "The brand blue".to_owned(),
            variable: Some("v1".to_owned()),
            ..VariableDraft::default()
        });
        state.handle_key("x");
        let action = state
            .handle_key("enter")
            .expect("the description is a change");
        assert_eq!(
            action,
            VariablesAction::DescribeVariable {
                variable: "v1".to_owned(),
                description: "x".to_owned()
            }
        );
    }

    // ── The panel, in a window ───────────────────────────────────────────────

    /// A host for the section in a window: it draws the section, remembers
    /// what the section asked for, and does what it is asked.
    struct Host {
        model: VariablesModel,
        state: Rc<RefCell<VariablesPanelState>>,
        asked: Rc<RefCell<Vec<VariablesAction>>>,
    }

    impl Host {
        fn new(model: VariablesModel) -> Self {
            Self {
                model,
                state: Rc::new(RefCell::new(VariablesPanelState::new())),
                asked: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl VariablesHost for Host {
        fn run_variables_action(&mut self, action: VariablesAction, cx: &mut Context<Self>) {
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
            variables_section(&model, &state, cx)
        }
    }

    /// Open a window with the section in it, over the given model.
    fn open_section(
        cx: &mut gpui::TestAppContext,
        model: VariablesModel,
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
    fn the_section_draws_the_document_s_variables(cx: &mut gpui::TestAppContext) {
        // The render test: the section, on its own, in a real window, showing
        // the document's collections, its modes and its variables.
        let (doc, id) = document_with_variables();
        let model = VariablesModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        // The collection is on screen, with its count.
        let collection = bounds(window, "variables-collection-c1");
        assert!(
            collection.size.height > px(0.0),
            "the collection is on screen: {collection:?}"
        );
        assert!(
            window
                .debug_bounds("variables-collection-c1-count")
                .is_some(),
            "the collection says how many variables it holds"
        );
        // The modes are on screen, by the engine's own ids.
        let mode = bounds(window, "variables-mode-c1/mode1");
        assert!(
            mode.size.height > px(0.0),
            "the mode is on screen: {mode:?}"
        );
        // And the variable's value in that mode.
        let value = bounds(window, "variables-value-v1-c1/mode1");
        assert!(
            value.size.height > px(0.0),
            "the value is on screen: {value:?}"
        );
    }

    #[gpui::test]
    fn the_section_shows_the_empty_state_when_there_are_no_collections(
        cx: &mut gpui::TestAppContext,
    ) {
        // figcraft's empty state, word for word.
        let (doc, id) = document_with();
        let model = VariablesModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let selector: &'static str =
            Box::leak(format!("variables-empty-{NO_COLLECTIONS}").into_boxed_str());
        assert!(
            window.debug_bounds(selector).is_some(),
            "the section says figcraft's own words"
        );
        assert!(
            window
                .debug_bounds("variables-empty-create-collection")
                .is_some(),
            "and offers the way to create one"
        );
    }

    #[gpui::test]
    fn the_section_shows_the_variable_a_node_is_bound_to(cx: &mut gpui::TestAppContext) {
        // The node's own half: what the layer binds.
        let (mut doc, id) = document_with_variables();
        doc.get_mut(id)
            .expect("the layer")
            .bound
            .insert("fills/0/color".to_owned(), "v1".to_owned());
        let model = VariablesModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let binding = bounds(window, "variables-binding-fills/0/color");
        assert!(
            binding.size.height > px(0.0),
            "the binding is on screen: {binding:?}"
        );
        let selector: &'static str = Box::leak(
            "variables-binding-fills/0/color-value"
                .to_owned()
                .into_boxed_str(),
        );
        assert!(
            window.debug_bounds(selector).is_some(),
            "the binding says which variable it holds"
        );
        assert!(
            window
                .debug_bounds("variables-binding-fills/0/color-unbind")
                .is_some(),
            "and offers the way to unbind it"
        );
    }

    #[gpui::test]
    fn creating_a_collection_asks_the_engine_to_make_one(cx: &mut gpui::TestAppContext) {
        let (doc, id) = document_with();
        let model = VariablesModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let button = bounds(window, "variables-create-collection");
        window.simulate_click(button.center(), gpui::Modifiers::none());
        window.run_until_parked();

        let asked = root.read_with(cx, |host, _| host.asked.borrow().clone());
        assert_eq!(asked.len(), 1, "the section asked for one change");
        assert_eq!(
            asked[0],
            VariablesAction::CreateCollection {
                name: "Collection".to_owned()
            }
        );
        // And the command it would run is the engine's own.
        let (command, params) = asked[0].command();
        assert_eq!(command, "variable.createCollection");
        assert_eq!(params["name"], json!("Collection"));
    }
}
