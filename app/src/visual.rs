//! Source interpretation: HTML and CSS into runtime text, style, and geometry.
//!
//! # The chain
//!
//! ```text
//! authored HTML + CSS
//!     -> element facts      (tag, classes, text)      from the bound parse
//!     -> style resolution   (supported subset only)    see `style`
//!     -> block layout       (deterministic)            this module
//!     -> runtime text / style / geometry
//! ```
//!
//! This replaces the two placeholders that made a loaded project look fake: the
//! `PrototypeGeometry::placeholder(index)` column and the default canvas style.
//!
//! # Layout scope
//!
//! Block flow only. Children stack vertically inside their parent, margins and
//! padding are honoured, explicit `width`/`height` are honoured, and text
//! boxes are sized from their font size. There is no inline layout, no flexbox,
//! no floats, no absolute positioning, and no text wrapping. That is enough for
//! the current fixtures to look like their source, and it is deliberately a
//! separate stage so a better layout engine can replace it without touching
//! style resolution.
//!
//! Everything here is derived and disposable. Nothing writes back to source,
//! and `lamine.yaml` never learns any of these values.

use std::collections::{BTreeMap, BTreeSet};

use crate::canvas::{Color, Fill, ObjectStyle, Stroke};
use crate::source_binding::{Reconciliation, SourceIndex};
use crate::source_document::{NodeId, PersistentDocument};
use crate::style::{self, RankedValue, Stylesheet};

/// Width the layout uses for a top-level frame when CSS does not say.
pub const DEFAULT_CONTENT_WIDTH: f32 = 960.0;

/// What the renderer can draw for a node, derived from source.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeVisual {
    /// The authored text, trimmed. Empty when the element has no text.
    pub text: Option<String>,
    pub style: ObjectStyle,
    pub geometry: BoxGeometry,
    /// Text colour and size, kept beside the geometry because the canvas
    /// stores them on the object rather than in `ObjectStyle`.
    pub text_color: Option<Color>,
    pub font_size: f32,
    /// True when nothing authored a background or border for this node.
    ///
    /// Meaning the renderer still substitutes its own default paint. Reported so
    /// the gap between "styled by source" and "styled by the editor's defaults"
    /// is visible rather than assumed away.
    pub unstyled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxGeometry {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Every node's visual model, keyed by persistent identity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VisualModel {
    nodes: BTreeMap<NodeId, NodeVisual>,
}

impl VisualModel {
    pub fn get(&self, id: &NodeId) -> Option<&NodeVisual> {
        self.nodes.get(id)
    }

    /// Nodes that carried no authored style at all.
    ///
    /// Reported rather than hidden: these are the ones still relying on the
    /// canvas default, which is the visible edge of what CSS support covers.
    pub fn unstyled(&self) -> Vec<&NodeId> {
        self.nodes
            .iter()
            .filter(|(_, visual)| visual.unstyled)
            .map(|(id, _)| id)
            .collect()
    }
}

/// Build the visual model for a loaded project.
///
/// Reads the bound parse for element facts and every stylesheet the project
/// references, then lays the result out deterministically.
///
/// A node authored `display: none` is absent from the model, along with its
/// subtree. There is no box to report for an element that generates none, so
/// omitting it is the honest encoding rather than a zero-size one that would
/// still be hit-tested and drawn.
pub fn build(reconciliation: &Reconciliation, document: &PersistentDocument) -> VisualModel {
    let mut model = VisualModel::default();
    let stylesheets = collect_stylesheets(document);

    // Resolve each node's style first; layout needs the resolved box values.
    let mut resolved: BTreeMap<NodeId, Resolved> = BTreeMap::new();
    for node in &document.structure.nodes {
        let Some(index) = reconciliation.files.get(&node.source.file) else {
            continue;
        };
        let Some(binding) = index.find(&node.id) else {
            continue;
        };
        let facts = element_facts(index, binding.element_range.clone());
        let declarations = resolve_across(&stylesheets, &facts);
        resolved.insert(
            node.id.clone(),
            Resolved {
                facts,
                declarations,
            },
        );
    }
    // Text colour and size cascade down the tree. This runs before layout so
    // an inherited font size also sizes the node's text box.
    let ancestry: BTreeMap<NodeId, Vec<Wrapper>> = document
        .structure
        .nodes
        .iter()
        .filter_map(|node| {
            let index = reconciliation.files.get(&node.source.file)?;
            let binding = index.find(&node.id)?;
            Some((
                node.id.clone(),
                unmanaged_wrappers(index.source.as_str(), binding.element_range.start),
            ))
        })
        .collect();
    inherit_text_properties(&mut resolved, document, &stylesheets, &ancestry);

    // Lay out from the roots. Children are positioned by their parent's
    // content box, so every node is reached by walking down from a root.
    let mut geometry: BTreeMap<NodeId, BoxGeometry> = BTreeMap::new();
    let mut not_rendered: BTreeSet<NodeId> = BTreeSet::new();
    let mut cursor = 0.0f32;
    for root in document
        .structure
        .nodes
        .iter()
        .filter(|node| node.parent.is_none())
    {
        let Some(placement) = layout_node(
            &resolved,
            &root.id,
            Flow { x: 0.0, y: cursor },
            DEFAULT_CONTENT_WIDTH,
            // A root's containing block is the page.
            ContainingBlock {
                x: 0.0,
                y: 0.0,
                width: DEFAULT_CONTENT_WIDTH,
                height: f32::MAX,
            },
            &document.structure.nodes,
            &mut not_rendered,
        ) else {
            continue;
        };
        geometry.insert(root.id.clone(), placement.geometry);
        geometry.extend(placement.subtree);
        // Roots stack down the page, except an absolutely positioned one, which
        // is placed against the page and leaves the flow where it finds it.
        if placement.in_flow {
            cursor = placement.flow_bottom + placement.margin_bottom;
        }
    }
    // A node not reachable from a root — malformed hierarchy, or a parent that
    // does not exist — still gets a box, so it is never silently missing from
    // the runtime. Recorded at a deterministic position.
    for (index, node) in document.structure.nodes.iter().enumerate() {
        if not_rendered.contains(&node.id) {
            continue;
        }
        geometry.entry(node.id.clone()).or_insert(BoxGeometry {
            x: 0.0,
            y: index as f32 * 48.0,
            width: DEFAULT_CONTENT_WIDTH,
            height: 48.0,
        });
    }

    for node in &document.structure.nodes {
        if not_rendered.contains(&node.id) {
            continue;
        }
        let Some(box_geometry) = geometry.get(&node.id).copied() else {
            continue;
        };
        model.nodes.insert(
            node.id.clone(),
            visual_of(&resolved, &node.id, box_geometry),
        );
    }
    model
}

struct Resolved {
    facts: ElementFacts,
    declarations: BTreeMap<String, String>,
}

impl Resolved {
    fn length(&self, property: &str) -> Option<f32> {
        self.declarations
            .get(property)
            .and_then(|value| style::parse_length(value))
    }

    /// A one-to-four component box value, defaulted to zero when absent.
    fn box_of(&self, property: &str) -> [f32; 4] {
        self.declarations
            .get(property)
            .and_then(|value| style::parse_box(value))
            .unwrap_or([0.0; 4])
    }

    /// Whether an enumerated property has a given authored value.
    fn is(&self, property: &str, value: &str) -> bool {
        self.declarations
            .get(property)
            .is_some_and(|authored| authored.trim().eq_ignore_ascii_case(value))
    }

    fn border(&self) -> Option<style::BorderStyle> {
        self.declarations
            .get("border")
            .and_then(|value| style::parse_border(value))
    }

    /// Height of this node's own text as one line.
    ///
    /// A single line whatever the text is: there is no inline layout, so a
    /// sentence is not measured and not wrapped. Font size is read from the
    /// resolved declarations, which by now include any inherited value, so an
    /// inherited `font-size` sizes the box too.
    fn own_text_height(&self) -> f32 {
        if self.facts.text.is_empty() {
            return 0.0;
        }
        self.length("font-size").unwrap_or(16.0) * LINE_HEIGHT
    }
}

struct ElementFacts {
    tag: String,
    classes: Vec<String>,
    id: Option<String>,
    text: String,
    /// Declarations authored on the element's own `style` attribute. These
    /// outrank the stylesheet, which is what an inline style means.
    inline: BTreeMap<String, String>,
}

/// Elements that never have a closing tag.
///
/// A first-pass scanner treats an unclosed `<br>` as opening an element, and
/// everything after it then reads as that element's content. `<p>Hello<br>World
/// </p>` loses "World" — the author's text, silently dropped, which is worse
/// than not modelling `<br>` at all.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// Extract the tag, classes, id, and direct text of an element from the
/// authored bytes.
///
/// Uses the bound element range rather than a second parse, so the facts are
/// guaranteed to describe the same bytes the identity was resolved from.
fn element_facts(index: &SourceIndex, element_range: std::ops::Range<usize>) -> ElementFacts {
    let source = index.source.as_str();
    let fragment = source.get(element_range).unwrap_or("");
    let (tag, classes, id) = element_identity(fragment);
    let open_tag_end = fragment.find('>').unwrap_or(fragment.len());
    let inline = inline_declarations(&fragment[..open_tag_end]);
    // Text is the element's own content, minus its tags. Nested elements'
    // text is deliberately not flattened: a child that is itself a managed node
    // keeps its own text.
    let text = strip_tags(fragment);
    ElementFacts {
        tag,
        classes,
        id,
        text,
        inline,
    }
}

/// Declarations authored on an element's own `style` attribute.
///
/// Later declarations override earlier ones, including across a shorthand: a
/// `style="background: red; background-color: blue"` is blue, because the inline
/// style is one declaration block and CSS expands the shorthand before the
/// cascade inside it.
fn inline_declarations(open_tag: &str) -> BTreeMap<String, String> {
    let mut inline = BTreeMap::new();
    for value in attribute_values(open_tag, "style") {
        for part in value.split(';') {
            let Some((property, raw)) = part.split_once(':') else {
                continue;
            };
            let property = property.trim().to_ascii_lowercase();
            let raw = raw.trim();
            if !property.is_empty() && !raw.is_empty() {
                inline.insert(longhand_of(&property), raw.to_owned());
            }
        }
    }
    inline
}

/// The computed longhand a property declaration contributes to.
///
/// `background` and `background-color` are one longhand spelled two ways, so
/// both land on the same key and author order decides between them — which is
/// what CSS does when it expands a shorthand before the cascade. Every other
/// property stands for itself.
fn longhand_of(property: &str) -> String {
    match property {
        "background" => "background-color".to_owned(),
        other => other.to_owned(),
    }
}

