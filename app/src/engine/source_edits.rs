//! Turning the engine's document into edits against the authored source.
//!
//! # What this module is for
//!
//! Spool's save path is edit-list driven, not document driven:
//! [`crate::project_save::save_project`] takes a list of
//! [`SourceEdit`](crate::project_save::SourceEdit) and writes each one into the
//! smallest authored span that can express it. This module produces that list
//! from the *engine's* document — the one the editor actually draws and edits —
//! so the retired hand-written runtime in `canvas.rs` is no longer the thing
//! that decides what a save writes.
//!
//! ```text
//!   engine document ──this module──> Vec<SourceEdit> ──save_project──> HTML / CSS
//! ```
//!
//! # The identity rule
//!
//! One fact carries the whole module, and it is not re-derived here:
//!
//! > `spool_source::reader::build_node` calls `Node::new(record.node_id(), …)`,
//! > so the engine's [`figcraft_doc::NodeId`] **is** the manifest node id — the
//! > `id:` in `lamine.yaml` — as a `u64`.
//!
//! So the engine's id, the manifest's numeric id, the manifest's `key` (the
//! `data-spool-id` in markup) and the source binding (file plus selector) are
//! four views of one identity:
//!
//! ```text
//!   engine NodeId(7) ──> manifest record id 7 ──> key "spool-frame-hero"
//!                                            └──> binding index.html + selector
//! ```
//!
//! A node the *engine* created has a fresh id above every id the manifest
//! uses, so the manifest has no record for it. Such a node has no source
//! binding and therefore nowhere to be authored: it is reported as an
//! unsupported edit, never invented into the source. That is the old runtime's
//! rule — "reported rather than invented" — and it is kept here.
//!
//! # The baseline: what the source says
//!
//! The engine's document is compared against the *same project compiled from
//! the authored source*. That baseline is not a snapshot taken when the project
//! was opened: it is recompiled from the bytes on disk at save time, so a save
//! that has already been applied is not applied twice, and a file edited
//! outside the editor is respected.
//!
//! The baseline is compiled by [`spool_source`], the crate that owns the
//! `lamine.yaml` schema and the source-to-model compile. That matters for two
//! reasons, and both are about the cascade:
//!
//! - The reader resolves a declaration per computed longhand, so a
//!   `background` shorthand and a `background-color` declaration compete with
//!   each other and only one of them survives. A property that is *not*
//!   authored for a node is reported as unresolved for it, which is what tells
//!   this module "the source does not own this value" — the difference
//!   between rewriting a declaration and writing a local one.
//! - The reader ranks across stylesheets by specificity first and link order
//!   second, exactly as a browser does. The writer in
//!   [`crate::project_save`] resolves the *same* question with the same
//!   answer, so the declaration an edit rewrites is the one the editor is
//!   drawing.
//!
//! # Which CSS parser the write path should use
//!
//! The repository has two CSS readers with byte-range provenance:
//!
//! | | [`crate::style::Stylesheet`] | [`spool_source::css::Stylesheet`] |
//! |---|---|---|
//! | used by | the app's renderer (`visual.rs`) and the writer (`project_save.rs`) | the source-to-model compile (`spool-source`) |
//! | winning declaration | private `winner()`; the writer re-implements the shorthand grouping in `ownership_spellings` | public `winner()` — the declaration *and* the spelling it was authored as |
//! | `!important` | re-derived from the value text | recorded on the declaration when it is parsed |
//! | selector subset | a type selector may only lead | a type selector may appear anywhere, as CSS allows |
//!
//! **The `spool-source` reader is the better basis for the write path**, for
//! reasons that are not stylistic:
//!
//! 1. It is the reader the document was compiled with. The value the engine
//!    draws and the declaration an edit rewrites then come from one cascade
//!    rather than two that have to agree.
//! 2. It records `!important` as a fact on the declaration. The app's reader
//!    keeps the flag inside the value text, so a value that goes through
//!    `var()` substitution can no longer be recognised as important — and
//!    dropping the flag while rewriting a value changes which declaration
//!    wins.
//! 3. It exposes the winning declaration itself, so the writer can ask for
//!    the property *as it was authored* instead of guessing a spelling.
//!
//! The writer still uses the app's reader today. Switching it is a change to
//! [`crate::project_save`] and to [`crate::visual::owning_sheet`], which is the
//! one place the cross-stylesheet ranking lives; see the report that came with
//! this module for the exact shape of that change.
//!
//! # What is compared
//!
//! | Engine | Authored property |
//! |---|---|
//! | `node.fills` | `background-color` |
//! | `node.strokes` + `node.stroke.weight` | `border` |
//! | `node.corners.radii` | `border-radius` |
//! | `node.opacity` | `opacity` |
//! | text `style.font_size` | `font-size` |
//! | text `style.fills` | `color` |
//! | text `characters` | the element's own text |
//! | `node.width` / `node.height` | `width` / `height` |
//! | `node.transform` | `left` / `top`, or `transform: translate(…)` |
//!
//! The same set the retired runtime wrote, with one addition: a size or a
//! position the *layout* owns is not compared at all, because the layout
//! computes it and the source never said it. Writing a computed size into the
//! source would pin a box the author never sized.
//!
//! # What is not compared
//!
//! Three cases, each for its own reason:
//!
//! - **A node the engine created.** It has no manifest record, so it has no
//!   key, no file and no selector: there is nowhere to author it. It is
//!   reported, never invented.
//! - **A node whose element the source does not carry**, while the metadata
//!   does describe it. That is a created node — an identity without its
//!   authored half — and it is the one case where the save writes an element
//!   rather than changing one.
//! - **A whole project whose source does not compile.** There is no
//!   trustworthy answer about the values it holds, so nothing is compared; the
//!   nodes it does not back are still authorable, and the reason is said out
//!   loud.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use figcraft_doc::color::{Color, Paint};
use figcraft_doc::{Document, Node, NodeId, Sizing};
use spool_source::manifest::{Manifest, ManifestError, NodeRecord};
use spool_source::reader::{self, Read, ReadError};

