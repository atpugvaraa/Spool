//! The production path from a project directory on disk to a document the
//! editor can render.
//!
//! # Why this module exists
//!
//! Phases A-D built every stage of this pipeline and tested each one in
//! isolation: [`ProjectBundle`] loads a project, [`reconcile`] binds persistent
//! identities to authored elements, and the engine's own compile turns the
//! result into the document the editor draws. None of them had a production
//! caller. The editor started from a hardcoded starter scene regardless of what
//! was on disk, so "Spool can open a source-backed project" was true of the
//! types and false of the program.
//!
//! This module is the one place that runs the stages in order and hands the
//! result to the editor. The shell consumes the outcome; it does not parse
//! anything, so adding a second loader would not be tempting later.
//!
//! # The path
//!
//! ```text
//! project directory
//!   -> ProjectBundle::load      (metadata validated, every binding resolved)
//!   -> source_binding::reconcile (identity confirmed against the authored parse)
//!   -> engine_project::compile   (the engine's own model, for the engine)
//! ```
//!
//! # What is authoritative and what is derived
//!
//! Authoritative: the HTML/CSS/SVG on disk, and the `lamine.yaml` metadata.
//! Neither is modified by opening a project.
//!
//! Derived and disposable: the engine's document, and the list of nodes the
//! engine has no kind for. Drop them and re-open the project and you get
//! equivalent state. Opening a project never writes anything back.
//!
//! # The retired runtime is gone
//!
//! This module used to derive a *second* model of the project — a runtime
//! document, a visual model, and a projection between the persistent ids and
//! the runtime's own handles — and hand all three to the editor alongside the
//! engine's document. The editor drew the runtime document; the engine drew
//! the engine's. Two models of one project, and the one the user could see was
//! the one the engine could not save.
//!
//! There is now one model. The engine's document is what the editor draws,
//! what the save path reads, and what a reopen reproduces. The list of nodes
//! the engine cannot represent survives because it is still a fact about the
//! project worth saying out loud — but it is answered by the engine's own
//! kinds now, not by a second parser's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::project_bundle::{BundleError, ProjectBundle};
use crate::source_binding::{self, BindingError};
use crate::source_document::PersistentDocument;

/// A project that loaded, bound, and compiled successfully.
///
/// Holds the canonical document beside the engine's document, so a caller can
/// persist metadata edits without re-reading the project.
#[derive(Clone, Debug)]
pub struct LoadedProject {
    /// The directory the project was opened from.
    pub root: PathBuf,
    /// Canonical, source-backed state. This is the only durable truth.
    pub document: PersistentDocument,
    /// The engine document compiled from the project. Derived and disposable.
    ///
    /// This is the document the editor draws, the document the save path reads
    /// and the document a reopen reproduces. There is no second model beside
    /// it.
    pub engine: crate::engine_project::EngineProject,
    /// Nodes that bound successfully but whose kind the engine does not have.
    ///
    /// The visible edge of what the editor can show. Reported rather than
    /// hidden, so a project that opens with a layer the engine cannot draw is
    /// visible as such instead of looking like the authored design.
    pub unrendered: Vec<String>,
}

/// Why a project could not be opened.
///
/// Every failure names its stage. Opening a project never degrades to an empty
/// canvas: the caller receives this error instead.
#[derive(Debug)]
pub enum ProjectOpenError {
    /// The directory, `lamine.yaml`, or a bound source file could not be
    /// read or was malformed.
    Load(BundleError),
    /// Metadata parsed, but the authored source does not agree with it.
    Binding(Vec<BindingError>),
    /// The project loaded and bound, but nothing in it can be drawn yet.
    NothingRenderable,
}

impl std::fmt::Display for ProjectOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(error) => write!(f, "could not load the project: {error}"),
            Self::Binding(errors) => {
                write!(f, "source bindings did not reconcile ({}):", errors.len())?;
                for error in errors {
                    write!(f, "\n  - {error}")?;
                }
                Ok(())
            }
            Self::NothingRenderable => write!(
                f,
                "the project bound successfully but no node has a canvas representation yet"
            ),
        }
    }
}

