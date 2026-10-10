//! Writing an edited project back to its authored source.
//!
//! # The rule this module exists to keep
//!
//! HTML, CSS, and SVG stay canonical. An edit changes the smallest authored
//! span that can express it and leaves every other byte alone. Nothing here
//! regenerates a document, reformats a stylesheet, or writes a visual value
//! into `lamine.yaml`.
//!
//! # What is supported
//!
//! | Edit | Written to | How |
//! |---|---|---|
//! | rename | `lamine.yaml` | existing metadata semantics |
//! | text | the element's text in HTML | replaces the text between its tags |
//! | geometry | a `style` attribute on the element | sets explicit box values |
//! | style | the owning CSS declaration | rewrites that declaration's value |
//!
//! An edit with no owned source is reported as unsupported rather than being
//! silently dropped, because a save that claims to have persisted something it
//! did not is worse than one that says so.
//!
//! # What is deliberately not supported
//!
//! Each of these is reported rather than approximated, and each is a decision
//! this milestone makes rather than a gap it has forgotten:
//!
//! - **Creating or deleting an authored object.** Minting a `data-spool-id`,
//!   an element, and a metadata node together is a different operation with a
//!   different failure surface; the editor already reports such objects as
//!   unsupported when it saves. Until it exists, a deleted object keeps its
//!   authored element untouched, which is the safe direction: the source is
//!   still valid and the element comes back on reopen.
//! - **Rewriting a declaration a rule shares with other elements.** Editing
//!   `.card .button` because one button was selected changes every card. The
//!   architecture document names that as the wrong case and the alternative —
//!   minting a class that beats the rule by specificity — as gated on the
//!   cascade engine. So a shared declaration is diagnosed, not edited.
//! - **`!important`.** The style subset does not model the flag, and dropping
//!   it while rewriting a value would change which declaration wins.
//! - **Combinators, pseudo-classes, media queries, `@import`, `@layer`.** The
//!   subset does not see them, so ownership cannot reason about a property they
//!   declare; such an edit becomes a local declaration, which is what wins in
//!   the cascade anyway.
//! - **A transaction across several files.** Each file is replaced atomically
//!   and every edit is resolved before any byte is written, so a failure while
//!   deciding changes nothing at all. A crash *between* two renames can still
//!   leave some files updated. That is recorded rather than papered over, and
//!   it is safe in the one direction that matters: metadata, which says which
//!   nodes exist, is written last.
//!
//! # Provenance
//!
//! Every write targets a byte range resolved against **the bytes on disk at the
//! moment of the save**, not against the snapshot taken when the project was
//! opened. One parse per file per save, so ownership and the ranges that follow
//! from it cannot describe different revisions of the same file, and an
//! externally edited file cannot cause a write to land in the wrong place.
//!
//! # What a save guarantees
//!
//! - Every edit is resolved and every file is rendered **before the first
//!   write**, so a save that cannot be decided changes nothing at all.
//! - A file outside an edited span is byte-identical afterwards, including
//!   whitespace, attribute order, quoting, comments, and untouched rules.
//! - A save that changes metadata refuses to write a structure that would not
//!   bind against the source it is writing, so it cannot leave a project that
//!   no longer opens.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::project_bundle::{BundleError, ProjectBundle};
use crate::source_binding::{self, is_markup, ByteRange, SourceIndex};
use crate::source_document::{NodeId, PersistentDocument, StructuralNode};
use crate::style::Stylesheet;

// # MUTATION HARNESS
//
// `app/mutate_created_objects.sh` breaks one rule at a time in this file — the
// authored element, the identity attribute, the source-order splice, the removal
// splice — and requires the suite to notice. A rule that can be broken without a
// test failing is reported as `SURVIVED`, because an untested invariant is the
// finding. The script refuses to run unless it sees this marker.

/// One change to write back to source.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceEdit {
    /// Replace the text content of a node's element.
    Text { node: NodeId, text: String },
    /// Write explicit geometry onto a node's element.
    ///
    /// A move and a resize are separate because they are separate kinds of
    /// source change: a resize is an explicit box, and a move is a position
    /// expressed in whichever of the two ways the element's own positioning
    /// allows. See [`Placement`].
    Geometry {
        node: NodeId,
        /// `None` when only the box changed.
        placement: Option<Placement>,
        width: Option<f32>,
        height: Option<f32>,
    },
    /// Rewrite the value of a property a node owns.
    ///
    /// Only supported when a declaration is already authored for that node and
    /// this property. An edit with no authored owner is reported, not invented.
    Style {
        node: NodeId,
        property: String,
        value: String,
    },
    /// Author an element for a node that has none yet.
    ///
    /// This is what makes a created object survive a reopen. The node is already
    /// in the persistent document — it got there through a structural operation,
    /// not through here — so all this edit carries is the authored half: the
    /// element itself, its identity attribute, and the appearance the editor
    /// cannot express any other way for an element that was not authored with a
    /// class to own.
    Create { node: NodeId, element: NewElement },
    /// Take an element back out of the source, for a node leaving the document.
    ///
    /// The inverse of [`SourceEdit::Create`], and deliberately not a separate
    /// mechanism: deleting a node is the same splice as never having written it.
    ///
    /// `file` travels with the edit because the node is no longer in the document
    /// to ask: a node that is leaving has, by definition, already been removed
    /// from the structure, so the only record of which file its element lived in
    /// is the one the caller kept.
    Remove { node: NodeId, file: String },
}

/// The element to author for a created node.
///
/// Carries only what has to be written into markup. Where the node goes is not
/// here: that is decided from the parent the document already records, so the
/// element lands where the hierarchy says it belongs rather than wherever this
/// edit happened to be built.
#[derive(Clone, Debug, PartialEq)]
pub struct NewElement {
    /// The tag to author, e.g. `div`.
    pub tag: String,
    /// Authored text content, for an object whose kind carries text.
    pub text: Option<String>,
    /// Declarations for the element's own inline `style`.
    ///
    /// Inline because that is the convention the geometry writer already follows
    /// for an element with no authored rule of its own, and because inventing a
    /// class would write a stylesheet rule that the author's stylesheet never
    /// asked for.
    pub declarations: Vec<(String, String)>,
}

/// How a moved element's new position is expressed in source.
///
/// The two cases exist because the two cases have different honest answers.
/// Turning every dragged element into `position: absolute` — which is what this
/// milestone inherited — is correct for a box the author already took out of
/// the flow, and wrong for one that did not: lifting a `main` out of `body`
/// deletes the only thing that let it reflow with its siblings, so reopening
/// the project shows a page the author never wrote.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    /// Out of the flow, at this offset from the containing block.
    ///
    /// Written as `left`/`top`, which is what an absolutely positioned box
    /// already means.
    ContainingBlock { x: f32, y: f32 },
    /// Still in the flow, offset by this much from where the flow puts it.
    ///
    /// Written as `transform: translate(...)`, which moves a box without
    /// removing it from the flow. The offset is added to whatever the author
    /// already authored, so a second move composes with the first instead of
    /// replacing it.
    Flow { dx: f32, dy: f32 },
}

/// Where the declarations for one geometry edit go.
///
/// Split because a box and a position do not have the same answer: a size is
/// always a local declaration on the element, while a flow offset may belong to
/// a stylesheet rule the author wrote.
#[derive(Clone, Debug, Default, PartialEq)]
struct GeometryWrites {
    /// Declarations for the element's own inline `style`.
    inline: Vec<(String, String)>,
    /// `(file, value range, replacement)` for authored declarations.
    css: Vec<(String, ByteRange, String)>,
}

/// A change that could not be written, with the reason.
#[derive(Clone, Debug, PartialEq)]
pub struct UnsupportedEdit {
    pub node: NodeId,
    pub kind: &'static str,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct SaveOutcome {
    /// Authored files actually written.
    pub written: Vec<PathBuf>,
    /// Edits deliberately not written.
    pub unsupported: Vec<UnsupportedEdit>,
}

/// A save that failed part-way through.
///
/// Multi-file writes are **not** atomic, and this type exists so that is visible
/// rather than implied. Each individual file is replaced atomically — a sibling
/// temporary and a rename — so no file is ever half-written. The *set* is
/// different: `lamine.yaml` is written last precisely because a crash between
/// two renames should leave metadata that still matches the source it was
/// written against, but nothing rolls a file that already landed back off the
/// disk.
///
/// So a failure can arrive with some files persisted and some not, and a caller
/// keeping per-file state has to be able to tell which is which. Returning a
/// bare [`BundleError`] would say only that something failed, and a caller would
/// reasonably conclude nothing was written.
#[derive(Debug)]
pub struct SaveError {
    /// What went wrong.
    ///
    /// Boxed because [`BundleError`] is itself a wide enum, and carrying it by
    /// value made every `Result` in the save path expensive to move.
    pub source: Box<BundleError>,
    /// Files that were replaced before the failure. These *are* on disk, so
    /// anything that tracks "what does the source say" must treat them as
    /// persisted rather than assume the save was a no-op.
    pub written: Vec<PathBuf>,
    /// Edits that were planned but never reached, because the failure came first.
    pub unsupported: Vec<UnsupportedEdit>,
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}

impl std::error::Error for SaveError {}

impl From<BundleError> for SaveError {
    /// A failure before any file was reached. `written` is empty because
    /// nothing was replaced.
    fn from(source: BundleError) -> Self {
        Self {
            source: Box::new(source),
            written: Vec::new(),
            unsupported: Vec::new(),
        }
    }
}

/// Everything one save intends to do, decided before a byte is written.
///
/// The phases are separate on purpose. Every edit is resolved against the
/// current bytes, every file is rendered from those same bytes, and the
/// metadata is encoded — all before the first write. A failure anywhere in that
/// work leaves the project exactly as it was, which is what "parse/patch errors
/// make no partial changes" has to mean once several files are involved.
#[derive(Default)]
struct SavePlan {
    /// Markup file -> the elements in it that need splicing. Keyed by node so
    /// a text edit and a style edit to one element merge into a single
    /// `style` attribute instead of two that fight over it.
    html: BTreeMap<String, BTreeMap<NodeId, ElementWrite>>,
    /// Stylesheet -> the declaration value spans to replace.
    css: BTreeMap<String, Vec<CssWrite>>,
    /// Markup file -> parent node -> the children to author inside it.
    ///
    /// Keyed by the *parent*, not the new node, because every child of one
    /// parent is written as a single splice. Two insertions into the same parent
    /// share one offset; as separate splices they would collapse into one, and
    /// only one of the two objects would be authored.
    creates: BTreeMap<String, BTreeMap<NodeId, PlannedInsertion>>,
    /// Markup file -> the authored elements to take back out.
    removes: BTreeMap<String, Vec<PlannedRemoval>>,
    unsupported: Vec<UnsupportedEdit>,
}

/// The new children of one parent element, as a single splice.
#[derive(Clone, Debug, PartialEq)]
struct PlannedInsertion {
    /// Byte offset the markup goes at, inside the parent's content.
    at: usize,
    /// Whitespace before `at` that the insertion replaces, so new children do
    /// not accumulate a blank line above the parent's close tag.
    replaces: std::ops::Range<usize>,
    /// Every new child, each already indented and newline-terminated, in the
    /// order the edits were resolved — which is the order they were created, so
    /// source order and creation order agree.
    markup: String,
}

/// One authored element to cut back out of a markup file.
#[derive(Clone, Debug, PartialEq)]
struct PlannedRemoval {
    range: ByteRange,
    node: NodeId,
}

/// One element's planned writes, resolved against the bytes they land in.
///
/// The spans are absolute and come from one parse, so rendering is arithmetic on
/// that file's bytes and nothing has to be looked up twice — or, worse, looked
/// up in a second parse that might disagree with the first.
#[derive(Clone, Debug, Default, PartialEq)]
struct ElementWrite {
    /// Offset of the `>` that closes this element's start tag, where a `style`
    /// attribute is inserted when the author did not write one.
    open_tag_end: usize,
    /// Range of the authored `style` attribute's value, when it has one.
    style_value: Option<ByteRange>,
    /// The range of text this element owns and the escaped text to put there.
    text: Option<(ByteRange, String)>,
    /// Declarations to merge into the element's own inline style.
    declarations: Vec<(String, String)>,
}

/// One stylesheet declaration value to replace.
#[derive(Clone, Debug, PartialEq)]
struct CssWrite {
    range: ByteRange,
    value: String,
}

/// Apply edits to the project on disk and persist metadata.
///
/// Returns what was written and what was not, so a caller never has to guess
/// whether an edit survived.
///
/// A multi-file save is not atomic — see [`SaveError`] — so the failure case
/// carries the files that did land rather than reporting only that something
/// broke.
pub fn save_project(
    root: &Path,
    document: &PersistentDocument,
    edits: &[SourceEdit],
) -> Result<SaveOutcome, SaveError> {
    // Phase 1 — resolve every edit against the bytes on disk right now.
    let mut files = AuthoredFiles::from_disk(root, document);
    let mut plan = SavePlan::default();
    for edit in edits {
        plan.resolve(&mut files, document, edit);
    }

    // Phase 2 — render. A file's new contents come from the very bytes its
    // ranges were resolved against, so a splice cannot drift onto a different
    // revision of the file.
    let mut rewritten: Vec<(String, String)> = Vec::new();
    // Every file this save touches has to be rendered, not only the ones with an
    // in-place edit. A file whose only change is a created element has no
    // replacement ranges at all, and leaving it out of `rewritten` would mean the
    // element is never written while the metadata that declares it is — which is
    // exactly the project this save refuses to create.
    let mut markup_files: Vec<&String> = plan.html.keys().collect();
    for file in plan.creates.keys().chain(plan.removes.keys()) {
        if !markup_files.contains(&file) {
            markup_files.push(file);
        }
    }
    markup_files.sort();
    for file in markup_files {
        let original = files.text(file).expect("planned files are read");
        let rendered = render_html(
            original,
            plan.html.get(file).unwrap_or(&BTreeMap::new()),
            plan.creates.get(file),
            plan.removes.get(file),
        );
        rewritten.push((file.clone(), rendered));
    }
    for (file, writes) in &plan.css {
        let original = files.text(file).expect("planned files are read");
        rewritten.push((
            file.clone(),
            apply_replacements(
                original,
                writes
                    .iter()
                    .map(|write| (write.range.clone(), write.value.clone())),
            ),
        ));
    }

    // Phase 3 — decide the metadata, still before anything is written. A
    // structure that cannot be encoded fails here, with every authored file
    // untouched, rather than after the project has been half rewritten.
    //
    // Authored sources are cleared because the bundle never writes them: HTML
    // and CSS are written below, byte-for-byte, from their own edits. The
    // metadata file is written only when the structure actually differs from
    // what is on disk, so a text-only or geometry-only edit leaves it — and the
    // formatting the author chose for it — untouched.
    let metadata_path = root.join(crate::project_bundle::METADATA_FILE);
    let mut updated = document.clone();
    updated.sources.clear();
    let on_disk = ProjectBundle::load_structure(root)?;
    let metadata = if on_disk == updated.structure {
        None
    } else {
        let bundle = ProjectBundle::from_document(root, updated)?;
        let encoded = bundle.metadata_yaml()?;
        // A save must not be able to leave a project that cannot be opened. The
        // structure about to be written is checked against the source about to
        // be written — not against what is on disk now — because both are about
        // to change. Renaming a node is a name change and always passes; adding
        // or removing one has to bring its authored element with it, and this is
        // where that is required rather than assumed.
        let mut after: BTreeMap<String, String> = document
            .sources
            .iter()
            .map(|(file, contents)| (file.clone(), contents.clone()))
            .collect();
        for (file, contents) in &rewritten {
            after.insert(file.clone(), contents.clone());
        }
        source_binding::reconcile(&bundle.document.structure, &after).map_err(|errors| {
            BundleError::UnsavableStructure {
                path: metadata_path.clone(),
                detail: errors
                    .iter()
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            }
        })?;
        Some(encoded)
    };

    // Phase 4 — write. Authored source first, metadata last: identity and
    // hierarchy are what say a source file is a valid project, so a crash
    // between two renames leaves a project whose metadata still matches its
    // source.
    let mut outcome = SaveOutcome {
        written: Vec::new(),
        unsupported: plan.unsupported,
    };
    // Authored files first, then metadata, with the partial outcome carried out
    // of a failure: everything in `outcome.written` at the moment of the error is
    // on disk, and a caller whose bookkeeping depends on that has to be able to
    // see it.
    let write_all = || -> Result<(), BundleError> {
        for (file, contents) in &rewritten {
            let original = files.text(file).expect("planned files are read");
            write_if_changed(&resolve(root, file)?, original, contents, &mut outcome)?;
        }
        if let Some(encoded) = metadata {
            crate::project_bundle::write_atomically(&metadata_path, encoded.as_bytes())?;
            outcome.written.push(metadata_path);
        }
        Ok(())
    };
    if let Err(source) = write_all() {
        return Err(SaveError {
            source: Box::new(source),
            written: outcome.written,
            unsupported: outcome.unsupported,
        });
    }
    Ok(outcome)
}

