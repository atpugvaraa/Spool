//! Compiling authored source into the engine's document model.
//!
//! This is the `source -> Document` direction of the pipeline `spool-source`
//! exists to own:
//!
//! ```text
//! HTML / CSS ─┐
//!             ├─> figcraft_doc::Document ─> engine ─> renderer
//! lamine.yaml ┘
//! ```
//!
//! # The one rule this module has to keep
//!
//! Every persisted property has exactly one owner
//! (`docs/02-document-and-source-model.md`):
//!
//! | Owner | Where it lives | What this module does with it |
//! |---|---|---|
//! | **A** | authored HTML/CSS | resolved through [`crate::css`] and written onto the node |
//! | **B** | `lamine.yaml` | copied from [`NodeRecord`] onto the node |
//! | **C** | derived | never written |
//! | **D** | session state | never touched |
//!
//! Nothing is invented. A value the source does not determine leaves the
//! engine's own default in place *and* produces an [`Unresolved`] entry, because
//! a default the author never wrote is a lie the caller has to be able to see.
//! Guessing a width, a fill or a font size would make the compiled document
//! disagree with the project it was compiled from, which is the one thing the
//! disposability guarantee forbids.
//!
//! # Auto-layout is authored as flex
//!
//! `display: flex` compiles to [`AutoLayout`]: `flex-direction` is the mode,
//! `justify-content`/`align-items` are the alignments, `flex-wrap` is `wrap`,
//! `padding` is the layout padding, and the two gap axes are the gaps. `hug`,
//! `fill`, grid auto-layout and everything else Figma-specific stays Owner B in
//! `lamine.yaml`. This is the decision recorded in `docs/02`.
//!
//! # Scope, stated so it can be widened deliberately
//!
//! - Markup is scanned for the facts the cascade matches on — tag, classes,
//!   `id`, `data-spool-id`, an inline `style`, and the element's own text. No
//!   HTML entity is decoded, so `&amp;` stays as authored.
//! - Only the `[data-spool-id="key"]` binding form is supported. Any other
//!   selector is *reported*, never approximated.
//! - Inherited properties (`color`, `font-size`) cascade down the **managed**
//!   ancestor chain. An unmanaged wrapper between two managed nodes is not
//!   walked, so `body { color: … }` in a project whose `<body>` is not a managed
//!   node does not reach the nodes inside it.
//! - Margin, `box-sizing`, `right`/`bottom`, `transform` and `font-weight` have
//!   no place on a compiled node. A node that authors one is told so.
//!
//! [`AutoLayout`]: figcraft_doc::AutoLayout

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use figcraft_color::{BlendMode, Color, Paint};
use figcraft_doc::geom::Affine;
use figcraft_doc::proto::ScrollPosition;
use figcraft_doc::{
    AutoLayout, ChildLayout, Corners, CrossAlign, Document, LayoutMode, MainAlign, Node, NodeId,
    NodeKind, StrokeProps, TextAlignH, TextData,
};

use crate::css::{RankedValue, Stylesheet};
use crate::manifest::{Manifest, ManifestError, NodeRecord};

/// The identity of the page every compiled node hangs from.
///
/// `0` rather than an allocated id: the engine's allocator counts from `1`, and
/// a `lamine.yaml` node may use any id including `1`. A page that is not part of
/// the manifest therefore cannot collide with a managed node.
pub const PAGE_ID: NodeId = NodeId(0);

/// The name of the single compiled page.
///
/// The manifest carries no document name and one is not an authored property,
/// so this is a fixed label rather than a value read from anywhere.
pub const PAGE_NAME: &str = "Page 1";

/// The properties this reader cascades down the ancestor chain.
///
/// Exactly the two that are both inherited in CSS and supported by
/// [`crate::css`]. Anything else added here has to be added deliberately.
pub const INHERITED_PROPERTIES: &[&str] = &["color", "font-size"];

/// The supported properties this module writes somewhere.
///
/// A stylesheet may resolve more than this for an element; those are reported
/// rather than silently dropped, so the line between "compiled" and "declared"
/// stays something a caller can point at.
pub const OWNER_A_APPLIED: &[&str] = &[
    "align-items",
    "background-color",
    "border",
    "border-radius",
    "color",
    "column-gap",
    "display",
    "flex-direction",
    "flex-wrap",
    "font-size",
    "gap",
    "height",
    "justify-content",
    "left",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "opacity",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "position",
    "row-gap",
    "text-align",
    "top",
    "width",
];

/// The declarations that only mean something inside an auto layout.
///
/// They are Owner A and this module compiles all of them, but only onto a node
/// that is a flex container. On a node that is not one they are reported rather
/// than dropped.
const LAYOUT_PROPERTIES: &[&str] = &[
    "align-items",
    "column-gap",
    "flex-direction",
    "flex-wrap",
    "gap",
    "justify-content",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "row-gap",
];

/// Elements that never have a closing tag.
///
/// Treating one of these as an open element would make everything after it read
/// as its children, silently moving text out of the node that authored it.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements whose content is text, not markup.
const RAW_TEXT_ELEMENTS: &[&str] = &["script", "style"];

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything compiling source into the engine model can fail to be.
///
/// Every variant is a refusal, never a repair. A binding that names a file that
/// is not there, a selector that matches nothing or two elements, markup that
/// cannot be interpreted, a cascade that cannot be ordered, and a hierarchy that
/// contradicts itself are all reported — none of them is guessed at.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReadError {
    #[error("lamine.yaml is not compilable: {0}")]
    Manifest(#[from] ManifestError),

    #[error("node {id} binds to {file}, which is not in the project")]
    MissingSource { id: u64, file: String },

    #[error(
        "node {id} binds with selector {selector:?}, which is outside the supported form `[data-spool-id=\"…\"]`"
    )]
    UnsupportedSelector { id: u64, file: String, selector: String },

    #[error("node {id} binds with selector {selector:?}, which matches nothing in {file}")]
    UnresolvedBinding {
        id: u64,
        file: String,
        selector: String,
    },

    #[error("node {id} binds with selector {selector:?}, which matches {count} elements in {file}")]
    AmbiguousBinding {
        id: u64,
        file: String,
        selector: String,
        count: usize,
    },

    #[error("{file} is malformed: {detail}")]
    MalformedSource { file: String, detail: String },

    #[error("node {id} declares the unknown kind {kind:?}")]
    UnknownKind { id: u64, kind: String },

    #[error("node {id} is not listed among the children of its parent {parent}")]
    UnlistedChild { id: u64, parent: u64 },

    #[error("node {id} is listed among the children of {listed} but names {parent:?} as its parent")]
    MismatchedParent {
        id: u64,
        listed: u64,
        parent: Option<u64>,
    },

    #[error("node {id} has a parent chain that loops back on itself")]
    HierarchyCycle { id: u64 },

    #[error("node {id} has {property} declared `!important`, whose precedence this cascade does not implement")]
    AmbiguousCascade { id: u64, property: String },

    #[error("the engine refused node {id}: {detail}")]
    Engine { id: u64, detail: String },
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// One fact the source does not determine, so the engine's default stands in.
///
/// This is the honest answer to "what does the compiled document hold that the
/// project never said?". Each entry names the node, the authored identity it
/// belongs to, the property, and what could not be read. Nothing here is ever
/// silently dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unresolved {
    /// The node the value would have gone on. [`PAGE_ID`] for document-level facts.
    pub node: NodeId,
    /// The node's `data-spool-id`, or a file path for document-level facts.
    pub source_key: String,
    /// The property, spelled as authored.
    pub property: String,
    /// Why it is unresolved.
    pub detail: String,
}

impl Unresolved {
    fn new(node: NodeId, source_key: &str, property: &str, detail: impl Into<String>) -> Self {
        Self {
            node,
            source_key: source_key.to_owned(),
            property: property.to_owned(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Unresolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "node {} ({}) has no readable {}: {}",
            self.node, self.source_key, self.property, self.detail
        )
    }
}

/// A compiled document and the state the source did not determine.
#[derive(Clone, Debug, PartialEq)]
pub struct Read {
    /// The compiled document. Disposable: rebuild it from the project any time.
    pub document: Document,
    /// Everything the compiled document holds that the project never authored.
    pub unresolved: Vec<Unresolved>,
}

impl Read {
    /// The compiled document, dropping the diagnostics.
    pub fn into_document(self) -> Document {
        self.document
    }

    /// Whether every property on every node came from source.
    pub fn is_complete(&self) -> bool {
        self.unresolved.is_empty()
    }

    /// The diagnostics for one node, in the order they were found.
    pub fn unresolved_for(&self, node: NodeId) -> impl Iterator<Item = &Unresolved> + '_ {
        self.unresolved.iter().filter(move |u| u.node == node)
    }

    /// The diagnostics naming one property, across every node.
    pub fn unresolved_property<'a>(
        &'a self,
        property: &str,
    ) -> impl Iterator<Item = &'a Unresolved> + 'a {
        // The needle is owned so the returned iterator borrows only `self`.
        let property = property.to_owned();
        self.unresolved.iter().filter(move |u| u.property == property)
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Compile a project into the engine's document model.
///
/// See [`read_with_diagnostics`] for the same compile plus the list of values
/// the source did not determine; this is that call with the list dropped.
pub fn read(
    manifest: &Manifest,
    sources: &BTreeMap<String, String>,
) -> Result<Document, ReadError> {
    read_with_diagnostics(manifest, sources).map(Read::into_document)
}

/// Compile a project into the engine's document model, reporting what is missing.
///
/// `sources` is keyed by project-relative path, exactly like a
/// [`SourceBinding::file`](crate::manifest::SourceBinding::file). The result is
/// the compiled document plus one
/// [`Unresolved`] per property left at the engine's default because the source
/// did not determine it.
///
/// Fails rather than approximating when a binding cannot be resolved, when the
/// markup cannot be interpreted, when the cascade cannot be ordered, or when the
/// manifest names a kind the engine does not have.
pub fn read_with_diagnostics(
    manifest: &Manifest,
    sources: &BTreeMap<String, String>,
) -> Result<Read, ReadError> {
    Builder::new(manifest, sources)?.compile()
}

// ---------------------------------------------------------------------------
// Markup
// ---------------------------------------------------------------------------

/// One element's facts: everything [`Stylesheet::resolve`] matches against, plus
/// the text a `Text` node carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Element {
    tag: String,
    classes: Vec<String>,
    id: Option<String>,
    /// The `data-spool-id` value, if the element carries one.
    spool_id: Option<String>,
    /// Declarations authored on the element's own `style` attribute.
    inline: BTreeMap<String, String>,
    /// The element's own text: its direct text, not a descendant's.
    text: String,
}

/// One stylesheet the document names, in the order it names it.
#[derive(Debug)]
struct StylesheetRef<'a> {
    /// `None` for an inline `<style>` block, which needs no path.
    key: Option<String>,
    /// The CSS text, borrowed from the file that holds it.
    css: &'a str,
}

/// Everything one file's markup says: its elements and its stylesheet order.
#[derive(Debug)]
struct Scanned<'a> {
    elements: Vec<Element>,
    stylesheets: Vec<StylesheetRef<'a>>,
}

impl<'a> Scanned<'a> {
    fn into_elements(self) -> Vec<Element> {
        self.elements
    }
}

