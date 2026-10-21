//! The Prototype tab: the prototype's flows, the layers' interactions, the scroll
//! behaviour and the prototype settings — and, on the canvas, the connections
//! between the layers that act on one another.
//!
//! # What this module owns
//!
//! Three things, in one file because they are one thing:
//!
//! 1. **The model.** [`ProtoModel`] reads the engine's [`Document`] and turns the
//!    page's flows, the selected layers' interactions and the prototype's
//!    settings into what the panel shows. [`ProtoAction`] turns a change into
//!    the engine's own command.
//! 2. **The panel.** [`proto_panel`] draws that model and asks the engine for
//!    the changes the user makes.
//! 3. **The prototype mode.** [`proto_connections`] and
//!    [`paint_proto_connections`] are what the canvas draws when the Prototype
//!    tab is open: the noodles from a layer to the frame it acts on.
//!
//! # The reference
//!
//! figcraft's Prototype tab — `crates/ui-egui/src/proto_panel.rs` (the panel),
//! `crates/ui-egui/src/proto_editor.rs` (the editor for one interaction) and
//! `crates/ui-egui/src/proto_canvas.rs` (the connections on the canvas) — is the
//! specification. Its sections, its rows, its order, its labels, its empty
//! state and its actions are mirrored here. The engine's own model is the
//! arbiter: `figcraft_doc::proto` says what an interaction is, and the engine's
//! commands say what a change is.
//!
//! # The source boundary — the answer, and it has three halves
//!
//! Spool is source-backed: HTML and CSS are the authored source and
//! `lamine.yaml` holds the structure. A prototype interaction is a fact about
//! the source, so the question is whether the source can carry one.
//!
//! **Interactions: the source format HAS a spelling, and the engine reads it.**
//! `lamine.yaml` is version 2, and every node record has an `interactions:`
//! list of the engine's own `Interaction` type
//! (`spool_source::manifest::NodeRecord::interactions`). The compiler copies
//! it onto the node (`spool_source::reader::apply_owner_b`), so a project's
//! interactions reach the editor. This is the one Owner B property in the whole
//! schema that is a fact about a prototype rather than about a layer's
//! appearance, and it is already there.
//!
//! **Flows: the source format has a field and the compiler refuses it.**
//! `NodeRecord::flow` exists (`Option<String>`), but
//! `spool_source::reader::insert_node` reports every record that carries one
//! as unresolved — "a prototype flow start has no compiled form yet" — and
//! leaves `NodeKind::Page { flows }` empty. So a source-backed project has no
//! flows to show, and the panel says so rather than inventing one.
//!
//! **And the save path writes neither of them back.** The save path writes the
//! editor's own version 1 metadata (a handful of fields per node) and leaves a
//! version 2 file exactly as it is ("the project's metadata is version 2, which
//! this editor reads but does not rewrite"). There is no `SourceEdit` that
//! carries an interaction, so an interaction added here stays in the editor's
//! document and does not reach `lamine.yaml`.
//!
//! So the honest position, which the panel states where the change is made:
//!
//! - The panel shows the interactions the engine holds, which for a
//!   source-backed project are the ones `lamine.yaml` carried in.
//! - The panel can add, change and remove them, and the change is real in the
//!   editor's document and in the history.
//! - The panel says that the change is not written back to `lamine.yaml` yet,
//!   because it is not, and a panel that pretended otherwise would break the
//!   round trip the architecture forbids.
//!
//! No source format is invented here, and no value is edited that the save
//! path cannot carry.
//!
//! # Where this differs from figcraft, and why
//!
//! - **The interaction editor is inline, not a popover.** figcraft floats the
//!   editor to the left of the panel. Spool's Inspector has no surface that
//!   owns a popover, so the editor opens under the row it belongs to — the
//!   same decision the Styles section made.
//! - **The list is folded per property.** figcraft folds the whole
//!   `Vec<Interaction>`: one difference between two selected layers hides the
//!   entire list. Spool folds per interaction and per field, so a layer that
//!   agrees about the trigger and disagrees about the destination says so on
//!   the destination's row. The one thing folded as a whole is the *number* of
//!   interactions, because two layers with a different number of them have no
//!   one list to show.
//! - **No tooltips.** The panel's buttons carry their names in their debug
//!   selectors rather than in hover text, because Spool has no tooltip
//!   surface yet.
//!
//! # Commands, not mutations
//!
//! Every change leaves through the engine's command surface:
//! `proto.interaction.add`, `proto.interaction.set`,
//! `proto.interaction.remove`, `proto.interaction.clear`, `proto.flow.add`,
//! `proto.flow.rename`, `proto.flow.remove`, `proto.settings`,
//! `proto.preview`, `proto.present` and `node.set`. Nothing here writes to a
//! [`Document`] directly, so an edit made from this panel, from the CLI and
//! from an agent are the same edit and land in the same history.

use std::cell::RefCell;
use std::rc::Rc;

use figcraft_doc::color::Color;
use figcraft_doc::proto::{
    Action, DeviceRotation, Direction, Easing, Flow, Interaction, Overflow, OverlayPosition,
    PrototypeSettings, ScrollPosition, Transition, Trigger,
};
use figcraft_doc::{Document, Node, NodeId, NodeKind};
use figcraft_engine::ViewState;
use gpui::{
    div, prelude::*, px, rgb, rgba, AnyElement, Bounds, Context, Div, Hsla, PathBuilder, Pixels,
    Point as GPoint, SharedString, Stateful, Window,
};
use serde_json::{json, Value};

use crate::engine::canvas::{Palette, Viewport};
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

/// The word for a layer the document no longer holds.
///
/// figcraft's word: a link to a layer that is gone is not "Not set" — the
/// interaction does point at something, and that something cannot be found.
pub const MISSING_LAYER: &str = "Missing layer";

/// What the save path does with a prototype interaction today.
///
/// The metadata can carry one — `lamine.yaml` is version 2 and every node
/// record has an `interactions:` list the engine reads — but the save path
/// writes the editor's own version 1 metadata and leaves a version 2 file
/// alone, so an interaction made or changed here stays in the editor's
/// document. Said where the change is made, rather than left for the user to
/// find out by saving.
pub const SAVE_PATH_NOTE: &str =
    "Interactions are not written back to lamine.yaml yet, so an interaction made or changed \
     here stays in the editor's document.";

/// What the source format does with a prototype flow today.
///
/// `lamine.yaml` has a `flow:` field on every node record, and the compiler
/// reports every record that carries one as unresolved: "a prototype flow
/// start has no compiled form yet". So the page holds no flows to show, and
/// the panel says that rather than pretending the list is empty by accident.
pub const FLOW_SOURCE_NOTE: &str =
    "lamine.yaml has a `flow:` field, but the compiler does not read it yet, so a flow made here \
     stays in the editor's document.";

// ═══════════════════════════════════════════════════════════════════════════
// The three states
// ═══════════════════════════════════════════════════════════════════════════

/// One property's state across a selection.
///
/// Three states, never two: a value every selected layer agrees on, agreement
/// that nobody set it, and the absence of agreement. The Inspector's own
/// properties have the same three, and the same words, so "Mixed" means one
/// thing in the panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ProtoValue<T> {
    /// Every selected layer has this value, and they agree on it.
    Set(T),
    /// No selected layer has this value.
    #[default]
    Unset,
    /// The selected layers disagree.
    Mixed,
}

impl<T: PartialEq> ProtoValue<T> {
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

/// A flow's starting point, as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowEntry {
    /// The flow's name.
    pub name: String,
    /// The top-level frame the flow starts at.
    pub start: NodeId,
    /// The flow's description, under its row.
    pub description: String,
}

/// One interaction of the selected layer, as the panel shows it.
///
/// Every field is folded across the selection on its own, so a layer that
/// agrees about the trigger and disagrees about the destination says so on the
/// destination's row rather than hiding the whole list.
#[derive(Clone, Debug, PartialEq)]
pub struct InteractionRow {
    /// The interaction's index in the layer's list.
    pub index: usize,
    /// The trigger, as the list shows it ("On click", "Key/Gamepad Space").
    pub trigger: ProtoValue<String>,
    /// The actions, as the list shows them ("Navigate to Frame 2").
    pub summary: ProtoValue<String>,
    /// The transition, as the editor shows it.
    pub transition: ProtoValue<String>,
    /// The transition's duration, in milliseconds.
    pub duration_ms: ProtoValue<u32>,
    /// The interaction the editor edits, in the engine's own type.
    ///
    /// The panel's own copy, taken when the model was read: the editor needs
    /// the action it is changing, and the action is the engine's type rather
    /// than a string the panel made up.
    pub interaction: Option<Interaction>,
}

/// A place an action can go.
#[derive(Clone, Debug, PartialEq)]
pub struct Destination {
    /// The layer.
    pub id: NodeId,
    /// Its name, as the picker shows it.
    pub name: String,
}

/// How the selected layers scroll.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScrollModel {
    /// The layer is inside a frame: it has a scroll position.
    pub in_frame: bool,
    /// The layer is a frame: it has an overflow.
    pub is_frame: bool,
    /// The scroll position, across the selection.
    pub position: ProtoValue<ScrollPosition>,
    /// The overflow, across the selection.
    pub overflow: ProtoValue<Overflow>,
}

/// What the Prototype tab shows about the document and the selection.
#[derive(Clone, Debug, PartialEq)]
pub struct ProtoModel {
    /// The layers the model was read for.
    pub ids: Vec<NodeId>,
    /// The page the prototype lives on.
    pub page: NodeId,
    /// Whether anything is selected at all.
    ///
    /// The empty state is a different panel, not a panel with no rows: with
    /// nothing selected figcraft shows the page's flows and the prototype
    /// settings, and no layer's own properties.
    pub has_selection: bool,
    /// The page's flows, in the engine's order.
    pub flows: Vec<FlowEntry>,
    /// The flow that starts at the selected top-level frame, if there is one.
    pub flow: Option<FlowEntry>,
    /// The selected layer is a top-level frame, so it can start a flow.
    pub is_top_level_frame: bool,
    /// A top-level frame that does not start a flow yet.
    ///
    /// The frame the "add starting point" button adds: figcraft takes the
    /// first free frame, and says so when there is none.
    pub free_frame: Option<NodeId>,
    /// The number of interactions the selected layers hold, across the
    /// selection.
    pub count: ProtoValue<usize>,
    /// The interactions themselves, one row per index.
    pub interactions: Vec<InteractionRow>,
    /// The layers an edit of an interaction applies to.
    ///
    /// The whole selection when every selected layer holds the same
    /// interactions, and just the layer the row is about otherwise —
    /// figcraft's rule, and the reason an edit cannot land on one layer and
    /// miss another.
    pub targets: Vec<NodeId>,
    /// The prototype's settings, as the page holds them.
    pub settings: PrototypeSettings,
    /// The places a "Navigate to" can go: the page's top-level frames, then
    /// its sections.
    pub destinations: Vec<Destination>,
    /// The places an "Open overlay" or a "Swap overlay" can go: the page's
    /// top-level frames.
    pub frames: Vec<Destination>,
    /// The places a "Change to" can go: the variants beside the layer's
    /// component.
    pub variants: Vec<Destination>,
    /// The layers a "Scroll to" can go: the selected layer's top-level frame's
    /// children.
    pub scroll_targets: Vec<Destination>,
    /// The selected layer's top-level frame.
    ///
    /// The frame a new interaction navigates away from, and the frame a
    /// "Scroll to" stays inside.
    pub own_frame: Option<NodeId>,
    /// The component the selected layer belongs to.
    ///
    /// The variant a "Change to" does *not* pick, so the default is a change
    /// rather than a no-op.
    pub own_component: Option<NodeId>,
    /// How the selected layers scroll.
    pub scroll: ScrollModel,
}

impl Default for ProtoModel {
    /// The model of nothing: no page, no selection, no prototype.
    ///
    /// The page's id is the engine's own zero, which is the id the engine
    /// gives a document that has no page yet — a state the editor is never
    /// in, but one a panel can be built in before the engine is attached.
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            page: NodeId(0),
            has_selection: false,
            flows: Vec::new(),
            flow: None,
            is_top_level_frame: false,
            free_frame: None,
            count: ProtoValue::Unset,
            interactions: Vec::new(),
            targets: Vec::new(),
            settings: PrototypeSettings::default(),
            destinations: Vec::new(),
            frames: Vec::new(),
            variants: Vec::new(),
            scroll_targets: Vec::new(),
            own_frame: None,
            own_component: None,
            scroll: ScrollModel::default(),
        }
    }
}

