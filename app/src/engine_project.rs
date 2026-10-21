//! Compiling an opened project into the engine's document model.
//!
//! Spool's premise is that the authored project — HTML/CSS/SVG plus
//! `lamine.yaml` — is the truth and the engine document is disposable. This
//! module is where that claim is cashed in on open: it takes the app's
//! [`PersistentDocument`] and produces a [`figcraft_doc::Document`] by calling
//! [`spool_source`], which owns both the `lamine.yaml` schema and the
//! source-to-model compile.
//!
//! ```text
//! PersistentDocument -> spool_source::Manifest -> spool_source::reader -> Document
//! ```
//!
//! # Why this module is a bridge and not an implementation
//!
//! The app's [`crate::source_document`] keeps its own older parallel model:
//! string node ids, six fields per node, and a hand-parsed `lamine.yaml` v1.
//! That model stays where it is — it is what the editor runtime, the layers
//! panel and the save path all speak today, and replacing it is not this
//! change's job.
//!
//! So the conversion happens here, in one direction, and everything about it is
//! additive: the compiled document is *produced*, not *installed*. Nothing in
//! the editor reads it yet, so no existing behaviour can depend on it.
//!
//! # Two readers of `lamine.yaml`, and which one this module uses
//!
//! The repository has two of them, and choosing between them is what this module
//! is for:
//!
//! - [`LamineStructure::from_yaml`] is the app's own. It understands version 1
//!   and refuses everything else, because it is a hand parser over a contract
//!   with six fields per node. It is what the runtime, the layers panel and the
//!   save path speak.
//! - [`Manifest::from_yaml`] is [`spool_source`]'s, and it is the one that
//!   defines the schema. It understands version 1 *and* version 2. Version 2 is
//!   where Owner B lives: constraints, sizing, masks, locks, dev status, grids,
//!   guides, exports, interactions, components, variable bindings, plugin data.
//!
//! When the raw metadata text is available, it is parsed here by the crate that
//! defines the schema, so a version 2 project compiles with its Owner B intact.
//! The text is the bytes [`ProjectBundle`] already read, never a second read of
//! the file: two reads of one file is two answers to what the author wrote.
//!
//! When no text is available, the manifest is rebuilt from the app's structure,
//! which is then the only description of the project in hand. That fallback
//! carries identity, hierarchy and the source binding and nothing else: every
//! Owner B field stays at its default, because inventing one would be a lie.
//!
//! # Who decides what, when both sources speak
//!
//! Owner B comes from the metadata text. Identity, hierarchy and the source
//! binding come from the open document, because that is what the editor loaded
//! and the only thing that knows about an edit which has not been saved. So a
//! rename in the editor reaches the compiled document *and* the node keeps the
//! constraints, lock, dev status and plugin data it was authored with.
//!
//! The two can disagree, and neither disagreement is resolved by guessing: a node
//! the document holds and the metadata does not is compiled with no Owner B and
//! says so in [`EngineProject::unresolved`]; a record the document does not hold
//! is not compiled and also says so.
//!
//! # The two ids
//!
//! The app's [`crate::source_document::NodeId`] is a string; the engine's
//! [`figcraft_doc::NodeId`] is a `u64`. Neither is derived from the other —
//! `spool-source`'s crate docs explain why that separation is the whole point.
//! With no metadata text, the model id is allocated from 1 in document order,
//! exactly as `Manifest::from_yaml` does when it upgrades a v1 file. With it, the
//! authored id is kept as written — it is the identity other Owner B fields refer
//! to, so renumbering it would silently repoint them — and only a node the
//! metadata does not describe is given a fresh id above every one in use.
//!
//! # Ownership
//!
//! Geometry, paint and text are never read out of `lamine.yaml`: they come from
//! the authored CSS and markup, resolved by `spool-source`. A property with two
//! owners is a defect, so nothing Owner A owns is duplicated into the manifest by
//! this module.
//!
//! # Failure
//!
//! [`compile`] fails honestly — it never returns a half-built document as if it
//! were the project. [`compile_or_report`] is the open-path wrapper: a project
//! the old path can still open must keep opening, so the failure becomes an
//! empty document plus a readable line in `unresolved`.

use std::collections::{BTreeMap, HashMap};

use figcraft_doc::Document;
use spool_source::manifest::{Manifest, ManifestError, NodeRecord, SourceBinding};
use spool_source::reader::{self, ReadError};

use crate::project_bundle::{ProjectBundle, METADATA_FILE};
use crate::source_document::{
    LamineStructure, ModelError, NodeId as SourceNodeId, PersistentDocument,
    SourceBinding as NodeSource, StructuralNode,
};

/// The name carried by the placeholder document a failed compile reports.
///
/// The manifest carries no document name and one is not an authored property,
/// so there is nothing to read: the compile either produces `spool-source`'s own
/// `"Spool"` or, failing that, this label on an empty document.
const FAILED_DOCUMENT_NAME: &str = "Spool";

/// Why a project could not be compiled into an engine document.
///
/// Nothing here is repaired or approximated. Every variant is a refusal, and
/// both underlying errors are refusals too — they are wrapped rather than
/// reimplemented so the reason a project did not compile reaches the caller
/// exactly as `spool-source` worded it.
#[derive(Debug)]
pub enum EngineProjectError {
    /// The app's structure could not be expressed as a compilable manifest.
    Manifest(ManifestError),
    /// The manifest is compilable but the authored source is not.
    Read(ReadError),
}

impl std::fmt::Display for EngineProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest(error) => write!(
                f,
                "the project's structure is not a compilable lamine.yaml manifest: {error}"
            ),
            Self::Read(error) => write!(
                f,
                "the authored source did not compile into an engine document: {error}"
            ),
        }
    }
}

impl std::error::Error for EngineProjectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Manifest(error) => Some(error),
            Self::Read(error) => Some(error),
        }
    }
}

impl From<ManifestError> for EngineProjectError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<ReadError> for EngineProjectError {
    fn from(error: ReadError) -> Self {
        Self::Read(error)
    }
}

/// An engine document compiled from a project, plus what the source did not say.
///
/// Both fields are derived and disposable. Drop this struct and re-open the
/// project and you get an equivalent one: nothing here is written back.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineProject {
    /// The compiled document. Rebuildable from the project at any time.
    pub document: Document,
    /// Every value the document holds that the project never authored, as one
    /// readable sentence each.
    ///
    /// A value the source does not determine leaves the engine's own default in
    /// place *and* appears here, because a default the author never wrote is a
    /// lie the caller has to be able to see. A failed compile is reported here
    /// too — see [`compile_or_report`].
    pub unresolved: Vec<String>,
    /// The engine's node id for every persistent key in the project.
    ///
    /// The engine's node id is the manifest's numeric id and the persistent key
    /// is the manifest's `key`, so the two are related only through the
    /// manifest. This is that relation, kept, so a caller does not have to
    /// rebuild it — and cannot get it subtly wrong.
    pub keys: BTreeMap<String, figcraft_doc::NodeId>,
}