/// The authored tag, class list, and id of one element fragment.
///
/// Shared deliberately: the save layer needs the same element identity this
/// module resolves style against, and two readers of an open tag would be two
/// chances to disagree about who owns a declaration.
///
/// All three, because the cascade needs all three. The id was previously
/// dropped here and read from a private twin instead, which is how the renderer
/// ended up honouring a `#hero { … }` rule that save concluded did not exist and
/// overrode with an inline declaration. One function returning everything the
/// cascade matches on means a caller cannot forget the id by accident: the
/// tuple arity is the compiler's problem.
///
/// Attributes are read from the element's own open tag only. Searching the
/// whole fragment would pick up a child's `class` and style the parent as if it
/// carried it.
pub fn element_identity(fragment: &str) -> (String, Vec<String>, Option<String>) {
    let mut tag = String::new();
    if let Some(open) = fragment.find('<') {
        let rest = &fragment[open + 1..];
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        tag = rest[..name_end].to_ascii_lowercase();
    }
    let open_tag_end = fragment.find('>').unwrap_or(fragment.len());
    let open_tag = &fragment[..open_tag_end];
    let id = attribute_values(open_tag, "id").into_iter().next();
    // A `class` attribute is a whitespace-separated SET, not one name. Returning
    // the raw attribute handed the cascade a single class called
    // "swatch theme", which no `.swatch` or `.theme` rule can ever match — so
    // every multi-class element came out unstyled.
    let classes: Vec<String> = attribute_values(open_tag, "class")
        .iter()
        .flat_map(|value| value.split_whitespace())
        .map(str::to_owned)
        .collect();
    (tag, classes, id)
}

/// Every value authored for one attribute of an open tag.
///
/// The name must be a whole attribute name, not a substring of a longer one.
/// A substring search reads `data-class="badge"` as a `class`, so a framework's
/// data attributes would quietly become the element's style hooks.
fn attribute_values(fragment: &str, name: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = fragment;
    while let Some(at) = rest.find(name) {
        let starts_an_attribute = at == 0
            || rest[..at]
                .chars()
                .next_back()
                .is_some_and(|before| before.is_whitespace() || before == '/');
        rest = &rest[at + name.len()..];
        let Some(equals) = rest.find('=') else {
            continue;
        };
        if !starts_an_attribute {
            continue;
        }
        let after = &rest[equals + 1..];
        let trimmed = after.trim_start();
        if trimmed.starts_with('"') || trimmed.starts_with('\'') {
            let quote = trimmed.as_bytes()[0] as char;
            let Some(end) = trimmed[1..].find(quote) else {
                break;
            };
            values.push(trimmed[1..1 + end].to_owned());
            rest = &trimmed[end + 2..];
        } else {
            let end = trimmed
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(trimmed.len());
            values.push(trimmed[..end].to_owned());
            rest = &trimmed[end..];
        }
    }
    values
}

/// Extract the element's own text, excluding any nested element's text.
///
/// A nested element that is itself a managed node keeps its own text, so
/// folding it into the parent would duplicate it in the runtime.
fn strip_tags(fragment: &str) -> String {
    let Some(open) = fragment.find('>') else {
        return String::new();
    };
    let inner = &fragment[open + 1..];
    // Trim the element's own closing tag.
    let inner = match inner.rfind("</") {
        Some(at) => &inner[..at],
        None => inner,
    };

    let mut out = String::new();
    let mut depth = 0usize;
    let bytes = inner.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'<' {
            if depth == 0 {
                // Push one whole char, not one byte: multi-byte text must not
                // be split.
                let ch = inner[index..].chars().next().expect("char boundary");
                out.push(ch);
                index += ch.len_utf8();
                continue;
            }
            index += 1;
            continue;
        }
        // Consume the whole tag and classify it.
        let Some(close) = inner[index..].find('>') else {
            break;
        };
        let tag = &inner[index + 1..index + close];
        let self_closing = tag.trim_end().ends_with('/');
        let name_end = tag
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(tag.len());
        let name = tag[..name_end].to_ascii_lowercase();
        if tag.starts_with('/') {
            depth = depth.saturating_sub(1);
        } else if self_closing || VOID_ELEMENTS.contains(&name.as_str()) {
            // A void element opens nothing, and it still separates the words on
            // either side of it: dropping the tag without a separator runs
            // `Hello<br>World` into `HelloWorld`.
            if depth == 0 {
                out.push(' ');
            }
        } else {
            depth += 1;
        }
        index += close + 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The names of the project's stylesheets, in the order the cascade walks them.
///
/// Cascade order across files is document order: the sheet a `<link>` appears
/// later in wins, exactly as in a browser. Sorting by filename instead would
/// hand the last word to `zz.css` over an `aa.css` the page loaded first, so
/// the result would depend on names the author never thought about.
///
/// Sheets that nothing links are appended in name order, so the list stays
/// total and does not depend on hash iteration.
///
/// Public because save needs the same order. It used to sort names
/// alphabetically while the renderer walked link order, so a project whose
/// alphabetical order differed from its link order could have a save rewrite a
/// declaration that did not control the rendered value. One traversal, so the
/// two cannot disagree about which file won.
///
/// Returns names only. Takes the source map rather than the document because
/// the two callers that share this order do not have the same document: the
/// renderer parses each sheet from [`PersistentDocument::sources`], while save
/// must parse from the bytes on disk. Sharing the *order* is the point, and
/// sharing the bytes would be wrong.
pub fn stylesheet_order(sources: &std::collections::HashMap<String, String>) -> Vec<String> {
    let mut pages: Vec<&String> = sources
        .keys()
        .filter(|name| name.ends_with(".html"))
        .collect();
    pages.sort();
    let mut order: Vec<String> = Vec::new();
    for page in pages {
        let Some(contents) = sources.get(page) else {
            continue;
        };
        for href in linked_stylesheets(contents, page) {
            if sources.contains_key(&href) && !order.contains(&href) {
                order.push(href);
            }
        }
    }
    for name in sources.keys().filter(|name| name.ends_with(".css")) {
        if !order.contains(name) {
            order.push(name.clone());
        }
    }
    order
}

/// [`stylesheet_order`], with each name paired with its parsed stylesheet.
fn collect_stylesheets(document: &PersistentDocument) -> Vec<(String, Stylesheet)> {
    stylesheet_order(&document.sources)
        .into_iter()
        .map(|name| {
            let contents = document
                .sources
                .get(&name)
                .map(String::as_str)
                .unwrap_or("");
            (name, Stylesheet::parse(contents))
        })
        .collect()
}

/// The project-relative source keys a page's `<link rel="stylesheet">` tags
/// name, in document order.
fn linked_stylesheets(html: &str, page: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("<link") {
        rest = &rest[at..];
        let Some(end) = rest.find('>') else {
            break;
        };
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        let is_stylesheet = attribute_values(tag, "rel").iter().any(|rel| {
            rel.split_whitespace()
                .any(|token| token.eq_ignore_ascii_case("stylesheet"))
        });
        if !is_stylesheet {
            continue;
        }
        for href in attribute_values(tag, "href") {
            let href = href.trim();
            if href.is_empty() || href.starts_with('#') || href.contains("://") {
                continue;
            }
            if let Some(key) = source_key_for(page, href) {
                if !found.contains(&key) {
                    found.push(key);
                }
            }
        }
    }
    found
}

/// Resolve an href authored in `page` to the key the same file is stored under.
///
/// An href is relative to the page that links it (`../styles/site.css` from
/// `pages/index.html`) while a source key is relative to the project root
/// (`styles/site.css`). Climbing above the root is refused, matching the rule a
/// binding reference follows.
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

/// Resolve declarations for an element across every stylesheet.
///
/// Each sheet is resolved on its own and the winners are then ranked against
/// each other: specificity first, then which sheet the document links the
/// declaration from, then author order inside it. Link order is therefore the
/// *last* tie-break rather than an override. Ranking on link order alone lets a
/// later `.theme` beat an earlier `#hero`, which is a value the author never
/// asked for and which nothing about the result would reveal as wrong.
///
/// The element's `id` is passed through because an `#id` rule is the most
/// specific thing an author can write, and without it the rule is simply
/// unreachable.
fn resolve_across(
    sheets: &[(String, Stylesheet)],
    facts: &ElementFacts,
) -> BTreeMap<String, String> {
    let winners = rank_across(sheets, &facts.tag, &facts.classes, facts.id.as_deref());
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (property, winner) in winners.by_property {
        out.insert(property, winner.value);
    }
    // An inline `style` attribute outranks the stylesheet, so it is applied
    // after the merge rather than ranked into it.
    for (property, value) in &facts.inline {
        out.insert(property.clone(), value.clone());
    }
    out
}

/// The winner for every computed longhand, across every stylesheet the document
/// links.
///
/// One traversal, shared by every caller that asks an element what its sheets
/// say — including the inherited-text path, which asks one property at a time.
/// Having a single ranked merge is what stops one of those callers from
/// answering "whichever sheet mentioned it" while the other answers "whichever
/// selector was most specific".
fn rank_across(
    sheets: &[(String, Stylesheet)],
    tag: &str,
    classes: &[String],
    id: Option<&str>,
) -> Winners {
    let mut winners = Winners::default();
    for (index, (_, sheet)) in sheets.iter().enumerate() {
        for (property, resolved) in sheet.resolve_ranked(tag, classes, id) {
            winners.consider(property, index, resolved);
        }
    }
    winners
}

/// One declaration competing for a property across every sheet the document
/// links, with the ranking that decides it.
///
/// Three criteria, in the order a browser applies them: selector specificity,
/// then which sheet the declaration is linked from, then author order inside
/// that sheet. Link order sits *between* specificity and author order rather
/// than replacing both, so an earlier `#hero` outranks a later `.theme` and a
/// later `.theme` still beats an earlier `.theme`.
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

/// A cross-sheet winner and the three numbers that rank it.
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

/// Height of one line of text, as a multiple of the font size.
///
/// A fixed ratio because there is no inline layout and nothing to measure: one
/// line is one `font-size` tall plus the leading a browser leaves around it.
const LINE_HEIGHT: f32 = 1.4;

/// Where a node's border box sits when the flow is followed.
///
/// `y` is the top of the *border* box: the caller has already collapsed this
/// node's top margin against its previous sibling's bottom one, so adding the
/// margin again here would double every vertical margin in the document.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Flow {
    x: f32,
    y: f32,
}

/// The box an out-of-flow descendant is positioned against.
///
/// For an absolutely positioned child this is its parent's content box, which is
/// the rectangle CSS forms the containing block in: the area between the inner
/// edge of the border and the outer edge of the content. `left: 0` therefore
/// lands on the padding edge, not on the border — resolving it from the border
/// box instead put every absolutely positioned element back at its parent's
/// corner.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ContainingBlock {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// One node's box model, split into the three regions CSS keeps apart.
///
/// Margin sits outside the border box and is never part of [`BoxGeometry`];
/// padding and border sit inside it. Keeping them separate is what lets an
/// `auto` width fill exactly the space its own margins and its siblings leave,
/// and what makes `min-*`/`max-*` clamp the same box the editor measures.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct BoxMetrics {
    /// Top, right, bottom, left.
    margin: [f32; 4],
    padding: [f32; 4],
    /// Uniform border width, from the `border` shorthand. Per-side shorthands
    /// are not in the supported subset, so there is nothing else to read.
    border: f32,
}

impl BoxMetrics {
    fn of(entry: &Resolved) -> Self {
        Self {
            margin: BoxMetrics::sides(entry, "margin"),
            padding: BoxMetrics::sides(entry, "padding"),
            border: entry.border().map(|border| border.width).unwrap_or(0.0),
        }
    }

