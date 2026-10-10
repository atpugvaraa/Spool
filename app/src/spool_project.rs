//! The `.spool` project boundary.
//!
//! # What this is
//!
//! A Spool project is a **directory whose name ends in `.spool`**. That is the
//! whole of the format. It is deliberately not a file: the authored HTML, CSS
//! and SVG a person actually wrote have to stay readable, diffable and editable
//! by their usual tools, and Spool is not in a position to ask them to give that
//! up in exchange for a container it would then own.
//!
//! ```text
//! MyProject.spool/
//!   lamine.yaml          required — Spool identity, hierarchy, bindings
//!   pages/index.html     the authored source the bindings point at
//!   styles/styles.css    discovered through the document's <link>
//!   assets/mark.svg      referenced by the markup; never parsed as structure
//! ```
//!
//! The `pages/`, `styles/` and `assets/` names above are what the canonical
//! fixture uses, **not** a requirement. Nothing in this module names them. Each
//! binding in `lamine.yaml` carries its own project-relative `file`, and
//! stylesheets are found by following `<link href>` the way a browser would, so
//! a project may lay itself out however it likes. Imposing a layout here would
//! duplicate a decision `lamine.yaml` already makes per binding, and would make
//! every existing project invalid for no reason.
//!
//! # What owns what
//!
//! ```text
//! MyProject.spool/
//!   lamine.yaml   identity, hierarchy, bindings, provenance — Spool's alone
//!   *.html *.css   the authored design, and the only visual truth
//!   runtime        derived, disposable, never written back
//! ```
//!
//! Nothing in the boundary copies authored source into metadata, and nothing
//! here is a second document database. Opening a project writes nothing.
//!
//! # Why it is a separate layer
//!
//! [`crate::project_open::open_project`] is the loader, and it stays
//! layout-agnostic and path-agnostic on purpose: it is what tests and internal
//! tooling use against plain fixture directories. This module is the product
//! boundary above it — it decides whether a path *claims* to be a Spool project
//! and refuses if it does not — and then delegates. There is one loader and one
//! open path; the check lives above them, not beside them.

//! # MUTATION HARNESS
//!
//! `app/mutate_spool_project.sh` breaks one rule at a time in this file — the
//! `.spool` name check, the manifest check, the command line's precedence over
//! the development override — and requires the test suite to notice. A rule that
//! can be broken without a test failing is reported as `SURVIVED`, because an
//! untested invariant is the finding. The script refuses to run unless it sees
//! this marker, so it cannot be pointed at an unrelated file.

use std::path::{Path, PathBuf};

use crate::project_bundle::{BundleError, ProjectBundle, METADATA_FILE};
use crate::project_open::{LoadedProject, ProjectOpenError};
use crate::source_document::{NodeId, PersistentDocument, SourceBinding, StructuralNode};

/// The suffix that makes a directory a Spool project.
pub const PROJECT_EXTENSION: &str = "spool";

/// Why a path is not a usable Spool project.
///
/// These are all *boundary* failures, decided before a single authored byte is
/// read. Anything wrong inside the project — a malformed manifest, a dangling
/// binding, an escaping reference — arrives as [`ProjectOpenError`] instead, so
/// a caller can always tell "that is not a Spool project" from "that is a Spool
/// project and it is broken".
#[derive(Debug)]
pub enum ProjectBoundaryError {
    /// Nothing exists at the path.
    Missing { path: PathBuf },
    /// The path exists but is a file rather than a directory.
    NotADirectory { path: PathBuf },
    /// The directory is not named like a Spool project.
    NotAProjectName { path: PathBuf, name: String },
    /// The directory has no manifest at its root.
    MissingManifest { path: PathBuf },
    /// More than one project was named on the command line.
    ///
    /// Spool opens one project. Two paths is not a choice Spool can make on the
    /// user's behalf, so it says so rather than picking the first one it saw.
    TooManyProjects { count: usize },
}

impl std::fmt::Display for ProjectBoundaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { path } => write!(f, "{} does not exist", path.display()),
            Self::NotADirectory { path } => {
                write!(f, "{} is not a directory", path.display())
            }
            Self::NotAProjectName { path, name } => write!(
                f,
                "{} is not a Spool project: a project directory must be named \
                 something.{PROJECT_EXTENSION}, and this one is named {name:?}",
                path.display(),
            ),
            Self::MissingManifest { path } => write!(
                f,
                "{} is not a Spool project: no {METADATA_FILE} at its root",
                path.display()
            ),
            Self::TooManyProjects { count } => write!(
                f,
                "{count} project paths were given; Spool opens one project at a time",
            ),
        }
    }
}

impl std::error::Error for ProjectBoundaryError {}

