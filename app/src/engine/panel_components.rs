//! The components section of Spool's Design panel.
//!
//! figcraft's `crates/ui-egui/src/component_panel.rs` is the Design panel's
//! component, property and instance sections. This is the same section in
//! Spool's frontend: the same rows, the same labels, the same actions and the
//! same empty state, read out of the engine's document.
//!
//! # What the source can say
//!
//! Spool is source-backed. HTML and CSS are the authored source and
//! `lamine.yaml` holds the structure, so a component is only something the
//! panel may edit if `lamine.yaml` can say it.
//!
//! It can. `docs/02-document-and-source-model.md` puts "components,
//! instances, slots, variants, component property definitions and overrides"
//! in Owner B — Spool-specific facts in `lamine.yaml` — and the schema spells
//! them:
//!
//! | the panel's row | the source construct |
//! |---|---|
//! | a main component | `kind: component` |
//! | a component set | `kind: componentSet` |
//! | an instance | `kind: instance`, and `component: <id>` for the main it mirrors |
//! | a layer's overrides | `overrides: [...]` |
//! | a layer bound to a component property | `prop_refs: {…}` |
//! | a set's variants | the set's `children`, named `Property=Value` |
//!
//! `spool-source`'s reader compiles every one of them, so an instance in Spool
//! is a real `NodeKind::Instance` and this panel reads the engine's own state
//! rather than a second copy of it.
//!
//! # What the source cannot say yet
//!
//! Two things the engine holds have no field in `lamine.yaml`:
//!
//! - a component's **property definitions**: the map that says a component has
//!   a boolean, a text, an instance swap or a slot, with their defaults and
//!   their preferred components;
//! - an **instance's property values**: the values an instance gives the
//!   component's properties.
//!
//! `crates/spool-source/src/reader.rs` compiles both empty, so a panel that
//! edited them would be editing something the source cannot carry and the
//! change would be gone the next time the project was opened. Those rows are
//! drawn with their value, are not editable, and say why where the row is —
//! rather than leaving the user to find out by saving.
//!
//! # The save path
//!
//! A change made here goes into the engine's document. The save path
//! (`app/src/engine/source_edits.rs`) is what turns that document back into
//! source, and today it compares a layer's styles, its text and its geometry —
//! the things CSS owns — and does not yet write component structure back. A
//! structural change made here therefore stays in the editor's document until
//! that lands, and the panel says so instead of implying the file already
//! holds it.
//!
//! # Not ported
//!
//! - The **Assets panel** — the library of components a file publishes and the
//!   components it can instantiate from. `lamine.yaml` has no libraries
//!   section, so there is nothing for such a panel to read.
//! - **Free-text editing** of a name, a default or a description. Spool's
//!   Design panel has no text-input widget — the Inspector's only inline
//!   editor is a number field — and every value that would be typed here is
//!   one `lamine.yaml` cannot carry today, apart from a variant's name.
//! - The **variable binding** figcraft offers beside a property value: a
//!   component property bound to a variable is `variable.bind` with a
//!   `componentProperties/<name>` field, and the Variables panel owns that
//!   surface.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use figcraft_doc::{Document, Node, NodeId, NodeKind, PropDef};
use figcraft_engine::components as engine;
use gpui::{
    div, prelude::*, px, rgb, AnyElement, Context, Div, FontWeight, IntoElement, ParentElement,
    SharedString, Stateful, Styled,
};
use serde_json::Value;

use crate::theme;

/// The engine's identity for a layer.
///
/// The engine's [`NodeId`] *is* the manifest's node id — the `id:` in
/// `lamine.yaml` — as a `u64`, so this is the same number the metadata holds.
pub use figcraft_doc::NodeId as EngineObjectId;

// ── The words the panel says ────────────────────────────────────────────────
//
// figcraft's labels, kept as they are: a panel that says "Reset all changes"
// in one place and "Reset" in another is two panels wearing one name.

/// The title of the instance section.
const INSTANCE: &str = "Instance";
/// The title of the instance section when the instance is inside another one.
const NESTED_INSTANCE: &str = "Nested instance";
/// The title of a component's property section.
const PROPERTIES: &str = "Properties";
/// The title of a component's description section.
const DESCRIPTION: &str = "Description";
/// The title of a slot's section.
const SLOT: &str = "Slot";
/// The title of the overrides section.
const OVERRIDES: &str = "Overrides";
/// The title of the section that binds a layer to a component property.
const COMPONENT_PROPERTY: &str = "Component property";

/// The button that resets every change an instance carries.
const RESET_ALL_CHANGES: &str = "Reset all changes";
/// The button that detaches an instance.
const DETACH_INSTANCE: &str = "Detach instance";
/// The button that opens the instance's other actions.
const MORE_INSTANCE_ACTIONS: &str = "More instance actions";
/// The action that selects the main component an instance mirrors.
const GO_TO_MAIN: &str = "Go to main component";
/// The action that copies an instance's changes onto its main component.
const PUSH_CHANGES: &str = "Push changes to main component";
/// The action that brings a deleted main component back.
const RESTORE_MAIN: &str = "Restore main component";
/// The name the panel gives a main component that has been deleted.
const MAIN_DELETED: &str = "Main component deleted";
/// The toggle that exposes a nested instance's properties.
const EXPOSE_PROPERTIES: &str = "Expose properties to instances";
/// The action that resets every override on a layer.
const RESET_ALL_OVERRIDES: &str = "Reset all overrides on this layer";
/// The action that resets one override on a layer.
const RESET_THIS_OVERRIDE: &str = "Reset this override";
/// The action that adds a variant to a component set.
const ADD_VARIANT: &str = "Add variant";
/// The action that selects a slot.
const SELECT_SLOT: &str = "Select slot";
/// The action that puts a slot back to the main's default content.
const RESET_SLOT: &str = "Reset slot";
/// The action that creates a component property.
const CREATE_PROPERTY: &str = "Create property…";
/// The action that makes a frame a slot.
const MAKE_SLOT: &str = "Make slot";
/// The action that removes a frame's slot binding.
const REMOVE_SLOT: &str = "Remove slot";
/// The action that unbinds a layer from a component property.
const DETACH_PROPERTY: &str = "Detach property";
/// The heading of a menu's preferred group.
const PREFERRED: &str = "Preferred";
/// The heading of a menu's group of every component.
const ALL_COMPONENTS: &str = "All components";

/// The note a component with no properties carries.
const NO_PROPERTIES_NOTE: &str = "Add properties to make instances configurable";
/// The note a component set with colliding variants carries.
const VARIANT_CONFLICT_NOTE: &str =
    "Some variants have the same property values: give each one a unique combination";
/// The panel's empty state for a selection of nothing.
const NO_SELECTION: &str = "No selection";
/// What the panel says when there is nothing selected.
const NO_SELECTION_NOTE: &str = "Select an object on the canvas or in Layers.";
/// The panel's empty state for a layer that is neither of them.
const NOT_A_COMPONENT: &str = "This layer is not a component or an instance.";

/// Why an instance's property values cannot be edited here.
const INSTANCE_VALUES_REASON: &str =
    "lamine.yaml has no field for an instance's property values, so a value set here would not \
     survive a reopen";
/// Why a component's property definitions cannot be edited here.
const COMPONENT_PROPERTIES_REASON: &str =
    "lamine.yaml has no field for a component's properties, so a property added here would not \
     survive a reopen";
/// Why a component's description cannot be edited here.
const DESCRIPTION_REASON: &str = "lamine.yaml has no field for a component's description";
/// Why the exposed flag cannot be set here.
const EXPOSED_REASON: &str = "lamine.yaml has no field for an exposed instance";
/// What the save path does with a component change today.
const SAVE_PATH_NOTE: &str =
    "Component structure is not written back to lamine.yaml yet, so a change made here stays in \
     the editor's document.";

// ── The model ───────────────────────────────────────────────────────────────

/// What the components section shows about the selection.
///
/// A *model*, not a widget: it reads the engine's document and says what the
/// panel would draw, so the reading can be tested without a window and the
/// panel can be drawn without reading.
#[derive(Clone, Debug, PartialEq)]
pub struct ComponentsModel {
    /// The selection the section is showing.
    pub selection: ComponentsSelection,
    /// Every component the document holds, for the swap and fill menus.
    pub components: Vec<LocalComponent>,
}

impl ComponentsModel {
    /// Read the components section's model out of the engine's document.
    ///
    /// figcraft's panel is drawn for a selection of exactly one layer, and so
    /// is this one: a selection of several layers has no single main
    /// component, no single set of overrides and no single instance to talk
    /// about. A layer the document does not hold is not shown either — it has
    /// been deleted, and a panel that drew it would be drawing a ghost.
    pub fn read(doc: &Document, selection: &[NodeId]) -> Self {
        let components = local_components(doc);
        let selection = match selection {
            [id] => match doc.node(*id) {
                Some(node) => ComponentsSelection::One(layer(doc, node)),
                None => ComponentsSelection::Empty,
            },
            _ => ComponentsSelection::Empty,
        };
        Self {
            selection,
            components,
        }
    }

    /// The model of a document with nothing selected.
    pub fn empty() -> Self {
        Self {
            selection: ComponentsSelection::Empty,
            components: Vec::new(),
        }
    }
}

/// The selection the section is showing.
#[allow(
    clippy::large_enum_variant,
    reason = "a layer carries its whole story; boxing it would only move it"
)]
#[derive(Clone, Debug, PartialEq)]
pub enum ComponentsSelection {
    /// Nothing is selected.
    Empty,
    /// One layer is selected.
    One(ComponentsLayer),
}

/// One layer, with everything the components section says about it.
#[derive(Clone, Debug, PartialEq)]
pub struct ComponentsLayer {
    /// The engine's identity for this layer.
    pub id: EngineObjectId,
    /// The layer's name.
    pub name: String,
    /// What the layer is, in the components vocabulary.
    pub role: LayerRole,
    /// The override groups the layer carries.
    pub overrides: Vec<LayerOverride>,
    /// The component property each bindable field is bound to.
    pub bindings: Vec<LayerBinding>,
    /// The slot the layer stands for, when it is one.
    pub slot: Option<SlotView>,
}

