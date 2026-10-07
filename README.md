# Spool

**A local-first design tool that makes graphics design, UI/UX and branding easier and quicker to prototype on your devices—even without an internet connection.**


Spool is a student-built, open-source design tool from [Google Developer Groups](https://www.instagram.com/gdg.tiet/)
at Thapar Institute of Engineering and Technology. Open an existing interface,
inspect its structure, select elements on a native canvas, make supported
visual edits, and save those changes back into the project's authored source.

Your project stays yours. HTML, CSS and SVG are the authored design. Spool uses
`lamine.yaml` for Spool-specific identity, hierarchy and source bindings; it
does not duplicate the project's visual properties. The editor is early and
actively being built. Private, local AI is a long-term vision, not a feature
available in the app today.

## What works today

- Open an existing `.spool` project — an `HTML`/`CSS` project with its
  `lamine.yaml` metadata — and edit it through the native Canvas, Layers and
  Inspector.
- Select and multi-select objects, move and resize them, duplicate or delete
  them in the current session, rename layers, edit text and change supported
  visual properties.
- Use alignment snapping, canvas navigation, and undo/redo for supported edits.
- Save supported changes back to their owning source files. The audit verified
  minimal authored diffs, correct CSS ownership, no-op saves that write
  nothing, and repeated save/reopen/edit cycles that preserve the project.

Spool's CSS and layout model is intentionally bounded. It supports a subset of
authored styles and layout behavior; it is not a browser engine. For example,
flex layout and `gap`, inline layout, text wrapping, CSS custom properties as
first-class values, descendant selectors, media queries, `@layer`, and
`!important` are not fully modelled. Unsupported edits may be refused on save
with a diagnostic. Some current interaction and Inspector inconsistencies are
also documented in the [implementation roadmap](docs/04-implementation-roadmap.md).

## Current limitations

- **Created-object persistence is not implemented.** Duplicated or otherwise
  created objects can exist during an editing session, but saving them to the
  authored project is not yet supported.
- **CSS and layout coverage is limited.** Spool does not yet reproduce all
  browser layout or cascade behavior. See the support boundary above and the
  [roadmap](docs/04-implementation-roadmap.md).
- **Some values and interactions need clearer feedback.** The audit found
  cases where opening an invalid project can fail without an on-screen error,
  the Inspector can show a display placeholder as though it were authored, and
  selection behavior differs between Canvas and Layers. These are known gaps,
  not claims of completed behavior.
- **AI is a future direction.** The current editor does not include AI agents
  or local AI features.

## Try it

### Native app

Install a current stable Rust toolchain and use a platform supported by GPUI.
From the repository root:

```sh
# for debug mode
cd app
cargo run
```

```sh
# for release build
cd app
cargo build --release --locked
./target/release/Spool
```

To open the included source-backed example:

```sh
# macOS / Linux
cd app
SPOOL_PROJECT=./fixtures/landing cargo run
```

```powershell
# Windows PowerShell
cd app
$env:SPOOL_PROJECT = ".\fixtures\landing"; cargo run
```

```bat
:: Windows cmd
cd app
set "SPOOL_PROJECT=.\fixtures\landing" && cargo run
```

`SPOOL_PROJECT` is a development override. A real project is a directory named
`*.spool`, and the application opens one from its command line:

```sh
dist/Spool.app/Contents/MacOS/Spool ~/projects/MyProject.spool
```

A `.spool` project is a directory containing a `lamine.yaml` manifest beside the
authored HTML, CSS and SVG — plain files that stay readable and editable in
place. See [app setup guide](docs/development/getting-started.md) for the exact
contract.

The first build downloads and compiles GPUI from the pinned Zed source, so it
can take a while and needs network access. See the
[app setup guide](docs/development/getting-started.md) for details.

### Website

The website is a separate Next.js project. It requires Bun 1.4.2, pinned in
`website/package.json`:

```sh
cd website
bun install
bun run dev
```

## Architecture and how it works

Spool starts from the project you open. HTML, CSS and SVG remain the authored
source; `lamine.yaml` supplies the stable identity, hierarchy and source
bindings Spool needs to connect those files to editor objects. The runtime is
an editing and rendering representation that can be reconstructed from the
project. When you make a supported change, Spool routes it back to the
appropriate source file while preserving unrelated authored content.

```text
Existing HTML / CSS / SVG + lamine.yaml
                  ↓
       Spool's native editor runtime
                  ↓
       Canvas, Layers and Inspector
                  ↓
     supported edits written to source
                  ↓
       save, close and reopen project
```

Read more about the boundaries and implementation in
[Product and Boundaries](docs/01-product-and-boundaries.md),
[Document and Source Model](docs/02-document-and-source-model.md), and
[Editor Runtime and History](docs/03-editor-runtime-history.md).

## Where we're going

The next major milestone is **created-object persistence**: making objects
created or duplicated in the editor persist correctly in the HTML/CSS/SVG
project and its metadata. This extends Spool's central promise: visual edits
operate on the project itself and survive saving and reopening.

Beyond that, the project will continue to expand its supported CSS and layout
model and improve feedback around unsupported or synthesized values. Private,
local AI remains a longer-term direction for working with structured projects,
not part of today's feature set. See the
[implementation roadmap](docs/04-implementation-roadmap.md) for current
priorities.

## Contributing

Spool is built by students, and contributions to both the native app and
website are welcome. Start with [CONTRIBUTING.md](CONTRIBUTING.md), then use
the [development setup guide](docs/development/getting-started.md) to run the
part you want to work on.

## Further docs

- [Implementation roadmap](docs/04-implementation-roadmap.md)
- [Product and Boundaries](docs/01-product-and-boundaries.md)
- [Document and Source Model](docs/02-document-and-source-model.md)
- [Editor Runtime and History](docs/03-editor-runtime-history.md)
- [Research notes](docs/research/README.md)
- [App development setup](docs/development/getting-started.md)

## License

This repository does not currently include a license. Until one is added, no
license should be assumed.