impl SavePlan {
    /// Turn one requested edit into the spans it will write, or into the reason
    /// it cannot be written.
    ///
    /// Resolution happens entirely against the bytes on disk, so two edits in
    /// one save cannot each be resolved against a different revision of the
    /// same file, and nothing here has to be re-checked at write time.
    fn resolve(
        &mut self,
        files: &mut AuthoredFiles,
        document: &PersistentDocument,
        edit: &SourceEdit,
    ) {
        let (node_id, kind) = match edit {
            SourceEdit::Text { node, .. } => (node, "text"),
            SourceEdit::Geometry { node, .. } => (node, "geometry"),
            SourceEdit::Style { node, .. } => (node, "style"),
            SourceEdit::Create { node, .. } => (node, "create"),
            SourceEdit::Remove { node, .. } => (node, "remove"),
        };
        // A removal is answered before the document is consulted, because the node
        // it names is gone from the document — that is what removing one means.
        // Asking the structure first would report every delete as an unknown node.
        if let SourceEdit::Remove { file, .. } = edit {
            let Ok(Some(index)) = files.index(file) else {
                return;
            };
            let Some(binding) = index.find(node_id) else {
                return;
            };
            let range = binding.element_range.clone();
            self.removes
                .entry(file.clone())
                .or_default()
                .push(PlannedRemoval {
                    range,
                    node: node_id.clone(),
                });
            return;
        }
        let Some(node) = document.structure.nodes.iter().find(|n| &n.id == node_id) else {
            self.report(
                node_id,
                "unknown-node",
                "no such node in the persistent document".into(),
            );
            return;
        };
        let file = node.source.file.clone();

        match edit {
            SourceEdit::Text { text, .. } => match files.text_range(&file, node) {
                // Text is written only into the range the element owns. An
                // element whose content is not a single run of text — a wrapper
                // around a child, or an empty one — owns nothing, and rewriting
                // "the inside" anyway would destroy markup the author wrote.
                Ok(Some(range)) => {
                    let Some(mut element) = self.element(files, &file, node_id, node, kind) else {
                        return;
                    };
                    element.text = Some((range, escape_text(text)));
                    self.remember(file, node_id, element);
                }
                Ok(None) => self.report(
                    node_id,
                    kind,
                    format!("no element in {file} owns a single run of text to replace"),
                ),
                Err(reason) => self.report(node_id, kind, reason),
            },
            SourceEdit::Geometry {
                placement,
                width,
                height,
                ..
            } => match geometry_writes(files, node, placement.as_ref(), *width, *height) {
                Ok(writes) => {
                    let Some(mut element) = self.element(files, &file, node_id, node, kind) else {
                        return;
                    };
                    for (property, value) in writes.inline {
                        match writable_declaration(&property, &value) {
                            Ok(()) => set_declaration(&mut element.declarations, property, value),
                            Err(reason) => self.report(node_id, kind, reason),
                        }
                    }
                    self.remember(file, node_id, element);
                    for (css_file, range, value) in writes.css {
                        self.css
                            .entry(css_file)
                            .or_default()
                            .push(CssWrite { range, value });
                    }
                }
                Err(reason) => self.report(node_id, kind, reason),
            },
            SourceEdit::Style {
                property, value, ..
            } => {
                if let Err(reason) = writable_declaration(property, value) {
                    self.report(node_id, kind, reason);
                    return;
                }
                // Ownership decides where the edit lands. See `StyleTarget` for
                // the policy; the short version is that an authored declaration
                // is rewritten in place when this element is its only
                // beneficiary, and a property this element does not itself
                // declare — including one it only inherits — becomes a local
                // declaration rather than a change to an ancestor.
                match files.style_target(node, property, Ownership::Exclusive) {
                    Ok(StyleTarget::Declaration { file, range, .. }) => {
                        self.css.entry(file).or_default().push(CssWrite {
                            range,
                            value: value.clone(),
                        });
                    }
                    Ok(StyleTarget::Element { .. }) => {
                        let Some(mut element) = self.element(files, &file, node_id, node, kind)
                        else {
                            return;
                        };
                        set_declaration(&mut element.declarations, property.clone(), value.clone());
                        self.remember(file, node_id, element);
                    }
                    Err(reason) => self.report(node_id, kind, reason),
                }
            }
            // Unreachable: every removal returned above, before the document was
            // consulted. Present only so the match is total.
            SourceEdit::Remove { .. } => {}
            SourceEdit::Create { element, .. } => {
                match files.insertion_point(&file, node) {
                    Ok(Some(plan)) => {
                        let parent = node.parent.clone().expect("a plan implies a parent");
                        let group = self.creates.entry(file.clone()).or_default();
                        let insertion = group.entry(parent).or_insert_with(|| PlannedInsertion {
                            at: plan.at,
                            replaces: plan.replaces.clone(),
                            markup: String::new(),
                        });
                        // No newline before the first child: the one already in
                        // the file leads up to the replaced indent, and adding
                        // another would leave a blank line above every new element.
                        insertion.markup.push_str(&plan.indent);
                        insertion
                            .markup
                            .push_str(&render_new_element(node_id, element));
                    }
                    // The node has no authored element yet — that is what this
                    // edit is for — so there is nothing to report and nowhere to
                    // report it to.
                    Ok(None) => self.report(
                        node_id,
                        kind,
                        "this node already has an authored element".into(),
                    ),
                    Err(reason) => self.report(node_id, kind, reason),
                }
            }
        }
    }

    /// The element's writable spans, or the reason there are none.
    fn element(
        &mut self,
        files: &mut AuthoredFiles,
        file: &str,
        node_id: &NodeId,
        node: &StructuralNode,
        kind: &'static str,
    ) -> Option<ElementWrite> {
        match files.element_spans(file, node) {
            Ok(Some(element)) => Some(element),
            Ok(None) => {
                self.report(
                    node_id,
                    kind,
                    format!("no element in {file} carries this node's identity"),
                );
                None
            }
            Err(reason) => {
                self.report(node_id, kind, reason);
                None
            }
        }
    }

    /// Merge an element's writes into the plan, so one element is written once.
    ///
    /// Two edits to one element produce one `style` attribute, not two that
    /// fight over it, and one text replacement rather than two at the same
    /// span. The spans themselves are identical between calls — they come from
    /// one parse — so merging is concatenation, not reconciliation.
    fn remember(&mut self, file: String, node: &NodeId, mut element: ElementWrite) {
        self.html
            .entry(file)
            .or_default()
            .entry(node.clone())
            .and_modify(|existing| {
                existing.declarations.append(&mut element.declarations);
                if element.text.is_some() {
                    existing.text = element.text.take();
                }
                if existing.style_value.is_none() {
                    existing.style_value = element.style_value.take();
                }
                existing.open_tag_end = element.open_tag_end;
            })
            .or_insert(element);
    }

    fn report(&mut self, node: &NodeId, kind: &'static str, reason: String) {
        self.unsupported.push(UnsupportedEdit {
            node: node.clone(),
            kind,
            reason,
        });
    }
}

/// Set a property on a list of inline declarations, replacing it in place when
/// the author already declared it.
fn set_declaration(declarations: &mut Vec<(String, String)>, property: String, value: String) {
    match declarations
        .iter_mut()
        .find(|(declared, _)| *declared == property)
    {
        Some(existing) => existing.1 = value,
        None => declarations.push((property, value)),
    }
}

/// Whether an ownership answer is going to be written or only read.
///
/// The distinction matters for one reason: a declaration a rule shares with
/// other elements is a perfectly good *answer* to "is this element out of the
/// flow?", because nothing is written to that rule. It is not an acceptable
/// *target* for an edit, because rewriting it changes every other element too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ownership {
    ReadOnly,
    Exclusive,
}

/// Where a style edit lands, and what the property is currently authored as.
#[derive(Clone, Debug, PartialEq)]
enum StyleTarget {
    /// An authored declaration already owns this property for this element.
    Declaration {
        /// The stylesheet holding it.
        file: String,
        /// Byte range of that declaration's value.
        range: ByteRange,
        /// The authored value text, taken from the same range.
        authored: String,
    },
    /// Nothing this element declares owns the property; write it locally.
    Element {
        /// The inline value, when the element happens to declare it there
        /// without owning it. Needed to compose a new value onto what the
        /// author wrote rather than overwriting it.
        authored: Option<String>,
    },
}

/// Whether a file a save may write to turned out to be readable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    Loaded,
    Absent,
}

/// Where [`AuthoredFiles`] gets the bytes it resolves against.
enum Origin<'a> {
    /// The project on disk. What a save uses: an edit must land in the file as
    /// it is now, not as it was when the project was opened.
    Disk(PathBuf),
    /// The document the editor opened. What a question asked from inside a
    /// session uses, where the open document is the only description of the
    /// project to hand and no write is being planned.
    Snapshot(&'a std::collections::HashMap<String, String>),
}

/// The authored files one save reads, each opened and parsed at most once.
///
/// Every range a save writes to comes from here, resolved against one read of
/// one revision of a file. That is the whole reason this type exists: a write
/// is only trustworthy if the answer to "where does this go?" was computed from
/// the same bytes the write will be applied to, and a save that resolves
/// ownership per edit cannot promise that.
struct AuthoredFiles<'a> {
    origin: Origin<'a>,
    /// Project-root-relative file -> the exact bytes every answer here used.
    texts: BTreeMap<String, String>,
    /// Markup files, parsed. Keyed the same as `texts`.
    indexes: BTreeMap<String, SourceIndex>,
    /// Stylesheets, parsed, in the order [`Self::sheet_names`] gives.
    sheets: BTreeMap<String, Stylesheet>,
    /// Tag, classes and id of every element per markup file.
    identities: BTreeMap<String, Vec<ElementIdentity>>,
    /// The stylesheets this project owns, in cascade order.
    sheet_names: Vec<String>,
    /// The markup files this project owns, in a stable order.
    markup_names: Vec<String>,
}

/// What the renderer calls one element's tag and classes.
/// Tag, classes and id of every element per markup file.
///
/// Read exactly the way the renderer reads them, id included, because that is
/// what the cascade matches on. Two readers of an element's identity would be
/// two chances to disagree about which declaration owns a property, and an
/// ownership answer that disagrees with the cascade would write to the wrong
/// span — or, when the id is dropped, conclude the element has no authored
/// owner at all and paper over an `#id` rule with an inline declaration.
type ElementIdentity = (String, Vec<String>, Option<String>);

impl<'a> AuthoredFiles<'a> {
    /// Read from the project on disk. What a save uses.
    fn from_disk(root: &Path, document: &PersistentDocument) -> Self {
        Self::new(Origin::Disk(root.to_path_buf()), document)
    }

    /// Read from the document the editor opened, with no structure to enumerate.
    ///
    /// Used for a question asked from inside a session, where the open document
    /// is the only description of the project to hand. Nothing is written from
    /// here, so there is nothing for the missing structure to plan against.
    fn from_snapshot(sources: &'a std::collections::HashMap<String, String>) -> Self {
        let sheets = crate::visual::stylesheet_order(sources);
        let mut markup: Vec<String> = sources
            .keys()
            .filter(|name| is_markup(name))
            .cloned()
            .collect();
        markup.sort();
        Self {
            origin: Origin::Snapshot(sources),
            texts: BTreeMap::new(),
            indexes: BTreeMap::new(),
            sheets: BTreeMap::new(),
            identities: BTreeMap::new(),
            sheet_names: sheets,
            markup_names: markup,
        }
    }

    fn new(origin: Origin<'a>, document: &PersistentDocument) -> Self {
        // Cascade order is the order the documents link the sheets, not their
        // names: the renderer walks [`crate::visual::stylesheet_order`], and
        // answering with a different order here would let a save rewrite a
        // declaration that does not control the rendered value.
        let sheets = crate::visual::stylesheet_order(&document.sources);
        let mut markup: Vec<String> = document
            .sources
            .keys()
            .filter(|name| is_markup(name))
            .cloned()
            .collect();
        markup.sort();
        Self {
            origin,
            texts: BTreeMap::new(),
            indexes: BTreeMap::new(),
            sheets: BTreeMap::new(),
            identities: BTreeMap::new(),
            sheet_names: sheets,
            markup_names: markup,
        }
    }

    /// The bytes a file had when this save resolved it.
    fn text(&self, file: &str) -> Option<&str> {
        self.texts.get(file).map(String::as_str)
    }

    /// Read a file this save may write to.
    ///
    /// `Absent` is a real answer rather than a failure: a file the project no
    /// longer has cannot own anything, and the edit that wanted it is reported
    /// instead of being quietly redirected somewhere it was never meant for.
    /// Any other IO failure aborts, because it says nothing about ownership and
    /// everything about whether a write can be trusted.
    fn read(&mut self, file: &str) -> Result<Read, BundleError> {
        if self.texts.contains_key(file) {
            return Ok(Read::Loaded);
        }
        let contents = match &self.origin {
            Origin::Snapshot(sources) => sources.get(file).cloned(),
            Origin::Disk(root) => {
                let path = root.join(file);
                match std::fs::read_to_string(&path) {
                    Ok(contents) => Some(contents),
                    Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
                    // Any other failure says nothing about who owns what and
                    // everything about whether a write can be trusted.
                    Err(source) => return Err(BundleError::Io { path, source }),
                }
            }
        };
        match contents {
            Some(contents) => {
                self.texts.insert(file.to_owned(), contents);
                Ok(Read::Loaded)
            }
            None => Ok(Read::Absent),
        }
    }

    /// Load and parse a markup file if it is not loaded yet.
    fn ensure_markup(&mut self, file: &str) -> Result<bool, BundleError> {
        if self.indexes.contains_key(file) {
            return Ok(true);
        }
        if self.read(file)? == Read::Absent {
            return Ok(false);
        }
        let contents = self.texts.get(file).expect("just read").clone();
        self.indexes
            .insert(file.to_owned(), SourceIndex::parse(&contents));
        Ok(true)
    }

    /// Load and parse a stylesheet if it is not loaded yet.
    fn ensure_sheet(&mut self, file: &str) -> Result<bool, BundleError> {
        if self.sheets.contains_key(file) {
            return Ok(true);
        }
        if self.read(file)? == Read::Absent {
            return Ok(false);
        }
        let contents = self.texts.get(file).expect("just read");
        self.sheets
            .insert(file.to_owned(), Stylesheet::parse(contents));
        Ok(true)
    }

    /// Load a markup file and record what every element in it is, if not already.
    fn ensure_census(&mut self, file: &str) -> Result<bool, BundleError> {
        if !self.ensure_markup(file)? {
            return Ok(false);
        }
        if self.identities.contains_key(file) {
            return Ok(true);
        }
        let Some(index) = self.indexes.get(file) else {
            return Ok(false);
        };
        // Read from the parse rather than a second one, so a census cannot
        // describe a different revision of the file than the bindings do.
        let census = index
            .element_ranges()
            .iter()
            .filter_map(|range| {
                let fragment = index.source.get(range.clone())?;
                Some(crate::visual::element_identity(fragment))
            })
            .collect();
        self.identities.insert(file.to_owned(), census);
        Ok(true)
    }

    /// The range of text an element owns, or `None` when it owns none.
    ///
    /// All-or-nothing, and decided by the authored parse rather than by string
    /// surgery at save time: an element whose content is not a single run of
    /// text — one wrapping a child, or an empty one — has no text range, and
    /// rewriting "the inside" anyway would destroy markup the author wrote.
    fn text_range(
        &mut self,
        file: &str,
        node: &StructuralNode,
    ) -> Result<Option<ByteRange>, String> {
        if !self
            .ensure_markup(file)
            .map_err(|error| error.to_string())?
        {
            return Err(unreadable(file, "this edit"));
        }
        let Some(index) = self.indexes.get(file) else {
            return Err(unreadable(file, "this edit"));
        };
        Ok(index
            .find(&node.id)
            .and_then(|binding| binding.text_range.clone()))
    }

    /// The parse for one of the project's own markup files.
    fn index(&mut self, file: &str) -> Result<Option<&SourceIndex>, String> {
        if !self
            .ensure_markup(file)
            .map_err(|error| error.to_string())?
        {
            return Ok(None);
        }
        Ok(self.indexes.get(file))
    }

