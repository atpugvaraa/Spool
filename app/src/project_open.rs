//! The production path from a project directory on disk to a document the
//! editor can render.
//!
//! # Why this module exists
//!
//! Phases A-D built every stage of this pipeline and tested each one in
//! isolation: [`ProjectBundle`] loads a project, [`reconcile`] binds persistent
//! identities to authored elements, and [`RuntimeProjection`] derives runtime
//! state. None of them had a production caller. The editor started from a
//! hardcoded starter scene regardless of what was on disk, so "Spool can open
//! a source-backed project" was true of the types and false of the program.
//!
//! This module is the one place that runs the stages in order and hands the
//! result to the editor. `CanvasView` consumes the outcome; it does not parse
//! anything, so adding a second loader would not be tempting later.
//!
//! # The path
//!
//! ```text
//! project directory
//!   -> ProjectBundle::load      (metadata validated, every binding resolved)
//!   -> source_binding::reconcile (identity confirmed against the authored parse)
//!   -> RuntimeProjection         (runtime keys and prototype geometry derived)
//!   -> canvas::Document          (the disposable scene the editor draws)
//!   -> engine_project::compile   (the engine's own model, for the engine)
//! ```
//!
//! # What is authoritative and what is derived
//!
//! Authoritative: the HTML/CSS/SVG on disk, and the `lamine.yaml` metadata.
//! Neither is modified by opening a project.
//!
//! Derived and disposable: everything in [`LoadedProject::projection`] and
//! [`LoadedProject::runtime`]. Drop them and re-open the project and you get
//! equivalent state. Opening a project never writes anything back.
//!
//! # Geometry is still provisional
//!
//! [`RuntimeProjection`] supplies [`PrototypeGeometry`], a deterministic
//! placeholder column. No layout engine exists, so a project's real authored
//! geometry is not read yet. The pipeline is proven; visual fidelity is not,
//! and nothing here should be read as a claim that it is.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::canvas::Document;
use crate::document_runtime_bridge::RuntimeProjection;
use crate::project_bundle::{BundleError, ProjectBundle};
use crate::source_binding::{self, BindingError};
use crate::source_document::PersistentDocument;