/// Scan `source` for elements and for the stylesheets it names.
///
/// Never fabricates structure. A `<` with no tag name and an attribute quote
/// that never closes both leave the file uninterpretably and are reported;
/// everything else — a stray close tag, an element left open at end of file —
/// recovers the way a browser does.
fn scan_markup<'a>(file: &str, source: &'a str) -> Result<Scanned<'a>, ReadError> {
    // Each element's content start, in the order the elements were opened.
    let mut content_start: Vec<usize> = Vec::new();
    // Where each opened element's content ends, so a parent can step over a
    // child's markup instead of flattening its text into its own.
    let mut ends_by_open: BTreeMap<usize, usize> = BTreeMap::new();
    let mut elements: Vec<Element> = Vec::new();
    let mut sheets: Vec<StylesheetRef<'a>> = Vec::new();
    // Indices of currently-open elements, outermost first.
    let mut stack: Vec<usize> = Vec::new();
    let mut index = 0usize;

    while index < source.len() {
        let Some(offset) = source[index..].find('<') else {
            break;
        };
        index += offset;
        let rest = &source[index..];

        // Comments, doctypes and processing instructions carry no elements.
        if rest.starts_with("<!--") {
            index = match rest.find("-->") {
                Some(end) => index + end + 3,
                None => source.len(),
            };
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            let Some(end) = find_outside(source, index + 2, b'>') else {
                return Err(ReadError::MalformedSource {
                    file: file.to_owned(),
                    detail: format!("the declaration at byte {index} never ends"),
                });
            };
            index = end + 1;
            continue;
        }

        let closing = rest.starts_with("</");
        let name_start = index + if closing { 2 } else { 1 };
        let Some(tag_end) =
            source[name_start..].find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        else {
            return Err(ReadError::MalformedSource {
                file: file.to_owned(),
                detail: format!("the `<` at byte {index} has no tag name"),
            });
        };
        let tag = source[name_start..name_start + tag_end].to_ascii_lowercase();

        let Some(close) = find_tag_end(file, source, name_start + tag_end)? else {
            return Err(ReadError::MalformedSource {
                file: file.to_owned(),
                detail: format!("the `<{tag}` at byte {index} is never closed"),
            });
        };
        let body = &source[name_start + tag_end..close];
        let self_closing = body.trim_end().ends_with('/');

        if closing {
            // Close this element and, when the markup does not line up, any
            // ancestors left open above it. Popping to the matching tag is what
            // makes `<p><div></p>` close the paragraph rather than nothing.
            let open = stack
                .iter()
                .rposition(|open| elements.get(*open).is_some_and(|e| e.tag == tag));
            if let Some(position) = open {
                // Innermost first: a parent's own text steps over its children, so
                // every child's content range has to be known before the parent's
                // is read.
                let closed: Vec<usize> = stack.drain(position..).collect();
                for at in closed.into_iter().rev() {
                    close_element(
                        source,
                        &mut elements,
                        &content_start,
                        &mut ends_by_open,
                        at,
                        index,
                    );
                }
            }
            index = close + 1;
            continue;
        }

        let attributes = parse_attributes(file, body, index)?;
        let inline = attribute(&attributes, "style")
            .and_then(|values| values.first().copied())
            .unwrap_or("");
        let at = elements.len();
        elements.push(Element {
            tag: tag.clone(),
            id: attribute(&attributes, "id")
                .and_then(|values| values.first().map(|value| (*value).to_owned())),
            classes: attribute(&attributes, "class")
                .map(|values| {
                    values
                        .iter()
                        .flat_map(|value| value.split_whitespace())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            spool_id: attribute(&attributes, "data-spool-id")
                .and_then(|values| values.first().map(|value| (*value).to_owned())),
            inline: inline_declarations(inline),
            text: String::new(),
        });
        let open_after = close + 1;
        content_start.push(open_after);
        if !self_closing && !VOID_ELEMENTS.contains(&tag.as_str()) {
            stack.push(at);
        } else {
            ends_by_open.insert(open_after, open_after);
        }

        // A `link rel="stylesheet"` names a file; a `style` carries one. Both are
        // recorded here so the cascade reads them in document order.
        if tag == "link" {
            let is_stylesheet = attribute(&attributes, "rel").is_some_and(|values| {
                values
                    .iter()
                    .flat_map(|value| value.split_whitespace())
                    .any(|token| token.eq_ignore_ascii_case("stylesheet"))
            });
            if is_stylesheet {
                for href in attribute(&attributes, "href").unwrap_or_default() {
                    let href = href.trim();
                    if href.is_empty() || href.starts_with('#') || href.contains("://") {
                        continue;
                    }
                    if let Some(key) = source_key_for(file, href) {
                        sheets.push(StylesheetRef {
                            key: Some(key),
                            css: "",
                        });
                    }
                }
            }
        }

        if RAW_TEXT_ELEMENTS.contains(&tag.as_str()) {
            // The content of a `script` or `style` is text and ends at its own
            // closing tag, or at the end of the file when it has none.
            let content_end = match raw_text_end(source, open_after, &tag) {
                Some(end) => end,
                None => {
                    if tag == "style" {
                        sheets.push(StylesheetRef {
                            key: None,
                            css: source.get(open_after..).unwrap_or(""),
                        });
                    }
                    if let Some(open) = stack.pop() {
                        close_element(
                            source,
                            &mut elements,
                            &content_start,
                            &mut ends_by_open,
                            open,
                            source.len(),
                        );
                    }
                    break;
                }
            };
            if tag == "style" {
                sheets.push(StylesheetRef {
                    key: None,
                    css: source.get(open_after..content_end).unwrap_or(""),
                });
            }
            index = match find_outside(source, content_end, b'>') {
                Some(end) => end + 1,
                None => source.len(),
            };
            ends_by_open.insert(open_after, content_end);
            stack.pop();
            continue;
        }

        index = open_after;
    }

    // An element the file never closed runs to the end of the file.
    while let Some(open) = stack.pop() {
        close_element(
            source,
            &mut elements,
            &content_start,
            &mut ends_by_open,
            open,
            source.len(),
        );
    }

    Ok(Scanned {
        elements,
        stylesheets: sheets,
    })
}

/// Settle one element's own text now that its content range is known.
fn close_element(
    source: &str,
    elements: &mut [Element],
    content_start: &[usize],
    ends_by_open: &mut BTreeMap<usize, usize>,
    at: usize,
    content_end: usize,
) {
    let Some(from) = content_start.get(at).copied() else {
        return;
    };
    if let Some(element) = elements.get_mut(at) {
        element.text = own_text(source, from, content_end, ends_by_open);
    }
    ends_by_open.insert(from, content_end);
}

/// Where a raw-text element's content ends: at its closing tag, or `None` when
/// the file has none.
fn raw_text_end(source: &str, from: usize, tag: &str) -> Option<usize> {
    let closing = format!("</{tag}");
    let mut index = from;
    while index < source.len() {
        index += source[index..].find('<')?;
        if source[index..].starts_with(&closing) {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// The text an element owns: its own text nodes, with a nested element's content
/// left to that element.
///
/// A managed child node keeps its own characters, and flattening them into the
/// parent would put the same words on two nodes.
fn own_text(
    source: &str,
    from: usize,
    content_end: usize,
    ends_by_open: &BTreeMap<usize, usize>,
) -> String {
    let mut out = String::new();
    let mut index = from;
    while index < content_end {
        let Some(offset) = source[index..content_end].find('<') else {
            out.push_str(&source[index..content_end]);
            break;
        };
        let at = index + offset;
        out.push_str(&source[index..at]);
        let tail = &source[at..content_end];
        if tail.starts_with("<!--") {
            index = match tail.find("-->") {
                Some(end) => at + end + 3,
                None => content_end,
            };
            continue;
        }
        if tail.starts_with("</") {
            // Markup that does not line up: a stray close tag inside this
            // element's content. Step over it and keep reading, or every word
            // after it is lost.
            index = match find_outside(source, at + 2, b'>') {
                Some(end) => end + 1,
                None => content_end,
            };
            continue;
        }
        // Step over a nested element whole; its content belongs to it.
        let Some((open_after, self_closing)) = tag_extent(source, at) else {
            break;
        };
        if self_closing {
            index = open_after;
        } else {
            index = match ends_by_open.get(&open_after) {
                Some(end) => (*end).max(open_after),
                None => open_after,
            };
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Where an opening tag at `at` ends, and whether it closes itself.
fn tag_extent(source: &str, at: usize) -> Option<(usize, bool)> {
    let rest = source.get(at..)?;
    if !rest.starts_with('<') || rest.starts_with("</") {
        return None;
    }
    let name_start = at + 1;
    let name_end =
        source[name_start..].find(|c: char| c.is_whitespace() || c == '>' || c == '/')?;
    let tag = source[name_start..name_start + name_end].to_ascii_lowercase();
    let close = find_outside(source, name_start + name_end, b'>')?;
    Some((
        close + 1,
        source[name_start + name_end..close].trim_end().ends_with('/')
            || VOID_ELEMENTS.contains(&tag.as_str()),
    ))
}

/// Parse the attributes of one open tag body (everything between the tag name
/// and the closing `>`).
fn parse_attributes(
    file: &str,
    inner: &str,
    tag_at: usize,
) -> Result<Vec<(String, String)>, ReadError> {
    let mut attributes = Vec::new();
    let bytes = inner.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b'/') {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        let name_start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && bytes[index] != b'='
            && bytes[index] != b'/'
        {
            index += 1;
        }
        let name = inner[name_start..index].to_ascii_lowercase();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            if !name.is_empty() {
                attributes.push((name, String::new()));
            }
            continue;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            attributes.push((name, String::new()));
            break;
        }
        let quote = bytes[index];
        let value = if quote == b'"' || quote == b'\'' {
            let value_start = index + 1;
            let Some(end) = inner[value_start..].find(quote as char) else {
                return Err(ReadError::MalformedSource {
                    file: file.to_owned(),
                    detail: format!(
                        "the `{name}` attribute of the tag at byte {tag_at} has an unterminated value"
                    ),
                });
            };
            index = value_start + end + 1;
            inner[value_start..value_start + end].to_owned()
        } else {
            let value_start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            inner[value_start..index].to_owned()
        };
        attributes.push((name, value));
    }
    Ok(attributes)
}

/// Every value authored for one attribute of an open tag.
fn attribute<'a>(attributes: &'a [(String, String)], name: &str) -> Option<Vec<&'a str>> {
    let found: Vec<&str> = attributes
        .iter()
        .filter(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .collect();
    (!found.is_empty()).then_some(found)
}

/// Declarations authored on an element's own `style` attribute.
///
/// Later declarations override earlier ones, including across a shorthand, and
/// `background` is expanded to `background-color` so the two spellings compete
/// the way they do inside a stylesheet rule.
fn inline_declarations(style: &str) -> BTreeMap<String, String> {
    let mut inline = BTreeMap::new();
    for part in style.split(';') {
        let Some((property, raw)) = part.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        let raw = raw.trim();
        if property.is_empty() || raw.is_empty() {
            continue;
        }
        let property = match property.as_str() {
            "background" => "background-color".to_owned(),
            other => other.to_owned(),
        };
        inline.insert(property, raw.to_owned());
    }
    inline
}

/// The `data-spool-id` a binding selector names, or `None` if it is not in the
/// supported `[data-spool-id="…"]` form.
///
/// Anything else is refused rather than approximated. Applying `#hero` because it
/// looks like a selector would bind a node to whichever element happened to
/// carry that id, which is not what the manifest said.
fn parse_identity_selector(selector: &str) -> Option<&str> {
    let body = selector
        .trim()
        .strip_prefix("[data-spool-id=")?
        .strip_suffix(']')?
        .trim();
    let bytes = body.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(quote @ (b'"' | b'\'')), Some(last)) if quote == last && bytes.len() >= 2 => {
            Some(&body[1..body.len() - 1])
        }
        _ => None,
    }
}

/// The project-relative key an `href` authored in `page` resolves to.
fn source_key_for(page: &str, href: &str) -> Option<String> {
    let directory = page.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    let mut segments: Vec<&str> = if directory.is_empty() {
        Vec::new()
    } else {
        directory.split('/').collect()
    };
    for segment in href.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            other => segments.push(other),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

/// The `>` that closes a tag, ignoring any inside a quoted attribute value.
fn find_tag_end(file: &str, source: &str, from: usize) -> Result<Option<usize>, ReadError> {
    let bytes = source.as_bytes();
    let mut index = from;
    while index < bytes.len() {
        match bytes[index] {
            b'>' => return Ok(Some(index)),
            quote @ (b'"' | b'\'') => {
                let value_start = index + 1;
                let Some(end) = source[value_start..].find(quote as char) else {
                    return Err(ReadError::MalformedSource {
                        file: file.to_owned(),
                        detail: format!("an attribute value at byte {index} is unterminated"),
                    });
                };
                index = value_start + end + 1;
            }
            _ => index += 1,
        }
    }
    Ok(None)
}

/// The next `target` byte at or after `from`, skipping quoted text.
fn find_outside(source: &str, from: usize, target: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut index = from;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'\'' => {
                let quote = bytes[index];
                let value_start = index + 1;
                index = value_start + source[value_start..].find(quote as char)? + 1;
            }
            byte if byte == target => return Some(index),
            _ => index += 1,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Compilation
// ---------------------------------------------------------------------------

/// One parsed file: the elements the cascade can match on, and its stylesheets.
#[derive(Debug, Default)]
struct ParsedFile {
    elements: Vec<Element>,
    stylesheets: Vec<Stylesheet>,
}

/// The state one compile carries between nodes.
struct Builder<'a> {
    manifest: &'a Manifest,
    sources: &'a BTreeMap<String, String>,
    /// Parsed files, keyed by project-relative path.
    files: BTreeMap<String, ParsedFile>,
    /// Per-node declarations, after the cascade and after inheritance.
    declarations: BTreeMap<u64, BTreeMap<String, String>>,
    /// Per-node element facts, for the elements whose binding resolved.
    elements: BTreeMap<u64, Element>,
    unresolved: Vec<Unresolved>,
}

impl<'a> Builder<'a> {
    fn new(
        manifest: &'a Manifest,
        sources: &'a BTreeMap<String, String>,
    ) -> Result<Self, ReadError> {
        // A manifest that does not validate has no honest compilation, and every
        // structural check downstream assumes that it holds.
        manifest.validate()?;
        let mut builder = Builder {
            manifest,
            sources,
            files: BTreeMap::new(),
            declarations: BTreeMap::new(),
            elements: BTreeMap::new(),
            unresolved: Vec::new(),
        };
        builder.resolve_elements()?;
        builder.inherit()?;
        Ok(builder)
    }

    fn compile(mut self) -> Result<Read, ReadError> {
        let mut document = Document::without_pages("Spool");
        document
            .insert_page(page_node())
            .map_err(|error| ReadError::Engine {
                id: PAGE_ID.0,
                detail: error.to_string(),
            })?;
        document.current_page = Some(PAGE_ID);

        // Roots are the nodes no parent names; they hang directly off the page.
        let roots = self.ordered_children(None);
        self.insert_node(&mut document, PAGE_ID, &roots, 0)?;
        self.apply_document_level(&mut document);
        Ok(Read {
            document,
            unresolved: self.unresolved,
        })
    }

    /// Parse every bound file once and resolve every node's declarations.
    ///
    /// Order matters: inheritance runs over the whole tree before any node is
    /// built, because a managed parent's `color` is what a child inherits.
    fn resolve_elements(&mut self) -> Result<(), ReadError> {
        let manifest = self.manifest;
        for record in &manifest.nodes {
            let file = record.binding.file.clone();
            if !self.files.contains_key(&file) {
                let contents =
                    self.sources
                        .get(&file)
                        .ok_or_else(|| ReadError::MissingSource {
                            id: record.id,
                            file: file.clone(),
                        })?;
                let scanned = scan_markup(&file, contents)?;
                let stylesheets = self.sheets_for(&file, &scanned);
                self.files.insert(
                    file.clone(),
                    ParsedFile {
                        elements: scanned.into_elements(),
                        stylesheets,
                    },
                );
            }
            let element = self.element_for(record)?;
            let declarations = self.declarations_for(record, &element)?;
            for property in OWNER_A_APPLIED {
                if let Some(value) = declarations.get(*property) {
                    check_cascade(record, property, value)?;
                }
            }
            self.elements.insert(record.id, element);
            self.declarations.insert(record.id, declarations);
        }
        Ok(())
    }

    /// The stylesheets one file names, in document order.
    ///
    /// A linked file overrides an earlier one and an inline `<style>` sits where
    /// the document puts it, so the list is the document's own order. A link that
    /// names a file the project does not have is reported: the document is
    /// missing something it references, which is not the same as the sheet being
    /// empty.
    fn sheets_for(&mut self, file: &str, scanned: &Scanned<'a>) -> Vec<Stylesheet> {
        let sources = self.sources;
        scanned
            .stylesheets
            .iter()
            .filter_map(|sheet| match &sheet.key {
                None => Some(Stylesheet::parse(sheet.css)),
                Some(key) => match sources.get(key) {
                    Some(css) => Some(Stylesheet::parse(css.as_str())),
                    None => {
                        self.unresolved.push(Unresolved::new(
                            PAGE_ID,
                            key,
                            "stylesheet",
                            format!("{file} links {key}, which is not in the project"),
                        ));
                        None
                    }
                },
            })
            .collect()
    }

    /// The element a node's binding names, or why it cannot be found.
    ///
    /// Exactly one element must match. Zero is a stale binding; two is a document
    /// whose identity is not unique, and picking either would bind the node to a
    /// construct its author never chose.
    fn element_for(&self, record: &NodeRecord) -> Result<Element, ReadError> {
        let file = record.binding.file.clone();
        let selector = record.binding.selector.clone();
        let key = parse_identity_selector(&selector).ok_or_else(|| ReadError::UnsupportedSelector {
            id: record.id,
            file: file.clone(),
            selector: selector.clone(),
        })?;
        let Some(parsed) = self.files.get(&file) else {
            return Err(ReadError::MissingSource { id: record.id, file });
        };
        let mut found = parsed
            .elements
            .iter()
            .filter(|element| element.spool_id.as_deref() == Some(key));
        let Some(first) = found.next() else {
            return Err(ReadError::UnresolvedBinding {
                id: record.id,
                file,
                selector,
            });
        };
        let extra = found.count();
        if extra > 0 {
            return Err(ReadError::AmbiguousBinding {
                id: record.id,
                file,
                selector,
                count: extra + 1,
            });
        }
        Ok(first.clone())
    }

    /// Every declaration that applies to one element, in cascade order.
    ///
    /// Linked stylesheets are each resolved on their own, and the winners are
    /// then ranked against each other: specificity first, then which sheet the
    /// document links them from, then author order within it. Link order is
    /// therefore the *last* tie-break rather than an override, which is the
    /// whole difference between the cascade and "the last sheet that mentions
    /// the property". Ranking on link order alone lets a later `.theme` beat an
    /// earlier `#hero`, which is a value the author never asked for.
    ///
    /// A `!important` declaration this cascade cannot order is refused rather
    /// than resolved: by the time a winner exists, the declaration that lost has
    /// been discarded along with the evidence that the cascade could not rank
    /// them. It is asked of each sheet before that happens.
    ///
    /// The element's own `style` attribute is applied last, because an inline
    /// declaration outranks any stylesheet.
    fn declarations_for(
        &self,
        record: &NodeRecord,
        element: &Element,
    ) -> Result<BTreeMap<String, String>, ReadError> {
        let mut winners = Winners::default();
        if let Some(parsed) = self.files.get(&record.binding.file) {
            for (index, sheet) in parsed.stylesheets.iter().enumerate() {
                // Scoped to the properties this reader actually compiles, the
                // same set `check_cascade` refuses. A flag on a property nobody
                // reads is not this node's ambiguity.
                let important = sheet.important_properties(
                    &element.tag,
                    &element.classes,
                    element.id.as_deref(),
                );
                if let Some(property) = OWNER_A_APPLIED
                    .iter()
                    .copied()
                    .find(|property| important.contains(property))
                {
                    return Err(ReadError::AmbiguousCascade {
                        id: record.id,
                        property: property.to_owned(),
                    });
                }
                for (property, resolved) in
                    sheet.resolve_ranked(&element.tag, &element.classes, element.id.as_deref())
                {
                    winners.consider(property.to_owned(), index, resolved);
                }
            }
        }
        let mut out: BTreeMap<String, String> = BTreeMap::new();
        for (property, winner) in winners.by_property {
            out.insert(property, winner.value);
        }
        for (property, value) in &element.inline {
            out.insert(property.clone(), value.clone());
        }
        Ok(out)
    }

    /// Fill the inherited properties down the managed ancestor chain.
    ///
    /// The nearest managed ancestor that declares a property wins, as inheritance
    /// does in CSS. A node's own declaration always outranks it, so only a node
    /// that declared nothing is filled in here.
    fn inherit(&mut self) -> Result<(), ReadError> {
        let parents: BTreeMap<u64, Option<u64>> = self
            .manifest
            .nodes
            .iter()
            .map(|record| (record.id, record.parent))
            .collect();
        let order: Vec<u64> = self.manifest.nodes.iter().map(|record| record.id).collect();
        // Walking up in manifest order rather than in tree order keeps this
        // correct whatever order `lamine.yaml` happens to list nodes in.
        for id in order {
            let chain = ancestor_chain(id, &parents)?;
            let mut inherited: Vec<(&str, String)> = Vec::new();
            for property in INHERITED_PROPERTIES {
                if self
                    .declarations
                    .get(&id)
                    .is_some_and(|declarations| declarations.contains_key(*property))
                {
                    continue;
                }
                let value = chain.iter().find_map(|ancestor| {
                    self.declarations
                        .get(ancestor)
                        .and_then(|declarations| declarations.get(*property).cloned())
                });
                if let Some(value) = value {
                    inherited.push((property, value));
                }
            }
            if let Some(declarations) = self.declarations.get_mut(&id) {
                for (property, value) in inherited {
                    declarations.insert(property.to_owned(), value);
                }
            }
        }
        Ok(())
    }

    /// The nodes whose parent is `parent`, in paint order.
    ///
    /// `lamine.yaml` states the hierarchy twice — as each child's `parent`, and
    /// as the parent's ordered `children`. Both must agree: a child listed under a
    /// parent that does not name it back means the file contradicts itself, and
    /// guessing which statement is true would silently reshuffle the tree.
    ///
    /// `parent: None` is the page. A page-level node has no `children` list to be
    /// found in, so it is normal rather than a diagnostic; its order is the order
    /// `lamine.yaml` lists it in.
    fn ordered_children(&mut self, parent: Option<u64>) -> Vec<u64> {
        let manifest = self.manifest;
        let listed = match parent {
            Some(id) => manifest
                .node(NodeId(id))
                .map(|record| record.children.clone())
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let mut out = Vec::new();
        for child in &listed {
            let Some(record) = manifest.node(NodeId(*child)) else {
                continue;
            };
            if record.parent != parent {
                self.unresolved.push(Unresolved::new(
                    record.node_id(),
                    &record.key,
                    "hierarchy",
                    format!(
                        "listed among the children of {} but names {} as its parent",
                        parent.map_or_else(|| "the page".to_owned(), |p| p.to_string()),
                        record
                            .parent
                            .map_or_else(|| "nothing".to_owned(), |p| p.to_string())
                    ),
                ));
            }
            out.push(*child);
        }
        // A node whose parent does not list it has no stated paint order. On the
        // page that is not a fault at all — the page holds them in file order — so
        // it is only reported for a real parent.
        let reported = parent.is_some();
        for record in &manifest.nodes {
            if record.parent == parent && !out.contains(&record.id) {
                if reported {
                    self.unresolved.push(Unresolved::new(
                        record.node_id(),
                        &record.key,
                        "hierarchy",
                        "not listed among its parent's children, so its paint order is undetermined",
                    ));
                }
                out.push(record.id);
            }
        }
        out
    }

    /// Insert one subtree under `parent`, depth first, in paint order.
    fn insert_node(
        &mut self,
        document: &mut Document,
        parent: NodeId,
        ids: &[u64],
        first: usize,
    ) -> Result<(), ReadError> {
        let manifest = self.manifest;
        for (offset, id) in ids.iter().enumerate() {
            let Some(record) = manifest.node(NodeId(*id)) else {
                continue;
            };
            let element = self.elements.get(id).cloned().unwrap_or_default();
            let declarations = self.declarations.get(id).cloned().unwrap_or_default();
            let mut notes = Vec::new();
            let mut node = build_node(record, &element, &declarations, &mut notes)?;
            if record.flow.is_some() {
                notes.push(Unresolved::new(
                    record.node_id(),
                    &record.key,
                    "flow",
                    "a prototype flow start has no compiled form yet",
                ));
            }
            if record.bounds_follow_children && !node.kind.bounds_follow_children() {
                notes.push(Unresolved::new(
                    record.node_id(),
                    &record.key,
                    "bounds_follow_children",
                    "only a group or a boolean operation follows its children in the engine's model",
                ));
            }
            apply_owner_b(record, &mut node);
            self.unresolved.append(&mut notes);
            if let Err(error) = document.insert(node, parent, Some(first + offset)) {
                self.unresolved.push(Unresolved::new(
                    record.node_id(),
                    &record.key,
                    "hierarchy",
                    format!("the engine refused the node: {error}"),
                ));
                continue;
            }
            let children = self.ordered_children(Some(*id));
            self.insert_node(document, record.node_id(), &children, 0)?;
        }
        Ok(())
    }

    /// Copy the manifest's document-level Owner B fields onto the document.
    fn apply_document_level(&mut self, document: &mut Document) {
        let manifest = self.manifest;
        if let Some(profile) = manifest.color_profile {
            document.color_profile = profile;
        }
        if !manifest.document_colors.is_empty() {
            document.document_colors = manifest.document_colors.clone();
        }
        if !manifest.comments.is_empty() {
            document.comments = manifest.comments.clone();
        }
        if !manifest.annotation_categories.is_empty() {
            document.annotation_categories = Some(manifest.annotation_categories.clone());
        }
        if !manifest.image_presets.is_empty() {
            document.image_presets = manifest.image_presets.clone();
        }
        if !manifest.styles.is_empty() {
            document.styles = manifest.styles.clone();
        }
        if !manifest.variables.is_empty() || !manifest.collections.is_empty() {
            document.variables = figcraft_doc::Variables {
                collections: manifest.collections.clone(),
                variables: manifest.variables.clone(),
            };
        }
        if !manifest.modes.is_empty() {
            self.unresolved.push(Unresolved::new(
                PAGE_ID,
                PAGE_NAME,
                "modes",
                "standalone modes are not compiled; a mode is read from its collection",
            ));
        }
    }
}

/// The ancestors of `id`, nearest first.
///
/// Bounded and cycle-checked: a malformed manifest must not become an infinite
/// walk, and a loop is reported rather than truncated silently because the
/// hierarchy it describes cannot be compiled at all.
fn ancestor_chain(id: u64, parents: &BTreeMap<u64, Option<u64>>) -> Result<Vec<u64>, ReadError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    seen.insert(id);
    let mut cursor = parents.get(&id).copied().flatten();
    while let Some(current) = cursor {
        if !seen.insert(current) {
            return Err(ReadError::HierarchyCycle { id });
        }
        out.push(current);
        cursor = parents.get(&current).copied().flatten();
    }
    Ok(out)
}

/// The page node every compiled node hangs from.
fn page_node() -> Node {
    Node::new(
        PAGE_ID,
        NodeKind::Page {
            background: Color::DEFAULT_PAGE,
            flows: Vec::new(),
            prototype: Default::default(),
            guides: Vec::new(),
        },
        PAGE_NAME,
        0.0,
        0.0,
    )
}

/// One declaration competing for a property across every sheet the document
/// links, with the ranking that decides it.
///
/// Three criteria, in the order a browser applies them: selector specificity,
/// then which sheet the document links the declaration from, then author order
/// inside that sheet. Link order sits *between* specificity and author order
/// rather than replacing both, so an earlier `#hero` outranks a later `.theme`
/// and a later `.theme` still beats an earlier `.theme`.
struct Winner {
    specificity: u32,
    sheet: usize,
    order: usize,
    value: String,
}

impl Winner {
    /// The comparable rank, for deciding whether this beats the incumbent.
    fn rank(&self) -> (u32, usize, usize) {
        (self.specificity, self.sheet, self.order)
    }
}

/// The sheet-wide winner map, ranked rather than overwritten.
#[derive(Default)]
struct Winners {
    by_property: BTreeMap<String, Winner>,
}

impl Winners {
    /// Keep this declaration if it outranks the incumbent for `property`.
    ///
    /// A tie cannot happen across sheets: `order` is unique within a sheet and
    /// `sheet` separates them, so exactly one declaration wins each property.
    fn consider(&mut self, property: String, sheet: usize, resolved: RankedValue) {
        let candidate = Winner {
            specificity: resolved.specificity,
            sheet,
            order: resolved.order,
            value: resolved.value,
        };
        match self.by_property.get(&property) {
            Some(current) if current.rank() >= candidate.rank() => {}
            _ => {
                self.by_property.insert(property, candidate);
            }
        }
    }
}

/// Refuse a value this cascade cannot order.
///
/// `!important` is the one precedence rule [`crate::css`] documents that it does
/// not implement, so it cannot tell whether such a declaration won. Reading it
/// as though it had would hand the node a value the author may not have meant,
/// and ignoring it would do the same. Either way the answer is a report.
///
/// This sees only the value that already won the cascade, so it is a check on
/// the *inline* `style` attribute — where the authored text is the value. The
/// stylesheet case is refused earlier, by
/// [`Stylesheet::important_properties`], because by the time a stylesheet
/// declaration has won, a losing `!important` has been discarded and this can
/// no longer see it.
fn check_cascade(record: &NodeRecord, property: &str, value: &str) -> Result<(), ReadError> {
    if value.to_ascii_lowercase().contains("!important") {
        return Err(ReadError::AmbiguousCascade {
            id: record.id,
            property: property.to_owned(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Owner A: authored values
// ---------------------------------------------------------------------------

/// Build one node from its kind and its authored declarations.
fn build_node(
    record: &NodeRecord,
    element: &Element,
    declarations: &BTreeMap<String, String>,
    unresolved: &mut Vec<Unresolved>,
) -> Result<Node, ReadError> {
    let width = read_length(unresolved, record, declarations, "width");
    let height = read_length(unresolved, record, declarations, "height");
    let kind = node_kind(record, element, declarations, unresolved)?;
    let mut node = Node::new(record.node_id(), kind, record.name.clone(), width, height);

    apply_position(record, &mut node, declarations, unresolved);
    apply_opacity(record, &mut node, declarations, unresolved);
    apply_paint(record, &mut node, declarations, unresolved);
    apply_corners(record, &mut node, declarations, unresolved);
    apply_child_layout(record, &mut node, declarations, unresolved);
    apply_layout(record, &mut node, declarations, unresolved);

    if let Some(text) = node.kind.text() {
        // The characters and their sizing came from the element and the cascade.
        if text.characters.is_empty() {
            unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "characters",
                "the bound element has no own text",
            ));
        }
        for property in INHERITED_PROPERTIES {
            if !declarations.contains_key(*property) {
                unresolved.push(Unresolved::new(
                    record.node_id(),
                    &record.key,
                    property,
                    "not authored and not inherited; the engine's own value stands",
                ));
            }
        }
    }

    // `display: none` is the one `display` value with an engine counterpart: an
    // element that generates no box is not visible.
    if declarations
        .get("display")
        .is_some_and(|value| value.trim() == "none")
    {
        node.visible = false;
    }

    for (property, value) in declarations {
        if !OWNER_A_APPLIED.contains(&property.as_str()) {
            unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                property,
                format!("{value:?} is authored but has no place on a compiled node"),
            ));
        }
    }

    Ok(node)
}

/// A declared length, or a diagnostic saying it is not one.
fn read_length(
    unresolved: &mut Vec<Unresolved>,
    record: &NodeRecord,
    declarations: &BTreeMap<String, String>,
    property: &str,
) -> f64 {
    match declarations.get(property) {
        Some(value) => match read_length_value(value) {
            Some(number) => number.max(0.0),
            None => {
                unresolved.push(Unresolved::new(
                    record.node_id(),
                    &record.key,
                    property,
                    format!("{value:?} is not a length in px"),
                ));
                0.0
            }
        },
        None => {
            unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                property,
                "not authored; the engine's own 0 stands",
            ));
            0.0
        }
    }
}

/// The engine kind a manifest record asks for.
///
/// `kind` is Owner B, so an unrecognised spelling is a manifest that cannot be
/// compiled rather than a hint to guess from. `component` and `bool_op` are
/// Owner B too, and decide the kind when present: an instance is only an instance
/// by the component it mirrors.
fn node_kind(
    record: &NodeRecord,
    element: &Element,
    declarations: &BTreeMap<String, String>,
    unresolved: &mut Vec<Unresolved>,
) -> Result<NodeKind, ReadError> {
    if let Some(component) = record.component {
        return Ok(NodeKind::Instance {
            component: NodeId(component),
            props: BTreeMap::new(),
        });
    }
    if let Some(op) = record.bool_op {
        return Ok(NodeKind::BooleanOp { op });
    }
    let kind = record.kind.trim().to_ascii_lowercase();
    Ok(match kind.as_str() {
        "frame" => NodeKind::Frame,
        "group" => NodeKind::Group,
        "section" => NodeKind::Section {
            contents_hidden: record.contents_hidden,
        },
        "rectangle" | "rect" => NodeKind::Rectangle,
        "ellipse" | "circle" => NodeKind::Ellipse {
            start: 0.0,
            sweep: std::f64::consts::TAU,
            inner: 0.0,
        },
        "polygon" => NodeKind::Polygon { count: 3 },
        "star" => NodeKind::Star {
            count: 5,
            inner_ratio: 0.5,
        },
        "line" => NodeKind::Line,
        "text" => NodeKind::Text(text_data(record, element, declarations, unresolved)),
        "component" => NodeKind::Component {
            description: String::new(),
            props: BTreeMap::new(),
        },
        "componentset" => NodeKind::ComponentSet {
            description: String::new(),
        },
        "instance" => {
            // An instance without the component it mirrors is not describable.
            // Reported rather than pointed at a component nobody named.
            unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "kind",
                "an instance with no component compiles as a frame",
            ));
            NodeKind::Frame
        }
        other => {
            return Err(ReadError::UnknownKind {
                id: record.id,
                kind: other.to_owned(),
            });
        }
    })
}