/// What a layer is, in the components vocabulary.
#[derive(Clone, Debug, PartialEq)]
pub enum LayerRole {
    /// An ordinary layer: not a component, not an instance.
    Layer,
    /// An instance of a main component.
    Instance(InstanceView),
    /// A main component.
    Component(ComponentView),
    /// A component set: its children are its variants.
    ComponentSet(ComponentView),
}

/// An instance, with what the panel says about it.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceView {
    /// The main component the instance mirrors.
    pub main: MainView,
    /// Whether the instance is inside another instance.
    pub nested: bool,
    /// Whether the instance sits inside a main component, so that its
    /// properties can be exposed on the main's own instances.
    pub exposable: bool,
    /// Whether the instance exposes its properties to the main's instances.
    pub exposed: bool,
    /// The main component's description, when it has one.
    pub description: Option<String>,
    /// The variant properties of the main's set, and what this instance shows.
    pub variants: Vec<InstanceProperty>,
    /// The main component's properties, and what this instance gives them.
    pub properties: Vec<InstanceProperty>,
    /// The nested instances this one exposes, and their properties.
    pub exposed_copies: Vec<ExposedInstance>,
}

/// The main component an instance mirrors.
#[derive(Clone, Debug, PartialEq)]
pub struct MainView {
    /// The main's node id.
    pub id: EngineObjectId,
    /// The main's name, or the panel's word for a main that is gone.
    pub name: String,
    /// Whether the main component has been deleted.
    pub missing: bool,
    /// The set the main is a variant of.
    pub set: Option<EngineObjectId>,
}

/// A main component or a component set.
#[derive(Clone, Debug, PartialEq)]
pub struct ComponentView {
    /// The component's node id.
    pub id: EngineObjectId,
    /// The component's name.
    pub name: String,
    /// The component's description, when it has one.
    pub description: Option<String>,
    /// The component's own properties.
    pub properties: Vec<ComponentProperty>,
    /// The set's variant matrix: one row per variant property.
    pub variants: Vec<VariantProperty>,
    /// Whether this is a set rather than a single component.
    pub is_set: bool,
    /// Whether two of the set's variants share the same property values.
    pub conflicts: bool,
}

/// A property a main component declares.
#[derive(Clone, Debug, PartialEq)]
pub struct ComponentProperty {
    /// The property's name.
    pub name: String,
    /// What kind of property it is.
    pub kind: PropertyKind,
    /// The default the component declares.
    pub default: PropertyValue,
    /// The components offered first, for an instance swap or a slot.
    pub preferred: Vec<NodeId>,
}

/// One row of a component set's variant matrix.
#[derive(Clone, Debug, PartialEq)]
pub struct VariantProperty {
    /// The property's name.
    pub name: String,
    /// The values the variants take.
    pub options: Vec<String>,
    /// The default: the value of the top-left variant.
    pub default: String,
}

/// A property of a main component, and the value an instance gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceProperty {
    /// The property's name.
    pub name: String,
    /// What kind of property it is.
    pub kind: PropertyKind,
    /// The value this instance gives the property.
    pub value: PropertyValue,
}

/// A nested instance an instance exposes, and its properties.
#[derive(Clone, Debug, PartialEq)]
pub struct ExposedInstance {
    /// The nested instance's node id.
    pub id: EngineObjectId,
    /// The nested instance's name.
    pub name: String,
    /// The properties it exposes.
    pub properties: Vec<InstanceProperty>,
}

/// An override group a layer carries.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerOverride {
    /// The engine's name for the group.
    pub group: String,
    /// The panel's label for it.
    pub label: String,
}

/// A field of a layer bound to a component property.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerBinding {
    /// The engine's name for the field: `visible`, `characters`,
    /// `mainComponent` or `slot`.
    pub field: String,
    /// The panel's label for the field.
    pub label: String,
    /// The property the field is bound to, when it is bound.
    pub property: Option<String>,
    /// The properties that could be bound to the field.
    pub candidates: Vec<String>,
}

/// A slot: a frame a component's instances fill with their own content.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotView {
    /// The slot's node id.
    pub id: EngineObjectId,
    /// The slot property's name.
    pub name: String,
    /// Whether the slot holds the instance's own content.
    pub edited: bool,
    /// How many layers the slot holds.
    pub layers: usize,
    /// The components offered first.
    pub preferred: Vec<NodeId>,
}

/// A component a project can instantiate from.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalComponent {
    /// The component's node id.
    pub id: EngineObjectId,
    /// The component's name.
    pub name: String,
    /// The page the component is on.
    pub page: Option<NodeId>,
    /// The page's name, for the menu's grouping.
    pub page_name: String,
}

/// What kind of component property it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyKind {
    /// A boolean, shown as a checkbox.
    Boolean,
    /// A text, shown as a field.
    Text,
    /// Another component's main, chosen from a menu.
    InstanceSwap,
    /// A value of the component set's variant matrix.
    Variant,
    /// Content a frame takes, rather than a value.
    Slot,
}

/// A property's value, as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum PropertyValue {
    /// A boolean.
    Boolean(bool),
    /// A text.
    Text(String),
    /// Another component's main.
    Instance {
        /// The component's id.
        id: Option<NodeId>,
        /// The component's name, or the panel's word for one that is gone.
        name: String,
    },
    /// A value of the variant matrix.
    Variant(String),
    /// A slot holds content rather than a value.
    Slot,
}

impl PropertyValue {
    /// The text the panel shows for this value.
    pub fn text(&self) -> String {
        match self {
            Self::Boolean(on) => {
                if *on {
                    "True".to_owned()
                } else {
                    "False".to_owned()
                }
            }
            Self::Text(text) => text.clone(),
            Self::Instance { name, .. } => name.clone(),
            Self::Variant(value) => value.clone(),
            Self::Slot => SLOT.to_owned(),
        }
    }
}

// ── Reading the engine's document ──────────────────────────────────────────

/// The components a document holds, for the swap and fill menus.
///
/// figcraft's `list_components`: a component that is a variant of a set is
/// listed by its set, and a component inside an instance is listed by the
/// instance — so the list holds what a project can instantiate from.
fn local_components(doc: &Document) -> Vec<LocalComponent> {
    let mut out: Vec<LocalComponent> = Vec::new();
    for page in &doc.pages {
        for id in doc.subtree(*page) {
            let Some(node) = doc.node(id) else { continue };
            let listed = match node.kind {
                NodeKind::Component { .. } => engine::variant_set(doc, id).is_none(),
                NodeKind::ComponentSet { .. } => true,
                _ => false,
            };
            if !listed || engine::inside_instance(doc, id) {
                continue;
            }
            let page_of = doc.page_of(id);
            out.push(LocalComponent {
                id,
                name: node.name.clone(),
                page: page_of,
                page_name: page_of
                    .and_then(|page| doc.node(page))
                    .map(|page| page.name.clone())
                    .unwrap_or_default(),
            });
        }
    }
    out
}

/// One layer, with everything the components section says about it.
fn layer(doc: &Document, node: &Node) -> ComponentsLayer {
    let id = node.id;
    let role = match &node.kind {
        NodeKind::Instance { component, .. } => {
            LayerRole::Instance(instance_view(doc, node, *component))
        }
        NodeKind::Component { description, .. } => {
            // A variant of a set is shown with the set's properties and the
            // set's matrix: a variant has no properties of its own, and
            // figcraft's panel reads them from the set.
            let owner = engine::variant_set(doc, id).unwrap_or(id);
            let is_set = owner != id;
            LayerRole::Component(ComponentView {
                id: owner,
                name: node.name.clone(),
                description: non_empty(description),
                properties: shared_properties(doc, owner),
                variants: if is_set {
                    variant_properties(doc, owner)
                } else {
                    Vec::new()
                },
                is_set,
                conflicts: is_set && !engine::variant_conflicts(doc, owner).is_empty(),
            })
        }
        NodeKind::ComponentSet { description } => LayerRole::ComponentSet(ComponentView {
            id,
            name: node.name.clone(),
            description: non_empty(description),
            properties: shared_properties(doc, id),
            variants: variant_properties(doc, id),
            is_set: true,
            conflicts: !engine::variant_conflicts(doc, id).is_empty(),
        }),
        _ => LayerRole::Layer,
    };

    // The slot a layer inside an instance stands for.
    let slot = engine::owning_instance(doc, id)
        .and_then(|_| engine::slot_def(doc, id))
        .map(|(name, preferred)| SlotView {
            id,
            name: name.to_owned(),
            edited: engine::slot_edited(doc, id),
            layers: doc.node(id).map(|node| node.children.len()).unwrap_or(0),
            preferred: preferred.to_vec(),
        });

    // The overrides a layer carries. figcraft only draws them for a layer
    // inside an instance that is not an instance itself.
    let overrides = if matches!(node.kind, NodeKind::Instance { .. })
        || node.source.is_none()
        || engine::owning_instance(doc, id).is_none()
    {
        Vec::new()
    } else {
        node.overrides
            .iter()
            .filter(|group| engine::OVERRIDE_GROUPS.contains(&group.as_str()))
            .map(|group| LayerOverride {
                group: group.clone(),
                label: group_label(group),
            })
            .collect()
    };

    // The component property a layer inside a main component is bound to.
    let bindings = layer_bindings(doc, node);

    ComponentsLayer {
        id,
        name: node.name.clone(),
        role,
        overrides,
        bindings,
        slot,
    }
}