impl ProtoModel {
    /// The prototype's flows, interactions and settings, as the panel shows
    /// them.
    ///
    /// A layer the document does not hold is skipped rather than shown as an
    /// empty one, and a selection with nothing selected shows the page's flows
    /// and the prototype's settings and nothing else.
    pub fn read(doc: &Document, page: NodeId, selection: &[NodeId]) -> Self {
        let nodes: Vec<&Node> = selection.iter().filter_map(|id| doc.node(*id)).collect();
        let first = nodes.first().copied();
        let first_id = selection.first().copied();

        // The number of interactions, folded across the selection: two layers
        // with a different number of them cannot show one list, so this is the
        // one property that is folded as a whole.
        let count = ProtoValue::across(nodes.iter().map(|node| Some(node.interactions.len())));

        // The interactions themselves, one row per index, every field folded
        // on its own.
        let mut interactions = Vec::new();
        if let ProtoValue::Set(n) = &count {
            for index in 0..*n {
                let trigger = ProtoValue::across(nodes.iter().map(|node| {
                    node.interactions
                        .get(index)
                        .map(|interaction| trigger_summary(&interaction.trigger))
                }));
                let summary = ProtoValue::across(nodes.iter().map(|node| {
                    node.interactions
                        .get(index)
                        .map(|interaction| action_summaries(doc, &interaction.actions))
                }));
                let transition = ProtoValue::across(nodes.iter().map(|node| {
                    node.interactions
                        .get(index)
                        .map(|interaction| interaction.transition.label().to_owned())
                }));
                let duration_ms = ProtoValue::across(
                    nodes
                        .iter()
                        .map(|node| node.interactions.get(index).map(|it| it.duration_ms)),
                );
                let interaction = first.and_then(|node| node.interactions.get(index).cloned());
                interactions.push(InteractionRow {
                    index,
                    trigger,
                    summary,
                    transition,
                    duration_ms,
                    interaction,
                });
            }
        }

        // The layers an edit applies to: the whole selection when they all
        // hold the same interactions, and the one layer otherwise.
        let targets = match (first_id, first) {
            (Some(id), Some(node)) => {
                let same = selection.len() > 1
                    && selection.contains(&id)
                    && selection.iter().all(|other| {
                        doc.node(*other).map(|n| &n.interactions) == Some(&node.interactions)
                    });
                if same {
                    selection.to_vec()
                } else {
                    vec![id]
                }
            }
            _ => Vec::new(),
        };

        let flows = flows_of(doc, page);
        let is_top_level_frame = first_id.is_some_and(|id| doc.top_level_frame(id) == Some(id));
        let flow = first_id.and_then(|id| {
            flows
                .iter()
                .find(|flow| flow.start == id)
                .map(|flow| FlowEntry {
                    name: flow.name.clone(),
                    start: flow.start,
                    description: flow.description.clone(),
                })
        });
        let free_frame = doc
            .top_frames(page)
            .into_iter()
            .find(|frame| !flows.iter().any(|flow| flow.start == *frame));

        let frames: Vec<Destination> = doc
            .top_frames(page)
            .into_iter()
            .map(|id| Destination {
                id,
                name: name_of(doc, id),
            })
            .collect();
        let mut destinations = frames.clone();
        destinations.extend(doc.sections_in(page).into_iter().map(|id| Destination {
            id,
            name: name_of(doc, id),
        }));

        let own_frame = first_id.and_then(|id| doc.top_level_frame(id));
        let own_component =
            first_id.and_then(|id| figcraft_engine::components::owning_component(doc, id));
        let variants = first_id
            .map(|id| {
                variants_for(doc, id)
                    .into_iter()
                    .map(|id| Destination {
                        id,
                        name: name_of(doc, id),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let scroll_targets = own_frame
            .map(|frame| {
                doc.subtree(frame)
                    .into_iter()
                    .skip(1)
                    .take(400)
                    .filter(|id| Some(*id) != first_id)
                    .map(|id| Destination {
                        id,
                        name: name_of(doc, id),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let node = first;
        let in_frame = node
            .and_then(|node| node.parent)
            .and_then(|parent| doc.node(parent))
            .is_some_and(|parent| parent.kind.is_frame_like());
        let is_frame = node.is_some_and(|node| node.kind.is_frame_like());
        let scroll = ScrollModel {
            in_frame,
            is_frame,
            position: ProtoValue::across(nodes.iter().map(|node| Some(node.scroll_position))),
            overflow: ProtoValue::across(nodes.iter().map(|node| Some(node.overflow))),
        };

        Self {
            ids: selection.to_vec(),
            page,
            has_selection: !selection.is_empty(),
            flows: flows
                .into_iter()
                .map(|flow| FlowEntry {
                    name: flow.name,
                    start: flow.start,
                    description: flow.description,
                })
                .collect(),
            flow,
            is_top_level_frame,
            free_frame,
            count,
            interactions,
            targets,
            settings: settings_of(doc, page),
            destinations,
            frames,
            variants,
            scroll_targets,
            own_frame,
            own_component,
            scroll,
        }
    }

    /// The flow that starts at a frame, if the page has one there.
    pub fn flow_at(&self, start: NodeId) -> Option<&FlowEntry> {
        self.flows.iter().find(|flow| flow.start == start)
    }

    /// The action a new interaction starts with.
    ///
    /// figcraft's rule: a click navigates to the next frame, and a layer with
    /// nowhere to go goes back. A frame that is not the layer's own is
    /// preferred, so a layer in "Frame 1" does not navigate to itself.
    pub fn default_action(&self) -> Action {
        let dest = self
            .frames
            .iter()
            .map(|frame| frame.id)
            .find(|frame| Some(*frame) != self.own_frame)
            .or_else(|| self.frames.first().map(|frame| frame.id));
        match dest {
            Some(dest) => Action::Navigate {
                dest,
                preserve_scroll: false,
            },
            None => Action::Back,
        }
    }

    /// The action of the kind named `kind`, keeping the current one's settings
    /// where the kind has any.
    ///
    /// The engine's own action types, so the panel never spells an action
    /// itself.
    pub fn action_of(&self, kind: &str, current: &Option<Action>) -> Option<Action> {
        let frames: Vec<NodeId> = self.frames.iter().map(|frame| frame.id).collect();
        let current_dest = current
            .as_ref()
            .and_then(|action| action.dest())
            .filter(|dest| frames.contains(dest));
        let frame_dest = current_dest
            .or_else(|| {
                frames
                    .iter()
                    .copied()
                    .find(|frame| Some(*frame) != self.own_frame)
            })
            .or_else(|| frames.first().copied());
        match kind {
            "Navigate to" => Some(Action::Navigate {
                dest: frame_dest?,
                preserve_scroll: matches!(
                    current,
                    Some(Action::Navigate {
                        preserve_scroll: true,
                        ..
                    })
                ),
            }),
            "Back" => Some(Action::Back),
            "Scroll to" => Some(Action::ScrollTo {
                dest: self.scroll_targets.first().map(|target| target.id)?,
                offset: [0.0, 0.0],
            }),
            "Open link" => Some(Action::OpenUrl { url: String::new() }),
            "Open overlay" => Some(Action::OpenOverlay {
                dest: frame_dest?,
                position: OverlayPosition::Center,
                close_on_outside: true,
                background: None,
                offset: None,
            }),
            "Swap overlay" => Some(Action::SwapOverlay { dest: frame_dest? }),
            "Close overlay" => Some(Action::CloseOverlay),
            "Change to" => Some(Action::ChangeTo {
                dest: self
                    .variants
                    .iter()
                    .map(|variant| variant.id)
                    .find(|variant| Some(*variant) != self.own_component)
                    .or_else(|| self.variants.first().map(|variant| variant.id))?,
            }),
            _ => None,
        }
    }

    /// The places the action at `index` can go.
    ///
    /// A "Navigate to" goes to a frame or a section, an overlay goes to a
    /// frame, a "Change to" goes to a variant and a "Scroll to" goes to a
    /// layer inside the frame.
    pub fn destinations_for(&self, action: &Action) -> &[Destination] {
        match action {
            Action::Navigate { .. } => &self.destinations,
            Action::OpenOverlay { .. } | Action::SwapOverlay { .. } => &self.frames,
            Action::ChangeTo { .. } => &self.variants,
            Action::ScrollTo { .. } => &self.scroll_targets,
            _ => &[],
        }
    }
}

/// The prototype's settings, as the page holds them.
fn settings_of(doc: &Document, page: NodeId) -> PrototypeSettings {
    match doc.node(page).map(|node| &node.kind) {
        Some(NodeKind::Page { prototype, .. }) => prototype.clone(),
        _ => PrototypeSettings::default(),
    }
}

/// The page's flows, in the engine's order.
fn flows_of(doc: &Document, page: NodeId) -> Vec<Flow> {
    match doc.node(page).map(|node| &node.kind) {
        Some(NodeKind::Page { flows, .. }) => flows.clone(),
        _ => Vec::new(),
    }
}

/// A layer's name, or figcraft's word for a layer the document no longer holds.
fn name_of(doc: &Document, id: NodeId) -> String {
    doc.node(id)
        .map(|node| node.name.clone())
        .unwrap_or_else(|| MISSING_LAYER.to_owned())
}

/// The places a "Change to" can pick: the variants next to the layer's
/// component.
fn variants_for(doc: &Document, id: NodeId) -> Vec<NodeId> {
    let component = figcraft_engine::components::owning_component(doc, id).or_else(|| {
        match doc.node(id).map(|node| &node.kind) {
            Some(NodeKind::Instance { component, .. }) => Some(*component),
            _ => None,
        }
    });
    let set = component
        .and_then(|component| doc.node(component))
        .and_then(|component| component.parent)
        .filter(|parent| {
            doc.node(*parent)
                .is_some_and(|node| matches!(node.kind, NodeKind::ComponentSet { .. }))
        });
    match set.and_then(|set| doc.node(set)) {
        Some(set) => set.children.clone(),
        None => component.into_iter().collect(),
    }
}

// ── The words of an interaction ─────────────────────────────────────────────

/// The ten triggers, in the order the picker shows them.
///
/// figcraft's order, separators included: the gesture triggers, then the key
/// one, then the mouse ones, then the timed one.
pub const TRIGGERS: [&str; 10] = [
    "On click",
    "On drag",
    "While hovering",
    "While pressing",
    "Key/Gamepad",
    "Mouse enter",
    "Mouse leave",
    "Mouse down",
    "Mouse up",
    "After delay",
];

/// The trigger's own name, as the picker shows it.
pub fn trigger_kind(trigger: &Trigger) -> &'static str {
    match trigger {
        Trigger::OnClick => "On click",
        Trigger::OnDrag => "On drag",
        Trigger::OnHover => "While hovering",
        Trigger::OnPress => "While pressing",
        Trigger::KeyPress { .. } => "Key/Gamepad",
        Trigger::MouseEnter => "Mouse enter",
        Trigger::MouseLeave => "Mouse leave",
        Trigger::MouseDown => "Mouse down",
        Trigger::MouseUp => "Mouse up",
        Trigger::AfterDelay { .. } => "After delay",
    }
}

/// The trigger of the kind named `kind`, keeping what the old one had where
/// the kind allows it.
///
/// A key trigger keeps its key and a timed one keeps its delay; every other
/// kind starts from nothing.
pub fn trigger_of(kind: &str, current: &Trigger) -> Trigger {
    match kind {
        "On drag" => Trigger::OnDrag,
        "While hovering" => Trigger::OnHover,
        "While pressing" => Trigger::OnPress,
        "Key/Gamepad" => match current {
            Trigger::KeyPress { key } => Trigger::KeyPress { key: key.clone() },
            _ => Trigger::KeyPress { key: String::new() },
        },
        "Mouse enter" => Trigger::MouseEnter,
        "Mouse leave" => Trigger::MouseLeave,
        "Mouse down" => Trigger::MouseDown,
        "Mouse up" => Trigger::MouseUp,
        "After delay" => match current {
            Trigger::AfterDelay { ms } => Trigger::AfterDelay { ms: *ms },
            _ => Trigger::AfterDelay { ms: 800 },
        },
        _ => Trigger::OnClick,
    }
}

/// The trigger as the list shows it ("Key/Gamepad Space", "After delay
/// 800ms").
pub fn trigger_summary(trigger: &Trigger) -> String {
    match trigger {
        Trigger::KeyPress { key } if key.is_empty() => "Key/Gamepad".to_owned(),
        // A controller button names itself ("Gamepad A").
        Trigger::KeyPress { key } if figcraft_doc::proto::is_gamepad_button(key) => key.clone(),
        Trigger::KeyPress { key } => format!("Key/Gamepad {key}"),
        Trigger::AfterDelay { ms } => format!("After delay {ms}ms"),
        other => trigger_kind(other).to_owned(),
    }
}

/// The eight transitions, in the order the picker shows them.
pub const TRANSITIONS: [&str; 8] = [
    "Instant",
    "Dissolve",
    "Smart animate",
    "Move in",
    "Move out",
    "Push",
    "Slide in",
    "Slide out",
];

/// The transition of the kind named `kind`, keeping the old one's direction
/// where the kind has one.
pub fn transition_of(kind: &str, current: Transition) -> Transition {
    let direction = current.direction().unwrap_or_default();
    match kind {
        "Dissolve" => Transition::Dissolve,
        "Smart animate" => Transition::SmartAnimate,
        "Move in" => Transition::MoveIn { direction },
        "Move out" => Transition::MoveOut { direction },
        "Push" => Transition::Push { direction },
        "Slide in" => Transition::SlideIn { direction },
        "Slide out" => Transition::SlideOut { direction },
        _ => Transition::Instant,
    }
}

/// The one-line summary of one action ("Navigate to Frame 2").
pub fn action_summary(doc: &Document, action: &Action) -> String {
    let vars = &doc.variables;
    match action {
        Action::OpenUrl { url } if !url.is_empty() => format!("Open link {url}"),
        Action::SetVariable { variable, expr, .. } => {
            let name = vars
                .get(variable)
                .map_or(variable.as_str(), |variable| variable.name.as_str());
            match expr {
                Some(expr) => format!("Set {name} to {}", expr.display(vars)),
                None => format!("Set variable {name}"),
            }
        }
        Action::SetVariableMode { collection, mode } => {
            let found = vars.collection(collection);
            let name = found
                .map(|found| found.name.clone())
                .unwrap_or_else(|| collection.clone());
            let mode = found
                .and_then(|found| found.modes.iter().find(|m| &m.id == mode))
                .map_or(mode.as_str(), |m| m.name.as_str());
            format!("Set {name} to {mode}")
        }
        Action::Conditional { blocks } => {
            match blocks.first().and_then(|block| block.condition.as_ref()) {
                Some(condition) => format!("If {}", condition.display(vars)),
                None => action.label().to_owned(),
            }
        }
        other => match other.dest() {
            Some(dest) => format!("{} {}", other.label(), name_of(doc, dest)),
            None => other.label().to_owned(),
        },
    }
}

/// The summaries of an interaction's actions, joined the way figcraft joins
/// them.
fn action_summaries(doc: &Document, actions: &[Action]) -> String {
    actions
        .iter()
        .map(|action| action_summary(doc, action))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The scroll positions, in the order the picker shows them.
pub const SCROLL_POSITIONS: [(&str, ScrollPosition); 3] = [
    ("Scroll with parent", ScrollPosition::Scrolls),
    ("Fixed", ScrollPosition::Fixed),
    ("Sticky", ScrollPosition::Sticky),
];

/// The overflows, in the order the picker shows them.
pub const OVERFLOWS: [(&str, Overflow); 4] = [
    ("No scrolling", Overflow::None),
    ("Horizontal", Overflow::Horizontal),
    ("Vertical", Overflow::Vertical),
    ("Both directions", Overflow::Both),
];

// ═══════════════════════════════════════════════════════════════════════════
// The commands
// ═══════════════════════════════════════════════════════════════════════════

/// Which scroll property a change is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollProp {
    /// The layer's scroll position.
    Position,
    /// The frame's overflow.
    Overflow,
}

/// The value of a scroll property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollSetting {
    /// A scroll position.
    Position(ScrollPosition),
    /// An overflow.
    Overflow(Overflow),
}

/// A change to the prototype's settings.
///
/// Every field is optional, because a change names the one thing it changes:
/// the engine's `proto.settings` takes the settings one at a time and leaves
/// the rest alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsEdit {
    /// The device the prototype is presented on.
    pub device: Option<String>,
    /// The prototype's background.
    pub background: Option<Color>,
    /// Which way the device is held.
    pub rotation: Option<DeviceRotation>,
    /// The device's colour, by name.
    pub color: Option<String>,
    /// The custom screen's width, in points.
    pub width: Option<f64>,
    /// The custom screen's height, in points.
    pub height: Option<f64>,
    /// Whether the presented frame is resized to the screen.
    pub responsive: Option<bool>,
}

/// A change the Prototype tab can ask for.
///
/// One variant per thing the tab can change, so a change the engine has no
/// command for cannot be written down as though it had one.
#[derive(Clone, Debug, PartialEq)]
pub enum ProtoAction {
    /// Add an interaction.
    AddInteraction {
        /// The layers it is added to.
        ids: Vec<NodeId>,
        /// The trigger that starts it.
        trigger: Trigger,
        /// What it does.
        action: Action,
    },
    /// Remove one interaction.
    RemoveInteraction {
        /// The layers it is removed from.
        ids: Vec<NodeId>,
        /// Its index.
        index: usize,
    },
    /// Remove every interaction from a set of layers.
    ClearInteractions {
        /// The layers they are removed from.
        ids: Vec<NodeId>,
    },
    /// Replace every interaction on a set of layers with one.
    ///
    /// figcraft's answer to a selection that disagrees: one click, and one
    /// undo step. The engine's `command.batch` is what makes it one step.
    ReplaceInteractions {
        /// The layers they are replaced on.
        ids: Vec<NodeId>,
        /// The trigger the new interaction has.
        trigger: Trigger,
        /// What it does.
        action: Action,
    },
    /// Change one interaction.
    PatchInteraction {
        /// The layers it is changed on.
        ids: Vec<NodeId>,
        /// Its index.
        index: usize,
        /// The fields to change, in the engine's own spelling.
        patch: Value,
    },
    /// Add a flow starting point.
    AddFlow {
        /// The top-level frame the flow starts at.
        start: NodeId,
    },
    /// Rename a flow, or change its description.
    RenameFlow {
        /// The frame the flow starts at.
        start: NodeId,
        /// Its new name.
        name: Option<String>,
        /// Its new description.
        description: Option<String>,
    },
    /// Remove a flow starting point.
    RemoveFlow {
        /// The frame the flow starts at.
        start: NodeId,
    },
    /// Change the prototype's settings.
    Settings {
        /// What to change.
        edit: SettingsEdit,
    },
    /// Open the prototype preview.
    Preview {
        /// The frame to preview, when it is not the default.
        start: Option<NodeId>,
    },
    /// Present the prototype.
    Present {
        /// The frame to present, when it is not the default.
        start: Option<NodeId>,
    },
    /// Change how a layer scrolls.
    Scroll {
        /// The layers to change.
        ids: Vec<NodeId>,
        /// Which property.
        prop: ScrollProp,
        /// What to set it to.
        value: ScrollSetting,
    },
}

impl ProtoAction {
    /// The engine's command id and parameters for this change.
    ///
    /// The engine's own spellings, so a change made from the panel is the
    /// same change the CLI, the keymap and an agent would make.
    pub fn command(&self) -> (String, Value) {
        match self {
            Self::AddInteraction {
                ids,
                trigger,
                action,
            } => (
                "proto.interaction.add".to_owned(),
                json!({
                    "ids": ids,
                    "trigger": serde_json::to_value(trigger).unwrap_or(Value::Null),
                    "action": serde_json::to_value(action).unwrap_or(Value::Null),
                }),
            ),
            Self::RemoveInteraction { ids, index } => (
                "proto.interaction.remove".to_owned(),
                json!({ "ids": ids, "index": index }),
            ),
            Self::ClearInteractions { ids } => {
                ("proto.interaction.clear".to_owned(), json!({ "ids": ids }))
            }
            Self::ReplaceInteractions {
                ids,
                trigger,
                action,
            } => (
                "command.batch".to_owned(),
                json!({
                    "label": "Replace interactions",
                    "commands": [
                        {
                            "command": "proto.interaction.clear",
                            "params": { "ids": ids },
                        },
                        {
                            "command": "proto.interaction.add",
                            "params": {
                                "ids": ids,
                                "trigger": serde_json::to_value(trigger).unwrap_or(Value::Null),
                                "action": serde_json::to_value(action).unwrap_or(Value::Null),
                            },
                        },
                    ],
                }),
            ),
            Self::PatchInteraction { ids, index, patch } => {
                let mut params = json!({ "ids": ids, "index": index });
                if let (Some(object), Some(fields)) = (params.as_object_mut(), patch.as_object()) {
                    for (key, value) in fields {
                        object.insert(key.clone(), value.clone());
                    }
                }
                ("proto.interaction.set".to_owned(), params)
            }
            Self::AddFlow { start } => ("proto.flow.add".to_owned(), json!({ "start": start })),
            Self::RenameFlow {
                start,
                name,
                description,
            } => {
                let mut params = json!({ "start": start });
                if let Some(name) = name {
                    params["name"] = json!(name);
                }
                if let Some(description) = description {
                    params["description"] = json!(description);
                }
                ("proto.flow.rename".to_owned(), params)
            }
            Self::RemoveFlow { start } => {
                ("proto.flow.remove".to_owned(), json!({ "start": start }))
            }
            Self::Settings { edit } => {
                let mut params = json!({});
                if let Some(device) = &edit.device {
                    params["device"] = json!(device);
                }
                if let Some(background) = edit.background {
                    params["background"] = json!(background.to_css_hex());
                }
                if let Some(rotation) = edit.rotation {
                    params["rotation"] = json!(match rotation {
                        DeviceRotation::Portrait => "portrait",
                        DeviceRotation::Landscape => "landscape",
                    });
                }
                if let Some(color) = &edit.color {
                    params["color"] = json!(color);
                }
                if let Some(width) = edit.width {
                    params["width"] = json!(width);
                }
                if let Some(height) = edit.height {
                    params["height"] = json!(height);
                }
                if let Some(responsive) = edit.responsive {
                    params["responsive"] = json!(responsive);
                }
                ("proto.settings".to_owned(), params)
            }
            Self::Preview { start } => {
                let mut params = json!({});
                if let Some(start) = start {
                    params["start"] = json!(start);
                }
                ("proto.preview".to_owned(), params)
            }
            Self::Present { start } => {
                let mut params = json!({});
                if let Some(start) = start {
                    params["start"] = json!(start);
                }
                ("proto.present".to_owned(), params)
            }
            Self::Scroll { ids, prop, value } => {
                let (key, spelling) = match (prop, value) {
                    (ScrollProp::Position, ScrollSetting::Position(position)) => {
                        ("scrollPosition", scroll_position_spelling(*position))
                    }
                    (ScrollProp::Overflow, ScrollSetting::Overflow(overflow)) => {
                        ("overflow", overflow_spelling(*overflow))
                    }
                    // A property and a value of the other kind cannot be
                    // spelled: the engine would refuse it, so nothing is sent.
                    _ => ("", ""),
                };
                (
                    "node.set".to_owned(),
                    json!({ "ids": ids, "props": { key: spelling } }),
                )
            }
        }
    }
}

/// The engine's spelling of a scroll position.
fn scroll_position_spelling(position: ScrollPosition) -> &'static str {
    match position {
        ScrollPosition::Scrolls => "scrolls",
        ScrollPosition::Fixed => "fixed",
        ScrollPosition::Sticky => "sticky",
    }
}

/// The engine's spelling of an overflow.
fn overflow_spelling(overflow: Overflow) -> &'static str {
    match overflow {
        Overflow::None => "none",
        Overflow::Horizontal => "horizontal",
        Overflow::Vertical => "vertical",
        Overflow::Both => "both",
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel's own state
// ═══════════════════════════════════════════════════════════════════════════

/// Which menu of the panel is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtoMenu {
    /// The device picker.
    Device,
    /// The device's colours.
    DeviceColor,
    /// The trigger picker of one interaction.
    Trigger(usize),
    /// The action picker of one interaction.
    ActionKind(usize),
    /// The destination picker of one interaction.
    Destination(usize),
    /// The overlay position picker of one interaction.
    OverlayPosition(usize),
    /// The transition picker of one interaction.
    Transition(usize),
    /// The easing picker of one interaction.
    Easing(usize),
    /// The controller buttons of one interaction's key trigger.
    Gamepad(usize),
    /// The scroll position picker.
    ScrollPosition,
    /// The overflow picker.
    Overflow,
}

/// Which field is being typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtoField {
    /// A flow's name.
    FlowName(NodeId),
    /// A flow's description.
    FlowDescription(NodeId),
    /// An interaction's link.
    Url(usize),
    /// The prototype's background, as hex.
    Background,
    /// An interaction's duration, in milliseconds.
    Duration(usize),
    /// A trigger's delay, in milliseconds.
    Delay(usize),
    /// A key trigger's key.
    Key(usize),
}

/// The menu of the controller buttons a key trigger can name.
pub const GAMEPAD_BUTTONS: [&str; 17] = figcraft_doc::proto::GAMEPAD_BUTTONS;

/// The text being typed into one field.
#[derive(Clone, Debug, PartialEq)]
pub struct ProtoDraft {
    /// Which field.
    pub field: ProtoField,
    /// What has been typed.
    pub text: String,
    /// Whether the text is selected, so the first key replaces it.
    pub select_all: bool,
    /// The layers the edit applies to.
    ///
    /// Taken when the field is opened, because a commit is a command and a
    /// command names its layers: the selection at the moment of the edit.
    pub ids: Vec<NodeId>,
    /// The interaction the edit applies to, when it is one.
    pub index: Option<usize>,
    /// The interaction's trigger, when the edit is about the trigger.
    ///
    /// The engine's own type, so the panel never spells a trigger itself: a
    /// timed trigger's delay lives in the trigger, and a mouse trigger's
    /// delay lives beside it.
    pub trigger: Option<Trigger>,
}

/// The panel's own state.
///
/// Presentation only: never in the document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProtoPanelState {
    /// Which menu is open, if any.
    pub open: Option<ProtoMenu>,
    /// The text being typed, if any.
    pub draft: Option<ProtoDraft>,
    /// The interaction whose editor is open, if any.
    pub editing: Option<usize>,
}

impl ProtoPanelState {
    /// The state a panel starts in.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the panel is part-way through something.
    ///
    /// Read-only, and kept in step with [`Self::cancel`]: the editor's Escape
    /// ladder reads this to decide which rung is on top, and only then asks
    /// the panel to act on it.
    pub fn is_busy(&self) -> bool {
        self.draft.is_some() || self.editing.is_some()
    }

    /// Give up whatever the panel was part-way through.
    ///
    /// Returns whether anything was given up, so the ladder knows the key was
    /// consumed.
    pub fn cancel(&mut self) -> bool {
        let was_editing = self.editing.take().is_some();
        let was_typing = self.draft.take().is_some();
        self.open = None;
        was_editing || was_typing
    }

    /// Handle one keystroke.
    ///
    /// Returns the change the key asks for, if the key was the panel's own.
    /// Escape is not claimed: it belongs to the editor's ladder, which walks
    /// down from the topmost thing, and this draft is one of the rungs the
    /// ladder knows about.
    pub fn handle_key(&mut self, key: &str) -> Option<ProtoAction> {
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
            // Arrows are not characters. The panel has no nudge to make, so
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
    fn commit_draft(&mut self, draft: ProtoDraft) -> Option<ProtoAction> {
        match draft.field {
            ProtoField::FlowName(start) => {
                let name = draft.text.trim().to_owned();
                if name.is_empty() {
                    return None;
                }
                Some(ProtoAction::RenameFlow {
                    start,
                    name: Some(name),
                    description: None,
                })
            }
            ProtoField::FlowDescription(start) => Some(ProtoAction::RenameFlow {
                start,
                name: None,
                description: Some(draft.text),
            }),
            ProtoField::Background => {
                let color = Color::from_hex_or_name(draft.text.trim())?;
                Some(ProtoAction::Settings {
                    edit: SettingsEdit {
                        background: Some(color),
                        ..SettingsEdit::default()
                    },
                })
            }
            ProtoField::Url(_) => {
                let index = draft.index?;
                Some(ProtoAction::PatchInteraction {
                    ids: draft.ids,
                    index,
                    patch: json!({
                        "action": { "type": "openUrl", "url": draft.text.trim() }
                    }),
                })
            }
            ProtoField::Duration(_) => {
                let index = draft.index?;
                let ms = draft.text.trim().parse::<f64>().ok()?;
                if !ms.is_finite() {
                    return None;
                }
                Some(ProtoAction::PatchInteraction {
                    ids: draft.ids,
                    index,
                    patch: json!({ "durationMs": ms.clamp(1.0, 10_000.0).round() }),
                })
            }
            ProtoField::Delay(_) => {
                let index = draft.index?;
                let ms = draft.text.trim().parse::<f64>().ok()?;
                if !ms.is_finite() {
                    return None;
                }
                let ms = ms.clamp(0.0, 600_000.0).round();
                let patch = match draft.trigger {
                    // A timed trigger's delay is the trigger itself.
                    Some(Trigger::AfterDelay { .. }) => {
                        json!({ "trigger": { "type": "afterDelay", "ms": ms } })
                    }
                    _ => json!({ "delayMs": ms }),
                };
                Some(ProtoAction::PatchInteraction {
                    ids: draft.ids,
                    index,
                    patch,
                })
            }
            ProtoField::Key(_) => {
                let index = draft.index?;
                let key = draft.text.trim().to_owned();
                Some(ProtoAction::PatchInteraction {
                    ids: draft.ids,
                    index,
                    patch: json!({ "trigger": { "type": "keyPress", "key": key } }),
                })
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// What a surface that hosts the Prototype panel must be able to do.
///
/// The panel draws and asks; the surface owns the engine and runs the changes.
/// The panel never touches the document itself, so the engine's command surface
/// stays the one operation boundary.
pub trait ProtoHost: Sized + 'static {
    /// Run a change through the engine.
    fn run_proto_action(&mut self, action: ProtoAction, cx: &mut Context<Self>);
}

/// The Prototype tab, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own state,
/// and the surface that runs the changes. It returns an element, so the
/// Inspector can put it in a tab of its own and a test can put it in a window
/// on its own.
pub fn proto_panel<T: ProtoHost>(
    model: &ProtoModel,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div()
        .id("proto-panel")
        .debug_selector(|| "proto-panel".to_owned())
        .flex()
        .flex_col()
        .gap_3()
        .px(px(14.0))
        .py(px(12.0));

    // Nothing selected: the page's flows and the prototype's settings, and
    // nothing about a layer.
    if !model.has_selection {
        column = column.child(flows_overview(model, state, cx));
        column = column.child(separator());
        column = column.child(settings_section(model, state, cx));
        return column.into_any_element();
    }

    // A top-level frame can start a flow, so it gets the flow section first.
    if model.is_top_level_frame {
        column = column.child(flow_section(model, state, cx));
        column = column.child(separator());
    }

    if model.count.is_mixed() {
        column = column.child(mixed_interactions(model, cx));
    } else {
        column = column.child(interactions(model, state, cx));
    }

    column = column.child(separator());
    if model.scroll.in_frame || model.scroll.is_frame {
        column = column.child(scroll_section(model, state, cx));
    }
    column.into_any_element()
}

/// The page's flows, and the way to start another one.
///
/// The empty state's first half: with nothing selected, figcraft shows the
/// page's flows and a button that starts one.
fn flows_overview<T: ProtoHost>(
    model: &ProtoModel,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let mut actions = Vec::new();
    if let Some(frame) = model.free_frame {
        actions.push((
            "＋".to_owned(),
            "proto-flow-add".to_owned(),
            ProtoAction::AddFlow { start: frame },
        ));
    }
    let mut section = section_header("Flow starting points", actions, model.flows.is_empty(), cx);
    if model.flows.is_empty() {
        section = section.child(note(
            "Select a frame and add a starting point to present it as a flow.",
        ));
    }
    for flow in &model.flows {
        section = section.child(flow_row(model, flow, state, cx));
        // Its description under it.
        let description = flow.description.trim();
        if !description.is_empty() {
            section = section.child(
                div()
                    .pl(px(26.0))
                    .text_xs()
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(description.to_owned()),
            );
        }
    }
    section = section.child(note(FLOW_SOURCE_NOTE));
    section
}

/// One flow's row: present it, rename it, preview it, remove it.
fn flow_row<T: ProtoHost>(
    model: &ProtoModel,
    flow: &FlowEntry,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let start = flow.start;
    let name = flow.name.clone();
    let editing = is_editing(state, ProtoField::FlowName(start));
    let shown = match state.borrow().draft.as_ref() {
        Some(draft) if draft.field == ProtoField::FlowName(start) => format!("{}|", draft.text),
        _ => name.clone(),
    };
    let draft = ProtoDraft {
        field: ProtoField::FlowName(start),
        text: name,
        select_all: true,
        ids: model.ids.clone(),
        index: None,
        trigger: None,
    };
    let shared = Rc::clone(state);
    let field = div()
        .id(SharedString::from(format!("proto-flow-name-{start}")))
        .debug_selector(move || format!("proto-flow-name-{start}"))
        .flex_1()
        .text_xs()
        .text_color(rgb(if editing {
            theme::TEXT
        } else {
            theme::TEXT_SECONDARY
        }))
        .border_1()
        .border_color(rgb(if editing {
            theme::ACCENT
        } else {
            theme::BORDER_SOFT
        }))
        .rounded_sm()
        .px(px(6.0))
        .py(px(4.0))
        .cursor_pointer()
        .child(shown)
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = None;
                state.draft = Some(draft.clone());
            }
            cx.notify();
        }));
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(action_button(
            "▶",
            format!("proto-flow-present-{start}"),
            ProtoAction::Present { start: Some(start) },
            cx,
        ))
        .child(field)
        .child(action_button(
            "▣",
            format!("proto-flow-preview-{start}"),
            ProtoAction::Preview { start: Some(start) },
            cx,
        ))
        .child(action_button(
            "−",
            format!("proto-flow-remove-{start}"),
            ProtoAction::RemoveFlow { start },
            cx,
        ))
}

/// The selected layer's own flow: it is a top-level frame, so it can start
/// one.
fn flow_section<T: ProtoHost>(
    model: &ProtoModel,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let mut actions = Vec::new();
    if model.flow.is_none() {
        if let Some(frame) = model.free_frame {
            actions.push((
                "▣".to_owned(),
                "proto-flow-preview-frame".to_owned(),
                ProtoAction::Preview { start: Some(frame) },
            ));
        }
        if let Some(start) = model.ids.first().copied() {
            actions.push((
                "＋".to_owned(),
                "proto-flow-add".to_owned(),
                ProtoAction::AddFlow { start },
            ));
        }
    }
    let mut section = section_header("Flow starting point", actions, model.flow.is_none(), cx);
    if let Some(flow) = &model.flow {
        section = section.child(flow_row(model, flow, state, cx));
        let start = flow.start;
        let description = flow.description.clone();
        let editing = is_editing(state, ProtoField::FlowDescription(start));
        let shown = match state.borrow().draft.as_ref() {
            Some(draft) if draft.field == ProtoField::FlowDescription(start) => {
                format!("{}|", draft.text)
            }
            _ => {
                if description.is_empty() {
                    "Add a description".to_owned()
                } else {
                    description.clone()
                }
            }
        };
        let draft = ProtoDraft {
            field: ProtoField::FlowDescription(start),
            text: description,
            select_all: false,
            ids: model.ids.clone(),
            index: None,
            trigger: None,
        };
        let shared = Rc::clone(state);
        section = section.child(
            div()
                .id(SharedString::from(format!(
                    "proto-flow-description-{start}"
                )))
                .debug_selector(move || format!("proto-flow-description-{start}"))
                .text_xs()
                .text_color(rgb(if editing {
                    theme::TEXT
                } else {
                    theme::TEXT_MUTED
                }))
                .cursor_pointer()
                .child(shown)
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.open = None;
                        state.draft = Some(draft.clone());
                    }
                    cx.notify();
                })),
        );
    }
    section = section.child(note(FLOW_SOURCE_NOTE));
    section
}

