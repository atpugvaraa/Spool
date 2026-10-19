//! The cascade, as the writer and the renderer both see it.
//!
//! # Why this module exists
//!
//! A project has more than one stylesheet, and a declaration in one of them
//! may or may not be the one that decides what an element looks like. Deciding
//! that is a *ranking*, and a ranking that exists twice is two answers to one
//! question:
//!
//! - the renderer resolves the value an element is drawn with, and
//! - the writer has to find the authored span that produced it, so a save
//!   rewrites the declaration the user was actually looking at.
//!
//! If the two disagreed, a save would rewrite a declaration that had already
//! lost, and the file on disk would end up naming a different owner for the
//! value than the editor had been drawing. Nothing about the result would look
//! wrong.
//!
//! So the ranking lives in one place, [`spool_source::css::Cascade`], the
//! crate that owns the `lamine.yaml` schema and the source-to-model compile.
//! The reader that compiles the project uses it, and so does this module, which
//! asks it the one question a writer has.
//!
//! # Which reader
//!
//! The ranking is computed over [`spool_source::css::Stylesheet`] — the reader
//! the *engine* compiles the project with. That is the whole point: the engine
//! draws the value the writer rewrites, so both come out of one cascade rather
//! than two that have to agree.

use std::collections::HashMap;

use spool_source::css::{Cascade, Stylesheet};

/// The stylesheets a project owns, in cascade order.
///
/// Public because save needs the same order. It used to sort names
/// alphabetically while the renderer walked link order, so a project whose
/// alphabetical order differed from its link order could have a save rewrite a
/// declaration that did not control the rendered value. One traversal, so the
/// two cannot disagree about which file won.
///
/// Returns names only. Takes the source map rather than the document because
/// the two callers that share this order do not have the same document: the
/// renderer parses each sheet from the document's sources, while save must
/// parse from the bytes on disk. Sharing the *order* is the point, and sharing
/// the bytes would be wrong.
pub fn stylesheet_order(sources: &HashMap<String, String>) -> Vec<String> {
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

/// The authored tag, class list, and id of one element fragment.
///
/// Shared deliberately: the save layer needs the same element identity the
/// cascade matches on, and two readers of an element's identity would be two
/// chances to disagree about who owns a declaration.
///
/// All three, because the cascade needs all three. The id was previously
/// dropped here and read from a private twin instead, which is how the editor
/// ended up honouring a `#hero { … }` rule that save concluded did not exist
/// and overrode with an inline declaration. One function returning everything
/// the cascade matches on means a caller cannot forget the id by accident: the
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

/// Which of `sheets` the cascade says owns `property` for this element.
///
/// The index is into `sheets` as given, so a caller that already has the sheets
/// in link order can name the file without re-deriving the order.
///
/// This is the cascade asked the one question a writer has. It exists so that
/// deciding *which* declaration owns a value and deciding *what value* it
/// resolves to are the same traversal: the renderer takes the values out of the
/// same merge and a source-preserving edit takes the owning sheet out of here,
/// so the span an edit rewrites and the fill the user is looking at cannot come
/// from different rules.
///
/// `property` is a computed longhand — the same key
/// [`Stylesheet::resolve_ranked`] reports under — because that is what both
/// sides of the cascade compare on.
pub fn owning_sheet(
    sheets: &[&Stylesheet],
    tag: &str,
    classes: &[String],
    id: Option<&str>,
    property: &str,
) -> Option<usize> {
    let mut cascade = Cascade::default();
    for (index, sheet) in sheets.iter().enumerate() {
        for (property, resolved) in sheet.resolve_ranked(tag, classes, id) {
            cascade.consider(&property, index, &resolved);
        }
    }
    cascade.sheet_of(property)
}