    /// Where a new element for `node` should be spliced into `file`.
    ///
    /// `Ok(None)` when the node already has an element, because then it needs no
    /// new one. An error carries the reason it cannot be placed.
    ///
    /// The insertion point is the inside of the parent's content, and the
    /// whitespace immediately before it is replaced rather than kept, so a new
    /// child takes the parent's indentation and leaves no blank line above the
    /// close tag. That is what keeps the authored diff to one added line.
    fn insertion_point(
        &mut self,
        file: &str,
        node: &StructuralNode,
    ) -> Result<Option<InsertionPoint>, String> {
        // Already authored: this edit would give one element two identities.
        if let Some(index) = self.index(file)? {
            if index.find(&node.id).is_some() {
                return Ok(None);
            }
        }
        let Some(parent_id) = node.parent.clone() else {
            return Err(format!(
                "{file} has no parent element to write into: a created object needs a \
                 container, because a project with nowhere to put an element cannot \
                 be reopened"
            ));
        };
        let Some(index) = self.index(file)? else {
            return Err(unreadable(file, "this creation"));
        };
        // The parent must live in the same file. Authoring a child into another
        // page would mean inventing a cross-file reference, so it is reported.
        let Some(parent) = index.find(&parent_id) else {
            return Err(format!(
                "{file} has no element for the parent {}",
                parent_id.as_str()
            ));
        };
        let at = parent.content_end;
        let (indent_start, indent) = source_binding::trailing_indent(&index.source, at);
        // Line up with the parent's existing children when it has any, so the new
        // element is a sibling in the author's own layout rather than at some
        // indentation the writer invented.
        let child_indent = index
            .element_ranges()
            .iter()
            .find(|range| {
                range.start > parent.element_range.start && range.end <= parent.element_range.end
            })
            .map(|first_child| line_indent(&index.source, first_child.start))
            .filter(|candidate| !candidate.is_empty())
            .unwrap_or_else(|| format!("{indent}  "));
        Ok(Some(InsertionPoint {
            at,
            replaces: indent_start..at,
            indent: child_indent,
        }))
    }

    /// The spans one element may be written at, resolved against current bytes.
    ///
    /// `None` when no element carries this identity: a node the authored source
    /// does not back has nowhere to write, and the edit is reported rather than
    /// attached to some element that happens to be nearby.
    fn element_spans(
        &mut self,
        file: &str,
        node: &StructuralNode,
    ) -> Result<Option<ElementWrite>, String> {
        if !self
            .ensure_markup(file)
            .map_err(|error| error.to_string())?
        {
            return Err(unreadable(file, "this edit"));
        }
        let Some(index) = self.indexes.get(file) else {
            return Err(unreadable(file, "this edit"));
        };
        let Some(binding) = index.find(&node.id) else {
            return Ok(None);
        };
        let Some(fragment) = index.source.get(binding.element_range.clone()) else {
            return Err(format!("this node's element in {file} could not be read"));
        };
        // The start tag ends at the first `>`; the attributes before it are the
        // only ones a write may touch, so the value span inside them is
        // resolved from here rather than by searching the element's contents.
        let open_end = fragment.find('>').unwrap_or(fragment.len());
        let start = binding.element_range.start;
        Ok(Some(ElementWrite {
            open_tag_end: start + open_end,
            style_value: style_attribute_value(&fragment[..open_end])
                .map(|range| (start + range.start)..(start + range.end)),
            text: None,
            declarations: Vec::new(),
        }))
    }

    /// Where an edit to `property` on `node` belongs, or why it cannot be
    /// placed.
    ///
    /// Every file the project lists is loaded before the question is answered,
    /// for two reasons. One parse per file rather than one per candidate, and
    /// because an answer computed from a project that is only partly readable
    /// is not an answer: a stylesheet that is not there might have owned the
    /// property, and a page that is not there might have matched the rule. Both
    /// are reported rather than treated as evidence of absence.
    fn style_target(
        &mut self,
        node: &StructuralNode,
        property: &str,
        ownership: Ownership,
    ) -> Result<StyleTarget, String> {
        for file in self.markup_names.clone() {
            if !self
                .ensure_census(&file)
                .map_err(|error| error.to_string())?
            {
                return Err(unreadable(&file, property));
            }
        }
        for name in self.sheet_names.clone() {
            if !self
                .ensure_sheet(&name)
                .map_err(|error| error.to_string())?
            {
                return Err(unreadable(&name, property));
            }
        }
        self.resolve_style_target(node, property, ownership)
    }

    /// The policy, in order.
    ///
    /// 1. The element's own inline `style` declaration wins: an inline style is
    ///    the author saying "this element, not the class".
    /// 2. Otherwise the last stylesheet rule matching this element that declares
    ///    the property is rewritten in place — same file, same declaration, same
    ///    position. That includes a value written as `var(--accent)`: the edit
    ///    replaces it with the resolved colour on this element and leaves the
    ///    variable alone for every other element using it.
    /// 3. Otherwise the element gets a local declaration. That covers both a
    ///    property nobody authored and one the element only *inherits*: editing an
    ///    inherited `color` writes `color` on this element, and deliberately does
    ///    not rewrite the ancestor that happened to be its source. Changing an
    ///    ancestor would silently restyle every sibling inheriting from it, which
    ///    is never what editing one selected object means.
    fn resolve_style_target(
        &self,
        node: &StructuralNode,
        property: &str,
        ownership: Ownership,
    ) -> Result<StyleTarget, String> {
        let file = &node.source.file;
        let Some(index) = self.indexes.get(file) else {
            return Err(format!("{file} is not part of this project's source"));
        };
        let Some(binding) = index.find(&node.id) else {
            return Err(format!("no element in {file} carries this node's identity"));
        };
        let Some(fragment) = index.source.get(binding.element_range.clone()) else {
            return Err(format!("this node's element in {file} could not be read"));
        };
        let (tag, classes, id) = crate::visual::element_identity(fragment);

        let open_end = fragment.find('>').unwrap_or(fragment.len());
        if let Some(declared) = inline_segments(&fragment[..open_end])
            .into_iter()
            .find(|segment| segment.property.as_deref() == Some(property))
        {
            // The value, not the whole declaration. Everything downstream asks
            // CSS-shaped questions of it — is this `absolute`? does it parse as a
            // translate? — and the property name is not part of any answer.
            let authored = declared
                .text
                .split_once(':')
                .map(|(_, value)| value.trim().to_owned())
                .unwrap_or_default();
            return self.authored_inline(property, authored, ownership);
        }

        // Across files the cascade is last-wins: `self.sheet_names` is in the order
        // the documents link the sheets, and a sheet linked later overrides an
        // earlier one. So the owner is the *last* sheet with a winning
        // declaration, not the first — which is the opposite of what this loop
        // used to do, and disagreed with the renderer for any project whose
        // sheets are linked in an order that is not their name order.
        //
        // The rewindability check runs on the sheet that actually won, because
        // that is the only one an edit here would be rewriting.
        let mut owner: Option<StyleTarget> = None;
        for name in &self.sheet_names {
            let Some(sheet) = self.sheets.get(name) else {
                return Err(unreadable(name, property));
            };
            let Some((range, spelling)) =
                winning_declaration(sheet, &tag, &classes, id.as_deref(), property)
            else {
                continue;
            };
            let authored = self
                .texts
                .get(name)
                .and_then(|contents| contents.get(range.clone()))
                .unwrap_or("")
                .to_owned();
            self.check_rewritable(name, &range, &spelling, &authored, ownership)?;
            owner = Some(StyleTarget::Declaration {
                file: name.clone(),
                range,
                authored,
            });
        }
        // Nothing authored here, so the element itself becomes the owner.
        Ok(owner.unwrap_or(StyleTarget::Element { authored: None }))
    }

    /// Guard the two ways rewriting a declaration would not mean what it says.
    fn check_rewritable(
        &self,
        sheet: &str,
        range: &ByteRange,
        spelling: &str,
        authored: &str,
        ownership: Ownership,
    ) -> Result<(), String> {
        if ownership == Ownership::ReadOnly {
            return Ok(());
        }
        if is_important(authored) {
            return Err(format!(
                "the `{spelling}` declaration that owns this value ends in `!important`, \
                 which this editor does not model; rewriting it would change which \
                 declaration wins"
            ));
        }
        let owners = self.owners_of(sheet, range.clone(), spelling);
        if owners != 1 {
            return Err(format!(
                "`{spelling}` is authored in a rule that applies to {owners} of the \
                 elements this project loaded, so rewriting it here would change all \
                 of them. Give the element its own declaration instead."
            ));
        }
        Ok(())
    }

    /// The same check for a declaration the element already carries inline.
    fn authored_inline(
        &self,
        property: &str,
        authored: String,
        ownership: Ownership,
    ) -> Result<StyleTarget, String> {
        if ownership == Ownership::Exclusive && is_important(&authored) {
            return Err(format!(
                "this element's inline `{property}` ends in `!important`, which this \
                 editor does not model; rewriting it would change which declaration wins"
            ));
        }
        Ok(StyleTarget::Element {
            authored: Some(authored),
        })
    }

    /// How many authored elements a stylesheet declaration governs.
    ///
    /// Counted with the query that found it, over every element of every markup
    /// file the project loaded — including the ones no node claims, because a
    /// rule shared with an unmarked element is still shared. Files no node binds
    /// to are not loaded at all, so this is strong evidence rather than proof;
    /// it is checked because the alternative is editing a rule that may govern
    /// elements nobody can see.
    fn owners_of(&self, sheet: &str, range: ByteRange, spelling: &str) -> usize {
        let Some(stylesheet) = self.sheets.get(sheet) else {
            return 0;
        };
        let mut owners = 0;
        for census in self.identities.values() {
            for (tag, classes, id) in census {
                if stylesheet.declaration_value_range(tag, classes, id.as_deref(), spelling)
                    == Some((range.start, range.end))
                {
                    owners += 1;
                }
            }
        }
        owners
    }

    /// Whether a node's element is already positioned out of the normal flow.
    ///
    /// Asked as a read: a `position` rule shared with other elements still
    /// answers the question truthfully, because nothing is written to it. The
    /// error still propagates, because a position this editor guessed wrong
    /// would be written as a `transform` when the author meant `left`/`top`.
    fn out_of_flow(&mut self, node: &StructuralNode) -> Result<bool, String> {
        let target = self.style_target(node, "position", Ownership::ReadOnly)?;
        let authored = match target {
            StyleTarget::Declaration { authored, .. } => Some(authored),
            StyleTarget::Element { authored } => authored,
        };
        Ok(matches!(
            authored.as_deref().map(str::trim),
            Some("absolute" | "fixed")
        ))
    }
}

/// Whether a node's element is already positioned out of the normal flow.
///
/// Asked before every move, because it is the whole difference between a
/// `left`/`top` write and a `transform` write.
///
/// This is the editor's question, answered from the document it opened, so it
/// runs against that snapshot rather than the disk. A save resolves the same
/// policy against the bytes it is about to write; both go through
/// [`AuthoredFiles::out_of_flow`], so the two answers cannot diverge by
/// construction.
pub fn is_out_of_flow(
    document: &PersistentDocument,
    node: &StructuralNode,
) -> Result<bool, String> {
    let mut files = AuthoredFiles::from_snapshot(&document.sources);
    files.out_of_flow(node)
}

/// The stylesheet declaration that wins for `property` on an element, and the
/// spelling of the property it was written as.
fn winning_declaration(
    stylesheet: &Stylesheet,
    tag: &str,
    classes: &[String],
    id: Option<&str>,
    property: &str,
) -> Option<(ByteRange, String)> {
    ownership_spellings(property)
        .into_iter()
        .find_map(|spelling| {
            stylesheet
                .declaration_value_range(tag, classes, id, spelling)
                .map(|(start, end)| (start..end, spelling.to_owned()))
        })
}

/// The property spellings a visual property may be authored as, in preference
/// order.
///
/// A fill may be authored as `background-color` or as the `background` shorthand;
/// both mean the same colour to the renderer. Rewriting whichever one the author
/// already wrote is the minimal edit — the alternative is adding a second
/// declaration that merely overrides the first.
fn ownership_spellings(property: &str) -> Vec<&str> {
    match property {
        "background-color" => vec!["background-color", "background"],
        other => vec![other],
    }
}

/// Why a file the project lists but cannot read stops an ownership answer.
fn unreadable(file: &str, about: &str) -> String {
    format!(
        "{file} is part of this project but could not be read, so {about} cannot be \
         determined from what is there now"
    )
}

/// Whether an authored value ends in the `!important` flag.
fn is_important(value: &str) -> bool {
    value
        .trim_end()
        .to_ascii_lowercase()
        .ends_with("!important")
}

/// Whether a declaration can be written into a `style` attribute without
/// breaking the attribute or splitting the declaration list early.
///
/// Everything Spool writes here is a number, a colour, or a keyword. A value
/// carrying a quote, an entity, a tag character, or a `;` would end the
/// attribute early or introduce a declaration nobody asked for, turning a style
/// edit into broken markup — so it is reported instead.
fn writable_declaration(property: &str, value: &str) -> Result<(), String> {
    let named = !property.is_empty()
        && property.starts_with(|c: char| c.is_ascii_alphabetic() || c == '-')
        && property
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if !named {
        return Err(format!(
            "`{property}` is not a CSS property name this editor can write"
        ));
    }
    match value
        .chars()
        .find(|c| matches!(c, '"' | '&' | '<' | '>' | ';'))
    {
        Some(character) => Err(format!(
            "`{value}` cannot be written into a `style` attribute: it contains `{character}`"
        )),
        None => Ok(()),
    }
}

/// Format a length the way an author would write it.
///
/// Layout runs in `f32`, so an exact-looking position comes out as
/// `22.399994`. Writing that into the document is accurate and unreadable, and
/// it makes every save produce a noisy diff. Two decimals is finer than any
/// screen can show and keeps the source something a person would have written.
pub fn css_length(value: f32) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    if rounded == rounded.trunc() {
        return format!("{}", rounded as i64);
    }
    format!("{rounded}")
}

/// Decide what a geometry edit writes, and where each part goes.
///
/// # The policy
///
/// A size is always the element's own business: `width`/`height` become local
/// declarations, because pinning a box to the number the editor measured is
/// the only way to write an authored width down at all.
///
/// A position follows the element's own positioning:
///
/// - Already out of the flow: `left`/`top`, measured from the containing block.
///   `position` is not restated, because the author already said so — inline
///   or in a rule — and repeating it locally would be a second declaration
///   saying the same thing.
/// - In the flow: `transform: translate(...)`, added to whatever offset the
///   author already wrote. An element that has no authored transform gains one;
///   an element whose author wrote something this editor cannot offset (a
///   rotation, say) is reported rather than having that transform replaced.
///
/// # The failure this is honest about
///
/// A `transform` the editor does not understand is a real conflict, not a
/// missing feature. Overwriting `rotate(4deg)` with a translate would delete an
/// authored decision, so the save reports the node and leaves the file alone.
fn geometry_writes(
    files: &mut AuthoredFiles,
    node: &StructuralNode,
    placement: Option<&Placement>,
    width: Option<f32>,
    height: Option<f32>,
) -> Result<GeometryWrites, String> {
    let mut writes = GeometryWrites::default();
    if placement.is_none() && width.is_none() && height.is_none() {
        return Err("no geometry value to write".to_owned());
    }
    if let Some(width) = width {
        writes
            .inline
            .push(("width".to_owned(), format!("{}px", css_length(width))));
    }
    if let Some(height) = height {
        writes
            .inline
            .push(("height".to_owned(), format!("{}px", css_length(height))));
    }
    let Some(placement) = placement else {
        return Ok(writes);
    };
    match *placement {
        Placement::ContainingBlock { x, y } => {
            if !files.out_of_flow(node)? {
                writes
                    .inline
                    .push(("position".to_owned(), "absolute".to_owned()));
            }
            writes
                .inline
                .push(("left".to_owned(), format!("{}px", css_length(x))));
            writes
                .inline
                .push(("top".to_owned(), format!("{}px", css_length(y))));
        }
        Placement::Flow { dx, dy } => {
            // Exclusive: a shared `transform` rule would move every element it
            // matches, which is not what dragging one of them asked for.
            let target = files
                .style_target(node, "transform", Ownership::Exclusive)
                .map_err(|reason| format!("`transform` cannot be written: {reason}"))?;
            let (base, css_target) = match target {
                StyleTarget::Declaration {
                    file,
                    range,
                    authored,
                    ..
                } => {
                    let base = crate::style::parse_translate(&authored).ok_or_else(|| {
                        format!(
                            "this element's authored `transform: {authored}` is not an offset \
                             this editor can add to"
                        )
                    })?;
                    (base, Some((file, range)))
                }
                StyleTarget::Element { authored } => {
                    // No authored transform means no base to compose with. An
                    // authored one this subset cannot read is a conflict, not an
                    // absent base.
                    let base = match authored {
                        None => (0.0, 0.0),
                        Some(authored) => {
                            crate::style::parse_translate(&authored).ok_or_else(|| {
                                format!(
                                    "this element's authored `transform: {authored}` is not an \
                                     offset this editor can add to"
                                )
                            })?
                        }
                    };
                    (base, None)
                }
            };
            let moved = (base.0 + dx, base.1 + dy);
            if (dx, dy) == (0.0, 0.0) {
                // Back where the flow puts it. `transform: none` says exactly
                // that and reads as no offset; leaving an authored offset in
                // place would reopen the element somewhere the editor is not
                // showing it.
                if base != (0.0, 0.0) {
                    match css_target {
                        Some((file, range)) => writes.css.push((file, range, "none".to_owned())),
                        None => writes
                            .inline
                            .push(("transform".to_owned(), "none".to_owned())),
                    }
                }
                return Ok(writes);
            }
            let value = format!(
                "translate({}px, {}px)",
                css_length(moved.0),
                css_length(moved.1)
            );
            match css_target {
                // The author owns this offset, so the edit belongs to their
                // declaration rather than a new local one that would fight it.
                Some((file, range)) => writes.css.push((file, range, value)),
                None => writes.inline.push(("transform".to_owned(), value)),
            }
        }
    }
    Ok(writes)
}

