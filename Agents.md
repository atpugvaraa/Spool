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

## `vendor/figcraft/**` — never modify

`vendor/figcraft` is a verbatim copy of the upstream figcraft engine
(`storytold/figcraft`, MIT OR Apache-2.0), recorded in
`vendor/figcraft/UPSTREAM.md` with its exact revision. **Do not edit, reformat,
or "fix" anything under it** — not to satisfy a lint, not to rename a field, not
to remove an `allow`, not to add a feature flag.

It exists so that `git subtree pull` produces a reviewable diff. An edit inside
it is a permanent merge conflict against every future upstream sync.

If the engine genuinely lacks something Spool needs, the change goes in a
Spool-owned crate that depends on it — an extension trait, an adapter, a wrapper
— or upstream, to be vendored when it lands there. If neither is possible, stop
and report it rather than patching the copy.

There are exactly two sanctioned local modifications, both documented in
`UPSTREAM.md` and both mechanical: upstream's root `Cargo.toml` is renamed
because two workspace roots cannot nest, and `plugins/` is removed because it is
a separate wasm-only workspace.

## Project layout

Spool is a Cargo workspace rooted at `Cargo.toml`: `app/` holds the GPUI
application and `vendor/figcraft/` holds the upstream engine.

`app/` is not moving to `crates/`. Doing so would churn the protected paths
above, break the `dist/` packaging scripts and the `mutate_*.sh` harnesses, and
buy nothing.

The workspace compiles the **engine side** of figcraft only. `crates/ui-egui` is
deliberately excluded: it is the egui/eframe frontend, and Spool's frontend is
GPUI. `crates/collab-net` is excluded until collaboration is built. Adding
either to the workspace members is a deliberate act, not a cleanup.

## Source and metadata rules

Never duplicate canonical HTML/CSS/SVG data into `lamine.yaml` for convenience.
Do not put computed CSS properties or full HTML trees into `lamine.yaml` as
authoritative state.

`lamine.yaml` holds only what Spool needs for: stable semantic identity,
hierarchy, frame/group/mask/component semantics, structural relationships, source
bindings, provenance, and Spool-specific metadata.

Authored source remains authoritative for authored implementation.

**Which owner a property belongs to is decided in
`docs/02-document-and-source-model.md`, and it is not a matter of taste.** Every
persisted property is authored in HTML/CSS/SVG, is a Spool-specific fact in
`lamine.yaml`, is derived, or is session state. The evidence behind each call
is in `docs/research/document/property-ownership.md`. Assigning a property to
two owners is a defect. If you find yourself wanting an authored property in
`lamine.yaml` because writing it to CSS is awkward, that is the signal the
ownership decision was wrong — report it rather than routing around it.

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

### Assigned on 2026-10-17 — these four are now in scope

The list above was written when Spool was building its own engine from scratch.
The following are now **explicitly assigned** and are no longer gated on a new
request. Everything not named here is still deferred.

- **Core design parity** with the vendored engine: layers, frames, auto-layout,
  components, variables, styles, vector editing, text, prototype, effects,
  masks, import and export, Dev Mode.
- **CLI and the command surface.** Every registered command runnable from a
  terminal, plus a JSON control channel over a local port.
- **MCP server.** The agent-facing surface. This is the concrete form Spool's
  long-term "local AI that understands the project" direction takes.
- **Collaboration and the server.** Inherited from upstream rather than written.

Still deferred, despite figcraft shipping them: the WebAssembly plugin sandbox,
the browser/wasm build, and the collaboration server app. Do not start them.

Note the tension worth naming: "do not implement a layout engine" is satisfied by
adopting one, not by writing one. The rule's purpose is to stop Spool from
growing a parallel system. Vendoring the engine is the opposite move.

## Agent image limit

Inspect no more than 3 images unless an image is genuinely essential to the
assigned task.

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