/// The selected layers' interactions.
fn interactions<T: ProtoHost>(
    model: &ProtoModel,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let mut actions = Vec::new();
    if !model.targets.is_empty() {
        actions.push((
            "＋".to_owned(),
            "proto-interaction-add".to_owned(),
            ProtoAction::AddInteraction {
                ids: model.targets.clone(),
                trigger: Trigger::OnClick,
                action: model.default_action(),
            },
        ));
    }
    let mut section = section_header(
        "Interactions",
        actions,
        matches!(model.count, ProtoValue::Set(0)) || model.count.is_unset(),
        cx,
    );
    for row in &model.interactions {
        section = section.child(interaction_row(model, row, state, cx));
    }
    let editing = state.borrow().editing;
    if let Some(index) = editing {
        if let Some(row) = model.interactions.iter().find(|row| row.index == index) {
            section = section.child(interaction_editor(model, row, state, cx));
        }
    }
    section = section.child(note(SAVE_PATH_NOTE));
    section
}

/// The selected layers hold different interactions.
///
/// figcraft's answer to a selection that disagrees: one button replaces them
/// all with a single click interaction, and another clears them. The list is
/// not shown, because there is no one list to show.
fn mixed_interactions<T: ProtoHost>(model: &ProtoModel, cx: &mut Context<T>) -> impl IntoElement {
    let mut actions = Vec::new();
    if !model.ids.is_empty() {
        actions.push((
            "＋".to_owned(),
            "proto-interaction-replace".to_owned(),
            ProtoAction::ReplaceInteractions {
                ids: model.ids.clone(),
                trigger: Trigger::OnClick,
                action: model.default_action(),
            },
        ));
    }
    let mut section = section_header("Interactions", actions, false, cx);
    section = section.child(note("Mixed interactions: click + to replace them"));
    section = section.child(action_button(
        "Remove all",
        "proto-interaction-clear".to_owned(),
        ProtoAction::ClearInteractions {
            ids: model.ids.clone(),
        },
        cx,
    ));
    section
}