/// A text node's content, sized and coloured by the cascade.
fn text_data(
    record: &NodeRecord,
    element: &Element,
    declarations: &BTreeMap<String, String>,
    unresolved: &mut Vec<Unresolved>,
) -> TextData {
    let mut data = TextData::new(element.text.clone());
    let mut style = data.style.clone();
    if let Some(value) = declarations.get("font-size") {
        match read_length_value(value) {
            Some(size) => style.font_size = size.max(0.0),
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "font-size",
                format!("{value:?} is not a length in px"),
            )),
        }
    }
    if let Some(value) = declarations.get("color") {
        match read_color(value) {
            Some(color) => style.fills = Some(vec![Paint::solid(color)]),
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "color",
                format!("{value:?} is not a solid colour"),
            )),
        }
    }
    if let Some(value) = declarations.get("text-align") {
        match read_align_h(value) {
            Some(align) => data.align_h = align,
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "text-align",
                format!("{value:?} has no engine counterpart"),
            )),
        }
    }
    data.style = style;
    data
}

/// Place a node from `position`, `left` and `top`.
fn apply_position(
    record: &NodeRecord,
    node: &mut Node,
    declarations: &BTreeMap<String, String>,
    unresolved: &mut Vec<Unresolved>,
) {
    let position = declared(declarations, "position");
    if !declarations.contains_key("left") && !declarations.contains_key("top") {
        return;
    }
    if matches!(position.as_deref(), Some("absolute") | Some("fixed")) {
        let x = declarations
            .get("left")
            .and_then(|value| read_length_value(value))
            .unwrap_or(0.0);
        let y = declarations
            .get("top")
            .and_then(|value| read_length_value(value))
            .unwrap_or(0.0);
        node.transform = Affine::translate((x, y));
        return;
    }
    let detail = match position.as_deref() {
        Some("relative") => "a relative offset is resolved by layout, not stored on the node",
        Some("sticky") => "a sticky offset is resolved by layout, not stored on the node",
        _ => "CSS ignores left/top on an element that is not positioned",
    };
    unresolved.push(Unresolved::new(
        record.node_id(),
        &record.key,
        "position",
        detail.to_owned(),
    ));
}