    /// One box property as four sides: the shorthand, with any per-side longhand
    /// overriding the side it names.
    ///
    /// `margin-bottom: 30px` is the same declaration as the bottom component of
    /// `margin`, and reading only the shorthand would drop it — so an element
    /// spaced with the longhand, which is what a browser devtools panel hands
    /// you, would come out with no spacing at all.
    fn sides(entry: &Resolved, property: &str) -> [f32; 4] {
        let mut sides = entry.box_of(property);
        for (index, edge) in ["top", "right", "bottom", "left"].into_iter().enumerate() {
            if let Some(value) = entry.length(&format!("{property}-{edge}")) {
                sides[index] = value;
            }
        }
        sides
    }

    /// The area children flow into, for a border box at `origin`.
    fn content(&self, origin: (f32, f32), width: f32, height: f32) -> ContainingBlock {
        ContainingBlock {
            x: origin.0 + self.border + self.padding[3],
            y: origin.1 + self.border + self.padding[0],
            width: (width - self.horizontal_inset()).max(0.0),
            height: (height - self.vertical_inset()).max(0.0),
        }
    }

    /// The box `left`/`top`/`right`/`bottom` resolve against, for a border box
    /// at `origin`.
    ///
    /// Identical to [`BoxMetrics::content`] on purpose. CSS forms the containing
    /// block of an absolutely positioned element at the ancestor's *padding
    /// edge*, which is where its padding ends and its content begins — the same
    /// rectangle. The name is kept separate because the two are reached at
    /// different moments: the content box is needed before the height is known
    /// (to flow children) and the containing block only after (to place an
    /// out-of-flow child against it).
    fn containing_block(&self, origin: (f32, f32), width: f32, height: f32) -> ContainingBlock {
        self.content(origin, width, height)
    }

    /// Border and padding removed from the left and right edges.
    fn horizontal_inset(&self) -> f32 {
        2.0 * self.border + self.padding[1] + self.padding[3]
    }

    /// Border and padding removed from the top and bottom edges.
    fn vertical_inset(&self) -> f32 {
        2.0 * self.border + self.padding[0] + self.padding[2]
    }
}

/// Apply `min` and then `max`, in that order.
///
/// CSS applies the minimum first and the maximum second, so a contradictory
/// pair resolves to the maximum rather than to whichever happened to be written
/// first. Written this way rather than with [`f32::clamp`], which panics when
/// `min > max`.
fn clamp_size(value: f32, min: Option<f32>, max: Option<f32>) -> f32 {
    value
        .max(min.unwrap_or(0.0))
        .min(max.unwrap_or(f32::INFINITY))
}

/// Where a node ended up, and what its parent needs to know to lay out the next
/// one.
struct Placement {
    /// This node's own border box, in the caller's frame.
    geometry: BoxGeometry,
    /// Bottom of this node's border box in the flow, ignoring its transform.
    /// A transform moves a box on screen without moving it among its siblings,
    /// so the flow cursor and an auto-height parent both read this, not
    /// `geometry.y + geometry.height`.
    flow_bottom: f32,
    /// Trailing margin, for collapsing against the next sibling.
    margin_bottom: f32,
    /// False for `position: absolute`, which does not advance the flow.
    in_flow: bool,
    /// Every descendant box, in this node's provisional frame, for the caller to
    /// translate once it knows this node's origin.
    subtree: Vec<(NodeId, BoxGeometry)>,
}

/// Lay out one node and its subtree.
///
/// Returns `None` for a node that generates no box at all — `display: none`, or
/// a node style resolution never reached. Otherwise returns the placement plus
/// the flow position the *next* sibling starts at. A node that generates no box
/// leaves the flow untouched, and its subtree is added to `not_rendered` so the
/// fallback pass does not resurrect it.
///
/// This function writes nothing outside its return value. Every descendant box
/// comes back inside the placement, still in this node's provisional frame, and
/// is translated exactly once — by the parent that knows this node's origin.
/// Recording a descendant straight into a shared map here would strand it in a
/// frame nobody revisits.
#[allow(clippy::too_many_arguments)]
fn layout_node(
    resolved: &BTreeMap<NodeId, Resolved>,
    id: &NodeId,
    flow: Flow,
    available_width: f32,
    containing_block: ContainingBlock,
    all_nodes: &[crate::source_document::StructuralNode],
    not_rendered: &mut BTreeSet<NodeId>,
) -> Option<Placement> {
    let Some(entry) = resolved.get(id) else {
        mark_subtree_not_rendered(id, all_nodes, not_rendered);
        return None;
    };
    if entry.is("display", "none") {
        mark_subtree_not_rendered(id, all_nodes, not_rendered);
        return None;
    }

    let metrics = BoxMetrics::of(entry);
    // `transform: translate(...)` offsets a box from where the flow would put it
    // without taking it out of the flow. That is what lets a moved element keep
    // its place among its siblings instead of becoming an absolutely positioned
    // box that no longer reflows with them. It applies on top of whatever
    // placement resolved, absolute or not.
    let offset = translate_of(entry);
    let absolute = entry.is("position", "absolute");

    // --- Used width -------------------------------------------------------
    //
    // An authored width is a border box, because that is the frame the editor
    // measures and writes; `box-sizing: content-box` opts back into the CSS
    // meaning. Either way the value is clamped by `min-width`/`max-width`, and
    // an `auto` width fills what is left once this node's own margins are taken
    // out — a child's margins eat into the space it shares with its siblings, so
    // ignoring them would let two 40px-margined children both claim the full
    // width and spill out of their parent.
    let offered = (available_width - metrics.margin[1] - metrics.margin[3]).max(0.0);
    let width = match entry.length("width") {
        Some(authored) => clamp_size(
            if entry.is("box-sizing", "content-box") {
                authored + metrics.horizontal_inset()
            } else {
                authored
            },
            entry.length("min-width"),
            entry.length("max-width"),
        ),
        None => clamp_size(
            offered,
            entry.length("min-width"),
            entry.length("max-width"),
        ),
    };

    // --- Children ---------------------------------------------------------
    //
    // Everything below is laid out in a frame whose origin is this node's own
    // border-box top-left, and translated to the real origin at the very end.
    // The alternative is a chicken-and-egg problem: the content box sits inside
    // this box, whose position depends on its height, which depends on the
    // content. Anchoring first and shifting once breaks the cycle without ever
    // laying a subtree out twice.
    let children: Vec<&crate::source_document::StructuralNode> = all_nodes
        .iter()
        .filter(|node| node.parent.as_ref() == Some(id))
        .collect();
    let is_out_of_flow = |child: &crate::source_document::StructuralNode| {
        resolved
            .get(&child.id)
            .is_some_and(|child| child.is("position", "absolute"))
    };
    let content = metrics.content((0.0, 0.0), width, 0.0);
    // Every flow coordinate below is measured from the content box's top, while
    // the frames the recursive calls report in are measured from this box's own
    // border-box origin. `content_top` is the one conversion between the two;
    // keeping them apart here is what stops a padded parent from reporting a
    // height that disagrees with where its children were actually put.
    let content_top = content.y;

    let mut subtree: Vec<(NodeId, BoxGeometry)> = Vec::new();
    // The element's own text is an anonymous block at the top of its content
    // box, so children flow below it rather than on top of it. Treating it as
    // the first in-flow box is what makes that true and keeps one code path.
    let own_text = entry.own_text_height();
    let mut content_extent = own_text;
    let mut flow_bottom: Option<f32> = (own_text > 0.0).then_some(own_text);
    let mut margin_bottom: f32 = 0.0;
    // Where the next in-flow child goes. Out-of-flow children do not advance it,
    // which is also what makes their static position meaningful, so the second
    // pass below re-walks the children in document order to find the right point
    // for each one.
    let mut cursor = own_text;
    let mut in_flow_steps: Vec<(NodeId, f32)> = Vec::new();
    for child in children.iter().filter(|child| !is_out_of_flow(child)) {
        let child_margin_top = resolved
            .get(&child.id)
            .map(|child| BoxMetrics::of(child).margin[0])
            .unwrap_or(0.0);
        // Adjoining margins collapse: the gap between two stacked boxes is the
        // larger of the two margins, not their sum. CSS 8.3.1 — without it every
        // list of spaced items comes out twice as tall as it looks, and the
        // geometry no longer round-trips against a browser.
        let y = content_top
            + match flow_bottom {
                Some(bottom) => bottom + margin_bottom.max(child_margin_top),
                None => child_margin_top,
            };
        let Some(placement) = layout_node(
            resolved,
            &child.id,
            Flow { x: content.x, y },
            content.width,
            // An in-flow child places itself from `flow`, so this is never read
            // for it. The real containing block is filled in below, once the
            // height is known, for the children that do read it.
            ContainingBlock {
                height: 0.0,
                ..content
            },
            all_nodes,
            not_rendered,
        ) else {
            continue;
        };
        let bottom = placement.flow_bottom - content_top;
        content_extent = content_extent.max(bottom);
        flow_bottom = Some(bottom);
        margin_bottom = placement.margin_bottom;
        cursor = bottom + placement.margin_bottom;
        in_flow_steps.push((child.id.clone(), cursor));
        subtree.push((child.id.clone(), placement.geometry));
        subtree.extend(placement.subtree);
    }

    // --- Used height ------------------------------------------------------
    //
    // `auto` grows to the content, measured to the bottom of the last in-flow
    // child's *border box*. That is what makes the last child's bottom margin
    // collapse out of this box instead of stretching it — the same collapse as
    // above, at the other end of the box — with no special case needed for it.
    // (A parent that has its own bottom padding or border does contain the
    // margin, because that padding is added below the content extent.)
    let height = match entry.length("height") {
        Some(authored) => clamp_size(
            if entry.is("box-sizing", "content-box") {
                authored + metrics.vertical_inset()
            } else {
                authored
            },
            entry.length("min-height"),
            entry.length("max-height"),
        ),
        None => clamp_size(
            content_top + content_extent + metrics.padding[2] + metrics.border,
            entry.length("min-height"),
            entry.length("max-height"),
        ),
    };

    // --- Placement --------------------------------------------------------
    //
    // `left` wins over `right` when both are authored, as CSS specifies for a box
    // that already has a definite width.
    let static_position = Flow {
        x: flow.x + metrics.margin[3],
        y: flow.y,
    };
    let (origin_x, origin_y) = if absolute {
        let x = match (entry.length("left"), entry.length("right")) {
            (Some(left), _) => containing_block.x + left,
            (None, Some(right)) => containing_block.x + containing_block.width - right - width,
            (None, None) => static_position.x,
        };
        // `bottom` measures from the far edge of the containing block, which only
        // exists if that block has a height. The page has none, so `bottom` on a
        // root falls back to where the flow would have put it rather than
        // resolving against a made-up height.
        let y = match (entry.length("top"), entry.length("bottom")) {
            (Some(top), _) => containing_block.y + top,
            (None, Some(bottom)) if containing_block.height.is_finite() => {
                containing_block.y + containing_block.height - bottom - height
            }
            _ => static_position.y,
        };
        (x, y)
    } else {
        (static_position.x, static_position.y)
    };

    // Out-of-flow children come last, now that this box's height — and so the
    // box `right`/`bottom` resolve against — is settled. The
    // children are re-walked in document order so each absolute one takes the
    // flow position of the point it appears at, which is what its `left`/`top`
    // fall back to.
    let containing_box = metrics.containing_block((0.0, 0.0), width, height);
    let mut static_position = cursor;
    for child in &children {
        if let Some((_, after)) = in_flow_steps
            .iter()
            .find(|(laid_out, _)| laid_out == &child.id)
        {
            static_position = *after;
            continue;
        }
        let Some(placement) = layout_node(
            resolved,
            &child.id,
            Flow {
                x: content.x,
                y: content_top + static_position,
            },
            content.width,
            containing_box,
            all_nodes,
            not_rendered,
        ) else {
            continue;
        };
        subtree.push((child.id.clone(), placement.geometry));
        subtree.extend(placement.subtree);
    }

    // Out of the provisional frame and into the caller's. Every descendant moves
    // with this box — a transform moves a whole subtree, in-flow children
    // because they are placed inside the transformed content area, and absolute
    // ones because their containing block is this box's content area, which the
    // transform moves too.
    let shift = (origin_x + offset.0, origin_y + offset.1);
    for (_, geometry) in subtree.iter_mut() {
        geometry.x += shift.0;
        geometry.y += shift.1;
    }

    Some(Placement {
        geometry: BoxGeometry {
            x: shift.0,
            y: shift.1,
            width,
            height,
        },
        // The flow reads the untransformed bottom: a transform does not change
        // where a box sits among its siblings.
        flow_bottom: origin_y + height,
        margin_bottom: metrics.margin[2],
        // An absolutely positioned box does not advance the flow either, which
        // is what `in_flow` records for the caller.
        in_flow: !absolute,
        subtree,
    })
}