/// One interaction's row: the trigger, what it does, and the way to remove it.
fn interaction_row<T: ProtoHost>(
    model: &ProtoModel,
    row: &InteractionRow,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let open = state.borrow().editing == Some(index);
    let trigger = match &row.trigger {
        ProtoValue::Set(text) => text.clone(),
        ProtoValue::Unset => NOT_SET.to_owned(),
        ProtoValue::Mixed => MIXED.to_owned(),
    };
    let summary = match &row.summary {
        ProtoValue::Set(text) => text.clone(),
        ProtoValue::Unset => NOT_SET.to_owned(),
        ProtoValue::Mixed => MIXED.to_owned(),
    };
    let shared = Rc::clone(state);
    let body = div()
        .id(SharedString::from(format!("proto-interaction-{index}")))
        .debug_selector(move || format!("proto-interaction-{index}"))
        .flex_1()
        .flex()
        .flex_col()
        .gap_1()
        .cursor_pointer()
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.editing = if state.editing == Some(index) {
                    None
                } else {
                    Some(index)
                };
                state.open = None;
                state.draft = None;
            }
            cx.notify();
        }))
        // A small connection glyph, as figcraft draws one.
        .child(
            div()
                .size(px(10.0))
                .rounded_full()
                .border_1()
                .border_color(rgb(theme::ACCENT))
                .bg(rgb(theme::ACCENT_WASH)),
        )
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(theme::TEXT))
                .child(trigger),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(summary),
        );
    div()
        .flex()
        .items_start()
        .gap_2()
        .px(px(6.0))
        .py(px(6.0))
        .rounded_md()
        .bg(rgb(if open {
            theme::ACCENT_WASH
        } else {
            theme::WINDOW
        }))
        .child(body)
        .child(action_button(
            "−",
            format!("proto-interaction-remove-{index}"),
            ProtoAction::RemoveInteraction {
                ids: model.targets.clone(),
                index,
            },
            cx,
        ))
}