use crate::project_bundle::{BundleError, ProjectBundle, METADATA_FILE};
use crate::project_save::{
    self, NewElement, Placement, SaveError, SaveOutcome, SourceEdit, UnsupportedEdit,
};
use crate::source_document::{
    LamineStructure, ModelError, NodeId as SourceNodeId, PersistentDocument, SourceBinding,
    StructuralNode,
};

/// How far two lengths may differ and still be the same length.
///
/// A tenth of a pixel. The engine measures in `f64` and the source rounds to
/// two decimals, so an exact comparison would report a difference where there
/// is none and write a size the author never asked for.
const LENGTH_EPSILON: f64 = 0.05;

/// How far two opacities may differ and still be the same opacity.
///
/// A thousandth. The engine keeps opacity in `f32` and the source rounds to
/// two decimals, so the same reasoning as [`LENGTH_EPSILON`] applies.
const OPACITY_EPSILON: f64 = 0.001;

/// The changes the engine's document asks for in the authored source.
#[derive(Clone, Debug, Default)]
pub struct EngineSourceEdits {
    /// The edits, in the order they were produced.
    pub edits: Vec<SourceEdit>,
    /// The changes that could not be written, with the reason.
    pub unsupported: Vec<UnsupportedEdit>,
    /// What could not be done at all, beyond the per-node edits.
    ///
    /// A project-level fact — a source that did not compile, a metadata format
    /// this editor does not write — belongs here rather than in
    /// [`Self::unsupported`], which is about nodes.
    pub notes: Vec<String>,
}