/// `opacity`.
fn apply_opacity(
    record: &NodeRecord,
    node: &mut Node,
    declarations: &BTreeMap<String, String>,
    unresolved: &mut Vec<Unresolved>,
) {
    match declarations.get("opacity") {
        Some(value) => match read_number(value) {
            Some(number) => node.opacity = number.clamp(0.0, 1.0) as f32,
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "opacity",
                format!("{value:?} is not a number"),
            )),
        },
        None => unresolved.push(Unresolved::new(
            record.node_id(),
            &record.key,
            "opacity",
            "not authored; the engine's own 1.0 stands",
        )),
    }
}

/// Fills and strokes, both of which are authored in CSS.
fn apply_paint(
    record: &NodeRecord,
    node: &mut Node,
    declarations: &BTreeMap<String, String>,
    unresolved: &mut Vec<Unresolved>,
) {
    match declarations.get("background-color") {
        Some(value) => match read_color(value) {
            Some(color) => node.fills = vec![Paint::solid(color)],
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "background-color",
                format!("{value:?} is not a solid colour"),
            )),
        },
        None => unresolved.push(Unresolved::new(
            record.node_id(),
            &record.key,
            "background-color",
            "not authored; the engine's own default fill stands",
        )),
    }

    // Stroke geometry has two owners. Weight and dashes are authored in CSS;
    // alignment, caps, joins, the width profile and a brush are not, and come
    // from `lamine.yaml`. Start from Owner B so those survive, then let Owner A
    // overwrite the property it owns.
    let mut stroke: StrokeProps = record.stroke.clone().unwrap_or_default();
    match declarations.get("border") {
        Some(value) => match read_border(value) {
            Some(border) if border.none => node.strokes = Vec::new(),
            Some(border) => {
                node.strokes = vec![Paint::solid(border.color)];
                stroke.weight = border.width;
            }
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "border",
                format!("{value:?} is not a solid border"),
            )),
        },
        None => {
            if node.strokes.is_empty() {
                unresolved.push(Unresolved::new(
                    record.node_id(),
                    &record.key,
                    "border",
                    "not authored; the engine's own default stroke stands",
                ));
            }
        }
    }
    if let Some(profile) = record.width_profile.clone() {
        stroke.width_profile = Some(profile);
    }
    if let Some(brush) = record.brush.clone() {
        stroke.brush = Some(brush);
    }
    node.stroke = stroke;
}

/// Corner radii, authored in CSS, with `lamine.yaml` owning only the smoothing.
fn apply_corners(
    record: &NodeRecord,
    node: &mut Node,
    declarations: &BTreeMap<String, String>,
    unresolved: &mut Vec<Unresolved>,
) {
    let mut corners = Corners::default();
    match declarations.get("border-radius") {
        Some(value) => match read_radii(value) {
            Some(radii) => corners.radii = radii,
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "border-radius",
                format!("{value:?} is not a set of px radii"),
            )),
        },
        None => unresolved.push(Unresolved::new(
            record.node_id(),
            &record.key,
            "border-radius",
            "not authored; the engine's own square corners stand",
        )),
    }
    if let Some(declared) = record.corners {
        // `corners.radii` is Owner A. Set while no `border-radius` is authored, the
        // two owners disagree and CSS is the authority that holds.
        if declared.radii != [0.0; 4] && !declarations.contains_key("border-radius") {
            unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "border-radius",
                "lamine.yaml carries radii but no border-radius is authored; CSS is authoritative and these are not compiled",
            ));
        }
        corners.smoothing = declared.smoothing;
    }
    node.corners = corners;
}