/// A node's authored `transform: translate(...)` offset, or none.
///
/// Separate from layout so the parent can ask about a child's offset without
/// laying it out again, and so the "not a translate" case reads as one place.
fn translate_of(entry: &Resolved) -> (f32, f32) {
    entry
        .declarations
        .get("transform")
        .and_then(|value| style::parse_translate(value))
        .unwrap_or((0.0, 0.0))
}

/// Mark a node and everything under it as generating no box.
///
/// `display: none` takes the subtree out of the flow entirely, and a
/// descendant of a hidden node generates no box whatever it declares. Recording
/// that here is what stops the fallback pass, which exists to give a box to
/// every node, from handing one back to something the author hid.
fn mark_subtree_not_rendered(
    id: &NodeId,
    all_nodes: &[crate::source_document::StructuralNode],
    not_rendered: &mut BTreeSet<NodeId>,
) {
    not_rendered.insert(id.clone());
    let mut pending = vec![id.clone()];
    while let Some(current) = pending.pop() {
        for node in all_nodes
            .iter()
            .filter(|node| node.parent.as_ref() == Some(&current))
        {
            if not_rendered.insert(node.id.clone()) {
                pending.push(node.id.clone());
            }
        }
    }
}

/// Properties that inherit, and only those.
///
/// Provisional: CSS inherits a documented list of properties, and this
/// implements just the two that change how text looks. Box properties are
/// deliberately not inherited, which happens to be correct for every property
/// in [`style::SUPPORTED_PROPERTIES`] except `color` and `font-size`. The
/// nearest ancestor that declares the property wins, so a node's own
/// declaration always outranks an inherited one.
const INHERITED_PROPERTIES: &[&str] = &["color", "font-size"];

/// An element that encloses a node in the source but has no node of its own.
///
/// Read with its classes and id as well as its tag: a wrapper is styled by
/// exactly the same rules a node is, and `body.theme { color: … }` is how a
/// themed page says what colour its text is.
#[derive(Clone, Debug, PartialEq)]
struct Wrapper {
    tag: String,
    classes: Vec<String>,
    id: Option<String>,
    inline: BTreeMap<String, String>,
}

impl Wrapper {
    /// The value this wrapper contributes for one inherited property, if any.
    ///
    /// Its own `style` attribute outranks its stylesheet rules, same as for a
    /// node. The sheets are merged by [`rank_across`] rather than searched
    /// for the first one that mentions the property, because a wrapper is
    /// styled by the same cascade a node is — `body#shell` in one file and
    /// `body.theme` in another is decided by specificity, not by which file
    /// happens to be linked first.
    fn declaration(&self, property: &str, sheets: &[(String, Stylesheet)]) -> Option<String> {
        if let Some(inline) = self.inline.get(property) {
            return Some(inline.clone());
        }
        rank_across(sheets, &self.tag, &self.classes, self.id.as_deref())
            .by_property
            .get(property)
            .map(|winner| winner.value.clone())
    }
}

/// The elements that enclose `start` in `source` and have no node of their own,
/// outermost first.
///
/// A first-pass scan, not a parse: it tracks open and close tags with a stack and
/// only needs each open tag's attributes, which is all inheritance needs. Void
/// elements are listed so `<img>` never appears to swallow its siblings. Used to
/// carry a wrapper's authored text properties — most often `body`'s — down to
/// the nodes inside it, which node-level ancestry alone would miss.
fn unmanaged_wrappers(source: &str, start: usize) -> Vec<Wrapper> {
    let mut open: Vec<Wrapper> = Vec::new();
    let bytes = source.as_bytes();
    let mut index = 0usize;
    while index < start && index < bytes.len() {
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        let Some(offset) = source[index..].find('>') else {
            break;
        };
        let tag = source[index + 1..index + offset].trim();
        index += offset + 1;
        if tag.starts_with('!') {
            // Comment, doctype, or CDATA: not an element boundary.
            continue;
        }
        if let Some(closing) = tag.strip_prefix('/') {
            if let Some(position) = open.iter().rposition(|open| open.tag == closing) {
                open.truncate(position);
            }
            continue;
        }
        let name_end = tag
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(tag.len());
        let name = tag[..name_end].to_ascii_lowercase();
        if name.is_empty() || tag.ends_with('/') || VOID_ELEMENTS.contains(&name.as_str()) {
            continue;
        }
        // Read the open tag through the same readers a node's own facts use, so
        // a wrapper's classes and id are extracted exactly one way.
        let reconstructed = format!("<{tag}>");
        let (parsed_tag, classes, id) = element_identity(&reconstructed);
        open.push(Wrapper {
            tag: parsed_tag,
            classes,
            id,
            inline: inline_declarations(&reconstructed),
        });
    }
    open
}

/// Fill in inherited text properties from the ancestor chain.
///
/// Without this, `body { color: var(--ink) }` never reaches a headline and every
/// text node falls back to the canvas default — which is exactly the "fake
/// visual bridge" this module exists to remove. Walking up rather than
/// iterating in document order keeps it correct whatever order the metadata
/// lists nodes in.
fn inherit_text_properties(
    resolved: &mut BTreeMap<NodeId, Resolved>,
    document: &PersistentDocument,
    sheets: &[(String, Stylesheet)],
    ancestry: &BTreeMap<NodeId, Vec<Wrapper>>,
) {
    let parents: BTreeMap<NodeId, Option<NodeId>> = document
        .structure
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.parent.clone()))
        .collect();
    let nodes: Vec<NodeId> = document
        .structure
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect();

    for id in nodes {
        // Guard against a parent cycle, which metadata validation should already
        // have rejected, rather than looping forever on malformed input.
        let mut chain = Vec::new();
        let mut cursor = parents.get(&id).cloned().flatten();
        while let Some(current) = cursor {
            if chain.contains(&current) {
                break;
            }
            chain.push(current.clone());
            cursor = parents.get(&current).cloned().flatten();
        }
        // Collect first, then mutate: the ancestor lookup needs shared access to the
        // same map this node is about to be written into.
        let mut inherited: Vec<(&str, String)> = Vec::new();
        for property in INHERITED_PROPERTIES {
            if resolved
                .get(&id)
                .is_some_and(|entry| entry.declarations.contains_key(*property))
            {
                continue;
            }
            // Nearest managed ancestor first, then the nearest unmanaged
            // wrapper. `body { color: … }` lives on an element Spool has no node
            // for, so without the wrapper step that colour never reaches the
            // headline inside it.
            let value = chain
                .iter()
                .filter_map(|ancestor| resolved.get(ancestor))
                .find_map(|ancestor| ancestor.declarations.get(*property).cloned())
                .or_else(|| {
                    ancestry.get(&id).and_then(|wrappers| {
                        // Outermost first in the source, so reversed is nearest
                        // first — the same order the managed chain is walked in.
                        wrappers
                            .iter()
                            .rev()
                            .find_map(|wrapper| wrapper.declaration(property, sheets))
                    })
                });
            if let Some(value) = value {
                inherited.push((property, value));
            }
        }
        if let Some(entry) = resolved.get_mut(&id) {
            for (property, value) in inherited {
                entry.declarations.insert(property.to_owned(), value);
            }
        }
    }
}