/// Why the edits could not be produced at all.
#[derive(Debug)]
pub enum EditError {
    /// The project could not be read.
    Project(BundleError),
    /// `lamine.yaml` is not a manifest this editor can read.
    Manifest {
        /// The metadata file.
        path: PathBuf,
        /// What the manifest reader said.
        source: ManifestError,
    },
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Project(error) => write!(f, "the project could not be read: {error}"),
            Self::Manifest { path, source } => write!(
                f,
                "{} is not a manifest this editor can read: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for EditError {}

impl From<EditError> for BundleError {
    fn from(error: EditError) -> Self {
        match error {
            EditError::Project(error) => error,
            EditError::Manifest { path, source } => BundleError::MalformedMetadata {
                path,
                source: ModelError::InvalidYaml(source.to_string()),
            },
        }
    }
}

/// The project the engine's document belongs to, as it stands on disk.
#[derive(Clone, Debug)]
pub struct SourceProject {
    /// The manifest: the identity authority.
    ///
    /// Read with [`spool_source::manifest::Manifest`], the crate that defines
    /// the schema, so a version 2 file is read as the version 2 file it is.
    pub manifest: Manifest,
    /// The authored files, project-root-relative path to contents.
    pub sources: BTreeMap<String, String>,
    /// The editor's own model of the project.
    pub document: PersistentDocument,
}

/// Read the project the engine's document belongs to.
///
/// Everything is read once, from disk: the metadata through the crate that
/// defines it, and the authored files through the bundle that already resolves
/// every binding to exactly one element.
pub fn read_project(root: &Path) -> Result<SourceProject, EditError> {
    let bundle = ProjectBundle::load(root).map_err(EditError::Project)?;
    let metadata = bundle.manifest_yaml().ok_or_else(|| {
        EditError::Project(BundleError::MissingMetadata {
            root: root.to_path_buf(),
        })
    })?;
    let manifest = Manifest::from_yaml(metadata).map_err(|source| EditError::Manifest {
        path: root.join(METADATA_FILE),
        source,
    })?;
    let sources = bundle
        .document
        .sources
        .iter()
        .map(|(path, contents)| (path.clone(), contents.clone()))
        .collect();
    Ok(SourceProject {
        manifest,
        sources,
        document: bundle.document,
    })
}

/// The edits that write `live` back to the project in `root`.
///
/// The whole save in one call: read the project, work out what changed, and
/// write it. Returns what was written and what could not be written, so a
/// caller never has to guess whether an edit survived.
pub fn save_engine_document(root: &Path, live: &Document) -> Result<SaveOutcome, SaveError> {
    let project = read_project(root).map_err(|error| SaveError::from(BundleError::from(error)))?;
    let mut edits = edits(live, &project);
    let document = persistent_document(live, &project);
    let mut outcome = project_save::save_project(root, &document, &edits.edits)?;
    outcome.unsupported.append(&mut edits.unsupported);
    outcome.notes.append(&mut edits.notes);
    Ok(outcome)
}

/// The edits that turn the authored source into the engine's document.
///
/// `live` is the engine's document as the user has it; `project` is the
/// project it belongs to, read from disk.
pub fn edits(live: &Document, project: &SourceProject) -> EngineSourceEdits {
    let mut out = EngineSourceEdits::default();

    // A document that was never compiled has nothing to compare against, and
    // every node the manifest names would look like a node that left the
    // document. Reporting that as a removal would delete the project.
    if live.pages.is_empty() {
        out.notes.push(
            "the engine's document holds no page, so it was never compiled from this \
             project and nothing was written"
                .to_owned(),
        );
        return out;
    }

    let baseline = compile_baseline(&project.manifest, &project.sources);
    let read = match baseline {
        Baseline::Compiled(read) => read,
        Baseline::Broken { unbound, reason } => {
            // The source does not compile, so there is no trustworthy answer
            // about the values it holds and nothing is compared. The nodes it
            // does not back are still authorable: they have an identity in the
            // metadata and no element, which is exactly what a created node
            // is.
            out.notes.push(format!(
                "the authored source did not compile, so only the nodes it does not back \
                 were considered: {reason}"
            ));
            let mut comparison = Comparison {
                live,
                base: live,
                read: None,
                project,
                out: &mut out,
            };
            for record in &project.manifest.nodes {
                if !unbound.contains(&record.id) {
                    continue;
                }
                let Some(live_node) = live.node(NodeId(record.id)) else {
                    continue;
                };
                comparison.created(record, live_node);
            }
            return out;
        }
    };

    let mut comparison = Comparison {
        live,
        base: &read.document,
        read: Some(&read),
        project,
        out: &mut out,
    };

    for record in &project.manifest.nodes {
        let id = NodeId(record.id);
        let Some(live_node) = live.node(id) else {
            // The node was in the project and is no longer in the document: it
            // has to leave the source too, or a reopen brings it back.
            //
            // Only if the source still has it. A node the *compile* would not
            // accept is in neither document, and taking its element out would
            // delete a layer the editor never had.
            if read.document.node(id).is_some() {
                comparison.removed(record);
            } else {
                comparison.report(
                    record,
                    "node",
                    "the engine's model would not accept this node, so it is in neither \
                     document and its element was left alone",
                );
            }
            continue;
        };
        comparison.compared(record, live_node);
    }

    // A node the engine created has no manifest record, and therefore no
    // source binding and nowhere to be authored.
    for id in live.ids() {
        let Some(node) = live.node(id) else {
            continue;
        };
        if matches!(node.kind, figcraft_doc::NodeKind::Page { .. }) {
            continue;
        }
        if project.manifest.node(id).is_some() {
            continue;
        }
        comparison.unbound(id);
    }

    out
}

/// What the authored source says about the project.
enum Baseline {
    /// The source compiled: every node has an answer, and the answers are
    /// trustworthy.
    ///
    /// Boxed because the compile is a whole document and the other variant is
    /// two small fields.
    Compiled(Box<Read>),
    /// The source did not compile.
    ///
    /// The nodes whose binding does not resolve are still known, and they are
    /// the ones that can be authored: they have an identity in the metadata
    /// and no element. Nothing else is compared, because a baseline built from
    /// a project that does not compile is not a trustworthy answer about the
    /// values the source holds.
    Broken {
        /// The manifest ids the source does not back.
        unbound: Vec<u64>,
        /// Why the source did not compile.
        reason: String,
    },
}

/// Compile the authored source, tolerating a node the source does not back.
///
/// A binding that does not resolve to exactly one element is a node the
/// source does not describe. Dropping it and retrying finds the others, so
/// one stale binding does not stop the rest of the project from being
/// written.
fn compile_baseline(manifest: &Manifest, sources: &BTreeMap<String, String>) -> Baseline {
    match reader::read_with_diagnostics(manifest, sources) {
        Ok(read) => Baseline::Compiled(Box::new(read)),
        Err(error) => {
            let reason = error.to_string();
            let mut working = manifest.clone();
            let mut unbound = Vec::new();
            loop {
                match reader::read_with_diagnostics(&working, sources) {
                    Ok(_) => break,
                    Err(ReadError::UnresolvedBinding { id, .. })
                    | Err(ReadError::AmbiguousBinding { id, .. }) => {
                        if !drop_record(&mut working, id) {
                            break;
                        }
                        unbound.push(id);
                    }
                    Err(_) => break,
                }
            }
            Baseline::Broken { unbound, reason }
        }
    }
}

/// Drop a node from a manifest, and the edges that named it.
///
/// The children of a dropped node are re-parented onto its own parent, so the
/// manifest stays a tree the reader can compile.
fn drop_record(manifest: &mut Manifest, id: u64) -> bool {
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

/// The persistent document the engine's document describes.
///
/// The engine's document is the truth about what the project contains, so the
/// structure projected here is the engine's hierarchy, with the identity and
/// the source binding each node keeps from the manifest.
///
/// The order is the manifest's, and the kind is the manifest's: both are
/// spelled by the metadata, and reproducing them exactly is what lets a save
/// notice that nothing structural changed and leave `lamine.yaml` alone.
pub fn persistent_document(live: &Document, project: &SourceProject) -> PersistentDocument {
    let mut nodes = Vec::new();
    for record in &project.manifest.nodes {
        let Some(node) = live.node(NodeId(record.id)) else {
            continue;
        };
        let Some(id) = source_id(record) else {
            continue;
        };
        let parent = node
            .parent
            .and_then(|parent| project.manifest.node(parent))
            .and_then(source_id);
        let children = node
            .children
            .iter()
            .filter_map(|child| project.manifest.node(*child))
            .filter_map(source_id)
            .collect();
        nodes.push(StructuralNode {
            id,
            name: if node.name.trim().is_empty() {
                record.name.clone()
            } else {
                node.name.clone()
            },
            kind: record.kind.clone(),
            parent,
            children,
            source: SourceBinding {
                file: record.binding.file.clone(),
                selector: record.binding.selector.clone(),
            },
        });
    }
    PersistentDocument {
        structure: LamineStructure { nodes },
        sources: project.document.sources.clone(),
    }
}

/// The editor's node id for a manifest record: the authored key.
fn source_id(record: &NodeRecord) -> Option<SourceNodeId> {
    SourceNodeId::new(record.key.clone()).ok()
}

/// The comparison of one project, between the engine's document and the
/// source it was compiled from.
struct Comparison<'a> {
    /// The engine's document, as the user has it.
    live: &'a Document,
    /// The same project, compiled from the authored source.
    base: &'a Document,
    /// The compile, with the diagnostics that say what the source did not own.
    ///
    /// `None` when the source did not compile at all: then there is no
    /// baseline, and nothing is compared.
    read: Option<&'a Read>,
    /// The project the engine's document belongs to.
    project: &'a SourceProject,
    /// What the comparison has found so far.
    out: &'a mut EngineSourceEdits,
}

impl Comparison<'_> {
    /// A node that left the document takes its authored element with it.
    fn removed(&mut self, record: &NodeRecord) {
        let Some(id) = source_id(record) else {
            return;
        };
        self.out.edits.push(SourceEdit::Remove {
            node: id,
            file: record.binding.file.clone(),
        });
    }

    /// A node the engine created, which the manifest does not describe.
    ///
    /// Reported, not invented: without a manifest record there is no key, no
    /// file and no selector, and a save that guessed them would put an element
    /// in the source that nothing binds to.
    fn unbound(&mut self, id: NodeId) {
        let label = format!("engine-{}", id.0);
        let node = match SourceNodeId::new(label) {
            Ok(node) => node,
            Err(error) => {
                self.out.notes.push(format!(
                    "the engine's node {} could not be reported: {error}",
                    id.0
                ));
                return;
            }
        };
        self.out.unsupported.push(UnsupportedEdit {
            node,
            kind: "object",
            reason: "not in lamine.yaml, so it has no source binding and nowhere to be \
                     authored"
                .to_owned(),
        });
    }

    /// A node the manifest describes and the editor holds, but whose element
    /// the source does not carry.
    ///
    /// It has an identity — a key, a file and a selector, all in the metadata
    /// — and no authored half. That is what a created node is: the identity
    /// exists, and the element is what the identity has to bind to.
    ///
    /// The element is authored complete, the way the retired runtime authored
    /// one: a created box has no authored position for the flow to discover,
    /// so it is taken out of the flow and given coordinates from where the
    /// editor put it, and it is given the size and the appearance the editor
    /// is showing, so that it looks the same after a reopen.
    fn created(&mut self, record: &NodeRecord, live_node: &Node) {
        let Some(key) = source_id(record) else {
            self.report(
                record,
                "create",
                "its authored key is not a legal node id, so it cannot be addressed in \
                 the source",
            );
            return;
        };
        let at = figcraft_geom::translation(live_node.transform);
        let mut declarations = vec![
            ("position".to_owned(), "absolute".to_owned()),
            (
                "left".to_owned(),
                format!("{}px", project_save::css_length(at.x as f32)),
            ),
            (
                "top".to_owned(),
                format!("{}px", project_save::css_length(at.y as f32)),
            ),
            (
                "width".to_owned(),
                format!("{}px", project_save::css_length(live_node.width as f32)),
            ),
            (
                "height".to_owned(),
                format!("{}px", project_save::css_length(live_node.height as f32)),
            ),
        ];
        if let Some(color) = solid(live_node.fills.first()) {
            declarations.push(("background-color".to_owned(), css_color(color)));
        }
        if let Some(radius) = live_node.corners.radii.first() {
            declarations.push((
                "border-radius".to_owned(),
                format!("{}px", project_save::css_length(*radius as f32)),
            ));
        }
        let text = live_node
            .kind
            .text()
            .map(|text| text.characters.clone())
            .filter(|text| !text.is_empty());
        self.out.edits.push(SourceEdit::Create {
            node: key,
            element: NewElement {
                tag: "div".to_owned(),
                text,
                declarations,
            },
        });
    }

    /// Everything that changed about one node.
    fn compared(&mut self, record: &NodeRecord, live_node: &Node) {
        let id = NodeId(record.id);
        let Some(base_node) = self.base.node(id) else {
            self.report(
                record,
                "node",
                "the authored source does not describe this node, so there is nothing to \
                 compare it against",
            );
            return;
        };
        let Some(key) = source_id(record) else {
            self.report(
                record,
                "node",
                "its authored key is not a legal node id, so it cannot be addressed in \
                 the source",
            );
            return;
        };
        self.styles(&key, live_node, base_node);
        self.text(&key, live_node, base_node);
        self.geometry(&key, record, live_node, base_node);
    }

    /// A change that could not be written, named after the node it is about.
    fn report(&mut self, record: &NodeRecord, kind: &'static str, reason: &str) {
        let node = match source_id(record) {
            Some(node) => node,
            None => {
                self.out.notes.push(format!(
                    "node {} could not be reported: its authored key is not a legal node id",
                    record.id
                ));
                return;
            }
        };
        self.out.unsupported.push(UnsupportedEdit {
            node,
            kind,
            reason: reason.to_owned(),
        });
    }

    /// The appearance that changed, one property at a time.
    ///
    /// Only what actually changed becomes an edit: writing an unchanged fill
    /// back would replace a declaration the author wrote with one the editor
    /// chose, and the two differ in more than their bytes.
    fn styles(&mut self, key: &SourceNodeId, live_node: &Node, base_node: &Node) {
        let live_fill = solid(live_node.fills.first());
        let base_fill = solid(base_node.fills.first());
        if live_fill != base_fill {
            if let Some(color) = live_fill {
                self.style(key, "background-color", &css_color(color));
            }
        }

        let live_stroke = stroke(live_node);
        let base_stroke = stroke(base_node);
        if live_stroke != base_stroke {
            if let Some((weight, color)) = live_stroke {
                self.style(
                    key,
                    "border",
                    &format!(
                        "{}px solid {}",
                        project_save::css_length(weight as f32),
                        css_color(color)
                    ),
                );
            }
        }

        if (live_node.opacity as f64 - base_node.opacity as f64).abs() > OPACITY_EPSILON {
            self.style(key, "opacity", &project_save::css_length(live_node.opacity));
        }

        if live_node.corners.radii != base_node.corners.radii {
            self.style(key, "border-radius", &radii_value(live_node.corners.radii));
        }

        let (Some(live_text), Some(base_text)) = (live_node.kind.text(), base_node.kind.text())
        else {
            return;
        };
        if (live_text.style.font_size - base_text.style.font_size).abs() > LENGTH_EPSILON {
            self.style(
                key,
                "font-size",
                &format!(
                    "{}px",
                    project_save::css_length(live_text.style.font_size as f32)
                ),
            );
        }
        let live_color = text_color(live_text);
        let base_color = text_color(base_text);
        if live_color != base_color {
            if let Some(color) = live_color {
                self.style(key, "color", &css_color(color));
            }
        }
    }

    /// The text that changed.
    fn text(&mut self, key: &SourceNodeId, live_node: &Node, base_node: &Node) {
        let (Some(live_text), Some(base_text)) = (live_node.kind.text(), base_node.kind.text())
        else {
            return;
        };
        if live_text.characters == base_text.characters {
            return;
        }
        // An empty text is not written: the element owns a single run of text
        // or it owns none, and clearing one is a change the source subset
        // cannot express without destroying what the author wrote around it.
        if live_text.characters.is_empty() {
            return;
        }
        self.out.edits.push(SourceEdit::Text {
            node: key.clone(),
            text: live_text.characters.clone(),
        });
    }

    /// The size and the position that changed.
    ///
    /// A size or a position the *layout* owns is not compared at all: the
    /// layout computes it, the source never said it, and writing a computed
    /// value into the source would pin a box the author never sized.
    fn geometry(
        &mut self,
        key: &SourceNodeId,
        record: &NodeRecord,
        live_node: &Node,
        base_node: &Node,
    ) {
        let id = NodeId(record.id);
        let mut width = None;
        let mut height = None;
        if !self.size_is_layout_owned(live_node) {
            if self.source_owns(id, "width")
                && (live_node.width - base_node.width).abs() > LENGTH_EPSILON
            {
                width = Some(live_node.width as f32);
            }
            if self.source_owns(id, "height")
                && (live_node.height - base_node.height).abs() > LENGTH_EPSILON
            {
                height = Some(live_node.height as f32);
            }
        }

        let mut placement = None;
        if !self.position_is_layout_owned(live_node) {
            let live_at = figcraft_geom::translation(live_node.transform);
            let base_at = figcraft_geom::translation(base_node.transform);
            let dx = live_at.x - base_at.x;
            let dy = live_at.y - base_at.y;
            if dx.abs() > LENGTH_EPSILON || dy.abs() > LENGTH_EPSILON {
                placement = Some(self.placement(key, live_at, dx, dy));
            }
        }

        if width.is_none() && height.is_none() && placement.is_none() {
            return;
        }
        self.out.edits.push(SourceEdit::Geometry {
            node: key.clone(),
            placement,
            width,
            height,
        });
    }

    /// How a moved element's new position is expressed in source.
    ///
    /// The two cases have different honest answers, and the writer decides
    /// which one to write: an element already out of the flow is written as
    /// `left`/`top` from its containing block, and one in the flow is written
    /// as a `transform: translate(…)` that composes with what the author
    /// already wrote.
    fn placement(
        &self,
        key: &SourceNodeId,
        live_at: figcraft_geom::Point,
        dx: f64,
        dy: f64,
    ) -> Placement {
        let out_of_flow = self
            .project
            .document
            .structure
            .nodes
            .iter()
            .find(|node| &node.id == key)
            .and_then(|node| project_save::is_out_of_flow(&self.project.document, node).ok())
            .unwrap_or(false);
        if out_of_flow {
            Placement::ContainingBlock {
                x: live_at.x as f32,
                y: live_at.y as f32,
            }
        } else {
            Placement::Flow {
                dx: dx as f32,
                dy: dy as f32,
            }
        }
    }

    /// Whether the source owns a property for this node.
    ///
    /// The reader reports every property it could not determine, so a property
    /// it did not report is one the source stated. A property the source did
    /// not state has no authored owner, and a write to it becomes a local
    /// declaration rather than a change to a rule somebody else shares.
    fn source_owns(&self, id: NodeId, property: &str) -> bool {
        self.read.is_some_and(|read| {
            !read
                .unresolved_for(id)
                .any(|unresolved| unresolved.property == property)
        })
    }

    /// Whether the layout, rather than the source, decides this node's size.
    fn size_is_layout_owned(&self, node: &Node) -> bool {
        if node.child_layout.sizing_h != Sizing::Fixed
            || node.child_layout.sizing_v != Sizing::Fixed
        {
            // A node that hugs or fills gets its size from the layout.
            return true;
        }
        self.in_auto_layout_flow(node)
    }

    /// Whether the layout, rather than the source, decides this node's place.
    fn position_is_layout_owned(&self, node: &Node) -> bool {
        self.in_auto_layout_flow(node)
    }

    /// Whether the node is placed by an auto-layout frame.
    fn in_auto_layout_flow(&self, node: &Node) -> bool {
        if node.child_layout.absolute {
            return false;
        }
        node.parent
            .and_then(|parent| self.live.node(parent))
            .is_some_and(|parent| parent.layout.is_some() && parent.kind.is_frame_like())
    }

    /// One style edit.
    fn style(&mut self, key: &SourceNodeId, property: &str, value: &str) {
        self.out.edits.push(SourceEdit::Style {
            node: key.clone(),
            property: property.to_owned(),
            value: value.to_owned(),
        });
    }
}

/// The solid colour of a paint, if it is one.
fn solid(paint: Option<&Paint>) -> Option<Color> {
    paint.and_then(Paint::solid_color)
}

/// The stroke of a node: its weight and its colour, if it has one.
fn stroke(node: &Node) -> Option<(f64, Color)> {
    solid(node.strokes.first()).map(|color| (node.stroke.weight, color))
}

/// The colour a text node's own style paints with.
fn text_color(text: &figcraft_doc::TextData) -> Option<Color> {
    solid(text.style.fills.as_ref().and_then(|fills| fills.first()))
}

/// A colour as the author would write it.
fn css_color(color: Color) -> String {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.r),
        channel(color.g),
        channel(color.b)
    )
}

