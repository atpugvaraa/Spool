# Porting figcraft's UI to GPUI

This is the working agreement for every UI-parity slice. It exists because
three agents in a row opened by re-deriving the same map: where Spool's panels
are built, what the engine already supports, and which figcraft file is the
reference for a given panel.

**Read this first. Then work.** If something here is wrong or missing, fix
this file in the same commit as the slice that found it wrong.

---

## 1. What is being ported, and from where

figcraft's frontend is `vendor/figcraft/crates/ui-egui/` — 52,449 lines over
100 files, written in egui. Spool's frontend is GPUI. The frontend cannot be
copied; the **behaviour** must be 1:1.

Two things are therefore in scope for every slice:

- **The reference**: the egui file that implements the panel. It is the
  specification. Read it and mirror its rows, its order, its labels, its
  defaults, its empty state and its enable/disable behaviour.
- **The engine**: `vendor/figcraft/crates/engine/`. The model and the commands
  already exist. A slice is a **UI port**, not an engine change. If a value
  cannot be expressed by the engine, the panel must say so rather than invent
  something.

`vendor/**` is FROZEN. Never edit it. If the engine cannot do what figcraft's
UI does, stop and report it.

---

## 2. The engine's command contract

Everything the editor does is a **command id** through
`Session::execute(id, &params)`. There is no other write path, and there never
will be: it is what keeps the UI, the CLI, the MCP server and a future agent
on one operation surface.

The commands are plain data. Read them with:

```sh
cargo run -q -p spool-cli -- list-commands --filter <substr> --json
```

The two that matter for the Inspector:

| command | what it sets |
|---|---|
| `node.set` | `props` on a selection — geometry, style, layout, effects, exports, text |
| `layout.add` / `layout.remove` | adding and removing a frame's auto layout |

`node.set`'s full prop list is in
`vendor/figcraft/crates/engine/src/cmd/props.rs` — that file is the contract.
**Never guess a prop name or a value spelling: read it there.**

The engine's model is in `vendor/figcraft/crates/doc/src/`:

| what | where |
|---|---|
| `Node` | `node.rs` |
| `ChildLayout`, `Layout`, `Effect`, `ExportSetting`, `Stroke`, `Corners` | `props.rs` |
| components, instances | `library.rs` |

---

## 3. Spool's Inspector: the conventions

`app/src/inspector.rs` is the Design panel. It is the ONLY place a panel is
rendered, and every slice must match what is already there.

### The shape of a section

```rust
fn inspector_section(
    &self,
    title: &str,
    open: bool,
    cx: &mut Context<Self>,
) -> impl IntoElement
```

Sections are collapsible, with a header that may carry one action
(`inspector_section_with` — that is how "Add auto layout" is a header button,
as in figcraft). A header click must call `cx.stop_propagation()` so it does
not also collapse the section.

### The shape of a value

Every value in the panel is a `Property`. A property knows:

- how to **read** it from the model (`Property::read`),
- how to **write** it (`Property::style_edit`, `Property::box_command`,
  `Property::layout_edit`),
- whether it applies to the current selection (`Property::availability`).

A new kind of value adds a kind to `Property` and a commit path
(`commit_number`, `commit_style`, `commit_box`, and now `commit_layout`).
**Do not invent a second number widget** — a new number uses the Inspector's
existing field, with its typing grammar, its focus rules and its commit path.

### The three states — every property, always

