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
//! change's job. The job is to prove the engine can be fed from a real project.
//!
//! So the conversion happens here, in one direction, and everything about it is
//! additive: the compiled document is *produced*, not *installed*. Nothing in
//! the editor reads it yet, so no existing behaviour can depend on it.
//!
//! # The two ids
//!
//! The app's [`crate::source_document::NodeId`] is a string; the engine's
//! [`figcraft_doc::NodeId`] is a `u64`. Neither is derived from the other —
//! `spool-source`'s crate docs explain why that separation is the whole point.
//! This module allocates the model id from 1 in document order, exactly as
//! `Manifest::from_yaml` does when it upgrades a v1 file, and carries the
//! string through as the record's `key`.
//!
//! # Ownership
//!
//! Only what the app actually holds is converted: identity, hierarchy, the
//! source binding, and the layer name. Every Owner B field `spool-source`
//! defines beyond those stays at its default, because this slice is not about
//! populating them and inventing values would be a lie. Geometry, paint and
//! text come from the authored CSS and markup, resolved by `spool-source`.
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

use crate::source_document::{LamineStructure, NodeId as SourceNodeId, PersistentDocument};

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
}

/// Compile a persistent document into the engine's document model.
///
/// The three steps, in order: convert the app's structure into a
/// [`Manifest`], hand it and the authored bytes to
/// [`spool_source::reader::read_with_diagnostics`], and flatten the returned
/// [`spool_source::reader::Read`] into this crate's shape.
///
/// Fails rather than approximating. A duplicate id, a dangling parent, a
/// binding that matches nothing in the markup and a kind the engine does not
/// have are all returned as errors; none of them becomes a default.
pub fn compile(persistent: &PersistentDocument) -> Result<EngineProject, EngineProjectError> {
    let manifest = to_manifest(persistent)?;
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
        unresolved: read
            .unresolved
            .iter()
            .map(spool_source::reader::Unresolved::to_string)
            .collect(),
    })
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
pub fn compile_or_report(persistent: &PersistentDocument) -> EngineProject {
    match compile(persistent) {
        Ok(compiled) => compiled,
        Err(error) => EngineProject {
            document: Document::without_pages(FAILED_DOCUMENT_NAME),
            unresolved: vec![format!("the engine document did not compile: {error}")],
        },
    }
}

/// Convert the app's structure into a `spool-source` manifest.
///
/// Model ids are allocated from 1 in document order — the same allocation
/// `Manifest::from_yaml` performs when it upgrades a version 1 file, so a
/// project converted here and a project loaded through `spool-source`'s own
/// loader agree on which node is which.
///
/// The string id becomes the record's `key`, because it is the authored
/// identity: it is what `data-spool-id` carries in markup and what the
/// binding's selector resolves against. Everything else Owner B — constraints,
/// auto-layout, masks, grids, exports, components — stays at its default,
/// because the app's structure does not hold any of it and this slice is not
/// about inventing it.
///
/// Validated before returning, so a structure that cannot become a compilable
/// manifest is refused here rather than downstream with a less specific
/// complaint.
fn to_manifest(persistent: &PersistentDocument) -> Result<Manifest, EngineProjectError> {
    let manifest = manifest_from_structure(&persistent.structure);
    manifest.validate()?;
    Ok(manifest)
}

/// Build the manifest records for one structure.
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
    use figcraft_doc::{Node, NodeId, NodeKind};
    use std::path::{Path, PathBuf};

    /// One of the committed fixtures, resolved against this crate.
    fn fixture_root(fixture: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture)
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
        let root = compiled.document.node(NodeId(1)).expect("model id 1 is the root");
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
                compiled.unresolved.iter().any(|line| line
                    .contains("(spool-frame-root)")
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
            compiled.unresolved.iter().any(|line| line.contains("opacity")),
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
        // `nested` declares `spool-image-logo` with kind `image`. The old path
        // opens it and reports the node as unrendered; the engine has no such
        // kind, and `kind` is Owner B — so picking the nearest one would be
        // inventing a node the author never described. Refused, by name.
        let loaded = open_project(fixture_root("nested")).expect("nested opens");
        assert_eq!(
            loaded.unrendered,
            vec!["Logo (spool-image-logo)".to_string()],
            "the old path surfaces it as unrendered"
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
        // `nested` is the case that makes this real: a committed project the old
        // path opens and renders, which the engine cannot yet compile.
        let loaded = open_project(fixture_root("nested")).expect("nested opens");
        assert!(
            !loaded.runtime.objects().is_empty(),
            "the old path drew it, so the project does open"
        );

        let reported = compile_or_report(&loaded.document);
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
            compile_or_report(&ok.document).document.node_count(),
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
            opened.runtime.objects().len(),
            2,
            "and the editor still draws the project it always drew"
        );
    }

    #[test]
    fn opening_a_project_compiles_it_and_leaves_the_old_path_untouched() {
        let loaded = open_project(fixture_root("landing")).expect("landing opens");

        assert_eq!(
            names(&loaded.engine.document),
            vec!["Landing Frame", "Headline", "Primary CTA"],
            "opening a project now produces an engine document"
        );
        // The runtime the editor actually draws is the pre-existing one, built
        // the same way as before this module existed.
        assert_eq!(loaded.runtime.objects().len(), 3);
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
        let manifest = to_manifest(&loaded.document).expect("landing is a valid manifest");

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
        // identity, hierarchy, name, kind and binding is still the default.
        assert_eq!(manifest.nodes[0], NodeRecord {
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
        });
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

        let error = to_manifest(&document).expect_err("a text node cannot hold children");
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
        assert!(Path::new(&fixture_root("landing")).join("lamine.yaml").exists());
        assert!(Path::new(&fixture_root("landing")).join("styles.css").exists());
    }
}