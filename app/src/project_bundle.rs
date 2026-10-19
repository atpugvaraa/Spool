//! Project bundle persistence: the lifecycle around a directory of authored
//! source plus its Spool metadata.
//!
//! A bundle pairs `lamine.yaml` (Spool structural metadata) with the
//! HTML/CSS/SVG files the metadata binds to. This module owns exactly two
//! transitions:
//!
//! ```text
//!   project on disk -> lamine.yaml -> resolve references -> validate
//!                   -> PersistentDocument
//!   PersistentDocument -> lamine.yaml -> disk
//! ```
//!
//! Two rules constrain everything here:
//!
//! 1. **Authored source is canonical.** Loading reads source; it never
//!    parses, normalizes, or rewrites it. Saving writes `lamine.yaml` and
//!    nothing else. There is deliberately no code path in this module that
//!    emits HTML, CSS, or SVG, so "source bytes survive a metadata-only
//!    save" holds by absence of capability rather than by a check that a
//!    later edit could weaken.
//! 2. **Ambiguity is reported, never guessed.** A binding that resolves to
//!    zero or to more than one authored element is an error. So is a
//!    reference that escapes the bundle root.
//!
//! This module does not define the project's directory layout. `lamine.yaml`
//! is located at the bundle root; the `file` of each source binding is
//! resolved relative to that root, so a project may use `pages/`, `styles/`,
//! `assets/`, or anything else without this module being changed.
//!
//! # The `sources` key namespace
//!
//! [`PersistentDocument::sources`] is keyed by a project-root-relative path,
//! spelled with `/` separators: the same namespace metadata's `file` field
//! uses, and the same one a later write resolves against. A stylesheet reached
//! through a `<link>` is therefore keyed by where it *landed*
//! (`styles/site.css`), never by the href that found it
//! (`../styles/site.css`). Keeping one namespace is what lets a save address a
//! file without knowing whether metadata or markup discovered it.
//!
//! # Two metadata formats, and why the load is not the save
//!
//! `lamine.yaml` has two formats. Version 1 is the six-fields-per-node contract
//! the editor has always spoken, and [`LamineStructure::from_yaml`] is its
//! reader. Version 2 is defined by `spool-source` and is where Owner B lives —
//! constraints, sizing, masks, locks, dev status, grids, guides, exports,
//! interactions, components, variable bindings, plugin data.
//!
//! [`ProjectBundle::load`] reads version 1 with the editor's own reader first, so
//! a project that has always opened keeps going through exactly the code it
//! always went through. Only a file that reader rejects *for not being its
//! format* is handed to [`spool_source::Manifest`], the crate that defines the
//! schema, and projected onto the editor's structure by
//! [`crate::engine_project::structure_from_manifest`]. A version 1 file with a
//! fault in it stays refused with the reason that reader gave. The Owner B fields
//! are not in the projection and were never going to be: they stay in the bytes,
//! which the bundle keeps as [`ProjectBundle::manifest_yaml`] for
//! [`crate::engine_project::compile_with_metadata`] to read.
//!
//! [`ProjectBundle::load_structure`], by contrast, stays on the strict version 1
//! reader on purpose. It is what a save compares the editor's document against,
//! and a version 2 project failing there is what stops a save from writing a
//! version 1 file over it and losing every Owner B value it carried. Making the
//! save understand version 2 is real work — it has to merge structural edits into
//! the manifest rather than re-encode it — and belongs in its own change.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::source_document::{HtmlSource, LamineStructure, ModelError, NodeId, PersistentDocument};

/// The one file Spool owns inside a project directory.
pub const METADATA_FILE: &str = "lamine.yaml";

