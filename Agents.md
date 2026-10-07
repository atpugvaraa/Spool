# Agents.md

Instructions for coding and research agents working in this repository.

Read this before making changes. It defines where you may write, what you must
not touch, and what you must report.

## Project principles

Spool is a Rust + GPUI design editor. Five layers, kept separate:

| Layer | What it is |
|---|---|
| Authored source | HTML / CSS / SVG — the canonical authored implementation |
| Spool structure/metadata | `lamine.yaml` — identity, hierarchy, bindings, provenance |
| Persistent document | The source-backed semantic document |
| Editor runtime | Selection, camera, tools, interaction state |
| Renderer | Disposable runtime representation |

Preserve these boundaries. The Rust/GPUI runtime is a disposable, optimized
representation derived from persistent document and source state — never an
authority. `lamine.yaml` is **not** a duplicate datastore for HTML/CSS
properties.

## Default write scope

Unless a task explicitly says otherwise, you may modify files directly related
to the task:

- `app/src/`
- `app/tests/` if present
- `tests/` if present
- `docs/04-implementation-roadmap.md`, only when the task explicitly requires roadmap updates
- Narrowly relevant `Cargo.toml` / `Cargo.lock` changes
- Project-level configuration the task requires

State your intended file scope before broad edits. Prefer a focused new module
over modifying unrelated existing modules.

## Protected — do not modify by default

Read as architectural context; never treat as implementation scratchpads:

- `docs/01-product-and-boundaries.md`
- `docs/02-document-and-source-model.md`
- `docs/03-editor-runtime-history.md`
- `docs/04-implementation-roadmap.md`
- `docs/research/**`

Do not rewrite or "update" an architecture decision because it is inconvenient
to implement. If an implementation contradicts one of these documents, **stop and
report the conflict** rather than silently changing the document.

## Research context

You may read `docs/research/**` to understand prior decisions. Do not reopen
broad research when the governing documents already answer the question.

Revisit research only when an explicit implementation gate fails, an assumption
is contradicted by implementation evidence, or the task requests new research.

## Other protected files

Unless directly required by the assigned task, do not modify:

- `app/src/main.rs`
- `app/src/shell.rs`
- `app/src/canvas.rs`

These are high-coupling. If a change is required: inspect the existing
implementation first, make the smallest possible change, preserve existing
behavior, and explain the reason in your final report. No opportunistic refactors.

## Source and metadata rules

Never duplicate canonical HTML/CSS/SVG data into `lamine.yaml` for convenience.
Do not put computed CSS properties or full HTML trees into `lamine.yaml` as
authoritative state.

`lamine.yaml` holds only what Spool needs for: stable semantic identity,
hierarchy, frame/group/mask/component semantics, structural relationships, source
bindings, provenance, and Spool-specific metadata.

Authored source remains authoritative for authored implementation.

## Runtime rules

Do not serialize editor runtime state into the persistent document unless
explicitly required. Selection, camera, pointer, drag, hover, caret, and
snapping previews are runtime concerns.

Do not create a second persistent geometry/style database to make the current
prototype easier to implement.

## History and operations

User actions, AI actions, plugins, importers, and future automation should
converge through semantic document operations. Do not create independent
mutation systems per caller.

Do not copy tldraw's history architecture directly. Preserve the project's
established eager semantic history direction. Cancelled interactions must not
create committed history entries.

## No premature systems

Unless explicitly assigned, do not implement:

- A general CSS cascade engine
- A browser or layout engine
- A new rendering architecture
- A WebView-based editor
- Million-object optimization
- Collaboration
- MCP infrastructure
- AI agent runtime
- Plugin architecture
- Code-generation architecture
- Export architecture
- A second document database

Do not solve future problems before the current implementation gate requires them.

## Agent image limit

Inspect no more than 3 images unless an image is genuinely essential to the
assigned task.

## Platform-specific tests

Tests must pass on Windows, macOS and Linux, and no CI runs them, so check by
hand. A test that only holds on one platform must say so with a gate, not fail
elsewhere.

- Unix-only APIs (`PermissionsExt`, symlinks, file modes): gate the `use` and the
  test with `#[cfg(unix)]`.
- Platform-dependent output (GPUI `Keystroke::unparse` renders the platform
  modifier as `cmd-`, `win-` or `super-`): normalize in the test, or gate with
  `#[cfg(target_os = "...")]`.
- Run `cargo test` on the platform you are on and name the platforms you did not
  run in the PR description.

## Git rules

Preserve unrelated user work. Before editing:

```sh
git status --short --branch
```

Before committing:

```sh
git diff --stat
git diff --check
cargo test --offline
cargo check --offline
```

where applicable.

Do not use `git reset --hard`, `git clean`, `git push --force`, or
`git push --force-with-lease` without explicit user authorization.

Prefer small, coherent commits that represent logical implementation slices. Do
not create giant "update everything" commits. Do not modify unrelated files
merely to make the working tree look cleaner.

## Final report

Every implementation agent must report:

- Files modified
- Files created
- Tests run and their results
- Architectural decisions made
- Any deviations from the governing docs
- Remaining limitations
- Whether unrelated files were touched

The goal is not maximum code output. The goal is incremental, reviewable,
source-backed implementation that preserves the existing editor.