/// `min-*`/`max-*` and the out-of-flow flag, all authored in CSS.
fn apply_child_layout(
    record: &NodeRecord,
    node: &mut Node,
    declarations: &BTreeMap<String, String>,
    unresolved: &mut Vec<Unresolved>,
) {
    // `hug` and `fill` are Owner B and have no CSS spelling, so the manifest's
    // child layout starts the node and CSS refines it.
    let mut child: ChildLayout = record.child_layout.clone().unwrap_or_default();
    for (property, slot) in [
        ("min-width", 0usize),
        ("max-width", 1),
        ("min-height", 2),
        ("max-height", 3),
    ] {
        let Some(value) = declarations.get(property) else {
            continue;
        };
        match read_length_value(value) {
            Some(number) => match slot {
                0 => child.min_w = Some(number),
                1 => child.max_w = Some(number),
                2 => child.min_h = Some(number),
                _ => child.max_h = Some(number),
            },
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                property,
                format!("{value:?} is not a length in px"),
            )),
        }
    }
    if matches!(
        declared(declarations, "position").as_deref(),
        Some("absolute") | Some("fixed")
    ) {
        child.absolute = true;
    }
    node.child_layout = child;
}

/// `display: flex` compiled to [`AutoLayout`].
fn apply_layout(
    record: &NodeRecord,
    node: &mut Node,
    declarations: &BTreeMap<String, String>,
    unresolved: &mut Vec<Unresolved>,
) {
    // `lamine.yaml` may already declare a layout (`hug`/`fill`, grid tracks,
    // strokes-in-layout, canvas stacking). Flex supplies the Owner A half of it
    // and must not drop the Owner B half, so start from the manifest's.
    let display = declared(declarations, "display");
    if !matches!(display.as_deref(), Some("flex") | Some("inline-flex")) {
        // The flex properties are authored but there is no layout to put them in.
        // Reporting them is the only honest encoding: silently ignoring them would
        // make the compiled document look as if the author had not written them.
        for property in LAYOUT_PROPERTIES {
            if let Some(value) = declarations.get(*property) {
                unresolved.push(Unresolved::new(
                    record.node_id(),
                    &record.key,
                    property,
                    format!(
                        "{value:?} is authored but the node is not a flex container, so it has no auto layout"
                    ),
                ));
            }
        }
        return;
    }
    let mut layout: AutoLayout = record.layout.clone().unwrap_or_default();

    match declared(declarations, "flex-direction") {
        Some(value) => match read_layout_mode(&value) {
            Some((mode, reversed)) => {
                layout.mode = mode;
                if reversed {
                    unresolved.push(Unresolved::new(
                        record.node_id(),
                        &record.key,
                        "flex-direction",
                        format!("{value:?} reverses the order, which auto-layout has no flag for"),
                    ));
                }
            }
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "flex-direction",
                format!("{value:?} has no auto-layout counterpart"),
            )),
        },
        None => unresolved.push(Unresolved::new(
            record.node_id(),
            &record.key,
            "flex-direction",
            "not authored; auto-layout's own horizontal mode stands",
        )),
    }

    // `row-gap` separates items along a vertical main axis and separates the
    // rows of a horizontal one, so which engine axis it fills depends on the
    // mode. `gap` is the shorthand for both axes and sets both.
    let horizontal = layout.mode == LayoutMode::Horizontal;
    for property in ["gap", "row-gap", "column-gap"] {
        let Some(value) = declarations.get(property) else {
            continue;
        };
        let Some(number) = read_length_value(value) else {
            unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                property,
                format!("{value:?} is not a length in px"),
            ));
            continue;
        };
        match (property, horizontal) {
            ("gap", _) => {
                layout.gap = number;
                layout.cross_gap = number;
            }
            ("row-gap", true) => layout.cross_gap = number,
            ("row-gap", false) => layout.gap = number,
            (_, true) => layout.gap = number,
            (_, false) => layout.cross_gap = number,
        }
    }

    if declarations.contains_key("padding") {
        match read_box(declarations, "padding") {
            Some(padding) => layout.padding = padding,
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "padding",
                "not a set of px lengths",
            )),
        }
    }

    match declared(declarations, "justify-content") {
        Some(value) => match read_main_align(&value) {
            Some(align) => layout.main_align = align,
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "justify-content",
                format!("{value:?} has no auto-layout counterpart"),
            )),
        },
        None => unresolved.push(Unresolved::new(
            record.node_id(),
            &record.key,
            "justify-content",
            "not authored; auto-layout's own packed alignment stands",
        )),
    }

    match declared(declarations, "align-items") {
        Some(value) => match read_cross_align(&value) {
            Some(align) => layout.cross_align = align,
            None => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "align-items",
                format!(
                    "{value:?} has no auto-layout counterpart; a child sizing to fill is how it is expressed"
                ),
            )),
        },
        None => unresolved.push(Unresolved::new(
            record.node_id(),
            &record.key,
            "align-items",
            "not authored; auto-layout's own packed alignment stands",
        )),
    }

    match declared(declarations, "flex-wrap") {
        Some(value) => match value.as_str() {
            "nowrap" => layout.wrap = false,
            "wrap" => layout.wrap = true,
            "wrap-reverse" => {
                layout.wrap = true;
                unresolved.push(Unresolved::new(
                    record.node_id(),
                    &record.key,
                    "flex-wrap",
                    "wrap-reverse reverses the lines, which auto-layout has no flag for",
                ));
            }
            other => unresolved.push(Unresolved::new(
                record.node_id(),
                &record.key,
                "flex-wrap",
                format!("{other:?} has no auto-layout counterpart"),
            )),
        },
        None => unresolved.push(Unresolved::new(
            record.node_id(),
            &record.key,
            "flex-wrap",
            "not authored; auto-layout's own nowrap stands",
        )),
    }

    node.layout = Some(layout);
}

/// A keyword property, lower-cased and trimmed.
fn declared(declarations: &BTreeMap<String, String>, property: &str) -> Option<String> {
    declarations
        .get(property)
        .map(|value| value.trim().to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// Owner B: lamine.yaml
// ---------------------------------------------------------------------------

/// Copy every Owner B field of a manifest record onto the compiled node.
///
/// Anything Owner A owns is deliberately absent from this function. Radii and
/// stroke weight live in CSS, geometry is not here at all, and a field that
/// appears in this list is a claim that the manifest owns something it does not.
fn apply_owner_b(record: &NodeRecord, node: &mut Node) {
    if let Some(constraints) = record.constraints {
        node.constraints = constraints;
    }
    node.constrain_proportions = record.constrain_proportions;
    node.is_mask = record.is_mask;
    if let Some(mask_type) = record.mask_type {
        node.mask_type = mask_type;
    }
    node.locked = record.locked;
    if let Some(status) = record.dev_status {
        node.dev_status = status;
    }
    node.grids = record.grids.clone();
    node.guides = record.guides.clone();
    node.exports = record.exports.clone();
    node.interactions = record.interactions.clone();
    if let Some(styles) = record.styles.clone() {
        node.styles = styles;
    }
    for (field, variable) in &record.bound {
        node.bound.insert(field.clone(), variable.clone());
    }
    for (collection, mode) in &record.var_modes {
        node.var_modes.insert(collection.clone(), mode.clone());
    }
    node.overrides = record.overrides.clone();
    node.prop_refs = record.prop_refs.clone();
    node.bool_op = record.bool_op;
    node.annotations = record.annotations.clone();
    node.code_links = record.code_link.iter().cloned().collect();
    node.dev_resources = record.dev_resources.clone();
    node.plugin_data = record.plugin_data.clone();
    if record.scroll_position_fixed {
        node.scroll_position = ScrollPosition::Fixed;
    }
    // `PassThrough` is a container's own default blend, so an isolation flag that
    // names it needs nothing written. Anything else is not pass-through.
    if record.blend_isolation.is_some()
        && record.blend_isolation.as_deref() != Some("passThrough")
    {
        node.blend = BlendMode::Normal;
    }
}

// ---------------------------------------------------------------------------
// Value readers
// ---------------------------------------------------------------------------
//
// Each answers `None` for anything it cannot read with certainty. A `None`
// becomes an `Unresolved` entry; it never becomes a default of this module's own
// choosing.

/// Split a CSS value into its components on whitespace, keeping a parenthesised
/// group in one piece.
///
/// `split_whitespace` alone tears `rgb(22, 22, 29)` into three tokens, and a
/// shorthand whose components are functions is the ordinary case, not an edge.
fn split_value_parts(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let bytes = value.as_bytes();
    let mut start = 0usize;
    let mut index = 0usize;
    let mut depth = 0u32;
    let mut quote: Option<u8> = None;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(open) => {
                if byte == open {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                _ if byte.is_ascii_whitespace() && depth == 0 => {
                    let part = &value[start..index];
                    if !part.is_empty() {
                        parts.push(part);
                    }
                    start = index + 1;
                }
                _ => {}
            },
        }
        index += 1;
    }
    let part = &value[start..];
    if !part.is_empty() {
        parts.push(part);
    }
    parts
}

/// A CSS length in px, or `None` for a unit or keyword this reader does not
/// convert.
///
/// Only px and a bare number are accepted. `%`, `em`, `rem`, `vh`, `auto` and
/// `calc()` all depend on a box or a font this reader does not have, and
/// converting them would be inventing the number.
fn read_length_value(value: &str) -> Option<f64> {
    let lowered = value.trim().to_ascii_lowercase();
    if lowered.is_empty() || lowered.contains('(') || lowered.contains('%') {
        return None;
    }
    let number = lowered.strip_suffix("px").unwrap_or(&lowered).trim();
    let parsed: f64 = number.parse().ok()?;
    parsed.is_finite().then_some(parsed)
}

/// A bare CSS number, or a percentage of one.
fn read_number(value: &str) -> Option<f64> {
    let lowered = value.trim().to_ascii_lowercase();
    if lowered.contains('(') {
        return None;
    }
    if let Some(percent) = lowered.strip_suffix('%') {
        let parsed: f64 = percent.trim().parse().ok()?;
        return parsed.is_finite().then_some(parsed / 100.0);
    }
    let parsed: f64 = lowered.parse().ok()?;
    parsed.is_finite().then_some(parsed)
}

/// A solid CSS colour.
///
/// A gradient, an image or anything else this reader does not compile comes back
/// `None` and is reported; it is never approximated with a colour.
fn read_color(value: &str) -> Option<Color> {
    let lowered = value.trim().to_ascii_lowercase();
    let colour_function = ["rgba(", "rgb(", "hsla(", "hsl("]
        .iter()
        .any(|prefix| lowered.starts_with(prefix));
    if lowered.contains('(') && !colour_function {
        return None;
    }
    Color::parse_css(&lowered)
}

/// A parsed `border` shorthand.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Border {
    /// Width in px.
    width: f64,
    /// Whether the author switched the stroke off.
    none: bool,
    /// The stroke colour.
    color: Color,
}

/// A `border` shorthand of the form `<width> <style> <colour>`.
///
/// CSS does not fix the order of the components, so each is matched by what it
/// looks like rather than by position. A style that is not a solid line —
/// `dashed`, `dotted`, `double` — is refused rather than drawn as a solid one,
/// because a dashed border and a solid one are not the same authored value.
fn read_border(value: &str) -> Option<Border> {
    let mut width = None;
    let mut solid = None;
    let mut color = None;
    for part in split_value_parts(value) {
        let part = part.to_ascii_lowercase();
        match part.as_str() {
            "none" | "hidden" => {
                solid = Some(false);
                continue;
            }
            "solid" => {
                solid = Some(true);
                continue;
            }
            _ => {}
        }
        if color.is_none() && read_length_value(&part).is_none() {
            color = read_color(&part);
            continue;
        }
        if width.is_none() {
            width = read_length_value(&part);
        }
    }
    match solid? {
        // `border: none` names no colour and no width, and means no stroke.
        false => Some(Border {
            width: 0.0,
            none: true,
            color: Color::TRANSPARENT,
        }),
        true => Some(Border {
            width: width.unwrap_or(1.0),
            none: false,
            color: color?,
        }),
    }
}

/// Corner radii `[tl, tr, br, bl]` in px from a `border-radius` value.
///
/// One to four lengths in CSS's corner order. The `/` ellipse form and any
/// percentage are refused: they describe a shape this reader cannot express.
fn read_radii(value: &str) -> Option<[f64; 4]> {
    if value.contains('/') || value.contains('%') {
        return None;
    }
    let lengths: Vec<f64> = split_value_parts(value)
        .into_iter()
        .map(read_length_value)
        .collect::<Option<Vec<f64>>>()?;
    match lengths.as_slice() {
        [one] => Some([*one; 4]),
        [horizontal, vertical] => Some([*horizontal, *horizontal, *vertical, *vertical]),
        [tl, tr, br, bl] => Some([*tl, *tr, *br, *bl]),
        _ => None,
    }
}