/// A failure while loading or saving a project bundle.
///
/// Each variant names the specific file or reference at fault so a caller can
/// act on it without re-reading the directory.
#[derive(Debug)]
pub enum BundleError {
    /// No `lamine.yaml` at the bundle root.
    MissingMetadata { root: PathBuf },
    /// `lamine.yaml` was read but is not valid metadata in either format.
    ///
    /// Carries the app's [`ModelError`] whether the file was refused by the
    /// version 1 reader or by `spool-source`'s; a refusal from the second is
    /// carried as its own wording inside `InvalidYaml`, so the reason a file was
    /// turned away reaches the caller rather than being replaced by a generic one.
    MalformedMetadata { path: PathBuf, source: ModelError },
    /// A source binding named a file that is not present in the bundle.
    MissingSource { file: String, referenced_by: NodeId },
    /// A source binding pointed outside the bundle root or was otherwise not
    /// a usable in-bundle reference.
    InvalidSourceReference { file: String, referenced_by: NodeId },
    /// A source binding did not resolve to exactly one authored element.
    UnresolvedBinding {
        id: NodeId,
        file: String,
        selector: String,
        matches: usize,
    },
    /// The document about to be written would not bind against the source about
    /// to be written.
    ///
    /// Reported instead of written because the alternative is a project on disk
    /// that no longer opens — the one failure a source-backed editor cannot
    /// repair for the user.
    UnsavableStructure { path: PathBuf, detail: String },
    /// Reading or writing failed. Metadata replacement is failure-safe, so
    /// this never leaves a partially written `lamine.yaml` behind.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMetadata { root } => {
                write!(f, "no {METADATA_FILE} found in project root {}", root.display())
            }
            Self::MalformedMetadata { path, source } => {
                write!(f, "{} is not valid project metadata: {source}", path.display())
            }
            Self::MissingSource {
                file,
                referenced_by,
            } => write!(
                f,
                "node {} binds to {file}, which is missing from the project",
                referenced_by.as_str()
            ),
            Self::InvalidSourceReference {
                file,
                referenced_by,
            } => write!(
                f,
                "node {} binds to {file:?}, which is not a valid in-project reference",
                referenced_by.as_str()
            ),
            Self::UnresolvedBinding {
                id,
                file,
                selector,
                matches,
            } => write!(
                f,
                "node {} binds to {selector} in {file}, which matched {matches} elements (expected exactly 1)",
                id.as_str()
            ),
            Self::UnsavableStructure { path, detail } => write!(
                f,
                "{} would not describe a project that can be opened: {detail}",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(f, "{} could not be accessed: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for BundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MalformedMetadata { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A loaded project bundle: validated metadata plus the exact authored bytes
/// each node binds to.
pub struct ProjectBundle {
    root: PathBuf,
    pub document: PersistentDocument,
    /// `lamine.yaml` exactly as it was read, when this bundle read it.
    ///
    /// The bytes, not a re-parse and not a re-read of the file: the file could
    /// have changed on disk since, and "what did this project say when it was
    /// opened" has exactly one answer. `None` for a bundle built by
    /// [`ProjectBundle::from_document`], which wraps a document that has no file
    /// behind it.
    manifest_yaml: Option<String>,
}

impl ProjectBundle {
    /// Locate `lamine.yaml` for a project root without reading it.
    ///
    /// Returns `MissingMetadata` when the root holds no metadata file. This is
    /// the only place the metadata filename is decided.
    pub fn locate_metadata(root: impl AsRef<Path>) -> Result<PathBuf, BundleError> {
        let root = root.as_ref();
        let path = root.join(METADATA_FILE);
        if path.is_file() {
            Ok(path)
        } else {
            Err(BundleError::MissingMetadata {
                root: root.to_path_buf(),
            })
        }
    }

    /// Load a bundle from a project root.
    ///
    /// Reads and validates `lamine.yaml`, resolves every source binding to a
    /// file inside the bundle, and confirms each binding resolves to exactly
    /// one authored element. Returns an error rather than a partially
    /// resolved document, so a caller never observes a bundle it did not
    /// validate.
    pub fn load(root: impl AsRef<Path>) -> Result<Self, BundleError> {
        let root = root.as_ref().to_path_buf();
        let metadata_path = Self::locate_metadata(&root)?;
        let metadata = read(&metadata_path)?;
        let structure = parse_structure(&metadata_path, &metadata)?;

        // Read each bound file once, in deterministic order. A reference that
        // escapes the root is rejected before any file is opened.
        let mut authored: BTreeMap<String, String> = BTreeMap::new();
        // One file, one key. Two metadata spellings of the same path (`a.css`
        // and `./a.css`) must not become two copies of one stylesheet, or a
        // later write would not know which key it owns.
        let mut loaded: BTreeMap<PathBuf, String> = BTreeMap::new();
        for node in &structure.nodes {
            if authored.contains_key(&node.source.file) {
                continue;
            }
            let path = resolve_within_root(&root, &node.source.file).ok_or_else(|| {
                BundleError::InvalidSourceReference {
                    file: node.source.file.clone(),
                    referenced_by: node.id.clone(),
                }
            })?;
            // A missing file is the common case and deserves its own
            // actionable variant rather than a generic IO error.
            let contents = match std::fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    return Err(BundleError::MissingSource {
                        file: node.source.file.clone(),
                        referenced_by: node.id.clone(),
                    });
                }
                Err(source) => return Err(BundleError::Io { path, source }),
            };
            loaded.insert(path, node.source.file.clone());
            authored.insert(node.source.file.clone(), contents);
        }

        // Stylesheets the HTML links are part of the authored project and are
        // loaded alongside it. Without them the runtime cannot know how the
        // source is meant to look, and would fall back to invented defaults.
        // The link is followed the way a browser would: out of the document
        // that wrote it, into the project, never above the root.
        //
        // Collected before reading, because the references come from the
        // documents already loaded and inserting into that map while reading
        // it would not be a sequential pass.
        let linked: Vec<(String, PathBuf)> = authored
            .iter()
            .flat_map(|(file, contents)| {
                linked_stylesheets(contents)
                    .into_iter()
                    .filter_map(|href| resolve_document_reference(&root, file, &href))
            })
            .collect();
        for (key, path) in linked {
            if authored.contains_key(&key) || loaded.contains_key(&path) {
                continue;
            }
            // A stylesheet that is not there is left unstyled rather than
            // resolved against an unexpected path, and a stylesheet that
            // cannot be read is reported rather than silently styled without.
            match std::fs::read_to_string(&path) {
                Ok(contents) => {
                    loaded.insert(path, key.clone());
                    authored.insert(key, contents);
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(BundleError::Io { path, source });
                }
            }
        }

        // Every node must bind to exactly one authored element. Zero is a
        // dangling binding; more than one is ambiguous. Neither is guessed.
        for node in &structure.nodes {
            let source = HtmlSource {
                file: node.source.file.clone(),
                contents: authored[&node.source.file].clone(),
            };
            let matches = source.binding_occurrences(node);
            if matches != 1 {
                return Err(BundleError::UnresolvedBinding {
                    id: node.id.clone(),
                    file: node.source.file.clone(),
                    selector: node.source.selector.clone(),
                    matches,
                });
            }
        }

        Ok(Self {
            root,
            document: PersistentDocument {
                structure,
                sources: authored.into_iter().collect(),
            },
            manifest_yaml: Some(metadata),
        })
    }

    /// The `lamine.yaml` text this bundle was loaded from, if it read one.
    ///
    /// Reaches the engine compile as the authoritative answer to "what does this
    /// project's metadata say", which is what a version 2 file needs: the
    /// document beside it cannot express Owner B, so a compile from the document
    /// alone would quietly produce a project with no constraints, no locks and no
    /// plugin data. See [`crate::engine_project::compile_with_metadata`].
    pub fn manifest_yaml(&self) -> Option<&str> {
        self.manifest_yaml.as_deref()
    }

    /// Read only `lamine.yaml`, as it currently stands on disk.
    ///
    /// A save needs to know whether the structure it holds differs from the one
    /// on disk, and answering that must not re-read and re-validate every
    /// authored file: by the time a save runs, its edits are already resolved,
    /// and a full reload could fail for a reason that has nothing to do with
    /// the metadata being written. Nothing here touches authored source.
    ///
    /// Deliberately strict: this is the version 1 reader, and it is the reason a
    /// save of a version 2 project stops rather than overwriting the file with a
    /// version 1 encoding that would drop every Owner B value in it. See the
    /// module docs.
    pub fn load_structure(root: impl AsRef<Path>) -> Result<LamineStructure, BundleError> {
        let root = root.as_ref();
        let metadata_path = Self::locate_metadata(root)?;
        let metadata = read(&metadata_path)?;
        LamineStructure::from_yaml(&metadata).map_err(|source| BundleError::MalformedMetadata {
            path: metadata_path,
            source,
        })
    }

    /// Wrap an already-loaded document as a bundle rooted at `root`.
    ///
    /// Exists so metadata is written through exactly one writer. A save that
    /// has renamed a node in the editor must persist the editor's document, not
    /// whatever was on disk when the project was opened, and it must still go
    /// through [`ProjectBundle::save`] so validation and the atomic replace are
    /// not re-implemented.
    pub fn from_document(
        root: impl AsRef<Path>,
        document: PersistentDocument,
    ) -> Result<Self, BundleError> {
        Ok(Self {
            root: root.as_ref().to_path_buf(),
            document,
            // No file was read, so there are no bytes to claim about one. A
            // caller that has the file should use [`ProjectBundle::load`].
            manifest_yaml: None,
        })
    }

    /// The project root this bundle was loaded from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Serialize the metadata exactly as it would be written to disk.
    ///
    /// Deterministic: the same document always produces the same bytes, which
    /// is what makes metadata-only diffs meaningful. Validates first, so this
    /// fails rather than emitting metadata that could not be reloaded.
    pub fn metadata_yaml(&self) -> Result<String, BundleError> {
        self.document
            .structure
            .to_yaml()
            .map_err(|source| BundleError::MalformedMetadata {
                path: self.root.join(METADATA_FILE),
                source,
            })
    }

    /// Persist metadata only, replacing `lamine.yaml` failure-safely.
    ///
    /// Writes to a temporary file in the same directory and renames it over
    /// the target, so a failure part-way through leaves the previous
    /// `lamine.yaml` intact rather than a truncated one. Authored files are
    /// never written, so HTML/CSS/SVG keep their exact bytes and any files
    /// Spool does not understand are left untouched.
    pub fn save(&self) -> Result<(), BundleError> {
        let encoded = self.metadata_yaml()?;
        let target = self.root.join(METADATA_FILE);
        write_atomically(&target, encoded.as_bytes())
    }
}