fn visual_of(
    resolved: &BTreeMap<NodeId, Resolved>,
    id: &NodeId,
    geometry: BoxGeometry,
) -> NodeVisual {
    let entry = &resolved[id];
    let declarations = &entry.declarations;

    // `background` and `background-color` arrive already merged under one key by
    // the cascade, so whichever the author wrote last is the one read here.
    let background = declarations
        .get("background-color")
        .and_then(|value| style::parse_color(value));
    let border = entry.border().and_then(|border| border.color);

    let style_out = ObjectStyle {
        fill: background.map(|color| Fill { color }),
        stroke: border.map(|color| Stroke {
            color,
            width: entry.border().map(|border| border.width).unwrap_or(1.0),
        }),
        border_radius: declarations
            .get("border-radius")
            .and_then(|value| style::parse_length(value))
            .unwrap_or(0.0),
        opacity: declarations
            .get("opacity")
            .and_then(|value| style::parse_number(value))
            .map(|value| value.clamp(0.0, 1.0))
            .unwrap_or(1.0),
    };

    let text_color = declarations
        .get("color")
        .and_then(|value| style::parse_color(value));
    let font_size = declarations
        .get("font-size")
        .and_then(|value| style::parse_length(value))
        .unwrap_or(16.0);

    NodeVisual {
        text: (!entry.facts.text.is_empty()).then(|| entry.facts.text.clone()),
        style: style_out,
        geometry,
        text_color,
        font_size,
        // No paint of its own means the renderer default fill and border still
        // apply. This is the visible edge of CSS coverage, so it is recorded
        // rather than hidden. Text colour is not part of it: an inherited or
        // authored colour is used as authored, and only falls back when nothing
        // authored one.
        unstyled: background.is_none() && border.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fixture(fixture: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture)
    }

    fn node(id: &str) -> NodeId {
        NodeId::new(id).expect("valid id")
    }

    /// Write a throwaway project so a case the fixtures deliberately do not
    /// cover still runs against the real loader.
    fn scratch(name: &str, yaml: &str, html: &str, css: Option<&str>) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "spool-visual-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create project");
        std::fs::write(root.join("lamine.yaml"), yaml).expect("write metadata");
        std::fs::write(root.join("index.html"), html).expect("write html");
        if let Some(css) = css {
            std::fs::write(root.join("styles.css"), css).expect("write css");
        }
        root
    }

    fn node_yaml(id: &str, name: &str, kind: &str, parent: Option<&str>, children: &str) -> String {
        format!(
            "  - id: \"{id}\"\n    name: \"{name}\"\n    kind: \"{kind}\"\n    parent: {parent}\n    file: \"index.html\"\n    selector: \"[data-spool-id=\\\"{id}\\\"]\"\n    children: [{children}]\n",
            parent = parent
                .map(|parent| format!("\"{parent}\""))
                .unwrap_or_else(|| "null".to_owned()),
        )
    }

    #[test]
    fn the_landing_fixture_produces_authored_text_paint_and_geometry() {
        let visuals = crate::project_open::open_project(fixture("landing"))
            .expect("landing opens")
            .visuals;

        let frame = visuals.get(&node("spool-frame-root")).expect("frame");
        assert_eq!(
            frame.geometry,
            BoxGeometry {
                x: 0.0,
                y: 0.0,
                width: DEFAULT_CONTENT_WIDTH,
                // The frame holds the headline and the CTA and nothing else.
                height: 22.4 + 46.4,
            }
        );
        assert!(
            frame.unstyled,
            "no background or border is authored for the frame"
        );

        let headline = visuals.get(&node("spool-text-headline")).expect("headline");
        assert_eq!(
            headline.text.as_deref(),
            Some("Design in source, structure in Spool")
        );
        assert_eq!(
            headline.text_color,
            Some(Color::from_rgb(0x16161d)),
            "the ink custom property on :root, inherited from body"
        );
        assert!(headline.unstyled, "it has a colour but no paint of its own");

        let cta = visuals.get(&node("spool-cta-primary")).expect("cta");
        assert_eq!(cta.text.as_deref(), Some("Start designing"));
        assert_eq!(
            cta.style.fill,
            Some(Fill {
                color: Color::from_rgb(0x3b5bfd)
            }),
            "the accent custom property resolves through var()"
        );
        assert_eq!(cta.text_color, Some(Color::from_rgb(0xffffff)));
        assert!(!cta.unstyled, "the CTA is fully styled");
        assert_eq!(
            cta.geometry,
            BoxGeometry {
                x: 0.0,
                // Below the headline: block flow, in document order.
                y: 22.4,
                width: DEFAULT_CONTENT_WIDTH,
                // Text line plus 12px of vertical padding, top and bottom.
                height: 22.4 + 24.0,
            }
        );
    }

    #[test]
    fn a_parent_never_inherits_its_childs_class_or_text() {
        // The regression this exists for: reading attributes from the whole
        // element fragment rather than its own open tag made `main` match
        // `.cta`, so the frame came out with the button's blue background and
        // its label.
        let visuals = crate::project_open::open_project(fixture("landing"))
            .expect("landing opens")
            .visuals;
        let frame = visuals.get(&node("spool-frame-root")).expect("frame");
        assert_eq!(frame.style.fill, None, "the frame has no authored fill");
        assert_eq!(frame.text, None, "and no text of its own");
    }

    #[test]
    fn an_inline_style_outranks_the_stylesheet() {
        let yaml = format!(
            "version: 1\nnodes:\n{}{}",
            node_yaml("spool-card", "Card", "frame", None, ""),
            node_yaml("spool-label", "Label", "text", Some("spool-card"), ""),
        );
        let html = "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-card\" style=\"background-color: #ff0000; padding: 4px\">\n    <span data-spool-id=\"spool-label\" style=\"color: #00ff00\">Hi</span>\n  </div>\n</body>\n";
        let root = scratch(
            "inline",
            &yaml,
            html,
            Some(".card, div { background-color: #0000ff; }\n"),
        );
        let visuals = crate::project_open::open_project(&root)
            .expect("opens")
            .visuals;

        assert_eq!(
            visuals.get(&node("spool-card")).expect("card").style.fill,
            Some(Fill {
                color: Color::from_rgb(0xff0000)
            }),
            "the inline declaration wins over the stylesheet"
        );
        assert_eq!(
            visuals.get(&node("spool-label")).expect("label").text_color,
            Some(Color::from_rgb(0x00ff00))
        );
        assert_eq!(
            visuals
                .get(&node("spool-label"))
                .expect("label")
                .text
                .as_deref(),
            Some("Hi")
        );
    }

    #[test]
    fn a_moved_element_comes_back_where_the_editor_left_it() {
        // What a save writes is an absolutely positioned inline style; this is
        // the read side of that same pair.
        let yaml = format!(
            "version: 1\nnodes:\n{}",
            node_yaml("spool-box", "Box", "frame", None, "")
        );
        let html = "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-box\" style=\"position: absolute; left: 40px; top: 120px; width: 220px; height: 48px\">Box</div>\n</body>\n";
        let root = scratch("absolute", &yaml, html, None);
        let visuals = crate::project_open::open_project(&root)
            .expect("opens")
            .visuals;
        let box_ = visuals.get(&node("spool-box")).expect("box");
        assert_eq!(
            box_.geometry,
            BoxGeometry {
                x: 40.0,
                y: 120.0,
                width: 220.0,
                height: 48.0
            },
            "an authored position is honoured exactly"
        );
    }

    #[test]
    fn a_transform_moves_a_box_without_moving_the_flow_around_it() {
        // The whole point of writing a move as a transform rather than as an
        // absolute position: the element is drawn where the user put it, and the
        // flow around it is exactly what the author wrote. A sibling below it
        // must not move, and a parent that grows with its content must not grow
        // to contain the offset.
        let yaml = format!(
            "version: 1\nnodes:\n{}{}{}",
            node_yaml(
                "spool-frame-root",
                "Root",
                "frame",
                None,
                "\"spool-text-headline\", \"spool-text-body\""
            ),
            node_yaml(
                "spool-text-headline",
                "Headline",
                "text",
                Some("spool-frame-root"),
                ""
            ),
            node_yaml(
                "spool-text-body",
                "Body",
                "text",
                Some("spool-frame-root"),
                ""
            )
        );
        let plain = scratch(
            "flow-plain",
            &yaml,
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-frame-root\">\n    <h1 data-spool-id=\"spool-text-headline\">Headline</h1>\n    <p data-spool-id=\"spool-text-body\">Body</p>\n  </div>\n</body>\n",
            None,
        );
        let moved = scratch(
            "flow-moved",
            &yaml,
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-frame-root\">\n    <h1 data-spool-id=\"spool-text-headline\" style=\"transform: translate(0px, 60px)\">Headline</h1>\n    <p data-spool-id=\"spool-text-body\">Body</p>\n  </div>\n</body>\n",
            None,
        );

        let before = crate::project_open::open_project(&plain)
            .expect("opens")
            .visuals;
        let after = crate::project_open::open_project(&moved)
            .expect("opens")
            .visuals;

        let headline_before = before.get(&node("spool-text-headline")).expect("headline");
        let headline_after = after.get(&node("spool-text-headline")).expect("headline");
        assert!(
            (headline_after.geometry.y - (headline_before.geometry.y + 60.0)).abs() < 0.01,
            "the box is drawn 60px lower: {} vs {}",
            headline_after.geometry.y,
            headline_before.geometry.y
        );

        let body_before = before.get(&node("spool-text-body")).expect("body");
        let body_after = after.get(&node("spool-text-body")).expect("body");
        assert_eq!(
            body_after.geometry.y, body_before.geometry.y,
            "the sibling below still starts where the flow puts it"
        );

        assert_eq!(
            after
                .get(&node("spool-frame-root"))
                .expect("root")
                .geometry
                .height,
            before
                .get(&node("spool-frame-root"))
                .expect("root")
                .geometry
                .height,
            "and the auto-height parent did not grow to contain the offset"
        );
    }

    #[test]
    fn layout_is_deterministic_for_the_same_project() {
        let first = crate::project_open::open_project(fixture("landing"))
            .expect("opens")
            .visuals;
        let second = crate::project_open::open_project(fixture("landing"))
            .expect("opens")
            .visuals;
        assert_eq!(
            first, second,
            "the same source must always produce the same model"
        );
        assert_eq!(first.unstyled().len(), 2, "the frame and the headline");
    }

    #[test]
    fn unmanaged_wrappers_finds_the_wrappers_a_node_sits_in() {
        let source = "<!doctype html>\n<html>\n  <body class=\"theme\" id=\"page\">\n    <main>\n      <img src=\"a.png\" />\n      <h1>Hi</h1>\n    </main>\n  </body>\n</html>\n";
        let start = source.find("<h1>").expect("h1 present");
        let tags = |wrappers: &[Wrapper]| {
            wrappers
                .iter()
                .map(|wrapper| wrapper.tag.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            tags(&unmanaged_wrappers(source, start)),
            vec!["html".to_owned(), "body".to_owned(), "main".to_owned()],
            "the still-open wrappers, outermost first"
        );

        // A wrapper is styled by the same rules a node is, so its own classes
        // and id have to come through — not just its tag name.
        let body = unmanaged_wrappers(source, start)
            .into_iter()
            .find(|wrapper| wrapper.tag == "body")
            .expect("body is open");
        assert_eq!(body.classes, vec!["theme".to_owned()]);
        assert_eq!(body.id.as_deref(), Some("page"));

        // A void element must not appear to contain what follows it.
        let img = source.find("<img").unwrap();
        let after_img = source[img..].find('>').unwrap() + img + 1;
        assert_eq!(
            tags(&unmanaged_wrappers(source, after_img)),
            vec!["html".to_owned(), "body".to_owned(), "main".to_owned()],
            "an `<img>` opens nothing, so its siblings share its ancestors"
        );

        // A closed element is no longer an ancestor.
        let after = source.find("</main>").unwrap() + "</main>".len();
        assert_eq!(
            tags(&unmanaged_wrappers(source, after)),
            vec!["html".to_owned(), "body".to_owned()]
        );
    }

    #[test]
    fn a_wrapper_carries_its_text_colour_into_the_nodes_inside_it() {
        // `body { color: … }` is authored on an element Spool has no node for.
        // Without the wrapper step the headline would fall back to the canvas
        // default and the opened project would not look like its source.
        let visuals = crate::project_open::open_project(fixture("landing"))
            .expect("landing opens")
            .visuals;
        assert_eq!(
            visuals
                .get(&node("spool-text-headline"))
                .expect("headline")
                .text_color,
            Some(Color::from_rgb(0x16161d)),
            "body's authored ink reaches the headline inside it"
        );
        assert_eq!(
            visuals
                .get(&node("spool-cta-primary"))
                .expect("cta")
                .text_color,
            Some(Color::from_rgb(0xffffff)),
            "and a node's own declaration still outranks the inherited one"
        );
        let unstyled: Vec<&str> = visuals.unstyled().iter().map(|id| id.as_str()).collect();
        assert_eq!(
            unstyled,
            vec!["spool-frame-root", "spool-text-headline"],
            "only the CTA authors any paint, so it is the only fully styled node"
        );
    }

    /// A project whose metadata declares `structure` and whose HTML is `html`.
    fn project(name: &str, structure: &str, html: &str, css: Option<&str>) -> PathBuf {
        let yaml = format!("version: 1\nnodes:\n{structure}");
        scratch(name, &yaml, html, css)
    }

    fn two_children(parent_style: &str, first_style: &str, second_style: &str) -> PathBuf {
        project(
            "box",
            &format!(
                "{}{}{}",
                node_yaml("spool-p", "P", "frame", None, "\"spool-a\", \"spool-b\""),
                node_yaml("spool-a", "A", "text", Some("spool-p"), ""),
                node_yaml("spool-b", "B", "text", Some("spool-p"), "")
            ),
            &format!(
                "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\" style=\"{parent_style}\">\n    \
                 <p data-spool-id=\"spool-a\" style=\"{first_style}\">A</p>\n    \
                 <p data-spool-id=\"spool-b\" style=\"{second_style}\">B</p>\n  </div>\n</body>\n"
            ),
            None,
        )
    }

    fn open(root: &Path) -> crate::visual::VisualModel {
        crate::project_open::open_project(root)
            .expect("opens")
            .visuals
    }

    /// A project whose page links `sheets` in exactly the order given.
    ///
    /// `scratch` writes one stylesheet named `styles.css`, which cannot express a
    /// case that is about *the order two files are linked in*, so the cascade
    /// across sheets needs a project of its own. `body_attributes` goes on the
    /// `<body>` tag, which is the wrapper the inherited-text path reads.
    fn linked_project(
        name: &str,
        structure: &str,
        body_attributes: &str,
        body: &str,
        sheets: &[(&str, &str)],
    ) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "spool-visual-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create project");
        std::fs::write(
            root.join("lamine.yaml"),
            format!("version: 1\nnodes:\n{structure}"),
        )
        .expect("write metadata");
        let links: String = sheets
            .iter()
            .map(|(href, _)| format!("  <link rel=\"stylesheet\" href=\"{href}\" />\n"))
            .collect();
        std::fs::write(
            root.join("index.html"),
            format!(
                "<!doctype html>\n<html>\n<head>\n{links}</head>\n<body{body_attributes}>\n{body}\n\
                 </body>\n</html>\n"
            ),
        )
        .expect("write html");
        for (href, contents) in sheets {
            std::fs::write(root.join(href), contents).expect("write css");
        }
        root
    }

    #[test]
    fn an_id_rule_outranks_a_class_rule_in_another_sheet_however_they_are_linked() {
        // The defect: each sheet was asked for its winners and the answer
        // dropped the specificity that decided them, so the merge across sheets
        // had only link order left and an earlier `#hero` lost to a later
        // `.theme`. The winner is a value the author never asked for, and
        // nothing about it looks wrong.
        let sheets = [
            ("a.css", "#hero { background-color: #ff0000; }"),
            ("b.css", ".theme { background-color: #0000ff; }"),
        ];
        for (name, sheets) in [
            ("id-beats-class-id-first", sheets.as_slice()),
            (
                "id-beats-class-class-first",
                [sheets[1], sheets[0]].as_slice(),
            ),
        ] {
            let root = linked_project(
                name,
                &node_yaml("spool-a", "A", "frame", None, ""),
                "",
                "  <div id=\"hero\" class=\"theme\" data-spool-id=\"spool-a\">A</div>",
                sheets,
            );
            assert_eq!(
                open(&root).get(&node("spool-a")).expect("node").style.fill,
                Some(Fill {
                    color: Color::from_rgb(0xff0000)
                }),
                "the id selector wins in both link orders; link order is the last \
                 tie-break, not an override"
            );
        }
    }

    #[test]
    fn equal_specificity_across_sheets_is_still_decided_by_link_order() {
        // The other half of the fix, and the reason the pre-existing link-order
        // test never caught the bug: `.theme` against `.theme` is a tie, and CSS
        // settles a tie by source order. Ranking must not have cost that.
        let sheets = [
            ("a.css", ".theme { background-color: #ff0000; }"),
            ("b.css", ".theme { background-color: #0000ff; }"),
        ];
        let root = linked_project(
            "equal-specificity",
            &node_yaml("spool-a", "A", "frame", None, ""),
            "",
            "  <div class=\"theme\" data-spool-id=\"spool-a\">A</div>",
            &sheets,
        );
        assert_eq!(
            open(&root).get(&node("spool-a")).expect("node").style.fill,
            Some(Fill {
                color: Color::from_rgb(0x0000ff)
            }),
            "two equally specific rules are settled by the order the page links them"
        );
    }

    #[test]
    fn an_inline_style_outranks_every_sheet_however_specific_those_sheets_are() {
        let root = linked_project(
            "inline-outranks-sheets",
            &node_yaml("spool-a", "A", "text", None, ""),
            "",
            "  <div id=\"hero\" class=\"theme\" data-spool-id=\"spool-a\" style=\"color: #00ff00\">A</div>",
            &[
                ("a.css", "#hero { color: #ff0000; }"),
                ("b.css", ".theme { color: #0000ff; }"),
            ],
        );
        assert_eq!(
            open(&root).get(&node("spool-a")).expect("node").text_color,
            Some(Color::from_rgb(0x00ff00)),
            "an inline declaration outranks every stylesheet, however specific"
        );
    }

    #[test]
    fn an_inherited_colour_ranks_across_sheets_the_way_a_node_own_does() {
        // The wrapper path is a second cross-sheet reader, and it was ranking on
        // something else again: `find_map` over the sheets took the first one
        // that *mentioned* the property. Here the tag rule in the first sheet
        // loses to the class rule in the second by specificity, which is also
        // what the node itself would have been given.
        let root = linked_project(
            "wrapper-across-sheets",
            &node_yaml("spool-a", "A", "text", None, ""),
            " class=\"theme\"",
            "  <p data-spool-id=\"spool-a\">A</p>",
            &[
                ("a.css", "body { color: #ff0000; }"),
                ("b.css", ".theme { color: #0000ff; }"),
            ],
        );

        assert_eq!(
            open(&root).get(&node("spool-a")).expect("node").text_color,
            Some(Color::from_rgb(0x0000ff)),
            "the inherited colour comes from the same cascade a node's own \
             declaration would use, so the class rule beats the tag rule in a \
             sheet linked earlier"
        );
    }

    #[test]
    fn padding_is_counted_once_not_twice() {
        // The defect this exists for: `inner_cursor` started at `top + padding-top`
        // and the height then added `padding-top` again, so every padded box came
        // out exactly one padding-top too tall. Children and the parent's height
        // have to agree about where the content area starts.
        let root = two_children("padding: 10px", "", "");
        let visuals = open(&root);
        let parent = visuals.get(&node("spool-p")).expect("parent");
        let first = visuals.get(&node("spool-a")).expect("first");
        let second = visuals.get(&node("spool-b")).expect("second");

        assert_eq!(
            first.geometry.y, 10.0,
            "the first child starts below the padding"
        );
        assert_eq!(
            second.geometry.y,
            10.0 + 22.4,
            "the second follows the first, not the padding again"
        );
        assert_eq!(
            parent.geometry.height,
            10.0 + 22.4 + 22.4 + 10.0,
            "one padding above and one below, each counted once"
        );
    }

    #[test]
    fn a_border_is_geometry_and_is_inside_the_width_it_was_measured_against() {
        // A border occupies space. Ignoring it means an element reads back
        // narrower than the author drew it, and `box-sizing: border-box` — the
        // frame the editor measures in — silently stops meaning anything.
        let root = project(
            "border",
            &format!(
                "{}{}",
                node_yaml("spool-p", "P", "frame", None, "\"spool-a\""),
                node_yaml("spool-a", "A", "frame", Some("spool-p"), "")
            ),
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\" style=\"border: 4px solid #ff0000\">\n    \
             <p data-spool-id=\"spool-a\" style=\"width: 100px; height: 10px\">A</p>\n  </div>\n</body>\n",
            None,
        );
        let visuals = open(&root);
        let child = visuals.get(&node("spool-a")).expect("child");
        assert_eq!(
            child.geometry.width, 100.0,
            "an authored width is a border box"
        );
        assert_eq!(
            child.geometry.x, 4.0,
            "the child sits inside the parent's border"
        );

        let parent = visuals.get(&node("spool-p")).expect("parent");
        assert_eq!(
            parent.geometry.height,
            10.0 + 8.0,
            "the auto height encloses the child and both border edges"
        );
        assert_eq!(
            parent.style.stroke,
            Some(Stroke {
                color: Color::from_rgb(0xff0000),
                width: 4.0
            }),
            "and the border is still painted"
        );
    }

    #[test]
    fn horizontal_margins_move_a_child_and_shrink_it() {
        // The defect this exists for: left and right margins were never applied
        // at all, so a margined child sat flush at x=0 and claimed the parent's
        // full width, overflowing it by exactly its own margins.
        let root = two_children("", "margin: 40px", "");
        let visuals = open(&root);
        let first = visuals.get(&node("spool-a")).expect("first");
        assert_eq!(
            first.geometry.x, 40.0,
            "the left margin offsets the border box"
        );
        assert_eq!(
            first.geometry.width,
            DEFAULT_CONTENT_WIDTH - 80.0,
            "and the right margin takes its share out of the width it fills"
        );
        let second = visuals.get(&node("spool-b")).expect("second");
        assert_eq!(
            second.geometry.x, 0.0,
            "an un-margined sibling is unaffected"
        );
        assert_eq!(second.geometry.width, DEFAULT_CONTENT_WIDTH);
    }

    #[test]
    fn adjoining_sibling_margins_collapse_to_the_larger() {
        // CSS 8.3.1: two stacked boxes are separated by the larger of the two
        // margins, not their sum. Adding them made every spaced list twice as
        // tall as the author wrote it.
        let root = two_children("", "margin-bottom: 30px", "margin-top: 30px");
        let visuals = open(&root);
        let first = visuals.get(&node("spool-a")).expect("first");
        let second = visuals.get(&node("spool-b")).expect("second");
        // Accumulated `f32` arithmetic is not exact, and the claim here is about
        // the collapse, not about the last bit of a float.
        assert!(
            (second.geometry.y - (first.geometry.y + first.geometry.height) - 30.0).abs() < 0.01,
            "30 and 30 meet as 30, not 60: {} - {} = {}",
            second.geometry.y,
            first.geometry.y + first.geometry.height,
            second.geometry.y - (first.geometry.y + first.geometry.height)
        );

        let unequal = two_children("", "margin-bottom: 10px", "margin-top: 30px");
        let visuals = open(&unequal);
        let first = visuals.get(&node("spool-a")).expect("first");
        let second = visuals.get(&node("spool-b")).expect("second");
        assert!(
            (second.geometry.y - (first.geometry.y + first.geometry.height) - 30.0).abs() < 0.01,
            "10 and 30 meet as 30, not 40"
        );
    }

    #[test]
    fn a_last_childs_bottom_margin_collapses_out_of_its_parent() {
        // The same collapse at the parent's bottom edge: a parent's `auto` height
        // does not grow to contain the last child's bottom margin.
        let root = two_children("", "", "margin-bottom: 30px");
        let visuals = open(&root);
        assert_eq!(
            visuals
                .get(&node("spool-p"))
                .expect("parent")
                .geometry
                .height,
            22.4 * 2.0,
            "the trailing margin collapses through rather than stretching the parent"
        );

        // Only the *last* margin collapses out. A margin in the middle of the
        // box still separates its own two neighbours.
        let middle = two_children("", "margin-bottom: 30px", "");
        let visuals = open(&middle);
        assert_eq!(
            visuals.get(&node("spool-b")).expect("second").geometry.y,
            22.4 + 30.0,
            "a margin between two children still opens a gap"
        );
        assert_eq!(
            visuals
                .get(&node("spool-p"))
                .expect("parent")
                .geometry
                .height,
            22.4 * 2.0 + 30.0,
            "and the parent grows to contain it"
        );

        // A parent that has its own bottom padding stops the collapse, because
        // the padding separates the parent from its child's margin.
        let padded = two_children("padding-bottom: 5px", "", "margin-bottom: 30px");
        assert_eq!(
            open(&padded)
                .get(&node("spool-p"))
                .expect("parent")
                .geometry
                .height,
            22.4 * 2.0 + 5.0,
            "bottom padding is a boundary the margin cannot collapse through"
        );
    }

    #[test]
    fn an_absolute_child_is_placed_against_the_padding_edge() {
        // `left: 0` means the padding edge, not the border edge. Resolving it
        // from the parent's border box put every saved position back at the
        // parent's corner, so a project moved in Spool reopened shifted.
        let root = project(
            "containing-block",
            &format!(
                "{}{}",
                node_yaml("spool-p", "P", "frame", None, "\"spool-a\""),
                node_yaml("spool-a", "A", "frame", Some("spool-p"), "")
            ),
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\" style=\"padding: 30px; border: 5px solid #ff0000\">\n    \
             <p data-spool-id=\"spool-a\" style=\"position: absolute; left: 0px; top: 0px; width: 10px; height: 10px\">A</p>\n  \
             </div>\n</body>\n",
            None,
        );
        let absolute = open(&root).get(&node("spool-a")).expect("child").geometry;
        assert_eq!(
            (absolute.x, absolute.y),
            (35.0, 35.0),
            "5px of border plus 30px of padding: the padding edge is where the \
             content starts"
        );

        // With no border, the padding edge is one padding width in — which is
        // exactly what the old border-box origin got wrong.
        let padding_only = project(
            "containing-block-padding",
            &format!(
                "{}{}",
                node_yaml("spool-p", "P", "frame", None, "\"spool-a\""),
                node_yaml("spool-a", "A", "frame", Some("spool-p"), "")
            ),
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\" style=\"padding: 30px\">\n    \
             <p data-spool-id=\"spool-a\" style=\"position: absolute; left: 0px; top: 0px; \
             width: 10px; height: 10px\">A</p>\n  </div>\n</body>\n",
            None,
        );
        let absolute = open(&padding_only)
            .get(&node("spool-a"))
            .expect("child")
            .geometry;
        assert_eq!(
            (absolute.x, absolute.y),
            (30.0, 30.0),
            "`left: 0` is the padding edge"
        );
    }

    #[test]
    fn right_and_bottom_place_a_box_against_the_far_edges() {
        // `right`/`bottom` were in the supported property list and read by
        // nothing, so an author who pinned a box to the bottom-right corner got
        // the top-left corner instead — geometry that looks plausible and is wrong.
        let root = project(
            "right-bottom",
            &format!(
                "{}{}",
                node_yaml("spool-p", "P", "frame", None, "\"spool-a\""),
                node_yaml("spool-a", "A", "frame", Some("spool-p"), "")
            ),
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\" style=\"width: 500px; height: 200px\">\n    \
             <p data-spool-id=\"spool-a\" style=\"position: absolute; right: 20px; bottom: 10px; \
             width: 100px; height: 50px\">A</p>\n  </div>\n</body>\n",
            None,
        );
        let absolute = open(&root).get(&node("spool-a")).expect("child").geometry;
        assert_eq!(
            (absolute.x, absolute.y),
            (380.0, 140.0),
            "500 - 100 - 20 and 200 - 50 - 10"
        );
    }

    #[test]
    fn an_absolute_box_does_not_take_up_room_in_the_flow() {
        let root = project(
            "out-of-flow",
            &format!(
                "{}{}{}",
                node_yaml("spool-p", "P", "frame", None, "\"spool-a\", \"spool-b\""),
                node_yaml("spool-a", "A", "frame", Some("spool-p"), ""),
                node_yaml("spool-b", "B", "text", Some("spool-p"), "")
            ),
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\">\n    \
             <p data-spool-id=\"spool-a\" style=\"position: absolute; left: 0px; top: 0px; height: 200px\">A</p>\n    \
             <p data-spool-id=\"spool-b\">B</p>\n  </div>\n</body>\n",
            None,
        );
        let visuals = open(&root);
        assert_eq!(
            visuals.get(&node("spool-b")).expect("sibling").geometry.y,
            0.0,
            "the sibling below is not pushed down by a box that left the flow"
        );
        assert_eq!(
            visuals
                .get(&node("spool-p"))
                .expect("parent")
                .geometry
                .height,
            22.4,
            "and an auto-height parent does not grow to contain it either"
        );
    }

    #[test]
    fn min_and_max_width_and_height_clamp_the_used_box() {
        // `min-*`/`max-*` were listed as supported and applied to nothing.
        let clamped = project(
            "clamp",
            &node_yaml("spool-a", "A", "frame", None, ""),
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-a\" \
             style=\"width: 800px; height: 800px; max-width: 400px; max-height: 300px\">A</div>\n</body>\n",
            None,
        );
        let geometry = open(&clamped).get(&node("spool-a")).expect("node").geometry;
        assert_eq!((geometry.width, geometry.height), (400.0, 300.0));

        // An `auto` width that would come out below the floor is raised to it.
        let narrow = project(
            "min-width",
            &node_yaml("spool-a", "A", "frame", None, ""),
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-a\" \
             style=\"width: 10px; min-width: 250px; height: 4px; min-height: 90px\">A</div>\n</body>\n",
            None,
        );
        let geometry = open(&narrow).get(&node("spool-a")).expect("node").geometry;
        assert_eq!((geometry.width, geometry.height), (250.0, 90.0));

        // A contradictory pair resolves to the maximum, because CSS applies the
        // minimum first and the maximum second.
        let contradictory = project(
            "max-beats-min",
            &node_yaml("spool-a", "A", "frame", None, ""),
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-a\" \
             style=\"width: 500px; min-width: 400px; max-width: 300px\">A</div>\n</body>\n",
            None,
        );
        assert_eq!(
            open(&contradictory)
                .get(&node("spool-a"))
                .expect("node")
                .geometry
                .width,
            300.0
        );
    }

    #[test]
    fn box_sizing_selects_which_frame_an_authored_width_is_measured_in() {
        // Border box is the default, because that is what the editor writes.
        // `content-box` has to actually mean something, or an element saved from
        // a browser grows by its padding every time it is reopened.
        let structure = node_yaml("spool-p", "P", "frame", None, "\"spool-a\"")
            + &node_yaml("spool-a", "A", "frame", Some("spool-p"), "");
        let border_box = project(
            "border-box",
            &structure,
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\" style=\"width: 200px; padding: 20px\">\n    \
             <p data-spool-id=\"spool-a\">A</p></div>\n</body>\n",
            None,
        );
        assert_eq!(
            open(&border_box)
                .get(&node("spool-p"))
                .expect("node")
                .geometry
                .width,
            200.0,
            "the default: the authored width is the border box"
        );

        let content_box = project(
            "content-box",
            &structure,
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\" \
             style=\"width: 200px; padding: 20px; box-sizing: content-box\">\n    \
             <p data-spool-id=\"spool-a\">A</p></div>\n</body>\n",
            None,
        );
        assert_eq!(
            open(&content_box)
                .get(&node("spool-p"))
                .expect("node")
                .geometry
                .width,
            240.0,
            "content-box: the authored width is the content, and padding sits outside it"
        );
    }

    #[test]
    fn display_none_takes_a_node_and_its_subtree_out_of_the_model() {
        // `display: none` was listed as supported and read by nothing, so a hidden
        // element got a full-size box and pushed its siblings down.
        let root = project(
            "display-none",
            &format!(
                "{}{}{}{}",
                node_yaml("spool-p", "P", "frame", None, "\"spool-a\", \"spool-b\""),
                node_yaml("spool-a", "A", "text", Some("spool-p"), "\"spool-a-child\""),
                node_yaml("spool-a-child", "A child", "text", Some("spool-a"), ""),
                node_yaml("spool-b", "B", "text", Some("spool-p"), "")
            ),
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\">\n    \
             <p data-spool-id=\"spool-a\" style=\"display: none\">A<span data-spool-id=\"spool-a-child\">kid</span></p>\n    \
             <p data-spool-id=\"spool-b\">B</p>\n  </div>\n</body>\n",
            None,
        );
        let visuals = open(&root);
        assert!(
            visuals.get(&node("spool-a")).is_none(),
            "a node that generates no box is absent, not zero-sized — a zero box \
             would still be drawn and hit-tested"
        );
        assert!(
            visuals.get(&node("spool-a-child")).is_none(),
            "and its subtree goes with it, whatever the child declares"
        );
        assert_eq!(
            visuals.get(&node("spool-b")).expect("sibling").geometry.y,
            0.0,
            "and the sibling below moves up into the space it left"
        );
    }

    #[test]
    fn an_absolute_box_still_honours_its_own_transform() {
        // A transform is an offset from wherever the box ended up, absolute or
        // not. Dropping it whenever the box was absolute meant a moved object
        // snapped back to its authored coordinates on reopen.
        let root = project(
            "absolute-transform",
            &node_yaml("spool-a", "A", "frame", None, ""),
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-a\" \
             style=\"position: absolute; left: 40px; top: 120px; width: 220px; height: 48px; \
             transform: translate(50px, 60px)\">A</div>\n</body>\n",
            None,
        );
        let geometry = open(&root).get(&node("spool-a")).expect("node").geometry;
        assert_eq!(
            (geometry.x, geometry.y),
            (90.0, 180.0),
            "the offset is applied on top"
        );
    }

    #[test]
    fn a_transform_carries_the_whole_subtree_but_not_the_flow() {
        // Two halves of one rule that have to agree: a transformed box moves
        // everything inside it, and moves nothing outside it.
        let structure = format!(
            "{}{}{}{}",
            node_yaml("spool-p", "P", "frame", None, "\"spool-a\", \"spool-b\""),
            node_yaml("spool-a", "A", "text", Some("spool-p"), "\"spool-a-child\""),
            node_yaml("spool-a-child", "A child", "text", Some("spool-a"), ""),
            node_yaml("spool-b", "B", "text", Some("spool-p"), "")
        );
        let plain = project(
            "subtree-plain",
            &structure,
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\">\n    \
             <p data-spool-id=\"spool-a\">A<span data-spool-id=\"spool-a-child\">kid</span></p>\n    \
             <p data-spool-id=\"spool-b\">B</p>\n  </div>\n</body>\n",
            None,
        );
        let moved = project(
            "subtree-moved",
            &structure,
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\" style=\"transform: translate(0px, 60px)\">\n    \
             <p data-spool-id=\"spool-a\">A<span data-spool-id=\"spool-a-child\">kid</span></p>\n    \
             <p data-spool-id=\"spool-b\">B</p>\n  </div>\n</body>\n",
            None,
        );
        let before = open(&plain);
        let after = open(&moved);
        // Compared with a tolerance: the claim is that the whole subtree moves
        // together, not that `f32` addition is exact.
        let delta = |id: &str| {
            after.get(&node(id)).expect("node").geometry.y
                - before.get(&node(id)).expect("node").geometry.y
        };
        let close = |moved: f32| (moved - 60.0).abs() < 0.01;
        assert!(close(delta("spool-p")), "the transformed box itself");
        assert!(close(delta("spool-a")), "its in-flow child");
        assert!(close(delta("spool-a-child")), "and its grandchild");
        assert!(close(delta("spool-b")), "everything after it moves too");
    }

    #[test]
    fn an_id_selector_reaches_the_element_that_carries_the_id() {
        // `#id` rules resolved correctly inside `style` and were unreachable from
        // here, because the caller passed no id at all. The most specific thing
        // an author can write silently did nothing.
        let root = project(
            "id-selector",
            &node_yaml("spool-a", "A", "frame", None, ""),
            "<!doctype html>\n<html>\n<head>\n  <link rel=\"stylesheet\" href=\"styles.css\" />\n</head>\n<body>\n  <div id=\"hero\" data-spool-id=\"spool-a\">A</div>\n</body>\n</html>\n",
            Some("#hero { background-color: #00ff00; }"),
        );
        assert_eq!(
            open(&root).get(&node("spool-a")).expect("node").style.fill,
            Some(Fill {
                color: Color::from_rgb(0x00ff00)
            })
        );
    }

    #[test]
    fn a_wrapper_is_styled_by_its_classes_and_id_too() {
        // Inheritance looked wrappers up by tag name alone, so `body.theme` and
        // `#shell` never contributed: the class and id were read off the wrapper
        // and then thrown away.
        let root = project(
            "wrapper-identity",
            &node_yaml("spool-a", "A", "text", None, ""),
            "<!doctype html>\n<html>\n<head>\n  <link rel=\"stylesheet\" href=\"styles.css\" />\n</head>\n<body class=\"theme\" id=\"shell\">\n  <p data-spool-id=\"spool-a\">A</p>\n</body>\n</html>\n",
            Some("body.theme { color: #ff00ff; }\n#shell { font-size: 30px; }"),
        );
        let visuals = open(&root);
        assert_eq!(
            visuals.get(&node("spool-a")).expect("node").text_color,
            Some(Color::from_rgb(0xff00ff)),
            "the class rule on the wrapper reaches the node inside it"
        );
        assert_eq!(
            visuals.get(&node("spool-a")).expect("node").font_size,
            30.0,
            "and so does the id rule"
        );
    }

    #[test]
    fn a_class_attribute_is_a_set_of_names_rather_than_one_name() {
        // A `class` attribute is whitespace-separated. Returning it whole gave
        // the cascade a single class called "swatch theme", which no `.swatch` or
        // `.theme` rule can match — so a multi-class element matched nothing at
        // all and rendered with no authored paint. The parser accepts `.a.b`, so
        // the selector side was ready and only the element side was not.
        let (tag, classes, id) = element_identity("<div class=\"swatch theme\" id=\"a\">x</div>");
        assert_eq!(tag, "div");
        assert_eq!(
            classes,
            vec!["swatch".to_owned(), "theme".to_owned()],
            "both names, in the order the author wrote them"
        );
        assert_eq!(id.as_deref(), Some("a"));

        let (_, spaced, _) = element_identity("<div class=\"page  page   shell\">x</div>");
        assert_eq!(
            spaced,
            vec!["page".to_owned(), "page".to_owned(), "shell".to_owned()],
            "runs of whitespace separate names. A repeat is kept rather than \
             collapsed: matching is a set membership test, so `class=\"body body\"` \
             being two entries is harmless, and not deduplicating keeps this \
             reader a faithful report of what was authored"
        );
    }

    #[test]
    fn an_element_with_several_classes_is_styled_by_each_of_them() {
        // The same defect read through the real loader, because a unit test on
        // `element_identity` only proves the split; this proves the split
        // reaches the cascade.
        let root = project(
            "multi-class",
            &node_yaml("spool-a", "A", "frame", None, ""),
            "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <div class=\"swatch theme\" data-spool-id=\"spool-a\">A</div>\n</body>\n</html>\n",
            Some(".theme { background-color: #0000ff; }\n"),
        );
        assert_eq!(
            open(&root).get(&node("spool-a")).expect("node").style.fill,
            Some(Fill {
                color: Color::from_rgb(0x0000ff)
            }),
            "`.theme` matches an element whose class attribute also names another class"
        );
    }

    #[test]
    fn a_data_attribute_is_not_read_as_a_style_hook() {
        // Attribute lookup was a substring search, so `data-class` matched `class`
        // and `data-style` matched `style`. A framework's data attributes would
        // silently become the element's styling.
        let (tag, classes, id) = element_identity(
            "<div data-class=\"bad\" class=\"good\" id=\"real\" data-spool-id=\"s\">x</div>",
        );
        assert_eq!(tag, "div");
        assert_eq!(
            classes,
            vec!["good".to_owned()],
            "only the real `class` attribute"
        );
        assert_eq!(
            id.as_deref(),
            Some("real"),
            "and the id reaches the caller, because the cascade matches on it"
        );
        let (_, none, absent) = element_identity("<div data-style=\"x\" class=\"y\">z</div>");
        assert_eq!(none, vec!["y".to_owned()]);
        assert_eq!(absent, None, "an element with no `id` says so");
        assert!(
            inline_declarations("<div data-style=\"background-color: red\">").is_empty(),
            "and an authored `data-style` is not an inline style"
        );
    }

    #[test]
    fn a_void_element_does_not_swallow_the_text_after_it() {
        // `<br>` has no closing tag. Treating it as an unclosed element put
        // everything after it at depth 1, so it was dropped from the node's own
        // text — authored copy, silently lost.
        assert_eq!(
            strip_tags("<p data-spool-id=\"x\">Hello<br>World</p>"),
            "Hello World"
        );
        assert_eq!(
            strip_tags("<p>Hello<img src=\"a.png\" />World</p>"),
            "Hello World"
        );
        assert_eq!(
            strip_tags("<p>A<span>B</span>C</p>"),
            "AC",
            "a real nested element still hides its own text, and nothing separates \
             the two runs around it"
        );
    }

    #[test]
    fn later_stylesheets_win_across_files_the_way_the_document_links_them() {
        // Stylesheets were ordered by filename, so the cascade across files
        // depended on names the author never thought about: a page that links
        // `zz.css` then `aa.css` was resolved as if it had loaded them the other
        // way round.
        let root =
            std::env::temp_dir().join(format!("spool-visual-link-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create project");
        std::fs::write(
            root.join("lamine.yaml"),
            format!(
                "version: 1\nnodes:\n{}",
                node_yaml("spool-a", "A", "frame", None, "")
            ),
        )
        .expect("yaml");
        std::fs::write(
            root.join("index.html"),
            "<!doctype html>\n<html>\n<head>\n  <link rel=\"stylesheet\" href=\"zz.css\" />\n  \
             <link rel=\"stylesheet\" href=\"aa.css\" />\n</head>\n<body>\n  \
             <div data-spool-id=\"spool-a\">A</div>\n</body>\n</html>\n",
        )
        .expect("html");
        std::fs::write(root.join("zz.css"), "div { background-color: #ff0000; }").expect("zz");
        std::fs::write(root.join("aa.css"), "div { background-color: #0000ff; }").expect("aa");

        assert_eq!(
            open(&root).get(&node("spool-a")).expect("node").style.fill,
            Some(Fill {
                color: Color::from_rgb(0x0000ff)
            }),
            "the sheet linked last is the one that wins, exactly as in a browser"
        );
    }

    #[test]
    fn an_inline_background_shorthand_still_outranks_the_stylesheet() {
        let root = project(
            "inline-shorthand",
            &node_yaml("spool-a", "A", "frame", None, ""),
            "<!doctype html>\n<html>\n<head>\n  <link rel=\"stylesheet\" href=\"styles.css\" />\n</head>\n<body>\n  <div data-spool-id=\"spool-a\" style=\"background: #00ff00\">A</div>\n</body>\n</html>\n",
            Some("div { background-color: #ff0000; }"),
        );
        assert_eq!(
            open(&root).get(&node("spool-a")).expect("node").style.fill,
            Some(Fill {
                color: Color::from_rgb(0x00ff00)
            }),
            "the shorthand is read as the longhand it stands for, not as a separate property"
        );
    }

    #[test]
    fn a_href_resolves_against_the_page_that_links_it() {
        // An href is relative to the linking document; a source key is relative
        // to the project root. Getting this wrong silently drops every
        // stylesheet a page in a subdirectory links.
        assert_eq!(
            source_key_for("index.html", "styles.css").as_deref(),
            Some("styles.css")
        );
        assert_eq!(
            source_key_for("pages/index.html", "../styles/site.css").as_deref(),
            Some("styles/site.css")
        );
        assert_eq!(
            source_key_for("pages/deep/index.html", "../../base.css").as_deref(),
            Some("base.css")
        );
        assert_eq!(
            source_key_for("index.html", "../outside.css"),
            None,
            "a href that climbs out of the project is refused, not followed"
        );
    }

    #[test]
    fn an_elements_own_text_and_its_children_stack_rather_than_being_summed_into_one_line() {
        // There is no inline layout, so a parent's own text is one line placed at
        // the top of its content box and its children follow it. Recorded here
        // because it is the one place the model approximates, and the shape of
        // the approximation should be asserted rather than discovered.
        let root = project(
            "text-and-children",
            &format!(
                "{}{}",
                node_yaml("spool-p", "P", "frame", None, "\"spool-a\""),
                node_yaml("spool-a", "A", "text", Some("spool-p"), "")
            ),
            "<!doctype html>\n<body>\n  <div data-spool-id=\"spool-p\">Before<p data-spool-id=\"spool-a\">A</p></div>\n</body>\n",
            None,
        );
        let visuals = open(&root);
        assert_eq!(
            visuals
                .get(&node("spool-p"))
                .expect("parent")
                .geometry
                .height,
            22.4 * 2.0,
            "the line and the child each get a line's height"
        );
        assert_eq!(
            visuals.get(&node("spool-a")).expect("child").geometry.y,
            22.4,
            "and the child starts below the text"
        );
    }
}
