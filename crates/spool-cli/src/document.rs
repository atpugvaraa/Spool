//! Loading and saving documents, and the sessions the commands run in.
//!
//! **`.spool` is not supported here, and this module says so instead of pretending.** A Spool
//! project is a *directory* holding `lamine.yaml` plus authored HTML/CSS/SVG; `figcraft_format`
//! reads and writes single files (`.figcraft`, `.json`), and this crate may not depend on
//! `app`/`spool-source`, which is where Spool's project boundary lives. So `open`, `save --in`,
//! `run --in`, `inspect --in` and `export` refuse a `.spool` path with [`SPOOL_UNSUPPORTED`].

use std::path::Path;

use figcraft_doc::{Document, NodeId};
use figcraft_engine::{DocState, Session};
use serde_json::{json, Value};

use crate::CliError;
use crate::CliResult;

/// Why a `.spool` path cannot be used, and what to use instead.
pub const SPOOL_UNSUPPORTED: &str = "\
a `.spool` project is a DIRECTORY (lamine.yaml plus authored HTML/CSS/SVG), not a document file; \
spool-cli drives the vendored figcraft engine, whose files are `.figcraft` (or `.json`). \
Opening and saving Spool projects is NOT implemented: this crate cannot depend on `app` or \
`spool-source`, so there is no project loader behind it yet.";

/// Refuse a `.spool` project (or any directory) before the format layer sees it.
pub fn guard_project(path: &Path, what: &str) -> CliResult<()> {
    let is_spool = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".spool"))
        || path.is_dir();
    if is_spool {
        return Err(CliError::Failed(format!(
            "{what} `{}`: this project format is not supported here. {SPOOL_UNSUPPORTED}",
            path.display()
        )));
    }
    Ok(())
}

/// Read a document: the native format first (`.figcraft`, `.json`), then the engine's importers
/// (`.svg`, images, `.fig`). Returns the loader's notes (repairs of a damaged file, import
/// warnings) for the caller to report on stderr.
pub fn load(path: &str) -> CliResult<(Document, Vec<String>)> {
    let p = Path::new(path);
    guard_project(p, "can't open")?;
    match figcraft_format::load_file(p) {
        Ok((doc, notes)) => Ok((doc, notes)),
        Err(native) => {
            // Not a native file: an import (SVG, an image layer, best-effort `.fig`).
            let bytes = std::fs::read(p)
                .map_err(|e| CliError::Failed(format!("can't open {path}: {e}")))?;
            figcraft_engine::cmd::file::load_bytes(&bytes, path).map_err(|import| {
                CliError::Failed(format!(
                    "can't open {path}: not a native file ({native}) and not an import ({import})"
                ))
            })
        }
    }
}

/// A session holding `path`'s document, or a session with one new empty document. The second value
/// is the loader's notes.
pub fn session(path: Option<&str>) -> CliResult<(Session, Vec<String>)> {
    let Some(path) = path.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok((Session::new(), Vec::new()));
    };
    let (doc, notes) = load(path)?;
    let mut session = Session::empty();
    session
        .docs
        .push(DocState::new(doc, Some(path.to_string())));
    Ok((session, notes))
}

/// Write `doc` to `path`: `.figcraft` natively (with its thumbnail), `.json` as the document JSON.
/// Anything else is a usage error — `export` writes images, SVG and PDF.
pub fn save_document(doc: &Document, path: &str) -> CliResult<Value> {
    let p = Path::new(path);
    guard_project(p, "can't save")?;
    let ext = extension(path);
    match ext.as_str() {
        "figcraft" => {
            let thumbnail = figcraft_engine::cmd::file::thumbnail(doc);
            figcraft_format::save_file(doc, p, thumbnail.as_deref())
                .map_err(|e| CliError::Failed(format!("can't save {path}: {e}")))?;
        }
        "json" => {
            let value = figcraft_format::to_json(doc)
                .map_err(|e| CliError::Failed(format!("can't save {path}: {e}")))?;
            let bytes = serde_json::to_vec_pretty(&value)
                .map_err(|e| CliError::Failed(format!("can't save {path}: {e}")))?;
            write_atomic(p, &bytes)?;
        }
        other => {
            return Err(CliError::Usage(format!(
                "save writes `.figcraft` or `.json`, not `.{other}` (use `export` for png, jpg, webp, svg and pdf)"
            )));
        }
    }
    Ok(json!({
        "path": path,
        "format": ext,
        "bytes": size_of(path),
    }))
}

/// Write the active document of `session` to `path`. A `.figcraft` file keeps the document's
/// version history and branches (through the engine); `.json` is the plain document.
pub fn save_session(session: &Session, path: &str) -> CliResult<Value> {
    let p = Path::new(path);
    guard_project(p, "can't save")?;
    let ds = session.ds().map_err(|e| CliError::Failed(e.to_string()))?;
    if extension(path) != "figcraft" {
        // `.json`, and anything else `save_document` rejects with a usage error.
        return save_document(&ds.doc, path);
    }
    let bytes = figcraft_engine::cmd::file::file_bytes(ds)
        .map_err(|e| CliError::Failed(format!("can't save {path}: {e}")))?;
    write_atomic(p, &bytes)?;
    Ok(json!({"path": path, "format": "figcraft", "bytes": bytes.len()}))
}

/// `figcraft_format`'s atomic write: a staging file next to `path`, then one rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> CliResult<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| CliError::Failed(format!("{}: {e}", dir.display())))?;
    }
    figcraft_format::write_file_atomic(path, bytes)
        .map_err(|e| CliError::Failed(format!("{}: {e}", path.display())))
}