/// Either the path is not a Spool project, or it is one that does not open.
#[derive(Debug)]
pub enum ProjectError {
    /// The path does not name a Spool project at all.
    Boundary(ProjectBoundaryError),
    /// It is a Spool project, and loading it failed.
    Open(ProjectOpenError),
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Boundary(error) => write!(f, "{error}"),
            Self::Open(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ProjectError {}

impl From<ProjectBoundaryError> for ProjectError {
    fn from(error: ProjectBoundaryError) -> Self {
        Self::Boundary(error)
    }
}

impl From<ProjectOpenError> for ProjectError {
    fn from(error: ProjectOpenError) -> Self {
        Self::Open(error)
    }
}

/// A directory that has claimed to be a Spool project and has a manifest.
///
/// Constructing one proves only the boundary: the name, the directory, and the
/// manifest's presence. Everything the project *says* is still unverified, and
/// [`SpoolProject::open`] is what verifies it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpoolProject {
    root: PathBuf,
}

impl SpoolProject {
    /// Decide whether `path` is a Spool project, without reading the project.
    ///
    /// This is a naming and shape check, and it is cheap enough to run before
    /// anything else. It exists so that pointing Spool at the wrong thing is a
    /// distinct, reportable outcome rather than a project that opens to nothing.
    pub fn locate(path: impl AsRef<Path>) -> Result<Self, ProjectBoundaryError> {
        let path = path.as_ref();
        let root = path.to_path_buf();
        if !root.exists() {
            return Err(ProjectBoundaryError::Missing { path: root });
        }
        if !root.is_dir() {
            return Err(ProjectBoundaryError::NotADirectory { path: root });
        }
        let name = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if Path::new(&name).extension().and_then(|ext| ext.to_str()) != Some(PROJECT_EXTENSION) {
            return Err(ProjectBoundaryError::NotAProjectName { path: root, name });
        }
        // Reuse the loader's own manifest lookup so the filename is decided in
        // exactly one place, and a missing manifest is reported as a boundary
        // failure here rather than as a load failure later.
        ProjectBundle::locate_metadata(&root).map_err(|error| match error {
            BundleError::MissingMetadata { .. } => {
                ProjectBoundaryError::MissingManifest { path: root.clone() }
            }
            other => unreachable!("locate_metadata only reports MissingMetadata: {other}"),
        })?;
        Ok(Self { root })
    }

