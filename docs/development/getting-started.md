# Development setup

Use this guide to clone Spool and run either the native app or the website.
They are separate projects, so you can set up just the one you want to work
on. For contribution expectations, see [CONTRIBUTING.md](../../CONTRIBUTING.md).

## Clone the repository

```sh
git clone https://github.com/atpugvaraa/Spool.git
cd Spool
```

## Run the native app

Install a current stable Rust toolchain with [rustup](https://rustup.rs/). On
Windows, GPUI also needs Visual Studio Build Tools with the "Desktop development
with C++" workload and a Windows SDK. Then run the app from its Cargo package directory:

```sh
cd app
cargo run
```

With no project configured, Spool opens a blank starter scene. To open the
included source-backed landing-page fixture instead:

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

The first build can take a while: GPUI is fetched from the Zed repository at
the revision pinned in `app/Cargo.lock` and compiled from source. Network
access is needed the first time. Once dependencies are available locally,
`cargo build --offline` can build without network access. Avoid `cargo update`,
which changes dependency revisions.

### Opening your own project during development

Set `SPOOL_PROJECT` to a directory containing a `lamine.yaml` file. Project paths
are resolved relative to that directory. A project typically contains HTML,
stylesheets and the Spool structure file:

```text
my-project/
├── lamine.yaml
├── index.html
└── styles.css
```

This is the development override described under [Projects](#projects); it
accepts any directory holding a `lamine.yaml`, so it does not require the
`.spool` suffix a real project has.

The `app/fixtures/landing.spool`, `app/fixtures/landing`,
`app/fixtures/nested` and `app/fixtures/awkward` directories are working
examples. On startup, Spool reports whether the project opened successfully. If
it fails, the reason is printed and shown in the window; the starter scene stays
on screen but is never presented as the project.

## Projects

A Spool project is a **directory whose name ends in `.spool`**. That is the whole
format. The authored HTML, CSS and SVG stay plain files on disk so they remain
readable, diffable and editable with the author's own tools.

```text
MyProject.spool/
├── lamine.yaml          required — Spool identity, hierarchy, bindings
├── pages/index.html     the source the bindings point at
├── styles/styles.css    found through the document's <link href>
└── assets/mark.svg      referenced by the markup; never parsed as structure
```

What owns what:

- `lamine.yaml` — Spool's identity, hierarchy, source bindings and provenance.
- `*.html`, `*.css`, `*.svg` — the authored design, and the only visual truth.
- the runtime document — derived and disposable; drop it and reopen and you get
  equivalent state.

`lamine.yaml` is versioned (`version: 1`) and read strictly. An unknown version
or an unrecognised field is refused rather than guessed at.

### What the loader requires

- The path is a directory, its name ends in `.spool`, and it holds a `lamine.yaml`
  at its root. Failing any of those means "not a Spool project".
- Every `file` in `lamine.yaml` resolves **inside** the project root, exists, and
  its `selector` matches exactly one element in that file. Zero matches is a
  dangling binding; more than one is ambiguous. Neither is guessed.
- Stylesheets are discovered by following `<link href>` the way a browser would.
  A missing stylesheet leaves the document unstyled rather than failing the open.

The `pages/`, `styles/` and `assets/` names above are what
`app/fixtures/landing.spool` uses, **not** a requirement. Nothing in the code
names them: each binding carries its own project-relative `file`, so a project may
lay itself out however it likes.

### Opening a project

The product's own path is a command-line argument:

```sh
dist/Spool.app/Contents/MacOS/Spool ~/projects/MyProject.spool
```

With no argument, Spool opens its empty starter state. A project that fails to
open is reported on stderr *and* shown in the window — it is never presented as
though the starter scene were the project.

### `SPOOL_PROJECT` is a development override

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

`SPOOL_PROJECT` still works and is still how the committed fixtures and the test
suite are used. It is a **development and testing override, not a product
surface**: it accepts any directory containing a `lamine.yaml` and does not
require the `.spool` suffix, which is how the plain `app/fixtures/landing`,
`app/fixtures/nested` and `app/fixtures/awkward` directories remain usable.
Nothing a user can reach should depend on it. A path given on the command line
takes precedence over it.

`app/fixtures/landing.spool` is the canonical fixture: the shape a user actually
receives. The other three fixtures are internal loader test data.

### Saving

`⌘S` writes back into the same project, in place:

- authored `HTML`/`CSS` are rewritten only where an edit changed them, byte for
  byte elsewhere — same whitespace, attribute order and comments;
- `lamine.yaml` is rewritten only when the structure actually changed;
- a save with nothing to change writes nothing at all;
- a second save is measured from the file as it is *now*, so consecutive saves
  compose instead of fighting.

Nothing is flattened, relocated, or copied into a second store.

### Verifying the project contract

```sh
cd app
cargo test --offline spool_project::   # the .spool contract
./mutate_spool_project.sh              # proves those tests can fail
```

`mutate_spool_project.sh` breaks one boundary rule at a time and requires a test
to notice. Three rules inside `lifecycle::install` are reported separately as
`RTIME`: they need a live `App`, so they are covered by running the packaged
application rather than by a unit test.

## Package a macOS app bundle

`cargo run` is for development. To produce the distributable macOS artifact,
run the packaging script from the repository root:

```sh
app/scripts/package-macos.sh
```

It builds with `cargo build --release`, assembles `dist/Spool.app`, and writes
`dist/Spool-macos-arm64.zip`. `Spool.app` runs on its own: it needs no Cargo,
Rust toolchain, or development environment, only a copy of the project the user
opens.

The script uses only tools that ship with macOS. Application identity comes
from `app/resources/Info.plist.in`, and the version comes from the `[package]`
version in `app/Cargo.toml`, so there is one source of truth. The icon is
generated at package time from `app/resources/icon-1024.png`.

The bundle is signed ad-hoc, which seals it for local use. It is not signed
with a Developer ID certificate and is not notarized, so macOS Gatekeeper will
still warn on first launch when it is opened from another machine.

## Run the website

The website requires Bun 1.4.2, specified by `packageManager` in
`website/package.json`. Install Bun from [bun.sh](https://bun.sh/), then:

```sh
cd website
bun install
bun run dev
```

Open the local address printed by the development server. The website has its
own contributor instructions in [`website/README.md`](../../website/README.md)
and [`website/AGENTS.md`](../../website/AGENTS.md).

## Verify a change

Run the checks for the project you changed. From `app/`:

```sh
cargo fmt --all -- --check
cargo test
cargo check
cargo clippy --all-targets
```

From `website/`:

```sh
bun run lint
bun run build
```

## If something goes wrong

- **The first app build is slow:** expected; GPUI and its dependencies compile
  from source. Later builds use Cargo's incremental cache.
- **Cargo cannot fetch Zed:** the initial dependency fetch needs network access.
  After it succeeds, Cargo can use its local cache, including for offline
  builds.
- **A project does not open:** check that `SPOOL_PROJECT` points to the
  directory containing `lamine.yaml`, and look for the startup error in the
  terminal.
- **A check fails:** rerun the exact command and include the relevant output
  when asking for help or opening an issue.

## Further reading

- [Contributing to Spool](../../CONTRIBUTING.md)
- [Product and architecture boundaries](../01-product-and-boundaries.md)
- [Implementation roadmap](../04-implementation-roadmap.md)