/// Corner radii as the author would write them.
fn radii_value(radii: [f64; 4]) -> String {
    let lengths = |value: f64| project_save::css_length(value as f32);
    if radii[0] == radii[1] && radii[1] == radii[2] && radii[2] == radii[3] {
        return format!("{}px", lengths(radii[0]));
    }
    format!(
        "{}px {}px {}px {}px",
        lengths(radii[0]),
        lengths(radii[1]),
        lengths(radii[2]),
        lengths(radii[3])
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use figcraft_doc::color::{Color, Paint};
    use figcraft_doc::NodeKind;

    /// A committed fixture, copied to a scratch directory so it can be written to.
    fn scratch(fixture: &str) -> PathBuf {
        let from = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture);
        let to = std::env::temp_dir().join(format!(
            "spool-source-edits-{fixture}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&to);
        copy_tree(&from, &to).expect("copy fixture");
        to
    }

    fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let target = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_tree(&entry.path(), &target)?;
            } else {
                std::fs::copy(entry.path(), &target)?;
            }
        }
        Ok(())
    }

    /// The project, and the document the authored source compiles into.
    fn project_and_document(root: &Path) -> (SourceProject, Document) {
        let project = read_project(root).expect("the project reads");
        let read = reader::read_with_diagnostics(&project.manifest, &project.sources)
            .expect("the source compiles");
        (project, read.document)
    }

    /// The engine id of the node with a given authored key.
    fn engine_id(project: &SourceProject, key: &str) -> NodeId {
        project
            .manifest
            .node_by_key(key)
            .expect("the manifest describes the node")
            .node_id()
    }

    /// The style edits for one node, as `(property, value)` pairs.
    fn styles_for(edits: &EngineSourceEdits, key: &str) -> Vec<(String, String)> {
        edits
            .edits
            .iter()
            .filter_map(|edit| match edit {
                SourceEdit::Style {
                    node,
                    property,
                    value,
                } if node.as_str() == key => Some((property.clone(), value.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_engine_id_is_the_manifest_node_id() {
        // The identity rule this module is built on: `spool-source` builds a
        // node with the manifest's own id, so the engine's id and the
        // manifest's id are the same number.
        let root = scratch("cascade");
        let (project, document) = project_and_document(&root);
        let record = project
            .manifest
            .node_by_key("spool-frame-hero")
            .expect("the hero is described");
        assert_eq!(record.id, 1);
        assert!(
            document.node(NodeId(1)).is_some(),
            "the engine's node 1 is the manifest's node 1"
        );
        assert_eq!(
            document.node(NodeId(1)).map(|node| node.name.clone()),
            Some("Hero".to_owned())
        );
    }

    #[test]
    fn an_untouched_project_asks_for_nothing() {
        let root = scratch("cascade");
        let (project, document) = project_and_document(&root);
        let edits = edits(&document, &project);
        assert!(
            edits.edits.is_empty(),
            "a project nobody edited produces no edits: {:?}",
            edits.edits
        );
        assert!(edits.unsupported.is_empty(), "{:?}", edits.unsupported);
    }

    #[test]
    fn a_changed_fill_asks_for_the_property_that_changed() {
        let root = scratch("cascade");
        let (project, mut document) = project_and_document(&root);
        let hero = engine_id(&project, "spool-frame-hero");
        let node = document.node_mut(hero).expect("the hero is there");
        node.fills = vec![Paint::solid(Color::from_rgb8_const(0xff, 0x00, 0x00))];

        let edits = edits(&document, &project);
        assert_eq!(
            styles_for(&edits, "spool-frame-hero"),
            vec![("background-color".to_owned(), "#ff0000".to_owned())],
            "one property changed, so one edit: {:?}",
            edits.edits
        );
    }

    #[test]
    fn the_change_lands_in_the_declaration_that_won_the_cascade() {
        // The round trip. `#hero` (base.css, an id) and `.theme` (theme.css, a
        // class) both declare a background for the hero, and the id wins. The
        // edit has to land in the declaration that won, or the file on disk
        // would go on naming a different owner for the value than the editor
        // is drawing.
        let root = scratch("cascade");
        let (project, mut document) = project_and_document(&root);
        let hero = engine_id(&project, "spool-frame-hero");
        document.node_mut(hero).expect("the hero is there").fills =
            vec![Paint::solid(Color::from_rgb8_const(0x00, 0x66, 0x33))];

        let outcome = save_engine_document(&root, &document).expect("the save succeeds");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);
        assert!(outcome.notes.is_empty(), "{:?}", outcome.notes);

        let base = std::fs::read_to_string(root.join("base.css")).expect("base.css");
        let theme = std::fs::read_to_string(root.join("theme.css")).expect("theme.css");
        assert!(
            base.contains("background: #006633;"),
            "the winning declaration was rewritten: {base}"
        );
        assert!(
            base.contains("background: #ffffff;"),
            "the other rule in the same file was left alone: {base}"
        );
        assert!(
            theme.contains("background: #0000ff;"),
            "the losing declaration was left alone: {theme}"
        );
        assert_eq!(
            theme,
            std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("fixtures")
                    .join("cascade")
                    .join("theme.css")
            )
            .expect("the committed fixture"),
            "the losing stylesheet is byte-identical"
        );
    }

    #[test]
    fn a_second_save_writes_nothing() {
        // The baseline is the source as it stands now, so a save that has
        // already been applied is not applied a second time.
        let root = scratch("cascade");
        let (project, mut document) = project_and_document(&root);
        let hero = engine_id(&project, "spool-frame-hero");
        document.node_mut(hero).expect("the hero is there").fills =
            vec![Paint::solid(Color::from_rgb8_const(0x00, 0x66, 0x33))];

        save_engine_document(&root, &document).expect("the first save succeeds");
        let outcome = save_engine_document(&root, &document).expect("the second save succeeds");
        assert!(
            outcome.written.is_empty(),
            "nothing changed, so nothing was written: {:?}",
            outcome.written
        );
    }

    #[test]
    fn a_node_the_engine_created_is_reported_rather_than_invented() {
        let root = scratch("cascade");
        let (project, mut document) = project_and_document(&root);
        // A node the engine made for itself: a fresh id above every id the
        // manifest uses, and no manifest record behind it.
        let created = document
            .create(
                document.current_page.expect("a page"),
                NodeKind::Rectangle,
                "Rectangle 1",
                10.0,
                10.0,
                20.0,
                20.0,
            )
            .expect("the engine creates the node");
        assert!(
            project.manifest.node(created).is_none(),
            "the manifest does not describe it"
        );

        let edits = edits(&document, &project);
        assert!(
            edits.edits.is_empty(),
            "a node with nowhere to go produces no edit: {:?}",
            edits.edits
        );
        assert_eq!(edits.unsupported.len(), 1, "{:?}", edits.unsupported);
        assert_eq!(edits.unsupported[0].kind, "object");
        assert!(
            edits.unsupported[0].reason.contains("no source binding"),
            "{}",
            edits.unsupported[0].reason
        );
    }

    #[test]
    fn a_node_that_left_the_document_takes_its_element_with_it() {
        let root = scratch("cascade");
        let (project, mut document) = project_and_document(&root);
        let swatch = engine_id(&project, "spool-frame-swatch-a");
        remove_node(&mut document, swatch);

        let edits = edits(&document, &project);
        let removals: Vec<&SourceEdit> = edits
            .edits
            .iter()
            .filter(|edit| matches!(edit, SourceEdit::Remove { .. }))
            .collect();
        assert_eq!(removals.len(), 1, "{:?}", edits.edits);
        match removals[0] {
            SourceEdit::Remove { node, file } => {
                assert_eq!(node.as_str(), "spool-frame-swatch-a");
                assert_eq!(file, "index.html");
            }
            other => panic!("expected a removal, got {other:?}"),
        }
    }

    /// Take a node out of the document, the way the engine's delete does.
    fn remove_node(document: &mut Document, id: NodeId) {
        document.remove(id).expect("the node is in the document");
    }

    #[test]
    fn a_text_change_asks_for_the_text() {
        let root = scratch("cascade");
        let (project, mut document) = project_and_document(&root);
        let note = engine_id(&project, "spool-frame-note");
        let node = document.node_mut(note).expect("the note is there");
        match &mut node.kind {
            NodeKind::Text(text) => text.characters = "Changed".to_owned(),
            other => panic!("the note is a text node, not {other:?}"),
        }

        let edits = edits(&document, &project);
        let texts: Vec<&SourceEdit> = edits
            .edits
            .iter()
            .filter(|edit| matches!(edit, SourceEdit::Text { .. }))
            .collect();
        assert_eq!(texts.len(), 1, "{:?}", edits.edits);
        match texts[0] {
            SourceEdit::Text { node, text } => {
                assert_eq!(node.as_str(), "spool-frame-note");
                assert_eq!(text, "Changed");
            }
            other => panic!("expected a text edit, got {other:?}"),
        }
    }

    #[test]
    fn a_node_the_metadata_describes_and_the_source_does_not_carry_is_authored() {
        // The identity is in the metadata and the element is not in the
        // source: that is a created node, and it is the one case where the
        // save has to write an element rather than change one.
        let root = scratch("cascade");
        let (project, document) = project_and_document(&root);
        let mut manifest = project.manifest.clone();
        let record = manifest
            .nodes
            .iter_mut()
            .find(|record| record.key == "spool-frame-swatch-a")
            .expect("the swatch is described");
        record.binding.selector = "[data-spool-id=\"not-in-the-source\"]".to_owned();
        let mut project = project;
        project.manifest = manifest;

        let edits = edits(&document, &project);
        let creates: Vec<&SourceEdit> = edits
            .edits
            .iter()
            .filter(|edit| matches!(edit, SourceEdit::Create { .. }))
            .collect();
        assert_eq!(creates.len(), 1, "{:?}", edits.edits);
        match creates[0] {
            SourceEdit::Create { node, element } => {
                assert_eq!(node.as_str(), "spool-frame-swatch-a");
                assert_eq!(element.tag, "div");
                let width = element
                    .declarations
                    .iter()
                    .find(|(property, _)| property == "width")
                    .map(|(_, value)| value.clone());
                assert_eq!(width.as_deref(), Some("200px"), "{:?}", element);
            }
            other => panic!("expected a creation, got {other:?}"),
        }
    }

    #[test]
    fn the_projected_document_is_the_document_on_disk() {
        // The projection has to reproduce the structure exactly, or a save
        // would think the structure changed and rewrite the metadata.
        let root = scratch("cascade");
        let (project, document) = project_and_document(&root);
        let projected = persistent_document(&document, &project);
        assert_eq!(
            projected.structure, project.document.structure,
            "an untouched project projects onto itself"
        );
    }
}