/// An instance, with what the panel says about it.
fn instance_view(doc: &Document, node: &Node, component: NodeId) -> InstanceView {
    let id = node.id;
    let missing = !doc.contains(component);
    let set = engine::variant_set(doc, component);
    // The set names its variants, so a variant's instance shows the set's name.
    let name = set
        .or(Some(component))
        .and_then(|id| doc.node(id))
        .map(|node| node.name.clone())
        .unwrap_or_else(|| MAIN_DELETED.to_owned());

    // The main component's description: the set's when the variant has none.
    let description = [Some(component), set]
        .into_iter()
        .flatten()
        .filter_map(|id| match doc.node(id).map(|node| &node.kind) {
            Some(
                NodeKind::Component { description, .. } | NodeKind::ComponentSet { description },
            ) => non_empty(description),
            _ => None,
        })
        .next();

    // The variant properties: what the set offers, and what this instance shows.
    let current = doc
        .node(component)
        .map(|node| engine::variant_props(&node.name))
        .unwrap_or_default();
    let variants = match set {
        Some(set) => engine::variant_options(doc, set)
            .into_iter()
            .map(|(name, _options)| InstanceProperty {
                kind: PropertyKind::Variant,
                value: PropertyValue::Variant(current.get(&name).cloned().unwrap_or_default()),
                name,
            })
            .collect(),
        None => Vec::new(),
    };

    // The main component's own properties, and the values this instance gives
    // them. A value the instance does not carry is the component's default.
    let (defs, values) = match &node.kind {
        NodeKind::Instance { props, .. } => (prop_defs(doc, component), props.clone()),
        _ => (BTreeMap::new(), BTreeMap::new()),
    };
    let properties = defs
        .iter()
        .map(|(name, def)| InstanceProperty {
            name: name.clone(),
            kind: property_kind(def),
            value: instance_value(doc, def, &values, name),
        })
        .collect();

    // The nested instances this one exposes, and their properties. Each copy
    // has a main component of its own, so its properties are its main's.
    let exposed_copies = engine::exposed_copies(doc, id)
        .into_iter()
        .filter_map(|copy| {
            let node = doc.node(copy)?;
            let (defs, values) = match &node.kind {
                NodeKind::Instance { component, props } => {
                    (prop_defs(doc, *component), props.clone())
                }
                _ => (BTreeMap::new(), BTreeMap::new()),
            };
            Some(ExposedInstance {
                id: copy,
                name: node.name.clone(),
                properties: defs
                    .iter()
                    .map(|(name, def)| InstanceProperty {
                        name: name.clone(),
                        kind: property_kind(def),
                        value: instance_value(doc, def, &values, name),
                    })
                    .collect(),
            })
        })
        .collect();

    InstanceView {
        main: MainView {
            id: component,
            name,
            missing,
            set,
        },
        nested: engine::inside_instance(doc, id),
        // A nested instance inside a main component can expose its properties
        // on the main's own instances.
        exposable: !engine::inside_instance(doc, id)
            && engine::owning_component(doc, id).is_some_and(|owner| owner != id),
        exposed: node.exposed,
        description,
        variants,
        properties,
        exposed_copies,
    }
}

/// The properties a main component declares.
fn component_properties(
    doc: &Document,
    defs: &BTreeMap<String, PropDef>,
) -> Vec<ComponentProperty> {
    defs.iter()
        .map(|(name, def)| ComponentProperty {
            name: name.clone(),
            kind: property_kind(def),
            default: default_value(doc, def),
            preferred: preferred_of(def),
        })
        .collect()
}

/// The properties a component set shares with its variants.
///
/// figcraft reads them from the first variant: a shared property lives on
/// every variant, and the set itself holds none.
fn shared_properties(doc: &Document, owner: NodeId) -> Vec<ComponentProperty> {
    let is_set = matches!(
        doc.node(owner).map(|node| &node.kind),
        Some(NodeKind::ComponentSet { .. })
    );
    let first = if is_set {
        doc.node(owner).and_then(|node| {
            node.children.iter().copied().find(|child| {
                matches!(
                    doc.node(*child).map(|n| &n.kind),
                    Some(NodeKind::Component { .. })
                )
            })
        })
    } else {
        Some(owner)
    };
    first
        .map(|first| component_properties(doc, &prop_defs(doc, first)))
        .unwrap_or_default()
}

/// A component set's variant matrix.
fn variant_properties(doc: &Document, set: NodeId) -> Vec<VariantProperty> {
    // The default value of a variant property is the top-left variant's.
    let defaults = engine::default_variant(doc, set)
        .and_then(|id| doc.node(id))
        .map(|node| engine::variant_props(&node.name))
        .unwrap_or_default();
    engine::variant_options(doc, set)
        .into_iter()
        .map(|(name, options)| VariantProperty {
            default: defaults
                .get(&name)
                .cloned()
                .or_else(|| options.first().cloned())
                .unwrap_or_default(),
            name,
            options,
        })
        .collect()
}

/// The property definitions a component declares.
fn prop_defs(doc: &Document, component: NodeId) -> BTreeMap<String, PropDef> {
    match doc.node(component).map(|node| &node.kind) {
        Some(NodeKind::Component { props, .. }) => props.clone(),
        _ => BTreeMap::new(),
    }
}

/// What kind of property a definition describes.
fn property_kind(def: &PropDef) -> PropertyKind {
    match def {
        PropDef::Boolean { .. } => PropertyKind::Boolean,
        PropDef::Text { .. } => PropertyKind::Text,
        PropDef::InstanceSwap { .. } => PropertyKind::InstanceSwap,
        PropDef::Variant { .. } => PropertyKind::Variant,
        PropDef::Slot { .. } => PropertyKind::Slot,
    }
}

/// The default a property definition declares.
fn default_value(doc: &Document, def: &PropDef) -> PropertyValue {
    match def {
        PropDef::Boolean { default } => PropertyValue::Boolean(*default),
        PropDef::Text { default } => PropertyValue::Text(default.clone()),
        PropDef::InstanceSwap { default, .. } => PropertyValue::Instance {
            id: Some(*default),
            name: doc
                .node(*default)
                .map(|node| node.name.clone())
                .unwrap_or_else(|| "Missing".to_owned()),
        },
        PropDef::Variant { default, .. } => PropertyValue::Variant(default.clone()),
        PropDef::Slot { .. } => PropertyValue::Slot,
    }
}

/// The components a property offers first.
fn preferred_of(def: &PropDef) -> Vec<NodeId> {
    match def {
        PropDef::InstanceSwap { preferred, .. } | PropDef::Slot { preferred } => preferred.clone(),
        _ => Vec::new(),
    }
}

/// The value an instance gives a property.
///
/// A value the instance does not carry is the component's default, which is
/// what the engine itself uses.
fn instance_value(
    doc: &Document,
    def: &PropDef,
    values: &BTreeMap<String, Value>,
    name: &str,
) -> PropertyValue {
    let Some(raw) = values.get(name) else {
        return default_value(doc, def);
    };
    match def {
        PropDef::Boolean { .. } => PropertyValue::Boolean(raw.as_bool().unwrap_or(false)),
        PropDef::Text { .. } => PropertyValue::Text(raw.as_str().unwrap_or_default().to_owned()),
        PropDef::InstanceSwap { .. } => {
            let id = figcraft_engine::params::id_of(raw);
            PropertyValue::Instance {
                id,
                name: id
                    .and_then(|id| doc.node(id))
                    .map(|node| node.name.clone())
                    .unwrap_or_else(|| "None".to_owned()),
            }
        }
        PropDef::Variant { .. } => {
            PropertyValue::Variant(raw.as_str().unwrap_or_default().to_owned())
        }
        PropDef::Slot { .. } => PropertyValue::Slot,
    }
}

/// The fields of a layer that can be bound to a component property.
fn layer_bindings(doc: &Document, node: &Node) -> Vec<LayerBinding> {
    // figcraft's rule: a layer inside a main component, that is not a
    // component itself and is not inside an instance.
    if matches!(
        node.kind,
        NodeKind::Component { .. } | NodeKind::ComponentSet { .. }
    ) || engine::inside_instance(doc, node.id)
    {
        return Vec::new();
    }
    let Some(owner) = engine::owning_component(doc, node.id) else {
        return Vec::new();
    };
    let defs = prop_defs(doc, owner);

    let mut fields: Vec<(&str, &str, PropertyKind)> =
        vec![("visible", "Visibility", PropertyKind::Boolean)];
    if node.kind.text().is_some() {
        fields.push(("characters", "Text", PropertyKind::Text));
    }
    if matches!(node.kind, NodeKind::Instance { .. }) {
        fields.push(("mainComponent", "Instance", PropertyKind::InstanceSwap));
    }
    if matches!(node.kind, NodeKind::Frame) {
        fields.push(("slot", SLOT, PropertyKind::Slot));
    }

    fields
        .into_iter()
        .map(|(field, label, kind)| {
            let candidates = defs
                .iter()
                .filter(|(_, def)| kind_matches(kind, def))
                .map(|(name, _)| name.clone())
                .collect();
            LayerBinding {
                field: field.to_owned(),
                label: label.to_owned(),
                property: node.prop_refs.get(field).cloned(),
                candidates,
            }
        })
        .collect()
}

/// Whether a property definition can drive a field.
fn kind_matches(kind: PropertyKind, def: &PropDef) -> bool {
    matches!(
        (kind, def),
        (PropertyKind::Boolean, PropDef::Boolean { .. })
            | (PropertyKind::Text, PropDef::Text { .. })
            | (PropertyKind::InstanceSwap, PropDef::InstanceSwap { .. })
            | (PropertyKind::Slot, PropDef::Slot { .. })
    )
}