impl EngineProject {
    /// The engine's node for a persistent key.
    ///
    /// `None` when the project does not hold that key, or when the compile
    /// failed and the document holds nothing at all.
    ///
    /// Read by the save path's tests, which ask the engine's document what a
    /// node looks like rather than keeping a second model of the project.
    #[allow(dead_code, reason = "the save path's round-trip checks read it")]
    pub fn node_for_key(&self, key: &str) -> Option<&figcraft_doc::Node> {
        self.keys.get(key).and_then(|id| self.document.node(*id))
    }
}

/// Compile a persistent document into the engine's document model.
///
/// The three steps, in order: build a [`Manifest`], hand it and the authored
/// bytes to [`spool_source::reader::read_with_diagnostics`], and flatten the
/// returned [`spool_source::reader::Read`] into this crate's shape.
///
/// With no metadata text in hand — see [`compile_with_metadata`] — the manifest
/// is rebuilt from the document's structure, so this is the version 1 path: it
/// describes identity, hierarchy, binding and the layer name, and every Owner B
/// field stays at its default.
///
/// Fails rather than approximating. A duplicate id, a dangling parent, a
/// binding that matches nothing in the markup and a kind the engine does not
/// have are all returned as errors; none of them becomes a default.
pub fn compile(persistent: &PersistentDocument) -> Result<EngineProject, EngineProjectError> {
    compile_with_metadata(persistent, None)
}

/// Compile a document, preferring the metadata text it was loaded from.
///
/// `metadata` is the `lamine.yaml` bytes [`ProjectBundle`] read, verbatim. When
/// it is `Some`, [`Manifest::from_yaml`] parses it — so a version 2 file is read
/// by the crate that defines the schema, every Owner B field it carries reaches
/// the compiled node, and [`Manifest::validate`] runs over the real file rather
/// than over a structure this module built.
///
/// `None` is the honest fallback and not a silent downgrade: without the bytes
/// there is nothing that could parse a version 2 file, so the structure is all
/// the project is described by.
pub fn compile_with_metadata(
    persistent: &PersistentDocument,
    metadata: Option<&str>,
) -> Result<EngineProject, EngineProjectError> {
    let (manifest, notes) = to_manifest(persistent, metadata)?;
    // `spool-source` keys sources by the same project-root-relative paths the
    // bindings use, so the app's map converts without renaming anything. It
    // wants a `BTreeMap` so the compile reads files in a stable order.
    let sources: BTreeMap<String, String> = persistent
        .sources
        .iter()
        .map(|(path, contents)| (path.clone(), contents.clone()))
        .collect();
    let read = reader::read_with_diagnostics(&manifest, &sources)?;
    Ok(EngineProject {
        document: read.document,
        // What this module had to say about the manifest comes first, so a
        // reader sees why a value is missing before the reader's own list of
        // values the authored source never stated.
        unresolved: notes
            .into_iter()
            .chain(
                read.unresolved
                    .iter()
                    .map(spool_source::reader::Unresolved::to_string),
            )
            .collect(),
        keys: manifest
            .nodes
            .iter()
            .map(|record| (record.key.clone(), figcraft_doc::NodeId(record.id)))
            .collect(),
    })
}

/// Compile the project a bundle was loaded from, metadata text and all.
///
/// The one call a caller that already holds a [`ProjectBundle`] should make: the
/// bundle read `lamine.yaml` once, and this hands those exact bytes to the parser
/// that defines the schema instead of rebuilding a manifest from the structure
/// and losing every Owner B field the file carried.
///
/// Not called by [`crate::project_open::open_project`] yet, which moves the
/// document out of the bundle before compiling it and so cannot reach the text.
/// That is a gap in the open path, not in this function: `open_project` keeps
/// using [`compile_or_report`], and nothing reads an engine document yet, so no
/// behaviour depends on the difference. Closing the gap means changing the open
/// path, which is a separate change.
#[allow(dead_code, reason = "wired for the open path; see the note above")]
pub fn compile_bundle(bundle: &ProjectBundle) -> Result<EngineProject, EngineProjectError> {
    compile_with_metadata(&bundle.document, bundle.manifest_yaml())
}

/// Compile a persistent document, or report why it could not be compiled.
///
/// A project the pre-engine path can still open must keep opening, so a
/// compile failure is never fatal here. The failure is not hidden either: the
/// result is an empty document and the message in
/// [`EngineProject::unresolved`], which is the one field on this type a reader
/// already has to look at before trusting the document beside it.
///
/// What this deliberately does *not* do is publish a partial document. A
/// document missing some of its nodes looks like a project with fewer nodes,
/// which is a different lie from a project that did not compile at all.
pub fn compile_or_report(persistent: &PersistentDocument, metadata: Option<&str>) -> EngineProject {
    match compile_with_metadata(persistent, metadata) {
        Ok(compiled) => compiled,
        Err(error) => EngineProject {
            document: Document::without_pages(FAILED_DOCUMENT_NAME),
            unresolved: vec![format!("the engine document did not compile: {error}")],
            keys: BTreeMap::new(),
        },
    }
}

/// The nodes of a project the engine has no kind for.
///
/// A probe, not a document. The published engine document is all-or-nothing —
/// see [`compile_or_report`] — and this answers a different question: which of
/// this project's nodes the engine cannot represent at all, because their kind
/// is not one it has.
///
/// The answer is found by compiling and, when the compile refuses a kind,
/// dropping that one record and trying again. Nothing here publishes a partial
/// document: the document the editor draws is the one [`compile_or_report`]
/// produced, and this only says which nodes are missing from it.
///
/// This is the engine-backed answer to a question the retired hand-written
/// runtime answered with its own drawable kinds. The names are the same shape
/// the old answer used — `name (key)` — so a reader sees the same thing.
pub fn unrenderable_nodes(persistent: &PersistentDocument, metadata: Option<&str>) -> Vec<String> {
    let Ok((mut manifest, _)) = to_manifest(persistent, metadata) else {
        return Vec::new();
    };
    // `spool-source` keys sources by the same project-root-relative paths the
    // bindings use, so the app's map converts without renaming anything.
    let sources: BTreeMap<String, String> = persistent
        .sources
        .iter()
        .map(|(path, contents)| (path.clone(), contents.clone()))
        .collect();
    let mut unrendered = Vec::new();
    loop {
        match reader::read_with_diagnostics(&manifest, &sources) {
            // The whole project compiles: nothing is missing from it.
            Ok(_) => break,
            Err(ReadError::UnknownKind { id, .. }) => {
                let Some((name, key)) = manifest
                    .nodes
                    .iter()
                    .find(|record| record.id == id)
                    .map(|record| (record.name.clone(), record.key.clone()))
                else {
                    break;
                };
                unrendered.push(format!("{name} ({key})"));
                if !drop_record(&mut manifest, id) {
                    break;
                }
            }
            // Any other refusal says nothing about kinds, so it says nothing
            // about which nodes the engine can represent.
            Err(_) => break,
        }
    }
    unrendered
}

/// The nodes of a compiled document that are not its page.
///
/// The engine's document holds one page per project alongside the project's
/// own nodes, so the count of what the editor can actually draw is this and
/// not [`Document::node_count`].
pub fn content_node_count(document: &Document) -> usize {
    document
        .nodes()
        .filter(|node| !matches!(node.kind, figcraft_doc::NodeKind::Page { .. }))
        .count()
}