/// Render one markup file from the bytes its write plan was resolved against.
fn render_html(
    original: &str,
    writes: &BTreeMap<NodeId, ElementWrite>,
    creates: Option<&BTreeMap<NodeId, PlannedInsertion>>,
    removes: Option<&Vec<PlannedRemoval>>,
) -> String {
    let mut replacements: Vec<(ByteRange, String)> = Vec::new();
    for write in writes.values() {
        if let Some((range, text)) = &write.text {
            replacements.push((range.clone(), text.clone()));
        }
        if write.declarations.is_empty() {
            continue;
        }
        let authored = write
            .style_value
            .as_ref()
            .and_then(|range| original.get(range.clone()))
            .unwrap_or("");
        let merged = merge_inline_declarations(authored, &write.declarations);
        match &write.style_value {
            // Replace the authored value in place, so the attribute keeps its
            // position and its quoting style.
            Some(range) => replacements.push((range.clone(), merged)),
            // No `style` attribute yet: add one just before the closing `>`.
            None => replacements.push((
                write.open_tag_end..write.open_tag_end,
                format!(" style=\"{merged}\""),
            )),
        }
    }
    // Creation and removal are splices on the same bytes as everything else, so
    // they travel through the same one pass. `apply_replacements` works back to
    // front, which is what lets an insertion and a removal in one file each land
    // without invalidating the other's offsets.
    for insertion in creates.into_iter().flat_map(|group| group.values()) {
        // The replaced run is the indentation that led up to the parent's close
        // tag, so it has to be written back: without it the new element and the
        // close tag end up sharing a line, and the file stops looking authored.
        let tail = original
            .get(insertion.replaces.clone())
            .unwrap_or_default()
            .to_owned();
        replacements.push((
            insertion.replaces.clone(),
            format!("{}\n{}", insertion.markup, tail),
        ));
    }

    for removal in outermost_only(removes.into_iter().flatten()) {
        // Taking an element out also takes the line it was on. Leaving the
        // indentation behind would leave a line of spaces where the element was.
        let start = line_start_with_indent(original, removal.range.start);
        replacements.push((start..removal.range.end, String::new()));
    }
    apply_replacements(original, replacements)
}

/// Drop removals whose range already sits inside another removal's.
///
/// A cascade removes a container and everything inside it, so both arrive here
/// with byte ranges, and the inner ones are already covered by the outer one.
/// They cannot simply all be applied: `apply_replacements` takes the latest
/// start first, so the innermost element would be lifted out and the container's
/// own tags would survive, empty. Removing the container takes its contents with
/// it in one edit instead.
///
/// Only ranges from the same file can contain one another, and the caller has
/// already grouped them by file, so this runs per file.
fn outermost_only<'a, I: IntoIterator<Item = &'a PlannedRemoval>>(
    removals: I,
) -> Vec<&'a PlannedRemoval> {
    let removals: Vec<&PlannedRemoval> = removals.into_iter().collect();
    let mut kept: Vec<&'a PlannedRemoval> = Vec::with_capacity(removals.len());
    for candidate in removals {
        // Contained by something already kept: that removal covers it.
        if kept.iter().any(|existing| {
            existing.range.start <= candidate.range.start
                && existing.range.end >= candidate.range.end
        }) {
            continue;
        }
        // Contains something already kept: this one covers them all instead.
        kept.retain(|existing| {
            !(candidate.range.start <= existing.range.start
                && candidate.range.end >= existing.range.end)
        });
        kept.push(candidate);
    }
    kept
}

/// Start the line `offset` is on, including its indentation.
///
/// Used when removing an element: the newline and the spaces before it belong to
/// the element's line, so removing the element without them would leave a line
/// of trailing whitespace behind.
fn line_start_with_indent(source: &str, offset: usize) -> usize {
    let newline = source[..offset].rfind('\n');
    match newline {
        Some(index) => index + 1,
        None => 0,
    }
}

/// One `;`-separated segment of an authored inline style.
#[derive(Clone, Debug, PartialEq)]
struct InlineSegment {
    /// The text that led up to this segment, including its `;`. Empty for the
    /// first, so an attribute nobody edited keeps its authored spacing.
    lead: String,
    /// The segment exactly as authored.
    text: String,
    /// The property this segment sets, lowercased, when it is a declaration
    /// this editor recognises. `None` for anything else — a bare word, a
    /// `--custom` property — which is preserved verbatim, never matched, and
    /// never rewritten.
    property: Option<String>,
}

/// Split an inline style value into its authored segments.
///
/// The split ignores `;` inside quotes, because `content: "a;b"` is one
/// declaration to CSS and splitting it would leave a fragment that looks like a
/// second property nobody wrote.
fn parse_inline_segments(value: &str) -> Vec<InlineSegment> {
    // An empty attribute is not a declaration with nothing in it, and treating
    // it as one would give the first appended declaration a leading `; `.
    if value.is_empty() {
        return Vec::new();
    }
    let mut segments = Vec::new();
    let mut lead = String::new();
    let mut start = 0usize;
    let mut quote: Option<char> = None;
    for (at, character) in value.char_indices() {
        match quote {
            Some(open) if character == open => quote = None,
            Some(_) => {}
            None => match character {
                '"' | '\'' => quote = Some(character),
                ';' => {
                    segments.push(segment(std::mem::take(&mut lead), &value[start..at]));
                    start = at + 1;
                    lead.push(';');
                }
                _ => {}
            },
        }
    }
    segments.push(segment(lead, &value[start..]));
    segments
}

fn segment(lead: String, text: &str) -> InlineSegment {
    // A declaration is a property and a colon. The value may be empty — CSS
    // ignores such a declaration, but it is still the author's line and still
    // the right place to write a value for that property.
    let property = text
        .split_once(':')
        .map(|(property, _)| property.trim().to_ascii_lowercase())
        .filter(|property| !property.is_empty());
    InlineSegment {
        lead,
        text: text.to_owned(),
        property,
    }
}

/// Apply edits to an element's inline style and render the attribute value.
///
/// A property already present is replaced where it stands, so a second save
/// cannot duplicate it. Every other segment — including one this editor does
/// not understand, and the spacing and order the author used — is reproduced
/// verbatim. The result is the authored attribute plus the edits, never just
/// the edits.
fn merge_inline_declarations(authored: &str, edits: &[(String, String)]) -> String {
    let mut segments = parse_inline_segments(authored);
    for (property, value) in edits {
        match segments
            .iter()
            .position(|segment| segment.property.as_deref() == Some(property.as_str()))
        {
            // Only the declaration itself is rewritten. The whitespace around it
            // belongs to the author, and dropping it would make every save
            // renormalise the whole attribute: `width: 1px; height: 2px` would
            // come back as `width: 1px;height: 2px` on the second save, and the
            // third would find nothing left to change.
            Some(at) => {
                let raw = &segments[at].text;
                let leading = &raw[..raw.len() - raw.trim_start().len()];
                let trailing = &raw[raw.trim_end().len()..];
                segments[at].text = format!("{leading}{property}: {value}{trailing}");
            }
            None => segments.push(InlineSegment {
                lead: if segments.is_empty() {
                    String::new()
                } else {
                    "; ".to_owned()
                },
                text: format!("{property}: {value}"),
                property: Some(property.clone()),
            }),
        }
    }
    segments
        .iter()
        .map(|segment| format!("{}{}", segment.lead, segment.text))
        .collect()
}

/// The `;`-separated segments of an element's authored inline style.
fn inline_segments(open_tag: &str) -> Vec<InlineSegment> {
    let Some(range) = style_attribute_value(open_tag) else {
        return Vec::new();
    };
    parse_inline_segments(open_tag.get(range).unwrap_or(""))
}

/// The byte range of an open tag's existing `style` attribute value.
///
/// Returns the span of the value alone — not of `style="..."` — so a rewrite
/// replaces the declarations and leaves the attribute itself exactly as the
/// author wrote it. Returning the wrong span is how a second save turns
/// `style="a"` into `style="a"a"`.
///
/// Only a real attribute counts: `data-style-note="x"` is a different attribute
/// and must not be treated as the style.
fn style_attribute_value(open_tag: &str) -> Option<ByteRange> {
    let bytes = open_tag.as_bytes();
    let mut search = 0usize;
    while let Some(offset) = open_tag[search..].find("style") {
        let at = search + offset;
        let after_name = at + "style".len();
        let before_ok = at == 0 || !is_attribute_name_byte(bytes[at - 1]);
        let after = open_tag[after_name..].trim_start();
        let lead = open_tag[after_name..].len() - after.len();
        let equals_at = after_name + lead;
        search = after_name;
        if !before_ok || !after.starts_with('=') {
            continue;
        }
        let value_start = equals_at + 1 + (after[1..].len() - after[1..].trim_start().len());
        let value = &open_tag[value_start..];
        let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'');
        return match quote {
            Some(quote) => {
                let inner = value_start + quote.len_utf8();
                let end = inner + open_tag[inner..].find(quote)?;
                Some(inner..end)
            }
            // Unquoted value: runs to the end of the tag.
            None => {
                let end = value_start
                    + value
                        .find(|c: char| c.is_whitespace() || c == '>')
                        .unwrap_or(value.len());
                Some(value_start..end)
            }
        };
    }
    None
}

/// Bytes that can appear inside an attribute name.
///
/// Used so a `style` match inside another attribute's name or value is not
/// mistaken for the attribute itself.
fn is_attribute_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b':'
}

/// Escape text written into an element's content.
///
/// `&` first: escaping it after `<` would turn the `&` of a freshly written
/// `&lt;` into `&amp;lt;`.
fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Splice replacements into a file's bytes.
///
/// One primitive for every write, because every write has the same two hazards.
/// Back to front, so an earlier replacement cannot shift a later one's offsets.
/// And deduplicated by span, so two edits to one span resolve to the one asked
/// for last instead of both being written into the same place.
///
/// A span that partially overlaps another is skipped. Spans resolved from a
/// single parse of one file cannot overlap, so that would mean ranges from two
/// revisions were applied to one file — and splicing them anyway would corrupt
/// it. Skipping is a guard, not a policy.
fn apply_replacements<I>(original: &str, replacements: I) -> String
where
    I: IntoIterator<Item = (ByteRange, String)>,
{
    // Keyed by the span's two ends rather than by the range itself: two ranges
    // are the same span exactly when their ends are, and a `Range` is not
    // ordered, so a map keyed by one could not dedupe at all.
    let mut by_span: BTreeMap<(usize, usize), String> = BTreeMap::new();
    for (range, value) in replacements {
        by_span.insert((range.start, range.end), value);
    }
    let mut ordered: Vec<(ByteRange, String)> = by_span
        .into_iter()
        .map(|((start, end), value)| (start..end, value))
        .collect();
    ordered.sort_by_key(|(span, _)| Reverse(span.start));

    let mut out = original.to_owned();
    let mut lowest_applied: Option<usize> = None;
    for (range, value) in ordered {
        if !out.is_char_boundary(range.start) || !out.is_char_boundary(range.end) {
            continue;
        }
        // An insertion is empty, so it can never overlap anything.
        if !range.is_empty() && lowest_applied.is_some_and(|end| range.end > end) {
            continue;
        }
        out.replace_range(range.clone(), &value);
        if !range.is_empty() {
            lowest_applied = Some(range.end);
        }
    }
    out
}

fn resolve(root: &Path, file: &str) -> Result<PathBuf, BundleError> {
    crate::project_bundle::resolve_within_root(root, file).ok_or_else(|| {
        BundleError::InvalidSourceReference {
            file: file.to_owned(),
            referenced_by: NodeId::new("unknown").expect("valid"),
        }
    })
}