/// The editor for one interaction: its trigger, what it does, and how it
/// gets there.
///
/// Inline rather than floating: Spool's Inspector has no surface that owns a
/// popover, so the editor opens under the row it belongs to.
fn interaction_editor<T: ProtoHost>(
    model: &ProtoModel,
    row: &InteractionRow,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let Some(interaction) = row.interaction.clone() else {
        return div().into_any_element();
    };
    let mut block = div()
        .id(SharedString::from(format!(
            "proto-interaction-editor-{index}"
        )))
        .debug_selector(move || format!("proto-interaction-editor-{index}"))
        .flex()
        .flex_col()
        .gap_2()
        .px(px(8.0))
        .py(px(8.0))
        .rounded_md()
        .bg(rgb(theme::SURFACE_RAISED));

    // ── The trigger ──
    block = block.child(sublabel("Trigger"));
    let trigger_label = trigger_kind(&interaction.trigger).to_owned();
    let open = state.borrow().open == Some(ProtoMenu::Trigger(index));
    let shared = Rc::clone(state);
    let control = dropdown(
        &trigger_label,
        format!("proto-interaction-trigger-{index}"),
        open,
    )
    .on_click(cx.listener(move |_host, _event, _window, cx| {
        {
            let mut state = shared.borrow_mut();
            state.open = if state.open == Some(ProtoMenu::Trigger(index)) {
                None
            } else {
                Some(ProtoMenu::Trigger(index))
            };
            state.draft = None;
        }
        cx.notify();
    }));
    block = block.child(control);
    if open {
        let ids = model.targets.clone();
        let mut list = menu_box();
        for kind in TRIGGERS {
            let next = trigger_of(kind, &interaction.trigger);
            list = list.child(menu_item(
                kind,
                &format!("proto-interaction-trigger-option-{index}-{kind}"),
                ProtoAction::PatchInteraction {
                    ids: ids.clone(),
                    index,
                    patch: json!({ "trigger": serde_json::to_value(&next).unwrap_or(Value::Null) }),
                },
                cx,
            ));
        }
        block = block.child(list);
    }

    // A mouse trigger's delay, a timed one's, and a key trigger's key.
    match &interaction.trigger {
        Trigger::MouseEnter | Trigger::MouseLeave | Trigger::MouseDown | Trigger::MouseUp => {
            block = block.child(field_row(
                "Delay",
                &format!("{} ms", interaction.delay_ms),
                format!("proto-interaction-delay-{index}"),
                ProtoField::Delay(index),
                interaction.delay_ms.to_string(),
                model.targets.clone(),
                index,
                Some(interaction.trigger.clone()),
                state,
                cx,
            ));
        }
        Trigger::AfterDelay { ms } => {
            block = block.child(field_row(
                "Delay",
                &format!("{ms} ms"),
                format!("proto-interaction-delay-{index}"),
                ProtoField::Delay(index),
                ms.to_string(),
                model.targets.clone(),
                index,
                Some(interaction.trigger.clone()),
                state,
                cx,
            ));
        }
        Trigger::KeyPress { key } => {
            let editing = is_editing(state, ProtoField::Key(index));
            let draft = ProtoDraft {
                field: ProtoField::Key(index),
                text: key.clone(),
                select_all: true,
                ids: model.targets.clone(),
                index: Some(index),
                trigger: Some(interaction.trigger.clone()),
            };
            let shown = match state.borrow().draft.as_ref() {
                Some(current) if current.field == ProtoField::Key(index) => {
                    format!("{}|", current.text)
                }
                _ => {
                    if key.is_empty() {
                        "Click to record a key".to_owned()
                    } else {
                        key.clone()
                    }
                }
            };
            let key_field = typed_field(
                shown,
                format!("proto-interaction-key-{index}"),
                editing,
                draft,
                state,
                cx,
            );
            // The controller menu beside the key: a button names itself.
            let open = state.borrow().open == Some(ProtoMenu::Gamepad(index));
            let shared = Rc::clone(state);
            let controller = dropdown(
                "Controller",
                format!("proto-interaction-controller-{index}"),
                open,
            )
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.open = if state.open == Some(ProtoMenu::Gamepad(index)) {
                        None
                    } else {
                        Some(ProtoMenu::Gamepad(index))
                    };
                    state.draft = None;
                }
                cx.notify();
            }));
            block = block.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(key_field)
                    .child(controller),
            );
            if open {
                let ids = model.targets.clone();
                let mut list = menu_box();
                for button in GAMEPAD_BUTTONS {
                    let next = Trigger::KeyPress {
                        key: button.to_owned(),
                    };
                    list = list.child(menu_item(
                        button,
                        &format!("proto-interaction-controller-option-{index}-{button}"),
                        ProtoAction::PatchInteraction {
                            ids: ids.clone(),
                            index,
                            patch: json!({
                                "trigger": serde_json::to_value(&next).unwrap_or(Value::Null)
                            }),
                        },
                        cx,
                    ));
                }
                block = block.child(list);
            }
        }
        _ => {}
    }

    // ── The action ──
    block = block.child(separator());
    let action = interaction.actions.first().cloned();
    let action_label = action
        .as_ref()
        .map(|action| action.label().to_owned())
        .unwrap_or_else(|| "None".to_owned());
    block = block.child(sublabel(if interaction.actions.len() > 1 {
        "Actions"
    } else {
        "Action"
    }));
    let open = state.borrow().open == Some(ProtoMenu::ActionKind(index));
    let shared = Rc::clone(state);
    let control = dropdown(
        &action_label,
        format!("proto-interaction-action-{index}"),
        open,
    )
    .on_click(cx.listener(move |_host, _event, _window, cx| {
        {
            let mut state = shared.borrow_mut();
            state.open = if state.open == Some(ProtoMenu::ActionKind(index)) {
                None
            } else {
                Some(ProtoMenu::ActionKind(index))
            };
            state.draft = None;
        }
        cx.notify();
    }));
    block = block.child(control);
    if open {
        let ids = model.targets.clone();
        let mut list = menu_box();
        for kind in ACTION_KINDS {
            let Some(next) = model.action_of(kind, &action) else {
                continue;
            };
            list = list.child(menu_item(
                kind,
                &format!("proto-interaction-action-option-{index}-{kind}"),
                ProtoAction::PatchInteraction {
                    ids: ids.clone(),
                    index,
                    patch: json!({ "action": serde_json::to_value(&next).unwrap_or(Value::Null) }),
                },
                cx,
            ));
        }
        block = block.child(list);
    }

    // The action's own fields.
    if let Some(action) = action.clone() {
        block = block.child(action_fields(model, index, &action, state, cx));
    }

    // ── The animation ──
    if action.as_ref().is_some_and(animates) {
        block = block.child(separator());
        block = block.child(sublabel("Animation"));
        let open = state.borrow().open == Some(ProtoMenu::Transition(index));
        let shared = Rc::clone(state);
        let control = dropdown(
            interaction.transition.label(),
            format!("proto-interaction-transition-{index}"),
            open,
        )
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = if state.open == Some(ProtoMenu::Transition(index)) {
                    None
                } else {
                    Some(ProtoMenu::Transition(index))
                };
                state.draft = None;
            }
            cx.notify();
        }));
        block = block.child(control);
        if open {
            let ids = model.targets.clone();
            let mut list = menu_box();
            for kind in TRANSITIONS {
                let next = transition_of(kind, interaction.transition);
                list = list.child(menu_item(
                    kind,
                    &format!("proto-interaction-transition-option-{index}-{kind}"),
                    ProtoAction::PatchInteraction {
                        ids: ids.clone(),
                        index,
                        patch: json!({ "transition": serde_json::to_value(next).unwrap_or(Value::Null) }),
                    },
                    cx,
                ));
            }
            block = block.child(list);
        }
        if let Some(direction) = interaction.transition.direction() {
            let mut row = div().flex().items_center().gap_2();
            for (label, next) in [
                ("Left", Direction::Left),
                ("Right", Direction::Right),
                ("Up", Direction::Top),
                ("Down", Direction::Bottom),
            ] {
                let active = direction == next;
                let ids = model.targets.clone();
                let transition = interaction.transition.with_direction(next);
                row = row.child(
                    div()
                        .id(SharedString::from(format!(
                            "proto-interaction-direction-{index}-{label}"
                        )))
                        .debug_selector(move || {
                            format!("proto-interaction-direction-{index}-{label}")
                        })
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(if active {
                            theme::ACCENT
                        } else {
                            theme::TEXT_SECONDARY
                        }))
                        .border_1()
                        .border_color(rgb(if active {
                            theme::ACCENT
                        } else {
                            theme::BORDER_SOFT
                        }))
                        .rounded_sm()
                        .px(px(4.0))
                        .py(px(3.0))
                        .text_center()
                        .cursor_pointer()
                        .child(label)
                        .on_click(cx.listener(move |host, _event, _window, cx| {
                            host.run_proto_action(
                                ProtoAction::PatchInteraction {
                                    ids: ids.clone(),
                                    index,
                                    patch: json!({
                                        "transition":
                                            serde_json::to_value(transition).unwrap_or(Value::Null)
                                    }),
                                },
                                cx,
                            );
                        })),
                );
            }
            block = block.child(row);
        }
        // The easing and the duration.
        let open = state.borrow().open == Some(ProtoMenu::Easing(index));
        let shared = Rc::clone(state);
        let control = dropdown(
            interaction.easing.label(),
            format!("proto-interaction-easing-{index}"),
            open,
        )
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = if state.open == Some(ProtoMenu::Easing(index)) {
                    None
                } else {
                    Some(ProtoMenu::Easing(index))
                };
                state.draft = None;
            }
            cx.notify();
        }));
        block = block.child(control);
        if open {
            let ids = model.targets.clone();
            let mut list = menu_box();
            for easing in Easing::ALL {
                let next = easing;
                list = list.child(menu_item(
                    easing.label(),
                    &format!("proto-interaction-easing-option-{index}-{}", easing.label()),
                    ProtoAction::PatchInteraction {
                        ids: ids.clone(),
                        index,
                        patch: json!({ "easing": serde_json::to_value(next).unwrap_or(Value::Null) }),
                    },
                    cx,
                ));
            }
            block = block.child(list);
        }
        block = block.child(field_row(
            "Duration",
            &format!("{} ms", interaction.duration_ms),
            format!("proto-interaction-duration-{index}"),
            ProtoField::Duration(index),
            interaction.duration_ms.to_string(),
            model.targets.clone(),
            index,
            None,
            state,
            cx,
        ));
    }

    block.into_any_element()
}

/// The action kinds the picker offers, in figcraft's order.
pub const ACTION_KINDS: [&str; 8] = [
    "Navigate to",
    "Back",
    "Scroll to",
    "Open overlay",
    "Swap overlay",
    "Close overlay",
    "Open link",
    "Change to",
];

/// Whether an action plays an animation, so the interaction's transition
/// applies to it.
fn animates(action: &Action) -> bool {
    !matches!(
        action,
        Action::OpenUrl { .. }
            | Action::SetVariable { .. }
            | Action::SetVariableMode { .. }
            | Action::CloseOverlay
            | Action::Conditional { .. }
    )
}

/// The action's own fields: where it goes, and what else it does.
fn action_fields<T: ProtoHost>(
    model: &ProtoModel,
    index: usize,
    action: &Action,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let mut block = div().flex().flex_col().gap_2();
    match action {
        Action::Navigate {
            dest,
            preserve_scroll,
        } => {
            block = block.child(destination_field(model, index, action, *dest, state, cx));
            block = block.child(checkbox_row(
                "Preserve scroll position",
                *preserve_scroll,
                &format!("proto-interaction-preserve-scroll-{index}"),
                ProtoAction::PatchInteraction {
                    ids: model.targets.clone(),
                    index,
                    patch: json!({
                        "action": {
                            "type": "navigate",
                            "dest": dest,
                            "preserveScroll": !preserve_scroll,
                        }
                    }),
                },
                cx,
            ));
        }
        Action::OpenOverlay {
            position,
            close_on_outside,
            ..
        } => {
            let Some(dest) = action.dest() else {
                return block.into_any_element();
            };
            block = block.child(destination_field(model, index, action, dest, state, cx));
            block = block.child(sublabel("Overlay position"));
            let open = state.borrow().open == Some(ProtoMenu::OverlayPosition(index));
            let shared = Rc::clone(state);
            let control = dropdown(
                position.label(),
                format!("proto-interaction-overlay-position-{index}"),
                open,
            )
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.open = if state.open == Some(ProtoMenu::OverlayPosition(index)) {
                        None
                    } else {
                        Some(ProtoMenu::OverlayPosition(index))
                    };
                    state.draft = None;
                }
                cx.notify();
            }));
            block = block.child(control);
            if open {
                let ids = model.targets.clone();
                let mut list = menu_box();
                for candidate in OverlayPosition::ALL {
                    let next = with_overlay_position(action, candidate);
                    list = list.child(menu_item(
                        candidate.label(),
                        &format!(
                            "proto-interaction-overlay-position-option-{index}-{}",
                            candidate.label()
                        ),
                        ProtoAction::PatchInteraction {
                            ids: ids.clone(),
                            index,
                            patch: json!({
                                "action": serde_json::to_value(&next).unwrap_or(Value::Null)
                            }),
                        },
                        cx,
                    ));
                }
                block = block.child(list);
            }
            block = block.child(checkbox_row(
                "Close when clicking outside",
                *close_on_outside,
                &format!("proto-interaction-close-on-outside-{index}"),
                ProtoAction::PatchInteraction {
                    ids: model.targets.clone(),
                    index,
                    patch: json!({
                        "action": {
                            "type": "openOverlay",
                            "dest": dest,
                            "position": position,
                            "closeOnOutside": !close_on_outside,
                        }
                    }),
                },
                cx,
            ));
        }
        Action::SwapOverlay { dest }
        | Action::ChangeTo { dest }
        | Action::ScrollTo { dest, .. } => {
            block = block.child(destination_field(model, index, action, *dest, state, cx));
        }
        Action::OpenUrl { url } => {
            block = block.child(field_row(
                "Link",
                url,
                format!("proto-interaction-url-{index}"),
                ProtoField::Url(index),
                url.clone(),
                model.targets.clone(),
                index,
                None,
                state,
                cx,
            ));
        }
        // Back and Close overlay have no settings, and the set-variable and
        // conditional actions have their own editors in figcraft, which this
        // slice does not port.
        Action::SetVariable { .. }
        | Action::SetVariableMode { .. }
        | Action::Conditional { .. }
        | Action::Back
        | Action::CloseOverlay => {}
    }
    block.into_any_element()
}

/// The picker of the layer an action acts on.
fn destination_field<T: ProtoHost>(
    model: &ProtoModel,
    index: usize,
    action: &Action,
    dest: NodeId,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let choices = model.destinations_for(action);
    let current = choices
        .iter()
        .find(|choice| choice.id == dest)
        .map(|choice| choice.name.clone())
        .unwrap_or_else(|| NOT_SET.to_owned());
    let open = state.borrow().open == Some(ProtoMenu::Destination(index));
    let shared = Rc::clone(state);
    let control = dropdown(
        &current,
        format!("proto-interaction-destination-{index}"),
        open,
    )
    .on_click(cx.listener(move |_host, _event, _window, cx| {
        {
            let mut state = shared.borrow_mut();
            state.open = if state.open == Some(ProtoMenu::Destination(index)) {
                None
            } else {
                Some(ProtoMenu::Destination(index))
            };
            state.draft = None;
        }
        cx.notify();
    }));
    let mut block = div().flex().flex_col().gap_2();
    block = block.child(sublabel(match action {
        Action::Navigate { .. } => "Destination",
        Action::ChangeTo { .. } => "Variant",
        Action::ScrollTo { .. } => "Scroll to layer",
        _ => "Overlay",
    }));
    block = block.child(control);
    if open {
        let ids = model.targets.clone();
        let mut list = menu_box();
        for choice in choices {
            let next = with_dest(action, choice.id);
            list = list.child(menu_item(
                &choice.name,
                &format!(
                    "proto-interaction-destination-option-{index}-{}",
                    choice.id.0
                ),
                ProtoAction::PatchInteraction {
                    ids: ids.clone(),
                    index,
                    patch: json!({ "action": serde_json::to_value(&next).unwrap_or(Value::Null) }),
                },
                cx,
            ));
        }
        block = block.child(list);
    }
    block.into_any_element()
}

/// The action, pointed at another layer.
fn with_dest(action: &Action, dest: NodeId) -> Action {
    match action {
        Action::Navigate {
            preserve_scroll, ..
        } => Action::Navigate {
            dest,
            preserve_scroll: *preserve_scroll,
        },
        Action::SwapOverlay { .. } => Action::SwapOverlay { dest },
        Action::ChangeTo { .. } => Action::ChangeTo { dest },
        Action::OpenOverlay {
            position,
            close_on_outside,
            background,
            offset,
            ..
        } => Action::OpenOverlay {
            dest,
            position: *position,
            close_on_outside: *close_on_outside,
            background: *background,
            offset: *offset,
        },
        Action::ScrollTo { offset, .. } => Action::ScrollTo {
            dest,
            offset: *offset,
        },
        other => other.clone(),
    }
}

/// The overlay action, placed somewhere else.
fn with_overlay_position(action: &Action, position: OverlayPosition) -> Action {
    match action {
        Action::OpenOverlay {
            dest,
            close_on_outside,
            background,
            offset,
            ..
        } => Action::OpenOverlay {
            dest: *dest,
            position,
            close_on_outside: *close_on_outside,
            background: *background,
            offset: *offset,
        },
        other => other.clone(),
    }
}