/// The panel's label for an override group.
fn group_label(group: &str) -> String {
    match group {
        "fills" => "Fill",
        "strokes" => "Stroke",
        "effects" => "Effects",
        "opacity" => "Opacity & blend",
        "visible" => "Visibility",
        "name" => "Layer name",
        "corners" => "Corner radius",
        "characters" => "Text content",
        "textStyle" => "Text style",
        "padding" => "Padding",
        "gap" => "Gap",
        "alignment" => "Alignment",
        "grids" => "Layout grids",
        "exports" => "Export settings",
        "interactions" => "Interactions",
        "size" => "Size",
        "position" => "Position",
        "component" => "Swapped instance",
        "props" => "Instance properties",
        "slot" => "Slot content",
        other => other,
    }
    .to_owned()
}

/// A description, when it has one.
fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

// ── The changes the panel can ask for ───────────────────────────────────────

/// Whether the authored source can carry a change.
///
/// A change the source has no spelling for is not offered: a panel that edited
/// it would be editing something the project cannot hold, and the change would
/// be gone the next time the project was opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceSupport {
    /// The source has a spelling for the change.
    Expressible,
    /// It does not, and the reason is said instead of the control.
    Unsupported(&'static str),
}

impl SourceSupport {
    /// Whether the change can be made.
    pub fn is_expressible(self) -> bool {
        matches!(self, Self::Expressible)
    }
}

/// A change the components panel can ask for.
///
/// One variant per thing the panel can change, so a change the source cannot
/// carry cannot be written down as though it had one.
#[derive(Clone, Debug, PartialEq)]
pub enum ComponentAction {
    /// Swap an instance for another main component.
    Swap {
        /// The instances to swap.
        ids: Vec<NodeId>,
        /// The main component to show.
        component: NodeId,
    },
    /// Detach an instance: its layers become its own.
    Detach {
        /// The instances to detach.
        ids: Vec<NodeId>,
    },
    /// Reset every change an instance carries.
    ResetInstance {
        /// The instances to reset.
        ids: Vec<NodeId>,
    },
    /// Reset one override group on a layer.
    ResetOverride {
        /// The layer.
        id: NodeId,
        /// The group, or every group when it is `None`.
        group: Option<String>,
    },
    /// Select the main component an instance mirrors.
    GoToMain {
        /// The instance.
        id: NodeId,
    },
    /// Bring a deleted main component back.
    RestoreMain {
        /// The instance.
        id: NodeId,
    },
    /// Copy an instance's changes onto its main component.
    PushChanges {
        /// The instance.
        id: NodeId,
    },
    /// Expose a nested instance's properties on the main's instances.
    ExposeInstance {
        /// The nested instance.
        id: NodeId,
        /// Whether the properties are exposed.
        exposed: bool,
    },
    /// Set an instance's property values.
    SetProperties {
        /// The instances.
        ids: Vec<NodeId>,
        /// The values, by property name.
        properties: BTreeMap<String, Value>,
    },
    /// Fill a slot with a component.
    SlotInsert {
        /// The slot.
        slot: NodeId,
        /// The component to put in it.
        component: NodeId,
    },
    /// Put a slot back to the main's default content.
    ResetSlot {
        /// The slot.
        id: NodeId,
    },
    /// Bind a layer's field to a component property.
    BindProperty {
        /// The layer.
        layer: NodeId,
        /// The property, or `None` to unbind.
        property: Option<String>,
        /// The field: `visible`, `characters`, `mainComponent` or `slot`.
        field: String,
    },
    /// Make a frame a slot.
    MakeSlot {
        /// The frame.
        id: NodeId,
    },
    /// Remove a frame's slot binding.
    RemoveSlot {
        /// The frame.
        id: NodeId,
    },
    /// Select layers.
    Select {
        /// The layers.
        ids: Vec<NodeId>,
    },
    /// Add a variant to a component set.
    AddVariant {
        /// The set.
        id: NodeId,
        /// The property the new variant names.
        property: Option<String>,
        /// The value the new variant takes.
        value: Option<String>,
    },
    /// Rename a variant property, rename one of its values, or choose the
    /// set's default.
    EditVariant {
        /// The set.
        id: NodeId,
        /// The property.
        name: String,
        /// The property's new name.
        new_name: Option<String>,
        /// The value that becomes the default.
        default: Option<String>,
        /// The value to rename.
        value: Option<String>,
        /// The value's new name.
        new_value: Option<String>,
    },
    /// Add a property to a component.
    AddProperty {
        /// The component.
        id: NodeId,
        /// The property's name.
        name: String,
        /// The property's type: `boolean`, `text`, `instanceSwap`, `variant`
        /// or `slot`.
        kind: String,
    },
    /// Edit a component's property.
    EditProperty {
        /// The component.
        id: NodeId,
        /// The property.
        name: String,
        /// The property's new name.
        new_name: Option<String>,
        /// The property's new default.
        default: Option<Value>,
    },
    /// Delete a component's property.
    DeleteProperty {
        /// The component.
        id: NodeId,
        /// The property.
        name: String,
    },
    /// Write a component's description.
    SetDescription {
        /// The component.
        id: NodeId,
        /// The description.
        description: String,
    },
}

impl ComponentAction {
    /// The engine's command id and parameters for this change.
    ///
    /// The engine's own spellings, so a change made from the panel is the
    /// same change the CLI, the keymap and an agent would make.
    pub fn command(&self) -> (String, Value) {
        let (id, params) = match self {
            Self::Swap { ids, component } => (
                "instance.swap",
                params(vec![
                    ("ids", id_list(ids)),
                    ("component", Value::from(component.0)),
                ]),
            ),
            Self::Detach { ids } => ("component.detach", params(vec![("ids", id_list(ids))])),
            Self::ResetInstance { ids } => (
                "component.resetInstance",
                params(vec![("ids", id_list(ids))]),
            ),
            Self::ResetOverride { id, group } => (
                "component.resetOverride",
                params(vec![
                    ("id", Value::from(id.0)),
                    (
                        "group",
                        group.clone().map(Value::String).unwrap_or(Value::Null),
                    ),
                ]),
            ),
            Self::GoToMain { id } => (
                "component.goToMain",
                params(vec![("id", Value::from(id.0))]),
            ),
            Self::RestoreMain { id } => (
                "component.restoreMain",
                params(vec![("id", Value::from(id.0))]),
            ),
            Self::PushChanges { id } => (
                "component.pushChanges",
                params(vec![("id", Value::from(id.0))]),
            ),
            Self::ExposeInstance { id, exposed } => (
                "component.exposeInstance",
                params(vec![
                    ("id", Value::from(id.0)),
                    ("exposed", Value::Bool(*exposed)),
                ]),
            ),
            Self::SetProperties { ids, properties } => (
                "instance.setProperties",
                params(vec![
                    ("ids", id_list(ids)),
                    (
                        "properties",
                        Value::Object(
                            properties
                                .iter()
                                .map(|(name, value)| (name.clone(), value.clone()))
                                .collect(),
                        ),
                    ),
                ]),
            ),
            Self::SlotInsert { slot, component } => (
                "instance.slotInsert",
                params(vec![
                    ("id", Value::from(slot.0)),
                    ("component", Value::from(component.0)),
                ]),
            ),
            Self::ResetSlot { id } => (
                "instance.resetSlot",
                params(vec![("id", Value::from(id.0))]),
            ),
            Self::BindProperty {
                layer,
                property,
                field,
            } => (
                "component.bindProperty",
                params(vec![
                    ("layer", Value::from(layer.0)),
                    (
                        "property",
                        property.clone().map(Value::String).unwrap_or(Value::Null),
                    ),
                    ("field", Value::String(field.clone())),
                ]),
            ),
            Self::MakeSlot { id } => (
                "component.makeSlot",
                params(vec![("id", Value::from(id.0))]),
            ),
            Self::RemoveSlot { id } => (
                "component.removeSlot",
                params(vec![("id", Value::from(id.0))]),
            ),
            Self::Select { ids } => ("select.set", params(vec![("ids", id_list(ids))])),
            Self::AddVariant {
                id,
                property,
                value,
            } => (
                "component.addVariant",
                params(vec![
                    ("id", Value::from(id.0)),
                    (
                        "property",
                        property.clone().map(Value::String).unwrap_or(Value::Null),
                    ),
                    (
                        "value",
                        value.clone().map(Value::String).unwrap_or(Value::Null),
                    ),
                ]),
            ),
            Self::EditVariant {
                id,
                name,
                new_name,
                default,
                value,
                new_value,
            } => (
                "component.editProperty",
                params(vec![
                    ("id", Value::from(id.0)),
                    ("name", Value::String(name.clone())),
                    (
                        "newName",
                        new_name.clone().map(Value::String).unwrap_or(Value::Null),
                    ),
                    (
                        "default",
                        default.clone().map(Value::String).unwrap_or(Value::Null),
                    ),
                    (
                        "value",
                        value.clone().map(Value::String).unwrap_or(Value::Null),
                    ),
                    (
                        "newValue",
                        new_value.clone().map(Value::String).unwrap_or(Value::Null),
                    ),
                ]),
            ),
            Self::AddProperty { id, name, kind } => (
                "component.addProperty",
                params(vec![
                    ("id", Value::from(id.0)),
                    ("name", Value::String(name.clone())),
                    ("type", Value::String(kind.clone())),
                ]),
            ),
            Self::EditProperty {
                id,
                name,
                new_name,
                default,
            } => (
                "component.editProperty",
                params(vec![
                    ("id", Value::from(id.0)),
                    ("name", Value::String(name.clone())),
                    (
                        "newName",
                        new_name.clone().map(Value::String).unwrap_or(Value::Null),
                    ),
                    ("default", default.clone().unwrap_or(Value::Null)),
                ]),
            ),
            Self::DeleteProperty { id, name } => (
                "component.deleteProperty",
                params(vec![
                    ("id", Value::from(id.0)),
                    ("name", Value::String(name.clone())),
                ]),
            ),
            Self::SetDescription { id, description } => (
                "node.set",
                params(vec![
                    ("id", Value::from(id.0)),
                    (
                        "props",
                        Value::Object(
                            [("description".to_owned(), Value::String(description.clone()))]
                                .into_iter()
                                .collect(),
                        ),
                    ),
                ]),
            ),
        };
        (id.to_owned(), params)
    }