fn write_if_changed(
    path: &Path,
    original: &str,
    rewritten: &str,
    outcome: &mut SaveOutcome,
) -> Result<(), BundleError> {
    if original == rewritten {
        return Ok(());
    }
    crate::project_bundle::write_atomically(path, rewritten.as_bytes())?;
    outcome.written.push(path.to_path_buf());
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_open::open_project;
    use crate::source_document::NodeId;
    // `from_mode` is how a directory is made read-only, which is how the
    // partial-write test refuses a write without touching any editor code.
    use std::os::unix::fs::PermissionsExt;

    /// Copy a fixture so a save never touches the committed files.
    fn scratch(fixture: &str) -> PathBuf {
        let from = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture);
        let to = std::env::temp_dir().join(format!(
            "spool-save-{fixture}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&to);
        copy_tree(&from, &to).expect("copy fixture");
        to
    }

    fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
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

    /// Write a throwaway project for the cases the shared fixtures do not cover.
    ///
    /// Three files and no more: a fixture is a real authored page, and these
    /// cases are about one declaration each. `scratch` remains the default
    /// because most saves are about the projects people actually author.
    fn project_named(name: &str, html: &str, css: Option<&str>, yaml: &str) -> PathBuf {
        let to = std::env::temp_dir().join(format!(
            "spool-save-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&to);
        std::fs::create_dir_all(&to).expect("create project");
        std::fs::write(to.join("lamine.yaml"), yaml).expect("write metadata");
        std::fs::write(to.join("index.html"), html).expect("write html");
        if let Some(css) = css {
            std::fs::write(to.join("styles.css"), css).expect("write css");
        }
        to
    }

    /// One node's entry in a `lamine.yaml`, in the shape the bundle reads.
    fn node_yaml(id: &str, parent: Option<&str>, children: &str) -> String {
        format!(
            "  - id: \"{id}\"\n    name: \"{id}\"\n    kind: \"frame\"\n    parent: {parent}\n    file: \"index.html\"\n    selector: \"[data-spool-id=\\\"{id}\\\"]\"\n    children: [{children}]\n",
            parent = parent
                .map(|parent| format!("\"{parent}\""))
                .unwrap_or_else(|| "null".to_owned()),
        )
    }

    /// Where a node sits in world coordinates after opening a project.
    fn position_of(runtime: &crate::canvas::Document, id: &NodeId) -> gpui::Point<f32> {
        runtime
            .objects()
            .iter()
            .find(|object| &object.spool_id == id)
            .expect("node is in the runtime")
            .position
    }

    fn node(id: &str) -> NodeId {
        NodeId::new(id).expect("valid id")
    }

    fn read(path: &std::path::Path) -> String {
        std::fs::read_to_string(path).expect("read file")
    }

    /// Assert that `after` is `before` with exactly these substitutions applied.
    ///
    /// Each anchor must occur exactly once, and the assertion checks that first,
    /// because an anchor that occurs twice would make the check vacuous. This is
    /// the strongest statement a source-preserving writer can be held to: the
    /// saved file equals the authored file with the requested edits applied and
    /// nothing else — same whitespace, same attribute order, same comments, same
    /// line endings, same untouched rules.
    fn assert_only(before: &str, after: &str, substitutions: &[(&str, &str)]) {
        let mut expected = before.to_owned();
        for (from, to) in substitutions {
            assert_eq!(
                expected.matches(from).count(),
                1,
                "the anchor {from:?} must occur exactly once for this check to mean anything"
            );
            expected = expected.replacen(from, to, 1);
        }
        assert_eq!(after, expected);
    }

    /// A throwaway project with any set of files.
    ///
    /// `scratch` and `project_named` cover the common shapes; this covers the
    /// one they cannot express, a project with more than one page.
    fn project_files(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let to = std::env::temp_dir().join(format!(
            "spool-save-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&to);
        std::fs::create_dir_all(&to).expect("create project");
        for (path, contents) in files {
            let target = to.join(path);
            std::fs::create_dir_all(target.parent().expect("a parent")).expect("create dir");
            std::fs::write(target, contents).expect("write file");
        }
        to
    }

    // -- The guarantee: nothing outside an edited region moves.

    #[test]
    fn a_multi_edit_save_touches_only_the_spans_it_was_given() {
        // One save, three kinds of edit, across the HTML, the stylesheet, and
        // the metadata. The round-trip tests cover that each edit landed; this
        // one is about everything the save did *not* touch.
        let root = scratch("landing");
        let html_before = read(&root.join("index.html"));
        let css_before = read(&root.join("styles.css"));
        let loaded = open_project(&root).expect("opens");
        let mut renamed = loaded.document.clone();
        renamed.structure.nodes[1].name = "Hero headline".into();

        let outcome = save_project(
            &root,
            &renamed,
            &[
                SourceEdit::Text {
                    node: node("spool-text-headline"),
                    text: "Ships".into(),
                },
                SourceEdit::Style {
                    node: node("spool-cta-primary"),
                    property: "border-radius".into(),
                    value: "12px".into(),
                },
                SourceEdit::Geometry {
                    node: node("spool-cta-primary"),
                    placement: Some(Placement::Flow { dx: 12.0, dy: 0.0 }),
                    width: Some(240.0),
                    height: None,
                },
            ],
        )
        .expect("save");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        // The HTML: the headline's text, and the CTA's open tag gaining the
        // declarations this save added. The doctype, the link, the indentation,
        // the frame, and the CTA's own text are the same bytes.
        assert_only(
            &html_before,
            &read(&root.join("index.html")),
            &[
                ("Design in source, structure in Spool", "Ships"),
                (
                    r##"<a class="cta" data-spool-id="spool-cta-primary" href="#start">"##,
                    r##"<a class="cta" data-spool-id="spool-cta-primary" href="#start" style="width: 240px; transform: translate(12px, 0px)">"##,
                ),
            ],
        );

        // The stylesheet: one declaration rewritten where the author put it.
        // The other five declarations, the `:root` block, and both custom
        // properties are untouched.
        assert_only(
            &css_before,
            &read(&root.join("styles.css")),
            &[("border-radius: 8px", "border-radius: 12px")],
        );

        // The metadata was written because the structure changed, and only
        // then: each file appears once, in the order it was resolved.
        let written: Vec<String> = outcome
            .written
            .iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .expect("a path inside the project")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            written,
            ["index.html", "styles.css", "lamine.yaml"],
            "authored source first, metadata last, each file once"
        );
    }

    #[test]
    fn a_save_with_nothing_to_change_writes_no_bytes_at_all() {
        // The end state of a trustworthy loop: a second save of the same edits
        // is a fixed point. Every file, including the metadata the author may
        // have formatted by hand, is byte-identical.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let edits = [
            SourceEdit::Text {
                node: node("spool-text-headline"),
                text: "Steady".into(),
            },
            SourceEdit::Style {
                node: node("spool-cta-primary"),
                property: "border-radius".into(),
                value: "10px".into(),
            },
        ];

        save_project(&root, &loaded.document, &edits).expect("first save");
        let after_first: Vec<(PathBuf, String)> = ["index.html", "styles.css", "lamine.yaml"]
            .iter()
            .map(|name| (root.join(name), read(&root.join(name))))
            .collect();

        let reopened = open_project(&root).expect("reopens");
        let second = save_project(&root, &reopened.document, &edits).expect("second save");
        assert!(
            second.written.is_empty(),
            "repeating the same edits wrote {:?}",
            second.written
        );

        // And an empty save changes nothing either, which is what makes opening
        // a project and saving it a safe thing for a user to do.
        let empty = save_project(&root, &reopened.document, &[]).expect("empty save");
        assert!(empty.written.is_empty(), "{:?}", empty.written);

        for (path, before) in after_first {
            assert_eq!(
                read(&path),
                before,
                "{} changed on a no-op save",
                path.display()
            );
        }
    }

    #[test]
    fn an_edit_lands_where_the_file_is_now_not_where_it_was() {
        // The write target is resolved from the bytes on disk, not from the
        // snapshot taken when the project was opened. Someone editing the
        // stylesheet in another window shifts every byte offset in it; an edit
        // made afterwards must still find the declaration it was asked to
        // change rather than writing at the offset it used to occupy.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let css_path = root.join("styles.css");
        let css_after_comment = read(&css_path);
        std::fs::write(
            &css_path,
            format!("/* a note someone added in another window */\n{css_after_comment}"),
        )
        .expect("external edit");

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-cta-primary"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        // Exactly the owning declaration changed. Everything else — the note
        // someone added, the `:root` block, both custom properties, the other
        // five declarations in the rule — is the byte it was, and nothing was
        // written at the offset the stale range used to occupy.
        assert_only(
            &format!(
                "/* a note someone added in another window */\n{}",
                css_after_comment
            ),
            &read(&css_path),
            &[("background: var(--accent)", "background: #ff0000")],
        );
    }

    #[test]
    fn text_that_stopped_being_one_run_is_reported_rather_than_dropped() {
        // The element used to own its text; someone else wrapped half of it in
        // an element. There is no longer a range that can be replaced without
        // destroying markup, so the edit is reported — the file is left alone
        // and the caller learns the edit did not happen.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let html_path = root.join("index.html");
        std::fs::write(
            &html_path,
            read(&html_path).replace(
                ">Design in source, structure in Spool<",
                ">Design in <em>source</em>, structure in Spool<",
            ),
        )
        .expect("external edit");
        let before = read(&html_path);

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Text {
                node: node("spool-text-headline"),
                text: "Rewritten".into(),
            }],
        )
        .expect("save");

        assert_eq!(
            outcome.unsupported.len(),
            1,
            "an edit with nowhere to go must be reported, not silently dropped: {:?}",
            outcome.unsupported
        );
        assert_eq!(outcome.unsupported[0].kind, "text");
        assert!(
            outcome.written.is_empty(),
            "nothing was written: {:?}",
            outcome.written
        );
        assert_eq!(
            read(&html_path),
            before,
            "and the file is byte-for-byte what the author left"
        );
    }

    #[test]
    fn a_save_that_cannot_be_decided_writes_nothing() {
        // The metadata cannot be encoded: two nodes would end up with the same
        // name. Deciding that has to happen before the first write, or the
        // project ends up with a rewritten stylesheet and an unwritten rename.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let mut broken = loaded.document.clone();
        broken.structure.nodes[1].name = broken.structure.nodes[0].name.clone();
        let before: Vec<(PathBuf, String)> = ["index.html", "styles.css", "lamine.yaml"]
            .iter()
            .map(|name| (root.join(name), read(&root.join(name))))
            .collect();

        let outcome = save_project(
            &root,
            &broken,
            &[
                SourceEdit::Text {
                    node: node("spool-text-headline"),
                    text: "Never written".into(),
                },
                SourceEdit::Style {
                    node: node("spool-cta-primary"),
                    property: "border-radius".into(),
                    value: "99px".into(),
                },
            ],
        );

        assert!(
            matches!(outcome, Err(ref failure) if matches!(*failure.source, BundleError::MalformedMetadata { .. })),
            "the unencodable structure is reported: {outcome:?}"
        );
        for (path, contents) in before {
            assert_eq!(
                read(&path),
                contents,
                "{} was written even though the save failed",
                path.display()
            );
        }
    }

    #[test]
    fn a_stylesheet_that_is_no_longer_there_is_reported_not_substituted() {
        // The project claims a stylesheet the author deleted. Nothing can own a
        // declaration in a file that does not exist, so the style edit is
        // reported — not quietly turned into an inline declaration, which would
        // have looked like success while silently dropping the author's rule
        // link — and the text edit beside it still lands.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        std::fs::remove_file(root.join("styles.css")).expect("remove stylesheet");
        let html_before = read(&root.join("index.html"));

        let outcome = save_project(
            &root,
            &loaded.document,
            &[
                SourceEdit::Text {
                    node: node("spool-text-headline"),
                    text: "Still written".into(),
                },
                SourceEdit::Style {
                    node: node("spool-cta-primary"),
                    property: "border-radius".into(),
                    value: "12px".into(),
                },
            ],
        )
        .expect("save");

        assert_eq!(outcome.unsupported.len(), 1, "{:?}", outcome.unsupported);
        assert_eq!(outcome.unsupported[0].kind, "style");
        assert_only(
            &html_before,
            &read(&root.join("index.html")),
            &[("Design in source, structure in Spool", "Still written")],
        );
        assert!(
            !read(&root.join("index.html")).contains("border-radius"),
            "the style edit did not become an inline declaration"
        );
    }

    // -- Style ownership: what a write is allowed to touch.

    #[test]
    fn a_declaration_a_rule_shares_is_reported_rather_than_rewritten() {
        // `.cta` styles two elements. Rewriting `background` there because one
        // of them was selected would recolour the other without being asked, so
        // the rule is diagnosed instead. The alternative — minting a class that
        // beats it by specificity — needs the cascade engine this milestone
        // does not have.
        let html = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <a class=\"cta\" data-spool-id=\"spool-first\">One</a>\n  <a class=\"cta\" data-spool-id=\"spool-second\">Two</a>\n</body>\n</html>\n";
        let css = ".cta { background: #0000ff; }\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}{}",
            node_yaml("spool-first", None, ""),
            node_yaml("spool-second", None, "")
        );
        let root = project_named("shared", html, Some(css), &yaml);
        let loaded = open_project(&root).expect("opens");

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-first"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");

        assert_eq!(outcome.unsupported.len(), 1, "{:?}", outcome.unsupported);
        assert_eq!(outcome.unsupported[0].kind, "style");
        assert!(
            outcome.unsupported[0].reason.contains("2"),
            "the reason says how many elements share it: {}",
            outcome.unsupported[0].reason
        );
        assert!(outcome.written.is_empty(), "{:?}", outcome.written);
        assert_eq!(read(&root.join("styles.css")), css, "the rule is untouched");
        assert_eq!(read(&root.join("index.html")), html);
    }

    #[test]
    fn a_rule_shared_only_with_an_element_nobody_claimed_is_still_shared() {
        // The other element carrying the class has no metadata node, so it is
        // absent from the project's node list. It is still in the authored
        // document, and it is still an element the rule styles.
        let html = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <a class=\"cta\" data-spool-id=\"spool-only\">Mine</a>\n  <a class=\"cta\">Not mine</a>\n</body>\n</html>\n";
        let css = ".cta { background: #0000ff; }\n";
        let yaml = format!("version: 1\nnodes:\n{}", node_yaml("spool-only", None, ""));
        let root = project_named("unmarked", html, Some(css), &yaml);
        let loaded = open_project(&root).expect("opens");

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-only"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");

        assert_eq!(outcome.unsupported.len(), 1, "{:?}", outcome.unsupported);
        assert_eq!(read(&root.join("styles.css")), css);
    }

    #[test]
    fn the_winning_declaration_among_several_rules_is_the_one_rewritten() {
        // Two rules match this element. Author order decides, so the later one
        // owns the property — the same answer the renderer gives, because both
        // ask the stylesheet the same question. The other rule is left exactly as
        // the author wrote it.
        let html = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <a class=\"cta\" data-spool-id=\"spool-cta-primary\">Go</a>\n</body>\n</html>\n";
        let css = "a { color: #808080; background: #111111; }\n.cta { background: #222222; }\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}",
            node_yaml("spool-cta-primary", None, "")
        );
        let root = project_named("two-rules", html, Some(css), &yaml);
        let loaded = open_project(&root).expect("opens");

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-cta-primary"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        assert_only(
            css,
            &read(&root.join("styles.css")),
            &[("background: #222222", "background: #ff0000")],
        );
    }

    // -- Selector identity: the ownership answer must be the cascade's answer.
    //
    // Every one of these failed the same way before element identity carried the
    // id: ownership was resolved from tag and classes alone, so a property held
    // by a `#id` rule looked unowned and save papered over it with an inline
    // declaration. The renderer honoured the rule, so the file and the screen
    // disagreed about who owned the value.

    /// One element whose `background` is owned by an `#id` rule.
    ///
    /// The edit must land in the author's rule. Writing an inline override
    /// instead would be indistinguishable from the bug: the fill would still be
    /// red on screen, and the author's `#hero` rule would silently stop
    /// governing the element it was written for.
    fn id_owned_project(name: &str, html: &str, css: &str) -> PathBuf {
        let yaml = format!(
            "version: 1\nnodes:\n{}",
            node_yaml("spool-cta-primary", None, "")
        );
        let root = project_named(name, html, Some(css), &yaml);
        assert!(
            open_project(&root).is_ok(),
            "the fixture is a project that opens"
        );
        root
    }

    #[test]
    fn an_id_rule_owns_its_property_rather_than_being_overridden_inline() {
        let html = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <a id=\"hero\" class=\"cta\" data-spool-id=\"spool-cta-primary\">Go</a>\n</body>\n</html>\n";
        let css = ".cta { border-radius: 4px; }\n#hero { background: #222222; }\n";
        let root = id_owned_project("id-owned", html, css);
        let loaded = open_project(&root).expect("opens");

        // What the renderer resolves is the check that matters: the ownership
        // answer has to be about the declaration the cascade actually picked.
        assert_eq!(
            loaded
                .runtime
                .object(
                    loaded
                        .runtime
                        .objects()
                        .iter()
                        .find(|object| object.spool_id.as_str() == "spool-cta-primary")
                        .expect("projected")
                        .id,
                )
                .and_then(|object| object.fill)
                .map(|fill| fill.color.to_rgb()),
            Some(0x22_22_22),
            "the `#hero` rule is what the element is drawn from"
        );

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-cta-primary"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        assert_only(
            css,
            &read(&root.join("styles.css")),
            &[(
                "#hero { background: #222222;",
                "#hero { background: #ff0000;",
            )],
        );
        assert!(
            !read(&root.join("index.html")).contains("background"),
            "and no inline override was invented on top of the author's rule"
        );
    }

    #[test]
    fn a_type_rule_and_a_class_rule_still_own_their_properties() {
        // The cases that already worked, pinned so the `#id` fix did not buy
        // them by breaking them: ownership follows the cascade for a type
        // selector and for a class selector, and neither grows an inline
        // override.
        let html = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <a class=\"cta\" data-spool-id=\"spool-cta-primary\">Go</a>\n</body>\n</html>\n";
        let css = "a { color: #808080; }\n.cta { background: #222222; }\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}",
            node_yaml("spool-cta-primary", None, "")
        );
        let root = project_named("type-and-class", html, Some(css), &yaml);
        let loaded = open_project(&root).expect("opens");

        save_project(
            &root,
            &loaded.document,
            &[
                SourceEdit::Style {
                    node: node("spool-cta-primary"),
                    property: "background".into(),
                    value: "#ff0000".into(),
                },
                SourceEdit::Style {
                    node: node("spool-cta-primary"),
                    property: "color".into(),
                    value: "#00ff00".into(),
                },
            ],
        )
        .expect("save");

        assert_only(
            css,
            &read(&root.join("styles.css")),
            &[
                ("background: #222222", "background: #ff0000"),
                ("color: #808080", "color: #00ff00"),
            ],
        );
    }

    #[test]
    fn an_inline_style_still_outranks_the_stylesheet_and_is_edited_in_place() {
        // The inline branch runs before any rule is consulted, so adding the id
        // to the cascade query must not have reordered it.
        let html = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <a id=\"hero\" class=\"cta\" style=\"background: #333333\" data-spool-id=\"spool-cta-primary\">Go</a>\n</body>\n</html>\n";
        let css = "#hero { background: #222222; }\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}",
            node_yaml("spool-cta-primary", None, "")
        );
        let root = project_named("inline-beats-id", html, Some(css), &yaml);
        let loaded = open_project(&root).expect("opens");

        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-cta-primary"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");

        assert!(
            read(&root.join("index.html")).contains("background: #ff0000"),
            "the inline declaration is the owner and is rewritten: {}",
            read(&root.join("index.html"))
        );
        assert_eq!(
            read(&root.join("styles.css")),
            css,
            "and the `#hero` rule is left exactly as authored"
        );
    }

    #[test]
    fn a_selector_naming_both_an_id_and_a_class_matches_the_element_it_describes() {
        // `#hero.cta` is in the supported subset, and it is the case that would
        // break if only the id were threaded through and the classes dropped.
        let html = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <a id=\"hero\" class=\"cta\" data-spool-id=\"spool-cta-primary\">Go</a>\n</body>\n</html>\n";
        let css = ".cta { color: #808080; }\n#hero.cta { background: #222222; }\n";
        let root = id_owned_project("id-and-class", html, css);
        let loaded = open_project(&root).expect("opens");

        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-cta-primary"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");

        assert_only(
            css,
            &read(&root.join("styles.css")),
            &[(
                "#hero.cta { background: #222222;",
                "#hero.cta { background: #ff0000;",
            )],
        );
    }

    #[test]
    fn an_id_rule_shared_by_two_elements_is_reported_rather_than_rewritten() {
        // The exclusive-ownership guard has to keep working now that `#id` rules
        // are visible to it. A rule that governs two elements is not this
        // element's to rewrite, and the count is taken with the same query that
        // found the declaration — id included — so it sees both owners.
        let html = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <a id=\"hero\" class=\"cta\" data-spool-id=\"spool-cta-primary\">Go</a>\n  <a id=\"hero\" class=\"cta\" data-spool-id=\"spool-frame-root\">Also</a>\n</body>\n</html>\n";
        let css = "#hero { background: #222222; }\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}{}",
            node_yaml("spool-cta-primary", None, ""),
            node_yaml("spool-frame-root", None, "")
        );
        let root = project_named("id-shared", html, Some(css), &yaml);
        let loaded = open_project(&root).expect("opens");

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-cta-primary"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");

        assert_eq!(
            outcome.unsupported.len(),
            1,
            "a rule with two owners is reported, not rewritten: {:?}",
            outcome.unsupported
        );
        assert_eq!(
            read(&root.join("styles.css")),
            css,
            "and the shared rule is untouched"
        );
    }

    #[test]
    fn a_variable_backed_value_is_replaced_without_touching_the_token() {
        // `background: var(--accent)` is authored as a reference to a token, and
        // the token may be used by anything. Editing this element's fill
        // replaces the reference here and leaves `:root` exactly as the author
        // wrote it, so every other user of `--accent` is unaffected.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let css_before = read(&root.join("styles.css"));

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-cta-primary"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        assert_only(
            &css_before,
            &read(&root.join("styles.css")),
            &[("background: var(--accent)", "background: #ff0000")],
        );
        assert!(read(&root.join("styles.css")).contains("--accent: #3b5bfd"));

        let reopened = open_project(&root).expect("reopens");
        let cta = reopened
            .runtime
            .objects()
            .iter()
            .find(|object| object.spool_id == node("spool-cta-primary"))
            .expect("cta survives");
        let fill = cta.fill.expect("the authored background is applied");
        assert_eq!(
            (fill.color.red, fill.color.green, fill.color.blue),
            (255, 0, 0)
        );
    }

    #[test]
    fn an_important_declaration_is_reported_rather_than_silently_downgraded() {
        // `!important` wins the cascade, and this editor does not model it.
        // Rewriting the value would quietly drop the flag and let a later
        // declaration start winning, which is not what the user asked for.
        let html = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <a class=\"cta\" data-spool-id=\"spool-cta-primary\">Go</a>\n</body>\n</html>\n";
        let css = ".cta { background: #0000ff !important; }\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}",
            node_yaml("spool-cta-primary", None, "")
        );
        let root = project_named("important", html, Some(css), &yaml);
        let loaded = open_project(&root).expect("opens");

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-cta-primary"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");

        assert_eq!(outcome.unsupported.len(), 1, "{:?}", outcome.unsupported);
        assert!(
            outcome.unsupported[0].reason.contains("!important"),
            "{}",
            outcome.unsupported[0].reason
        );
        assert_eq!(read(&root.join("styles.css")), css);
    }

    #[test]
    fn a_value_that_would_break_the_attribute_is_reported_not_written() {
        // Everything Spool writes is a number, a colour, or a keyword. A quote
        // or a `;` would end the attribute or split the declaration list, so the
        // edit is refused rather than producing broken markup that still parses
        // as *something*.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let html_before = read(&root.join("index.html"));

        for value in [r#"red" onmouseover="alert(1)"#, "1px; position: fixed"] {
            let outcome = save_project(
                &root,
                &loaded.document,
                &[SourceEdit::Style {
                    node: node("spool-text-headline"),
                    property: "letter-spacing".into(),
                    value: value.into(),
                }],
            )
            .expect("save");
            assert_eq!(outcome.unsupported.len(), 1, "{value:?}: {outcome:?}");
            assert_eq!(outcome.written.len(), 0, "{value:?}");
        }
        assert_eq!(read(&root.join("index.html")), html_before);
    }

    // -- Inline styles: merge, never replace.

    #[test]
    fn declarations_the_editor_does_not_understand_survive_an_unrelated_edit() {
        // Three things a naive `split(';')` gets wrong: a custom property, a
        // declaration whose value contains a semicolon inside quotes, and the
        // author's own spacing. None of them is being edited, so none of them
        // may move.
        let html = "<!doctype html>\n<body>\n  <span data-spool-id=\"spool-text-headline\" style=\"z-index:3;--brand:'a;b';  color : red ;content:'x'\">Hi</span>\n</body>\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}",
            node_yaml("spool-text-headline", None, "")
        );
        let root = project_named("odd-inline", html, None, &yaml);
        let loaded = open_project(&root).expect("opens");

        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Geometry {
                node: node("spool-text-headline"),
                placement: Some(Placement::Flow { dx: 5.0, dy: 0.0 }),
                width: None,
                height: None,
            }],
        )
        .expect("save");

        let after = read(&root.join("index.html"));
        for authored in [
            "z-index:3",
            "--brand:'a;b'",
            "  color : red ",
            "content:'x'",
        ] {
            assert!(
                after.contains(authored),
                "{authored:?} should have survived: {after}"
            );
        }
        assert!(after.contains("transform: translate(5px, 0px)"), "{after}");
    }

    #[test]
    fn a_value_replaced_in_place_keeps_its_neighbours_where_they_were() {
        // One declaration changes; the ones around it keep their position, their
        // spacing, and their text.
        let html = "<!doctype html>\n<body>\n  <span data-spool-id=\"spool-text-headline\" style=\"z-index: 3;opacity:0.9;letter-spacing:1px\">Hi</span>\n</body>\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}",
            node_yaml("spool-text-headline", None, "")
        );
        let root = project_named("inline-order", html, None, &yaml);
        let loaded = open_project(&root).expect("opens");

        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-text-headline"),
                property: "opacity".into(),
                value: "0.5".into(),
            }],
        )
        .expect("save");

        assert_only(
            html,
            &read(&root.join("index.html")),
            &[("opacity:0.9", "opacity: 0.5")],
        );
    }

    // -- Repeated and multi-region edits in one save.

    #[test]
    fn two_edits_that_change_length_in_both_directions_both_land() {
        // One edit makes the file longer before another edit's span and another
        // makes it shorter before a third's. Applying them in the wrong order
        // would shift a range out from under the next write.
        let html = "<!doctype html>\n<body>\n  <main data-spool-id=\"spool-frame-root\">\n    <h1 data-spool-id=\"spool-text-headline\">Title</h1>\n    <p data-spool-id=\"spool-cta-primary\">Button</p>\n  </main>\n</body>\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}{}{}",
            node_yaml(
                "spool-frame-root",
                None,
                "\"spool-text-headline\", \"spool-cta-primary\""
            ),
            node_yaml("spool-text-headline", Some("spool-frame-root"), ""),
            node_yaml("spool-cta-primary", Some("spool-frame-root"), "")
        );
        let root = project_named("shifting", html, None, &yaml);
        let loaded = open_project(&root).expect("opens");

        save_project(
            &root,
            &loaded.document,
            &[
                SourceEdit::Text {
                    node: node("spool-text-headline"),
                    text: "A much longer headline than the one it replaces".into(),
                },
                SourceEdit::Text {
                    node: node("spool-cta-primary"),
                    text: "Go".into(),
                },
            ],
        )
        .expect("save");

        assert_only(
            html,
            &read(&root.join("index.html")),
            &[
                ("Button", "Go"),
                ("Title", "A much longer headline than the one it replaces"),
            ],
        );
    }

    #[test]
    fn two_edits_to_the_same_text_resolve_to_the_one_asked_for_last() {
        // Deterministic, and stated rather than accidental: within one save the
        // last request for a span is the one that is written.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let html_before = read(&root.join("index.html"));

        save_project(
            &root,
            &loaded.document,
            &[
                SourceEdit::Text {
                    node: node("spool-text-headline"),
                    text: "First".into(),
                },
                SourceEdit::Text {
                    node: node("spool-text-headline"),
                    text: "Second".into(),
                },
            ],
        )
        .expect("save");

        assert_only(
            &html_before,
            &read(&root.join("index.html")),
            &[("Design in source, structure in Spool", "Second")],
        );
    }

    // -- Several files in one project.

    #[test]
    fn edits_to_two_pages_each_land_in_their_own_file() {
        let home = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <h1 data-spool-id=\"spool-home-title\">Home</h1>\n</body>\n</html>\n";
        let about = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <h1 data-spool-id=\"spool-about-title\">About</h1>\n</body>\n</html>\n";
        let css = "h1 { color: #101010; }\n";
        let yaml = "version: 1\nnodes:\n  - id: \"spool-home-title\"\n    name: \"Home\"\n    kind: \"text\"\n    parent: null\n    file: \"pages/home.html\"\n    selector: \"[data-spool-id=\\\"spool-home-title\\\"]\"\n    children: []\n  - id: \"spool-about-title\"\n    name: \"About\"\n    kind: \"text\"\n    parent: null\n    file: \"pages/about.html\"\n    selector: \"[data-spool-id=\\\"spool-about-title\\\"]\"\n    children: []\n".to_owned();
        let root = project_files(
            "two-pages",
            &[
                ("lamine.yaml", yaml.as_str()),
                ("pages/home.html", home),
                ("pages/about.html", about),
                ("styles.css", css),
            ],
        );
        let loaded = open_project(&root).expect("opens");

        let outcome = save_project(
            &root,
            &loaded.document,
            &[
                SourceEdit::Text {
                    node: node("spool-home-title"),
                    text: "Welcome".into(),
                },
                SourceEdit::Text {
                    node: node("spool-about-title"),
                    text: "Who we are".into(),
                },
            ],
        )
        .expect("save");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        assert_only(
            home,
            &read(&root.join("pages/home.html")),
            &[("Home", "Welcome")],
        );
        assert_only(
            about,
            &read(&root.join("pages/about.html")),
            &[("About", "Who we are")],
        );
        assert_eq!(
            read(&root.join("styles.css")),
            css,
            "the shared sheet is untouched"
        );
        // And the pages still resolve, which is the point of writing into the
        // file each identity lives in.
        let reopened = open_project(&root).expect("reopens");
        assert_eq!(reopened.document.structure.nodes.len(), 2);
    }

    #[test]
    fn a_rule_shared_between_two_pages_is_reported_rather_than_rewritten() {
        // The same stylesheet styles both pages, so one declaration governs two
        // elements in two files. Editing it from either page would change the
        // other, so it is diagnosed.
        // The stylesheet lives at the project root and each page reaches it the
        // way a browser would: from its own directory, upwards.
        let page = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"../styles.css\" /></head>\n<body>\n  <h1 class=\"title\" data-spool-id=\"spool-title-{n}\">{t}</h1>\n</body>\n</html>\n";
        let css = ".title { color: #101010; }\n";
        let home = page.replace("{n}", "home").replace("{t}", "Home");
        let about = page.replace("{n}", "about").replace("{t}", "About");
        let yaml = "version: 1\nnodes:\n  - id: \"spool-title-home\"\n    name: \"Home\"\n    kind: \"text\"\n    parent: null\n    file: \"pages/home.html\"\n    selector: \"[data-spool-id=\\\"spool-title-home\\\"]\"\n    children: []\n  - id: \"spool-title-about\"\n    name: \"About\"\n    kind: \"text\"\n    parent: null\n    file: \"pages/about.html\"\n    selector: \"[data-spool-id=\\\"spool-title-about\\\"]\"\n    children: []\n".to_owned();
        let root = project_files(
            "cross-page",
            &[
                ("lamine.yaml", yaml.as_str()),
                ("pages/home.html", home.as_str()),
                ("pages/about.html", about.as_str()),
                ("styles.css", css),
            ],
        );
        let loaded = open_project(&root).expect("opens");

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-title-home"),
                property: "color".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");

        assert_eq!(outcome.unsupported.len(), 1, "{:?}", outcome.unsupported);
        assert_eq!(read(&root.join("styles.css")), css);
    }

    #[test]
    fn a_failure_part_way_through_reports_the_files_that_did_land() {
        // A multi-file save is not atomic, and this is the case that proves the
        // code says so rather than implying it.
        //
        // A geometry edit lands in the markup and a fill edit in a stylesheet
        // under `assets/`, so the save writes `index.html` and then fails on the
        // stylesheet — because that directory is made read-only after the
        // project was opened, so the sibling temp file cannot be created. Reading
        // is unaffected, which is what makes this a *write* failure rather than
        // an unreadable project.
        //
        // The error has to carry the fact that `index.html` is already on disk. A
        // caller that concluded "the save failed, so nothing changed" would keep
        // per-file state that no longer describes the file, and write its next
        // edit against it twice.
        let html = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"assets/styles.css\" /></head>\n<body>\n  <a class=\"cta\" data-spool-id=\"spool-cta-primary\">Go</a>\n</body>\n</html>\n";
        let css = ".cta { background: #222222; }\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}",
            node_yaml("spool-cta-primary", None, "")
        );
        let root = project_files(
            "partial-write",
            &[
                ("lamine.yaml", yaml.as_str()),
                ("index.html", html),
                ("assets/styles.css", css),
            ],
        );
        let loaded = open_project(&root).expect("opens");

        let assets = root.join("assets");
        let writable = std::fs::Permissions::from_mode(0o755);
        let blocked_mode = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(&assets, blocked_mode)
            .expect("make the stylesheet directory read-only");
        assert!(
            std::fs::File::create(assets.join("probe.tmp")).is_err(),
            "the directory really does refuse a new file, so the failure below is \
             the filesystem's and not this code's"
        );
        let outcome = save_project(
            &root,
            &loaded.document,
            &[
                SourceEdit::Style {
                    node: node("spool-cta-primary"),
                    property: "background".into(),
                    value: "#ff0000".into(),
                },
                SourceEdit::Geometry {
                    node: node("spool-cta-primary"),
                    placement: Some(Placement::Flow { dx: 8.0, dy: 0.0 }),
                    width: None,
                    height: None,
                },
            ],
        );
        std::fs::set_permissions(&assets, writable).expect("restore permissions");

        let failure = outcome.expect_err("the blocked directory refuses the write");
        assert!(
            matches!(*failure.source, BundleError::Io { .. }),
            "and it is an I/O failure, not a silent success: {:?}",
            failure.source
        );
        assert_eq!(
            failure.written,
            vec![root.join("index.html")],
            "the file replaced before the failure is named, so a caller can tell \
             the save was not a no-op"
        );
        assert!(
            read(&root.join("index.html")).contains("translate(8px, 0px)"),
            "and that file really is on disk with the edit in it: {}",
            read(&root.join("index.html"))
        );
        assert_eq!(
            read(&root.join("assets/styles.css")),
            css,
            "while the stylesheet is untouched"
        );
        assert!(
            !failure
                .written
                .iter()
                .any(|path| path.ends_with("lamine.yaml")),
            "metadata is written last, so a failure before it leaves it alone"
        );
    }

    #[test]
    fn a_stylesheet_order_that_is_not_its_name_order_still_resolves_the_winner() {
        // The cascade across files is document order and last-wins: the sheet a
        // page links *last* overrides the earlier ones, and that is the only
        // declaration an edit may rewrite.
        //
        // Three sheets, because two cannot separate the two candidate orders.
        // Alphabetical is `a, m, z`; the page links `z, a, m`, so `m.css` wins
        // the cascade while `a.css` sorts first. An implementation that sorted by
        // name and took the first match would rewrite `a.css` — a declaration
        // that loses, so the element would go on rendering `m.css` and the edit
        // would appear to do nothing.
        let html = "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"z.css\" /><link rel=\"stylesheet\" href=\"a.css\" /><link rel=\"stylesheet\" href=\"m.css\" /></head>\n<body>\n  <a class=\"cta\" data-spool-id=\"spool-cta-primary\">Go</a>\n</body>\n</html>\n";
        // All three match this element and disagree on the value, so which one
        // won is visible in the render.
        let z_css = ".cta { background: #0000ff; }\n";
        let a_css = ".cta { background: #00ff00; }\n";
        let m_css = ".cta { background: #ff00ff; }\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}",
            node_yaml("spool-cta-primary", None, "")
        );
        let root = project_files(
            "sheet-order",
            &[
                ("lamine.yaml", yaml.as_str()),
                ("index.html", html),
                ("z.css", z_css),
                ("a.css", a_css),
                ("m.css", m_css),
            ],
        );
        let loaded = open_project(&root).expect("opens");

        // What the renderer resolved, which is the answer save has to match.
        let cta = loaded
            .runtime
            .objects()
            .iter()
            .find(|object| object.spool_id.as_str() == "spool-cta-primary")
            .expect("projected");
        assert_eq!(
            cta.fill.map(|fill| fill.color.to_rgb()),
            Some(0xff_00_ff),
            "`m.css` is linked last, so it is the declaration the cascade chose"
        );

        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-cta-primary"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");

        assert_eq!(
            read(&root.join("m.css")),
            ".cta { background: #ff0000; }\n",
            "the edit lands in the rule the cascade actually chose"
        );
        assert_eq!(
            read(&root.join("a.css")),
            a_css,
            "and the rule that merely sorted first is left as the author wrote it"
        );
        assert_eq!(read(&root.join("z.css")), z_css);
    }

    // -- Known limitations, pinned so they cannot drift silently.

    #[test]
    fn a_deleted_object_keeps_its_authored_element_and_comes_back() {
        // Deletion is a runtime-only operation today: the metadata node and the
        // authored element both survive, so reopening brings the object back.
        // That is the safe direction — the project never becomes invalid source
        // — but it is a real limitation, so it is pinned here rather than left
        // to be discovered by a user.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let html_before = read(&root.join("index.html"));

        // No edit mentions the CTA: the editor's save iterates what is on the
        // canvas, and a deleted object produces nothing to write.
        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Text {
                node: node("spool-text-headline"),
                text: "Edited".into(),
            }],
        )
        .expect("save");

        assert_only(
            &html_before,
            &read(&root.join("index.html")),
            &[("Design in source, structure in Spool", "Edited")],
        );
        assert_eq!(
            open_project(&root)
                .expect("still opens")
                .runtime
                .objects()
                .len(),
            3,
            "all three nodes still exist, including one the editor no longer draws"
        );
    }

    #[test]
    fn created_objects_have_nowhere_to_go_and_are_reported_by_the_editor() {
        // The counterpart: an object with no authored element cannot be written.
        // `CanvasView::save_project` reports it, and the save layer's contract is
        // that an edit naming a node the document does not hold is reported too.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Text {
                node: NodeId::new("spool-drawn-here").expect("legal id"),
                text: "New".into(),
            }],
        )
        .expect("save");

        assert_eq!(outcome.unsupported.len(), 1, "{:?}", outcome.unsupported);
        assert_eq!(outcome.unsupported[0].kind, "unknown-node");
        assert!(outcome.written.is_empty());
    }

    #[test]
    fn text_written_into_an_element_is_escaped_rather_than_injected() {
        // The author types markup characters as text. They have to be written as
        // entities: emitting them raw would invent an element nobody authored,
        // and the next open would parse a different document than the one that
        // was saved.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");

        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Text {
                node: node("spool-text-headline"),
                text: "a < b && b > c".into(),
            }],
        )
        .expect("save");

        let html = read(&root.join("index.html"));
        assert!(
            html.contains(">a &lt; b &amp;&amp; b &gt; c<"),
            "written as entities: {html}"
        );
        // The save left a project that still parses to one element with one run
        // of text, so the loop is still closed: the next text edit finds a range
        // again. (Decoding the entities is the renderer's job, not this layer's.)
        let reopened = open_project(&root).expect("reopens");
        assert!(reopened
            .runtime
            .objects()
            .iter()
            .any(|object| object.spool_id == node("spool-text-headline")));
        let range = SourceIndex::parse(&html)
            .find(&node("spool-text-headline"))
            .and_then(|binding| binding.text_range.clone())
            .expect("the element still owns a single run of text");
        assert_eq!(&html[range], "a &lt; b &amp;&amp; b &gt; c");
    }

    #[test]
    fn a_rename_that_only_changes_metadata_leaves_source_untouched() {
        // The other half of the save: HTML and CSS are not the only authored
        // state, and a rename that never reaches `lamine.yaml` is a rename the
        // user cannot see survive. Source is not collateral.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let html_before = read(&root.join("index.html"));
        let css_before = read(&root.join("styles.css"));

        let mut renamed = loaded.document.clone();
        renamed.structure.nodes[1].name = "Hero headline".into();
        let outcome = save_project(&root, &renamed, &[]).expect("save");

        assert_eq!(
            outcome.written,
            vec![root.join(crate::project_bundle::METADATA_FILE)],
            "only the metadata was written"
        );
        assert_eq!(read(&root.join("index.html")), html_before);
        assert_eq!(read(&root.join("styles.css")), css_before);
        assert_eq!(
            open_project(&root)
                .expect("reopens")
                .document
                .structure
                .nodes[1]
                .name,
            "Hero headline"
        );
    }

    #[test]
    fn a_text_edit_survives_open_edit_save_reopen() {
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");

        let edited = "Design in source, structure in Spool, saved";
        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Text {
                node: node("spool-text-headline"),
                text: edited.into(),
            }],
        )
        .expect("save succeeds");
        assert!(outcome.unsupported.is_empty(), "the edit was supported");
        assert_eq!(outcome.written.len(), 1, "only the HTML was rewritten");

        // Reopen from disk, not from memory.
        let reopened = open_project(&root).expect("reopens");
        let headline = reopened
            .runtime
            .objects()
            .iter()
            .find(|o| o.spool_id == node("spool-text-headline"))
            .expect("headline survives");
        assert_eq!(headline.text_content.as_deref(), Some(edited));
    }

    #[test]
    fn saving_a_text_edit_leaves_every_other_byte_alone() {
        let root = scratch("landing");
        let before = std::fs::read_to_string(root.join("index.html")).unwrap();
        let loaded = open_project(&root).expect("opens");

        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Text {
                node: node("spool-text-headline"),
                text: "Changed".into(),
            }],
        )
        .expect("save");

        let after = std::fs::read_to_string(root.join("index.html")).unwrap();
        // Everything outside the headline's text is byte-identical.
        let restored = after.replace("Changed", "Design in source, structure in Spool");
        assert_eq!(restored, before, "only the intended text changed");

        // The stylesheet was not touched at all.
        let css_before = std::fs::read_to_string(root.join("styles.css")).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("styles.css")).unwrap(),
            css_before
        );
    }

    #[test]
    fn a_geometry_edit_survives_the_round_trip() {
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        // The CTA is in the normal flow, so there is no authored position to
        // overwrite. The baseline is where the flow puts it, and the editor's
        // move is an offset from there.
        let authored = position_of(&loaded.runtime, &node("spool-cta-primary"));

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Geometry {
                node: node("spool-cta-primary"),
                placement: Some(Placement::Flow {
                    dx: 40.0,
                    dy: 120.0,
                }),
                width: Some(220.0),
                height: Some(48.0),
            }],
        )
        .expect("save");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        let html = std::fs::read_to_string(root.join("index.html")).expect("read html");
        assert!(
            html.contains("transform: translate(40px, 120px)"),
            "the move stayed in the flow: {html}"
        );
        assert!(
            !html.contains("position: absolute"),
            "a flow element must not be lifted out of it: {html}"
        );

        let reopened = open_project(&root).expect("reopens");
        let cta = reopened
            .runtime
            .objects()
            .iter()
            .find(|o| o.spool_id == node("spool-cta-primary"))
            .expect("cta survives");
        // Explicit dimensions are read as the border box, which is what the
        // editor measures and writes. Padding stays part of the box, so the
        // width that comes back is the width that was written.
        assert!(
            (cta.size.width - 220.0).abs() < 0.01,
            "the authored width survived, got {}",
            cta.size.width
        );
        assert!((cta.size.height - 48.0).abs() < 0.01);
        assert!(
            (cta.position.x - (authored.x + 40.0)).abs() < 0.01
                && (cta.position.y - (authored.y + 120.0)).abs() < 0.01,
            "the element came back where the editor left it: {} vs {}",
            cta.position,
            gpui::point(authored.x + 40.0, authored.y + 120.0)
        );

        // Re-saving what came back must not drift: a project opened, edited and
        // saved repeatedly has to converge rather than grow by its padding each
        // round trip. The bytes are compared too, because a duplicated style
        // attribute still parses to the same geometry — only the file on disk
        // shows the damage. The reopened project has not been moved again, so
        // the editor has no move to write — only the size it measured.
        let after_first_save = std::fs::read_to_string(root.join("index.html")).expect("read html");
        let measured = cta.size;
        save_project(
            &root,
            &reopened.document,
            &[SourceEdit::Geometry {
                node: node("spool-cta-primary"),
                placement: None,
                width: Some(measured.width),
                height: Some(measured.height),
            }],
        )
        .expect("second save");
        let again = open_project(&root).expect("reopens again");
        let cta = again
            .runtime
            .objects()
            .iter()
            .find(|o| o.spool_id == node("spool-cta-primary"))
            .expect("cta survives");
        assert!(
            (cta.size.width - measured.width).abs() < 0.01
                && (cta.size.height - measured.height).abs() < 0.01,
            "a second round trip is a fixed point, got {}x{}",
            cta.size.width,
            cta.size.height
        );
        assert!(
            (cta.position.x - (authored.x + 40.0)).abs() < 0.01
                && (cta.position.y - (authored.y + 120.0)).abs() < 0.01,
            "and the offset did not accumulate: {}",
            cta.position
        );
        assert_eq!(
            std::fs::read_to_string(root.join("index.html")).expect("read html"),
            after_first_save,
            "and the second save changed no bytes at all"
        );
        assert_eq!(
            after_first_save.matches("style=").count(),
            1,
            "one element carries one style attribute: {after_first_save}"
        );
    }

    #[test]
    fn a_style_edit_rewrites_the_owning_declaration_only() {
        let root = scratch("landing");
        let before = std::fs::read_to_string(root.join("styles.css")).unwrap();
        let loaded = open_project(&root).expect("opens");

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-cta-primary"),
                property: "background".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        let after = std::fs::read_to_string(root.join("styles.css")).unwrap();
        assert!(after.contains("#ff0000"), "the declaration was rewritten");
        assert_eq!(
            after.matches("background").count(),
            before.matches("background").count(),
            "no rule was added or removed"
        );

        let reopened = open_project(&root).expect("reopens");
        let cta = reopened
            .runtime
            .objects()
            .iter()
            .find(|o| o.spool_id == node("spool-cta-primary"))
            .expect("cta survives");
        let fill = cta.fill.expect("the authored background is applied");
        assert_eq!(
            (fill.color.red, fill.color.green, fill.color.blue),
            (255, 0, 0)
        );
    }

    #[test]
    fn a_moved_flow_element_does_not_mention_its_parent() {
        // A flow element has no position of its own, so a move says nothing
        // about where its parent is: the two stay independent, and moving the
        // parent later never requires rewriting the child.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let before = position_of(&loaded.runtime, &node("spool-cta-primary"));

        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Geometry {
                node: node("spool-cta-primary"),
                placement: Some(Placement::Flow { dx: 0.0, dy: 10.0 }),
                width: None,
                height: None,
            }],
        )
        .expect("save succeeds");

        let html = std::fs::read_to_string(root.join("index.html")).expect("read html");
        assert!(html.contains("transform: translate(0px, 10px)"), "{html}");
        assert!(!html.contains("left:"), "no absolute position: {html}");

        let reopened = open_project(&root).expect("reopens");
        let cta = position_of(&reopened.runtime, &node("spool-cta-primary"));
        assert!(
            (cta.y - (before.y + 10.0)).abs() < 0.01,
            "{cta} vs {before}"
        );
        let frame = position_of(&reopened.runtime, &node("spool-frame-root"));
        assert_eq!(frame, gpui::point(0.0, 0.0), "the parent did not move");
    }

    #[test]
    fn an_element_the_author_took_out_of_the_flow_keeps_its_own_coordinates() {
        // The other half of the policy: a box that is already out of the flow is
        // written with `left`/`top` from its containing block, and `position` is
        // not restated — the author already said so, and repeating it locally
        // would be a second declaration making the same promise.
        let html = "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-frame-root\" style=\"position: absolute; left: 100px; top: 60px; width: 200px\">\n    <span data-spool-id=\"spool-text-headline\" style=\"position: absolute; left: 4px; top: 6px\">Hello</span>\n  </div>\n</body>\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}{}",
            node_yaml("spool-frame-root", None, "\"spool-text-headline\""),
            node_yaml("spool-text-headline", Some("spool-frame-root"), "")
        );
        let root = project_named("absolute", html, None, &yaml);
        let loaded = open_project(&root).expect("opens");
        assert!(is_out_of_flow(&loaded.document, &loaded.document.structure.nodes[1]).unwrap());

        // The child is absolute, so its position is written relative to the
        // parent's containing block. Writing the child's world coordinate would
        // put it twice as far down once the parent itself moved.
        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Geometry {
                node: node("spool-text-headline"),
                placement: Some(Placement::ContainingBlock { x: 12.0, y: 20.0 }),
                width: None,
                height: None,
            }],
        )
        .expect("save succeeds");

        let after = std::fs::read_to_string(root.join("index.html")).expect("read html");
        assert!(
            after.contains("left: 12px") && after.contains("top: 20px"),
            "{after}"
        );
        assert!(
            after.contains(r#"<span data-spool-id="spool-text-headline" style="position: absolute; left: 12px; top: 20px">"#),
            "an already-absolute element keeps its single `position`, with only the \
             coordinates rewritten: {after}"
        );

        let reopened = open_project(&root).expect("reopens");
        // Parent at 100/60 plus a child at 12/20 from it.
        assert_eq!(
            position_of(&reopened.runtime, &node("spool-text-headline")),
            gpui::point(112.0, 80.0)
        );
    }

    #[test]
    fn a_second_move_adds_to_the_offset_the_author_wrote() {
        // Moves compose. A drag is a change from where the element already was,
        // so saving twice must not double the offset — and an author's own
        // `transform` is part of that baseline, not something to overwrite.
        let html = "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-frame-root\">\n    <span data-spool-id=\"spool-text-headline\" style=\"transform: translate(10px, 0px)\">Hello</span>\n  </div>\n</body>\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}{}",
            node_yaml("spool-frame-root", None, "\"spool-text-headline\""),
            node_yaml("spool-text-headline", Some("spool-frame-root"), "")
        );
        let root = project_named("compose", html, None, &yaml);
        let loaded = open_project(&root).expect("opens");
        let before = position_of(&loaded.runtime, &node("spool-text-headline"));

        // Each session the user drags the element 10px to the right of where it
        // was when that session opened, which is the only information a save
        // gets. Two sessions, two drags, one authored offset.
        for _ in 0..2 {
            let loaded = open_project(&root).expect("opens");
            save_project(
                &root,
                &loaded.document,
                &[SourceEdit::Geometry {
                    node: node("spool-text-headline"),
                    placement: Some(Placement::Flow { dx: 10.0, dy: 0.0 }),
                    width: None,
                    height: None,
                }],
            )
            .expect("save succeeds");
        }

        let after = std::fs::read_to_string(root.join("index.html")).expect("read html");
        assert!(
            after.contains("transform: translate(30px, 0px)"),
            "two moves of the same distance add to the authored offset instead of \
             replacing it: {after}"
        );
        assert_eq!(
            position_of(
                &open_project(&root).expect("reopens").runtime,
                &node("spool-text-headline")
            ),
            gpui::point(before.x + 20.0, before.y),
            "and the element sits where the editor left it"
        );
    }

    #[test]
    fn a_transform_the_editor_cannot_read_is_reported_rather_than_overwritten() {
        // A rotation is an authored decision. Rewriting it as a translate would
        // delete it, so the save says so and leaves the file alone.
        let html = "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-frame-root\">\n    <span data-spool-id=\"spool-text-headline\" style=\"transform: rotate(4deg)\">Hello</span>\n  </div>\n</body>\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}{}",
            node_yaml("spool-frame-root", None, "\"spool-text-headline\""),
            node_yaml("spool-text-headline", Some("spool-frame-root"), "")
        );
        let root = project_named("rotated", html, None, &yaml);
        let loaded = open_project(&root).expect("opens");

        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Geometry {
                node: node("spool-text-headline"),
                placement: Some(Placement::Flow { dx: 8.0, dy: 8.0 }),
                width: None,
                height: None,
            }],
        )
        .expect("save succeeds");
        assert_eq!(outcome.unsupported.len(), 1, "{:?}", outcome.unsupported);
        assert!(
            outcome.unsupported[0].reason.contains("rotate(4deg)"),
            "the reason names the conflict: {:?}",
            outcome.unsupported[0]
        );
        assert_eq!(
            std::fs::read_to_string(root.join("index.html")).expect("read html"),
            html,
            "and the file is byte-for-byte untouched"
        );
    }

    // -- Created objects: the authored half.

    /// A project with deliberately unusual indentation, so "indent the new
    /// element like its siblings" and "indent it two spaces past the parent" are
    /// different answers. Four-space children under a column-zero parent is the
    /// only way to tell them apart.
    fn wide_indent_project(name: &str) -> PathBuf {
        let html = "<body>\n<main data-spool-id=\"spool-frame-root\">\n    <h1 data-spool-id=\"spool-frame-head\">Title</h1>\n</main>\n</body>\n";
        let yaml = format!(
            "version: 1\nnodes:\n{}{}",
            node_yaml("spool-frame-root", None, "\"spool-frame-head\""),
            node_yaml("spool-frame-head", Some("spool-frame-root"), "")
        );
        project_files(
            name,
            &[("lamine.yaml", yaml.as_str()), ("index.html", html)],
        )
    }

    /// The document as the canvas leaves it after a creation: the node is already
    /// in the structure, and what is left for the save is the authored element.
    fn with_created(
        mut loaded: crate::project_open::LoadedProject,
        id: &str,
        parent: &str,
    ) -> crate::project_open::LoadedProject {
        loaded.document.structure.nodes.push(StructuralNode {
            id: node(id),
            name: "Created".to_owned(),
            kind: "rectangle".to_owned(),
            parent: Some(node(parent)),
            children: Vec::new(),
            source: crate::source_document::SourceBinding {
                file: "index.html".to_owned(),
                selector: format!("[data-spool-id=\"{id}\"]"),
            },
        });
        loaded
    }

    fn new_rectangle(id: &str) -> SourceEdit {
        SourceEdit::Create {
            node: node(id),
            element: NewElement {
                tag: "div".to_owned(),
                text: None,
                declarations: vec![("width".to_owned(), "10px".to_owned())],
            },
        }
    }

    #[test]
    fn a_created_element_is_indented_like_the_authored_children_not_the_parent() {
        let root = wide_indent_project("wide-indent");
        let loaded = with_created(
            open_project(&root).expect("opens"),
            "spool-new",
            "spool-frame-root",
        );

        let outcome = save_project(&root, &loaded.document, &[new_rectangle("spool-new")])
            .expect("save succeeds");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        let html = std::fs::read_to_string(root.join("index.html")).expect("html");
        let created = html
            .lines()
            .find(|line| line.contains("spool-new"))
            .unwrap_or_else(|| panic!("the element was authored: {html}"));
        assert!(
            created.starts_with("    "),
            "four spaces, like the sibling it was created beside, not two: {created:?}"
        );
        assert!(
            html.contains("\n</main>"),
            "and the parent's close tag kept its own line: {html}"
        );
    }

    #[test]
    fn two_created_elements_land_inside_the_parent_in_creation_order() {
        // Both children share one insertion point. Written as separate splices
        // they would collapse, so only one of the two objects would exist.
        let root = wide_indent_project("two-children");
        let mut document = open_project(&root).expect("opens").document;
        for id in ["spool-a", "spool-b"] {
            document.structure.nodes.push(StructuralNode {
                id: node(id),
                name: id.to_owned(),
                kind: "rectangle".to_owned(),
                parent: Some(node("spool-frame-root")),
                children: Vec::new(),
                source: crate::source_document::SourceBinding {
                    file: "index.html".to_owned(),
                    selector: format!("[data-spool-id=\"{id}\"]"),
                },
            });
        }

        save_project(
            &root,
            &document,
            &[new_rectangle("spool-a"), new_rectangle("spool-b")],
        )
        .expect("save succeeds");

        let html = std::fs::read_to_string(root.join("index.html")).expect("html");
        let a = html.find("spool-a").expect("first authored");
        let b = html.find("spool-b").expect("second authored");
        assert!(a < b, "creation order is source order: {html}");
        assert_eq!(html.matches("data-spool-id=").count(), 4, "both were added");
    }

    #[test]
    fn written_lengths_are_readable_rather_than_binary_noise() {
        // Layout runs in f32, so the editor's own geometry arrives as
        // `22.399994`. Authoring that into the document is accurate, ugly, and
        // makes every save look like a change even when nothing did.
        assert_eq!(css_length(22.399994), "22.4");
        assert_eq!(css_length(40.0), "40");
        assert_eq!(css_length(0.0), "0");
        assert_eq!(css_length(-3.5), "-3.5");
        assert_eq!(
            merge_inline_declarations(
                "",
                &[
                    ("left".to_owned(), "40px".to_owned()),
                    ("top".to_owned(), "22.4px".to_owned()),
                    ("width".to_owned(), "300px".to_owned()),
                    ("height".to_owned(), "46.4px".to_owned()),
                ]
            ),
            "left: 40px; top: 22.4px; width: 300px; height: 46.4px"
        );
        // A position alone must not pin a size the user never changed.
        assert_eq!(
            merge_inline_declarations(
                "",
                &[
                    ("left".to_owned(), "10px".to_owned()),
                    ("top".to_owned(), "20px".to_owned())
                ]
            ),
            "left: 10px; top: 20px"
        );
    }

    #[test]
    fn a_style_attribute_value_range_covers_the_value_and_nothing_else() {
        for (open_tag, expected) in [
            (
                r#"<a class="cta" style="color: red">"#,
                Some("color: red".to_owned()),
            ),
            (
                r#"<a style='color: red' data-x="1">"#,
                Some("color: red".to_owned()),
            ),
            // Spaces around the `=` are legal and must not break the range.
            (r#"<a style = "color: red">"#, Some("color: red".to_owned())),
            (r#"<a style=color:red>"#, Some("color:red".to_owned())),
            // A different attribute that merely contains the word.
            (r#"<a data-style-note="nope">"#, None),
            (r#"<a class="cta">"#, None),
        ] {
            let range = style_attribute_value(open_tag);
            assert_eq!(
                range
                    .as_ref()
                    .map(|range| open_tag[range.clone()].to_owned()),
                expected,
                "for {open_tag}"
            );
        }
    }

    #[test]
    fn a_rename_made_in_the_editor_is_written_to_the_metadata() {
        // The other half of the save: HTML and CSS are not the only authored
        // state, and a rename that never reaches `lamine.yaml` is a rename the
        // user cannot see survive.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let before = std::fs::read_to_string(root.join("lamine.yaml")).expect("read metadata");

        let mut renamed = loaded.document.clone();
        renamed.structure.nodes[1].name = "Hero headline".into();
        let outcome = save_project(&root, &renamed, &[]).expect("save succeeds");

        assert_eq!(
            outcome.written,
            vec![root.join(crate::project_bundle::METADATA_FILE)],
            "only the metadata file was written"
        );
        let after = std::fs::read_to_string(root.join("lamine.yaml")).expect("read metadata");
        assert_ne!(after, before);
        assert!(after.contains("Hero headline"));
        assert_eq!(
            open_project(&root)
                .expect("reopens")
                .document
                .structure
                .nodes[1]
                .name,
            "Hero headline",
            "and the name comes back from disk"
        );
        // Identity, hierarchy, and every authored file survive the rename.
        assert_eq!(
            open_project(&root)
                .expect("reopens")
                .document
                .structure
                .nodes
                .len(),
            3
        );
        assert_eq!(
            std::fs::read_to_string(root.join("index.html")).expect("read html"),
            loaded.document.sources["index.html"],
            "the authored source is untouched by a metadata-only save"
        );
    }

    #[test]
    fn two_edits_to_the_same_element_both_land() {
        // Text and geometry on one element are two separate spans in one file,
        // applied back to front so the first does not shift the second.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let target = node("spool-cta-primary");
        let flowed = position_of(&loaded.runtime, &target);
        save_project(
            &root,
            &loaded.document,
            &[
                SourceEdit::Text {
                    node: target.clone(),
                    text: "Start now".into(),
                },
                SourceEdit::Geometry {
                    node: target.clone(),
                    placement: Some(Placement::Flow { dx: 10.0, dy: 20.0 }),
                    width: Some(200.0),
                    height: Some(40.0),
                },
            ],
        )
        .expect("save succeeds");

        let reopened = open_project(&root).expect("reopens");
        let cta = reopened
            .runtime
            .objects()
            .iter()
            .find(|object| object.spool_id == target)
            .expect("cta survives");
        assert_eq!(cta.text_content.as_deref(), Some("Start now"));
        assert!(
            (cta.position.x - (flowed.x + 10.0)).abs() < 0.01
                && (cta.position.y - (flowed.y + 20.0)).abs() < 0.01,
            "the move is an offset on top of where the flow put it: {} vs {flowed}",
            cta.position
        );
        assert_eq!(cta.size.width, 200.0);
        let html = std::fs::read_to_string(root.join("index.html")).expect("read html");
        assert_eq!(
            html.matches("style=").count(),
            1,
            "one style attribute, not two: {html}"
        );
        assert!(
            html.contains(">Start now</a>"),
            "and the closing tag survived"
        );
    }

    #[test]
    fn a_style_edit_with_no_authored_owner_becomes_a_local_declaration() {
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let before = std::fs::read_to_string(root.join("styles.css")).unwrap();

        // Nothing in this project declares `letter-spacing` for the headline.
        // The policy is to give the element its own declaration rather than to
        // invent a rule in the stylesheet or to refuse the edit.
        let outcome = save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-text-headline"),
                property: "letter-spacing".into(),
                value: "2px".into(),
            }],
        )
        .expect("save succeeds");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        let html = std::fs::read_to_string(root.join("index.html")).unwrap();
        assert!(
            html.contains(
                r#"<h1 data-spool-id="spool-text-headline" style="letter-spacing: 2px">"#
            ),
            "the declaration landed on the element itself: {html}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("styles.css")).unwrap(),
            before,
            "and no rule was invented in the stylesheet"
        );
    }

    #[test]
    fn editing_an_inherited_property_writes_it_on_the_element_not_on_the_ancestor() {
        // The headline's colour comes from `body`. Editing the headline must not
        // restyle every element that inherits from body — that would change
        // objects the user never selected.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let css_before = std::fs::read_to_string(root.join("styles.css")).unwrap();

        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Style {
                node: node("spool-text-headline"),
                property: "color".into(),
                value: "#ff0000".into(),
            }],
        )
        .expect("save succeeds");

        let html = std::fs::read_to_string(root.join("index.html")).unwrap();
        assert!(
            html.contains(r#"<h1 data-spool-id="spool-text-headline" style="color: #ff0000">"#),
            "the headline now owns its colour: {html}"
        );
        assert!(
            !html.contains(r#"<main data-spool-id="spool-frame-root" style="color"#),
            "and the frame that inherited it was not touched: {html}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("styles.css")).unwrap(),
            css_before,
            "body's rule is untouched"
        );
    }

    #[test]
    fn a_geometry_edit_and_a_style_edit_share_one_inline_attribute() {
        // Both target the same element. Written separately they would either
        // duplicate the attribute or the second would erase the first.
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let target = node("spool-cta-primary");
        save_project(
            &root,
            &loaded.document,
            &[
                SourceEdit::Geometry {
                    node: target.clone(),
                    placement: Some(Placement::Flow { dx: 30.0, dy: 40.0 }),
                    width: None,
                    height: None,
                },
                SourceEdit::Style {
                    node: target.clone(),
                    property: "border-radius".into(),
                    value: "12px".into(),
                },
                SourceEdit::Style {
                    node: target.clone(),
                    property: "opacity".into(),
                    value: "0.5".into(),
                },
            ],
        )
        .expect("save succeeds");

        let html = std::fs::read_to_string(root.join("index.html")).unwrap();
        assert_eq!(
            html.matches("style=").count(),
            1,
            "one attribute holds the geometry and the unowned property: {html}"
        );
        assert!(
            html.contains("transform: translate(30px, 40px)"),
            "geometry is inline: {html}"
        );
        assert!(
            html.contains("opacity: 0.5"),
            "nothing authored an opacity, so it became a local declaration: {html}"
        );

        // `border-radius` is declared by `.cta`, so ownership sends that edit to
        // the stylesheet instead — one attribute on the element, one declaration
        // in the file that already owned it.
        let css = std::fs::read_to_string(root.join("styles.css")).unwrap();
        assert!(
            css.contains("border-radius: 12px"),
            "radius is in the rule: {css}"
        );
        assert_eq!(
            css.matches("border-radius").count(),
            1,
            "rewritten in place, not duplicated: {css}"
        );

        // And a repeated edit of the same value changes nothing.
        let reopened = open_project(&root).expect("reopens");
        save_project(
            &root,
            &reopened.document,
            &[SourceEdit::Style {
                node: target.clone(),
                property: "border-radius".into(),
                value: "12px".into(),
            }],
        )
        .expect("save succeeds");
        assert_eq!(
            std::fs::read_to_string(root.join("styles.css")).unwrap(),
            css,
            "a repeated edit is idempotent"
        );
    }

    #[test]
    fn a_hand_written_inline_declaration_survives_an_unrelated_edit() {
        // The author wrote `z-index` by hand. Moving the element must not delete
        // it: the editor merges into the existing value rather than replacing it.
        let root = scratch("landing");
        let html = std::fs::read_to_string(root.join("index.html")).unwrap();
        std::fs::write(
            root.join("index.html"),
            html.replace(
                r#"<h1 data-spool-id="spool-text-headline">"#,
                r#"<h1 data-spool-id="spool-text-headline" style="z-index: 3">"#,
            ),
        )
        .unwrap();
        let loaded = open_project(&root).expect("opens");

        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Geometry {
                node: node("spool-text-headline"),
                placement: Some(Placement::Flow { dx: 5.0, dy: 6.0 }),
                width: None,
                height: None,
            }],
        )
        .expect("save succeeds");

        let after = std::fs::read_to_string(root.join("index.html")).unwrap();
        assert!(after.contains("z-index: 3"), "kept: {after}");
        assert!(
            after.contains("transform: translate(5px, 6px)"),
            "and the move landed too: {after}"
        );
    }

    #[test]
    fn identity_and_hierarchy_survive_the_whole_loop() {
        let root = scratch("landing");
        let loaded = open_project(&root).expect("opens");
        let before_ids: Vec<String> = loaded
            .document
            .structure
            .nodes
            .iter()
            .map(|n| n.id.as_str().to_owned())
            .collect();
        let before_children = loaded.document.structure.nodes[0].children.clone();

        save_project(
            &root,
            &loaded.document,
            &[SourceEdit::Text {
                node: node("spool-cta-primary"),
                text: "Shipped".into(),
            }],
        )
        .expect("save");

        let reopened = open_project(&root).expect("reopens");
        let after_ids: Vec<String> = reopened
            .document
            .structure
            .nodes
            .iter()
            .map(|n| n.id.as_str().to_owned())
            .collect();
        assert_eq!(after_ids, before_ids, "identity survived");
        assert_eq!(
            reopened.document.structure.nodes[0].children, before_children,
            "hierarchy survived"
        );
    }
}