/// A `padding` value as `[top, right, bottom, left]`, with any per-side longhand
/// overriding the side it names.
fn read_box(declarations: &BTreeMap<String, String>, property: &str) -> Option<[f64; 4]> {
    let mut sides = [0.0; 4];
    if let Some(value) = declarations.get(property) {
        let lengths: Vec<f64> = split_value_parts(value)
            .into_iter()
            .map(read_length_value)
            .collect::<Option<Vec<f64>>>()?;
        sides = match lengths.as_slice() {
            [all] => [*all; 4],
            [vertical, horizontal] => [*vertical, *horizontal, *vertical, *horizontal],
            [top, horizontal, bottom] => [*top, *horizontal, *bottom, *horizontal],
            [top, right, bottom, left] => [*top, *right, *bottom, *left],
            _ => return None,
        };
    }
    for (index, edge) in ["top", "right", "bottom", "left"].into_iter().enumerate() {
        if let Some(value) = declarations.get(&format!("{property}-{edge}")) {
            sides[index] = read_length_value(value)?;
        }
    }
    Some(sides)
}

/// `flex-direction`, and whether it reverses the order.
fn read_layout_mode(value: &str) -> Option<(LayoutMode, bool)> {
    match value {
        "row" => Some((LayoutMode::Horizontal, false)),
        "row-reverse" => Some((LayoutMode::Horizontal, true)),
        "column" => Some((LayoutMode::Vertical, false)),
        "column-reverse" => Some((LayoutMode::Vertical, true)),
        _ => None,
    }
}

/// `justify-content`.
fn read_main_align(value: &str) -> Option<MainAlign> {
    match value {
        "flex-start" | "start" | "left" | "normal" => Some(MainAlign::Min),
        "center" => Some(MainAlign::Center),
        "flex-end" | "end" | "right" => Some(MainAlign::Max),
        "space-between" => Some(MainAlign::SpaceBetween),
        "space-around" => Some(MainAlign::SpaceAround),
        "space-evenly" => Some(MainAlign::SpaceEvenly),
        _ => None,
    }
}

/// `align-items`.
///
/// `stretch` has no counterpart: in auto-layout a child that fills its cell is
/// what stretching means, and that is the child's Owner B sizing.
fn read_cross_align(value: &str) -> Option<CrossAlign> {
    match value {
        "flex-start" | "start" | "self-start" | "normal" => Some(CrossAlign::Min),
        "center" => Some(CrossAlign::Center),
        "flex-end" | "end" | "self-end" => Some(CrossAlign::Max),
        "baseline" | "first baseline" | "last baseline" => Some(CrossAlign::Baseline),
        _ => None,
    }
}