    /// Whether Spool's source can carry this change.
    ///
    /// The source format is the boundary: a change it cannot spell is not
    /// offered, because the project could not hold it.
    pub fn support(&self) -> SourceSupport {
        match self {
            Self::Swap { .. }
            | Self::Detach { .. }
            | Self::ResetInstance { .. }
            | Self::ResetOverride { .. }
            | Self::GoToMain { .. }
            | Self::RestoreMain { .. }
            | Self::PushChanges { .. }
            | Self::SlotInsert { .. }
            | Self::ResetSlot { .. }
            | Self::BindProperty { .. }
            | Self::MakeSlot { .. }
            | Self::RemoveSlot { .. }
            | Self::Select { .. }
            | Self::AddVariant { .. }
            | Self::EditVariant { .. } => SourceSupport::Expressible,
            Self::ExposeInstance { .. } => SourceSupport::Unsupported(EXPOSED_REASON),
            Self::SetProperties { .. } => SourceSupport::Unsupported(INSTANCE_VALUES_REASON),
            Self::AddProperty { .. } | Self::EditProperty { .. } | Self::DeleteProperty { .. } => {
                SourceSupport::Unsupported(COMPONENT_PROPERTIES_REASON)
            }
            Self::SetDescription { .. } => SourceSupport::Unsupported(DESCRIPTION_REASON),
        }
    }
}

/// A JSON object, built one entry at a time.
fn params(entries: Vec<(&str, Value)>) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in entries {
        map.insert(key.to_owned(), value);
    }
    Value::Object(map)
}

/// The `ids` array a command takes.
fn id_list(ids: &[NodeId]) -> Value {
    Value::Array(ids.iter().map(|id| Value::from(id.0)).collect())
}

// ── The panel's own state ───────────────────────────────────────────────────

/// Which menu is open.
///
/// One menu at a time: a second one opening closes the first, which is what a
/// menu that opens another does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComponentsMenu {
    /// The instance's other actions.
    InstanceActions,
    /// The main component's swap menu.
    SwapMain,
    /// A slot's fill menu.
    SlotFill,
    /// The binding menu of a field.
    Bind(String),
    /// The default-value menu of a variant property.
    VariantDefault(String),
}

/// The panel's own state.
///
/// The state is a cell the surface that hosts the panel owns, so a menu stays
/// open across repaints while the panel itself stays a function.
#[derive(Clone, Debug, Default)]
pub struct ComponentsPanelState {
    /// Which menu is open, if any.
    pub menu: Option<ComponentsMenu>,
}

/// What a surface that hosts the components panel must be able to do.
///
/// The panel draws and asks; the surface owns the engine and runs the changes.
/// The panel never touches the document itself, so the engine's command
/// surface stays the one operation boundary.
pub trait ComponentsHost: Sized + 'static {
    /// Run a change through the engine.
    fn run_component_action(&mut self, action: ComponentAction, cx: &mut Context<Self>);
}

// ── The panel ───────────────────────────────────────────────────────────────

/// The components section, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own state,
/// and the surface that runs the changes. It returns an element, so the
/// Inspector can put it in a section of its own and a test can put it in a
/// window on its own.
pub fn components_section<T: ComponentsHost>(
    model: &ComponentsModel,
    state: &Rc<RefCell<ComponentsPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2();

    let ComponentsSelection::One(layer) = &model.selection else {
        return column
            .child(
                section_body()
                    .child(
                        div()
                            .id(SharedString::from("components-empty-title"))
                            .debug_selector(|| "components-empty-title".to_owned())
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(theme::TEXT))
                            .child(NO_SELECTION),
                    )
                    .child(note(NO_SELECTION_NOTE)),
            )
            .into_any_element();
    };

    // The section for what the layer is.
    match &layer.role {
        LayerRole::Instance(instance) => {
            column = column.child(instance_section(model, layer, instance, state, cx));
        }
        LayerRole::Component(component) | LayerRole::ComponentSet(component) => {
            column = column.child(properties_section(component, state, cx));
            column = column.child(description_section(component));
        }
        LayerRole::Layer => {}
    }
    // The slot the layer stands for.
    if let Some(slot) = &layer.slot {
        column = column.child(slot_section(model, slot, state, cx));
    }

    // The overrides the layer carries.
    if !layer.overrides.is_empty() {
        column = column.child(overrides_section(layer, cx));
    }

    // The component property the layer is bound to.
    if !layer.bindings.is_empty() {
        column = column.child(bind_section(layer, state, cx));
    }

    // A layer that is none of these has nothing to say.
    if matches!(layer.role, LayerRole::Layer)
        && layer.slot.is_none()
        && layer.overrides.is_empty()
        && layer.bindings.is_empty()
    {
        column = column.child(section_body().child(note(NOT_A_COMPONENT)));
    } else if !matches!(layer.role, LayerRole::Layer) {
        // The save path does not write component structure back yet, so a
        // change made here stays in the editor's document. Said where the
        // change is made, rather than left for the user to find out by saving.
        column = column.child(section_body().child(note(SAVE_PATH_NOTE)));
    }

    column.into_any_element()
}

/// The instance section: the main component, the properties, the actions.
fn instance_section<T: ComponentsHost>(
    model: &ComponentsModel,
    layer: &ComponentsLayer,
    instance: &InstanceView,
    state: &Rc<RefCell<ComponentsPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let mut body = div().flex().flex_col().gap_2();

    // The main component, with the menu that swaps it and the action that
    // selects it.
    let main_label = if instance.main.missing {
        MAIN_DELETED.to_owned()
    } else {
        instance.main.name.clone()
    };
    let mut entries = Vec::new();
    let mut page: Option<Option<NodeId>> = None;
    for component in &model.components {
        if page != Some(component.page) {
            page = Some(component.page);
            entries.push(MenuEntry::Heading(component.page_name.clone()));
        }
        let current = Some(component.id) == instance.main.set || component.id == instance.main.id;
        entries.push(MenuEntry::Choice {
            label: component.name.clone(),
            current,
            action: Some(ComponentAction::Swap {
                ids: vec![layer.id],
                component: component.id,
            }),
            enabled: !current,
        });
    }
    let mut main_row = div().flex().items_center().gap_2();
    main_row = main_row.child(dropdown(
        &main_label,
        ComponentsMenu::SwapMain,
        entries,
        state,
        cx,
    ));
    if instance.main.missing {
        main_row = main_row.child(button(
            RESTORE_MAIN,
            ComponentAction::RestoreMain { id: layer.id },
            SourceSupport::Expressible,
            cx,
        ));
    } else {
        main_row = main_row.child(button(
            GO_TO_MAIN,
            ComponentAction::GoToMain { id: layer.id },
            SourceSupport::Expressible,
            cx,
        ));
    }
    body = body.child(row("Main component", main_row));

    // The main component's description.
    if let Some(description) = &instance.description {
        body = body.child(note(description));
    }

    // The toggle that exposes a nested instance's properties.
    if instance.exposable {
        body = body.child(toggle(
            EXPOSE_PROPERTIES,
            instance.exposed,
            ComponentAction::ExposeInstance {
                id: layer.id,
                exposed: !instance.exposed,
            },
            SourceSupport::Unsupported(EXPOSED_REASON),
            cx,
        ));
    }

    // The variant properties.
    if !instance.variants.is_empty() {
        let mut rows = div().flex().flex_col().gap_1();
        for variant in &instance.variants {
            rows = rows.child(row(&variant.name, read_only_value(&variant.value.text())));
        }
        body = body.child(rows);
        body = body.child(note(INSTANCE_VALUES_REASON));
    }

    // The main component's own properties.
    if !instance.properties.is_empty() {
        let mut rows = div().flex().flex_col().gap_1();
        for property in &instance.properties {
            rows = rows.child(row(&property.name, read_only_value(&property.value.text())));
        }
        body = body.child(rows);
        body = body.child(note(INSTANCE_VALUES_REASON));
    }

    // The nested instances this one exposes.
    for copy in &instance.exposed_copies {
        body = body.child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(theme::TEXT))
                .child(copy.name.clone()),
        );
        let mut rows = div().flex().flex_col().gap_1();
        for property in &copy.properties {
            rows = rows.child(row(&property.name, read_only_value(&property.value.text())));
        }
        body = body.child(rows);
    }

    // The actions.
    let mut actions = div().flex().items_center().gap_2();
    actions = actions.child(button(
        RESET_ALL_CHANGES,
        ComponentAction::ResetInstance {
            ids: vec![layer.id],
        },
        SourceSupport::Expressible,
        cx,
    ));
    if !instance.nested {
        actions = actions.child(button(
            DETACH_INSTANCE,
            ComponentAction::Detach {
                ids: vec![layer.id],
            },
            SourceSupport::Expressible,
            cx,
        ));
    }
    actions = actions.child(menu_trigger(
        MORE_INSTANCE_ACTIONS,
        ComponentsMenu::InstanceActions,
        state,
        cx,
    ));

    let mut menu_entries = Vec::new();
    if !instance.main.missing {
        menu_entries.push(MenuEntry::Choice {
            label: GO_TO_MAIN.to_owned(),
            current: false,
            action: Some(ComponentAction::GoToMain { id: layer.id }),
            enabled: true,
        });
        menu_entries.push(MenuEntry::Choice {
            label: PUSH_CHANGES.to_owned(),
            current: false,
            action: Some(ComponentAction::PushChanges { id: layer.id }),
            enabled: true,
        });
    } else if !instance.nested {
        menu_entries.push(MenuEntry::Choice {
            label: RESTORE_MAIN.to_owned(),
            current: false,
            action: Some(ComponentAction::RestoreMain { id: layer.id }),
            enabled: true,
        });
    }
    menu_entries.push(MenuEntry::Choice {
        label: RESET_ALL_CHANGES.to_owned(),
        current: false,
        action: Some(ComponentAction::ResetInstance {
            ids: vec![layer.id],
        }),
        enabled: true,
    });
    if !instance.nested {
        menu_entries.push(MenuEntry::Choice {
            label: DETACH_INSTANCE.to_owned(),
            current: false,
            action: Some(ComponentAction::Detach {
                ids: vec![layer.id],
            }),
            enabled: true,
        });
    }

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .id(SharedString::from("components-instance-title"))
                .debug_selector(|| "components-instance-title".to_owned())
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(theme::TEXT))
                .child(if instance.nested {
                    NESTED_INSTANCE
                } else {
                    INSTANCE
                }),
        )
        .child(actions);

    // The "more actions" menu hangs under the header.
    let mut section = div().flex().flex_col().gap_2().child(header);
    if state.borrow().menu.as_ref() == Some(&ComponentsMenu::InstanceActions) {
        section = section.child(menu_block(menu_entries, state, cx));
    }
    section = section.child(section_body().child(body));
    section
}