/// Read `lamine.yaml` into the structure the editor speaks.
///
/// The editor's own reader goes first, and a file it accepts is finished: a
/// project that has always opened must keep going through exactly the code it
/// always went through, byte for byte. That is also what keeps a version 1 file
/// from being re-encoded on load, which would reformat a file the author wrote
/// and throw away every comment in it.
///
/// A refusal only moves the question to [`spool_source::Manifest::from_yaml`],
/// which understands both versions, when it is a refusal about the *format* —
/// see [`is_another_format`]. A version 1 file with a fault in it stays refused,
/// with the reason the editor's own reader gave, because that reader is the
/// authority on version 1 and looking for the same mistake in a second reader
/// would only find it stated more loosely.
fn parse_structure(path: &Path, metadata: &str) -> Result<LamineStructure, BundleError> {
    let malformed = |source: ModelError| BundleError::MalformedMetadata {
        path: path.to_path_buf(),
        source,
    };
    let version_one = match LamineStructure::from_yaml(metadata) {
        Ok(structure) => return Ok(structure),
        Err(error) if !is_another_format(&error) => return Err(malformed(error)),
        Err(error) => error,
    };
    match spool_source::manifest::Manifest::from_yaml(metadata) {
        Ok(manifest) => {
            crate::engine_project::structure_from_manifest(&manifest).map_err(malformed)
        }
        // Both readers turned it away, for the same reason: the file is not
        // either format's. Answering with the version 1 reader's reason alone is
        // what this file would have got before, so there is nothing to add.
        Err(version_two) if version_two.to_string() == version_one.to_string() => {
            Err(malformed(version_one))
        }
        // The format-aware reader has something specific to say — an unsupported
        // version, a field that does not exist — and that is the reason an author
        // needs. The version 1 reader's "expected version: 1" is not: they have
        // already been told, and repeating it buries the useful half.
        Err(version_two) => Err(malformed(ModelError::InvalidYaml(version_two.to_string()))),
    }
}

/// Whether the editor's reader refused the file for not being its format.
///
/// The version 1 reader checks `version: 1` and `nodes:` before it looks at a
/// single node, and this is the one refusal that means "not a version 1 file"
/// rather than "a mistake inside one". Every other refusal names a fault in a
/// file that *is* version 1 — a duplicate id, a dangling child, an empty binding
/// — and those are reported unchanged. Going to the schema's reader for those
/// would be worse, not better: its version 1 upgrade resolves hierarchy by
/// silently discarding references it does not recognise, so a dangling child
/// would stop being an error and become a layer quietly gone.
fn is_another_format(error: &ModelError) -> bool {
    matches!(error, ModelError::InvalidYaml(detail) if detail.contains("expected version: 1"))
}

/// Stylesheet paths an HTML document links, relative to that document.
///
/// Deliberately simple: `<link ... rel~="stylesheet" href=...>`, in document
/// order. Fragment-only and absolute URLs are ignored.
fn linked_stylesheets(html: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("<link") {
        rest = &rest[at..];
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[..end];
        if tag.contains("stylesheet") {
            for href in quoted_attribute(tag, "href") {
                let href = href.trim();
                if !href.is_empty()
                    && !href.starts_with('#')
                    && !href.contains("://")
                    && !found.contains(&href.to_owned())
                {
                    found.push(href.to_owned());
                }
            }
        }
        rest = &rest[end..];
    }
    found
}