/// How the layers scroll.
fn scroll_section<T: ProtoHost>(
    model: &ProtoModel,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let mut section = section_header("Scroll behavior", Vec::new(), false, cx);
    if model.scroll.in_frame {
        let current = match &model.scroll.position {
            ProtoValue::Set(position) => SCROLL_POSITIONS
                .iter()
                .find(|(_, candidate)| candidate == position)
                .map(|(label, _)| (*label).to_owned())
                .unwrap_or_else(|| NOT_SET.to_owned()),
            ProtoValue::Unset => NOT_SET.to_owned(),
            ProtoValue::Mixed => MIXED.to_owned(),
        };
        let open = state.borrow().open == Some(ProtoMenu::ScrollPosition);
        let shared = Rc::clone(state);
        let control = dropdown(&current, "proto-scroll-position".to_owned(), open).on_click(
            cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.open = if state.open == Some(ProtoMenu::ScrollPosition) {
                        None
                    } else {
                        Some(ProtoMenu::ScrollPosition)
                    };
                    state.draft = None;
                }
                cx.notify();
            }),
        );
        section = section.child(labeled_row("Position", control));
        if open {
            let ids = model.ids.clone();
            let mut list = menu_box();
            for (label, position) in SCROLL_POSITIONS {
                list = list.child(menu_item(
                    label,
                    &format!("proto-scroll-position-option-{label}"),
                    ProtoAction::Scroll {
                        ids: ids.clone(),
                        prop: ScrollProp::Position,
                        value: ScrollSetting::Position(position),
                    },
                    cx,
                ));
            }
            section = section.child(list);
        }
    }
    if model.scroll.is_frame {
        let current = match &model.scroll.overflow {
            ProtoValue::Set(overflow) => OVERFLOWS
                .iter()
                .find(|(_, candidate)| candidate == overflow)
                .map(|(label, _)| (*label).to_owned())
                .unwrap_or_else(|| NOT_SET.to_owned()),
            ProtoValue::Unset => NOT_SET.to_owned(),
            ProtoValue::Mixed => MIXED.to_owned(),
        };
        let open = state.borrow().open == Some(ProtoMenu::Overflow);
        let shared = Rc::clone(state);
        let control = dropdown(&current, "proto-scroll-overflow".to_owned(), open).on_click(
            cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.open = if state.open == Some(ProtoMenu::Overflow) {
                        None
                    } else {
                        Some(ProtoMenu::Overflow)
                    };
                    state.draft = None;
                }
                cx.notify();
            }),
        );
        section = section.child(labeled_row("Overflow", control));
        if open {
            let ids = model.ids.clone();
            let mut list = menu_box();
            for (label, overflow) in OVERFLOWS {
                list = list.child(menu_item(
                    label,
                    &format!("proto-scroll-overflow-option-{label}"),
                    ProtoAction::Scroll {
                        ids: ids.clone(),
                        prop: ScrollProp::Overflow,
                        value: ScrollSetting::Overflow(overflow),
                    },
                    cx,
                ));
            }
            section = section.child(list);
        }
    }
    section
}

/// The prototype's settings: the device, the colours, the size, the
/// background and whether the frame is resized to the screen.
fn settings_section<T: ProtoHost>(
    model: &ProtoModel,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let settings = model.settings.clone();
    let mut section = section_header(
        "Prototype settings",
        vec![(
            "▣".to_owned(),
            "proto-preview".to_owned(),
            ProtoAction::Preview { start: None },
        )],
        false,
        cx,
    );

    // The device.
    section = section.child(sublabel("Device"));
    let open = state.borrow().open == Some(ProtoMenu::Device);
    let shared = Rc::clone(state);
    let control = dropdown(&settings.device, "proto-device".to_owned(), open).on_click(
        cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = if state.open == Some(ProtoMenu::Device) {
                    None
                } else {
                    Some(ProtoMenu::Device)
                };
                state.draft = None;
            }
            cx.notify();
        }),
    );
    section = section.child(control);
    if open {
        let mut list = menu_box();
        list = list.child(menu_item(
            "None",
            "proto-device-option-None",
            ProtoAction::Settings {
                edit: SettingsEdit {
                    device: Some("None".to_owned()),
                    ..SettingsEdit::default()
                },
            },
            cx,
        ));
        list = list.child(div().h(px(1.0)).bg(rgb(theme::BORDER_SOFT)));
        for device in PrototypeSettings::DEVICES {
            list = list.child(menu_item(
                device.name,
                &format!("proto-device-option-{}", device.name),
                ProtoAction::Settings {
                    edit: SettingsEdit {
                        device: Some(device.name.to_owned()),
                        ..SettingsEdit::default()
                    },
                },
                cx,
            ));
        }
        list = list.child(div().h(px(1.0)).bg(rgb(theme::BORDER_SOFT)));
        list = list.child(menu_item(
            PrototypeSettings::CUSTOM_DEVICE,
            "proto-device-option-custom",
            ProtoAction::Settings {
                edit: SettingsEdit {
                    device: Some(PrototypeSettings::CUSTOM_DEVICE.to_owned()),
                    ..SettingsEdit::default()
                },
            },
            cx,
        ));
        section = section.child(list);
    }

    // The device's colours, and the custom size.
    if let Some(device) = settings.model() {
        if !device.colors.is_empty() {
            section = section.child(sublabel("Color"));
            let open = state.borrow().open == Some(ProtoMenu::DeviceColor);
            let current = settings.device_color();
            let shown = current.map_or(String::new(), |color| color.name.to_owned());
            let shared = Rc::clone(state);
            let control = dropdown(&shown, "proto-device-color".to_owned(), open).on_click(
                cx.listener(move |_host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.open = if state.open == Some(ProtoMenu::DeviceColor) {
                            None
                        } else {
                            Some(ProtoMenu::DeviceColor)
                        };
                        state.draft = None;
                    }
                    cx.notify();
                }),
            );
            section = section.child(control);
            if open {
                let mut list = menu_box();
                for color in device.colors {
                    list = list.child(menu_item(
                        color.name,
                        &format!("proto-device-color-option-{}", color.name),
                        ProtoAction::Settings {
                            edit: SettingsEdit {
                                color: Some(color.name.to_owned()),
                                ..SettingsEdit::default()
                            },
                        },
                        cx,
                    ));
                }
                section = section.child(list);
            }
        }
        if settings.device == PrototypeSettings::CUSTOM_DEVICE {
            section = section.child(sublabel("Size"));
            section = section.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(format!("W {}", device.width)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(format!("H {}", device.height)),
                    ),
            );
        }
    }

    // Whether the presented frame is resized to the screen.
    section = section.child(checkbox_row(
        "Responsive",
        settings.responsive,
        "proto-responsive",
        ProtoAction::Settings {
            edit: SettingsEdit {
                responsive: Some(!settings.responsive),
                ..SettingsEdit::default()
            },
        },
        cx,
    ));

    // The background.
    section = section.child(sublabel("Background"));
    let background = settings.background.to_css_hex();
    let editing = is_editing(state, ProtoField::Background);
    let shown = match state.borrow().draft.as_ref() {
        Some(draft) if draft.field == ProtoField::Background => format!("{}|", draft.text),
        _ => background.clone(),
    };
    let draft = ProtoDraft {
        field: ProtoField::Background,
        text: background,
        select_all: true,
        ids: model.ids.clone(),
        index: None,
        trigger: None,
    };
    let shared = Rc::clone(state);
    section = section.child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(swatch(settings.background))
            .child(
                div()
                    .id("proto-background")
                    .debug_selector(|| "proto-background".to_owned())
                    .flex_1()
                    .text_xs()
                    .text_color(rgb(if editing {
                        theme::TEXT
                    } else {
                        theme::TEXT_SECONDARY
                    }))
                    .border_1()
                    .border_color(rgb(if editing {
                        theme::ACCENT
                    } else {
                        theme::BORDER_SOFT
                    }))
                    .rounded_sm()
                    .px(px(6.0))
                    .py(px(4.0))
                    .cursor_pointer()
                    .child(shown)
                    .on_click(cx.listener(move |_host, _event, _window, cx| {
                        {
                            let mut state = shared.borrow_mut();
                            state.open = None;
                            state.draft = Some(draft.clone());
                        }
                        cx.notify();
                    })),
            ),
    );
    section
}

// ── The chrome ──────────────────────────────────────────────────────────────

/// A section's header: its name, and the actions that belong to the section
/// as a whole.
///
/// `dim` greys the title, as figcraft greys the title of a section that holds
/// nothing yet.
fn section_header<T: ProtoHost>(
    title: &str,
    actions: Vec<(String, String, ProtoAction)>,
    dim: bool,
    cx: &mut Context<T>,
) -> Div {
    let mut header = div().flex().items_center().justify_between().gap_2().child(
        div()
            .text_sm()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgb(if dim {
                theme::TEXT_SECONDARY
            } else {
                theme::TEXT
            }))
            .child(title.to_owned()),
    );
    let mut right = div().flex().items_center().gap_2();
    for (label, selector, action) in actions {
        right = right.child(action_button(&label, selector, action, cx));
    }
    header = header.child(right);
    div().flex().flex_col().gap_2().child(header)
}

/// A small button that runs a change.
fn action_button<T: ProtoHost>(
    label: &str,
    selector: String,
    action: ProtoAction,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .justify_center()
        .px(px(6.0))
        .py(px(2.0))
        .rounded_sm()
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer()
        .child(label.to_owned())
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_proto_action(action.clone(), cx);
        }))
}

/// A control that opens a menu: the value, and a chevron.
fn dropdown(label: &str, selector: String, open: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .w_full()
        .px(px(8.0))
        .py(px(6.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(if open {
            theme::ACCENT
        } else {
            theme::BORDER_SOFT
        }))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .cursor_pointer()
        .child(label.to_owned())
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child("▾"),
        )
}

/// One choice in a menu.
fn menu_item<T: ProtoHost>(
    label: &str,
    selector: &str,
    action: ProtoAction,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    let selector = selector.to_owned();
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .px(px(6.0))
        .py(px(5.0))
        .rounded_sm()
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer()
        .child(label.to_owned())
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_proto_action(action.clone(), cx);
        }))
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

/// A label above a row.
fn sublabel(text: &str) -> Div {
    div()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A row: a name on the left and a control on the right.
fn labeled_row(label: &str, control: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(label.to_owned()),
        )
        .child(control)
}

/// A checkbox with its name.
fn checkbox_row<T: ProtoHost>(
    label: &str,
    value: bool,
    selector: &str,
    action: ProtoAction,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    let selector = selector.to_owned();
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .child(
            div()
                .size(px(12.0))
                .rounded_sm()
                .border_1()
                .border_color(rgb(theme::BORDER))
                .bg(rgb(if value { theme::ACCENT } else { theme::WINDOW }))
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(rgb(theme::WINDOW))
                .child(if value { "✓" } else { "" }),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(label.to_owned()),
        )
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_proto_action(action.clone(), cx);
        }))
}

/// A colour as a swatch.
fn swatch(color: Color) -> Div {
    let [r, g, b, a] = color.to_rgba8();
    let packed = (u32::from(r) << 24) | (u32::from(g) << 16) | (u32::from(b) << 8) | u32::from(a);
    div()
        .size(px(16.0))
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgba(packed))
}

/// A typed field: the value, and a caret while it is being typed.
fn typed_field<T: ProtoHost>(
    value: String,
    selector: String,
    editing: bool,
    draft: ProtoDraft,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> Stateful<Div> {
    let shown = match state.borrow().draft.as_ref() {
        Some(current) if current.field == draft.field => format!("{}|", current.text),
        _ => value,
    };
    let shared = Rc::clone(state);
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex_1()
        .text_xs()
        .text_color(rgb(if editing {
            theme::TEXT
        } else {
            theme::TEXT_SECONDARY
        }))
        .border_1()
        .border_color(rgb(if editing {
            theme::ACCENT
        } else {
            theme::BORDER_SOFT
        }))
        .rounded_sm()
        .px(px(6.0))
        .py(px(4.0))
        .cursor_pointer()
        .child(shown)
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = None;
                state.draft = Some(draft.clone());
            }
            cx.notify();
        }))
}

/// A row with a name and a typed value.
#[allow(clippy::too_many_arguments)]
fn field_row<T: ProtoHost>(
    label: &str,
    value: &str,
    selector: String,
    field: ProtoField,
    text: String,
    ids: Vec<NodeId>,
    index: usize,
    trigger: Option<Trigger>,
    state: &Rc<RefCell<ProtoPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let editing = is_editing(state, field);
    let draft = ProtoDraft {
        field,
        text,
        select_all: true,
        ids,
        index: Some(index),
        trigger,
    };
    labeled_row(
        label,
        typed_field(value.to_owned(), selector, editing, draft, state, cx),
    )
}

/// Whether a field is being typed into.
fn is_editing(state: &Rc<RefCell<ProtoPanelState>>, field: ProtoField) -> bool {
    state
        .borrow()
        .draft
        .as_ref()
        .is_some_and(|draft| draft.field == field)
}

/// A line of text under a control.
fn note(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A rule between two parts of the panel.
fn separator() -> Div {
    div().h(px(1.0)).bg(rgb(theme::BORDER_SOFT))
}

// ═══════════════════════════════════════════════════════════════════════════
// The prototype mode: the connections on the canvas
// ═══════════════════════════════════════════════════════════════════════════

/// Radius of the "+" handle a selected layer shows, in logical pixels.
const HANDLE_R: f32 = 7.0;

/// One drawn connection: a layer, and the frame it acts on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProtoConnection {
    /// The layer the connection starts at.
    pub source: NodeId,
    /// Which of the layer's interactions the connection is.
    pub index: usize,
    /// The frame the connection ends at.
    pub dest: NodeId,
    /// The curve's four control points, in window pixels.
    pub points: [GPoint<f32>; 4],
    /// The destination's bounds, in window pixels.
    pub dest_bounds: Bounds<Pixels>,
}