/// `text-align`.
fn read_align_h(value: &str) -> Option<TextAlignH> {
    match value {
        "start" | "left" | "justify" => Some(TextAlignH::Left),
        "center" => Some(TextAlignH::Center),
        "end" | "right" => Some(TextAlignH::Right),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use figcraft_color::ColorProfile;
    use figcraft_doc::{
        BoolOp, Constraints, DevStatus, HConstraint, MaskType, VConstraint,
    };
    use crate::manifest::SourceBinding;

    fn record(id: u64, key: &str, parent: Option<u64>, children: Vec<u64>) -> NodeRecord {
        NodeRecord {
            id,
            key: key.to_owned(),
            name: key.to_owned(),
            kind: "Frame".to_owned(),
            parent,
            children,
            binding: SourceBinding {
                file: "index.html".to_owned(),
                selector: format!("[data-spool-id=\"{key}\"]"),
            },
            ..NodeRecord::default()
        }
    }

    fn text_record(id: u64, key: &str, parent: Option<u64>, children: Vec<u64>) -> NodeRecord {
        NodeRecord {
            kind: "Text".to_owned(),
            ..record(id, key, parent, children)
        }
    }

    fn files(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(path, contents)| ((*path).to_owned(), (*contents).to_owned()))
            .collect()
    }

    /// A one-node project whose element carries an inline `style`.
    fn inline(style: &str, id: u64, key: &str) -> (Manifest, BTreeMap<String, String>) {
        let html = format!("<div data-spool-id=\"{key}\" style=\"{style}\"></div>");
        (
            Manifest {
                nodes: vec![record(id, key, None, vec![])],
                ..Default::default()
            },
            files(&[("index.html", &html)]),
        )
    }

    /// A project whose element is styled by a linked stylesheet.
    fn styled(css: &str, key: &str, kind: &str) -> (Manifest, BTreeMap<String, String>) {
        // The class carries the key as well as the data-spool-id, so the fixtures
        // can use the ordinary `.name` selector the engine's cascade understands.
        let html = format!(
            "<link rel=\"stylesheet\" href=\"styles.css\">\n\
             <div class=\"{key}\" data-spool-id=\"{key}\"></div>"
        );
        (
            Manifest {
                nodes: vec![NodeRecord {
                    kind: kind.to_owned(),
                    ..record(1, key, None, vec![])
                }],
                ..Default::default()
            },
            files(&[("index.html", &html), ("styles.css", css)]),
        )
    }

    fn node_of(document: &Document, id: u64) -> &Node {
        document
            .node(NodeId(id))
            .unwrap_or_else(|| panic!("node {id} is in the document"))
    }

    fn solid_color(paint: &Paint) -> Color {
        paint
            .solid_color()
            .unwrap_or_else(|| panic!("the paint is solid"))
    }

    fn hex(value: &str) -> Color {
        Color::from_hex(value).unwrap_or_else(|| panic!("{value} is a colour"))
    }

    #[test]
    fn an_inline_project_compiles_to_the_values_it_authors() {
        let (manifest, sources) = inline(
            "position: absolute; left: 12px; top: 24px; width: 320px; height: 180px; \
             background-color: #3b5bfd; opacity: 0.5; border-radius: 8px; border: 2px solid #16161d",
            1,
            "card",
        );
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");

        let node = node_of(&read.document, 1);
        assert_eq!(node.name, "card");
        assert_eq!(node.kind, NodeKind::Frame);
        assert_eq!(node.width, 320.0);
        assert_eq!(node.height, 180.0);
        assert_eq!(node.transform, Affine::translate((12.0, 24.0)));
        assert_eq!(node.opacity, 0.5);
        assert_eq!(node.corners.radii, [8.0; 4]);
        assert_eq!(node.stroke.weight, 2.0);
        assert_eq!(node.fills.first().map(solid_color), Some(hex("3b5bfd")));
        assert_eq!(node.strokes.first().map(solid_color), Some(hex("16161d")));
        assert!(node.visible, "nothing authored display: none");
        assert_eq!(node.parent, Some(PAGE_ID));
        assert_eq!(read.document.pages, vec![PAGE_ID]);
        assert_eq!(read.document.current_page, Some(PAGE_ID));
        assert!(
            read.document.validate().is_empty(),
            "the compiled document is well formed: {:?}",
            read.document.validate()
        );
    }

    #[test]
    fn a_stylesheet_is_read_in_link_order_and_an_inline_style_wins() {
        let base = ".lead { color: #ff0000; font-size: 10px; }";
        let over = ".lead { font-size: 20px; }";
        let manifest = Manifest {
            nodes: vec![text_record(1, "t", None, vec![])],
            ..Default::default()
        };
        let read = read_with_diagnostics(
            &manifest,
            &files(&[
                (
                    "index.html",
                    "<link rel=\"stylesheet\" href=\"base.css\">\n\
                     <link rel=\"stylesheet\" href=\"over.css\">\n\
                     <p data-spool-id=\"t\" class=\"lead\">Hello</p>",
                ),
                ("base.css", base),
                ("over.css", over),
            ]),
        )
        .expect("compiles");
        let text = node_of(&read.document, 1)
            .kind
            .text()
            .cloned()
            .expect("a text node");
        assert_eq!(text.style.font_size, 20.0, "the later sheet wins");
        assert_eq!(
            text.style
                .fills
                .as_ref()
                .and_then(|fills| fills.first().map(solid_color)),
            Some(hex("ff0000")),
            "and the earlier sheet still supplies what the later one does not"
        );

        // An inline declaration outranks both sheets.
        let read = read_with_diagnostics(
            &manifest,
            &files(&[
                (
                    "index.html",
                    "<link rel=\"stylesheet\" href=\"base.css\">\n\
                     <p data-spool-id=\"t\" class=\"lead\" style=\"color: #00ff00\">Hello</p>",
                ),
                ("base.css", base),
            ]),
        )
        .expect("compiles");
        let text = node_of(&read.document, 1)
            .kind
            .text()
            .cloned()
            .expect("a text node");
        assert_eq!(
            text.style
                .fills
                .as_ref()
                .and_then(|fills| fills.first().map(solid_color)),
            Some(hex("00ff00"))
        );
    }

    #[test]
    fn specificity_outranks_the_order_stylesheets_are_linked_in() {
        let manifest = Manifest {
            nodes: vec![record(1, "card", None, vec![])],
            ..Default::default()
        };
        let width_of = |sources: &[(&str, &str)]| -> Result<f64, ReadError> {
            let read = read_with_diagnostics(&manifest, &files(sources))?;
            Ok(node_of(&read.document, 1).width)
        };
        let linked = "<link rel=\"stylesheet\" href=\"first.css\">\n\
                      <link rel=\"stylesheet\" href=\"second.css\">\n\
                      <div id=\"hero\" class=\"theme card\" data-spool-id=\"card\"></div>";
        let reversed = "<link rel=\"stylesheet\" href=\"second.css\">\n\
                        <link rel=\"stylesheet\" href=\"first.css\">\n\
                        <div id=\"hero\" class=\"theme card\" data-spool-id=\"card\"></div>";
        let by_id = "#hero { width: 320px; }";
        let by_class = ".theme { width: 40px; }";

        // The id rule is authored first and the class rule second. Only the
        // cascade can tell that `#hero` outranks `.theme`; link order says the
        // opposite, and a merge that reads it that way compiles a width the
        // author never asked for — with no diagnostic to say so.
        assert_eq!(
            width_of(&[
                ("index.html", linked),
                ("first.css", by_id),
                ("second.css", by_class)
            ]),
            Ok(320.0),
            "an id beats a class even when its stylesheet is linked first"
        );
        assert_eq!(
            width_of(&[
                ("index.html", reversed),
                ("first.css", by_id),
                ("second.css", by_class)
            ]),
            Ok(320.0),
            "and still beats it when its stylesheet is linked last"
        );

        // The same two rules in one sheet, where the cascade has always been
        // applied: merging several sheets must not change that answer.
        assert_eq!(
            width_of(&[
                ("index.html", linked),
                (
                    "first.css",
                    "#hero { width: 320px; }\n.theme { width: 40px; }"
                ),
                ("second.css", "")
            ]),
            Ok(320.0),
            "the same rules in one stylesheet resolve the same way"
        );

        // Equal specificity is the case link order *does* decide, and it decides
        // it before author order, because author order is local to one sheet.
        assert_eq!(
            width_of(&[
                ("index.html", linked),
                ("first.css", ".theme { width: 320px; }"),
                ("second.css", by_class)
            ]),
            Ok(40.0),
            "two class rules of equal specificity are decided by link order"
        );
        assert_eq!(
            width_of(&[
                ("index.html", linked),
                (
                    "first.css",
                    ".theme { width: 320px; }\n.theme { width: 40px; }"
                ),
                ("second.css", "")
            ]),
            Ok(40.0),
            "and two of them in one sheet by author order"
        );
        assert_eq!(
            width_of(&[
                ("index.html", linked),
                ("first.css", ".theme { width: 320px; }"),
                ("second.css", ".theme { width: 40px; }")
            ]),
            Ok(40.0),
            "which agrees in both directions"
        );

        // An inline declaration outranks both sheets, whichever of them wins.
        assert_eq!(
            width_of(&[
                ("index.html", "<link rel=\"stylesheet\" href=\"first.css\">\n<link rel=\"stylesheet\" href=\"second.css\">\n<div id=\"hero\" class=\"theme card\" data-spool-id=\"card\" style=\"width: 8px\"></div>"),
                ("first.css", by_id),
                ("second.css", by_class),
            ]),
            Ok(8.0),
            "an inline style still beats every stylesheet"
        );
    }

    #[test]
    fn an_important_declaration_is_refused_even_when_it_loses_the_cascade() {
        let manifest = Manifest {
            nodes: vec![record(1, "card", None, vec![])],
            ..Default::default()
        };
        // `#hero` outranks `.theme`, so a cascade that ignores `!important`
        // resolves 320px and discards the flagged 40px on the way. Reading the
        // winner and never seeing the flag would compile a width the author
        // explicitly marked as not-to-be-overridden, and say nothing.
        //
        // CSS would answer 40px, because `!important` outranks an id. This
        // cascade does not implement that rule, so it cannot answer at all:
        // reporting is the only answer that is not a value the author did not
        // write.
        let document = "<link rel=\"stylesheet\" href=\"theme.css\">\n\
                        <link rel=\"stylesheet\" href=\"layout.css\">\n\
                        <div id=\"hero\" class=\"theme card\" data-spool-id=\"card\"></div>";
        assert_eq!(
            read(
                &manifest,
                &files(&[
                    ("index.html", document),
                    ("theme.css", ".theme { width: 40px !important; }"),
                    ("layout.css", "#hero { width: 320px; }"),
                ]),
            ),
            Err(ReadError::AmbiguousCascade {
                id: 1,
                property: "width".to_owned(),
            }),
            "a losing !important is the case that must be reported, not the one that is dropped"
        );
        // The same two rules in one sheet, where the flagged declaration still
        // loses: the flag does not become reportable only by being linked first.
        assert_eq!(
            read(
                &manifest,
                &files(&[
                    ("index.html", "<link rel=\"stylesheet\" href=\"theme.css\">\n<div id=\"hero\" class=\"theme card\" data-spool-id=\"card\"></div>"),
                    ("theme.css", ".theme { width: 40px !important; } #hero { width: 320px; }"),
                ]),
            ),
            Err(ReadError::AmbiguousCascade {
                id: 1,
                property: "width".to_owned(),
            })
        );
        // And in the order where the flagged rule *would* win the cascade as
        // implemented, which the old check did catch — that one must stay an
        // error too, not become a value.
        assert_eq!(
            read(
                &manifest,
                &files(&[
                    ("index.html", "<link rel=\"stylesheet\" href=\"theme.css\">\n<div id=\"hero\" class=\"theme card\" data-spool-id=\"card\"></div>"),
                    ("theme.css", "#hero { width: 320px; } .theme { width: 40px !important; }"),
                ]),
            ),
            Err(ReadError::AmbiguousCascade {
                id: 1,
                property: "width".to_owned(),
            })
        );
    }

    #[test]
    fn an_important_declaration_on_a_selector_that_does_not_match_is_ignored() {
        let manifest = Manifest {
            nodes: vec![record(1, "card", None, vec![])],
            ..Default::default()
        };
        let element = "<link rel=\"stylesheet\" href=\"styles.css\">\n\
                       <div class=\"card\" data-spool-id=\"card\"></div>";
        // `.other` never matches this element, so the flag on it competes with
        // nothing and says nothing about this node. Refusing here would make the
        // restriction unusable: any project with an important declaration
        // anywhere in its stylesheets would stop compiling.
        let read = read_with_diagnostics(
            &manifest,
            &files(&[
                ("index.html", element),
                (
                    "styles.css",
                    ".other { width: 40px !important; }\n.card { width: 320px; }",
                ),
            ]),
        )
        .expect("an important declaration on another selector is not this node's problem");
        assert_eq!(
            node_of(&read.document, 1).width,
            320.0,
            "and the matching rule is the one that styles the element"
        );

        // A stylesheet with no flag at all is untouched by any of this.
        let read = read_with_diagnostics(
            &manifest,
            &files(&[
                ("index.html", element),
                (
                    "styles.css",
                    ".other { width: 40px; }\n.card { width: 320px; }",
                ),
            ]),
        )
        .expect("compiles");
        assert_eq!(node_of(&read.document, 1).width, 320.0);
    }

    #[test]
    fn an_inline_style_block_is_read_like_a_linked_sheet() {
        let (manifest, sources) = (
            Manifest {
                nodes: vec![text_record(1, "t", None, vec![])],
                ..Default::default()
            },
            files(&[(
                "index.html",
                "<style>.t { color: #123456; }</style><p class=\"t\" data-spool-id=\"t\">Hi</p>",
            )]),
        );
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        let text = node_of(&read.document, 1)
            .kind
            .text()
            .cloned()
            .expect("a text node");
        assert_eq!(
            text.style
                .fills
                .as_ref()
                .and_then(|fills| fills.first().map(solid_color)),
            Some(hex("123456"))
        );
    }

    #[test]
    fn flex_compiles_to_auto_layout() {
        let (manifest, sources) = styled(
            ".row { display: flex; flex-direction: column; gap: 8px; row-gap: 4px; \
             column-gap: 12px; padding: 10px 20px 30px 40px; justify-content: space-between; \
             align-items: center; flex-wrap: wrap; }",
            "row",
            "Frame",
        );
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        let layout = node_of(&read.document, 1)
            .layout
            .clone()
            .expect("flex is auto layout");
        assert_eq!(layout.mode, LayoutMode::Vertical);
        // A vertical layout puts items in a column: `row-gap` is the main-axis gap
        // and `column-gap` is the one between wrapped columns.
        assert_eq!(layout.gap, 4.0, "row-gap along a vertical main axis");
        assert_eq!(layout.cross_gap, 12.0, "column-gap between wrapped columns");
        assert_eq!(layout.padding, [10.0, 20.0, 30.0, 40.0]);
        assert_eq!(layout.main_align, MainAlign::SpaceBetween);
        assert_eq!(layout.cross_align, CrossAlign::Center);
        assert!(layout.wrap);
    }

    #[test]
    fn the_gap_shorthand_fills_both_axes_and_the_horizontal_case_swaps_them() {
        let (manifest, sources) = styled(".row { display: flex; gap: 6px; }", "row", "Frame");
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        let layout = node_of(&read.document, 1).layout.clone().expect("layout");
        assert_eq!((layout.gap, layout.cross_gap), (6.0, 6.0));

        // A horizontal layout separates items with `column-gap` and rows with
        // `row-gap`, which is the other way round from the vertical case.
        let (manifest, sources) = styled(
            ".row { display: flex; flex-direction: row; row-gap: 4px; column-gap: 12px; }",
            "row",
            "Frame",
        );
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        let layout = node_of(&read.document, 1).layout.clone().expect("layout");
        assert_eq!((layout.gap, layout.cross_gap), (12.0, 4.0));
    }

    #[test]
    fn a_node_without_display_flex_has_no_auto_layout() {
        let (manifest, sources) = styled(".row { gap: 6px; }", "row", "Frame");
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        assert_eq!(node_of(&read.document, 1).layout, None);
        // The declaration is authored and there is no layout to put it in, so it
        // is reported rather than silently dropped. `gap` resolves under its
        // longhand names, which are what the diagnostic carries.
        for property in ["row-gap", "column-gap"] {
            let entry = read
                .unresolved_property(property)
                .next()
                .unwrap_or_else(|| panic!("{property} is reported"));
            assert!(entry.detail.contains("not a flex container"), "{property}: {}", entry.detail);
        }
    }

    #[test]
    fn owner_b_sizing_survives_the_owner_a_half_of_the_layout() {
        let (mut manifest, sources) = styled(".row { display: flex; gap: 4px; }", "row", "Frame");
        manifest.nodes[0].layout = Some(AutoLayout {
            strokes_included: false,
            reverse_z: true,
            ..AutoLayout::default()
        });
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        let layout = node_of(&read.document, 1).layout.clone().expect("layout");
        assert_eq!(layout.gap, 4.0, "flex supplies the gap");
        assert!(layout.reverse_z, "and does not drop canvas stacking");
        assert!(!layout.strokes_included, "or strokes-in-layout");
    }

    #[test]
    fn colour_and_font_size_inherit_down_the_managed_chain() {
        let html = "<link rel=\"stylesheet\" href=\"styles.css\">\n\
             <section class=\"root\" data-spool-id=\"root\"><div class=\"wrap\" data-spool-id=\"wrap\">\
             <p class=\"leaf\" data-spool-id=\"leaf\">Deep</p></div></section>";
        let css = ".root { color: #16161d; font-size: 32px; } .wrap { color: #ff0000; }";
        let manifest = Manifest {
            nodes: vec![
                record(1, "root", None, vec![2]),
                record(2, "wrap", Some(1), vec![3]),
                text_record(3, "leaf", Some(2), vec![]),
            ],
            ..Default::default()
        };
        let read = read_with_diagnostics(
            &manifest,
            &files(&[("index.html", html), ("styles.css", css)]),
        )
        .expect("compiles");

        let leaf = node_of(&read.document, 3)
            .kind
            .text()
            .cloned()
            .expect("the leaf is a text node");
        assert_eq!(leaf.characters, "Deep", "its own text, not its wrappers'");
        assert_eq!(leaf.style.font_size, 32.0, "inherited through the middle node");
        assert_eq!(
            leaf.style
                .fills
                .as_ref()
                .and_then(|fills| fills.first().map(solid_color)),
            Some(hex("ff0000")),
            "the nearest ancestor that declared a colour wins"
        );
    }

    #[test]
    fn an_unauthored_property_is_reported_rather_than_guessed() {
        let (manifest, sources) = inline("", 1, "bare");
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");

        let node = node_of(&read.document, 1);
        assert_eq!(node.width, 0.0, "the engine's own default stands");
        assert_eq!(node.opacity, 1.0);
        assert!(!read.is_complete());
        for property in ["width", "height", "background-color", "border-radius", "opacity"] {
            let found: Vec<&Unresolved> = read.unresolved_property(property).collect();
            assert_eq!(found.len(), 1, "{property} is reported once: {found:?}");
            assert_eq!(found[0].node, NodeId(1));
            assert_eq!(found[0].source_key, "bare");
            assert!(
                found[0].detail.contains("not authored"),
                "{property} says what is missing: {}",
                found[0].detail
            );
        }
    }

    #[test]
    fn an_unreadable_value_is_reported_with_the_value_that_could_not_be_read() {
        let (manifest, sources) = styled(
            ".x { width: 50%; background-color: linear-gradient(red, blue); }",
            "x",
            "Frame",
        );
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        let width = read
            .unresolved_property("width")
            .next()
            .expect("the percentage width is reported");
        assert!(width.detail.contains("50%"), "{}", width.detail);
        let background = read
            .unresolved_property("background-color")
            .next()
            .expect("the gradient is reported");
        assert!(background.detail.contains("gradient"), "{}", background.detail);
        assert_eq!(
            node_of(&read.document, 1).width,
            0.0,
            "and neither was guessed"
        );
    }

    #[test]
    fn a_supported_property_this_module_does_not_compile_is_still_reported() {
        // The contract with the writer: a declaration that resolves is either
        // compiled or reported. `margin` resolves and is not compiled.
        let (manifest, sources) = styled(".x { margin: 0 auto; }", "x", "Frame");
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        let entry = read
            .unresolved_property("margin")
            .next()
            .expect("margin is reported");
        assert!(entry.detail.contains("no place"), "{}", entry.detail);
    }

    #[test]
    fn a_binding_whose_file_is_absent_is_an_error() {
        let manifest = Manifest {
            nodes: vec![record(1, "card", None, vec![])],
            ..Default::default()
        };
        let sources = files(&[("other.html", "<div data-spool-id=\"card\"></div>")]);
        assert_eq!(
            read(&manifest, &sources),
            Err(ReadError::MissingSource {
                id: 1,
                file: "index.html".to_owned()
            })
        );
    }

    #[test]
    fn a_binding_that_matches_nothing_or_two_elements_is_an_error() {
        let manifest = Manifest {
            nodes: vec![record(1, "card", None, vec![])],
            ..Default::default()
        };
        assert_eq!(
            read(
                &manifest,
                &files(&[("index.html", "<div data-spool-id=\"other\"></div>")])
            ),
            Err(ReadError::UnresolvedBinding {
                id: 1,
                file: "index.html".to_owned(),
                selector: "[data-spool-id=\"card\"]".to_owned()
            })
        );
        assert_eq!(
            read(
                &manifest,
                &files(&[(
                    "index.html",
                    "<div data-spool-id=\"card\"></div><div data-spool-id=\"card\"></div>"
                )])
            ),
            Err(ReadError::AmbiguousBinding {
                id: 1,
                file: "index.html".to_owned(),
                selector: "[data-spool-id=\"card\"]".to_owned(),
                count: 2,
            })
        );
    }

    #[test]
    fn a_selector_outside_the_supported_form_is_refused_rather_than_approximated() {
        let mut record = record(1, "card", None, vec![]);
        record.binding.selector = "#card".to_owned();
        let manifest = Manifest {
            nodes: vec![record],
            ..Default::default()
        };
        let sources = files(&[("index.html", "<div id=\"card\" data-spool-id=\"card\"></div>")]);
        assert_eq!(
            read(&manifest, &sources),
            Err(ReadError::UnsupportedSelector {
                id: 1,
                file: "index.html".to_owned(),
                selector: "#card".to_owned()
            })
        );
    }

    #[test]
    fn a_cascade_this_reader_cannot_order_is_an_error() {
        let (manifest, sources) = inline("width: 10px !important", 1, "card");
        assert_eq!(
            read(&manifest, &sources),
            Err(ReadError::AmbiguousCascade {
                id: 1,
                property: "width".to_owned()
            })
        );
    }

    #[test]
    fn malformed_markup_is_reported_rather_than_read_half_of() {
        let manifest = Manifest {
            nodes: vec![record(1, "card", None, vec![])],
            ..Default::default()
        };
        let sources = files(&[("index.html", "<div data-spool-id=\"card\" class=\"a")]);
        match read(&manifest, &sources) {
            Err(ReadError::MalformedSource { file, .. }) => assert_eq!(file, "index.html"),
            other => panic!("expected a malformed source, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        let mut record = record(1, "card", None, vec![]);
        record.kind = "Hyperframe".to_owned();
        let manifest = Manifest {
            nodes: vec![record],
            ..Default::default()
        };
        let sources = files(&[("index.html", "<div data-spool-id=\"card\"></div>")]);
        assert_eq!(
            read(&manifest, &sources),
            Err(ReadError::UnknownKind {
                id: 1,
                kind: "hyperframe".to_owned()
            })
        );
    }

    #[test]
    fn an_element_keeps_its_own_text_and_not_its_childs() {
        let html = "<p data-spool-id=\"outer\">Lead <span data-spool-id=\"inner\">inner words</span> tail</p>";
        let scanned = scan_markup("index.html", html).expect("well formed");
        let text = |key: &str| {
            scanned
                .elements
                .iter()
                .find(|element| element.spool_id.as_deref() == Some(key))
                .map(|element| element.text.clone())
                .unwrap_or_default()
        };
        assert_eq!(text("outer"), "Lead tail", "the child's words are not the parent's");
        assert_eq!(text("inner"), "inner words");
    }

    #[test]
    fn text_after_a_stray_close_tag_is_not_lost() {
        // The recovery this exists for: a close tag that matches nothing open must
        // not end the enclosing element's content, or every word after it is lost.
        let html = "<div data-spool-id=\"a\">one</em> two</div>";
        let scanned = scan_markup("index.html", html).expect("recovers");
        let text = |key: &str| {
            scanned
                .elements
                .iter()
                .find(|element| element.spool_id.as_deref() == Some(key))
                .map(|element| element.text.clone())
                .unwrap_or_default()
        };
        assert_eq!(text("a"), "one two");
    }

    #[test]
    fn an_element_left_open_at_end_of_file_keeps_the_rest_of_it() {
        let html = "<div data-spool-id=\"a\">one<p data-spool-id=\"b\">two</div>";
        let scanned = scan_markup("index.html", html).expect("recovers");
        let text = |key: &str| {
            scanned
                .elements
                .iter()
                .find(|element| element.spool_id.as_deref() == Some(key))
                .map(|element| element.text.clone())
                .unwrap_or_default()
        };
        assert_eq!(text("b"), "two");
        assert_eq!(text("a"), "one", "and the parent's own text is not its child's");
    }

    #[test]
    fn owner_b_fields_land_on_the_node_and_owner_a_ones_come_from_css() {
        let (mut manifest, sources) = styled(".card { border-radius: 4px; }", "card", "Group");
        let record = &mut manifest.nodes[0];
        record.locked = true;
        record.is_mask = true;
        record.mask_type = Some(MaskType::Luminance);
        record.dev_status = Some(DevStatus::ReadyForDev);
        record.constraints = Some(Constraints {
            horizontal: HConstraint::Center,
            vertical: VConstraint::Bottom,
        });
        record.constrain_proportions = true;
        record
            .bound
            .insert("fills/0/color".to_owned(), "VariableId:1".to_owned());
        record
            .var_modes
            .insert("CollectionId:1".to_owned(), "ModeId:dark".to_owned());
        record.overrides.insert("fills".to_owned());
        record
            .prop_refs
            .insert("visible".to_owned(), "show".to_owned());
        record.bool_op = Some(BoolOp::Subtract);
        record
            .plugin_data
            .insert("agent".to_owned(), "yes".to_owned());
        record.flow = Some("checkout".to_owned());
        manifest.color_profile = Some(ColorProfile::DisplayP3);

        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        let node = node_of(&read.document, 1);
        assert!(node.locked);
        assert!(node.is_mask);
        assert_eq!(node.mask_type, MaskType::Luminance);
        assert_eq!(node.dev_status, DevStatus::ReadyForDev);
        assert_eq!(node.constraints.horizontal, HConstraint::Center);
        assert_eq!(node.constraints.vertical, VConstraint::Bottom);
        assert!(node.constrain_proportions);
        assert_eq!(
            node.bound.get("fills/0/color").map(String::as_str),
            Some("VariableId:1")
        );
        assert_eq!(
            node.var_modes.get("CollectionId:1").map(String::as_str),
            Some("ModeId:dark")
        );
        assert!(node.overrides.contains("fills"));
        assert_eq!(node.prop_refs.get("visible").map(String::as_str), Some("show"));
        assert_eq!(node.bool_op, Some(BoolOp::Subtract));
        assert_eq!(node.kind, NodeKind::BooleanOp { op: BoolOp::Subtract });
        assert_eq!(node.plugin_data.get("agent").map(String::as_str), Some("yes"));
        assert_eq!(node.corners.radii, [4.0; 4], "the radius is CSS, not metadata");
        assert_eq!(read.document.color_profile, ColorProfile::DisplayP3);
        // A flow start has no compiled form yet, and saying so beats holding it.
        assert_eq!(read.unresolved_property("flow").count(), 1, "{:?}", read.unresolved);
    }

    #[test]
    fn a_radius_in_metadata_without_one_in_css_is_reported_as_an_owner_conflict() {
        let (mut manifest, sources) = inline("", 1, "card");
        manifest.nodes[0].corners = Some(Corners {
            radii: [9.0; 4],
            smoothing: 0.6,
        });
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        let node = node_of(&read.document, 1);
        assert_eq!(node.corners.smoothing, 0.6, "Owner B holds the smoothing");
        assert_eq!(
            node.corners.radii, [0.0; 4],
            "the radius is CSS's to give, and CSS gave none"
        );
        let entry = read
            .unresolved_property("border-radius")
            .find(|u| u.detail.contains("lamine.yaml"))
            .expect("the ownership conflict is reported");
        assert_eq!(entry.source_key, "card");
    }

    #[test]
    fn display_none_hides_a_node() {
        let (manifest, sources) = styled(".card { display: none; }", "card", "Frame");
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        assert!(!node_of(&read.document, 1).visible);
    }

    #[test]
    fn an_absolute_child_of_a_flex_frame_is_outside_its_layout() {
        let (manifest, sources) = (
            Manifest {
                nodes: vec![
                    record(1, "row", None, vec![2]),
                    record(2, "pin", Some(1), vec![]),
                ],
                ..Default::default()
            },
            files(&[(
                "index.html",
                "<div data-spool-id=\"row\"><p data-spool-id=\"pin\" \
                 style=\"position: absolute; left: 4px; top: 6px\"></p></div>",
            )]),
        );
        let read = read_with_diagnostics(&manifest, &sources).expect("compiles");
        let pin = node_of(&read.document, 2);
        assert!(pin.child_layout.absolute);
        assert_eq!(pin.transform, Affine::translate((4.0, 6.0)));
    }

    #[test]
    fn a_child_the_parent_does_not_list_is_reported_rather_than_ordered_by_guess() {
        let manifest = Manifest {
            nodes: vec![
                record(1, "root", None, vec![]),
                record(2, "child", Some(1), vec![]),
            ],
            ..Default::default()
        };
        let html = "<div data-spool-id=\"root\"><p data-spool-id=\"child\"></p></div>";
        let read = read_with_diagnostics(&manifest, &files(&[("index.html", html)])).expect("compiles");
        // It is still compiled — each child's own `parent` is the authority — but
        // the paint order it ended up with was stated nowhere.
        assert_eq!(node_of(&read.document, 1).children, vec![NodeId(2)]);
        assert_eq!(
            read.unresolved_property("hierarchy").count(),
            1,
            "{:?}",
            read.unresolved
        );
    }

    #[test]
    fn page_level_nodes_are_compiled_in_file_order_and_without_a_diagnostic() {
        let manifest = Manifest {
            nodes: vec![
                record(1, "a", None, vec![]),
                record(2, "b", None, vec![]),
                record(3, "c", None, vec![]),
            ],
            ..Default::default()
        };
        let html = "<p data-spool-id=\"a\"></p><p data-spool-id=\"b\"></p><p data-spool-id=\"c\"></p>";
        let read = read_with_diagnostics(&manifest, &files(&[("index.html", html)])).expect("compiles");
        assert_eq!(
            node_of(&read.document, PAGE_ID.0).children,
            vec![NodeId(1), NodeId(2), NodeId(3)],
            "roots hang off the page in the order lamine.yaml lists them"
        );
        assert!(
            read.unresolved_property("hierarchy").next().is_none(),
            "a root has no children list to be found in, so this is normal"
        );
        assert_eq!(read.document.node_count(), 4, "three nodes and one page");
    }

    #[test]
    fn children_are_compiled_in_the_order_lamine_yaml_states() {
        let manifest = Manifest {
            nodes: vec![
                record(1, "root", None, vec![3, 2]),
                record(2, "a", Some(1), vec![]),
                record(3, "b", Some(1), vec![]),
            ],
            ..Default::default()
        };
        let html = "<div data-spool-id=\"root\"><p data-spool-id=\"a\"></p>\
             <p data-spool-id=\"b\"></p></div>";
        let read = read_with_diagnostics(&manifest, &files(&[("index.html", html)])).expect("compiles");
        assert_eq!(
            node_of(&read.document, 1).children,
            vec![NodeId(3), NodeId(2)],
            "the metadata's order is paint order, not the markup's"
        );
        assert!(read.unresolved_property("hierarchy").next().is_none());
    }

    #[test]
    fn a_link_to_a_sheet_the_project_does_not_have_is_reported() {
        let html =
            "<link rel=\"stylesheet\" href=\"missing.css\">\n<div data-spool-id=\"card\"></div>";
        let manifest = Manifest {
            nodes: vec![record(1, "card", None, vec![])],
            ..Default::default()
        };
        let read = read_with_diagnostics(&manifest, &files(&[("index.html", html)])).expect("compiles");
        assert!(read
            .unresolved_property("stylesheet")
            .any(|u| u.detail.contains("missing.css")));
    }

    #[test]
    fn the_identity_selector_form_is_read_and_nothing_else_is() {
        for selector in [
            "[data-spool-id=\"card\"]",
            "[data-spool-id='card']",
            "  [data-spool-id=\"card\"]  ",
        ] {
            assert_eq!(parse_identity_selector(selector), Some("card"), "{selector}");
        }
        for selector in [
            "#card",
            ".card",
            "div.card",
            "[data-spool-id=card]",
            "[data-spool-id=\"",
            "[data-other=\"card\"]",
            "",
        ] {
            assert_eq!(parse_identity_selector(selector), None, "{selector}");
        }
    }

    #[test]
    fn an_href_resolves_against_the_file_that_links_it() {
        assert_eq!(
            source_key_for("pages/index.html", "../styles/site.css"),
            Some("styles/site.css".to_owned())
        );
        assert_eq!(
            source_key_for("index.html", "styles/site.css"),
            Some("styles/site.css".to_owned())
        );
        assert_eq!(source_key_for("index.html", "../../outside.css"), None);
    }

    #[test]
    fn a_value_splits_into_components_without_tearing_a_function() {
        assert_eq!(split_value_parts("1px solid #16161d"), vec!["1px", "solid", "#16161d"]);
        assert_eq!(
            split_value_parts("rgb(22, 22, 29)"),
            vec!["rgb(22, 22, 29)"],
            "a colour function's arguments are separated by commas and spaces, and must survive as one part"
        );
        assert_eq!(
            split_value_parts("10px 20px  30px"),
            vec!["10px", "20px", "30px"]
        );
        assert_eq!(split_value_parts(""), Vec::<&str>::new());
    }

    #[test]
    fn a_border_is_read_only_when_it_is_a_solid_line() {
        assert_eq!(
            read_border("1px solid #16161d"),
            Some(Border {
                width: 1.0,
                none: false,
                color: hex("16161d")
            })
        );
        // The components may be written in any order, and a colour may be a
        // function, which must survive being split on.
        assert_eq!(
            read_border("solid 2px rgb(22, 22, 29)"),
            Some(Border {
                width: 2.0,
                none: false,
                color: Color::parse_css("rgb(22, 22, 29)").unwrap_or(Color::BLACK)
            })
        );
        assert_eq!(read_border("none").map(|b| b.none), Some(true));
        // A dashed border is not a solid one, and is refused rather than drawn.
        for value in ["1px dashed #16161d", "2px dotted red", "1px double red", "1px solid"] {
            assert_eq!(read_border(value), None, "{value}");
        }
    }

    #[test]
    fn a_length_is_read_in_px_and_nothing_else_is_guessed() {
        for (value, expected) in [("10", 10.0), ("10px", 10.0), (" 10px ", 10.0), ("0", 0.0)] {
            assert_eq!(read_length_value(value), Some(expected), "{value}");
        }
        for value in ["50%", "auto", "2em", "calc(10px + 2px)", "var(--x)", "", "nonsense"] {
            assert_eq!(read_length_value(value), None, "{value}");
        }
    }

    #[test]
    fn radii_are_read_in_css_corner_order() {
        assert_eq!(read_radii("8px"), Some([8.0; 4]));
        assert_eq!(read_radii("8px 4px"), Some([8.0, 8.0, 4.0, 4.0]));
        assert_eq!(read_radii("1px 2px 3px 4px"), Some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(read_radii("50%"), None);
        assert_eq!(read_radii("8px / 4px"), None);
    }
}