fn size_of(path: &str) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// The lowercased extension, without the dot.
pub fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// A page by name, or the first page that isn't a divider.
pub fn page_named(doc: &Document, name: Option<&str>) -> CliResult<NodeId> {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        None => doc
            .first_open_page()
            .or_else(|| doc.first_page())
            .ok_or_else(|| CliError::Failed("the document has no pages".into())),
        Some(n) => doc
            .pages
            .iter()
            .copied()
            .find(|p| doc.node(*p).is_some_and(|pn| pn.name == n))
            .or_else(|| {
                doc.pages.iter().copied().find(|p| {
                    doc.node(*p)
                        .is_some_and(|pn| pn.name.eq_ignore_ascii_case(n))
                })
            })
            .ok_or_else(|| {
                let names: Vec<String> = doc
                    .pages
                    .iter()
                    .filter_map(|p| doc.node(*p).map(|n| n.name.clone()))
                    .collect();
                CliError::Failed(format!("no page named `{n}` (pages: {})", names.join(", ")))
            }),
    }
}

/// A layer by node id or name (an exact name first, then a prefix match, then anywhere).
pub fn find_node(doc: &Document, page: NodeId, query: &str) -> CliResult<NodeId> {
    if let Ok(id) = query.parse::<NodeId>() {
        if doc.contains(id) {
            return Ok(id);
        }
    }
    let exact = |ids: Vec<NodeId>| {
        ids.into_iter()
            .find(|i| doc.node(*i).is_some_and(|n| n.name == query))
    };
    exact(doc.find_by_name(query, Some(page)))
        .or_else(|| exact(doc.find_by_name(query, None)))
        .or_else(|| doc.find_by_name(query, Some(page)).into_iter().next())
        .ok_or_else(|| CliError::Failed(format!("no layer with id or name `{query}`")))
}

/// What `open` prints: the document's shape, not its contents (`inspect` is for that).
pub fn summary(doc: &Document, notes: &[String], path: &str) -> Value {
    let pages: Vec<Value> = doc
        .pages
        .iter()
        .filter_map(|p| {
            let node = doc.node(*p)?;
            let frames: Vec<Value> = node
                .children
                .iter()
                .filter_map(|c| doc.node(*c))
                .filter(|n| n.kind.is_frame_like())
                .map(|n| json!({"id": n.id, "name": n.name, "type": n.kind.type_name(), "width": n.width, "height": n.height}))
                .collect();
            Some(json!({
                "id": p,
                "name": node.name,
                "nodes": doc.subtree(*p).len().saturating_sub(1),
                "topLevel": node.children.len(),
                "frames": frames,
            }))
        })
        .collect();
    json!({
        "path": path,
        "name": doc.name,
        "nodes": doc.node_count(),
        "styles": doc.styles.len(),
        "variables": doc.variables.variables.len(),
        "images": doc.images.len(),
        "currentPage": doc.current_page,
        "pages": pages,
        "notes": notes,
        "spoolProjectSupported": false,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_spool_directory_is_refused_before_the_format_layer() {
        let e = guard_project(Path::new("/tmp/Some.spool"), "can't open").unwrap_err();
        assert!(
            matches!(e, CliError::Failed(_)),
            "a project path is a failure, not a usage error"
        );
        assert!(e.to_string().contains("NOT implemented"));
        // Any directory, whatever it is called.
        assert!(guard_project(Path::new("/tmp"), "can't open").is_err());
        assert!(guard_project(Path::new("/tmp/file.figcraft"), "can't open").is_ok());
    }

    #[test]
    fn extensions_are_lower_cased_without_the_dot() {
        assert_eq!(extension("a/b/c.FIGCRAFT"), "figcraft");
        assert_eq!(extension("noext"), "");
    }

    #[test]
    fn page_and_layer_lookup_report_what_is_available() {
        let mut doc = Document::new("test");
        let page = doc.first_page().unwrap();
        doc.create(
            page,
            figcraft_doc::NodeKind::Frame,
            "Hero",
            0.0,
            0.0,
            10.0,
            10.0,
        )
        .unwrap();
        assert_eq!(page_named(&doc, None).unwrap(), page);
        assert_eq!(page_named(&doc, Some("Page 1")).unwrap(), page);
        assert!(page_named(&doc, Some("Nope")).is_err());
        let id = doc
            .create(
                page,
                figcraft_doc::NodeKind::Rectangle,
                "Card",
                0.0,
                0.0,
                4.0,
                4.0,
            )
            .unwrap();
        assert_eq!(find_node(&doc, page, "Card").unwrap(), id);
        assert_eq!(find_node(&doc, page, &id.to_string()).unwrap(), id);
        assert!(find_node(&doc, page, "missing").is_err());
    }

    #[test]
    fn the_summary_reports_pages_frames_and_notes() {
        let mut doc = Document::new("test");
        let page = doc.first_page().unwrap();
        doc.create(
            page,
            figcraft_doc::NodeKind::Frame,
            "Hero",
            0.0,
            0.0,
            10.0,
            10.0,
        )
        .unwrap();
        let v = summary(&doc, &["repaired".to_string()], "in.figcraft");
        assert_eq!(v["path"], "in.figcraft");
        assert_eq!(v["pages"][0]["frames"][0]["name"], "Hero");
        assert_eq!(v["notes"][0], "repaired");
        assert_eq!(v["spoolProjectSupported"], false);
    }
}