fn quoted_attribute(tag: &str, name: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = tag;
    while let Some(at) = rest.find(name) {
        rest = &rest[at + name.len()..];
        let Some(equals) = rest.find('=') else {
            continue;
        };
        let after = rest[equals + 1..].trim_start();
        let bytes = after.as_bytes();
        if bytes.is_empty() {
            break;
        }
        let quote = bytes[0] as char;
        if quote == '"' || quote == '\'' {
            if let Some(end) = after[1..].find(quote) {
                values.push(after[1..1 + end].to_owned());
                rest = &after[end + 2..];
                continue;
            }
            break;
        }
        let end = after
            .find(|c: char| c.is_whitespace() || c == '>')
            .unwrap_or(after.len());
        values.push(after[..end].to_owned());
        rest = &after[end..];
    }
    values
}

/// Resolve a reference a document makes to another file in the project.
///
/// The rule is the browser's, not the bundle root's: a relative reference
/// resolves against the directory of the document that wrote it. A document at
/// `pages/index.html` that links `../styles/site.css` therefore reaches
/// `styles/site.css` — the layout a real site uses, and the one this loader
/// used to miss entirely because it measured the href from the root. Climbing
/// above the root is still refused, so "in this project" survives.
///
/// Returns the project-root-relative key the file is stored under, so callers
/// never have to reconstruct one from an href.
fn resolve_document_reference(
    root: &Path,
    document: &str,
    reference: &str,
) -> Option<(String, PathBuf)> {
    let reference = Path::new(reference);
    if reference.is_absolute() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let push_normal = |component: &Component, parts: &mut Vec<String>| -> bool {
        match component {
            Component::Normal(part) => match part.to_str() {
                Some(part) => {
                    parts.push(part.to_owned());
                    true
                }
                None => false,
            },
            Component::CurDir => true,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => false,
        }
    };
    for component in Path::new(document)
        .parent()
        .unwrap_or(Path::new(""))
        .components()
    {
        if !push_normal(&component, &mut parts) {
            return None;
        }
    }
    // `..` is how a reference climbs, which is why the root is checked as a
    // floor rather than banned: `pages/../styles/site.css` is a legal way to
    // name `styles/site.css`, and `../outside.css` is refused because it would
    // have to climb above an empty stack.
    for component in reference.components() {
        match component {
            Component::Normal(part) => match part.to_str() {
                Some(part) => parts.push(part.to_owned()),
                None => return None,
            },
            Component::CurDir => {}
            Component::ParentDir => {
                // Climbing above an empty stack would escape the root.
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    let key = parts.join("/");
    Some((key.clone(), root.join(Path::new(&key))))
}

/// Resolve `reference` inside `root`, rejecting anything that escapes it.
///
/// Absolute paths and `..` traversal are rejected: a binding names a file in
/// this project, not anywhere on the filesystem.
pub(crate) fn resolve_within_root(root: &Path, reference: &str) -> Option<PathBuf> {
    let relative = Path::new(reference);
    if relative.is_absolute() {
        return None;
    }
    let mut resolved = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir => {}
            // Escapes the root, or is a root/prefix we already rejected.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(resolved)
}

fn read(path: &Path) -> Result<String, BundleError> {
    std::fs::read_to_string(path).map_err(|source| BundleError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Replace `target` with `bytes` without ever exposing a partial write.
///
/// The temporary name carries the process id *and* a counter, so two saves in
/// one process — which is the normal shape of an editor, not an exotic one —
/// cannot pick the same scratch file and write through each other.
pub(crate) fn write_atomically(target: &Path, bytes: &[u8]) -> Result<(), BundleError> {
    use std::io::Write;

    static NEXT_TEMP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let sequence = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let directory = target.parent().unwrap_or_else(|| Path::new("."));
    // A sibling temp file keeps the rename on one filesystem, which is what
    // makes the replacement atomic.
    let temporary = directory.join(format!(
        ".{}.tmp-{}-{}",
        target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| METADATA_FILE.into()),
        std::process::id(),
        sequence
    ));

    let write_result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(source) = write_result {
        let _ = std::fs::remove_file(&temporary);
        return Err(BundleError::Io {
            path: target.to_path_buf(),
            source,
        });
    }

    if let Err(source) = std::fs::rename(&temporary, target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(BundleError::Io {
            path: target.to_path_buf(),
            source,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_document::{RenameNode, SemanticHistory};

    /// Copy a checked-in fixture into a scratch directory so tests that save,
    /// corrupt, or delete files never modify the committed fixture.
    ///
    /// Tests run in parallel, so each call gets a unique directory; sharing
    /// one would let concurrent tests delete each other's files.
    fn scratch(fixture: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let scratch = std::env::temp_dir().join(format!(
            "spool-bundle-{}-{unique}-{}",
            fixture.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        copy_tree(&fixture, &scratch).expect("copy fixture into scratch dir");
        scratch
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

    fn fixture_root(fixture: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture)
    }

    #[test]
    fn valid_bundle_loads_with_identity_hierarchy_and_bindings_intact() {
        let bundle = ProjectBundle::load(fixture_root("landing")).expect("fixture loads");
        let nodes = &bundle.document.structure.nodes;

        assert_eq!(nodes.len(), 3);
        let root = &nodes[0];
        assert_eq!(root.id.as_str(), "spool-frame-root");
        assert_eq!(root.name, "Landing Frame");
        assert_eq!(root.kind, "frame");
        assert_eq!(root.parent, None);
        assert_eq!(
            root.children,
            vec![
                NodeId::new("spool-text-headline").unwrap(),
                NodeId::new("spool-cta-primary").unwrap(),
            ]
        );

        let headline = &nodes[1];
        assert_eq!(headline.name, "Headline");
        assert_eq!(headline.kind, "text");
        assert_eq!(headline.parent.as_ref(), Some(&root.id));

        // Authored content is loaded as bytes, not reconstructed: the HTML
        // itself, plus the stylesheet it links so the runtime can interpret
        // how the page is meant to look.
        // `sources` is a map, so compare the set rather than an iteration
        // order.
        let mut loaded = bundle.document.sources.keys().collect::<Vec<_>>();
        loaded.sort();
        assert_eq!(loaded, ["index.html", "styles.css"]);
        assert!(
            bundle.document.sources["index.html"].contains("Design in source, structure in Spool")
        );
        assert!(bundle.document.sources["styles.css"].contains(".cta"));
    }

    #[test]
    fn bundle_layout_is_not_frozen_pages_styles_and_assets_resolve() {
        // Nothing in the loader knows these directory names; they come only
        // from each node's own `file` binding.
        let bundle = ProjectBundle::load(fixture_root("nested")).expect("nested fixture loads");
        // The page's own file, plus the stylesheet it links — resolved the way a
        // browser resolves it, from `pages/` upward, and keyed by where it
        // landed rather than by the href that found it. Compared as a set
        // because the map is not ordered.
        let mut files: Vec<&String> = bundle.document.sources.keys().collect();
        files.sort();
        assert_eq!(files, ["pages/index.html", "styles/site.css"]);
        assert!(bundle.document.sources["pages/index.html"].contains("spool-page-home"));
        assert!(
            bundle.document.sources["styles/site.css"].contains("letter-spacing"),
            "the linked stylesheet is authored source the runtime must see"
        );

        // assets/ carries no data-spool-id binding and no link, so it is
        // deliberately not loaded: this phase resolves what the project
        // references and does not go looking for more.
        assert!(bundle.root().join("assets/logo.svg").is_file());
        assert!(!bundle.document.sources.contains_key("assets/logo.svg"));
    }

    #[test]
    fn a_linked_stylesheet_is_keyed_by_where_it_landed_not_by_its_href() {
        // The key a file is stored under has to be resolvable as a write
        // target. An href of `../styles/site.css` is not; `styles/site.css`,
        // which is the same file measured from the project root, is.
        let bundle = ProjectBundle::load(fixture_root("nested")).expect("nested fixture loads");
        for key in bundle.document.sources.keys() {
            assert!(
                !key.contains(".."),
                "a sources key must be project-root-relative: {key}"
            );
            let path = resolve_within_root(bundle.root(), key).expect("key resolves in-project");
            assert!(path.is_file(), "{key} should name a file that exists");
        }
    }

    #[test]
    fn a_link_that_climbs_out_of_the_project_is_not_followed() {
        // `../outside.css` from a page at the root would escape the project.
        // It is left unfollowed rather than read from the filesystem above it,
        // which is the same rule a binding reference follows.
        let dir = scratch("landing");
        let html = std::fs::read_to_string(dir.join("index.html")).unwrap();
        std::fs::write(
            dir.join("index.html"),
            html.replace(
                r#"<link rel="stylesheet" href="styles.css" />"#,
                "<link rel=\"stylesheet\" href=\"../outside.css\" />",
            ),
        )
        .unwrap();

        let bundle = ProjectBundle::load(&dir).expect("the project still loads");
        assert!(
            !bundle.document.sources.contains_key("../outside.css"),
            "an escaping href must not become a source key"
        );
        assert!(!bundle.document.sources.contains_key("styles.css"));
    }

    #[test]
    fn one_file_loaded_twice_keeps_one_key() {
        // Two spellings of the same stylesheet must not become two copies: a
        // later write addresses a file by key and would not know which one it
        // owns.
        let dir = scratch("landing");
        let bundle = ProjectBundle::load(&dir).expect("scratch loads");
        let css: Vec<String> = bundle
            .document
            .sources
            .keys()
            .filter(|key| key.ends_with(".css"))
            .cloned()
            .collect();
        assert_eq!(css, ["styles.css"]);

        // Same file, spelled differently in metadata, reached through the link
        // as well: still one entry.
        let metadata = std::fs::read_to_string(dir.join(METADATA_FILE)).unwrap();
        std::fs::write(
            dir.join(METADATA_FILE),
            metadata.replace("file: \"index.html\"", "file: \"./index.html\""),
        )
        .unwrap();
        let bundle = ProjectBundle::load(&dir).expect("scratch loads with a dotted path");
        assert!(
            bundle.document.sources.contains_key("./index.html"),
            "metadata keeps the spelling it authored"
        );
        assert_eq!(
            bundle
                .document
                .sources
                .keys()
                .filter(|key| key.ends_with(".css"))
                .count(),
            1,
            "the linked stylesheet is not loaded a second time"
        );
    }

    #[test]
    fn load_structure_reads_metadata_without_reading_authored_source() {
        let dir = scratch("landing");
        let structure = ProjectBundle::load_structure(&dir).expect("metadata alone loads");
        assert_eq!(structure.nodes.len(), 3);

        // It is metadata only: deleting an authored file the bindings name does
        // not affect the answer, which is exactly why a save may use it.
        std::fs::remove_file(dir.join("index.html")).unwrap();
        assert_eq!(
            ProjectBundle::load_structure(&dir)
                .expect("still loads")
                .nodes
                .len(),
            3
        );
        assert!(
            ProjectBundle::load(&dir).is_err(),
            "a full load still refuses a project whose source is missing"
        );
    }

    #[test]
    fn missing_metadata_is_reported_with_the_root_that_was_searched() {
        let dir = scratch("landing");
        std::fs::remove_file(dir.join(METADATA_FILE)).unwrap();
        match ProjectBundle::load(&dir) {
            Err(BundleError::MissingMetadata { root }) => assert_eq!(root, dir),
            other => panic!(
                "expected MissingMetadata, got {other:?}",
                other = describe(other)
            ),
        }
        // locate_metadata is the single source of truth for the filename.
        assert!(matches!(
            ProjectBundle::locate_metadata(&dir),
            Err(BundleError::MissingMetadata { .. })
        ));
    }

    #[test]
    fn malformed_metadata_is_reported_against_the_metadata_path() {
        let dir = scratch("landing");
        // Refused by both readers, and it has to be refused by both for this to be
        // the interesting case: a version 2 file the editor's reader rejects is now
        // handed to the reader that defines the schema, so only a refusal from
        // that one is a malformed project. This file has a field that does not
        // exist, which is a mistake rather than a format.
        std::fs::write(
            dir.join(METADATA_FILE),
            "version: 2\nnodes: []\nnot_a_field: true\n",
        )
        .unwrap();
        match ProjectBundle::load(&dir) {
            Err(BundleError::MalformedMetadata { path, source }) => {
                assert_eq!(path, dir.join(METADATA_FILE));
                assert!(
                    source.to_string().contains("not_a_field"),
                    "and the reason is the schema's own wording: {source}"
                );
            }
            other => panic!(
                "expected MalformedMetadata, got {other:?}",
                other = describe(other)
            ),
        }
    }

    #[test]
    fn duplicate_and_dangling_metadata_are_rejected_by_existing_validation() {
        let dir = scratch("landing");
        let original = std::fs::read_to_string(dir.join(METADATA_FILE)).unwrap();

        // Duplicate id.
        let duplicated_id =
            original.replacen("id: \"spool-text-headline\"", "id: \"spool-frame-root\"", 1);
        std::fs::write(dir.join(METADATA_FILE), &duplicated_id).unwrap();
        assert!(matches!(
            ProjectBundle::load(&dir),
            Err(BundleError::MalformedMetadata {
                source: ModelError::DuplicateId(_),
                ..
            })
        ));

        // Dangling child reference.
        let dangling = original.replacen(
            "children: [\"spool-text-headline\", \"spool-cta-primary\"]",
            "children: [\"spool-text-headline\", \"spool-ghost\"]",
            1,
        );
        std::fs::write(dir.join(METADATA_FILE), &dangling).unwrap();
        assert!(matches!(
            ProjectBundle::load(&dir),
            Err(BundleError::MalformedMetadata {
                source: ModelError::MissingNode(_),
                ..
            })
        ));
    }

    #[test]
    fn missing_source_file_names_the_node_and_the_file() {
        let dir = scratch("landing");
        std::fs::remove_file(dir.join("index.html")).unwrap();
        match ProjectBundle::load(&dir) {
            Err(BundleError::MissingSource {
                file,
                referenced_by,
            }) => {
                assert_eq!(file, "index.html");
                assert_eq!(referenced_by.as_str(), "spool-frame-root");
            }
            other => panic!(
                "expected MissingSource, got {other:?}",
                other = describe(other)
            ),
        }
    }

    #[test]
    fn dangling_binding_reports_zero_matches_and_ambiguity_is_not_guessed() {
        let dir = scratch("landing");
        let html_path = dir.join("index.html");
        let original = std::fs::read_to_string(&html_path).unwrap();

        // Dangling: the authored identity marker is gone.
        std::fs::write(
            &html_path,
            original.replace("spool-cta-primary", "renamed-away"),
        )
        .unwrap();
        match ProjectBundle::load(&dir) {
            Err(BundleError::UnresolvedBinding { id, matches, .. }) => {
                assert_eq!(id.as_str(), "spool-cta-primary");
                assert_eq!(matches, 0);
            }
            other => panic!(
                "expected UnresolvedBinding, got {other:?}",
                other = describe(other)
            ),
        }

        // Ambiguous: the same identity appears twice.
        std::fs::write(&html_path, format!("{original}\n<!-- {original} -->")).unwrap();
        match ProjectBundle::load(&dir) {
            Err(BundleError::UnresolvedBinding { id, matches, .. }) => {
                assert_eq!(id.as_str(), "spool-frame-root");
                assert_eq!(matches, 2);
            }
            other => panic!(
                "expected UnresolvedBinding, got {other:?}",
                other = describe(other)
            ),
        }
    }

    #[test]
    fn references_escaping_the_project_root_are_rejected() {
        for escape in ["../outside.html", "/etc/passwd", "pages/../../outside.html"] {
            let dir = scratch("landing");
            let metadata = std::fs::read_to_string(dir.join(METADATA_FILE)).unwrap();
            let escaped = metadata.replacen("\"index.html\"", &format!("\"{escape}\""), 1);
            std::fs::write(dir.join(METADATA_FILE), &escaped).unwrap();
            assert!(
                matches!(
                    ProjectBundle::load(&dir),
                    Err(BundleError::InvalidSourceReference { .. })
                ),
                "expected {escape:?} to be rejected as an in-project reference"
            );
        }
    }

    #[test]
    fn save_load_round_trip_preserves_the_document() {
        let dir = scratch("landing");
        let mut bundle = ProjectBundle::load(&dir).expect("scratch loads");

        bundle.document.structure.nodes[1].name = "Hero headline".into();
        bundle.save().expect("save succeeds");

        let reloaded = ProjectBundle::load(&dir).expect("reload after save");
        assert_eq!(reloaded.document.structure, bundle.document.structure);
        assert_eq!(reloaded.document.sources, bundle.document.sources);
    }

    #[test]
    fn metadata_only_save_leaves_source_bytes_unchanged() {
        let dir = scratch("landing");
        let bundle = ProjectBundle::load(&dir).expect("scratch loads");

        let html_before = std::fs::read(dir.join("index.html")).unwrap();
        let css_before = std::fs::read(dir.join("styles.css")).unwrap();
        let metadata_before = std::fs::read_to_string(dir.join(METADATA_FILE)).unwrap();

        let mut edited = bundle;
        edited.document.structure.nodes[1].name = "Hero headline".into();
        edited.save().expect("save");

        assert_eq!(std::fs::read(dir.join("index.html")).unwrap(), html_before);
        assert_eq!(std::fs::read(dir.join("styles.css")).unwrap(), css_before);

        let metadata_after = std::fs::read_to_string(dir.join(METADATA_FILE)).unwrap();
        assert_ne!(metadata_after, metadata_before);
        assert!(metadata_after.contains("Hero headline"));
    }

    #[test]
    fn save_preserves_unrelated_and_unknown_project_files() {
        let dir = scratch("landing");
        // Files Spool knows nothing about must survive a metadata save.
        std::fs::write(dir.join("notes.md"), "scratch notes\n").unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        let unrelated = dir.join("assets/photo.png");
        std::fs::write(&unrelated, [0x89, b'P', b'N', b'G']).unwrap();

        let bundle = ProjectBundle::load(&dir).expect("scratch loads");
        bundle.save().expect("save");

        assert_eq!(
            std::fs::read_to_string(dir.join("notes.md")).unwrap(),
            "scratch notes\n"
        );
        assert_eq!(std::fs::read(&unrelated).unwrap(), [0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn metadata_serialization_is_deterministic() {
        let dir = scratch("landing");
        let bundle = ProjectBundle::load(&dir).expect("scratch loads");

        // Same document in, same bytes out, every time.
        assert_eq!(
            bundle.metadata_yaml().unwrap(),
            bundle.metadata_yaml().unwrap()
        );

        // And a reload of those bytes reproduces the same document.
        let first = bundle.metadata_yaml().unwrap();
        std::fs::write(dir.join(METADATA_FILE), &first).unwrap();
        let reloaded = ProjectBundle::load(&dir).expect("reload");
        assert_eq!(reloaded.metadata_yaml().unwrap(), first);
        assert_eq!(reloaded.document.structure, bundle.document.structure);
    }

    #[test]
    fn invalid_metadata_is_never_written_and_the_old_file_survives() {
        let dir = scratch("landing");
        let metadata_before = std::fs::read(dir.join(METADATA_FILE)).unwrap();

        let mut bundle = ProjectBundle::load(&dir).expect("scratch loads");
        // Collides with an existing name; to_yaml validates before writing.
        bundle.document.structure.nodes[1].name = "Landing Frame".into();
        assert!(matches!(
            bundle.save(),
            Err(BundleError::MalformedMetadata { .. })
        ));
        assert_eq!(
            std::fs::read(dir.join(METADATA_FILE)).unwrap(),
            metadata_before
        );
        assert!(
            !has_temp_files(&dir),
            "no temporary file should be left behind"
        );
    }

    #[test]
    #[cfg(unix)]
    fn metadata_is_replaced_atomically_rather_than_truncated_in_place() {
        use std::os::unix::fs::MetadataExt;

        let dir = scratch("landing");
        let target = dir.join(METADATA_FILE);
        let mut bundle = ProjectBundle::load(&dir).expect("scratch loads");

        // Atomic replacement swaps in a new file, so the target's identity
        // changes. An in-place rewrite would keep the same inode, which is
        // the observable difference between the two strategies.
        let before = std::fs::metadata(&target).unwrap().ino();

        bundle.document.structure.nodes[1].name = "Hero headline".into();
        bundle.save().expect("save succeeds");

        let after = std::fs::metadata(&target).unwrap().ino();
        assert_ne!(
            before, after,
            "metadata should be replaced by rename, not rewritten in place"
        );
        assert!(
            ProjectBundle::load(&dir).is_ok(),
            "the swapped-in file must be valid"
        );
    }

    #[test]
    #[cfg(unix)]
    fn unwritable_directory_fails_the_save_without_debris() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("landing");
        let metadata_before = std::fs::read_to_string(dir.join(METADATA_FILE)).unwrap();
        let bundle = ProjectBundle::load(&dir).expect("scratch loads");

        // Remove write permission from the directory so the temporary file
        // cannot be created. The bundle is loaded first, so the failure is
        // attributable to saving and not to loading.
        let original_mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();

        let outcome = bundle.save();

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(original_mode)).unwrap();

        assert!(
            matches!(outcome, Err(BundleError::Io { .. })),
            "saving into an unwritable directory must fail"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(METADATA_FILE)).unwrap(),
            metadata_before,
            "a failed save must not corrupt existing metadata"
        );
        assert!(
            !has_temp_files(&dir),
            "failed save must clean up its temporary file"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_failing_replacement_keeps_the_target_and_removes_the_temporary() {
        let dir = scratch("landing");
        let metadata_before = std::fs::read_to_string(dir.join(METADATA_FILE)).unwrap();
        let mut bundle = ProjectBundle::load(&dir).expect("scratch loads");
        bundle.document.structure.nodes[1].name = "Hero headline".into();
        let edited_yaml = bundle.metadata_yaml().expect("serialize edited metadata");

        // Occupy the target path with a directory. The temporary file is
        // still created successfully, so this exercises the replacement step
        // specifically: the save must fail there and clean up after itself,
        // rather than leaving a temporary file or a half-written target.
        let target = dir.join(METADATA_FILE);
        std::fs::remove_file(&target).unwrap();
        std::fs::create_dir(&target).unwrap();

        assert!(
            matches!(bundle.save(), Err(BundleError::Io { .. })),
            "replacing metadata with a directory must fail"
        );
        assert!(
            target.is_dir(),
            "a failed replacement must not have clobbered the target path"
        );
        assert!(
            !has_temp_files(&dir),
            "a failed replacement must clean up its temporary file"
        );

        // Once the obstruction is gone the same save succeeds and writes
        // exactly the metadata it had computed before the failure.
        std::fs::remove_dir(&target).unwrap();
        std::fs::write(&target, &metadata_before).unwrap();
        bundle
            .save()
            .expect("save succeeds once the target is a file");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            edited_yaml,
            "the retried save must produce the metadata it had computed"
        );
    }

    #[test]
    fn semantic_rename_survives_save_undo_and_reload() {
        let dir = scratch("landing");
        let css_before = std::fs::read(dir.join("styles.css")).unwrap();
        let mut bundle = ProjectBundle::load(&dir).expect("scratch loads");
        let mut history = SemanticHistory::default();

        let rename = RenameNode {
            id: NodeId::new("spool-text-headline").unwrap(),
            before: "Headline".into(),
            after: "Hero headline".into(),
        };
        assert!(history.record(rename.clone()));
        // The bundle holds no canvas runtime, which is why the unified
        // boundary needs a metadata-only target rather than requiring a
        // throwaway `Document` to satisfy `Full`.
        rename
            .apply(&mut bundle.document)
            .expect("the rename applies");
        bundle.save().expect("save renamed metadata");

        assert!(history
            .undo(&mut bundle.document)
            .expect("undo replays the rename"));
        bundle.save().expect("save after undo");
        let reloaded = ProjectBundle::load(&dir).expect("reload");
        assert_eq!(reloaded.document.structure.nodes[1].name, "Headline");

        // CSS is not in metadata and was byte-identical throughout.
        assert_eq!(std::fs::read(dir.join("styles.css")).unwrap(), css_before);
    }

    #[test]
    fn a_version_two_manifest_loads_into_the_fields_the_editor_speaks() {
        let bundle = ProjectBundle::load(fixture_root("owner-b")).expect("the fixture loads");
        let nodes = &bundle.document.structure.nodes;

        // Version 2 is a superset of what the editor reads, so every node is
        // there with the same six fields a version 1 fixture carries.
        assert_eq!(nodes.len(), 3);
        let root = &nodes[0];
        assert_eq!(root.id.as_str(), "spool-frame-release");
        assert_eq!(root.name, "Release Frame");
        assert_eq!(root.kind, "frame");
        assert_eq!(root.parent, None);
        assert_eq!(
            root.children,
            vec![
                NodeId::new("spool-text-headline").unwrap(),
                NodeId::new("spool-frame-badge").unwrap(),
            ],
            "hierarchy came across, in the order the file states"
        );
        assert_eq!(root.source.file, "index.html");
        assert_eq!(
            root.source.selector,
            "[data-spool-id=\"spool-frame-release\"]"
        );
        assert_eq!(nodes[1].kind, "text");
        assert_eq!(nodes[1].parent.as_ref(), Some(&root.id));
        assert_eq!(nodes[2].name, "Status Badge");

        // Bindings were still resolved against the authored markup, so this is a
        // project rather than a metadata file pointing at nothing.
        let mut loaded = bundle.document.sources.keys().collect::<Vec<_>>();
        loaded.sort();
        assert_eq!(loaded, ["index.html", "styles.css"]);
    }

    #[test]
    fn a_loaded_bundle_keeps_the_metadata_bytes_it_read() {
        let bundle = ProjectBundle::load(fixture_root("owner-b")).expect("the fixture loads");
        let text = bundle
            .manifest_yaml()
            .expect("a loaded bundle has the bytes it read");
        assert!(text.starts_with("# Spool project metadata"), "{text}");
        assert_eq!(
            text,
            std::fs::read_to_string(fixture_root("owner-b").join(METADATA_FILE))
                .expect("the fixture is on disk"),
            "verbatim: the same bytes, not a re-encoding"
        );

        // A bundle built from a document has no file behind it, so it claims
        // nothing about one.
        let wrapped =
            ProjectBundle::from_document(fixture_root("landing"), PersistentDocument::default())
                .expect("wrapping a document needs no file");
        assert_eq!(wrapped.manifest_yaml(), None);
    }

    #[test]
    fn load_structure_refuses_a_version_two_project_so_a_save_cannot_overwrite_it() {
        // The save path compares the editor's document against `load_structure`,
        // so this refusal is what stops a save from writing a version 1 encoding
        // over a version 2 file and losing every Owner B value in it. The honest
        // answer is that a version 2 project is not savable yet, not that it is
        // savable into something smaller.
        assert!(matches!(
            ProjectBundle::load_structure(fixture_root("owner-b")),
            Err(BundleError::MalformedMetadata { .. })
        ));
        // Version 1 is untouched: the comparison a save actually depends on.
        assert_eq!(
            ProjectBundle::load_structure(fixture_root("landing"))
                .expect("version 1 metadata still reads")
                .nodes
                .len(),
            3
        );
    }

    #[test]
    fn a_version_one_project_saves_exactly_as_it_did() {
        let dir = scratch("landing");
        let before = std::fs::read_to_string(dir.join(METADATA_FILE)).unwrap();
        let mut bundle = ProjectBundle::load(&dir).expect("scratch loads");

        bundle.document.structure.nodes[1].name = "Hero headline".into();
        bundle.save().expect("save succeeds");

        let after = std::fs::read_to_string(dir.join(METADATA_FILE)).unwrap();
        assert!(
            after.starts_with("version: 1\nnodes:\n"),
            "a version 1 project is still written as version 1: {after}"
        );
        assert_eq!(
            after,
            format!("{}\n", before.replace("\"Headline\"", "\"Hero headline\"")),
            "and the encoding is byte-identical to the one the file already used, \
             apart from the trailing newline the writer always emits"
        );
    }

    #[test]
    fn document_sources_never_duplicate_metadata_or_unreferenced_files() {
        let bundle = ProjectBundle::load(fixture_root("landing")).expect("fixture loads");
        // Sources are authored bytes a node binds to, plus the stylesheets
        // that HTML links. Metadata is never among them: `lamine.yaml` is not
        // loaded into the source map, so it cannot become a second copy of
        // itself.
        // `sources` is a map, so compare the set rather than an iteration
        // order.
        let mut loaded = bundle.document.sources.keys().collect::<Vec<_>>();
        loaded.sort();
        assert_eq!(loaded, ["index.html", "styles.css"]);
        assert!(!bundle.document.sources.contains_key(METADATA_FILE));
        // And nothing that no node references and no document links.
        assert!(!bundle.document.sources.contains_key("unrelated.css"));
    }

    /// Name the outcome of a load without printing a whole document.
    fn describe(error: Result<ProjectBundle, BundleError>) -> &'static str {
        match error {
            Ok(_) => "Ok",
            Err(_) => "Err",
        }
    }

    /// Whether any atomic-write scratch file survived in `dir`.
    ///
    /// Matches every file Spool stages, not just the metadata one: authored
    /// sources go through the same replace, so a save that leaves a scratch
    /// file behind has a bug a metadata-only check would miss.
    fn has_temp_files(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries.filter_map(Result::ok).any(|entry| {
                    entry.file_name().to_string_lossy().starts_with('.')
                        && entry.file_name().to_string_lossy().contains(".tmp-")
                })
            })
            .unwrap_or(false)
    }
}