    /// The project root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Open the project and derive its runtime document.
    ///
    /// Delegates to [`crate::project_open::open_project`]. Every existing
    /// guarantee holds unchanged: bindings resolve to exactly one authored
    /// element, references are refused if they leave the root, and a project
    /// that binds but draws nothing is reported rather than shown blank.
    pub fn open(&self) -> Result<LoadedProject, ProjectOpenError> {
        crate::project_open::open_project(self.root())
    }
}

/// Open the Spool project at `path`.
///
/// This is the product's entry point. `path` identifies a `.spool` project;
/// nothing about it is inferred. A path that is not one fails here rather than
/// opening as an empty scene.
pub fn open(path: impl AsRef<Path>) -> Result<LoadedProject, ProjectError> {
    Ok(SpoolProject::locate(path)?.open()?)
}

/// The identity of a brand-new project's one object.
///
/// Fixed, because there is exactly one and it is the document's root. It is the
/// same identity the fixtures use, so a project Spool created and one a person
/// authored are the same shape on disk.
const NEW_ROOT_ID: &str = "spool-frame-root";

/// The one authored file a new project needs.
///
/// Flat rather than `pages/`, because a layout is a choice the author makes and
/// `lamine.yaml` records per binding. A project with no layout yet should not
/// impose one.
const NEW_SOURCE_FILE: &str = "index.html";

/// Why a new project could not be written.
#[derive(Debug)]
pub enum ProjectCreateError {
    /// The name is empty or is not a path.
    NoName,
    /// The directory name does not end in `.spool`.
    NotAProjectName { path: PathBuf, name: String },
    /// Something is already there, so writing would destroy it.
    AlreadyExists { path: PathBuf },
    /// The parent directory could not be created.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The metadata could not be encoded, or could not be written.
    Bundle(BundleError),
}

impl std::fmt::Display for ProjectCreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoName => write!(f, "give a name for the new project"),
            Self::NotAProjectName { path, name } => write!(
                f,
                "{} must be named something.{PROJECT_EXTENSION}, and {name:?} is not",
                path.display()
            ),
            Self::AlreadyExists { path } => {
                write!(f, "{} already exists", path.display())
            }
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Bundle(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ProjectCreateError {}

/// Write a new, minimal, valid `.spool` project at `path`.
///
/// The smallest thing the loader accepts, and nothing more: a manifest and one
/// authored file, with the manifest produced by the *same* encoder that reads it
/// back. That is what makes the result indistinguishable from a project someone
/// authored by hand — the only way to guarantee it is not to write the format a
/// second time.
///
/// `path` is the project directory including its `.spool` suffix; the directory
/// name is the project's name, because `lamine.yaml` has no project-level name
/// field to hold one.
///
/// Refuses rather than overwrites: an existing directory with anything in it is
/// someone's work.
pub fn create(path: impl AsRef<Path>) -> Result<SpoolProject, ProjectCreateError> {
    let root = path.as_ref().to_path_buf();
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.is_empty() {
        return Err(ProjectCreateError::NoName);
    }
    if Path::new(&name).extension().and_then(|ext| ext.to_str()) != Some(PROJECT_EXTENSION) {
        return Err(ProjectCreateError::NotAProjectName { path: root, name });
    }
    // An existing empty directory is a location the user already chose and is
    // fine to fill. Anything in it is not.
    if root.exists() {
        let empty = std::fs::read_dir(&root)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if !empty {
            return Err(ProjectCreateError::AlreadyExists { path: root });
        }
    }
    // The project directory itself, not only its parent: the location may be a
    // new folder several levels deep.
    std::fs::create_dir_all(&root).map_err(|source| ProjectCreateError::Io {
        path: root.clone(),
        source,
    })?;

    let mut document = PersistentDocument::default();
    document.structure.nodes.push(StructuralNode {
        id: NodeId::new(NEW_ROOT_ID).expect("a fixed id is valid"),
        name: "Frame".to_owned(),
        kind: "frame".to_owned(),
        parent: None,
        children: Vec::new(),
        source: SourceBinding {
            file: NEW_SOURCE_FILE.to_owned(),
            selector: format!("[data-spool-id=\"{NEW_ROOT_ID}\"]"),
        },
    });

    // The authored file first. If the manifest were written first and this
    // failed, the project would be a manifest pointing at nothing.
    let document_html = format!(
        "<!doctype html>\n<html lang=\"en\">\n  <head>\n    <meta charset=\"utf-8\" />\n    <title>Spool</title>\n  </head>\n  <body>\n    <main data-spool-id=\"{NEW_ROOT_ID}\"></main>\n  </body>\n</html>\n"
    );
    std::fs::write(root.join(NEW_SOURCE_FILE), &document_html).map_err(|source| {
        ProjectCreateError::Io {
            path: root.join(NEW_SOURCE_FILE),
            source,
        }
    })?;

    // Through the existing bundle writer, so the manifest is encoded exactly as
    // a saved project's is.
    ProjectBundle::from_document(&root, document)
        .and_then(|bundle| bundle.save())
        .map_err(ProjectCreateError::Bundle)?;

    // Located rather than constructed: the project now has to satisfy exactly the
    // check every other project does, or it is not one.
    SpoolProject::locate(&root).map_err(|error| {
        ProjectCreateError::Bundle(BundleError::Io {
            path: root,
            source: std::io::Error::other(error.to_string()),
        })
    })
}

/// The project named by the process's arguments, if one was.
///
/// This is how `Spool.app /path/to/MyProject.spool` opens that project, and how
/// a launch with no argument opens nothing. It deliberately resolves only *which
/// path was named* — whether that path is a project is
/// [`SpoolProject::locate`]'s answer, and opening it is [`open`]'s.
///
/// `args` is the full argument vector, program name included; the name is
/// skipped. Anything beginning with `-` is treated as a switch rather than a
/// path, since this milestone defines no switches.
pub fn requested_from_args<I>(args: I) -> Result<Option<PathBuf>, ProjectBoundaryError>
where
    I: IntoIterator<Item = String>,
{
    let mut named = args
        .into_iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .filter(|arg| !arg.is_empty());
    let first = named.next();
    match named.next() {
        None => Ok(first.map(PathBuf::from)),
        Some(_) => {
            let count = 2 + named.count();
            Err(ProjectBoundaryError::TooManyProjects { count })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_open::ProjectOpenError;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");

    /// The canonical project, copied out under a name the boundary accepts so a
    /// test can damage it.
    fn scratch(name: &str) -> PathBuf {
        // The scratch name has to end in `.spool` or `locate` refuses it before
        // the test gets to the damage it meant to do.
        let to = std::env::temp_dir().join(format!(
            "spool-{name}-{}-{:?}.spool",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&to);
        copy_tree(&Path::new(FIXTURES).join("landing.spool"), &to).expect("copy fixture");
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

    /// One node's entry in a `lamine.yaml`, in the shape the bundle reads.
    ///
    /// The selector goes inside a quoted scalar, so its own quotes have to be
    /// escaped exactly as a hand-written manifest escapes them. Getting this
    /// wrong makes every test here pass for the wrong reason — a manifest that
    /// does not parse fails before the binding is ever resolved.
    fn node_yaml(id: &str, file: &str, selector: &str) -> String {
        format!(
            "  - id: \"{id}\"\n    name: \"{id}\"\n    kind: \"frame\"\n    parent: null\n    file: \"{file}\"\n    selector: \"{selector}\"\n    children: []\n",
            selector = selector.replace('"', "\\\"")
        )
    }

    /// The [`BundleError`] an open failed with, so a test can assert on *why*
    /// rather than merely that something failed.
    fn load_error(error: ProjectError) -> BundleError {
        match error {
            ProjectError::Open(ProjectOpenError::Load(error)) => error,
            other => panic!("expected a load failure, got {other}"),
        }
    }

    fn write_manifest(root: &Path, body: &str) {
        std::fs::write(
            root.join(METADATA_FILE),
            format!("version: 1\nnodes:\n{body}"),
        )
        .expect("write manifest");
    }

    // -- the contract ------------------------------------------------------

    #[test]
    fn a_valid_spool_project_opens() {
        let project = SpoolProject::locate(Path::new(FIXTURES).join("landing.spool"))
            .expect("the canonical fixture is a Spool project");
        assert!(project.root().ends_with("landing.spool"));
        assert_eq!(
            project.root().join(METADATA_FILE),
            project.root().join("lamine.yaml")
        );

        let loaded = project.open().expect("the canonical fixture opens");
        assert_eq!(loaded.document.structure.nodes.len(), 3);
        assert_eq!(loaded.runtime.objects().len(), 3);
        assert!(
            loaded.unrendered.is_empty(),
            "{}",
            loaded.unrendered.join(", ")
        );
    }

    /// The loader is layout-agnostic: nothing here may start depending on the
    /// fixture's directory names.
    #[test]
    fn the_layout_is_discovered_not_assumed() {
        let loaded = open(Path::new(FIXTURES).join("landing.spool")).expect("opens");
        let files: Vec<&str> = loaded.document.sources.keys().map(String::as_str).collect();
        assert!(files.contains(&"pages/index.html"), "{files:?}");
        // Found through the document's <link href="../styles/styles.css">.
        assert!(files.contains(&"styles/styles.css"), "{files:?}");
    }

    #[test]
    fn a_directory_not_named_spool_is_refused() {
        // A perfectly good source-backed project, but not a Spool project.
        let error = SpoolProject::locate(Path::new(FIXTURES).join("landing"))
            .expect_err("a bare directory is not a project");
        assert!(matches!(
            error,
            ProjectBoundaryError::NotAProjectName { .. }
        ));
    }

    #[test]
    fn a_file_is_not_a_project() {
        let error = SpoolProject::locate(Path::new(FIXTURES).join("landing/index.html"))
            .expect_err("a file is not a project");
        assert!(matches!(error, ProjectBoundaryError::NotADirectory { .. }));
    }

    #[test]
    fn a_missing_path_is_not_a_project() {
        let error = SpoolProject::locate(Path::new(FIXTURES).join("nope.spool"))
            .expect_err("nothing is not a project");
        assert!(matches!(error, ProjectBoundaryError::Missing { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "{} does not exist",
                Path::new(FIXTURES).join("nope.spool").display()
            ),
            "a path that was never there should not be reported as the wrong shape"
        );
    }

    // -- manifest ----------------------------------------------------------

    #[test]
    fn a_project_without_a_manifest_is_refused() {
        let root = scratch("no-manifest");
        std::fs::remove_file(root.join(METADATA_FILE)).expect("remove manifest");
        let error = SpoolProject::locate(&root).expect_err("no manifest is not a project");
        assert!(matches!(
            error,
            ProjectBoundaryError::MissingManifest { .. }
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_malformed_manifest_fails_to_open() {
        let root = scratch("malformed-manifest");
        std::fs::write(root.join(METADATA_FILE), "this is not the expected yaml\n").unwrap();
        // It has the right name and a manifest, so it is a project...
        SpoolProject::locate(&root).expect("named correctly");
        // ...that does not open.
        let error = open(&root).expect_err("a malformed manifest does not open");
        assert!(
            matches!(error, ProjectError::Open(ProjectOpenError::Load(_))),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_future_manifest_version_is_refused_rather_than_guessed() {
        let root = scratch("future-version");
        std::fs::write(root.join(METADATA_FILE), "version: 2\nnodes:\n").unwrap();
        assert!(
            open(&root).is_err(),
            "an unknown version must not be read as v1"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_entry_document_fails_to_open() {
        let root = scratch("missing-entry");
        std::fs::remove_file(root.join("pages/index.html")).expect("remove entry");
        let error = open(&root).expect_err("a binding with no file does not open");
        assert!(
            matches!(error, ProjectError::Open(ProjectOpenError::Load(_))),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_binding_that_matches_nothing_fails_to_open() {
        let root = scratch("dangling-binding");
        write_manifest(
            &root,
            &node_yaml(
                "spool-absent",
                "pages/index.html",
                "[data-spool-id=\"spool-absent\"]",
            ),
        );
        match load_error(open(&root).expect_err("a dangling binding must not open")) {
            BundleError::UnresolvedBinding { matches, .. } => assert_eq!(
                matches, 0,
                "the id is absent, so the binding resolves to nothing"
            ),
            other => panic!("expected UnresolvedBinding, got {other}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_ambiguous_binding_fails_to_open() {
        let root = scratch("ambiguous-binding");
        // Two elements carry the id, so the binding cannot mean one of them.
        std::fs::write(
            root.join("pages/index.html"),
            "<main data-spool-id=\"spool-twice\"></main><main data-spool-id=\"spool-twice\"></main>",
        )
        .unwrap();
        write_manifest(
            &root,
            &node_yaml(
                "spool-twice",
                "pages/index.html",
                "[data-spool-id=\"spool-twice\"]",
            ),
        );
        match load_error(open(&root).expect_err("an ambiguous binding must not open")) {
            BundleError::UnresolvedBinding { matches, .. } => {
                assert_eq!(matches, 2, "both elements match, so neither is chosen")
            }
            other => panic!("expected UnresolvedBinding, got {other}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    // -- confinement -------------------------------------------------------

    #[test]
    fn a_binding_outside_the_project_is_refused() {
        let root = scratch("escape-binding");
        let outside = root.parent().expect("a parent");
        std::fs::write(outside.join("spool-project-escape.html"), "<main></main>").unwrap();
        write_manifest(
            &root,
            &node_yaml(
                "spool-frame-root",
                "../spool-project-escape.html",
                "[data-spool-id=\"spool-frame-root\"]",
            ),
        );
        let error = open(&root).expect_err("a reference above the root is refused");
        assert!(
            matches!(
                load_error(error),
                BundleError::InvalidSourceReference { .. }
            ),
            "a `..` reference must be refused as an invalid reference, not read"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(outside.join("spool-project-escape.html"));
    }

    #[test]
    fn an_absolute_stylesheet_reference_is_refused() {
        let root = scratch("absolute-css");
        // An absolute href cannot name a file inside the project, so it resolves
        // to nothing and is left unstyled rather than read from outside.
        std::fs::write(
            root.join("pages/index.html"),
            "<!doctype html><html><head><link rel=\"stylesheet\" \
             href=\"/etc/hosts\" /></head><body><main data-spool-id=\"a\"></main></body></html>",
        )
        .unwrap();
        write_manifest(
            &root,
            &node_yaml("a", "pages/index.html", "[data-spool-id=\"a\"]"),
        );
        let loaded = open(&root).expect("an absolute href is not a project escape");
        assert!(
            !loaded.document.sources.keys().any(|k| k.contains("hosts")),
            "nothing outside the root may enter the source set"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // -- the three states the application must be able to tell -------------

    #[test]
    fn an_invalid_project_never_produces_a_document() {
        // The distinction the application depends on: an invalid path is an
        // error, not an empty project. Nothing here may come back as an `Ok`
        // carrying an empty scene.
        let cases: Vec<(PathBuf, &str)> = vec![
            (Path::new(FIXTURES).join("landing"), "not named .spool"),
            (
                Path::new(FIXTURES).join("landing/index.html"),
                "not a directory",
            ),
            (Path::new(FIXTURES).join("absent.spool"), "does not exist"),
        ];
        for (path, why) in cases {
            match open(&path) {
                Err(_) => {}
                Ok(loaded) => panic!(
                    "{why} ({}) must not open; produced {} objects",
                    path.display(),
                    loaded.runtime.objects().len()
                ),
            }
        }
    }

    #[test]
    fn no_project_at_all_is_not_an_error() {
        // "No project supplied" is a normal state, distinct from "invalid".
        // The application decides what to show; this layer only refuses what was
        // named and was not a project.
        let boundary = SpoolProject::locate(Path::new(FIXTURES).join("nope.spool"));
        assert!(
            boundary.is_err(),
            "a path that was named but is absent is an error"
        );
    }

    // -- creating a project.

    /// A path no test has used, inside the temp dir.
    fn fresh(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "spool-new-{name}-{}-{:?}.spool",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn a_created_project_opens_through_the_existing_path() {
        let root = fresh("opens");
        let project = create(&root).expect("a valid path creates a project");
        assert_eq!(project.root(), root);

        // The whole point: indistinguishable from a project someone authored. The
        // only way to know is to open it with the loader that opens every other
        // project, and to get the same answer.
        let loaded = open(&root).expect("the new project opens");
        assert_eq!(loaded.document.structure.nodes.len(), 1, "one object");
        assert_eq!(loaded.runtime.objects().len(), 1, "and it is drawable");
        assert!(
            loaded.unrendered.is_empty(),
            "a new project has nothing unrendered: {:?}",
            loaded.unrendered
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_created_project_writes_exactly_the_minimum() {
        let root = fresh("minimum");
        create(&root).expect("creates");
        assert_eq!(
            tree_of(&root),
            vec!["index.html", "lamine.yaml"],
            "a manifest and one authored file, and nothing else"
        );
        let manifest = std::fs::read_to_string(root.join("lamine.yaml")).expect("manifest");
        assert!(
            manifest.starts_with("version: 1\nnodes:\n"),
            "the manifest is in the loader's own format: {manifest}"
        );
        assert!(manifest.contains("kind: \"frame\""), "{manifest}");
        let html = std::fs::read_to_string(root.join("index.html")).expect("html");
        assert!(
            html.contains("data-spool-id=\"spool-frame-root\""),
            "and the element carries the identity the manifest binds: {html}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_created_projects_identity_is_bound_by_the_loader() {
        // The selector the loader accepts is exactly one spelling, and a binding
        // in any other form resolves to zero elements and fails the open.
        let root = fresh("bound");
        create(&root).expect("creates");
        let loaded = open(&root).expect("opens");
        let node = &loaded.document.structure.nodes[0];
        assert_eq!(node.id.as_str(), "spool-frame-root");
        assert_eq!(node.source.selector, "[data-spool-id=\"spool-frame-root\"]");
        assert_eq!(
            crate::source_document::HtmlSource {
                file: node.source.file.clone(),
                contents: loaded.document.sources[&node.source.file].clone(),
            }
            .binding_occurrences(node),
            1,
            "the binding resolves to exactly one authored element"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_created_project_rejects_a_name_that_is_not_a_project() {
        for name in ["NoSuffix", "NotADir.txt"] {
            let path = std::env::temp_dir().join(format!(
                "spool-badname-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            assert!(
                matches!(
                    create(&path),
                    Err(ProjectCreateError::NotAProjectName { .. })
                ),
                "{name} must not become a project"
            );
        }
    }

    #[test]
    fn creating_refuses_to_overwrite_something_that_is_already_there() {
        let root = fresh("occupied");
        std::fs::create_dir_all(&root).expect("make the directory");
        std::fs::write(root.join("work.html"), "someone's work").expect("write");

        assert!(matches!(
            create(&root),
            Err(ProjectCreateError::AlreadyExists { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(root.join("work.html")).expect("read"),
            "someone's work",
            "and the existing file is untouched"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn creating_into_an_empty_directory_is_allowed() {
        // The user may have made the folder first and then pointed Spool at it.
        let root = fresh("empty-dir");
        std::fs::create_dir_all(&root).expect("make the directory");
        create(&root).expect("an empty directory is a location, not a project");
        assert!(open(&root).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn creating_refuses_an_empty_name() {
        assert!(matches!(create(""), Err(ProjectCreateError::NoName)));
    }

    #[test]
    fn a_created_project_is_a_full_peer_of_a_committed_fixture() {
        // The strongest statement available without a file dialog: a project Spool
        // wrote opens, binds, projects and edits exactly as a committed fixture
        // does, because nothing about it is a special case.
        let root = fresh("peer");
        create(&root).expect("creates");
        let loaded = open(&root).expect("opens");
        assert_eq!(loaded.runtime.objects().len(), 1);

        // And a created object survives save and reopen in it, which is the whole
        // product loop and the reason the project exists.
        let mut document = loaded.document.clone();
        document.structure.nodes.push(StructuralNode {
            id: NodeId::new("spool-added").expect("valid"),
            name: "Added".to_owned(),
            kind: "rectangle".to_owned(),
            parent: Some(NodeId::new("spool-frame-root").unwrap()),
            children: Vec::new(),
            source: crate::source_document::SourceBinding {
                file: "index.html".to_owned(),
                selector: "[data-spool-id=\"spool-added\"]".to_owned(),
            },
        });
        crate::project_save::save_project(
            &root,
            &document,
            &[crate::project_save::SourceEdit::Create {
                node: NodeId::new("spool-added").unwrap(),
                element: crate::project_save::NewElement {
                    tag: "div".to_owned(),
                    text: None,
                    declarations: vec![],
                },
            }],
        )
        .expect("a save into a new project succeeds");

        let reopened = open(&root).expect("reopens");
        assert_eq!(reopened.document.structure.nodes.len(), 2);
        assert_eq!(reopened.runtime.objects().len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn saving_a_new_project_with_no_edits_changes_nothing() {
        // The chain the milestone asks for, on a project Spool made rather than
        // one it opened. A fresh save must not reformat, re-encode or otherwise
        // "tidy" a project the user has not touched — the two files are already
        // what the writer would produce, so a save has nothing to do.
        let root = fresh("noop");
        create(&root).expect("creates");
        let before: Vec<(String, String)> = tree_of(&root)
            .into_iter()
            .map(|rel| {
                let bytes = std::fs::read_to_string(root.join(&rel)).expect("read");
                (rel, bytes)
            })
            .collect();

        let loaded = open(&root).expect("opens");
        let outcome =
            crate::project_save::save_project(&loaded.root, &loaded.document, &[]).expect("save");
        assert!(
            outcome.written.is_empty(),
            "a save with nothing to change wrote {:?}",
            outcome.written
        );
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        for (rel, bytes) in before {
            assert_eq!(
                std::fs::read_to_string(root.join(&rel)).expect("read"),
                bytes,
                "{rel} was rewritten by a no-op save"
            );
        }
        // And a second one is still a no-op, measured from the first.
        let reopened = open(&root).expect("reopens");
        let again = crate::project_save::save_project(&reopened.root, &reopened.document, &[])
            .expect("save");
        assert!(again.written.is_empty(), "{:?}", again.written);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every file under `root`, relative and sorted.
    fn tree_of(root: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read dir") {
                let entry = entry.expect("entry");
                if entry.file_type().expect("file type").is_dir() {
                    stack.push(entry.path());
                } else {
                    found.push(
                        entry
                            .path()
                            .strip_prefix(root)
                            .expect("inside the root")
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        found.sort();
        found
    }

    // -- the command line -------------------------------------------------

    fn args(list: &[&str]) -> Vec<String> {
        std::iter::once("Spool".to_owned())
            .chain(list.iter().map(|arg| (*arg).to_owned()))
            .collect()
    }

    #[test]
    fn no_arguments_names_no_project() {
        assert_eq!(
            requested_from_args(args(&[])).expect("an empty command line is valid"),
            None
        );
    }

    #[test]
    fn one_argument_names_that_project() {
        assert_eq!(
            requested_from_args(args(&["/tmp/MyProject.spool"])).expect("one path is valid"),
            Some(PathBuf::from("/tmp/MyProject.spool")),
            "the path is not resolved here, only recorded"
        );
    }

    #[test]
    fn switches_are_not_mistaken_for_projects() {
        assert_eq!(
            requested_from_args(args(&["--some-switch", "/tmp/MyProject.spool"]))
                .expect("a switch and a path is valid"),
            Some(PathBuf::from("/tmp/MyProject.spool")),
        );
    }

    #[test]
    fn two_projects_are_refused_rather_than_one_being_chosen() {
        let error = requested_from_args(args(&["a.spool", "b.spool"]))
            .expect_err("two projects cannot both be opened");
        assert!(matches!(
            error,
            ProjectBoundaryError::TooManyProjects { count: 2 }
        ));
    }

    #[test]
    fn a_named_project_from_the_command_line_opens() {
        // The whole argv path, end to end.
        let named = requested_from_args(args(&[&format!("{FIXTURES}/landing.spool")]))
            .expect("one path")
            .expect("a project was named");
        let loaded = open(named).expect("the canonical fixture opens");
        assert_eq!(loaded.document.structure.nodes.len(), 3);
    }

    #[test]
    fn a_named_project_that_is_not_one_is_refused() {
        let named = requested_from_args(args(&[&format!("{FIXTURES}/landing")]))
            .expect("one path")
            .expect("a path was named");
        assert!(
            open(named).is_err(),
            "naming a bare directory on the command line must not open a blank scene"
        );
    }

    // -- saving -----------------------------------------------------------

    fn node(id: &str) -> crate::source_document::NodeId {
        crate::source_document::NodeId::new(id).expect("valid id")
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("read file")
    }

    /// Every path in the project, relative to `root`, sorted.
    ///
    /// Compared before and after a save to show the save neither added nor lost
    /// a file, which is how "does not flatten the project" is checked rather
    /// than assumed.
    fn tree(root: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read dir") {
                let entry = entry.expect("entry");
                if entry.file_type().expect("file type").is_dir() {
                    stack.push(entry.path());
                } else {
                    found.push(
                        entry
                            .path()
                            .strip_prefix(root)
                            .expect("inside the root")
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn a_save_writes_only_inside_the_project() {
        let root = scratch("save-inside");
        let before = tree(&root);
        let asset = read(&root.join("assets/mark.svg"));
        let loaded = open(&root).expect("opens");

        let mut renamed = loaded.document.clone();
        renamed.structure.nodes[1].name = "Hero headline".into();
        let outcome = crate::project_save::save_project(
            &root,
            &renamed,
            &[crate::project_save::SourceEdit::Text {
                node: node("spool-text-headline"),
                text: "Ships".into(),
            }],
        )
        .expect("save");
        assert!(
            outcome.unsupported.is_empty(),
            "the edit was refused: {:?}",
            outcome.unsupported
        );

        assert!(!outcome.written.is_empty(), "the save wrote something");
        for path in &outcome.written {
            assert!(
                path.starts_with(&root),
                "{} escaped the project root {}",
                path.display(),
                root.display()
            );
        }
        // Nothing added, nothing lost, nothing flattened.
        assert_eq!(tree(&root), before, "the save changed the project's shape");
        // An asset is authored content Spool does not own. A save must leave it
        // exactly as it was, rather than copying it somewhere it manages.
        assert_eq!(read(&root.join("assets/mark.svg")), asset);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_edit_survives_a_save_and_a_reopen() {
        let root = scratch("save-reopen");
        let mut document = open(&root).expect("opens").document;
        document.structure.nodes[1].name = "Hero headline".into();

        crate::project_save::save_project(
            &root,
            &document,
            &[
                crate::project_save::SourceEdit::Text {
                    node: node("spool-text-headline"),
                    text: "Design in source, structure in Spool".into(),
                },
                crate::project_save::SourceEdit::Style {
                    node: node("spool-cta-primary"),
                    property: "border-radius".into(),
                    value: "12px".into(),
                },
            ],
        )
        .expect("save");

        // The authored bytes carry the edit, in the files they were authored in.
        assert!(
            read(&root.join("styles/styles.css")).contains("border-radius: 12px"),
            "the stylesheet the <link> found is the file that was edited"
        );
        assert!(read(&root.join("lamine.yaml")).contains("Hero headline"));

        // And reopening the project reads them back.
        let reopened = open(&root).expect("reopens");
        assert_eq!(reopened.document.structure.nodes[1].name, "Hero headline");
        assert_eq!(
            reopened.runtime.objects().len(),
            3,
            "the project is still a whole project"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_save_with_no_edits_writes_nothing() {
        let root = scratch("save-noop");
        // Compared as bytes, not as text. The question is whether the save
        // changed the file, and bytes answer that for every file in the project
        // — including any that are not text, such as the `.DS_Store` macOS drops
        // into a directory whenever it has been opened in Finder.
        let before: Vec<(String, Vec<u8>)> = tree(&root)
            .into_iter()
            .map(|rel| {
                let bytes = std::fs::read(root.join(&rel)).expect("read file");
                (rel, bytes)
            })
            .collect();

        let loaded = open(&root).expect("opens");
        let outcome =
            crate::project_save::save_project(&loaded.root, &loaded.document, &[]).expect("save");

        assert!(
            outcome.written.is_empty(),
            "an empty save wrote {:?}",
            outcome.written
        );
        for (rel, bytes) in before {
            assert_eq!(
                std::fs::read(root.join(&rel)).expect("read file"),
                bytes,
                "{rel} changed on a no-op save"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_save_is_measured_from_the_last_one() {
        let root = scratch("save-twice");
        let mut first = open(&root).expect("opens").document;
        first.structure.nodes[1].name = "First".into();
        crate::project_save::save_project(
            &root,
            &first,
            &[crate::project_save::SourceEdit::Text {
                node: node("spool-text-headline"),
                text: "One".into(),
            }],
        )
        .expect("first save");

        // Reopen before the second edit. Byte ranges for the second save are
        // resolved against the file as it is *now*; resolving them against the
        // original would splice into text that has moved.
        let mut second = open(&root).expect("reopens").document;
        second.structure.nodes[2].name = "Second".into();
        crate::project_save::save_project(
            &root,
            &second,
            &[crate::project_save::SourceEdit::Text {
                node: node("spool-cta-primary"),
                text: "Two".into(),
            }],
        )
        .expect("second save");

        let html = read(&root.join("pages/index.html"));
        assert!(
            html.contains(">One</h1>"),
            "the first edit survived:\n{html}"
        );
        assert!(html.contains("Two"), "the second edit landed:\n{html}");
        assert!(
            !html.contains("Start designing"),
            "the CTA text was replaced"
        );

        let manifest = read(&root.join("lamine.yaml"));
        assert!(manifest.contains("First") && manifest.contains("Second"));

        let reopened = open(&root).expect("reopens");
        assert_eq!(reopened.document.structure.nodes[1].name, "First");
        assert_eq!(reopened.document.structure.nodes[2].name, "Second");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[test]
#[ignore = "prints the contract for a reader"]
fn show_what_a_new_project_looks_like() {
    // Not an assertion: prints the two files so the contract can be read rather
    // than inferred. Ignored by default.
    // `SPOOL_NEW_PROJECT_DIR` writes it somewhere a real binary can then be asked
    // to open, which is how the packaged app is checked against a project Spool
    // made. Otherwise a throwaway in the temp dir.
    let keep = std::env::var_os("SPOOL_NEW_PROJECT_DIR").is_some();
    let root = std::env::var_os("SPOOL_NEW_PROJECT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("spool-show-{}.spool", std::process::id()))
        });
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&root);
    create(&root).expect("creates");
    println!("--- tree ---");
    for entry in std::fs::read_dir(&root).unwrap() {
        println!(
            "{}",
            entry.unwrap().path().file_name().unwrap().to_string_lossy()
        );
    }
    println!(
        "--- lamine.yaml ---\n{}",
        std::fs::read_to_string(root.join("lamine.yaml")).unwrap()
    );
    println!(
        "--- index.html ---\n{}",
        std::fs::read_to_string(root.join("index.html")).unwrap()
    );
    // Left in place when a caller named a destination, so a real binary can be
    // asked to open exactly what was just written.
    if !keep {
        let _ = std::fs::remove_dir_all(&root);
    }
}