| state | shown as |
|---|---|
| a value | the value |
| the selection disagrees | **"Mixed"** |
| unset | **"Not set"** (figcraft's word — or **"Auto"** where the flow makes the gap up) |

Fold **per property**. figcraft folds the whole struct, so one difference
hides the entire block; Spool folds per property, and the existing properties
already do this. `Availability` is `Supported` or `Disabled(&'static str)` —
and a disabled control says **why, out loud**, in the reason string.

### Reading and writing

`app/src/engine/inspector_model.rs` is the model: it turns the engine's
`Document` and a `NodeId` into what the panel shows, and builds the command
params for a write. The vocabulary lives there — the engine's exact prop names
and value spellings — so the panel never spells an engine value itself.

**A slice that needs new model state adds it to the model.** If two slices
need the same model, they need the same file — so a slice that owns a new
model file owns it alone, and the Inspector is wired to it afterwards.

---

## 4. The slice protocol

Each slice is **one stacked PR** and **one file**. Not one file per agent —
one file per slice, and the file is **new**, so slices run in parallel with no
merge conflict.

1. **Create the module file first** and add its `pub mod` line to
   `app/src/engine/mod.rs`. Do this before dispatching parallel slices, so two
   slices never touch `mod.rs`.
2. **Put everything in that one file**: the model, the panel, and its
   `#[cfg(test)] mod tests`.
3. **Do not edit `app/src/inspector.rs`.** The Inspector is wired afterwards,
   in one pass, by whoever owns it. A slice reports the exact snippet it
   needs: the call, and the import line.
4. **Do not edit `app/src/engine/mod.rs`** once the module is declared.
5. **Do not edit `app/src/engine/inspector_model.rs`** if another slice owns
   it. Put your model in your own file.

### Proof obligations

Every slice must show, in its report:

- **Fail-before/pass-after** for each new test. A test that passes both ways
  proves nothing — say explicitly that you confirmed it fails first. To see
  it fail, revert your change and run it.
- **A real window test.** The panel must be renderable on its own, so it can
  be put in a real GPUI window and asserted on. `app/src/interaction_window_tests.rs`
  has the patterns; follow its approach, but put your test in your own file.
- **The real numbers** from `cargo build`, `cargo test`, `cargo clippy` and
  `cargo fmt --check`. If another agent is working in the same tree, the suite
  moves under you — report **your** file's numbers specifically and say what
  else was in flight.
- **What the app looked like.** Run it and look. If you cannot capture a
  screenshot reliably, say so rather than claiming you looked.

---

## 5. Use the graph, not grep

`graphify-out/graph.json` is a code knowledge graph over this repository,
built with tree-sitter — 19,288 nodes and 59,588 edges over 807 files, which
includes all 204,097 lines of vendored figcraft. Every edge is tagged
`EXTRACTED` (the parser found it), `INFERRED` (graphify resolved it) or
`AMBIGUOUS`.

```sh
graphify query "what calls the export panel" --graph graphify-out/graph.json
graphify path "vendor/figcraft/crates/doc/src/props.rs::Effect" \
                "vendor/figcraft/crates/ui-egui/src/right.rs" --graph graphify-out/graph.json
graphify explain "AppShell" --graph graphify-out/graph.json
```

`path` follows the real call and import edges and prints the hops, which is
how you find the figcraft file that implements a panel without reading your
way through the tree. Rebuild after commits with `graphify update .`.

The same graph is also served over **MCP**, as `graphify`, with the graph
tools as first-class tools rather than a command to remember. It is registered
in `~/.config/opencode/opencode.jsonc`; the CLI works whether or not the MCP
server is up.

**What the graph is good for:** where something is, what calls it, what it
touches, whether anything uses it, and how figcraft's model reaches its UI.

**What the graph cannot tell you:** conventions. It will find
`inspector.rs::Sections` and `inspector_model.rs::number()`; it cannot tell
you that a new number field must be typed rather than scrubbed, or that a
mixed value folds per property. **That is what this document is for.**

---

## 6. The panels, and where each one comes from

| panel | figcraft reference | engine support | Spool module |
|---|---|---|---|
| Canvas + overlays | `ui-egui/canvas.rs` | `figcraft_render` | `engine/canvas.rs` ✅ |
| Layers | `ui-egui/layers.rs` | `figcraft_engine` | `engine/layers.rs` ✅ |
| Design panel | `ui-egui/right.rs` | — | `inspector.rs` ✅ |
| Auto layout | `ui-egui/layout_panel.rs` | `node.set` layout props | `engine/panel_export.rs`… see below ✅ |
| Export | `ui-egui/export_panel.rs` | `export.add/update/remove` | `engine/panel_export.rs` |
| Effects | `ui-egui/right.rs` §Effects | `Node.effects`, `Effect::TYPES` | `engine/panel_effects.rs` |
| Components | `ui-egui/component_panel.rs` | `library.rs`, `components.rs` | `engine/panel_components.rs` |
| Typography | `ui-egui/typography.rs` | `node.set` text props | — |
| Colour picker | `ui-egui/colorpicker.rs` | `figcraft_color` | — |
| Style picker | `ui-egui/style_picker.rs` | `Document::styles` | — |
| Assets | `ui-egui/assets_panel.rs` | `Document::images` | — |
| Variables | `ui-egui/variables_panel.rs` | `Document::variables` | — |
| Prototyping | `ui-egui/proto_panel.rs` | `interactions`, `flow` | — |
| Dev Mode | `ui-egui/dev_mode.rs` | `figcraft_engine` inspect | — |
| Comments | `ui-egui/comments.rs` | `Document::comments` | — |
| A11y | `ui-egui/a11y.rs` | engine a11y keys | — |
| Home | `ui-egui/home*.rs` | — | — |

✅ = done.

---

## 7. The governing documents

- `docs/01-product-and-boundaries.md` — what Spool is
- `docs/02-document-and-source-model.md` — **the ownership rule**: a property
  has exactly one owner. A = authored CSS, B = `lamine.yaml`, C = derived,
  D = session. A property with two owners is a defect.
- `docs/03-editor-runtime-history.md` — the history is the engine's `Arc`
  snapshots plus a semantic intent journal
- `docs/04-implementation-roadmap.md` — the roadmap
- `docs/05-figcraft-adoption-audit.md` — what was adopted from figcraft

**If the implementation contradicts one of these, stop and report the
conflict. Do not change the document to match the code.**

### The one that bites porting a panel

A panel may show a value the **source cannot express**. Spool is
source-backed: HTML and CSS are the authored source and `lamine.yaml` holds
the structure. If figcraft's panel edits something Spool's source format has
no spelling for, then either

- the source format gains a spelling for it (a change to the contract, and a
  decision for a human), or
- the panel shows the value read-only, and says why it cannot be edited.

**Do not invent a source format, and do not edit a value the save path cannot
write back.** That would break the round trip, which is the one thing this
architecture forbids.

---

## 8. Before you start

1. Read this file.
2. `graphify query` / `graphify path` for the thing you need to find.
3. Read the figcraft reference **for your panel only** — not the whole tree.
4. Read the model, not the whole Inspector, unless you are changing it.
5. Write the test first, watch it fail, then make it pass.