impl std::error::Error for ProjectOpenError {}

impl From<BundleError> for ProjectOpenError {
    fn from(error: BundleError) -> Self {
        Self::Load(error)
    }
}

/// Open a source-backed project and derive the engine document for it.
///
/// This is the single production entry point. The shell calls it; nothing else
/// loads projects.
pub fn open_project(root: impl AsRef<Path>) -> Result<LoadedProject, ProjectOpenError> {
    // 1. Metadata + authored bytes, with every binding already resolved to
    //    exactly one authored element.
    let bundle = ProjectBundle::load(root)?;
    let root = bundle.root().to_path_buf();
    // Taken before `bundle.document` is moved out below. These are the bytes
    // `load` already read -- never a re-read -- and they are where a version 2
    // lamine.yaml keeps its Owner B facts. Without them every Owner B field is
    // silently defaulted, which is exactly the failure `docs/02` forbids.
    let manifest_yaml = bundle.manifest_yaml().map(str::to_owned);
    let document = bundle.document;

    // 2. Confirm identity against the authored parse rather than trusting the
    //    metadata alone. `sources` is keyed by the same relative paths the
    //    bindings use.
    let sources: BTreeMap<String, String> = document
        .sources
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    source_binding::reconcile(&document.structure, &sources).map_err(ProjectOpenError::Binding)?;

    // 3. Compile the same project into the engine's document model. Additive
    //    and non-fatal: a project the stages above accepted must not start
    //    failing because the engine compile cannot represent something in it.
    let engine = crate::engine_project::compile_or_report(&document, manifest_yaml.as_deref());

    // 4. The nodes the engine has no kind for. Derived by asking the engine's
    //    own compile, so the answer is the engine's and not a second parser's
    //    idea of what a layer is.
    let unrendered = crate::engine_project::unrenderable_nodes(&document, manifest_yaml.as_deref());

    // 5. A project that binds but draws nothing is reported, not shown blank.
    //    "Unrendered" means the node survived into the document, but its kind
    //    is not one the engine can represent, so there is nothing to draw.
    if document.structure.nodes.len() == unrendered.len() {
        return Err(ProjectOpenError::NothingRenderable);
    }

    Ok(LoadedProject {
        root,
        document,
        engine,
        unrendered,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_document::NodeId;

    fn fixture_root(fixture: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture)
    }

    fn node(id: &str) -> NodeId {
        NodeId::new(id).expect("valid fixture id")
    }

    /// The names of the engine's own nodes, in document order.
    fn engine_names(project: &LoadedProject) -> Vec<String> {
        project
            .engine
            .document
            .nodes()
            .filter(|node| !matches!(node.kind, figcraft_doc::NodeKind::Page { .. }))
            .map(|node| node.name.clone())
            .collect()
    }

    /// Write a throwaway project into a temp directory.
    ///
    /// Used for the cases the checked-in fixtures deliberately do not cover:
    /// a binding that only exists inside a comment, and a project with nothing
    /// drawable. Building them here keeps the fixtures honest rather than
    /// adding directories whose only purpose is to satisfy one assertion.
    fn scratch_project(name: &str, yaml: &str, html: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "spool-open-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch project");
        std::fs::write(root.join("lamine.yaml"), yaml).expect("write metadata");
        std::fs::write(root.join("index.html"), html).expect("write source");
        root
    }

    #[test]
    fn a_real_project_on_disk_loads_into_a_persistent_document() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");

        // Metadata reached the persistent document intact.
        let ids: Vec<&str> = loaded
            .document
            .structure
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "spool-frame-root",
                "spool-text-headline",
                "spool-cta-primary"
            ]
        );
        // And the authored bytes came with it.
        assert!(loaded.document.sources["index.html"].contains("data-spool-id"));
        assert!(loaded.document.sources.contains_key("index.html"));
    }

    #[test]
    fn authored_identities_bind_to_the_persistent_nodes() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");
        // Every node resolved to exactly one authored element, otherwise load
        // would have failed, and the authored markup is unchanged.
        let html = &loaded.document.sources["index.html"];
        for node in &loaded.document.structure.nodes {
            let marker = format!("data-spool-id=\"{}\"", node.id.as_str());
            assert_eq!(
                html.matches(&marker).count(),
                1,
                "node {} binds to exactly one authored element",
                node.id.as_str()
            );
        }
    }

    #[test]
    fn identity_survives_the_compile_and_maps_deterministically() {
        let first = open_project(fixture_root("landing")).expect("landing opens");
        let second = open_project(fixture_root("landing")).expect("landing opens");

        // Re-opening the same project yields the same engine nodes.
        let ids =
            |p: &LoadedProject| -> Vec<u64> { p.engine.document.ids().map(|id| id.0).collect() };
        assert_eq!(ids(&first), ids(&second));

        // Every persistent node is in the engine's document, under the name the
        // metadata gave it.
        for node in &first.document.structure.nodes {
            assert!(
                !first
                    .engine
                    .document
                    .find_by_name(&node.name, None)
                    .is_empty(),
                "node {} is in the engine's document",
                node.id.as_str()
            );
        }
        assert_eq!(
            crate::engine_project::content_node_count(&first.engine.document),
            first.document.structure.nodes.len(),
            "one engine node per persistent node"
        );
        // And the headline is the node the metadata named.
        assert!(
            !first
                .engine
                .document
                .find_by_name("Headline", None)
                .is_empty(),
            "the engine holds the node the metadata named"
        );
    }

    #[test]
    fn the_engines_own_ids_stay_clear_of_the_manifests() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");

        // The engine allocates its own ids for nodes it creates, and they have
        // to be above every id the manifest uses or a created node would be
        // indistinguishable from an authored one.
        let highest = loaded
            .engine
            .document
            .ids()
            .map(|id| id.0)
            .max()
            .unwrap_or_default();
        assert!(
            loaded.engine.document.next_id().0 > highest,
            "a node the engine creates must not take an id the manifest uses"
        );
    }

    #[test]
    fn hierarchy_survives_from_metadata_into_the_engine_document() {
        let nested = open_project(fixture_root("nested")).expect("nested opens");

        // Persistent hierarchy.
        let root = &nested.document.structure.nodes[0];
        assert_eq!(root.id.as_str(), "spool-page-home");
        assert_eq!(root.parent, None);
        assert_eq!(
            root.children,
            vec![node("spool-text-title"), node("spool-image-logo")],
            "the authored child order is preserved"
        );

        // The engine's document holds the same hierarchy, for a project it can
        // compile. `nested` cannot be compiled — it declares an image — so the
        // engine's answer is asked of `landing`.
        let loaded = open_project(fixture_root("landing")).expect("landing opens");
        let document = &loaded.engine.document;
        let frame = document
            .find_by_name("Landing Frame", None)
            .first()
            .copied()
            .expect("the frame is in the engine's document");
        let children: Vec<String> = document
            .box_children(document.node(frame).expect("the frame is there"))
            .map(|(id, _)| document.node(id).expect("a child is there").name.clone())
            .collect();
        assert_eq!(
            children,
            vec!["Headline".to_string(), "Primary CTA".to_string()],
            "hierarchy order reaches the engine's document"
        );
    }

    #[test]
    fn the_engine_document_holds_the_projects_nodes() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");

        // landing declares frame, text, frame: all three are drawable.
        assert!(loaded.unrendered.is_empty(), "landing is fully renderable");
        assert_eq!(
            engine_names(&loaded),
            vec![
                "Landing Frame".to_string(),
                "Headline".to_string(),
                "Primary CTA".to_string()
            ],
            "one engine node per persistent node, in hierarchy order"
        );
    }

    #[test]
    fn a_node_the_engine_cannot_draw_is_reported_not_dropped() {
        let loaded = open_project(fixture_root("nested")).expect("nested opens");

        // `nested` declares an image, which the engine has no kind for. It must
        // survive in the document and be named in `unrendered`.
        assert!(
            loaded
                .document
                .structure
                .nodes
                .iter()
                .any(|n| n.id == node("spool-image-logo")),
            "the undrawable node is still persistent"
        );
        assert_eq!(
            loaded.unrendered,
            vec!["Logo (spool-image-logo)".to_string()],
            "the node is reported rather than silently dropped"
        );
        // The engine refused the whole compile, and said so: a document missing
        // some of its nodes would look like a project with fewer nodes.
        assert!(
            loaded.engine.unresolved[0].contains("did not compile"),
            "the refusal is reported: {:?}",
            loaded.engine.unresolved
        );
    }

    #[test]
    fn the_engine_document_is_what_the_editor_draws() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");

        // The engine's document has a page to draw on, and the project's nodes
        // on it. There is no second model beside it: the runtime document, the
        // visual model and the projection between them are gone, and what the
        // editor opens is this.
        let document = &loaded.engine.document;
        assert!(
            document.first_page().is_some(),
            "the engine's document has a page"
        );
        assert_eq!(
            crate::engine_project::content_node_count(document),
            3,
            "and the project's three nodes"
        );
    }

    #[test]
    fn a_binding_that_exists_only_in_a_comment_is_rejected() {
        // This is what the SourceIndex pass is actually for. The bundle loader
        // counts the marker as text, so a marker inside a comment looks like a
        // match; only a real parse knows there is no element there. If the
        // reconcile step were dropped, this project would open and the ghost
        // node would reach the canvas.
        let yaml = "version: 1\nnodes:\n  - id: \"spool-ghost\"\n    name: \"Ghost\"\n    kind: \"frame\"\n    parent: null\n    file: \"index.html\"\n    selector: \"[data-spool-id=\\\"spool-ghost\\\"]\"\n    children: []\n";
        let html = "<!doctype html>\n<body>\n  <!-- <div data-spool-id=\"spool-ghost\">not an element</div> -->\n</body>\n";
        let root = scratch_project("ghost", yaml, html);

        let error = open_project(&root).expect_err("a commented-out identity is not a binding");
        assert!(
            matches!(error, ProjectOpenError::Binding(_)),
            "expected a binding failure, got {error:?}"
        );
        assert!(
            format!("{error}").contains("source bindings did not reconcile"),
            "the error names the failing stage: {error}"
        );
    }

    #[test]
    fn a_project_with_nothing_drawable_is_an_error_not_a_blank_canvas() {
        // Every node binds, but no kind is one the engine can draw yet. The
        // editor must say so rather than showing its starter scene, which
        // would look like the project simply had no content.
        let yaml = "version: 1\nnodes:\n  - id: \"spool-art\"\n    name: \"Art\"\n    kind: \"image\"\n    parent: null\n    file: \"index.html\"\n    selector: \"[data-spool-id=\\\"spool-art\\\"]\"\n    children: []\n";
        let html = "<!doctype html>\n<body>\n  <img data-spool-id=\"spool-art\" src=\"a.svg\" />\n</body>\n";
        let root = scratch_project("undrawable", yaml, html);

        let error = open_project(&root).expect_err("an undrawable project must not open blank");
        assert!(matches!(error, ProjectOpenError::NothingRenderable));
        assert!(
            format!("{error}").contains("no node has a canvas representation"),
            "the error explains the real cause: {error}"
        );
    }

    #[test]
    fn a_broken_project_fails_loudly_instead_of_opening_blank() {
        // A directory with no metadata is a load failure, not an empty project.
        let missing = fixture_root("landing").join("does-not-exist");
        let error = open_project(&missing).expect_err("a missing project must fail");
        assert!(matches!(error, ProjectOpenError::Load(_)));
        assert!(
            format!("{error}").contains("could not load"),
            "the error names the stage: {error}"
        );
    }
}