/// Drop a node from a manifest, and the edges that named it.
///
/// The children of a dropped node are re-parented onto its own parent, so the
/// manifest stays a tree the reader can compile.
pub(crate) fn drop_record(manifest: &mut Manifest, id: u64) -> bool {
    let Some(index) = manifest.nodes.iter().position(|record| record.id == id) else {
        return false;
    };
    let record = manifest.nodes.remove(index);
    for other in &mut manifest.nodes {
        if other.parent == Some(id) {
            other.parent = record.parent;
        }
        other.children.retain(|child| *child != id);
    }
    true
}

/// The manifest to compile, plus what had to be said to get it.
///
/// Validated before returning, so a project that cannot become a compilable
/// manifest is refused here rather than downstream with a less specific
/// complaint. The notes are the divergences between the two sources — never
/// errors, because neither side is authoritative enough to overrule the other
/// while the project is open — and they are reported next to the document rather
/// than swallowed.
fn to_manifest(
    persistent: &PersistentDocument,
    metadata: Option<&str>,
) -> Result<(Manifest, Vec<String>), EngineProjectError> {
    let (manifest, notes) = match metadata {
        Some(text) => {
            let authored = Manifest::from_yaml(text)?;
            overlay_structure(authored, &persistent.structure)
        }
        None => (manifest_from_structure(&persistent.structure), Vec::new()),
    };
    manifest.validate()?;
    Ok((manifest, notes))
}

/// Overlay the open document's identity and hierarchy onto the authored manifest.
///
/// The metadata owns Owner B; the document owns who exists, what it is called and
/// what it contains. Neither is allowed to quietly overrule the other, so this is
/// the only place the two are combined, and each divergence is named:
///
/// - a node the document holds and the metadata does not is compiled from the
///   document alone, with every Owner B field at its default, and reported —
///   otherwise a node with no constraints would be indistinguishable from a node
///   whose author wrote none;
/// - a record the document does not hold is not compiled, and is reported, since
///   an edit that removed it is the document's answer and silence would make the
///   compiled document look like a project that never had the layer.
///
/// Model ids are the metadata's own, because an Owner B field elsewhere in the
/// file can refer to one (`component`). Only a node with no authored record is
/// given a fresh id, allocated above every id in use so it cannot collide with a
/// reference that is still live.
fn overlay_structure(authored: Manifest, structure: &LamineStructure) -> (Manifest, Vec<String>) {
    let mut notes = Vec::new();
    // One map, holding only the nodes the *document* holds. A parent or child it
    // does not hold is therefore a miss even when the metadata still describes
    // it: the compiled document has the document's hierarchy, and an edge to a
    // node that was removed is not compilable.
    let mut model_ids: HashMap<&str, u64> = HashMap::new();
    let mut next_id = authored
        .nodes
        .iter()
        .map(|record| record.id)
        .max()
        .unwrap_or_default()
        .saturating_add(1);
    for node in &structure.nodes {
        let id = authored
            .node_by_key(node.id.as_str())
            .map(|record| record.id)
            .unwrap_or_else(|| {
                let id = next_id;
                next_id = next_id.saturating_add(1);
                id
            });
        model_ids.insert(node.id.as_str(), id);
    }
    // Every node the document names has an id by now, so the only miss below is a
    // malformed document naming a child it does not hold. That is reported and
    // the edge dropped, rather than becoming a manifest that fails `validate`
    // with a dangling-reference error that says less than this does.
    let edge =
        |notes: &mut Vec<String>, owner: &str, relation: &str, id: &SourceNodeId| match model_ids
            .get(id.as_str())
        {
            Some(model) => Some(*model),
            None => {
                notes.push(format!(
                    "node {owner} names {relation} {}, which the document does not \
                     describe, so that edge does not compile",
                    id.as_str()
                ));
                None
            }
        };

    let records: Vec<NodeRecord> = structure
        .nodes
        .iter()
        .map(|node| {
            let key = node.id.as_str();
            // Unconditional: every node in the structure was entered into the map
            // above, whatever the metadata held.
            let id = model_ids[key];
            let mut record = authored.node_by_key(key).cloned().unwrap_or_else(|| {
                notes.push(format!(
                    "node {key} is in the document but not in {METADATA_FILE}, \
                     so it compiles with no Owner B values"
                ));
                NodeRecord::default()
            });
            record.id = id;
            record.key = key.to_owned();
            record.name = node.name.clone();
            // Passed through verbatim, exactly as in the fallback. `spool-source`
            // normalises it when it builds the kind; normalising here would be a
            // second place where kind means something slightly different from
            // what the author wrote.
            record.kind = node.kind.clone();
            record.parent = node
                .parent
                .as_ref()
                .and_then(|parent| edge(&mut notes, key, "parent", parent));
            record.children = node
                .children
                .iter()
                .filter_map(|child| edge(&mut notes, key, "child", child))
                .collect();
            record.binding = SourceBinding {
                file: node.source.file.clone(),
                selector: node.source.selector.clone(),
            };
            record
        })
        .collect();

    for record in &authored.nodes {
        if !structure
            .nodes
            .iter()
            .any(|node| node.id.as_str() == record.key)
        {
            notes.push(format!(
                "node {} ({}) is in {METADATA_FILE} but not in the document, \
                 so it does not compile",
                record.id, record.key
            ));
        }
    }

    // Everything document-level — variables, collections, comments, the colour
    // profile — is the metadata's alone and is carried across untouched.
    (
        Manifest {
            nodes: records,
            ..authored
        },
        notes,
    )
}

/// Project a manifest onto the structure the editor speaks.
///
/// The reverse direction of [`overlay_structure`], and deliberately lossy in the
/// other direction: the editor's model has six fields per node, so Owner B
/// cannot live here. It stays in the metadata text, which is what
/// [`compile_with_metadata`] reads.
///
/// Exists because a version 2 file must still open in the editor. Refusing it
/// instead would mean Spool cannot open a project its own schema defines, and
/// the Owner B fields would be unreachable rather than merely not displayed.
pub fn structure_from_manifest(manifest: &Manifest) -> Result<LamineStructure, ModelError> {
    // Refused before projecting: a dangling parent or child would otherwise be
    // silently dropped here, and a layer vanishing is not a lossy conversion.
    manifest
        .validate()
        .map_err(|error| ModelError::InvalidOperation(error.to_string()))?;
    let key_of = |id: u64| -> Option<String> {
        manifest
            .node(figcraft_doc::NodeId(id))
            .map(|record| record.key.clone())
    };
    let structure = LamineStructure {
        nodes: manifest
            .nodes
            .iter()
            .map(|record| {
                Ok(StructuralNode {
                    id: SourceNodeId::new(record.key.clone())?,
                    name: record.name.clone(),
                    kind: record.kind.clone(),
                    parent: record
                        .parent
                        .and_then(key_of)
                        .map(SourceNodeId::new)
                        .transpose()?,
                    children: record
                        .children
                        .iter()
                        .filter_map(|child| key_of(*child))
                        .map(SourceNodeId::new)
                        .collect::<Result<Vec<SourceNodeId>, ModelError>>()?,
                    source: NodeSource {
                        file: record.binding.file.clone(),
                        selector: record.binding.selector.clone(),
                    },
                })
            })
            .collect::<Result<Vec<StructuralNode>, ModelError>>()?,
    };
    // The editor's own rules as well: unique names, non-empty bindings, and ids
    // it can actually spell.
    structure.validate()?;
    Ok(structure)
}