/// A component's property section: its properties and its variant matrix.
fn properties_section<T: ComponentsHost>(
    component: &ComponentView,
    state: &Rc<RefCell<ComponentsPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let mut body = div().flex().flex_col().gap_2();

    if component.properties.is_empty() && component.variants.is_empty() {
        body = body.child(note(NO_PROPERTIES_NOTE));
    }
    if component.conflicts {
        body = body.child(note(VARIANT_CONFLICT_NOTE));
    }

    // The variant matrix.
    for variant in &component.variants {
        let mut entries = Vec::new();
        for option in &variant.options {
            entries.push(MenuEntry::Choice {
                label: option.clone(),
                current: option == &variant.default,
                action: Some(ComponentAction::EditVariant {
                    id: component.id,
                    name: variant.name.clone(),
                    new_name: None,
                    default: Some(option.clone()),
                    value: None,
                    new_value: None,
                }),
                enabled: option != &variant.default,
            });
        }
        let mut value = div().flex().items_center().gap_2();
        value = value.child(dropdown(
            &variant.default,
            ComponentsMenu::VariantDefault(variant.name.clone()),
            entries,
            state,
            cx,
        ));
        body = body.child(row(&variant.name, value));

        // The values the variants take.
        let mut chips = div().flex().flex_wrap().gap_1();
        for option in &variant.options {
            chips = chips.child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child(option.clone()),
            );
        }
        body = body.child(chips);
    }

    // The component's own properties.
    for property in &component.properties {
        body = body.child(row(
            &property.name,
            read_only_value(&property.default.text()),
        ));
    }
    if !component.properties.is_empty() {
        body = body.child(note(COMPONENT_PROPERTIES_REASON));
    }

    // A set can have another variant.
    if component.is_set {
        body = body.child(button(
            ADD_VARIANT,
            ComponentAction::AddVariant {
                id: component.id,
                property: None,
                value: None,
            },
            SourceSupport::Expressible,
            cx,
        ));
    }

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .id(SharedString::from("components-properties-title"))
                .debug_selector(|| "components-properties-title".to_owned())
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(theme::TEXT))
                .child(PROPERTIES),
        )
        .child(button(
            "+",
            ComponentAction::AddProperty {
                id: component.id,
                name: "Property 1".to_owned(),
                kind: "boolean".to_owned(),
            },
            SourceSupport::Unsupported(COMPONENT_PROPERTIES_REASON),
            cx,
        ));

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(header)
        .child(section_body().child(body))
}

/// A component's description.
fn description_section(component: &ComponentView) -> Div {
    let body = match &component.description {
        Some(description) => div()
            .flex()
            .flex_col()
            .gap_1()
            .child(note(description))
            .child(note(DESCRIPTION_REASON)),
        None => div()
            .flex()
            .flex_col()
            .gap_1()
            .child(note(DESCRIPTION_REASON)),
    };
    let header = div().flex().items_center().justify_between().gap_2().child(
        div()
            .id(SharedString::from("components-description-title"))
            .debug_selector(|| "components-description-title".to_owned())
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(theme::TEXT))
            .child(DESCRIPTION),
    );
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(header)
        .child(section_body().child(body))
}

/// A slot's section: what it holds, and the menu that fills it.
fn slot_section<T: ComponentsHost>(
    model: &ComponentsModel,
    slot: &SlotView,
    state: &Rc<RefCell<ComponentsPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let label = if slot.edited {
        format!("{} layers", slot.layers)
    } else if slot.layers == 0 {
        "Empty".to_owned()
    } else {
        "Default content".to_owned()
    };

    let mut entries = Vec::new();
    let preferred: Vec<&LocalComponent> = model
        .components
        .iter()
        .filter(|component| slot.preferred.contains(&component.id))
        .collect();
    if !preferred.is_empty() {
        entries.push(MenuEntry::Heading(PREFERRED.to_owned()));
        for component in preferred {
            entries.push(MenuEntry::Choice {
                label: component.name.clone(),
                current: false,
                action: Some(ComponentAction::SlotInsert {
                    slot: slot.id,
                    component: component.id,
                }),
                enabled: true,
            });
        }
    }
    entries.push(MenuEntry::Heading(ALL_COMPONENTS.to_owned()));
    for component in &model.components {
        entries.push(MenuEntry::Choice {
            label: component.name.clone(),
            current: false,
            action: Some(ComponentAction::SlotInsert {
                slot: slot.id,
                component: component.id,
            }),
            enabled: true,
        });
    }
    entries.push(MenuEntry::Choice {
        label: SELECT_SLOT.to_owned(),
        current: false,
        action: Some(ComponentAction::Select { ids: vec![slot.id] }),
        enabled: true,
    });
    if slot.edited {
        entries.push(MenuEntry::Choice {
            label: RESET_SLOT.to_owned(),
            current: false,
            action: Some(ComponentAction::ResetSlot { id: slot.id }),
            enabled: true,
        });
    }

    let header = div().flex().items_center().justify_between().gap_2().child(
        div()
            .id(SharedString::from("components-slot-title"))
            .debug_selector(|| "components-slot-title".to_owned())
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(theme::TEXT))
            .child(SLOT),
    );
    let body = section_body().child(row(
        &slot.name,
        dropdown(&label, ComponentsMenu::SlotFill, entries, state, cx),
    ));
    div().flex().flex_col().gap_2().child(header).child(body)
}

/// The overrides a layer carries.
fn overrides_section<T: ComponentsHost>(layer: &ComponentsLayer, cx: &mut Context<T>) -> Div {
    let mut body = div().flex().flex_col().gap_1();
    for over in &layer.overrides {
        body = body.child(row(
            &over.label,
            button(
                RESET_THIS_OVERRIDE,
                ComponentAction::ResetOverride {
                    id: layer.id,
                    group: Some(over.group.clone()),
                },
                SourceSupport::Expressible,
                cx,
            ),
        ));
    }
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .id(SharedString::from("components-overrides-title"))
                .debug_selector(|| "components-overrides-title".to_owned())
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(theme::TEXT))
                .child(OVERRIDES),
        )
        .child(button(
            RESET_ALL_OVERRIDES,
            ComponentAction::ResetOverride {
                id: layer.id,
                group: None,
            },
            SourceSupport::Expressible,
            cx,
        ));
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(header)
        .child(section_body().child(body))
}

/// The section that binds a layer's fields to a component's properties.
fn bind_section<T: ComponentsHost>(
    layer: &ComponentsLayer,
    state: &Rc<RefCell<ComponentsPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let mut body = div().flex().flex_col().gap_1();
    for binding in &layer.bindings {
        let mut entries = Vec::new();
        for candidate in &binding.candidates {
            entries.push(MenuEntry::Choice {
                label: candidate.clone(),
                current: binding.property.as_deref() == Some(candidate.as_str()),
                action: Some(ComponentAction::BindProperty {
                    layer: layer.id,
                    property: Some(candidate.clone()),
                    field: binding.field.clone(),
                }),
                enabled: binding.property.as_deref() != Some(candidate.as_str()),
            });
        }
        entries.push(MenuEntry::Choice {
            label: CREATE_PROPERTY.to_owned(),
            current: false,
            action: Some(ComponentAction::AddProperty {
                id: layer.id,
                name: binding.label.clone(),
                kind: "boolean".to_owned(),
            }),
            enabled: false,
        });
        if binding.field == "slot" {
            if binding.property.is_none() {
                entries.push(MenuEntry::Choice {
                    label: MAKE_SLOT.to_owned(),
                    current: false,
                    action: Some(ComponentAction::MakeSlot { id: layer.id }),
                    enabled: true,
                });
            } else {
                entries.push(MenuEntry::Choice {
                    label: REMOVE_SLOT.to_owned(),
                    current: false,
                    action: Some(ComponentAction::RemoveSlot { id: layer.id }),
                    enabled: true,
                });
            }
        } else if binding.property.is_some() {
            entries.push(MenuEntry::Choice {
                label: DETACH_PROPERTY.to_owned(),
                current: false,
                action: Some(ComponentAction::BindProperty {
                    layer: layer.id,
                    property: None,
                    field: binding.field.clone(),
                }),
                enabled: true,
            });
        }
        let shown = binding
            .property
            .clone()
            .unwrap_or_else(|| "Not bound".to_owned());
        body = body.child(row(
            &binding.label,
            dropdown(
                &shown,
                ComponentsMenu::Bind(binding.field.clone()),
                entries,
                state,
                cx,
            ),
        ));
    }
    let header = div().flex().items_center().justify_between().gap_2().child(
        div()
            .id(SharedString::from("components-property-title"))
            .debug_selector(|| "components-property-title".to_owned())
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(theme::TEXT))
            .child(COMPONENT_PROPERTY),
    );
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(header)
        .child(section_body().child(body))
}

// ── The chrome ──────────────────────────────────────────────────────────────

