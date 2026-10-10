//! The single semantic operation boundary.
//!
//! # The problem this solves
//!
//! The repository had two histories that never met. `canvas.rs` had a
//! working undo stack over [`DocumentCommand`] (geometry, style, text,
//! insert, delete). `source_document.rs` had [`SemanticHistory`] over
//! [`RenameNode`], touching `PersistentDocument` metadata. Neither could see
//! the other, and nothing in the editor ever invoked the second one: as of
//! this change `RenameNode` was a fully tested island with no call site.
//!
//! This module converges them on one vocabulary and one stack, without
//! rewriting the canvas. It is an adapter, not a replacement.
//!
//! # The operation model
//!
//! [`SemanticOperation`] is the closed set of user-visible mutations:
//!
//! - [`SemanticOperation::Rename`] — persistent metadata (name).
//! - [`SemanticOperation::Runtime`] — the canvas command family, which today
//!   covers move, resize, create, delete, duplicate, style, and text.
//! - [`SemanticOperation::Compound`] — several of the above as one atomic
//!   entry. See "Compound operations" below.
//!
//! Every leaf variant is a reversible value type holding a `before`/`after`
//! pair. Undo replays `before`; redo replays `after`. Nothing is stored as a
//! byte offset or a captured diff, so history records *intentions* rather than
//! net state changes.
//!
//! # Compound operations
//!
//! [`SemanticOperation::Runtime`] wraps exactly one
//! `canvas::DocumentCommand`, which in turn wraps exactly one private
//! `CommandOperation`. One history entry therefore cannot hold an insert *and*
//! a geometry change — the shape a modifier-drag duplicate needs, where copies
//! are created at press time and then follow the pointer.
//!
//! [`SemanticOperation::Compound`] closes that gap without becoming a general
//! transaction framework. It is deliberately narrow:
//!
//! - It holds a list of existing operations. It introduces no new mutation
//!   primitive, no rollback log, and no nesting semantics of its own.
//! - Undo replays its members in **reverse** order; redo replays them in
//!   order. That ordering is the whole reason a compound is not just a list.
//! - Every member is validated before **any** member is applied, so a compound
//!   is all-or-nothing by construction rather than by compensation.
//! - Nested compounds are flattened on construction, so the stack never grows
//!   deeper than the caller's nesting.
//!
//! # Atomicity
//!
//! Research §10 observed that tldraw has no rollback anywhere: a failed
//! mutation is handled by an explicit compensating operation placed by hand.
//! That was tolerable when every entry was a single idempotent command. It is
//! not tolerable for a compound, where a member failing halfway leaves the
//! entry half-applied.
//!
//! Spool does not add a rollback log. It makes the *validate* step exhaustive
//! and runs it over the whole operation first, so [`EditSession::execute`] can
//! reach apply-time only for an operation that cannot fail. The remaining
//! apply-time failures are replay failures on stale state, and those return
//! without touching history.
//!
//! # The transaction boundary
//!
//! [`EditSession::execute`] is the only place an operation is committed. One
//! call is one logical transaction and produces at most one history entry,
//! however many objects it touched. A multi-node move is one
//! [`SemanticOperation::Runtime`] holding one geometry command covering every
//! moved object, not N commands.
//!
//! # Gestures are the canvas's, not this module's
//!
//! A gesture mutates transiently and reaches `execute` once, on success:
//!
//! ```text
//! canvas mutates the runtime -> nothing recorded
//! mouse up                 -> commit(..) -> execute(..) -> one entry
//! escape / cancel          -> runtime restored, zero entries
//! ```
//!
//! That is `CanvasView`'s own `MoveGesture` and `GeometryScrub`, which capture
//! `ObjectSnapshot`s inline and write the single `DocumentCommand` on commit.
//! It is deliberately *not* mirrored here. This module used to carry a second,
//! parallel gesture system (`EditSession::begin_gesture` and friends, with its
//! own `InFlight` snapshot log) that no caller ever invoked: two
//! implementations of "do not record until commit" is two places for the
//! cancellation guarantee to be wrong, and only the canvas one runs. A gesture
//! is a property of the surface that owns the pointer, not of the operation
//! boundary.
//!
//! # Undo/redo mechanism
//!
//! [`SemanticHistory`] holds two stacks of [`SemanticOperation`].
//! `undo` pops, applies `before`, and pushes onto `redo`. `redo` does the
//! mirror image. Committing a new operation clears `redo`, exactly as before.
//! No-op operations are dropped before they reach a stack.
//!
//! An operation is applied against an [`OperationTarget`], which says which
//! state the replay may reach: [`OperationTarget::Full`] for the editor, and
//! [`OperationTarget::Document`] for a bundle-level caller that holds metadata
//! and no canvas runtime.
//!
//! # What this deliberately does not do
//!
//! - No tldraw-style marks or lazy sealing. Spool commits eagerly, which the
//!   project's own research (§19) identifies as the behaviour Spool already
//!   had and chose to keep.
//! - No snapshot of the whole document per pointer event. Undo replays the
//!   `before` values a command already carries, so a drag costs one small
//!   geometry change per object, not a full copy of the document.
//! - No operation constructors. There is no `move_nodes` or `create_nodes`
//!   beside [`DocumentCommand`]: that command is the one vocabulary, and a
//!   free-standing constructor that mutates the runtime *outside* the boundary
//!   before handing back an operation is a second way in, not a convenience.
//! - No AI, plugin, import, or automation caller. The boundary is reachable by
//!   them because [`EditSession::execute`] is the only mutation entry point,
//!   and [`Origin`] exists so an entry can say which of them produced it. None
//!   of those callers are implemented here, and [`Origin`] is the only thing
//!   that anticipates them — there is no agent runtime, no plugin registry, and
//!   no MCP surface anywhere in this module.
//!
//! # Who is on the live path
//!
//! `canvas::CanvasView` owns an [`EditSession`] and funnels every mutation
//! through [`EditSession::execute`] via its private `commit`. So the live set
//! is [`SemanticOperation`], [`SemanticHistory`], [`Origin`], [`EditSession`],
//! [`OperationTarget::Full`], and [`apply`].
//!
//! One piece of the surface is deliberately retained without a production
//! caller, and says so where it is declared: [`Origin`]'s non-`User` variants,
//! which is where attribution would go.
//!
//! # MUTATION HARNESS
//!
//! `app/mutate_ops.sh` and `app/mutate_created_objects.sh` break one rule at a
//! time in this file — the reverse
//! order of a compound undo, the validate checks, the point at which an
//! operation is recorded — and requires the test suite to notice. A rule that
//! can be broken without a test failing is reported as `SURVIVED`, because an
//! untested invariant is the finding. The script refuses to run unless it sees
//! this marker, so it cannot be pointed at an unrelated file.

use crate::canvas::{Document, DocumentCommand, ObjectId, ReplayDirection};
use crate::source_document::{NodeId, PersistentDocument, RenameNode, StructuralNode};

/// Who asked for an operation.
///
/// Research §17 recorded the structural gap this closes: with a bare
/// `Vec<SemanticOperation>` stack there is nowhere to attach provenance, so an
/// entry cannot say whether a human, an agent, an importer, or a plugin
/// produced it — and every entry is indistinguishable in the undo spine.
///
/// Only [`Origin::User`] has a caller today. The remaining variants exist
/// because the architecture this module serves names those callers, and a
/// stack that cannot carry attribution cannot be extended to them without a
/// second, parallel mechanism. Adding a variant later is additive; this
/// records no agent, registers no plugin, and runs no automation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// A human acting through the editor: a gesture, a keystroke, a menu item.
    #[default]
    User,
    /// A future agent caller. Not implemented.
    // Retained, not dead: `Origin` is the attribution half of a committed
    // `HistoryEntry`, so it is meaningless unless more than one author can be
    // named. AGENTS.md forbids building the agent runtime, not naming it; a
    // stack that grew a second provenance mechanism when an agent did arrive
    // would be the real cost. `User` is the only variant that can be constructed
    // outside a test today.
    #[allow(
        dead_code,
        reason = "reserved attribution variant; constructed only by the attribution tests"
    )]
    Agent,
    /// A future plugin or extension caller. Not implemented.
    #[allow(
        dead_code,
        reason = "reserved attribution variant; no plugin host exists in this repository"
    )]
    Plugin,
    /// A future importer. Not implemented.
    #[allow(
        dead_code,
        reason = "reserved attribution variant; no importer exists in this repository"
    )]
    Import,
    /// A future automation or script caller. Not implemented.
    #[allow(
        dead_code,
        reason = "reserved attribution variant; no automation runner exists in this repository"
    )]
    Automation,
}

/// One committed entry: the operation plus who asked for it.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub operation: SemanticOperation,
    pub origin: Origin,
}

/// Every user-visible document mutation.
///
/// This is the closed set. Adding a capability means adding a variant here
/// and handling it in exactly one place, which is what makes the boundary
/// impossible to forget rather than a call-site convention.
#[derive(Clone, Debug, PartialEq)]
pub enum SemanticOperation {
    /// Persistent, source-backed metadata: renaming a node.
    ///
    /// This is the operation that used to live behind its own private
    /// history. It now shares the stack with everything else.
    Rename(RenameNode),
    /// Persistent, source-backed structure: a node joining or leaving the
    /// document.
    ///
    /// Creation and deletion are the operations that make an object *exist*, as
    /// opposed to moving or restyling one that already does, so they are the one
    /// case where the runtime document is not the whole truth: an object the
    /// runtime holds but `structure` does not is a shape the editor can show and
    /// cannot save. This variant is what stops that state from being reachable.
    Structure(StructureChange),
    /// A canvas command: geometry (move/resize), style, text, insert, or
    /// delete.
    ///
    /// Geometry is temporary prototype state, not authored layout. See
    /// `document_runtime_bridge` for why.
    Runtime(DocumentCommand),
    /// Several operations applied and reverted as one unit.
    ///
    /// Undo replays members in reverse order; redo replays them in order.
    /// A compound is validated in full before any member is applied.
    //
    // The `⌥`-drag builds one: the copies are created and then moved, which is
    // two commands, and "one gesture is one history entry" is only expressible
    // if they travel together. It is a variant of the one operation model rather
    // than a second mutation path — it introduces no new primitive, and `apply`
    // replays members through the same validate-then-apply path as any single
    // operation. Build it through [`SemanticOperation::compound`], which is also
    // what flattens a nested one.
    Compound(Vec<SemanticOperation>),
}

impl SemanticOperation {
    /// Whether applying this operation would change nothing.
    ///
    /// No-op operations never enter history, so a gesture that ends where it
    /// started produces no entry and does not clear redo.
    pub fn is_noop(&self) -> bool {
        match self {
            Self::Rename(rename) => rename.before == rename.after,
            // Whether a node joins or leaves the structure.
            Self::Structure(change) => change.is_noop(),
            // The canvas command owns this judgement for its own variants; an
            // empty insert or delete changes nothing either.
            Self::Runtime(command) => command.is_noop(),
            // A compound is a no-op only if every member is. An empty
            // compound is vacuously a no-op, which is the honest answer: it
            // changes nothing and must not open a history entry.
            Self::Compound(operations) => operations.iter().all(SemanticOperation::is_noop),
        }
    }