/// The connections the page's layers hold.
///
/// The layers with interactions, in tree order, and for each of them the
/// interactions that point at another layer on the same page. A connection to
/// another page is not drawn: it has no place to point at.
pub fn proto_connections(
    doc: &Document,
    page: NodeId,
    view: &ViewState,
    viewport: Viewport,
) -> Vec<ProtoConnection> {
    let mut out = Vec::new();
    for id in doc.subtree(page) {
        let Some(node) = doc.node(id) else {
            continue;
        };
        if node.interactions.is_empty() {
            continue;
        }
        let Some(from) = doc.world_bounds(id) else {
            continue;
        };
        for (index, interaction) in node.interactions.iter().enumerate() {
            let Some(dest) =
                figcraft_doc::proto::walk(&interaction.actions).find_map(|action| action.dest())
            else {
                continue;
            };
            if doc.page_of(dest) != Some(page) {
                continue;
            }
            let Some(to) = doc.world_bounds(dest) else {
                continue;
            };
            out.push(ProtoConnection {
                source: id,
                index,
                dest,
                points: curve_between(from, to, view, viewport),
                dest_bounds: screen_bounds(to, view, viewport),
            });
        }
    }
    out
}

/// The curve from `a` to `b`: it leaves the side facing the destination.
///
/// figcraft's shape, in page space: a cubic whose two control points sit
/// halfway to the destination and no closer than 40 points.
fn curve_between(
    a: figcraft_geom::Rect,
    b: figcraft_geom::Rect,
    view: &ViewState,
    viewport: Viewport,
) -> [GPoint<f32>; 4] {
    let right = (b.x0 + b.x1) / 2.0 >= (a.x0 + a.x1) / 2.0;
    let (p0, p3) = if right {
        (
            GPoint {
                x: a.x1,
                y: (a.y0 + a.y1) / 2.0,
            },
            GPoint {
                x: b.x0,
                y: (b.y0 + b.y1) / 2.0,
            },
        )
    } else {
        (
            GPoint {
                x: a.x0,
                y: (a.y0 + a.y1) / 2.0,
            },
            GPoint {
                x: b.x1,
                y: (b.y0 + b.y1) / 2.0,
            },
        )
    };
    let s = if right { 1.0 } else { -1.0 };
    let dx = ((p3.x - p0.x).abs() * 0.5).max(40.0) * s;
    [
        page_to_screen(view, viewport, p0.x, p0.y),
        page_to_screen(view, viewport, p0.x + dx, p0.y),
        page_to_screen(view, viewport, p3.x - dx, p3.y),
        page_to_screen(view, viewport, p3.x, p3.y),
    ]
}

/// A page rect in window pixels.
fn screen_bounds(
    rect: figcraft_geom::Rect,
    view: &ViewState,
    viewport: Viewport,
) -> Bounds<Pixels> {
    let a = page_to_screen(view, viewport, rect.x0, rect.y0);
    let b = page_to_screen(view, viewport, rect.x1, rect.y1);
    Bounds::new(
        gpui::point(px(a.x.min(b.x)), px(a.y.min(b.y))),
        gpui::size(px((a.x - b.x).abs()), px((a.y - b.y).abs())),
    )
}

/// A page point in window pixels.
fn page_to_screen(view: &ViewState, viewport: Viewport, x: f64, y: f64) -> GPoint<f32> {
    crate::engine::canvas::page_to_screen(view, viewport, x, y)
}

/// The connection a pointer at `at` is over, if any.
///
/// The nearest one within five points, so a click on a crossing picks the
/// closest line rather than the first one drawn.
pub fn connection_at(connections: &[ProtoConnection], at: GPoint<f32>) -> Option<(NodeId, usize)> {
    connections
        .iter()
        .map(|connection| (connection, bezier_distance(&connection.points, at)))
        .filter(|(_, distance)| *distance < 5.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(connection, _)| (connection.source, connection.index))
}

/// The distance from `q` to a cubic, sampled.
fn bezier_distance(points: &[GPoint<f32>; 4], q: GPoint<f32>) -> f32 {
    let mut best = f32::INFINITY;
    let mut previous = points[0];
    for step in 1..=32 {
        let current = bezier(points, step as f32 / 32.0);
        best = best.min(segment_distance(previous, current, q));
        previous = current;
    }
    best
}

/// A point on a cubic.
fn bezier(points: &[GPoint<f32>; 4], t: f32) -> GPoint<f32> {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    GPoint {
        x: w[0] * points[0].x + w[1] * points[1].x + w[2] * points[2].x + w[3] * points[3].x,
        y: w[0] * points[0].y + w[1] * points[1].y + w[2] * points[2].y + w[3] * points[3].y,
    }
}