/// The body of a section: one column, one gap, one padding.
fn section_body() -> Div {
    div().flex().flex_col().gap_2().px(px(14.0)).py(px(12.0))
}

/// One row: a label on the left, a value on the right.
fn row(label: &str, value: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .id(SharedString::from(format!("components-label-{label}")))
                .debug_selector(|| format!("components-label-{label}"))
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(label.to_owned()),
        )
        .child(value)
}

/// A line of text under the rows it belongs to.
fn note(text: &str) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("components-note-{text}")))
        .debug_selector(|| format!("components-note-{text}"))
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A value the panel shows but will not edit.
fn read_only_value(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .opacity(0.7)
        .child(text.to_owned())
}

/// A button that asks for a change.
fn button<T: ComponentsHost>(
    label: &str,
    action: ComponentAction,
    support: SourceSupport,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    let mut button = div()
        .id(SharedString::from(format!("components-button-{label}")))
        .debug_selector(|| format!("components-button-{label}"))
        .flex()
        .items_center()
        .px(px(8.0))
        .py(px(5.0))
        .rounded_sm()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY));
    if support.is_expressible() {
        button = button
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .on_click(cx.listener(move |host, _, _, cx| {
                host.run_component_action(action.clone(), cx);
            }));
    } else {
        button = button.opacity(0.5).cursor_default();
    }
    button.child(label.to_owned())
}

/// A toggle that asks for a change.
fn toggle<T: ComponentsHost>(
    label: &str,
    on: bool,
    action: ComponentAction,
    support: SourceSupport,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    let mut toggle = div()
        .id(SharedString::from(format!("components-toggle-{label}")))
        .debug_selector(|| format!("components-toggle-{label}"))
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY));
    let mark = div()
        .size(px(12.0))
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(if on {
            rgb(theme::ACCENT)
        } else {
            rgb(theme::WINDOW)
        });
    toggle = toggle.child(mark).child(label.to_owned());
    if support.is_expressible() {
        toggle = toggle
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .on_click(cx.listener(move |host, _, _, cx| {
                host.run_component_action(action.clone(), cx);
            }));
    } else {
        toggle = toggle.opacity(0.5).cursor_default();
    }
    toggle
}

/// One entry in a menu.
enum MenuEntry {
    /// A heading: the title of a group of entries.
    Heading(String),
    /// A choice.
    Choice {
        /// The text.
        label: String,
        /// Whether it is the value in force.
        current: bool,
        /// The change choosing it asks for.
        action: Option<ComponentAction>,
        /// Whether it can be chosen.
        enabled: bool,
    },
}

/// The trigger of a menu: the button that opens it.
fn menu_trigger<T: ComponentsHost>(
    label: &str,
    menu: ComponentsMenu,
    state: &Rc<RefCell<ComponentsPanelState>>,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    let open = state.borrow().menu.as_ref() == Some(&menu);
    let st = Rc::clone(state);
    let chosen = menu.clone();
    div()
        .id(SharedString::from(format!("components-menu-{label}")))
        .debug_selector(|| format!("components-menu-{label}"))
        .flex()
        .items_center()
        .gap_1()
        .px(px(8.0))
        .py(px(5.0))
        .rounded_sm()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer()
        .child(label.to_owned())
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(if open { "▲" } else { "▼" }),
        )
        .on_click(cx.listener(move |_host, _, _, cx| {
            let mut held = st.borrow_mut();
            held.menu = if held.menu.as_ref() == Some(&chosen) {
                None
            } else {
                Some(chosen.clone())
            };
            cx.notify();
        }))
}

/// A dropdown: the trigger, and the menu under it when it is open.
fn dropdown<T: ComponentsHost>(
    label: &str,
    menu: ComponentsMenu,
    entries: Vec<MenuEntry>,
    state: &Rc<RefCell<ComponentsPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let open = state.borrow().menu.as_ref() == Some(&menu);
    let mut block = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(menu_trigger(label, menu, state, cx));
    if open {
        block = block.child(menu_block(entries, state, cx));
    }
    block
}