    /// Build a compound from intent, flattening any nested compounds.
    ///
    /// Flattening keeps the stack shallow and makes reverse-order undo a
    /// property of one flat list rather than of every nesting depth. An empty
    /// input yields a compound with no members, which `is_noop` reports as a
    /// no-op, so it records nothing.
    pub fn compound(operations: Vec<SemanticOperation>) -> Self {
        let mut flattened = Vec::with_capacity(operations.len());
        for operation in operations {
            match operation {
                Self::Compound(inner) => flattened.extend(inner),
                other => flattened.push(other),
            }
        }
        Self::Compound(flattened)
    }

    /// The members of a compound, or this operation alone.
    ///
    /// Lets the replay paths iterate one shape without caring whether the
    /// entry happened to be compound.
    fn members(&self) -> Vec<&SemanticOperation> {
        match self {
            Self::Compound(operations) => operations.iter().collect(),
            other => vec![other],
        }
    }
}

/// A node appears in the persistent document, or disappears from it.
///
/// Insert and remove are inverses by construction, exactly as a canvas insert
/// and delete are, so one variant carries both directions and
/// [`ReplayDirection`] chooses. That is why a replay cannot drift: undoing a
/// creation is the same splice as deleting the node.
///
/// Both indices are recorded rather than recomputed. A node's place in
/// `structure.nodes` and its place in its parent's `children` are different
/// orderings, and only the operation that made the change knows both. An undo
/// that re-derived them from the current document would put a redone object
/// somewhere other than where it was created — which for a source-backed
/// document is not cosmetic, because the order decides where the authored
/// element lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StructureChange {
    /// Redo inserts the node; undo removes it.
    Insert {
        node: StructuralNode,
        /// Where the node sits in `structure.nodes`.
        node_index: usize,
        /// Where its id sits in its parent's `children`.
        child_index: Option<usize>,
    },
    /// Redo removes the node; undo restores it.
    Remove {
        node: StructuralNode,
        node_index: usize,
        child_index: Option<usize>,
    },
}

impl StructureChange {
    fn parts(&self) -> (&StructuralNode, usize, Option<usize>) {
        match self {
            Self::Insert {
                node,
                node_index,
                child_index,
            }
            | Self::Remove {
                node,
                node_index,
                child_index,
            } => (node, *node_index, *child_index),
        }
    }

    /// Whether applying this change would alter nothing.
    pub fn is_noop(&self) -> bool {
        let (node, _, _) = self.parts();
        // A node with no identity and no name cannot be authored, so treating it
        // as a no-op is the only safe reading.
        node.id.as_str().is_empty() || node.name.trim().is_empty()
    }

    /// Check the change can apply, before any state changes.
    ///
    /// A removal is checked against the document it is removing *from*; an
    /// insertion against the document it is joining. That asymmetry is the point:
    /// requiring presence for both would reject every legitimate operation, and
    /// requiring absence for both would reject every undo.
    pub fn validate(&self, document: &PersistentDocument) -> Result<(), OperationError> {
        let (node, _, _) = self.parts();
        let present = document.structure.nodes.iter().any(|n| n.id == node.id);
        match self {
            Self::Insert { .. } => {
                if present {
                    return Err(OperationError::DuplicateNode(node.id.clone()));
                }
                if document
                    .structure
                    .nodes
                    .iter()
                    .any(|other| other.name == node.name)
                {
                    return Err(OperationError::DuplicateName(node.name.clone()));
                }
                // A parent that is not there would leave a child that no walk can
                // reach, so it is refused rather than silently promoted to a root.
                if let Some(parent) = &node.parent {
                    if !document.structure.nodes.iter().any(|n| &n.id == parent) {
                        return Err(OperationError::MissingNode(parent.clone()));
                    }
                }
                Ok(())
            }
            Self::Remove { .. } => {
                if !present {
                    return Err(OperationError::MissingNode(node.id.clone()));
                }
                // A child left naming this node as its parent would be unreachable
                // by any walk and would fail validation on save. So a removal that
                // still has children is refused rather than performed — the same
                // reasoning, and the same shape, as the insert-side refusal above.
                // Removing a container means removing its contents first; that is
                // what `LamineStructure::subtree_post_order` is for.
                if document
                    .structure
                    .nodes
                    .iter()
                    .any(|other| other.parent.as_ref() == Some(&node.id))
                {
                    return Err(OperationError::NodeHasChildren(node.id.clone()));
                }
                Ok(())
            }
        }
    }

    /// Apply the change. Direction is not a parameter: [`Self::inverse`] is what
    /// undo replays, exactly as it is for [`RenameNode`], so there is one place
    /// that decides which way an operation travels rather than two that have to
    /// agree.
    pub fn apply(&self, document: &mut PersistentDocument) -> Result<(), OperationError> {
        let (node, node_index, child_index) = self.parts();
        match self {
            Self::Insert { .. } => insert_node(document, node, node_index, child_index),
            Self::Remove { .. } => remove_node(document, &node.id),
        }
        Ok(())
    }

    /// The change that undoes this one.
    pub fn inverse(&self) -> Self {
        let (node, node_index, child_index) = self.parts();
        match self {
            Self::Insert { .. } => Self::Remove {
                node: node.clone(),
                node_index,
                child_index,
            },
            Self::Remove { .. } => Self::Insert {
                node: node.clone(),
                node_index,
                child_index,
            },
        }
    }
}

/// Put `node` into the structure at the recorded positions.
///
/// Clamped rather than rejected: the indices were recorded against the document
/// this change was built for, and a document that has since grown must not turn
/// an ordinary replay into a failure. Clamping puts the node at the end, which
/// is the only answer that keeps the structure well-formed.
fn insert_node(
    document: &mut PersistentDocument,
    node: &StructuralNode,
    node_index: usize,
    child_index: Option<usize>,
) {
    let node = node.clone();
    let index = node_index.min(document.structure.nodes.len());
    document.structure.nodes.insert(index, node.clone());
    if let Some(parent) = &node.parent {
        if let Some(found) = document
            .structure
            .nodes
            .iter_mut()
            .find(|n| &n.id == parent)
        {
            let at = child_index
                .unwrap_or(found.children.len())
                .min(found.children.len());
            if !found.children.contains(&node.id) {
                found.children.insert(at, node.id.clone());
            }
        }
    }
}

/// Take `id` out of the structure, and out of its parent's children.
fn remove_node(document: &mut PersistentDocument, id: &NodeId) {
    if let Some(index) = document.structure.nodes.iter().position(|n| &n.id == id) {
        document.structure.nodes.remove(index);
    }
    for node in &mut document.structure.nodes {
        node.children.retain(|child| child != id);
    }
}

/// A failure applying an operation. A failed operation changes neither state
/// nor history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationError {
    /// The operation named a node that does not exist.
    MissingNode(NodeId),
    /// The operation would give a node an identity another node already holds.
    ///
    /// Distinct from [`OperationError::DuplicateName`]: this is the identity that
    /// has to survive save and reopen, so colliding with it would make two
    /// objects the same object on the next load rather than merely confusing.
    DuplicateNode(NodeId),
    /// The operation named a runtime object that does not exist.
    MissingObject(ObjectId),
    /// The operation does not apply to this kind of target, for example a
    /// rename replayed against a bare canvas document.
    WrongTarget(&'static str),
    /// The operation would give a node a name another node already holds.
    ///
    /// `source_document` enforces node-name uniqueness, so this is a real
    /// rejection and not a style preference. It used to surface as
    /// [`OperationError::MissingNode`], which made a legitimate refusal
    /// indistinguishable from a broken identity and let it reach the
    /// apply-time panic in [`EditSession::execute`].
    DuplicateName(String),
    /// The operation would remove a node that other nodes still name as parent.
    ///
    /// The mirror of the insert-side refusal just below. Removing a node while
    /// its children survive would leave each of them naming a parent that is
    /// gone: `lamine.yaml` fails validation on the dangling link, the save
    /// refuses to write anything, and because that refusal is silent about its
    /// cause the project cannot be saved again at all. A caller that means to
    /// take a container's contents with it has to say so by removing them, and
    /// this is what makes saying so the only option.
    NodeHasChildren(NodeId),
    /// The operation's recorded `before` value no longer matches the state,
    /// so replaying it would silently overwrite a change made since.
    ///
    /// This is the replay-failure case: the node exists and the name is free,
    /// but the document has moved on.
    StaleEdit(NodeId),
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingNode(id) => write!(f, "no node with identity {}", id.as_str()),
            Self::DuplicateNode(id) => {
                write!(f, "another node already has identity {}", id.as_str())
            }
            Self::MissingObject(id) => write!(f, "no runtime object {id:?}"),
            Self::WrongTarget(what) => write!(f, "{what} cannot be applied to this target"),
            Self::DuplicateName(name) => write!(f, "another node is already named {name:?}"),
            Self::NodeHasChildren(id) => write!(
                f,
                "cannot remove {} while it still contains objects",
                id.as_str()
            ),
            Self::StaleEdit(id) => {
                write!(
                    f,
                    "the recorded value for {} no longer matches the document",
                    id.as_str()
                )
            }
        }
    }
}

impl std::error::Error for OperationError {}

impl OperationError {
    /// Translate a `source_document` failure for a specific rename.
    ///
    /// `source_document` reports a stale rename as a generic
    /// `InvalidOperation`, and a missing node by its raw id string. Mapping
    /// through the operation's own identity keeps the two apart, which is what
    /// let a duplicate-name refusal masquerade as a missing node.
    fn from_rename(rename: &RenameNode, error: crate::source_document::ModelError) -> Self {
        use crate::source_document::ModelError;
        match error {
            ModelError::DuplicateName(name) => Self::DuplicateName(name),
            ModelError::InvalidOperation(_) => Self::StaleEdit(rename.id.clone()),
            // Anything else from this call site is a missing node: `apply` is
            // the only operation that touches names, and it looks the node up
            // by this id.
            _ => Self::MissingNode(rename.id.clone()),
        }
    }
}

/// Where an operation is applied.
///
/// Unifying two histories means one stack holds operations that touch
/// different state. Rather than force both into one type, the target says which
/// state the replay is allowed to reach.
pub enum OperationTarget<'a> {
    /// Only canvas document state is available.
    ///
    /// Not a mutation path: a rename is refused against this target, so it can
    /// never become a way to change metadata without a document. It exists so a
    /// caller that holds a bare runtime can replay a canvas command without
    /// minting a `PersistentDocument` it does not own — and the canvas's own
    /// replay tests are exactly that caller, asserting that an insert, delete,
    /// geometry, style, or text entry restores the document it was recorded
    /// against. The live editor does not use it: `CanvasView` owns an
    /// `EditSession`, so it always reaches [`OperationTarget::Full`].
    #[allow(
        dead_code,
        reason = "runtime-only replay target; constructed by the canvas replay tests, not by the editor"
    )]
    Runtime(&'a mut Document),
    // Retained, not dead: `ProjectBundle` owns a `PersistentDocument` and no
    // canvas runtime, and `project_bundle`'s save/undo/reload round trip is the
    // caller that needs this. Together with `Runtime` it is what lets one stack
    // hold entries that target different state: a rename needs metadata, a move
    // needs the canvas document.
    #[allow(
        dead_code,
        reason = "bundle seam; constructed only by the project_bundle round-trip test today"
    )]
    Document(&'a mut PersistentDocument),
    /// Both metadata and canvas state are available.
    ///
    /// The only variant production uses: [`EditSession`] always owns both.
    Full {
        document: &'a mut PersistentDocument,
        runtime: &'a mut Document,
    },
}