/// A project that loaded, bound, and projected successfully.
///
/// Holds the canonical document beside the derived runtime state so a caller
/// can persist metadata edits without re-reading the project.
#[derive(Clone, Debug)]
pub struct LoadedProject {
    /// The directory the project was opened from.
    pub root: PathBuf,
    /// Canonical, source-backed state. This is the only durable truth.
    pub document: PersistentDocument,
    /// Derived runtime mapping from `NodeId` to `ObjectId`. Disposable.
    ///
    /// Read by callers that want to go the other way, from a runtime object back
    /// to the node it came from, and by the open-path tests.
    #[allow(dead_code)]
    pub projection: RuntimeProjection,
    /// The disposable scene the editor draws and edits. Derived.
    pub runtime: Document,
    /// Derived text, style, and layout for each node. Disposable.
    ///
    /// The interpretation of the authored source, kept beside the runtime it
    /// produced so a caller can ask "what did the source say?" instead of
    /// inferring it from the runtime.
    #[allow(dead_code)]
    pub visuals: crate::visual::VisualModel,
    /// Nodes that bound and drew, but whose paint came from the editor's
    /// defaults because no authored CSS gave them any.
    ///
    /// The visible edge of CSS coverage. Reported rather than hidden, so a
    /// project that opens with editor-default chrome is visible as such
    /// instead of looking like the authored design.
    pub unstyled: Vec<String>,
    /// Nodes that bound successfully but have no canvas representation yet.
    ///
    /// A node is never dropped silently: if its kind is not one the runtime can
    /// draw, it still exists in `document` and `projection`, and it is counted
    /// here so the caller can say so.
    pub unrendered: Vec<String>,
    /// The engine document compiled from the project. Derived and disposable.
    ///
    /// This is the first thing in the pipeline that speaks the engine's own
    /// model, and nothing reads it yet: it is produced on open so the claim
    /// "a real project compiles into an engine document" is true of the running
    /// program rather than only of a test.
    ///
    /// A compile failure is not fatal. A project the path above can open must
    /// keep opening, so the failure becomes an empty document plus a readable
    /// line in `engine.unresolved` — see [`crate::engine_project`].
    // Dead code today, exactly as `projection` and `visuals` were when they
    // were added: the compile is wired in so that the production open path
    // produces an engine document, and the reader that will consume it is a
    // later change. Nothing reads it, so say so rather than pretend otherwise.
    #[allow(dead_code)]
    pub engine: crate::engine_project::EngineProject,
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

/// Open a source-backed project and derive the runtime document for it.
///
/// This is the single production entry point. `CanvasView::load_project`
/// calls it; nothing else loads projects.
pub fn open_project(root: impl AsRef<Path>) -> Result<LoadedProject, ProjectOpenError> {
    // 1. Metadata + authored bytes, with every binding already resolved to
    //    exactly one authored element.
    let bundle = ProjectBundle::load(root)?;
    let root = bundle.root().to_path_buf();
    let document = bundle.document;

    // 2. Confirm identity against the authored parse rather than trusting the
    //    metadata alone. `sources` is keyed by the same relative paths the
    //    bindings use.
    let sources: BTreeMap<String, String> = document
        .sources
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let reconciled = source_binding::reconcile(&document.structure, &sources)
        .map_err(ProjectOpenError::Binding)?;

    // 3. Interpret the authored source into text, style, and layout. This is
    //    derived and disposable: nothing here is written back to the project.
    let visuals = crate::visual::build(&reconciled, &document);

    // 4. Derive runtime state. Disposable, and keyed from the reserved range so
    //    it cannot collide with keys the canvas allocates later.
    let projection = RuntimeProjection::from_document_with_visuals(&document, None, &visuals);
    let runtime = Document::from_design_objects(projection.canvas_objects());

    let unstyled: Vec<String> = visuals
        .unstyled()
        .iter()
        .map(|id| id.as_str().to_owned())
        .collect();

    // 5. A project that binds but draws nothing is reported, not shown blank.
    //    "Unrendered" means the node survived into the projection and the
    //    persistent document, but its kind has no canvas representation yet, so
    //    it is absent from the runtime document. Derived by comparing the two,
    //    rather than by asking the projection to classify again.
    let drawn: std::collections::HashSet<&str> = runtime
        .objects()
        .iter()
        .map(|object| object.spool_id.as_str())
        .collect();
    let unrendered: Vec<String> = projection
        .in_hierarchy_order()
        .iter()
        .filter(|node| !drawn.contains(node.node_id.as_str()))
        .map(|node| format!("{} ({})", node.name, node.node_id.as_str()))
        .collect();
    if runtime.objects().is_empty() {
        return Err(ProjectOpenError::NothingRenderable);
    }

    // 6. Compile the same project into the engine's document model. Additive
    //    and non-fatal: nothing below reads it yet, and a project the stages
    //    above accepted must not start failing because the engine compile
    //    cannot represent something in it.
    let engine = crate::engine_project::compile_or_report(&document);

    Ok(LoadedProject {
        root,
        document,
        projection,
        runtime,
        visuals,
        unstyled,
        unrendered,
        engine,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{CanvasView, ObjectId};
    use crate::document_runtime_bridge::PROJECTED_OBJECT_ID_BASE;
    use crate::source_document::NodeId;

    fn fixture_root(fixture: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture)
    }

    fn node(id: &str) -> NodeId {
        NodeId::new(id).expect("valid fixture id")
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
    fn identity_survives_projection_and_maps_deterministically() {
        let first = open_project(fixture_root("landing")).expect("landing opens");
        let second = open_project(fixture_root("landing")).expect("landing opens");

        // Re-opening the same project yields the same runtime keys.
        let ids = |p: &LoadedProject| -> Vec<ObjectId> {
            p.projection.nodes.iter().map(|n| n.object_id).collect()
        };
        assert_eq!(ids(&first), ids(&second));

        // Each persistent node keeps a distinct runtime key, and the reverse
        // lookup agrees.
        let mut seen = std::collections::HashSet::new();
        for node in &first.projection.nodes {
            assert!(
                seen.insert(node.object_id),
                "two persistent nodes share runtime key {}",
                node.object_id.0
            );
            assert_eq!(
                first.projection.object_of(node.object_id),
                Some(&node.node_id)
            );
            assert_eq!(
                first.projection.identity_of(&node.node_id),
                Some(node.object_id)
            );
        }
        // And that NodeId is the one authored in metadata and markup.
        assert_eq!(
            first.projection.identity_of(&node("spool-text-headline")),
            Some(ObjectId(PROJECTED_OBJECT_ID_BASE + 1))
        );
    }

    #[test]
    fn projected_keys_stay_clear_of_the_canvas_allocator() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");

        // Objects the user draws afterwards get keys above every projected key.
        let mut canvas = loaded.runtime.clone();
        let drawn = canvas.create_object(
            crate::canvas::ObjectType::Rectangle,
            gpui::point(0.0, 0.0),
            gpui::size(10.0, 10.0),
            None,
        );
        for projected in &loaded.projection.nodes {
            assert_ne!(
                projected.object_id, drawn.id,
                "a drawn object collided with a projected key"
            );
            assert_eq!(
                loaded.projection.object_of(drawn.id),
                None,
                "a drawn object must not resolve to a persistent node"
            );
        }
    }

    #[test]
    fn hierarchy_survives_from_metadata_into_the_runtime_document() {
        let loaded = open_project(fixture_root("nested")).expect("nested opens");

        // Persistent hierarchy.
        let root = &loaded.document.structure.nodes[0];
        assert_eq!(root.id.as_str(), "spool-page-home");
        assert_eq!(root.parent, None);
        assert_eq!(
            root.children,
            vec![node("spool-text-title"), node("spool-image-logo")],
            "the authored child order is preserved"
        );

        // Projected hierarchy, in the same order.
        let projected: Vec<&str> = loaded
            .projection
            .in_hierarchy_order()
            .iter()
            .map(|n| n.node_id.as_str())
            .collect();
        assert_eq!(
            projected,
            vec!["spool-page-home", "spool-text-title", "spool-image-logo"],
            "hierarchy order reaches the projection"
        );

        // The parent relationship survives into the projection itself.
        let title = loaded
            .projection
            .nodes
            .iter()
            .find(|n| n.node_id == node("spool-text-title"))
            .unwrap();
        assert_eq!(title.parent, Some(node("spool-page-home")));
    }

    #[test]
    fn the_runtime_document_holds_the_projected_objects() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");

        // landing declares frame, text, frame: all three are drawable.
        assert!(loaded.unrendered.is_empty(), "landing is fully renderable");
        assert_eq!(loaded.runtime.objects().len(), 3);

        let objects = loaded.runtime.objects();
        assert_eq!(objects[0].spool_id, node("spool-frame-root"));
        assert_eq!(objects[0].name, "Landing Frame");
        assert_eq!(objects[1].spool_id, node("spool-text-headline"));
        assert_eq!(objects[2].spool_id, node("spool-cta-primary"));
        // Order follows the hierarchy, so the parent is first.
        assert_eq!(objects[0].id, ObjectId(PROJECTED_OBJECT_ID_BASE));
    }

    #[test]
    fn a_node_the_runtime_cannot_draw_is_reported_not_dropped() {
        let loaded = open_project(fixture_root("nested")).expect("nested opens");

        // `nested` declares an image, which has no canvas representation yet.
        // It must survive in the document and be named in `unrendered`.
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
        assert_eq!(
            loaded.runtime.objects().len(),
            2,
            "only drawable nodes reach the runtime document"
        );
    }

    #[test]
    fn canvas_view_actually_receives_the_projected_document() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");
        let expected: Vec<NodeId> = loaded
            .runtime
            .objects()
            .iter()
            .map(|o| o.spool_id.clone())
            .collect();

        let mut view = CanvasView::new();
        // Before opening, the canvas is the starter scene, not the project.
        assert_ne!(
            view.document_objects().len(),
            expected.len(),
            "the blank canvas is not already showing the project"
        );

        view.load_project(loaded);

        let now: Vec<NodeId> = view
            .document_objects()
            .iter()
            .map(|o| o.spool_id.clone())
            .collect();
        assert_eq!(now, expected, "the live canvas holds the projected nodes");
        assert_eq!(view.persistent_document().structure.nodes.len(), 3);
        assert!(
            view.selection().ids().is_empty(),
            "stale selection was cleared"
        );

        // Hit testing sees the projected objects, so they are really in the
        // scene rather than merely listed. Children are painted over their
        // parent, so a point inside the call-to-action resolves to the CTA and
        // not to the frame behind it.
        let cta = view
            .document_objects()
            .iter()
            .find(|object| object.spool_id.as_str() == "spool-cta-primary")
            .expect("the CTA is projected");
        let inside = gpui::point(
            cta.position.x + cta.size.width / 2.0,
            cta.position.y + cta.size.height / 2.0,
        );
        assert_eq!(view.runtime_document().hit_test(inside), Some(cta.id));
    }

    #[test]
    fn the_blank_document_path_still_works() {
        // No project requested: the editor keeps its original behaviour.
        let view = CanvasView::new();
        assert_eq!(
            view.document_objects().len(),
            4,
            "the starter scene remains"
        );
        assert!(
            view.persistent_document().structure.nodes.is_empty(),
            "no project means no persistent metadata"
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
        // Every node binds, but no kind is one the runtime can draw yet. The
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