/// Build the manifest records for one structure.
///
/// The fallback: used when no metadata text is in hand, so it is the version 1
/// path. Model ids are allocated from 1 in document order — the same allocation
/// `Manifest::from_yaml` performs when it upgrades a version 1 file, so a
/// project converted here and a project read by `spool-source`'s own loader
/// agree on which node is which.
///
/// Split from [`to_manifest`] only so the record-building loop reads as what it
/// is: a table of one row per node, with the id translation applied to every
/// edge.
fn manifest_from_structure(structure: &LamineStructure) -> Manifest {
    // Model id per authored id. The allocation is positional, so the map is
    // built in one pass before any edge is translated.
    let model_ids: HashMap<&str, u64> = structure
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index as u64 + 1))
        .collect();
    let model_id = |id: &SourceNodeId| model_ids[id.as_str()];

    let mut manifest = Manifest {
        nodes: structure
            .nodes
            .iter()
            .map(|node| NodeRecord {
                id: model_id(&node.id),
                key: node.id.as_str().to_owned(),
                name: node.name.clone(),
                // Passed through verbatim. `spool-source` lower-cases and trims
                // it when it builds the kind, and its own container check
                // accepts the lowercase spelling the app already writes. Any
                // normalisation here would be a second place where kind means
                // something slightly different from what the author wrote.
                kind: node.kind.clone(),
                parent: node.parent.as_ref().map(model_id),
                children: node.children.iter().map(model_id).collect(),
                binding: SourceBinding {
                    file: node.source.file.clone(),
                    selector: node.source.selector.clone(),
                },
                ..NodeRecord::default()
            })
            .collect(),
        ..Manifest::default()
    };
    manifest.version = spool_source::manifest::MANIFEST_VERSION;
    manifest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_open::open_project;
    use figcraft_doc::color::ColorProfile;
    use figcraft_doc::{
        Constraints, DevStatus, HConstraint, MaskType, Node, NodeId, NodeKind, VConstraint,
    };
    use std::path::{Path, PathBuf};

    /// One of the committed fixtures, resolved against this crate.
    fn fixture_root(fixture: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture)
    }

    /// The version 2 fixture, loaded as a bundle so the metadata bytes are in
    /// hand — which is the whole difference between the two compile paths.
    fn owner_b_bundle() -> ProjectBundle {
        ProjectBundle::load(fixture_root("owner-b")).expect("the version 2 fixture loads")
    }

    /// The managed nodes of a compiled document, page excluded.
    ///
    /// `spool-source` hangs every node off a synthetic page of its own, which is
    /// not a node the project authored and so is not counted here.
    fn managed(document: &Document) -> Vec<&Node> {
        let page = document
            .current_page
            .expect("a compiled document has a page");
        document
            .subtree(page)
            .into_iter()
            .filter_map(|id| document.node(id))
            .filter(|node| !matches!(node.kind, NodeKind::Page { .. }))
            .collect()
    }

    fn names(document: &Document) -> Vec<&str> {
        managed(document)
            .into_iter()
            .map(|node| node.name.as_str())
            .collect()
    }

    fn node_named<'a>(document: &'a Document, name: &str) -> &'a Node {
        managed(document)
            .into_iter()
            .find(|node| node.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "no node named {name:?}; the document holds {:?}",
                    names(document)
                )
            })
    }

    /// A project written to a temp directory, for the cases the checked-in
    /// fixtures deliberately do not cover.
    ///
    /// Same reasoning as `project_open`'s scratch projects: a stylesheet that
    /// authors something the fixtures do not is test data, not a reason to grow
    /// a committed fixture.
    fn scratch_project(name: &str, yaml: &str, html: &str, css: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "spool-engine-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch project");
        std::fs::write(root.join("lamine.yaml"), yaml).expect("write metadata");
        std::fs::write(root.join("index.html"), html).expect("write markup");
        std::fs::write(root.join("styles.css"), css).expect("write stylesheet");
        root
    }

    fn single_frame_project(css: &str) -> std::path::PathBuf {
        let yaml = "version: 1\nnodes:\n  - id: \"spool-frame-root\"\n    name: \"Root\"\n    kind: \"frame\"\n    parent: null\n    file: \"index.html\"\n    selector: \"[data-spool-id=\\\"spool-frame-root\\\"]\"\n    children: []\n";
        let html = "<!doctype html>\n<html>\n  <head>\n    <link rel=\"stylesheet\" href=\"styles.css\" />\n  </head>\n  <body>\n    <main data-spool-id=\"spool-frame-root\">content</main>\n  </body>\n</html>\n";
        scratch_project("frame", yaml, html, css)
    }

    #[test]
    fn the_committed_landing_fixture_compiles_into_an_engine_document() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");
        let compiled = compile(&loaded.document).expect("landing compiles");

        // The compiled document has a page, and it is the current one.
        let page = compiled
            .document
            .current_page
            .expect("the compile produces a page");
        assert_eq!(compiled.document.pages, vec![page]);
        assert!(matches!(
            compiled.document.node(page).map(|node| &node.kind),
            Some(NodeKind::Page { .. })
        ));

        // Every managed node is present, hanging off that page.
        let root = compiled
            .document
            .node(NodeId(1))
            .expect("model id 1 is the root");
        assert_eq!(root.name, "Landing Frame");
        assert_eq!(root.parent, Some(page));
        assert_eq!(
            root.children,
            vec![NodeId(2), NodeId(3)],
            "the authored child order is preserved"
        );
    }

    #[test]
    fn the_compiled_nodes_are_the_ones_lamine_yaml_names() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");
        let compiled = compile(&loaded.document).expect("landing compiles");

        assert_eq!(
            names(&compiled.document),
            vec!["Landing Frame", "Headline", "Primary CTA"],
            "one node per record, in the order lamine.yaml lists them"
        );
        assert_eq!(
            compiled.document.node_count(),
            4,
            "the three authored nodes plus the page they hang from"
        );

        // The app's structure and the compiled document agree node for node, by
        // name and by hierarchy — the compile changed the identity scheme and
        // nothing else.
        for node in &loaded.document.structure.nodes {
            let compiled_node = node_named(&compiled.document, &node.name);
            assert_eq!(
                compiled_node.children.len(),
                node.children.len(),
                "{} kept its child count",
                node.name
            );
        }
        assert_eq!(
            managed(&compiled.document)
                .into_iter()
                .map(|node| node.id.0)
                .collect::<Vec<_>>(),
            vec![1, 2, 3],
            "model ids are allocated from 1 in document order"
        );
    }

    #[test]
    fn geometry_comes_from_the_authored_stylesheet_and_is_never_invented() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");
        let compiled = compile(&loaded.document).expect("landing compiles");

        // `app/fixtures/landing/styles.css` authors `.cta { border-radius: 8px; }`,
        // so the CTA's corner radii are 8 on every side. That is a real value out
        // of the committed stylesheet, not a default.
        let cta = node_named(&compiled.document, "Primary CTA");
        assert_eq!(
            cta.corners.radii,
            [8.0, 8.0, 8.0, 8.0],
            "the authored corner radius reached the node"
        );

        // The same stylesheet authors no width and no height for any managed
        // node. The engine's own 0 stands and every one of those is reported,
        // rather than a size being made up to fill the gap.
        for node in managed(&compiled.document) {
            assert_eq!(node.width, 0.0, "{} has no authored width", node.name);
            assert_eq!(node.height, 0.0, "{} has no authored height", node.name);
        }
        for property in ["width", "height"] {
            assert!(
                compiled
                    .unresolved
                    .iter()
                    .any(|line| line.contains("(spool-frame-root)")
                        && line.contains(property)
                        && line.contains("not authored")),
                "the missing {property} is reported: {:?}",
                compiled.unresolved
            );
        }
    }

    #[test]
    fn an_authored_size_reaches_the_node_and_nothing_else_does() {
        // `landing` cannot answer this on its own: it authors no sizes. A
        // stylesheet that does is the honest place to prove the value that CSS
        // determines lands on the node unchanged.
        let css = "main { width: 640px; height: 480px; }\n";
        let root = single_frame_project(css);
        let loaded = open_project(&root).expect("the scratch project opens");
        let compiled = compile(&loaded.document).expect("the scratch project compiles");

        let node = node_named(&compiled.document, "Root");
        assert_eq!(node.width, 640.0, "the authored width, in px");
        assert_eq!(node.height, 480.0, "the authored height, in px");

        // And nothing was invented for it: a property the stylesheet did not
        // author is still reported as missing rather than guessed.
        assert!(
            compiled
                .unresolved
                .iter()
                .any(|line| line.contains("opacity")),
            "an unauthored property is still reported: {:?}",
            compiled.unresolved
        );
        assert!(
            !compiled
                .unresolved
                .iter()
                .any(|line| line.contains("width") || line.contains("height")),
            "the authored sizes are not reported as missing: {:?}",
            compiled.unresolved
        );
    }

    #[test]
    fn a_text_node_carries_the_characters_the_author_wrote() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");
        let compiled = compile(&loaded.document).expect("landing compiles");

        // `<h1 data-spool-id="spool-text-headline">Design in source, structure in
        // Spool</h1>` — the compiled node holds those words and no others.
        let headline = node_named(&compiled.document, "Headline");
        let text = headline
            .kind
            .text()
            .expect("a text node compiles as a text node");
        assert_eq!(text.characters, "Design in source, structure in Spool");

        // The characters came from the markup, not from the layer name: the
        // frame's own text is the CTA's, because a managed child's content is
        // left to that child.
        let root = node_named(&compiled.document, "Landing Frame");
        assert!(
            root.kind.text().is_none(),
            "the frame is a frame, not a text node"
        );
    }

    #[test]
    fn unresolved_reads_as_a_sentence_naming_the_node_and_the_property() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");
        let compiled = compile(&loaded.document).expect("landing compiles");

        assert!(
            !compiled.unresolved.is_empty(),
            "a project that authors nothing for most properties says so"
        );
        // Each entry names the node's engine id, its authored key, the property
        // and why it is unresolved: everything needed to fix it.
        let entry = compiled
            .unresolved
            .iter()
            .find(|line| line.contains("spool-cta-primary") && line.contains("width"))
            .unwrap_or_else(|| panic!("no width report: {:?}", compiled.unresolved));
        assert!(
            entry.starts_with("node 3 (spool-cta-primary) has no readable width: "),
            "the report is a sentence, not a code: {entry}"
        );
    }

    #[test]
    fn a_value_css_states_that_the_compiler_cannot_read_is_reported_not_guessed() {
        // `100%` is a length in CSS and has no px value the engine can hold.
        // Dropping it would make the document look as if the author had not
        // written it; guessing one would make the document disagree with the
        // project. It is reported, with the authored text quoted back.
        let css = "main { width: 100%; }\n";
        let root = single_frame_project(css);
        let loaded = open_project(&root).expect("the scratch project opens");
        let compiled = compile(&loaded.document).expect("a bad length is a report, not a failure");

        assert_eq!(node_named(&compiled.document, "Root").width, 0.0);
        let entry = compiled
            .unresolved
            .iter()
            .find(|line| line.contains("width"))
            .unwrap_or_else(|| panic!("no width report: {:?}", compiled.unresolved));
        assert_eq!(
            entry,
            "node 1 (spool-frame-root) has no readable width: \"100%\" is not a length in px"
        );
    }

    #[test]
    fn a_kind_the_engine_does_not_have_fails_the_compile_rather_than_becoming_a_guess() {
        // `nested` declares `spool-image-logo` with kind `image`. The engine has
        // no such kind, and `kind` is Owner B — so picking the nearest one would
        // be inventing a node the author never described. Refused, by name.
        let loaded = open_project(fixture_root("nested")).expect("nested opens");
        assert_eq!(
            loaded.unrendered,
            vec!["Logo (spool-image-logo)".to_string()],
            "the node the engine has no kind for is named rather than dropped"
        );

        let error = compile(&loaded.document).expect_err("the engine compile must refuse it");
        assert!(
            matches!(
                error,
                EngineProjectError::Read(ReadError::UnknownKind { id: 3, .. })
            ),
            "expected the unknown-kind refusal, got {error:?}"
        );
        assert!(
            format!("{error}").contains("the unknown kind \"image\""),
            "and it names the kind it could not honour: {error}"
        );
    }

    #[test]
    fn a_binding_the_markup_does_not_have_never_reaches_the_engine_compile() {
        // The same refusal, one stage earlier: a node whose identity is absent
        // from the markup stops the project at load, so the engine compile is
        // never handed a binding it would have to guess at.
        let yaml = "version: 1\nnodes:\n  - id: \"spool-frame-ghost\"\n    name: \"Ghost\"\n    kind: \"frame\"\n    parent: null\n    file: \"index.html\"\n    selector: \"[data-spool-id=\\\"spool-frame-ghost\\\"]\"\n    children: []\n";
        let html = "<!doctype html>\n<html>\n  <body>\n    <main>no identity here</main>\n  </body>\n</html>\n";
        let root = scratch_project("ghost", yaml, html, "");

        let error = open_project(&root).expect_err("a ghost binding is not a binding");
        assert!(
            matches!(error, crate::project_open::ProjectOpenError::Load(_)),
            "expected the load to refuse it, got {error:?}"
        );
    }

    #[test]
    fn a_failed_compile_is_reported_beside_an_empty_document_rather_than_fatal() {
        // `nested` is the case that makes this real: a committed project the
        // editor opens, which the engine cannot compile.
        let loaded = open_project(fixture_root("nested")).expect("nested opens");
        assert_eq!(
            loaded.unrendered,
            vec!["Logo (spool-image-logo)".to_string()],
            "and the node it cannot represent is named"
        );

        let reported = compile_or_report(&loaded.document, None);
        assert_eq!(
            reported.document.node_count(),
            0,
            "no partial document: an absent node would look like an absent layer"
        );
        assert_eq!(reported.document.name, FAILED_DOCUMENT_NAME);
        assert!(
            reported.document.pages.is_empty(),
            "and no page to mistake for an empty project"
        );
        assert_eq!(reported.unresolved.len(), 1);
        assert!(
            reported.unresolved[0].contains("did not compile")
                && reported.unresolved[0].contains("image"),
            "the failure is stated, not hidden: {:?}",
            reported.unresolved
        );

        // A project that does compile is untouched by the wrapper.
        let ok = open_project(fixture_root("landing")).expect("landing opens");
        assert_eq!(
            compile_or_report(&ok.document, None).document.node_count(),
            4,
            "the non-fatal path returns the whole document when there is one"
        );
        // And the non-fatal decision is made on open, not only when asked:
        // `nested` came back from `open_project` with an empty engine document
        // and a stated reason rather than an error.
        let opened = open_project(fixture_root("nested")).expect("nested still opens");
        assert_eq!(opened.engine.document.node_count(), 0);
        assert!(
            opened.engine.unresolved[0].contains("did not compile"),
            "open_project reported the engine failure instead of failing: {:?}",
            opened.engine.unresolved
        );
        assert_eq!(
            opened.engine.document.node_count(),
            0,
            "and the editor is told the project holds nothing it can draw"
        );
    }

    #[test]
    fn opening_a_project_compiles_it_into_the_document_the_editor_draws() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");

        assert_eq!(
            names(&loaded.engine.document),
            vec!["Landing Frame", "Headline", "Primary CTA"],
            "opening a project produces the engine document the editor draws"
        );
        assert_eq!(content_node_count(&loaded.engine.document), 3);
        assert!(loaded.unrendered.is_empty());
        assert_eq!(
            loaded.document.structure.nodes.len(),
            3,
            "the persistent document is unchanged by the compile"
        );
    }

    #[test]
    fn model_ids_follow_document_order_and_keys_carry_the_authored_identity() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");
        let (manifest, notes) = to_manifest(&loaded.document, None).expect("landing is a manifest");
        assert!(
            notes.is_empty(),
            "the fallback rebuilds the manifest from the document, so there is \
             nothing to report: {notes:?}"
        );

        assert_eq!(manifest.nodes.len(), 3);
        for (index, record) in manifest.nodes.iter().enumerate() {
            let structure = &loaded.document.structure.nodes[index];
            assert_eq!(record.id, index as u64 + 1, "model id from 1, in order");
            assert_eq!(record.key, structure.id.as_str());
            assert_eq!(record.name, structure.name);
            assert_eq!(record.kind, structure.kind);
            assert_eq!(record.binding.file, structure.source.file);
            assert_eq!(record.binding.selector, structure.source.selector);
        }
        // Hierarchy is translated onto the model ids, not the string ids.
        let root = &manifest.nodes[0];
        assert_eq!(root.parent, None);
        assert_eq!(root.children, vec![2, 3]);
        assert_eq!(manifest.nodes[1].parent, Some(1));
        assert_eq!(manifest.nodes[2].parent, Some(1));

        // No Owner B field was invented: everything `spool-source` defines beyond
        // identity, hierarchy, name, kind and binding is still the default, because
        // a structure holds none of it and this path is the fallback.
        assert_eq!(
            manifest.nodes[0],
            NodeRecord {
                id: 1,
                key: "spool-frame-root".to_string(),
                name: "Landing Frame".to_string(),
                kind: "frame".to_string(),
                parent: None,
                children: vec![2, 3],
                binding: SourceBinding {
                    file: "index.html".to_string(),
                    selector: "[data-spool-id=\"spool-frame-root\"]".to_string(),
                },
                ..NodeRecord::default()
            }
        );
    }

    #[test]
    fn a_text_node_with_children_is_refused_before_the_engine_ever_sees_it() {
        // `Manifest::validate` rejects this, and it is the app's structure that
        // has to answer for it: a `text` kind with children describes a tree the
        // engine cannot hold. Reporting it is better than emitting a document in
        // which the child silently vanished.
        let loaded = open_project(fixture_root("landing")).expect("landing opens");
        let mut document = loaded.document.clone();
        let ghost = document.structure.nodes[2].id.clone();
        document.structure.nodes[1].children.push(ghost);

        let error = to_manifest(&document, None).expect_err("a text node cannot hold children");
        assert!(
            matches!(
                error,
                EngineProjectError::Manifest(ManifestError::NotAContainer { id: 2, .. })
            ),
            "expected the container check, got {error:?}"
        );
        assert!(
            compile(&document).is_err(),
            "and the compile refuses rather than dropping the child"
        );
    }

    #[test]
    fn an_empty_project_compiles_to_a_page_and_nothing_else() {
        // Nothing authored, nothing invented: an empty document is a valid
        // compile, not a failure.
        let compiled = compile(&PersistentDocument::default()).expect("empty compiles");
        assert_eq!(names(&compiled.document), Vec::<&str>::new());
        assert_eq!(compiled.document.node_count(), 1, "just the page");
        // And it is fully reported as unauthored rather than quietly defaulted.
        assert!(compiled.unresolved.is_empty());
    }

    #[test]
    fn compiling_the_same_project_twice_produces_the_same_document() {
        // The premise of this module: the engine document is derived. Nothing
        // about the compile may depend on when it ran.
        let loaded = open_project(fixture_root("landing")).expect("landing opens");
        let first = compile(&loaded.document).expect("first compile");
        let second = compile(&loaded.document).expect("second compile");
        assert_eq!(first, second);
    }

    #[test]
    fn a_project_nested_in_subdirectories_compiles_through_its_relative_paths() {
        // `landing.spool` binds `pages/index.html` and reaches `styles/styles.css`
        // through a `..` in the `href`. The sources map and the bindings use the
        // same project-root-relative namespace, so this has to resolve.
        let loaded = open_project(fixture_root("landing.spool")).expect("landing.spool opens");
        let compiled = compile(&loaded.document).expect("landing.spool compiles");
        assert_eq!(
            names(&compiled.document),
            vec!["Landing Frame", "Headline", "Primary CTA"]
        );
        assert!(
            compiled.document.current_page.is_some(),
            "and it has a page to hang them from"
        );
        // The stylesheet that `pages/index.html` links is present, not reported
        // missing, so nothing in the report names it.
        assert!(
            !compiled
                .unresolved
                .iter()
                .any(|line| line.contains("styles/styles.css")),
            "the linked stylesheet resolved: {:?}",
            compiled.unresolved
        );
    }

    #[test]
    fn the_fixture_root_helper_resolves_against_this_crate() {
        // Guards the tests above from silently pointing at a path that does not
        // exist: `open_project` would fail, but for the wrong reason.
        assert!(Path::new(&fixture_root("landing"))
            .join("lamine.yaml")
            .exists());
        assert!(Path::new(&fixture_root("landing"))
            .join("styles.css")
            .exists());
        assert!(Path::new(&fixture_root("owner-b"))
            .join("lamine.yaml")
            .exists());
        assert!(Path::new(&fixture_root("owner-b"))
            .join("styles.css")
            .exists());
    }

    // --- version 2 and Owner B ---------------------------------------------

    #[test]
    fn a_version_two_manifest_lands_its_owner_b_on_the_compiled_node() {
        let compiled = compile_bundle(&owner_b_bundle()).expect("a version 2 project compiles");

        // `app/fixtures/owner-b/lamine.yaml` constraints the release frame to the
        // centre horizontally and the bottom vertically, marks it ready for
        // handoff, and carries two plugin entries. None of these has a CSS
        // spelling; all three are Owner B and all three reach the node.
        let release = node_named(&compiled.document, "Release Frame");
        assert_eq!(release.constraints.horizontal, HConstraint::Center);
        assert_eq!(release.constraints.vertical, VConstraint::Bottom);
        assert_eq!(release.dev_status, DevStatus::ReadyForDev);
        assert_eq!(
            release.plugin_data.get("spool.agent").map(String::as_str),
            Some("release-owner")
        );
        assert_eq!(
            release.plugin_data.get("spool.ticket").map(String::as_str),
            Some("SPOOL-412")
        );
        assert!(
            !release.locked,
            "the metadata did not lock this node, and an absent lock is not a lock"
        );

        // A different node, a different set: the values are per record, not
        // applied uniformly by the compile.
        let headline = node_named(&compiled.document, "Release Headline");
        assert!(headline.locked);
        assert_eq!(headline.dev_status, DevStatus::Completed);
        assert_eq!(headline.constraints.horizontal, HConstraint::LeftRight);
        assert_eq!(headline.constraints.vertical, VConstraint::Center);
        assert!(
            headline.plugin_data.is_empty(),
            "the release frame's plugin data did not spread"
        );

        // Masks are Owner B as well: `overflow: hidden` is CSS, what hides what
        // is not.
        let badge = node_named(&compiled.document, "Status Badge");
        assert!(badge.is_mask);
        assert_eq!(badge.mask_type, MaskType::Luminance);

        // Document-level Owner B is the manifest's alone and survives the compile.
        assert_eq!(compiled.document.color_profile, ColorProfile::DisplayP3);

        assert!(
            compiled
                .unresolved
                .iter()
                .all(|line| !line.contains("lamine.yaml")),
            "nothing about the manifest had to be reported: {:?}",
            compiled.unresolved
        );
    }

    #[test]
    fn the_owner_b_values_come_from_the_metadata_and_nowhere_else() {
        // The same document, compiled without the metadata bytes. If this one
        // still produced a locked node, the previous test would be proving
        // nothing about the metadata.
        let bundle = owner_b_bundle();
        let compiled = compile(&bundle.document).expect("the fallback compiles the same project");

        assert_eq!(
            names(&compiled.document),
            vec!["Release Frame", "Release Headline", "Status Badge"],
            "and it is the same project: same nodes, same order"
        );
        let headline = node_named(&compiled.document, "Release Headline");
        assert!(!headline.locked);
        assert_eq!(headline.dev_status, DevStatus::None);
        assert_eq!(headline.constraints, Constraints::default());
        assert!(headline.plugin_data.is_empty());
        let badge = node_named(&compiled.document, "Status Badge");
        assert!(!badge.is_mask);
        assert_eq!(badge.mask_type, MaskType::Alpha);
        assert_eq!(
            compiled.document.color_profile,
            ColorProfile::Srgb,
            "the document's own default stands"
        );
    }

    #[test]
    fn owner_a_still_comes_from_the_stylesheet_and_is_not_written_into_the_metadata() {
        let bundle = owner_b_bundle();
        let compiled = compile_bundle(&bundle).expect("compiles");

        // `styles.css` authors `main { width: 640px; height: 480px; }` and a 12px
        // corner radius. Those reach the node from the stylesheet, as they always
        // have.
        let release = node_named(&compiled.document, "Release Frame");
        assert_eq!(release.width, 640.0);
        assert_eq!(release.height, 480.0);
        assert_eq!(release.corners.radii, [12.0, 12.0, 12.0, 12.0]);

        // And the manifest holds none of them. A property with two owners is a
        // defect, so the check that matters is the one on the metadata: the one
        // Owner B field that looks geometric is corner *smoothing*, and the
        // authored radii are absent.
        let text = bundle
            .manifest_yaml()
            .expect("the bundle kept the metadata bytes");
        let manifest = Manifest::from_yaml(text).expect("the metadata parses as a manifest");
        assert_eq!(manifest.nodes.len(), 3);
        let record = manifest
            .node_by_key("spool-frame-release")
            .expect("the frame is described");
        assert!(
            record.corners.is_none(),
            "a radius belongs to CSS; lamine.yaml may only carry the smoothing"
        );
    }

    #[test]
    fn an_unsaved_edit_compiles_with_the_owner_b_the_metadata_carried() {
        let mut bundle = owner_b_bundle();
        bundle.document.structure.nodes[1].name = "Release headline".into();

        let compiled = compile_bundle(&bundle).expect("compiles");
        let headline = node_named(&compiled.document, "Release headline");
        assert!(
            headline.locked && headline.dev_status == DevStatus::Completed,
            "the rename came from the document and the lock from the metadata"
        );
        assert!(
            !names(&compiled.document).contains(&"Release Headline"),
            "and the name the document no longer holds is not still there"
        );
    }

    #[test]
    fn a_record_the_document_does_not_hold_is_reported_and_not_compiled() {
        let mut bundle = owner_b_bundle();
        // Removed the way the editor removes a node: off its parent's children
        // too, so the only thing left inconsistent is the metadata.
        bundle.document.structure.nodes.pop();
        let badge = SourceNodeId::new("spool-frame-badge").expect("a valid fixture id");
        bundle.document.structure.nodes[0]
            .children
            .retain(|child| *child != badge);

        let compiled = compile_bundle(&bundle).expect("the rest still compiles");
        assert_eq!(
            names(&compiled.document),
            vec!["Release Frame", "Release Headline"]
        );
        let note = compiled
            .unresolved
            .iter()
            .find(|line| line.contains("spool-frame-badge"))
            .unwrap_or_else(|| panic!("no report: {:?}", compiled.unresolved));
        assert!(
            note.contains("is in lamine.yaml but not in the document"),
            "the missing record is named, not dropped quietly: {note}"
        );
    }

    #[test]
    fn a_node_the_metadata_does_not_describe_compiles_without_owner_b_and_says_so() {
        // The reverse divergence: the editor holds a layer the file has no record
        // of — a node added since the last save. It has to compile, because a
        // layer the user can see must not vanish from the derived document, and it
        // has to be reported, because a node with no constraints looks exactly
        // like a node whose author wrote none.
        let yaml = "version: 1\nnodes:\n  - id: \"spool-frame-root\"\n    name: \"Root\"\n    kind: \"frame\"\n    parent: null\n    file: \"index.html\"\n    selector: \"[data-spool-id=\\\"spool-frame-root\\\"]\"\n    children: []\n";
        let html = "<!doctype html>\n<html>\n  <body>\n    <main data-spool-id=\"spool-frame-root\">\n      <p data-spool-id=\"spool-text-added\">added in the editor</p>\n    </main>\n  </body>\n</html>\n";
        let root = scratch_project("added", yaml, html, "");
        let mut bundle = ProjectBundle::load(&root).expect("the scratch project loads");
        let added = SourceNodeId::new("spool-text-added").expect("a valid id");
        bundle.document.structure.nodes.push(StructuralNode {
            id: added.clone(),
            name: "Added".into(),
            kind: "text".into(),
            parent: Some(bundle.document.structure.nodes[0].id.clone()),
            children: Vec::new(),
            source: NodeSource {
                file: "index.html".into(),
                selector: "[data-spool-id=\"spool-text-added\"]".into(),
            },
        });
        bundle.document.structure.nodes[0].children.push(added);

        let compiled = compile_bundle(&bundle).expect("an added layer still compiles");
        let node = node_named(&compiled.document, "Added");
        assert_eq!(
            node.kind.text().map(|text| text.characters.as_str()),
            Some("added in the editor"),
            "and it is bound to the element the editor named"
        );
        assert_eq!(node.constraints, Constraints::default());
        let note = compiled
            .unresolved
            .iter()
            .find(|line| line.contains("spool-text-added"))
            .unwrap_or_else(|| panic!("no report: {:?}", compiled.unresolved));
        assert!(
            note.contains("is in the document but not in lamine.yaml"),
            "the missing record is named: {note}"
        );
    }

    #[test]
    fn an_edge_the_document_does_not_describe_is_reported_rather_than_a_panic() {
        // A document naming a child it does not hold is malformed, and the old
        // path answers that by indexing a map that has no entry for it. The
        // manifest path has to say so instead.
        let mut bundle = owner_b_bundle();
        bundle.document.structure.nodes.pop();
        let compiled = compile_bundle(&bundle).expect("the rest still compiles");
        let note = compiled
            .unresolved
            .iter()
            .find(|line| line.contains("child spool-frame-badge"))
            .unwrap_or_else(|| panic!("no report: {:?}", compiled.unresolved));
        assert!(
            note.contains("does not describe"),
            "the dangling edge is named: {note}"
        );
    }

    #[test]
    fn a_version_two_project_opens_in_the_editor_and_still_draws() {
        let opened = open_project(fixture_root("owner-b")).expect("a version 2 project opens");

        assert_eq!(
            content_node_count(&opened.engine.document),
            3,
            "all three nodes drew"
        );
        assert!(opened.unrendered.is_empty());
        assert_eq!(opened.document.structure.nodes.len(), 3);
        // The persistent document is the six fields the editor has always held.
        // Owner B did not move into it, because a property with two owners is a
        // defect: the values are in the metadata and nowhere else.
        assert!(
            opened.document.sources.contains_key("styles.css"),
            "authored source is still what the document carries"
        );
        assert!(opened.engine.document.current_page.is_some());
    }

    // --- the version 1 fixtures must not move -------------------------------

    #[test]
    fn every_version_one_fixture_compiles_the_same_with_and_without_its_metadata_text() {
        let mut compiled_count = 0;
        for fixture in ["landing", "nested", "landing.spool"] {
            let bundle = ProjectBundle::load(fixture_root(fixture)).expect("fixture loads");
            match (compile(&bundle.document), compile_bundle(&bundle)) {
                (Ok(without), Ok(with)) => {
                    compiled_count += 1;
                    assert_eq!(
                        without, with,
                        "{fixture} compiles to the same document either way"
                    );
                }
                (Err(without), Err(with)) => assert_eq!(
                    without.to_string(),
                    with.to_string(),
                    "{fixture} is refused the same way either way"
                ),
                (without, with) => panic!(
                    "{fixture}: the metadata text changed the outcome — {without:?} vs {with:?}"
                ),
            }
        }
        // Two of the three compile and one does not, for the same reason as
        // before: `nested` declares a node of kind `image`, which the engine has
        // no node kind for. Pinned so this test cannot pass by every fixture
        // failing.
        assert_eq!(compiled_count, 2, "landing and landing.spool compile");

        // `awkward` cannot be compared this way at all: its markup spells the
        // identity with single quotes and spaces around `=`, which the strict
        // loader has always refused. It is a `source_binding` fixture, not a
        // loadable project — and it is still refused, which is the behaviour that
        // matters here.
        assert!(
            matches!(
                ProjectBundle::load(fixture_root("awkward")),
                Err(crate::project_bundle::BundleError::UnresolvedBinding { .. })
            ),
            "awkward is still a project the loader refuses, as it always was"
        );
    }

    #[test]
    fn the_open_path_produces_the_same_engine_document_it_always_did() {
        // `open_project` compiles from the document alone — it cannot reach the
        // metadata text — and that must stay exactly what it produced before this
        // change existed.
        let opened = open_project(fixture_root("landing")).expect("landing opens");
        let bundle = ProjectBundle::load(fixture_root("landing")).expect("landing loads");
        let with_text = compile_bundle(&bundle).expect("landing compiles");
        assert_eq!(
            with_text.document, opened.engine.document,
            "the metadata text changes nothing for a version 1 project"
        );
    }

    /// The one that matters end to end. `open_project` used to move
    /// `bundle.document` out before compiling, which discarded the raw metadata
    /// and silently defaulted every Owner B field. This asserts the real open
    /// path, not the compile helper, so that cannot come back unnoticed.
    #[test]
    fn owner_b_survives_the_real_open_path() {
        let loaded = open_project(fixture_root("owner-b")).expect("a v2 project opens");

        let frame = loaded
            .engine
            .document
            .node(NodeId(1))
            .expect("the frame is in the document");
        assert_eq!(frame.name, "Release Frame");
        assert_eq!(
            frame.constraints.horizontal,
            HConstraint::Center,
            "Owner B: resize behaviour"
        );
        assert_eq!(frame.constraints.vertical, VConstraint::Bottom);
        assert_eq!(
            frame.dev_status,
            DevStatus::ReadyForDev,
            "Owner B: handoff status has no CSS spelling"
        );
        assert_eq!(
            frame.plugin_data.get("spool.agent").map(String::as_str),
            Some("release-owner"),
            "Owner B: free-form agent data reaches the node"
        );

        let headline = loaded
            .engine
            .document
            .node(NodeId(2))
            .expect("the headline is in the document");
        assert!(
            headline.locked,
            "Owner B: a locked layer is still locked after the compile"
        );
        assert_eq!(headline.constraints.horizontal, HConstraint::LeftRight);

        let badge = loaded
            .engine
            .document
            .node(NodeId(3))
            .expect("the badge is in the document");
        assert!(badge.is_mask, "Owner B: what masks what is not CSS");
        assert_eq!(badge.mask_type, MaskType::Luminance);

        assert_eq!(
            loaded.engine.document.color_profile,
            ColorProfile::DisplayP3,
            "document-level Owner B reaches the document"
        );

        // Owner A still comes from CSS, not the metadata. The fixture authors
        // 640x480 and a 12px radius in styles.css and repeats neither.
        assert_eq!(frame.width, 640.0);
        assert_eq!(frame.height, 480.0);
        assert_eq!(frame.corners.radii, [12.0; 4]);
    }
}