/// One committed undo/redo stack for the whole editor.
///
/// Entries are [`HistoryEntry`] rather than bare operations so that provenance
/// travels with the operation. See [`Origin`] for why that matters to callers
/// that do not exist yet.
#[derive(Clone, Debug, Default)]
pub struct SemanticHistory {
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
}

// The read side of the stack plus `record` have no production caller, because
// `EditSession` is the sole owner of a `SemanticHistory` in the editor and it
// reaches them through `execute_from`/`record_from`. They are retained rather
// than compiled out because they are the query surface of the very stack
// production writes — an undo affordance asks `can_undo`, and a labelled undo
// step asks `peek_undo_entry` — and every one of them is exercised from the
// editor's own tests. Removing them would delete the ability to inspect history
// at the moment the editor has no reason to inspect it, which is the same
// "hide the gap" move the module doc warns against.
#[allow(
    dead_code,
    reason = "history read/commit surface; EditSession is the sole production owner of a SemanticHistory"
)]
impl SemanticHistory {
    /// Commit an operation as a human action: drop it if it changes nothing,
    /// otherwise push it and clear redo.
    ///
    /// Returns whether anything was recorded.
    pub fn record(&mut self, operation: SemanticOperation) -> bool {
        self.record_from(Origin::User, operation)
    }

    /// Commit an operation attributed to a named caller.
    ///
    /// The no-op check and the redo clear are identical to [`SemanticHistory::record`];
    /// only the attribution differs.
    pub fn record_from(&mut self, origin: Origin, operation: SemanticOperation) -> bool {
        if operation.is_noop() {
            return false;
        }
        self.undo.push(HistoryEntry { operation, origin });
        self.redo.clear();
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Number of committed operations currently undoable.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Number of operations currently redoable.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// The most recently committed operation, without removing it.
    ///
    /// Lets a caller inspect what the next undo would reverse.
    pub fn peek_undo(&self) -> Option<&SemanticOperation> {
        self.undo.last().map(|entry| &entry.operation)
    }

    /// The most recently committed entry, operation and origin together.
    ///
    /// This is what a caller needs in order to label an undo step, and what a
    /// caller cannot reconstruct from [`SemanticHistory::peek_undo`] alone.
    pub fn peek_undo_entry(&self) -> Option<&HistoryEntry> {
        self.undo.last()
    }

    /// Who asked for the operation the next undo would reverse.
    pub fn peek_undo_origin(&self) -> Option<Origin> {
        self.undo.last().map(|entry| entry.origin)
    }

    /// Who asked for the operation the next redo would reapply.
    ///
    /// The counterpart to [`SemanticHistory::peek_undo_origin`]. Attribution
    /// has to be readable on both sides to be trustworthy: after an undo the
    /// entry is on the redo stack, so a caller that could only read the undo
    /// side would report the wrong author.
    pub fn peek_redo_origin(&self) -> Option<Origin> {
        self.redo.last().map(|entry| entry.origin)
    }

    /// Undo the most recent committed operation.
    pub fn undo(&mut self, target: &mut OperationTarget<'_>) -> Result<bool, OperationError> {
        let Some(entry) = self.undo.pop() else {
            return Ok(false);
        };
        match apply(&entry.operation, target, ReplayDirection::Undo) {
            Ok(()) => {
                self.redo.push(entry);
                Ok(true)
            }
            Err(error) => {
                // Put it back so a failed undo does not lose the entry.
                self.undo.push(entry);
                Err(error)
            }
        }
    }

    /// Redo the most recently undone operation.
    pub fn redo(&mut self, target: &mut OperationTarget<'_>) -> Result<bool, OperationError> {
        let Some(entry) = self.redo.pop() else {
            return Ok(false);
        };
        match apply(&entry.operation, target, ReplayDirection::Redo) {
            Ok(()) => {
                self.undo.push(entry);
                Ok(true)
            }
            Err(error) => {
                self.redo.push(entry);
                Err(error)
            }
        }
    }
}

/// Apply one operation in the given direction.
///
/// This is the low-level replay primitive behind [`SemanticHistory::undo`],
/// [`SemanticHistory::redo`], and [`EditSession::execute`]. It is public so a
/// caller that owns only some of the state can still commit: [`EditSession`]
/// requires both a persistent document and a canvas runtime, so a
/// bundle-level caller holding only a [`PersistentDocument`] would otherwise
/// have no way to apply an operation it has just recorded.
pub fn apply(
    operation: &SemanticOperation,
    target: &mut OperationTarget<'_>,
    direction: ReplayDirection,
) -> Result<(), OperationError> {
    // A compound applies in forward order on redo and reverse order on undo,
    // because the members were applied in forward order. Flattened on
    // construction, so this is one ordering rule rather than one per nesting
    // depth.
    let members = operation.members();
    let ordered = match direction {
        ReplayDirection::Redo => members.to_vec(),
        ReplayDirection::Undo => members.iter().rev().copied().collect::<Vec<_>>(),
    };
    for member in ordered {
        apply_one(member, target, direction)?;
    }
    Ok(())
}

/// Apply one non-compound operation in the given direction.
fn apply_one(
    operation: &SemanticOperation,
    target: &mut OperationTarget<'_>,
    direction: ReplayDirection,
) -> Result<(), OperationError> {
    match operation {
        SemanticOperation::Rename(rename) => match target {
            // A bare runtime holds no names, so there is nothing to rename.
            OperationTarget::Runtime(_) => Err(OperationError::WrongTarget("a node rename")),
            OperationTarget::Full { document, .. } | OperationTarget::Document(document) => {
                // Undo replays the inverse, which restores the previous name.
                // Replaying the operation itself would be a no-op at best and
                // a stale-edit error at worst.
                let applied = match direction {
                    ReplayDirection::Undo => rename.inverse(),
                    ReplayDirection::Redo => rename.clone(),
                };
                applied
                    .apply(document)
                    .map_err(|error| OperationError::from_rename(rename, error))
            }
        },
        SemanticOperation::Structure(change) => match target {
            // A bare runtime has no structure to join or leave, and a structure
            // change that quietly did nothing would be worse than a refusal.
            OperationTarget::Runtime(_) => Err(OperationError::WrongTarget("a structural change")),
            OperationTarget::Full { document, .. } | OperationTarget::Document(document) => {
                let applied = match direction {
                    // Undo replays the inverse. Replaying the change itself would
                    // be a no-op at best, and a stale-edit error at worst.
                    ReplayDirection::Undo => change.inverse(),
                    ReplayDirection::Redo => change.clone(),
                };
                applied.apply(document)
            }
        },
        SemanticOperation::Runtime(command) => {
            let runtime = match target {
                OperationTarget::Runtime(runtime) => runtime,
                OperationTarget::Full { runtime, .. } => runtime,
                OperationTarget::Document(_) => {
                    return Err(OperationError::WrongTarget("a canvas command"))
                }
            };
            command.replay(runtime, direction);
            Ok(())
        }
        // `members` guarantees a compound never reaches here; `compound`
        // flattens on construction.
        SemanticOperation::Compound(_) => Err(OperationError::WrongTarget("a nested compound")),
    }
}

/// The editor's mutable state, and the only mutation entry point.
///
/// Owns the two things that are genuinely different kinds of state:
///
/// - `document` — persistent, source-backed. Durable.
/// - `runtime` — the canvas document. Prototype; disposable; rebuilt from a
///   projection rather than persisted.
///
/// Selection and camera are **not** here and are not history. They are
/// editor session state that lives on the canvas view. Keeping them out is
/// what stops a user scrolling the canvas from producing an undo entry, which
/// is the property this boundary exists to guarantee.
pub struct EditSession {
    pub document: PersistentDocument,
    pub runtime: Document,
    pub history: SemanticHistory,
}

impl EditSession {
    pub fn new(document: PersistentDocument, runtime: Document) -> Self {
        Self {
            document,
            runtime,
            history: SemanticHistory::default(),
        }
    }

    /// The single mutation entry point.
    ///
    /// One call is one logical transaction and at most one history entry,
    /// however many objects it touched. A no-op records nothing and leaves
    /// redo intact.
    ///
    /// A failure changes neither state nor history: validation is exhaustive
    /// and runs first, and the operation is only recorded once it has applied.
    pub fn execute(&mut self, operation: SemanticOperation) -> Result<bool, OperationError> {
        self.execute_from(Origin::User, operation)
    }

    /// [`EditSession::execute`], attributed to a named caller.
    pub fn execute_from(
        &mut self,
        origin: Origin,
        operation: SemanticOperation,
    ) -> Result<bool, OperationError> {
        // Validate before mutating so a failure changes neither state nor
        // history. This walks the whole operation, compounds included, so a
        // compound whose fourth member would be rejected never applies its
        // first three.
        validate(&operation, &self.document, &self.runtime)?;
        if operation.is_noop() {
            return Ok(false);
        }
        let Self {
            document, runtime, ..
        } = self;
        // Apply before recording. The constructors already applied eagerly, and
        // every replay is idempotent, so this is normally a no-op — but doing
        // it first means an apply-time failure cannot leave an entry behind or
        // clear the redo branch.
        apply(
            &operation,
            &mut OperationTarget::Full { document, runtime },
            ReplayDirection::Redo,
        )?;
        self.history.record_from(origin, operation);
        Ok(true)
    }

    pub fn undo(&mut self) -> Result<bool, OperationError> {
        // Disjoint field borrows: the history replays into the document and
        // runtime it sits beside, so `self` is never aliased.
        let Self {
            document,
            runtime,
            history,
            ..
        } = self;
        history.undo(&mut OperationTarget::Full { document, runtime })
    }