/// The distance from `q` to the segment `a`-`b`.
fn segment_distance(a: GPoint<f32>, b: GPoint<f32>, q: GPoint<f32>) -> f32 {
    let ab = GPoint {
        x: b.x - a.x,
        y: b.y - a.y,
    };
    let length = ab.x * ab.x + ab.y * ab.y;
    let t = if length > 0.0 {
        (((q.x - a.x) * ab.x + (q.y - a.y) * ab.y) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let dx = a.x + ab.x * t - q.x;
    let dy = a.y + ab.y * t - q.y;
    (dx * dx + dy * dy).sqrt()
}

/// Draw the prototype's connections, and the handles a selected layer shows.
///
/// The canvas's hook: it is called when the Prototype tab is open, and it
/// paints the noodles from the layers that have interactions to the frames
/// they act on. The connections of the selected layers are drawn in the accent
/// colour and the others are drawn faintly, so a selection reads at a glance.
///
/// Returns how many connections were drawn, so a caller can tell an empty
/// prototype from one that was not drawn.
pub fn paint_proto_connections(
    window: &mut Window,
    view: &ViewState,
    viewport: Viewport,
    doc: &Document,
    page: NodeId,
    selection: &[NodeId],
    palette: &Palette,
) -> usize {
    let connections = proto_connections(doc, page, view, viewport);
    for connection in &connections {
        let selected = selection.contains(&connection.source);
        let colour = if selected {
            palette.accent
        } else {
            palette.accent.alpha(0.55)
        };
        let width = if selected { 2.0 } else { 1.5 };
        // The destination is outlined, so the frame the connection acts on
        // reads as the target.
        if selected {
            stroke_rect(
                window,
                connection.dest_bounds,
                1.0,
                palette.accent.alpha(0.7),
            );
        }
        cubic(window, &connection.points, width, colour);
        // The dot the connection starts at.
        circle(
            window,
            connection.points[0],
            3.5,
            colour,
            Some(palette.white),
        );
        arrow(window, &connection.points, colour);
    }
    // The "+" handles: the way a new connection is dragged out.
    for centre in handles(doc, view, viewport, selection) {
        plus_handle(window, centre, palette);
    }
    connections.len()
}

/// The centres of the selected layers' handles: the right edge of each,
/// vertically centred.
fn handles(
    doc: &Document,
    view: &ViewState,
    viewport: Viewport,
    selection: &[NodeId],
) -> Vec<GPoint<f32>> {
    selection
        .iter()
        .take(32)
        .filter(|id| doc.node(**id).is_some_and(|node| !node.is_page()))
        .filter_map(|id| {
            let bounds = doc.world_bounds(*id)?;
            let centre = page_to_screen(view, viewport, bounds.x1, (bounds.y0 + bounds.y1) / 2.0);
            Some(GPoint {
                x: centre.x + HANDLE_R + 4.0,
                y: centre.y,
            })
        })
        .collect()
}

/// A cubic curve through four window points, stroked.
fn cubic(window: &mut Window, points: &[GPoint<f32>; 4], width: f32, colour: Hsla) {
    let mut builder = PathBuilder::stroke(px(width));
    builder.move_to(px_point(points[0]));
    builder.cubic_bezier_to(
        px_point(points[3]),
        px_point(points[1]),
        px_point(points[2]),
    );
    if let Ok(built) = builder.build() {
        window.paint_path(built, colour);
    }
}

/// The arrow head at the end of a connection.
fn arrow(window: &mut Window, points: &[GPoint<f32>; 4], colour: Hsla) {
    let tip = points[3];
    let before = bezier(points, 0.9);
    let mut direction = GPoint {
        x: tip.x - before.x,
        y: tip.y - before.y,
    };
    let length = (direction.x * direction.x + direction.y * direction.y).sqrt();
    if !length.is_finite() || length <= 0.5 {
        direction = GPoint { x: 1.0, y: 0.0 };
    } else {
        direction.x /= length;
        direction.y /= length;
    }
    let normal = GPoint {
        x: -direction.y,
        y: direction.x,
    };
    let a = GPoint {
        x: tip.x - direction.x * 8.0 + normal.x * 4.5,
        y: tip.y - direction.y * 8.0 + normal.y * 4.5,
    };
    let b = GPoint {
        x: tip.x - direction.x * 8.0 - normal.x * 4.5,
        y: tip.y - direction.y * 8.0 - normal.y * 4.5,
    };
    let mut builder = PathBuilder::fill();
    builder.add_polygon(
        &[
            gpui::point(px(tip.x), px(tip.y)),
            gpui::point(px(a.x), px(a.y)),
            gpui::point(px(b.x), px(b.y)),
        ],
        true,
    );
    if let Ok(built) = builder.build() {
        window.paint_path(built, colour);
    }
}

/// A circle, filled, with a ring of another colour.
fn circle(window: &mut Window, at: GPoint<f32>, radius: f32, colour: Hsla, ring: Option<Hsla>) {
    let mut builder = PathBuilder::fill();
    circle_path(&mut builder, px(at.x), px(at.y), px(radius));
    if let Ok(built) = builder.build() {
        window.paint_path(built, colour);
    }
    if let Some(ring) = ring {
        let mut builder = PathBuilder::stroke(px(1.0));
        circle_path(&mut builder, px(at.x), px(at.y), px(radius));
        if let Ok(built) = builder.build() {
            window.paint_path(built, ring);
        }
    }
}

/// A full circle as two half-arcs, the only circle GPUI's path builder can
/// express.
fn circle_path(builder: &mut PathBuilder, x: Pixels, y: Pixels, r: Pixels) {
    builder.move_to(gpui::point(x + r, y));
    builder.arc_to(
        gpui::point(r, r),
        px(0.0),
        false,
        true,
        gpui::point(x - r, y),
    );
    builder.arc_to(
        gpui::point(r, r),
        px(0.0),
        false,
        true,
        gpui::point(x + r, y),
    );
    builder.close();
}

/// The "+" handle a selected layer shows.
fn plus_handle(window: &mut Window, centre: GPoint<f32>, palette: &Palette) {
    let mut builder = PathBuilder::fill();
    circle_path(&mut builder, px(centre.x), px(centre.y), px(HANDLE_R));
    if let Ok(built) = builder.build() {
        window.paint_path(built, palette.accent);
    }
    let mut builder = PathBuilder::stroke(px(1.5));
    circle_path(&mut builder, px(centre.x), px(centre.y), px(HANDLE_R));
    if let Ok(built) = builder.build() {
        window.paint_path(built, palette.white);
    }
    let mut builder = PathBuilder::stroke(px(1.5));
    builder.move_to(gpui::point(px(centre.x - 3.5), px(centre.y)));
    builder.line_to(gpui::point(px(centre.x + 3.5), px(centre.y)));
    builder.move_to(gpui::point(px(centre.x), px(centre.y - 3.5)));
    builder.line_to(gpui::point(px(centre.x), px(centre.y + 3.5)));
    if let Ok(built) = builder.build() {
        window.paint_path(built, palette.white);
    }
}

/// A rectangle's outline.
fn stroke_rect(window: &mut Window, bounds: Bounds<Pixels>, width: f32, colour: Hsla) {
    if bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
        return;
    }
    let mut builder = PathBuilder::stroke(px(width));
    let a = bounds.origin;
    let b = bounds.bottom_right();
    builder.move_to(gpui::point(a.x, a.y));
    builder.line_to(gpui::point(b.x, a.y));
    builder.line_to(gpui::point(b.x, b.y));
    builder.line_to(gpui::point(a.x, b.y));
    builder.close();
    if let Ok(built) = builder.build() {
        window.paint_path(built, colour);
    }
}

/// A window point as a GPUI point.
fn px_point(at: GPoint<f32>) -> GPoint<Pixels> {
    gpui::point(px(at.x), px(at.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    use figcraft_doc::NodeKind;
    use figcraft_engine::Session;
    use gpui::{point, size, IntoElement, Render, TestAppContext, Window};

    // ── The fixture ──────────────────────────────────────────────────────────

    /// A document with two frames and a button in the first.
    ///
    /// The engine's own commands make the prototype, so the tests are looking
    /// at a document an editor would really hold rather than one assembled by
    /// hand.
    struct Fixture {
        session: Session,
        page: NodeId,
        first: NodeId,
        second: NodeId,
        button: NodeId,
    }

    fn fixture() -> Fixture {
        let mut doc = Document::new("Project");
        let page = doc.pages[0];
        let first = doc
            .create(page, NodeKind::Frame, "First", 0.0, 0.0, 320.0, 200.0)
            .expect("a frame");
        let second = doc
            .create(page, NodeKind::Frame, "Second", 400.0, 0.0, 320.0, 200.0)
            .expect("a frame");
        let button = doc
            .create(first, NodeKind::Rectangle, "Button", 10.0, 10.0, 80.0, 30.0)
            .expect("a rectangle");
        Fixture {
            session: Session::with_document(doc),
            page,
            first,
            second,
            button,
        }
    }

    /// The id a command made, as the engine's own node id.
    fn created_id(result: &Value) -> NodeId {
        NodeId(result["id"].as_u64().expect("the command made a node"))
    }

    /// A debug selector that lives as long as the test.
    fn selector(text: String) -> &'static str {
        Box::leak(text.into_boxed_str()) as &'static str
    }

    impl Fixture {
        /// The button navigates to the second frame when it is clicked.
        fn with_navigation(&mut self) -> &mut Self {
            self.session
                .execute(
                    "proto.interaction.add",
                    &json!({
                        "ids": [self.button],
                        "trigger": { "type": "onClick" },
                        "action": { "type": "navigate", "dest": self.second },
                    }),
                )
                .expect("the button gets an interaction");
            self
        }

        /// The first frame starts a flow.
        fn with_flow(&mut self) -> &mut Self {
            self.session
                .execute("proto.flow.add", &json!({ "start": self.first }))
                .expect("the first frame starts a flow");
            self
        }

        fn doc(&self) -> Document {
            self.session.doc().expect("a document").clone()
        }
    }

    // ── Reading the engine's model ──────────────────────────────────────────

    #[test]
    fn the_model_reads_the_engines_interactions_and_flows() {
        let mut fixture = fixture();
        fixture.with_navigation().with_flow();
        let doc = fixture.doc();

        // The selected layer's interaction, as the panel shows it.
        let model = ProtoModel::read(&doc, fixture.page, &[fixture.button]);
        assert_eq!(
            model.count,
            ProtoValue::Set(1),
            "the layer holds one interaction"
        );
        assert_eq!(model.interactions.len(), 1);
        assert_eq!(
            model.interactions[0].trigger,
            ProtoValue::Set("On click".to_owned()),
            "the trigger is the engine's own, in figcraft's words"
        );
        assert_eq!(
            model.interactions[0].summary,
            ProtoValue::Set("Navigate to Second".to_owned()),
            "and the action names the frame it goes to"
        );

        // The page's flows, which the empty state shows.
        let empty = ProtoModel::read(&doc, fixture.page, &[]);
        assert!(!empty.has_selection, "nothing is selected");
        assert_eq!(empty.flows.len(), 1, "the page has one flow");
        assert_eq!(empty.flows[0].name, "Flow 1", "the engine names it");
        assert_eq!(
            empty.flows[0].start, fixture.first,
            "it starts at the first frame"
        );
        assert!(
            empty.interactions.is_empty(),
            "and no layer's own properties"
        );
    }

    #[test]
    fn a_layer_with_no_interaction_shows_the_empty_state_for_its_list() {
        let fixture = fixture();
        let doc = fixture.doc();
        let model = ProtoModel::read(&doc, fixture.page, &[fixture.button]);
        assert_eq!(model.count, ProtoValue::Set(0));
        assert!(
            model.interactions.is_empty(),
            "a layer with no interactions has no rows"
        );
        // And the page's flows are still there: the empty state is a
        // different panel, not a panel with no rows.
        assert!(model.flows.is_empty(), "the page has no flows yet");
        assert_eq!(
            model.free_frame,
            Some(fixture.first),
            "so a frame is free to start one"
        );
    }

    #[test]
    fn a_selection_that_disagrees_is_mixed_per_property() {
        let mut fixture = fixture();
        fixture.with_navigation();
        // A second button, with a different trigger.
        let other = created_id(
            &fixture
                .session
                .execute(
                    "node.create",
                    &json!({
                        "type": "rectangle",
                        "parent": fixture.first,
                        "x": 10.0,
                        "y": 50.0,
                        "width": 80.0,
                        "height": 30.0,
                        "name": "Other",
                    }),
                )
                .expect("a rectangle"),
        );
        fixture
            .session
            .execute(
                "proto.interaction.add",
                &json!({
                    "ids": [other],
                    "trigger": { "type": "onHover" },
                    "action": { "type": "navigate", "dest": fixture.second },
                }),
            )
            .expect("the second button gets an interaction");
        let doc = fixture.doc();

        let model = ProtoModel::read(&doc, fixture.page, &[fixture.button, other]);
        assert_eq!(
            model.count,
            ProtoValue::Set(1),
            "both layers hold one interaction, so there is one row"
        );
        assert_eq!(
            model.interactions[0].trigger,
            ProtoValue::Mixed,
            "and they disagree about the trigger: {}",
            model.interactions[0].trigger.word()
        );
        assert_eq!(
            model.interactions[0].summary,
            ProtoValue::Set("Navigate to Second".to_owned()),
            "but they agree about what it does, so that row is not mixed"
        );
        assert_eq!(model.interactions[0].trigger.word(), MIXED);
    }

    #[test]
    fn a_selection_with_a_different_number_of_interactions_is_mixed() {
        let mut fixture = fixture();
        fixture.with_navigation();
        let doc = fixture.doc();
        // The frame holds none, the button holds one: there is no one list.
        let model = ProtoModel::read(&doc, fixture.page, &[fixture.first, fixture.button]);
        assert!(
            model.count.is_mixed(),
            "two layers with a different number of interactions have no one list"
        );
        assert!(model.interactions.is_empty(), "and so no rows are shown");
    }

    // ── The commands ─────────────────────────────────────────────────────────

    #[test]
    fn a_change_carries_the_engines_exact_command_id_and_params() {
        let (command, params) = ProtoAction::AddInteraction {
            ids: vec![NodeId(7)],
            trigger: Trigger::OnClick,
            action: Action::Navigate {
                dest: NodeId(9),
                preserve_scroll: false,
            },
        }
        .command();
        assert_eq!(command, "proto.interaction.add");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["trigger"], json!({ "type": "onClick" }));
        assert_eq!(params["action"], json!({ "type": "navigate", "dest": 9 }));

        let (command, params) = ProtoAction::AddFlow { start: NodeId(3) }.command();
        assert_eq!(command, "proto.flow.add");
        assert_eq!(params["start"], json!(3));

        let (command, params) = ProtoAction::RenameFlow {
            start: NodeId(3),
            name: Some("Checkout".to_owned()),
            description: None,
        }
        .command();
        assert_eq!(command, "proto.flow.rename");
        assert_eq!(params["start"], json!(3));
        assert_eq!(params["name"], json!("Checkout"));
        assert!(
            params.get("description").is_none(),
            "a rename that does not change the description does not send one"
        );

        let (command, params) = ProtoAction::RemoveFlow { start: NodeId(3) }.command();
        assert_eq!(command, "proto.flow.remove");
        assert_eq!(params["start"], json!(3));

        let (command, params) = ProtoAction::RemoveInteraction {
            ids: vec![NodeId(7)],
            index: 2,
        }
        .command();
        assert_eq!(command, "proto.interaction.remove");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["index"], json!(2));

        let (command, params) = ProtoAction::ClearInteractions {
            ids: vec![NodeId(7)],
        }
        .command();
        assert_eq!(command, "proto.interaction.clear");
        assert_eq!(params["ids"], json!([7]));
    }

    #[test]
    fn replacing_the_interactions_is_one_command_and_one_undo_step() {
        let (command, params) = ProtoAction::ReplaceInteractions {
            ids: vec![NodeId(7)],
            trigger: Trigger::OnClick,
            action: Action::Back,
        }
        .command();
        assert_eq!(command, "command.batch");
        assert_eq!(params["label"], json!("Replace interactions"));
        let steps = params["commands"].as_array().expect("the steps");
        assert_eq!(steps.len(), 2, "the old ones go, and one new one comes");
        assert_eq!(steps[0]["command"], json!("proto.interaction.clear"));
        assert_eq!(steps[0]["params"]["ids"], json!([7]));
        assert_eq!(steps[1]["command"], json!("proto.interaction.add"));
        assert_eq!(steps[1]["params"]["ids"], json!([7]));
        assert_eq!(steps[1]["params"]["trigger"], json!({ "type": "onClick" }));
        assert_eq!(steps[1]["params"]["action"], json!({ "type": "back" }));
    }

    #[test]
    fn a_change_to_an_interaction_uses_the_engines_own_field_names() {
        let (command, params) = ProtoAction::PatchInteraction {
            ids: vec![NodeId(7)],
            index: 0,
            patch: json!({ "transition": { "type": "moveIn", "direction": "top" } }),
        }
        .command();
        assert_eq!(command, "proto.interaction.set");
        assert_eq!(params["index"], json!(0));
        assert_eq!(
            params["transition"],
            json!({ "type": "moveIn", "direction": "top" }),
            "the transition is the engine's own, with its own field names"
        );
    }

    #[test]
    fn the_scroll_change_is_a_node_set_with_the_engines_own_spelling() {
        let (command, params) = ProtoAction::Scroll {
            ids: vec![NodeId(7)],
            prop: ScrollProp::Position,
            value: ScrollSetting::Position(ScrollPosition::Fixed),
        }
        .command();
        assert_eq!(command, "node.set");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["props"]["scrollPosition"], json!("fixed"));

        let (command, params) = ProtoAction::Scroll {
            ids: vec![NodeId(7)],
            prop: ScrollProp::Overflow,
            value: ScrollSetting::Overflow(Overflow::Vertical),
        }
        .command();
        assert_eq!(command, "node.set");
        assert_eq!(params["props"]["overflow"], json!("vertical"));
    }

    #[test]
    fn the_settings_change_names_only_what_it_changes() {
        let (command, params) = ProtoAction::Settings {
            edit: SettingsEdit {
                device: Some("iPhone 16".to_owned()),
                ..SettingsEdit::default()
            },
        }
        .command();
        assert_eq!(command, "proto.settings");
        assert_eq!(params["device"], json!("iPhone 16"));
        assert_eq!(
            params.as_object().map(|object| object.len()),
            Some(1),
            "only the device is sent, so the rest of the settings are left alone"
        );
    }

    #[test]
    fn a_new_interaction_goes_to_the_next_frame() {
        let fixture = fixture();
        let doc = fixture.doc();
        let model = ProtoModel::read(&doc, fixture.page, &[fixture.button]);
        match model.default_action() {
            Action::Navigate { dest, .. } => assert_eq!(
                dest, fixture.second,
                "the button navigates to the other frame, not to its own"
            ),
            other => panic!("a click should navigate: {other:?}"),
        }
    }

    // ── The prototype mode: the connections on the canvas ───────────────────

    #[test]
    fn the_connections_are_the_layers_that_act_on_another_frame() {
        let mut fixture = fixture();
        fixture.with_navigation();
        let doc = fixture.doc();
        let view = ViewState::default();
        let viewport = Viewport::empty();

        let connections = proto_connections(&doc, fixture.page, &view, viewport);
        assert_eq!(
            connections.len(),
            1,
            "the button has one interaction, and it points at the second frame"
        );
        assert_eq!(connections[0].source, fixture.button);
        assert_eq!(connections[0].index, 0);
        assert_eq!(connections[0].dest, fixture.second);
        // The curve leaves the source on the side facing the destination, and
        // ends at the destination's facing side.
        assert!(
            connections[0].points[0].x < connections[0].points[3].x,
            "the second frame is to the right, so the curve leaves on the right: {:?}",
            connections[0].points
        );
    }

    #[test]
    fn a_pointer_over_a_connection_finds_it() {
        let mut fixture = fixture();
        fixture.with_navigation();
        let doc = fixture.doc();
        let view = ViewState::default();
        let viewport = Viewport::empty();
        let connections = proto_connections(&doc, fixture.page, &view, viewport);
        // A point on the curve, not one of its control points: only the curve
        // itself is what a pointer can be over.
        let middle = bezier(&connections[0].points, 0.5);
        assert_eq!(
            connection_at(&connections, middle),
            Some((fixture.button, 0)),
            "a pointer over the curve finds the connection"
        );
        assert_eq!(
            connection_at(
                &connections,
                GPoint {
                    x: -100.0,
                    y: -100.0
                }
            ),
            None,
            "and a pointer far away finds nothing"
        );
    }

    // ── The panel, in a real window ─────────────────────────────────────────

    /// A surface that holds the panel and remembers what it asked for.
    struct TestHost {
        model: ProtoModel,
        state: Rc<RefCell<ProtoPanelState>>,
        actions: Vec<ProtoAction>,
    }

    impl TestHost {
        fn new(model: ProtoModel) -> Self {
            Self {
                model,
                state: Rc::new(RefCell::new(ProtoPanelState::new())),
                actions: Vec::new(),
            }
        }
    }

    impl ProtoHost for TestHost {
        fn run_proto_action(&mut self, action: ProtoAction, _cx: &mut Context<Self>) {
            self.actions.push(action);
        }
    }

    impl Render for TestHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            proto_panel(&self.model, &self.state, cx)
        }
    }

    /// The panel, on its own, in a real window.
    ///
    /// The panel is renderable on its own, so it can be put in a window of its
    /// own: a test that puts it in the Inspector would be measuring the
    /// Inspector's layout as much as the panel's.
    #[gpui::test]
    fn the_prototyping_panel_lays_out_in_a_real_window(cx: &mut TestAppContext) {
        let mut fixture = fixture();
        fixture.with_navigation().with_flow();
        let doc = fixture.doc();
        let model = ProtoModel::read(&doc, fixture.page, &[fixture.button]);

        let (host, window) = cx.add_window_view(|_window, _cx| TestHost::new(model));
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(720.0)),
            |_, _| host.clone().into_any_element(),
        );
        window.run_until_parked();

        let panel = window
            .debug_bounds("proto-panel")
            .expect("the panel is laid out");
        assert!(
            panel.size.height > px(0.0),
            "the panel has no height, so nothing can be drawn in it: {panel:?}"
        );
        assert!(
            panel.size.width > px(0.0),
            "the panel has no width: {panel:?}"
        );

        // The layer's interaction is one of the rows the panel draws.
        let row = window
            .debug_bounds("proto-interaction-0")
            .expect("the interaction is drawn as a row");
        assert!(
            row.size.height > px(0.0),
            "the row is drawn with no height: {row:?}"
        );
        assert!(
            row.origin.y >= panel.origin.y && row.bottom() <= panel.bottom(),
            "the row is drawn inside the panel: row {row:?} panel {panel:?}"
        );
    }

    #[gpui::test]
    fn the_prototyping_panel_shows_the_empty_state_when_nothing_is_selected(
        cx: &mut TestAppContext,
    ) {
        let mut fixture = fixture();
        fixture.with_flow();
        let doc = fixture.doc();
        let model = ProtoModel::read(&doc, fixture.page, &[]);

        let (host, window) = cx.add_window_view(|_window, _cx| TestHost::new(model));
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(720.0)),
            |_, _| host.clone().into_any_element(),
        );
        window.run_until_parked();

        let panel = window
            .debug_bounds("proto-panel")
            .expect("the panel is laid out");
        assert!(panel.size.height > px(0.0), "the panel has no height");
        // The page's flow is listed, and the prototype's settings are there.
        assert!(
            window
                .debug_bounds(selector(format!("proto-flow-name-{}", fixture.first)))
                .is_some(),
            "the page's flow is drawn"
        );
        assert!(
            window.debug_bounds("proto-device").is_some(),
            "the prototype's settings are drawn"
        );
        // And no layer's interactions, because no layer is selected.
        assert!(
            window.debug_bounds("proto-interaction-0").is_none(),
            "the empty state has no interaction rows"
        );
    }
}
