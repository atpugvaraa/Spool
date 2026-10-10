# Spool

**A local-first design editor for graphics, UI/UX, and branding.** Shape and
refine an interface on a native canvas, keep its authored source, and build
toward AI that understands the project you are editing.

Spool is a student-built project from [Google Developer Groups](https://www.instagram.com/gdg.tiet/) at Thapar
Institute of Engineering and Technology.
It is for people who want the directness of a visual editor without losing the
transparency and portability of the code behind a design.

Your project stays yours: Spool writes supported edits back into the source
files you opened. It is not a browser engine, a hosted design service, or a
Figma reimplementation. Familiar editor interactions help make it approachable; source-backed editing is the product's center.

[Get started](#get-started) · [What works](#what-works-today) · [Roadmap](docs/04-implementation-roadmap.md) · [Contributing](CONTRIBUTING.md)

## Why Spool

### Work on the design you already have

Open a `.spool` project and inspect its structure in the Canvas, Layers, and
Inspector. Make supported changes directly, then save them back into the
project's authored files.

### Keep the source human-readable

HTML, CSS, and SVG remain the authored design. Spool uses `lamine.yaml` for stable Spool identity, hierarchy, and source bindings. It does not turn that metadata into a second store for visual properties.

### Keep editing local

The native editor works with project files on your device. The project can be opened and edited as ordinary source; Spool's runtime is a working representation that can be rebuilt from the project.

### Build toward design-aware assistance

The long-term direction includes private, local AI that can work with the same
structured project as the editor. AI assistance is **not available in the app
today**.

## What works today

- Open and create `.spool` projects.
- Select and multi-select supported objects; create, duplicate, move, resize,
  rename, and delete them.
- Use the native Canvas, Layers, and Inspector, with text editing, alignment
  snapping, canvas navigation, and undo/redo for supported actions.
- Save supported changes to the authored project. Created and duplicated
  objects persist through save and reopen; deleting a container deletes its
  descendants as one undoable action.
- Preserve unrelated authored source when saving supported edits.

These capabilities are still developing. The project tracks the current
milestone, known gaps, and deferred work in the [implementation roadmap](docs/04-implementation-roadmap.md).

## Honest status

Spool is an early-stage editor. Its CSS and layout support is deliberately
bounded: it does not reproduce browser behavior for all flex layouts and `gap`, inline layout, text wrapping, CSS custom properties, descendant selectors,
media queries, `@layer`, or `!important`. Ambiguous or unsupported edits may be refused.

Two current reliability gaps are tracked for the next milestone: some refused
semantic operations need clearer user feedback, and reopening a project with no managed nodes can fall back to the starter scene. See the [roadmap](docs/04-implementation-roadmap.md) for acceptance checks and task status. The roadmap is the changing project-status page; this README focuses on what Spool is and how to try it.

## Under the hood

- **Source-backed:** HTML, CSS, and SVG are the authored implementation.
- **Structural metadata:** `lamine.yaml` supplies Spool IDs, hierarchy, and
  source bindings.
- **Native editor:** Rust and GPUI provide the Canvas, Layers, Inspector, and
  interaction runtime.
- **Semantic history:** persistent user actions use the shared operation and
  undo/redo path.
- **Bounded by design:** unsupported source ownership is surfaced rather than guessed at or silently rewritten.

```text
HTML / CSS / SVG + lamine.yaml
              ↓
     source-backed document
              ↓
       native editor runtime
              ↓
 Canvas · Layers · Inspector
              ↓
 supported edits saved to source
```

Read the [product boundaries](docs/01-product-and-boundaries.md),
[document and source model](docs/02-document-and-source-model.md), and
[editor runtime and history](docs/03-editor-runtime-history.md) for the
architecture contracts.

## Get started

### Run the native app

Install a current stable Rust toolchain on a platform supported by GPUI. From
the repository root:

```sh
cd app
cargo run
```

To open the included source-backed example during development:

```sh
cd app
SPOOL_PROJECT=./fixtures/landing cargo run
```

`SPOOL_PROJECT` is a development override. A real project is a directory named
`*.spool`; open it by passing its path to the app:

```sh
dist/Spool.app/Contents/MacOS/Spool ~/projects/MyProject.spool
```

The first build fetches and compiles GPUI from the pinned Zed source, so it can take a while and needs network access. The [development guide](docs/development/getting-started.md) explains the project format, app setup, packaging, and verification commands.

### Run the website

The website is a separate Next.js project. It requires Bun 1.4.2, pinned in
`website/package.json`:

```sh
cd website
bun install
bun run dev
```

## Contribute

Start with [CONTRIBUTING.md](CONTRIBUTING.md) to find the current milestone, choose a bounded task, set up the app or website, and check your change. The
[implementation roadmap](docs/04-implementation-roadmap.md) is the single live
status summary; assigned issues or tasks carry owners and detailed progress.

## License

This repository does not currently include a license. Until one is added, no
license should be assumed.