/// The entries of an open menu.
fn menu_block<T: ComponentsHost>(
    entries: Vec<MenuEntry>,
    state: &Rc<RefCell<ComponentsPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let mut block = div()
        .flex()
        .flex_col()
        .gap_1()
        .p(px(6.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER));
    for entry in entries {
        match entry {
            MenuEntry::Heading(text) => {
                block = block.child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(text),
                );
            }
            MenuEntry::Choice {
                label,
                current,
                action,
                enabled,
            } => {
                let text = if current {
                    format!("✓ {label}")
                } else {
                    label.clone()
                };
                let mut item = div()
                    .id(SharedString::from(format!("components-menu-item-{label}")))
                    .debug_selector(|| format!("components-menu-item-{label}"))
                    .text_xs()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child(text);
                if enabled {
                    if let Some(action) = action {
                        let st = Rc::clone(state);
                        item = item
                            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                            .cursor_pointer()
                            .on_click(cx.listener(move |host, _, _, cx| {
                                st.borrow_mut().menu = None;
                                host.run_component_action(action.clone(), cx);
                            }));
                    }
                } else {
                    item = item.opacity(0.5);
                }
                block = block.child(item);
            }
        }
    }
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::rc::Rc;

    use figcraft_doc::{Document, NodeKind};
    use figcraft_engine::Session;
    use gpui::{
        point, px, size, Context, Entity, IntoElement, Render, TestAppContext, VisualTestContext,
        Window,
    };
    use serde_json::json;

    /// A document with a main component and an instance of it.
    ///
    /// Built with the engine's own commands, so the document is one an editor
    /// would really hold rather than one a test assembled by hand.
    fn session_with_an_instance() -> (Rc<RefCell<Session>>, NodeId, NodeId) {
        let mut doc = Document::new("Project");
        let page = doc.pages[0];
        let frame = doc
            .create(page, NodeKind::Frame, "Card", 0.0, 0.0, 200.0, 120.0)
            .expect("a frame");
        let mut session = Session::with_document(doc);
        session
            .execute("component.create", &json!({ "ids": [frame.0] }))
            .expect("the frame becomes a component");
        let component = frame;
        let placed = session
            .execute(
                "instance.create",
                &json!({ "component": component.0, "x": 0.0, "y": 200.0, "parent": page.0 }),
            )
            .expect("an instance of the component");
        let instance = NodeId(
            placed["id"]
                .as_u64()
                .expect("the engine names the instance it made"),
        );
        (Rc::new(RefCell::new(session)), component, instance)
    }

    /// The document the session holds.
    fn document_of(session: &Rc<RefCell<Session>>) -> Document {
        let session = session.borrow();
        session.doc().cloned().expect("the engine holds a document")
    }

    // ── The model ───────────────────────────────────────────────────────────

    #[test]
    fn the_model_reads_an_instance_and_its_main() {
        let (session, component, instance) = session_with_an_instance();
        let doc = document_of(&session);
        let model = ComponentsModel::read(&doc, &[instance]);

        let ComponentsSelection::One(layer) = &model.selection else {
            panic!("the instance is selected, so the section has something to show");
        };
        let LayerRole::Instance(view) = &layer.role else {
            panic!("the selection is an instance");
        };
        assert_eq!(view.main.id, component, "the instance names its main");
        assert_eq!(view.main.name, "Card", "the main's name is the main's");
        assert!(!view.main.missing, "the main is there");
        assert!(
            !view.nested,
            "the instance is on the page, not in an instance"
        );
        assert!(
            !view.exposable,
            "the instance is not inside a main component, so it has no properties to expose"
        );
        assert_eq!(
            model.components.len(),
            1,
            "the document holds one component: {model:?}"
        );
        assert_eq!(model.components[0].id, component);
    }

    #[test]
    fn a_deleted_main_is_reported_as_deleted() {
        let (session, component, instance) = session_with_an_instance();
        {
            let mut session = session.borrow_mut();
            session
                .execute("edit.delete", &json!({ "ids": [component.0] }))
                .expect("the main is deleted");
        }
        let doc = document_of(&session);
        let model = ComponentsModel::read(&doc, &[instance]);
        let ComponentsSelection::One(layer) = &model.selection else {
            panic!("the instance is selected");
        };
        let LayerRole::Instance(view) = &layer.role else {
            panic!("the selection is an instance");
        };
        assert!(view.main.missing, "the main is gone");
        assert_eq!(
            view.main.name, MAIN_DELETED,
            "the panel says the main is deleted, not that it has no name"
        );
    }

    #[test]
    fn the_model_reads_a_component_and_its_properties() {
        let mut doc = Document::new("Project");
        let page = doc.pages[0];
        let frame = doc
            .create(page, NodeKind::Frame, "Card", 0.0, 0.0, 200.0, 120.0)
            .expect("a frame");
        {
            let node = doc.get_mut(frame).expect("the frame");
            node.kind = NodeKind::Component {
                description: "A card.".to_owned(),
                props: BTreeMap::from([(
                    "Label".to_owned(),
                    PropDef::Text {
                        default: "Hello".to_owned(),
                    },
                )]),
            };
        }
        let model = ComponentsModel::read(&doc, &[frame]);
        let ComponentsSelection::One(layer) = &model.selection else {
            panic!("the component is selected");
        };
        let LayerRole::Component(view) = &layer.role else {
            panic!("the selection is a component");
        };
        assert_eq!(view.name, "Card");
        assert_eq!(view.description.as_deref(), Some("A card."));
        assert_eq!(view.properties.len(), 1);
        assert_eq!(view.properties[0].name, "Label");
        assert_eq!(view.properties[0].kind, PropertyKind::Text);
        assert_eq!(
            view.properties[0].default,
            PropertyValue::Text("Hello".to_owned())
        );
    }

    #[test]
    fn the_model_reads_the_overrides_a_layer_carries() {
        // An override belongs to a layer *inside* an instance: it is the copy
        // of the main's layer that changed locally.
        let mut doc = Document::new("Project");
        let page = doc.pages[0];
        let frame = doc
            .create(page, NodeKind::Frame, "Card", 0.0, 0.0, 200.0, 120.0)
            .expect("a frame");
        let label = doc
            .create(frame, NodeKind::Rectangle, "Label", 0.0, 0.0, 80.0, 20.0)
            .expect("a label");
        let mut session = Session::with_document(doc);
        session
            .execute("component.create", &json!({ "ids": [frame.0] }))
            .expect("the frame becomes a component");
        let placed = session
            .execute(
                "instance.create",
                &json!({ "component": frame.0, "x": 0.0, "y": 200.0, "parent": page.0 }),
            )
            .expect("an instance of the component");
        let instance = NodeId(
            placed["id"]
                .as_u64()
                .expect("the engine names the instance it made"),
        );

        // The instance's own copy of the label.
        let doc = session.doc().expect("a document");
        let copy = doc
            .node(instance)
            .and_then(|node| node.children.first().copied())
            .expect("the instance holds a copy of the label");
        assert_eq!(
            doc.node(copy).and_then(|node| node.source),
            Some(label),
            "the copy names the main's layer it mirrors"
        );

        let mut doc = doc.clone();
        {
            let node = doc.get_mut(copy).expect("the copy");
            node.overrides.insert("fills".to_owned());
            node.overrides.insert("characters".to_owned());
            node.overrides.insert("not-a-group".to_owned());
        }
        let model = ComponentsModel::read(&doc, &[copy]);
        let ComponentsSelection::One(layer) = &model.selection else {
            panic!("the copy is selected");
        };
        assert_eq!(
            layer.overrides.len(),
            2,
            "only the engine's own groups are overrides: {layer:?}"
        );
        assert_eq!(layer.overrides[0].label, "Text content");
        assert_eq!(layer.overrides[1].label, "Fill");
    }

    #[test]
    fn a_selection_of_nothing_is_the_empty_state() {
        let doc = Document::new("Project");
        let model = ComponentsModel::read(&doc, &[]);
        assert_eq!(model.selection, ComponentsSelection::Empty);
    }

    #[test]
    fn a_layer_the_document_does_not_hold_is_not_shown() {
        let doc = Document::new("Project");
        let model = ComponentsModel::read(&doc, &[NodeId(9999)]);
        assert_eq!(
            model.selection,
            ComponentsSelection::Empty,
            "a deleted layer is not drawn as an empty one"
        );
    }

    // ── The commands ───────────────────────────────────────────────────────

    #[test]
    fn a_command_carries_the_engines_command_id_and_params() {
        let action = ComponentAction::Detach {
            ids: vec![NodeId(3)],
        };
        let (command, params) = action.command();
        assert_eq!(command, "component.detach");
        assert_eq!(params["ids"], json!([3]));

        let (command, params) = ComponentAction::Swap {
            ids: vec![NodeId(3)],
            component: NodeId(9),
        }
        .command();
        assert_eq!(command, "instance.swap");
        assert_eq!(params["ids"], json!([3]));
        assert_eq!(params["component"], json!(9));

        let (command, params) = ComponentAction::ResetOverride {
            id: NodeId(4),
            group: Some("fills".to_owned()),
        }
        .command();
        assert_eq!(command, "component.resetOverride");
        assert_eq!(params["id"], json!(4));
        assert_eq!(params["group"], json!("fills"));

        let (command, params) = ComponentAction::BindProperty {
            layer: NodeId(5),
            property: Some("Label".to_owned()),
            field: "characters".to_owned(),
        }
        .command();
        assert_eq!(command, "component.bindProperty");
        assert_eq!(params["layer"], json!(5));
        assert_eq!(params["property"], json!("Label"));
        assert_eq!(params["field"], json!("characters"));

        let (command, params) = ComponentAction::Select {
            ids: vec![NodeId(6)],
        }
        .command();
        assert_eq!(command, "select.set");
        assert_eq!(params["ids"], json!([6]));
    }

    #[test]
    fn the_engine_accepts_the_commands_the_panel_asks_for() {
        let (session, component, instance) = session_with_an_instance();
        let mut session = session.borrow_mut();

        let (command, params) = ComponentAction::GoToMain { id: instance }.command();
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");

        let (command, params) = ComponentAction::ResetInstance {
            ids: vec![instance],
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");

        let (command, params) = ComponentAction::Swap {
            ids: vec![instance],
            component,
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");

        let (command, params) = ComponentAction::Detach {
            ids: vec![instance],
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let doc = session.doc().expect("a document");
        assert!(
            !matches!(
                doc.node(instance).expect("the instance").kind,
                NodeKind::Instance { .. }
            ),
            "the instance was detached, so it is a frame now"
        );
    }

    #[test]
    fn a_change_the_source_cannot_carry_is_not_expressible() {
        assert!(
            !ComponentAction::AddProperty {
                id: NodeId(1),
                name: "Label".to_owned(),
                kind: "text".to_owned(),
            }
            .support()
            .is_expressible(),
            "lamine.yaml has no field for a component's properties"
        );
        assert!(
            !ComponentAction::SetProperties {
                ids: vec![NodeId(1)],
                properties: BTreeMap::new(),
            }
            .support()
            .is_expressible(),
            "lamine.yaml has no field for an instance's property values"
        );
        assert!(
            ComponentAction::Detach {
                ids: vec![NodeId(1)]
            }
            .support()
            .is_expressible(),
            "lamine.yaml can say that a layer is a frame"
        );
    }

    // ── The panel, in a real window ────────────────────────────────────────

    /// A host for the panel in a test: it runs the commands the panel asks for
    /// and remembers what it was asked.
    struct TestHost {
        session: Rc<RefCell<Session>>,
        state: Rc<RefCell<ComponentsPanelState>>,
        asked: Vec<ComponentAction>,
    }

    impl TestHost {
        fn new(session: Rc<RefCell<Session>>, _cx: &mut Context<Self>) -> Self {
            Self {
                session,
                state: Rc::new(RefCell::new(ComponentsPanelState::default())),
                asked: Vec::new(),
            }
        }

        /// The model of what the session holds.
        fn model(&self) -> ComponentsModel {
            let session = self.session.borrow();
            let Ok(doc) = session.doc() else {
                return ComponentsModel::empty();
            };
            let selection = session.selection();
            ComponentsModel::read(doc, &selection)
        }
    }

    impl ComponentsHost for TestHost {
        fn run_component_action(&mut self, action: ComponentAction, cx: &mut Context<Self>) {
            let (command, params) = action.command();
            self.session
                .borrow_mut()
                .execute(&command, &params)
                .expect("the engine accepts the command");
            self.asked.push(action);
            cx.notify();
        }
    }

    impl Render for TestHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let model = self.model();
            components_section(&model, &self.state, cx)
        }
    }

    /// The size the window is drawn at.
    fn window_size() -> gpui::Size<gpui::Pixels> {
        size(px(900.0), px(700.0))
    }

    /// The panel in a real window, with a session to read.
    fn open_panel(
        cx: &mut TestAppContext,
        session: Rc<RefCell<Session>>,
    ) -> (Entity<TestHost>, &mut VisualTestContext) {
        let (host, window) = cx.add_window_view(move |_, cx| TestHost::new(session, cx));
        window.draw(point(px(0.0), px(0.0)), window_size(), |_, _| {
            host.clone().into_any_element()
        });
        window.run_until_parked();
        (host, window)
    }

    #[gpui::test]
    fn the_panel_draws_the_instance_section_over_an_instance(cx: &mut TestAppContext) {
        let (session, _component, instance) = session_with_an_instance();
        {
            let mut session = session.borrow_mut();
            session.set_selection(vec![instance]);
        }
        let (_host, window) = open_panel(cx, session);

        // figcraft's title for the section, on screen.
        let title = window
            .debug_bounds("components-instance-title")
            .expect("the instance section is drawn");
        assert!(
            title.size.height > px(0.0),
            "the instance title is drawn with no height: {title:?}"
        );

        // And the main component's name, which figcraft shows in the row under
        // the title.
        let main = window
            .debug_bounds("components-label-Main component")
            .expect("the main component row is drawn");
        assert!(
            main.size.height > px(0.0),
            "the main component row is drawn with no height: {main:?}"
        );

        // The action figcraft offers on an instance.
        let detach = window
            .debug_bounds("components-button-Detach instance")
            .expect("the detach action is drawn");
        assert!(
            detach.size.height > px(0.0),
            "the detach action is drawn with no height: {detach:?}"
        );
    }

    #[gpui::test]
    fn the_panel_draws_the_empty_state(cx: &mut TestAppContext) {
        let (session, _component, _instance) = session_with_an_instance();
        {
            let mut session = session.borrow_mut();
            session.set_selection(Vec::new());
        }
        let (_host, window) = open_panel(cx, session);

        let empty = window
            .debug_bounds("components-empty-title")
            .expect("the empty state is drawn");
        assert!(
            empty.size.height > px(0.0),
            "the empty state is drawn with no height: {empty:?}"
        );
    }

    #[gpui::test]
    fn the_panel_draws_a_layer_that_is_neither(cx: &mut TestAppContext) {
        let (session, _component, _instance) = session_with_an_instance();
        let rectangle = {
            let mut held = session.borrow_mut();
            let page = held.page().expect("a page");
            let rectangle = held
                .doc_mut()
                .expect("a document")
                .create(page, NodeKind::Rectangle, "Swatch", 0.0, 0.0, 40.0, 40.0)
                .expect("a rectangle");
            held.set_selection(vec![rectangle]);
            rectangle
        };
        assert_eq!(rectangle.0, rectangle.0);

        let (_host, window) = open_panel(cx, session);
        let note = window
            .debug_bounds("components-note-This layer is not a component or an instance.")
            .expect("the empty state is drawn");
        assert!(
            note.size.height > px(0.0),
            "the empty state is drawn with no height: {note:?}"
        );
    }
}