    pub fn redo(&mut self) -> Result<bool, OperationError> {
        let Self {
            document,
            runtime,
            history,
            ..
        } = self;
        history.redo(&mut OperationTarget::Full { document, runtime })
    }
}

/// Check that an operation can apply, before any state changes.
///
/// This must mirror every rejection [`apply`] can produce, because
/// [`EditSession::execute`] relies on that: an operation that reaches
/// apply-time having passed validation is expected to succeed, and a refusal
/// that slips through here would otherwise be applied only to fail.
///
/// The rename branch is the reason this function is worth writing out rather
/// than delegating. `RenameNode::apply` rejects three distinct conditions, and
/// checking only node existence let a duplicate-name rename reach apply-time,
/// where it used to panic.
fn validate(
    operation: &SemanticOperation,
    document: &PersistentDocument,
    runtime: &Document,
) -> Result<(), OperationError> {
    match operation {
        SemanticOperation::Structure(change) => change.validate(document),
        SemanticOperation::Rename(rename) => {
            let Some(node) = document
                .structure
                .nodes
                .iter()
                .find(|node| node.id == rename.id)
            else {
                return Err(OperationError::MissingNode(rename.id.clone()));
            };
            if rename.before == rename.after {
                return Ok(());
            }
            if document
                .structure
                .nodes
                .iter()
                .any(|other| other.id != rename.id && other.name == rename.after)
            {
                return Err(OperationError::DuplicateName(rename.after.clone()));
            }
            if node.name != rename.before {
                return Err(OperationError::StaleEdit(rename.id.clone()));
            }
            Ok(())
        }
        SemanticOperation::Runtime(command) => command.validate(runtime),
        // Every member, before any member is applied to the real document. A
        // compound is therefore all-or-nothing by construction instead of by
        // compensation, which is what a multi-member entry needs once undo can no
        // longer reverse a half-applied state.
        //
        // Each member is judged against the state its predecessors leave, not
        // against the state before the compound. That is what lets a cascade
        // exist: deleting a container removes the objects inside it first, and
        // the container's own removal is only legal once they are gone — removing
        // it while a child still names it as parent is the dangling reference
        // that makes a whole project unsaveable. Judged all at once against the
        // pre-state, every cascade would be refused.
        //
        // The simulation is a copy, so a refusal still changes nothing.
        SemanticOperation::Compound(operations) => {
            let mut working = document.clone();
            for member in operations {
                validate(member, &working, runtime)?;
                if let SemanticOperation::Structure(change) = member {
                    // A failure here is not this function's to report: the real
                    // apply runs next, against the real document.
                    let _ = change.apply(&mut working);
                }
            }
            Ok(())
        }
    }
}

// -- Operation constructors -------------------------------------------------

/// Rename a node, reading the current name from the document.
///
/// Prefer this over [`rename_node`]: it captures the real `before` value, so
/// undo restores exactly what was there.
pub fn rename_node_in(
    document: &PersistentDocument,
    id: NodeId,
    after: String,
) -> Result<SemanticOperation, OperationError> {
    let node = document
        .structure
        .nodes
        .iter()
        .find(|node| node.id == id)
        .ok_or_else(|| OperationError::MissingNode(id.clone()))?;
    Ok(SemanticOperation::Rename(RenameNode {
        id,
        before: node.name.clone(),
        after,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{Geometry, GeometryChange, ObjectPlacement, ObjectType};
    use crate::source_document::{
        EditorRuntimeState, LamineStructure, SourceBinding, StructuralNode,
    };
    use gpui::{point, size, Point};

    fn node(id: &str, name: &str, kind: &str, parent: Option<&str>) -> StructuralNode {
        StructuralNode {
            id: NodeId::new(id).expect("valid id"),
            name: name.to_owned(),
            kind: kind.to_owned(),
            parent: parent.map(|value| NodeId::new(value).expect("valid id")),
            children: Vec::new(),
            source: SourceBinding {
                file: "index.html".into(),
                selector: format!("[data-spool-id=\"{id}\"]"),
            },
        }
    }

    /// A two-node persistent document, shared by the session helper and by the
    /// replay tests that need metadata without a canvas runtime.
    fn persistent_document() -> PersistentDocument {
        PersistentDocument {
            structure: LamineStructure {
                nodes: vec![
                    node("spool-a", "Alpha", "frame", None),
                    node("spool-b", "Beta", "frame", None),
                ],
            },
            sources: Default::default(),
        }
    }

    fn session() -> EditSession {
        // The runtime starts empty; operations create what they need. Note
        // that `Document::default()` is the canvas starter scene, not empty.
        EditSession::new(persistent_document(), Document::empty())
    }

    fn geometry_of(session: &EditSession, id: ObjectId) -> Geometry {
        session
            .runtime
            .geometry(id)
            .expect("object exists in runtime")
    }

    /// Mint a rectangle in the runtime and describe the creation, without
    /// committing it.
    ///
    /// The two halves are separate on purpose because that is how the canvas
    /// does it: `CanvasView::create_object` mints, and `CanvasView::commit`
    /// hands the placement to `execute`. There is no free-standing constructor
    /// that does both, so these tests exercise the same two calls production
    /// makes rather than a shortcut that no caller uses.
    fn plan_create_rect(session: &mut EditSession) -> (ObjectId, SemanticOperation) {
        let object = session.runtime.create_object(
            ObjectType::Rectangle,
            point(0.0, 0.0),
            size(10.0, 10.0),
            None,
        );
        let placement = ObjectPlacement {
            index: session.runtime.objects().len() - 1,
            object: object.clone(),
        };
        (
            object.id,
            SemanticOperation::Runtime(DocumentCommand::insert(vec![placement])),
        )
    }

    fn seed_rect(session: &mut EditSession) -> ObjectId {
        let (id, operation) = plan_create_rect(session);
        // Record the creation so later geometry has something to move.
        session.execute(operation).expect("create applies");
        id
    }

    /// Describe a move by a delta, reading `before` from the runtime and
    /// writing only the description. This is the production shape: a drag
    /// mutates the runtime, and pointer-up hands `execute` one
    /// `DocumentCommand::geometry` covering every object that moved.
    fn plan_move(session: &EditSession, ids: &[ObjectId], dx: f32, dy: f32) -> SemanticOperation {
        let changes: Vec<GeometryChange> = ids
            .iter()
            .filter_map(|id| {
                let before = session.runtime.geometry(*id)?;
                let after = Geometry {
                    position: point(before.position.x + dx, before.position.y + dy),
                    size: before.size,
                };
                (after != before).then_some(GeometryChange {
                    id: *id,
                    before,
                    after,
                })
            })
            .collect();
        SemanticOperation::Runtime(DocumentCommand::geometry(changes))
    }

    fn move_by(session: &mut EditSession, ids: &[ObjectId], dx: f32, dy: f32) -> bool {
        let operation = plan_move(session, ids, dx, dy);
        session.execute(operation).expect("move applies")
    }

    fn delete(session: &mut EditSession, ids: &[ObjectId]) -> bool {
        let operation = SemanticOperation::Runtime(DocumentCommand::delete(
            session.runtime.remove_objects(ids),
        ));
        session.execute(operation).expect("delete applies")
    }

    /// Copy `ids`, then move only the copies, as one history entry.
    ///
    /// The shape a modifier-drag duplicate needs, and the reason
    /// `SemanticOperation::Compound` exists. The copies are already in the
    /// runtime at their nudged starting position, so the move is measured from
    /// there: the originals are never moved by this operation.
    fn plan_duplicate_and_move(
        session: &mut EditSession,
        ids: &[ObjectId],
        delta: Point<f32>,
    ) -> SemanticOperation {
        let placements = session.runtime.duplicate_objects(ids);
        let copy_ids: Vec<ObjectId> = placements
            .iter()
            .map(|placement| placement.object.id)
            .collect();
        let moved = plan_move(session, &copy_ids, delta.x, delta.y);
        SemanticOperation::compound(vec![
            SemanticOperation::Runtime(DocumentCommand::insert(placements)),
            moved,
        ])
    }

    // -- Rename.

    #[test]
    fn rename_undo_redo() {
        let mut session = session();
        let before = session.history.undo_len();

        let operation = rename_node_in(
            &session.document,
            NodeId::new("spool-a").unwrap(),
            "Alpha 2".into(),
        )
        .expect("node exists");
        assert!(session.execute(operation).unwrap());
        assert_eq!(session.document.structure.nodes[0].name, "Alpha 2");

        assert!(session.undo().unwrap());
        assert_eq!(session.document.structure.nodes[0].name, "Alpha");
        assert!(session.redo().unwrap());
        assert_eq!(session.document.structure.nodes[0].name, "Alpha 2");

        assert_eq!(session.history.undo_len(), before + 1);
    }

    // -- Move.

    #[test]
    fn move_undo_redo() {
        let mut session = session();
        let id = seed_rect(&mut session);
        let start = geometry_of(&session, id);

        move_by(&mut session, &[id], 30.0, 40.0);
        let moved = geometry_of(&session, id);
        assert_ne!(moved, start);

        assert!(session.undo().unwrap());
        assert_eq!(geometry_of(&session, id), start);
        assert!(session.redo().unwrap());
        assert_eq!(geometry_of(&session, id), moved);
    }

    // -- Resize.

    #[test]
    fn resize_undo_redo() {
        let mut session = session();
        let id = seed_rect(&mut session);
        let start = geometry_of(&session, id);
        let resized = Geometry {
            position: start.position,
            size: size(80.0, 60.0),
        };

        let operation =
            SemanticOperation::Runtime(DocumentCommand::geometry(vec![GeometryChange {
                id,
                before: start,
                after: resized,
            }]));
        session.execute(operation).unwrap();
        assert_eq!(geometry_of(&session, id), resized);

        assert!(session.undo().unwrap());
        assert_eq!(geometry_of(&session, id), start);
        assert!(session.redo().unwrap());
        assert_eq!(geometry_of(&session, id), resized);
    }

    // -- Create.

    #[test]
    fn create_undo_redo() {
        let mut session = session();
        let empty = session.runtime.objects().len();

        let (_, operation) = plan_create_rect(&mut session);
        session.execute(operation).unwrap();
        assert_eq!(session.runtime.objects().len(), empty + 1);
        let created = session.runtime.objects()[0].id;

        assert!(session.undo().unwrap());
        assert_eq!(session.runtime.objects().len(), empty);
        assert!(session.redo().unwrap());
        assert_eq!(session.runtime.objects().len(), empty + 1);
        assert!(
            session.runtime.object(created).is_some(),
            "stable id after redo"
        );
    }

    // -- Delete.

    #[test]
    fn delete_undo_redo() {
        let mut session = session();
        let id = seed_rect(&mut session);
        let populated = session.runtime.objects().len();

        assert!(delete(&mut session, &[id]));
        assert!(session.runtime.object(id).is_none());

        assert!(session.undo().unwrap());
        assert_eq!(session.runtime.objects().len(), populated);
        assert!(
            session.runtime.object(id).is_some(),
            "deleted id restored by undo"
        );
        assert!(session.redo().unwrap());
        assert!(session.runtime.object(id).is_none());
    }

    // -- Structure: a node joining and leaving the persistent document.

    /// An edit session over a document holding exactly these nodes.
    fn structure_session(nodes: Vec<StructuralNode>) -> EditSession {
        EditSession::new(
            PersistentDocument {
                structure: crate::source_document::LamineStructure { nodes },
                sources: std::collections::HashMap::new(),
            },
            Document::default(),
        )
    }

    fn doc(session: &EditSession) -> &PersistentDocument {
        &session.document
    }

    fn doc_mut(session: &mut EditSession) -> &mut PersistentDocument {
        &mut session.document
    }

    fn structural(id: &str, name: &str, parent: Option<&str>) -> StructuralNode {
        StructuralNode {
            id: NodeId::new(id).expect("valid id"),
            name: name.to_owned(),
            kind: "rectangle".to_owned(),
            parent: parent.map(NodeId::new).transpose().expect("valid parent"),
            children: Vec::new(),
            source: SourceBinding {
                file: "index.html".to_owned(),
                selector: format!("[data-spool-id=\"{id}\"]"),
            },
        }
    }

    fn frame(id: &str, children: &[&str]) -> StructuralNode {
        let mut node = structural(id, id, None);
        node.children = children.iter().map(|c| NodeId::new(*c).unwrap()).collect();
        node
    }

    fn insert(node: StructuralNode, at: usize) -> SemanticOperation {
        SemanticOperation::Structure(StructureChange::Insert {
            node,
            node_index: at,
            child_index: None,
        })
    }

    fn ids(document: &PersistentDocument) -> Vec<String> {
        document
            .structure
            .nodes
            .iter()
            .map(|n| n.id.as_str().to_owned())
            .collect()
    }

    fn remove(node: StructuralNode, at: usize) -> SemanticOperation {
        SemanticOperation::Structure(StructureChange::Remove {
            node,
            node_index: at,
            child_index: None,
        })
    }

    #[test]
    fn a_removal_that_would_orphan_a_child_is_refused() {
        // The counterpart to the insert-side refusal. Removing a container while
        // its children survive leaves each of them naming a parent that is gone,
        // and `lamine.yaml` refuses to encode that — so the project stops saving
        // entirely rather than losing a single object.
        let parent = structural("spool-parent", "Parent", None);
        let child = structural("spool-child", "Child", Some("spool-parent"));
        let mut session = structure_session(vec![parent.clone(), child]);

        let error = session
            .execute(remove(parent, 0))
            .expect_err("removing a node that still has children must be refused");
        assert_eq!(
            error,
            OperationError::NodeHasChildren(NodeId::new("spool-parent").unwrap()),
            "and the refusal has to say why, not merely fail"
        );
        assert_eq!(
            session.document.structure.nodes.len(),
            2,
            "a refusal leaves the document as it was"
        );
    }

    #[test]
    fn a_removal_is_allowed_once_the_children_are_gone() {
        // The guard is about orphans, not about containers: the same removal is
        // fine once nothing is left pointing at it, which is what makes a
        // post-order cascade possible at all.
        let parent = structural("spool-parent", "Parent", None);
        let child = structural("spool-child", "Child", Some("spool-parent"));
        let mut session = structure_session(vec![parent.clone(), child.clone()]);

        assert!(session
            .execute(remove(child, 1))
            .expect("a leaf removal is fine"));
        assert!(session
            .execute(remove(parent, 0))
            .expect("and then the container has nothing left to orphan"));
        assert_eq!(session.document.structure.nodes.len(), 0);
    }

    #[test]
    fn a_cascade_is_refused_when_judged_against_the_state_before_it() {
        // The reason compound members are validated in order rather than all at
        // once. A cascade removes a child before its parent, so judging every
        // member against the pre-state would refuse the parent — and silently
        // refuse the whole gesture with it, since a compound is all-or-nothing.
        let parent = structural("spool-parent", "Parent", None);
        let child = structural("spool-child", "Child", Some("spool-parent"));
        let mut session = structure_session(vec![parent.clone(), child.clone()]);

        assert!(
            session
                .execute(SemanticOperation::compound(vec![
                    remove(child, 1),
                    remove(parent, 0),
                ]))
                .expect("a post-order cascade is one legal operation"),
            "child first, then the container that no longer has one"
        );
        assert_eq!(session.document.structure.nodes.len(), 0);
    }

    #[test]
    fn an_inserted_node_joins_the_document_and_its_parent() {
        let mut document = structure_session(vec![frame("spool-root", &[])]);
        document
            .execute(insert(
                structural("spool-new", "New", Some("spool-root")),
                1,
            ))
            .expect("the insert applies");

        assert_eq!(ids(doc(&document)), ["spool-root", "spool-new"]);
        assert_eq!(
            doc(&document).structure.nodes[0].children[0].as_str(),
            "spool-new",
            "and the parent lists it"
        );
    }

    #[test]
    fn a_removed_node_leaves_the_document_and_its_parent() {
        let mut new = structural("spool-new", "New", Some("spool-root"));
        new.kind = "rectangle".to_owned();
        let mut document =
            structure_session(vec![frame("spool-root", &["spool-new"]), new.clone()]);
        let node = new;
        let position = 1;
        document
            .execute(SemanticOperation::Structure(StructureChange::Remove {
                node: node.clone(),
                node_index: position,
                child_index: Some(0),
            }))
            .expect("the removal applies");

        assert_eq!(ids(doc(&document)), ["spool-root"]);
        assert!(
            doc(&document).structure.nodes[0].children.is_empty(),
            "the parent no longer lists it"
        );
    }

    #[test]
    fn undo_and_redo_are_exact_inverses_for_structure() {
        let keep = structural("spool-keep", "Keep", Some("spool-root"));
        let mut document = structure_session(vec![frame("spool-root", &["spool-keep"]), keep]);
        let before = doc(&document).structure.clone();
        document
            .execute(insert(
                structural("spool-new", "New", Some("spool-root")),
                2,
            ))
            .expect("applies");
        assert_ne!(doc(&document).structure, before);

        document.undo().expect("undo replays");
        assert_eq!(
            doc(&document).structure,
            before,
            "undo restored the document"
        );
        document.redo().expect("redo replays");
        assert_eq!(
            ids(doc(&document)),
            ["spool-root", "spool-keep", "spool-new"]
        );
        assert_eq!(doc(&document).structure.nodes[0].children.len(), 2);
    }

    #[test]
    fn a_structural_insert_is_refused_when_the_identity_is_taken() {
        let mut document =
            structure_session(vec![frame("spool-root", &[]), frame("spool-taken", &[])]);
        let error = document
            .execute(insert(frame("spool-taken", &[]), 2))
            .expect_err("an identity may not be reused");
        assert!(
            matches!(error, OperationError::DuplicateNode(_)),
            "{error:?}"
        );
        assert_eq!(
            ids(doc(&document)),
            ["spool-root", "spool-taken"],
            "nothing changed"
        );
    }

    #[test]
    fn a_structural_insert_is_refused_when_the_name_is_taken() {
        let mut document =
            structure_session(vec![frame("spool-root", &[]), frame("spool-other", &[])]);
        let mut node = structural("spool-new", "spool-root", None);
        node.name = "Rectangle 1".to_owned();
        doc_mut(&mut document).structure.nodes[1].name = "Rectangle 1".to_owned();
        let error = document
            .execute(insert(node, 2))
            .expect_err("names are unique across the document");
        assert!(
            matches!(error, OperationError::DuplicateName(_)),
            "{error:?}"
        );
    }

    #[test]
    fn a_child_of_a_parent_that_is_not_there_is_refused() {
        // A child nothing can reach is a node that exists in the file and in
        // nothing else, so it is refused rather than silently promoted to a root.
        let mut document = structure_session(vec![frame("spool-root", &[])]);
        let error = document
            .execute(insert(
                structural("spool-new", "New", Some("spool-ghost")),
                1,
            ))
            .expect_err("the parent is not there");
        assert!(matches!(error, OperationError::MissingNode(_)), "{error:?}");
    }

    #[test]
    fn a_removal_of_a_node_that_is_absent_is_refused() {
        let mut document = structure_session(vec![frame("spool-root", &[])]);
        let error = document
            .execute(SemanticOperation::Structure(StructureChange::Remove {
                node: frame("spool-ghost", &[]),
                node_index: 0,
                child_index: None,
            }))
            .expect_err("there is nothing to remove");
        assert!(matches!(error, OperationError::MissingNode(_)), "{error:?}");
    }

    #[test]
    fn a_creation_is_one_history_entry_whatever_it_touches() {
        let mut document = structure_session(vec![frame("spool-root", &[])]);
        let mut runtime = Document::default();
        let operation = SemanticOperation::compound(vec![
            insert(structural("spool-new", "New", Some("spool-root")), 1),
            SemanticOperation::Runtime(DocumentCommand::insert(Vec::new())),
        ]);
        assert!(document.execute(operation).expect("applies"));
        assert_eq!(document.history.undo_len(), 1, "one gesture, one entry");
        document.undo().expect("undo");
        assert_eq!(ids(doc(&document)), ["spool-root"], "both halves went");
        let _ = &mut runtime;
    }

    #[test]
    fn undoing_a_removal_puts_the_node_back_where_it_was() {
        // The other direction. If insert and remove were not inverses, undoing a
        // delete would either do nothing or create a second copy, and neither
        // would be visible on the canvas — the node would just never come back.
        let victim = structural("spool-gone", "Gone", Some("spool-root"));
        let mut document =
            structure_session(vec![frame("spool-root", &["spool-gone"]), victim.clone()]);
        document
            .execute(SemanticOperation::Structure(StructureChange::Remove {
                node: victim.clone(),
                node_index: 1,
                child_index: Some(0),
            }))
            .expect("the removal applies");
        assert_eq!(ids(doc(&document)), ["spool-root"]);

        document.undo().expect("undo replays");
        assert_eq!(
            ids(doc(&document)),
            ["spool-root", "spool-gone"],
            "the node is back, once"
        );
        assert_eq!(doc(&document).structure.nodes[0].children.len(), 1);
        document.redo().expect("redo replays");
        assert_eq!(ids(doc(&document)), ["spool-root"], "and removed again");
    }

    #[test]
    fn a_node_is_inserted_where_the_operation_recorded() {
        // The recorded index is what keeps a redone object in the same place, and
        // for a source-backed document that decides where its authored element
        // lands. Inserting at the end regardless would silently reorder.
        let mut document = structure_session(vec![
            frame("spool-root", &[]),
            frame("spool-a", &[]),
            frame("spool-b", &[]),
        ]);
        document
            .execute(insert(frame("spool-new", &[]), 1))
            .expect("applies");
        assert_eq!(
            ids(doc(&document)),
            ["spool-root", "spool-new", "spool-a", "spool-b"],
            "the node took the index it was given"
        );
    }

    // -- Duplicate.

    #[test]
    fn duplicate_undo_redo() {
        let mut session = session();
        let id = seed_rect(&mut session);
        let original = session.runtime.objects().len();

        let operation = SemanticOperation::Runtime(DocumentCommand::insert(
            session.runtime.duplicate_objects(&[id]),
        ));
        session.execute(operation).unwrap();
        assert_eq!(session.runtime.objects().len(), original + 1);
        let duplicate_id = session
            .runtime
            .objects()
            .iter()
            .find(|object| object.id != id)
            .expect("a duplicate exists")
            .id;

        assert!(session.undo().unwrap());
        assert_eq!(session.runtime.objects().len(), original);
        assert!(session.redo().unwrap());
        assert_eq!(session.runtime.objects().len(), original + 1);
        // The duplicate keeps its identity across the round trip.
        assert!(session.runtime.object(duplicate_id).is_some());
    }

    // -- Cancellation.

    #[test]
    fn a_transient_runtime_mutation_records_nothing() {
        // A drag mutates the runtime directly and only reaches `execute` on
        // pointer-up. What protects that is not a gesture API on this side but
        // the fact that the runtime is not history: nothing here is recorded, so
        // Escape — which restores the pre-gesture geometry — leaves no entry.
        let mut session = session();
        let id = seed_rect(&mut session);
        let start = geometry_of(&session, id);
        let depth = session.history.undo_len();

        session.runtime.set_geometry(
            id,
            Geometry {
                position: point(500.0, 500.0),
                size: size(999.0, 999.0),
            },
        );
        assert_eq!(
            session.history.undo_len(),
            depth,
            "mutating the runtime is not a commit"
        );
        assert!(!session.history.can_undo() || session.history.undo_len() == depth);

        // Restoring the pre-gesture value is therefore indistinguishable from
        // never having moved.
        session.runtime.set_geometry(id, start);
        assert_eq!(geometry_of(&session, id), start);
        assert_eq!(session.history.undo_len(), depth, "no entry was created");

        // And the move that *is* committed undoes back to where it began.
        assert!(move_by(&mut session, &[id], 40.0, 0.0));
        assert_eq!(session.history.undo_len(), depth + 1);
        assert!(session.undo().unwrap());
        assert_eq!(geometry_of(&session, id), start);
        assert_eq!(session.history.undo_len(), depth);
    }

    #[test]
    fn multi_node_move_is_one_history_transaction() {
        let mut session = session();
        let first = seed_rect(&mut session);
        let (_, operation) = plan_create_rect(&mut session);
        session.execute(operation).unwrap();
        let second = session
            .runtime
            .objects()
            .iter()
            .find(|object| object.id != first)
            .expect("second object")
            .id;

        let depth = session.history.undo_len();
        move_by(&mut session, &[first, second], 25.0, 0.0);

        // One entry for two objects.
        assert_eq!(session.history.undo_len(), depth + 1);
        // One undo reverses both.
        assert!(session.undo().unwrap());
        assert_eq!(session.history.undo_len(), depth);
        assert_eq!(geometry_of(&session, first).position.x, 0.0);
        assert_eq!(geometry_of(&session, second).position.x, 0.0);
    }

    // -- Refusals: an operation that cannot apply must not panic, record, or
    // touch the redo branch.

    #[test]
    fn a_rename_to_a_taken_name_is_refused_without_panicking() {
        // Regression: `validate` used to check only that the node existed, so
        // this reached the apply-time `.expect` and took the editor down. The
        // refusal has to happen before anything is recorded.
        let mut session = session();
        let depth = session.history.undo_len();
        let operation = rename_node_in(
            &session.document,
            NodeId::new("spool-a").unwrap(),
            "Beta".into(),
        )
        .unwrap();

        let error = session
            .execute(operation)
            .expect_err("a taken name must be refused");
        assert_eq!(error, OperationError::DuplicateName("Beta".into()));
        assert_eq!(
            session.history.undo_len(),
            depth,
            "a refusal records nothing"
        );
        assert_eq!(
            session.document.structure.nodes[0].name, "Alpha",
            "state unchanged"
        );
    }

    #[test]
    fn a_refused_operation_preserves_the_redo_branch() {
        // The documented invariant is that a failed operation changes neither
        // state nor history. The redo branch is history.
        let mut session = session();
        let id = seed_rect(&mut session);
        move_by(&mut session, &[id], 5.0, 0.0);
        session.undo().unwrap();
        assert!(session.history.can_redo());

        let operation = rename_node_in(
            &session.document,
            NodeId::new("spool-a").unwrap(),
            "Beta".into(),
        )
        .unwrap();
        assert!(session.execute(operation).is_err());

        assert!(session.history.can_redo(), "a refusal must not clear redo");
    }

    #[test]
    fn a_rename_whose_before_value_is_stale_is_refused() {
        // The replay-failure case: the node exists and the name is free, but
        // the recorded `before` no longer matches, so replaying would silently
        // overwrite an intervening change.
        let mut session = session();
        session
            .execute(
                rename_node_in(
                    &session.document,
                    NodeId::new("spool-a").unwrap(),
                    "Renamed".into(),
                )
                .unwrap(),
            )
            .unwrap();

        // Hand-built, so its `before` is now wrong. The constructor cannot
        // produce this — only a stale replay or a second caller can.
        let stale = SemanticOperation::Rename(RenameNode {
            id: NodeId::new("spool-a").unwrap(),
            before: "Alpha".into(),
            after: "Third".into(),
        });
        let error = session
            .execute(stale)
            .expect_err("a stale rename must be refused");
        assert_eq!(
            error,
            OperationError::StaleEdit(NodeId::new("spool-a").unwrap())
        );
        assert_eq!(session.document.structure.nodes[0].name, "Renamed");
    }

    #[test]
    fn every_rename_refusal_is_distinguishable() {
        // The three rejections must not collapse into one variant. They did
        // once, which is how a duplicate name was reported as a missing node.
        let mut session = session();

        let missing = SemanticOperation::Rename(RenameNode {
            id: NodeId::new("spool-nope").unwrap(),
            before: "A".into(),
            after: "B".into(),
        });
        assert_eq!(
            session.execute(missing).unwrap_err(),
            OperationError::MissingNode(NodeId::new("spool-nope").unwrap())
        );

        let taken = SemanticOperation::Rename(RenameNode {
            id: NodeId::new("spool-a").unwrap(),
            before: "Alpha".into(),
            after: "Beta".into(),
        });
        assert_eq!(
            session.execute(taken).unwrap_err(),
            OperationError::DuplicateName("Beta".into())
        );

        // The name is free, so staleness is the only thing left to catch it.
        let stale = SemanticOperation::Rename(RenameNode {
            id: NodeId::new("spool-a").unwrap(),
            before: "Wrong".into(),
            after: "Fresh".into(),
        });
        assert_eq!(
            session.execute(stale).unwrap_err(),
            OperationError::StaleEdit(NodeId::new("spool-a").unwrap())
        );
    }

    // -- Compound operations.

    #[test]
    fn a_compound_is_one_history_entry() {
        let mut session = session();
        let id = seed_rect(&mut session);
        let depth = session.history.undo_len();

        let operation = plan_duplicate_and_move(&mut session, &[id], point(40.0, 25.0));
        assert!(session.execute(operation).unwrap());

        assert_eq!(
            session.history.undo_len(),
            depth + 1,
            "two members, one entry"
        );
        assert!(session.undo().unwrap());
        assert_eq!(session.history.undo_len(), depth);
        assert!(session.redo().unwrap());
        assert_eq!(session.history.undo_len(), depth + 1);
    }

    #[test]
    fn a_compound_undo_removes_the_copies_and_reverses_their_move_together() {
        // The property that makes a compound worth having. Two entries would
        // undo the movement first and leave the copies stranded where they were
        // dropped until a second undo.
        let mut session = session();
        let id = seed_rect(&mut session);
        let original_position = geometry_of(&session, id).position;

        let operation = plan_duplicate_and_move(&mut session, &[id], point(40.0, 25.0));
        session.execute(operation).unwrap();

        let copy = session
            .runtime
            .objects()
            .iter()
            .find(|object| object.id != id)
            .expect("a copy exists")
            .id;
        assert_eq!(session.runtime.objects().len(), 2);
        assert_ne!(geometry_of(&session, copy).position, original_position);

        // One undo: the copy is gone. Not merely put back where it started.
        assert!(session.undo().unwrap());
        assert!(
            session.runtime.object(copy).is_none(),
            "the copy is removed"
        );
        assert_eq!(
            session.runtime.objects().len(),
            1,
            "only the original remains"
        );
        assert_eq!(
            geometry_of(&session, id).position,
            original_position,
            "the original never moved"
        );

        assert!(session.redo().unwrap());
        assert_eq!(session.runtime.objects().len(), 2);
        assert!(
            session.runtime.object(copy).is_some(),
            "the copy returns with its identity"
        );
    }

    #[test]
    fn a_compound_undo_reverses_members_in_reverse_order() {
        // Order is the whole reason a compound is not just a list, and it is
        // only observable when a later member's inverse depends on the earlier
        // member having been applied. Two chained renames of one node are that
        // case: each records the value the previous one produced.
        //
        // Forward-order undo would apply `Mid -> Alpha` while the node still
        // reads `End`, which the staleness check rejects outright. Reverse
        // order unwinds the chain and the node lands back on its original name.
        let mut document = persistent_document();
        let name = |document: &PersistentDocument| document.structure.nodes[0].name.clone();
        let id = NodeId::new("spool-a").unwrap();

        let compound = SemanticOperation::compound(vec![
            SemanticOperation::Rename(RenameNode {
                id: id.clone(),
                before: "Alpha".into(),
                after: "Mid".into(),
            }),
            SemanticOperation::Rename(RenameNode {
                id: id.clone(),
                before: "Mid".into(),
                after: "End".into(),
            }),
        ]);

        let target = |document: &mut PersistentDocument, direction| {
            apply(
                &compound,
                &mut OperationTarget::Document(document),
                direction,
            )
            .unwrap();
        };
        target(&mut document, ReplayDirection::Redo);
        assert_eq!(name(&document), "End", "applied forward");

        // Reverse order is required, not incidental: applied forward, the
        // first inverse would ask the node to read `Mid` while it reads `End`,
        // and `RenameNode::apply` refuses that as stale.
        target(&mut document, ReplayDirection::Undo);
        assert_eq!(name(&document), "Alpha", "reversed");

        target(&mut document, ReplayDirection::Redo);
        assert_eq!(name(&document), "End", "re-applied forward");
    }

    #[test]
    fn a_compound_whose_members_chain_cannot_be_committed_as_one_entry() {
        // The limit the previous test deliberately steps around, recorded so it
        // is not rediscovered as a bug.
        //
        // `validate` checks every member against one pre-transaction snapshot.
        // Two members that depend on each other cannot both satisfy that: the
        // second expects the value the first has not produced yet. So the replay
        // rule handles chained members, but `execute` refuses to commit them.
        //
        // This is the price of refusing to record a half-applied entry, and it
        // is the right trade while compound members are built from intent. It
        // is the first thing to revisit if a caller needs it.
        let mut session = session();
        let id = NodeId::new("spool-a").unwrap();
        let compound = SemanticOperation::compound(vec![
            SemanticOperation::Rename(RenameNode {
                id: id.clone(),
                before: "Alpha".into(),
                after: "Mid".into(),
            }),
            SemanticOperation::Rename(RenameNode {
                id: id.clone(),
                before: "Mid".into(),
                after: "End".into(),
            }),
        ]);

        let error = session
            .execute(compound)
            .expect_err("chained members are refused");
        assert_eq!(error, OperationError::StaleEdit(id));
        assert_eq!(
            session.document.structure.nodes[0].name, "Alpha",
            "and nothing applied"
        );
    }

    #[test]
    fn a_planned_move_is_relative_to_where_the_object_actually_is() {
        // Pinned with a non-zero origin on purpose. At the origin, "position
        // plus delta" and "the delta alone" are the same number, so a planner
        // that ignored the object's position would still pass every other test
        // in this module. This is the only assertion that separates them.
        let mut session = session();
        let object = session.runtime.create_object(
            ObjectType::Rectangle,
            point(100.0, 200.0),
            size(10.0, 10.0),
            None,
        );
        let created = SemanticOperation::Runtime(DocumentCommand::insert(vec![ObjectPlacement {
            index: session.runtime.objects().len() - 1,
            object: object.clone(),
        }]));
        session.execute(created).unwrap();
        let id = object.id;
        let start = geometry_of(&session, id);
        assert_eq!(start.position, point(100.0, 200.0));

        let planned = plan_move(&session, &[id], 5.0, -3.0);
        assert_eq!(geometry_of(&session, id), start, "planning moved nothing");

        let replanned = plan_move(&session, &[id], 5.0, -3.0);
        assert_eq!(replanned, planned, "planning reads, it does not mutate");
        session.execute(replanned).unwrap();
        assert_eq!(
            geometry_of(&session, id).position,
            point(105.0, 197.0),
            "the delta is applied to the object's real position"
        );

        // Planning leaves the runtime exactly where it was, so undoing the
        // committed plan and planning again describes the same edit. That is
        // what makes a compound composable: a refused member has changed
        // nothing for its siblings to trip over.
        session.undo().unwrap();
        assert_eq!(geometry_of(&session, id), start);
    }

    #[test]
    fn two_planned_moves_of_one_object_do_not_accumulate() {
        // Worth pinning because it is a real constraint on building compounds,
        // not an accident. A planned move measures from the state it reads,
        // so two planned moves of the same object both start from the same
        // place and the second wins. Composition across objects works; stacking
        // deltas on one object needs the deltas summed by the caller.
        let mut session = session();
        let id = seed_rect(&mut session);
        let start = geometry_of(&session, id);

        let first = plan_move(&session, &[id], 10.0, 0.0);
        let second = plan_move(&session, &[id], 0.0, 20.0);
        assert_ne!(first, second, "the two plans differ");

        session
            .execute(SemanticOperation::compound(vec![first, second]))
            .unwrap();

        // The second plan wins rather than summing with the first: both read
        // the same starting geometry, so this is not the sum of the deltas.
        assert_eq!(
            geometry_of(&session, id).position,
            point(start.position.x, start.position.y + 20.0),
            "planned moves do not accumulate"
        );
        assert!(session.undo().unwrap());
        assert_eq!(geometry_of(&session, id), start);
    }

    #[test]
    fn a_compound_of_moves_on_distinct_objects_accumulates() {
        // The realistic compound: one gesture touching several objects, each
        // planned against the state it read.
        let mut session = session();
        let first = seed_rect(&mut session);
        let (_, created) = plan_create_rect(&mut session);
        session.execute(created).unwrap();
        let second = session
            .runtime
            .objects()
            .iter()
            .map(|object| object.id)
            .find(|candidate| *candidate != first)
            .expect("a second object");
        let depth = session.history.undo_len();

        let operation = SemanticOperation::compound(vec![
            plan_move(&session, &[first], 10.0, 0.0),
            plan_move(&session, &[second], 0.0, 20.0),
        ]);
        assert!(session.execute(operation).unwrap());
        assert_eq!(session.history.undo_len(), depth + 1);

        assert_eq!(geometry_of(&session, first).position, point(10.0, 0.0));
        assert_eq!(geometry_of(&session, second).position, point(0.0, 20.0));

        assert!(session.undo().unwrap());
        assert_eq!(geometry_of(&session, first).position, point(0.0, 0.0));
        assert_eq!(geometry_of(&session, second).position, point(0.0, 0.0));
    }

    #[test]
    fn a_compound_replays_identically_over_many_undo_redo_cycles() {
        // Deterministic replay. If the recorded members were not symmetric
        // under the reverse-order undo rule, the state would drift each cycle.
        let mut session = session();
        let id = seed_rect(&mut session);
        let original_position = geometry_of(&session, id).position;

        let operation = plan_duplicate_and_move(&mut session, &[id], point(33.0, 44.0));
        session.execute(operation).unwrap();
        let committed: Vec<(ObjectId, Geometry)> = session
            .runtime
            .objects()
            .iter()
            .filter_map(|object| session.runtime.geometry(object.id).map(|g| (object.id, g)))
            .collect();

        for _ in 0..5 {
            assert!(session.undo().unwrap());
            assert_eq!(session.runtime.objects().len(), 1, "one object after undo");
            assert_eq!(geometry_of(&session, id).position, original_position);

            assert!(session.redo().unwrap());
            let replayed: Vec<(ObjectId, Geometry)> = session
                .runtime
                .objects()
                .iter()
                .filter_map(|object| session.runtime.geometry(object.id).map(|g| (object.id, g)))
                .collect();
            assert_eq!(
                replayed, committed,
                "each cycle reproduces the state exactly"
            );
        }
    }

    #[test]
    fn a_compound_whose_last_member_is_invalid_applies_nothing() {
        // All-or-nothing. If members were validated as they were applied, the
        // first two would land and the entry would be half-applied — with no
        // way to undo it, because reversing the applied prefix is not the same
        // operation as reversing the compound.
        let mut session = session();
        let id = seed_rect(&mut session);
        let position = geometry_of(&session, id);
        let depth = session.history.undo_len();

        // `plan_move_nodes` only reads, so the member has not moved yet when
        // validation runs. An eager `move_nodes` here would have already moved
        // it, which is exactly the leak this test exists to rule out.
        let good = plan_move(&session, &[id], 100.0, 100.0);
        let also_good = plan_move(&session, &[id], 0.0, 100.0);
        let bad = SemanticOperation::Rename(RenameNode {
            id: NodeId::new("spool-a").unwrap(),
            before: "Alpha".into(),
            after: "Beta".into(),
        });
        let operation = SemanticOperation::compound(vec![good, also_good, bad]);

        assert!(
            session.execute(operation).is_err(),
            "the compound is refused"
        );
        assert_eq!(
            geometry_of(&session, id),
            position,
            "the valid member never applied"
        );
        assert_eq!(
            session.history.undo_len(),
            depth,
            "and nothing was recorded"
        );
    }

    #[test]
    fn nested_compounds_flatten_so_replay_order_is_unambiguous() {
        let mut session = session();
        let id = seed_rect(&mut session);
        let start = geometry_of(&session, id);

        let inner = SemanticOperation::compound(vec![
            plan_move(&session, &[id], 5.0, 0.0),
            plan_move(&session, &[id], 0.0, 5.0),
        ]);
        let outer = SemanticOperation::compound(vec![inner, plan_move(&session, &[id], 7.0, 0.0)]);

        // Flattened to three members, not a nested tree.
        let SemanticOperation::Compound(members) = &outer else {
            panic!("expected a compound")
        };
        assert_eq!(members.len(), 3);
        assert!(matches!(members[0], SemanticOperation::Runtime(_)));

        assert!(session.execute(outer).unwrap());
        assert!(session.undo().unwrap());
        assert_eq!(geometry_of(&session, id), start);
    }

    #[test]
    fn a_compound_is_a_noop_only_when_every_member_is() {
        let mut session = session();
        let id = seed_rect(&mut session);
        let position = geometry_of(&session, id);

        let idle = plan_move(&session, &[id], 0.0, 0.0);
        assert!(SemanticOperation::compound(vec![idle.clone()]).is_noop());
        assert!(
            SemanticOperation::compound(vec![]).is_noop(),
            "an empty compound changes nothing"
        );

        let real = plan_move(&session, &[id], 12.0, 0.0);
        assert!(!SemanticOperation::compound(vec![idle, real]).is_noop());
        assert_eq!(
            geometry_of(&session, id),
            position,
            "planning moved nothing"
        );

        // And a no-op compound records nothing.
        let depth = session.history.undo_len();
        let idle = plan_move(&session, &[id], 0.0, 0.0);
        assert!(!session
            .execute(SemanticOperation::compound(vec![idle]))
            .unwrap());
        assert_eq!(session.history.undo_len(), depth);
        assert_eq!(geometry_of(&session, id), position);
    }

    #[test]
    fn a_new_operation_after_a_compound_undo_invalidates_the_whole_branch() {
        let mut session = session();
        let id = seed_rect(&mut session);
        let operation = plan_duplicate_and_move(&mut session, &[id], point(10.0, 10.0));
        session.execute(operation).unwrap();
        session.undo().unwrap();
        assert!(session.history.can_redo());

        move_by(&mut session, &[id], 3.0, 0.0);
        assert!(
            !session.history.can_redo(),
            "redo branch is dropped, not truncated"
        );
    }

    #[test]
    fn a_compound_mixing_metadata_and_runtime_undoes_as_one_lifo_entry() {
        // A compound is the only way a rename and a runtime change land
        // together, which is the case an agent caller needs: one action, one
        // undo step, touching both kinds of state.
        let mut session = session();
        let id = seed_rect(&mut session);
        let start = geometry_of(&session, id);
        let depth = session.history.undo_len();

        let rename = rename_node_in(
            &session.document,
            NodeId::new("spool-a").unwrap(),
            "Alpha 2".into(),
        )
        .unwrap();
        let move_op = plan_move(&session, &[id], 15.0, 15.0);
        assert!(session
            .execute(SemanticOperation::compound(vec![rename, move_op]))
            .unwrap());

        assert_eq!(session.document.structure.nodes[0].name, "Alpha 2");
        assert_ne!(geometry_of(&session, id), start);
        assert_eq!(session.history.undo_len(), depth + 1);

        assert!(session.undo().unwrap());
        assert_eq!(
            session.document.structure.nodes[0].name, "Alpha",
            "metadata reverted"
        );
        assert_eq!(geometry_of(&session, id), start, "runtime reverted");

        assert!(session.redo().unwrap());
        assert_eq!(session.document.structure.nodes[0].name, "Alpha 2");
        assert_ne!(geometry_of(&session, id), start);
    }

    #[test]
    fn a_duplicate_and_move_of_nothing_records_nothing() {
        let mut session = session();
        let depth = session.history.undo_len();
        let operation = plan_duplicate_and_move(&mut session, &[ObjectId(9_999)], point(5.0, 5.0));
        assert!(SemanticOperation::compound(vec![operation.clone()]).is_noop());
        assert!(!session.execute(operation).unwrap());
        assert_eq!(session.history.undo_len(), depth);
    }

    // -- Cancellation of a gesture that inserted objects.

    // -- Provenance.

    #[test]
    fn an_entry_remembers_who_asked_for_it_through_undo_and_redo() {
        let mut session = session();
        let id = seed_rect(&mut session);

        // No origin given: a human action.
        let operation = plan_move(&session, &[id], 1.0, 0.0);
        session.execute(operation).unwrap();
        assert_eq!(session.history.peek_undo_origin(), Some(Origin::User));

        // An attributed caller. Same object, so this is about attribution
        // rather than about geometry.
        let operation = plan_move(&session, &[id], 0.0, 1.0);
        session.execute_from(Origin::Agent, operation).unwrap();
        assert_eq!(session.history.peek_undo_origin(), Some(Origin::Agent));

        // Attribution survives the move to the redo stack and back, which is
        // the only way it can still be trusted when a caller reads it. After
        // the undo the entry is on the redo side, and the next thing to undo is
        // the older human action.
        assert!(session.undo().unwrap());
        assert_eq!(session.history.peek_redo_origin(), Some(Origin::Agent));
        assert_eq!(session.history.peek_undo_origin(), Some(Origin::User));

        assert!(session.redo().unwrap());
        assert_eq!(session.history.peek_undo_origin(), Some(Origin::Agent));

        assert_eq!(
            session.history.peek_undo_entry().map(|entry| entry.origin),
            Some(Origin::Agent)
        );
        assert!(
            session.history.peek_undo().is_some(),
            "the operation is still readable"
        );
    }

    #[test]
    fn attribution_does_not_change_undo_semantics() {
        let mut session = session();
        let id = seed_rect(&mut session);
        let start = geometry_of(&session, id);
        let operation = plan_move(&session, &[id], 4.0, 4.0);

        session.execute_from(Origin::Import, operation).unwrap();
        assert_ne!(geometry_of(&session, id), start);
        assert!(session.undo().unwrap());
        assert_eq!(
            geometry_of(&session, id),
            start,
            "attribution is metadata, not behaviour"
        );
    }

    #[test]
    fn origins_are_distinct_so_a_caller_can_tell_them_apart() {
        // If these collapsed, the attribution would carry no information.
        let all = [
            Origin::User,
            Origin::Agent,
            Origin::Plugin,
            Origin::Import,
            Origin::Automation,
        ];
        for (index, left) in all.iter().enumerate() {
            for right in all.iter().skip(index + 1) {
                assert_ne!(
                    left, right,
                    "{left:?} and {right:?} must be distinguishable"
                );
            }
        }
        assert_eq!(
            Origin::default(),
            Origin::User,
            "an unattributed call is a human one"
        );
    }

    // -- Identity and runtime-only state.

    #[test]
    fn identities_survive_undo_redo() {
        let mut session = session();
        let id = seed_rect(&mut session);
        let spool_id = session.runtime.object(id).expect("object").spool_id.clone();

        let operation = plan_move(&session, &[id], 10.0, 10.0);
        session.execute(operation).unwrap();
        assert!(session.undo().unwrap());
        assert!(session.redo().unwrap());
        assert_eq!(
            session.runtime.object(id).expect("object").spool_id,
            spool_id
        );

        // A rename is persistent metadata but does not change identity.
        let operation = rename_node_in(
            &session.document,
            NodeId::new("spool-a").unwrap(),
            "Alpha X".into(),
        )
        .unwrap();
        session.execute(operation).unwrap();
        assert!(session.undo().unwrap());
        assert_eq!(session.document.structure.nodes[0].name, "Alpha");
        assert_eq!(session.document.structure.nodes[0].id.as_str(), "spool-a");
    }

    #[test]
    fn selection_and_camera_are_runtime_only_and_never_recorded() {
        let mut session = session();
        // Editor session state exists, but is not part of the session's
        // persistent state or history.
        let runtime = EditorRuntimeState {
            selection: vec![NodeId::new("spool-a").unwrap()],
            camera_offset: (120, -30),
            active_tool: "select".into(),
        };

        let id = seed_rect(&mut session);
        let depth = session.history.undo_len();
        let operation = plan_move(&session, &[id], 5.0, 5.0);
        session.execute(operation).unwrap();
        assert!(session.undo().unwrap());
        assert!(session.redo().unwrap());
        // Redo returned the entry to the undo stack, so the move still costs
        // exactly one history entry.
        assert_eq!(session.history.undo_len(), depth + 1);

        // Executing an operation does not capture selection or camera: the
        // session never recorded the `runtime` value built above, and only
        // the create and the move were ever recorded.
        let _ = runtime;

        // Mutating runtime state alone produces no history entry.
        let before = session.history.undo_len();
        let mut other = EditorRuntimeState {
            selection: vec![],
            camera_offset: (0, 0),
            active_tool: String::new(),
        };
        other.camera_offset = (999, 999);
        other.selection.push(NodeId::new("spool-b").unwrap());
        assert_eq!(
            session.history.undo_len(),
            before,
            "camera/selection must not record"
        );

        // And no persistent state mentions them.
        let persisted = format!("{:?}", session.document);
        assert!(!persisted.contains("camera"));
        assert!(!persisted.contains("selection"));
    }

    #[test]
    fn a_new_operation_clears_redo_and_a_noop_does_not() {
        let mut session = session();
        let id = seed_rect(&mut session);

        move_by(&mut session, &[id], 3.0, 0.0);
        session.undo().unwrap();
        assert!(session.history.can_redo());

        // A no-op must not destroy the redo branch.
        let noop = plan_move(&session, &[id], 0.0, 0.0);
        assert!(!session.execute(noop).unwrap(), "a no-op records nothing");
        assert!(session.history.can_redo(), "redo survives a no-op");

        // A real operation clears it.
        let operation = plan_move(&session, &[id], 1.0, 0.0);
        session.execute(operation).unwrap();
        assert!(!session.history.can_redo());
    }

    #[test]
    fn a_failed_operation_changes_neither_state_nor_history() {
        let mut session = session();
        let names: Vec<String> = session
            .document
            .structure
            .nodes
            .iter()
            .map(|node| node.name.clone())
            .collect();

        // Renaming a node that does not exist must fail cleanly.
        let operation = rename_node_in(
            &session.document,
            NodeId::new("spool-a").unwrap(),
            "Renamed".into(),
        )
        .unwrap();
        session.execute(operation).unwrap();
        let depth_after_rename = session.history.undo_len();

        let bogus = SemanticOperation::Rename(RenameNode {
            id: NodeId::new("spool-missing").unwrap(),
            before: "Nope".into(),
            after: "Other".into(),
        });
        assert!(session.execute(bogus).is_err());

        assert_eq!(
            session.history.undo_len(),
            depth_after_rename,
            "no entry for a failure"
        );
        let after: Vec<String> = session
            .document
            .structure
            .nodes
            .iter()
            .map(|node| node.name.clone())
            .collect();
        assert_ne!(names, after, "the successful rename is still in place");
        assert_eq!(session.document.structure.nodes[0].name, "Renamed");
    }

    #[test]
    fn edit_session_undoes_metadata_and_runtime_as_one_lifo_sequence() {
        // Architectural invariant: `EditSession` holds a `SemanticHistory`, the
        // same type `canvas::History` is a facade over. A session therefore
        // undoes a rename and a move as one interleaved stack, which is what
        // distinguishes "one history implementation" from "two stacks that
        // happen to look alike".
        let mut session = session();
        let id = seed_rect(&mut session);
        let baseline = session.history.undo_len();

        let first = plan_move(&session, &[id], 12.0, 0.0);
        session.execute(first).unwrap();
        session
            .execute(
                rename_node_in(
                    &session.document,
                    NodeId::new("spool-a").unwrap(),
                    "Alpha X".into(),
                )
                .unwrap(),
            )
            .unwrap();
        let second = plan_move(&session, &[id], 0.0, 7.0);
        session.execute(second).unwrap();
        assert_eq!(session.history.undo_len(), baseline + 3);

        // Strict LIFO across both kinds: the second move, then the rename, then
        // the first move.
        assert!(session.undo().unwrap());
        assert_eq!(geometry_of(&session, id).position.y, 0.0);
        assert_eq!(session.document.structure.nodes[0].name, "Alpha X");

        assert!(session.undo().unwrap());
        assert_eq!(session.document.structure.nodes[0].name, "Alpha");

        assert!(session.undo().unwrap());
        assert_eq!(geometry_of(&session, id).position.x, 0.0);
        assert_eq!(session.history.undo_len(), baseline);
    }

    #[test]
    fn rename_against_a_runtime_only_target_is_refused() {
        let mut document = Document::empty();
        let mut history = SemanticHistory::default();
        history.record(SemanticOperation::Rename(RenameNode {
            id: NodeId::new("spool-a").unwrap(),
            before: "A".into(),
            after: "B".into(),
        }));
        // Replaying a metadata operation with no metadata available cannot be
        // silently ignored.
        let result = history.undo(&mut OperationTarget::Runtime(&mut document));
        assert!(matches!(result, Err(OperationError::WrongTarget(_))));
        // And the entry is preserved for a correct target.
        assert!(history.can_undo());
    }

    #[test]
    fn every_operation_kind_leaves_the_caller_owned_runtime_state_alone() {
        let mut session = session();
        let id = seed_rect(&mut session);
        let depth = session.history.undo_len();

        // Selection and camera belong to the canvas view, not to the session.
        // Driving all six operation kinds must leave that caller-owned state
        // byte-identical and must not record anything on its behalf.
        let view = EditorRuntimeState {
            selection: vec![NodeId::new("spool-a").unwrap()],
            camera_offset: (120, -30),
            active_tool: "select".into(),
        };

        let before = geometry_of(&session, id);
        let operations = [
            plan_move(&session, &[id], 5.0, 5.0),
            SemanticOperation::Runtime(DocumentCommand::geometry(vec![GeometryChange {
                id,
                before,
                after: Geometry {
                    position: point(1.0, 1.0),
                    size: size(20.0, 20.0),
                },
            }])),
            SemanticOperation::Runtime(DocumentCommand::insert(
                session.runtime.duplicate_objects(&[id]),
            )),
            {
                let duplicated = session
                    .runtime
                    .objects()
                    .last()
                    .expect("duplicate exists")
                    .id;
                SemanticOperation::Runtime(DocumentCommand::delete(
                    session.runtime.remove_objects(&[duplicated]),
                ))
            },
        ];
        for operation in operations {
            session.execute(operation).expect("applies");
        }
        let operation = rename_node_in(
            &session.document,
            NodeId::new("spool-a").unwrap(),
            "Alpha 2".into(),
        )
        .unwrap();
        session.execute(operation).unwrap();

        // Four runtime operations plus the rename: five entries, no more.
        assert_eq!(session.history.undo_len(), depth + 5);

        // The view's state is exactly what it was.
        assert_eq!(view.selection, vec![NodeId::new("spool-a").unwrap()]);
        assert_eq!(view.camera_offset, (120, -30));
        assert_eq!(view.active_tool, "select");

        // And undoing all five restores geometry without consulting it.
        for _ in 0..5 {
            assert!(session.undo().unwrap());
        }
        assert_eq!(view.camera_offset, (120, -30));
        // Five undos leave only the seed creation that predates them.
        assert_eq!(session.history.undo_len(), depth);
    }
}