/// Where a new element goes, and what it displaces to get there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct InsertionPoint {
    /// Byte offset the markup is written at.
    at: usize,
    /// Whitespace immediately before `at` that the insertion replaces, so the
    /// parent keeps one newline rather than accumulating a blank line per child.
    replaces: std::ops::Range<usize>,
    /// The line indentation the new element is written with.
    indent: String,
}

/// The leading whitespace of the line `offset` sits on.
///
/// Only spaces and tabs: the newline is left for the caller to add, because
/// whether the new element needs one depends on whether the file uses them.
fn line_indent(source: &str, offset: usize) -> String {
    let line_start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    source[line_start..offset]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// The markup for a created object.
///
/// One element, carrying its identity and the appearance the editor cannot
/// express any other way for an element that was never authored with a class to
/// own. Nothing is written that the object does not actually have: the
/// declarations are the ones the editor will read back, and text is escaped so
/// what the user typed cannot become markup.
fn render_new_element(node: &NodeId, element: &NewElement) -> String {
    let style = merge_inline_declarations("", &element.declarations);
    let mut markup = format!(
        "<{tag} data-spool-id=\"{id}\"",
        tag = element.tag,
        id = node.as_str()
    );
    if !style.is_empty() {
        markup.push_str(&format!(" style=\"{style}\""));
    }
    markup.push('>');
    if let Some(text) = &element.text {
        markup.push_str(&escape_text(text));
    }
    markup.push_str(&format!("</{}>", element.tag));
    markup
}
