# Contributing to Spool

Thanks for wanting to work on Spool. We’re building it together as students,
and there are useful ways to contribute whether you’re interested in Rust,
design tools, web development, testing or writing. You don’t need to understand
the whole project to get started. Pick one small problem, run the part you’re
changing, and ask when something is unclear.

## Choose what you want to work on

The repository has two distinct projects:

- **`app/`** is the native desktop editor, written in Rust with GPUI. It owns
  the canvas, document editing, layers and inspector.
- **`website/`** is the public website, built with Next.js, React and
  TypeScript. It has its own setup instructions in `website/README.md` and
  `website/AGENTS.md`.

You can contribute to either one. Keep a change focused on the part you chose;
if a task needs to cross the boundary, explain why in the pull request.

## Find a first task

Start with an assigned issue or a concrete problem you’ve noticed. Check the
[implementation roadmap](docs/04-implementation-roadmap.md) for the current
milestone and its acceptance checks; use issues or assigned tasks for owners,
discussion, and detailed progress. Roadmap bullets are priorities, not blanket
authorization to expand scope. Good first changes are small enough to
understand and review: fix a clear bug, improve one existing interaction, add a
focused test, or clarify a page of documentation. If you have an idea but
aren’t sure where it belongs, open an issue to discuss the scope before
building a larger change.

The README introduces Spool and its stable entry points. Routine feature status
belongs in the roadmap, so ordinary implementation work should not need a
README update. Update the roadmap when milestone status or priorities change;
update architecture docs only when an underlying decision changes.

The [roadmap](docs/04-implementation-roadmap.md) shows current project status,
the proposed next milestone, and candidate tasks. The architecture contracts
are in [Product and Boundaries](docs/01-product-and-boundaries.md),
[Document and Source Model](docs/02-document-and-source-model.md), and
[Editor Runtime and History](docs/03-editor-runtime-history.md). Prior
investigation is in [`docs/research/`](docs/research/README.md); treat it as
background evidence, not as the live task list.

## Set up your project

For the native app, install stable Rust and follow the
[development setup guide](docs/development/getting-started.md). From `app/`,
`cargo run` starts the editor and `cargo test` runs its test suite.

For the website, install Bun 1.4.2 and run these commands from `website/`:

```sh
bun install
bun run dev
```

The development server prints its local address. The
[setup guide](docs/development/getting-started.md) includes app and website
verification commands and common first-build notes.

## A few architecture rules to know

You don’t need to memorize the architecture before contributing, but these
boundaries matter when changing the app:

- HTML, CSS and SVG are the authored design. Keep edits in those files when
  changing authored content or appearance.
- `lamine.yaml` supplies Spool-specific identity, hierarchy and source
  bindings. It must not become a duplicate store for HTML or CSS values.
- Runtime layout, rendering and interaction state are derived from the project.
  Selection, camera and gesture state do not belong in saved project data.
- Persistent edits go through the semantic operation path so they can be
  validated and undone. Don’t add a second way to mutate the document.
- Preserve authored source outside the part an edit is meant to change. If an
  edit is ambiguous or unsupported, surface that limitation instead of
  rewriting unrelated source.

For the full contracts, see [Product and Boundaries](docs/01-product-and-boundaries.md),
[Document and Source Model](docs/02-document-and-source-model.md), and
[Editor Runtime and History](docs/03-editor-runtime-history.md).

## Make and check a change

Before editing, read the relevant module’s opening comment and trace the
existing path your change should follow. In the app, add focused tests for
behavior you change and run the checks for the affected project.

From `app/`:

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

If a check cannot run on your machine, say which one and why in the pull
request. Don’t report a check as passing unless you ran it.

### UI and interaction work

Make interactions feel predictable and consistent with the editor. Consider
mouse, keyboard and trackpad use where relevant, and preserve clear focus and
selection feedback. Keep transient previews separate from committed document
edits; a cancelled gesture should not create an undo step. For website changes,
follow the established direction and instructions in `website/AGENTS.md`.

## Using AI while contributing

AI tools can help you explore unfamiliar code, discuss an approach or draft a
small change. You are responsible for understanding and checking anything they
produce. Verify file paths and project conventions, inspect the full diff, and
run the relevant checks. Don’t include private information or credentials in
prompts, and don’t let a tool make broad changes outside the task.

## Pull requests and review

Keep each pull request focused. In its description, explain the problem, the
change, how you checked it, and any known limitation. Include screenshots or a
short recording when they help reviewers understand a visual or interaction
change. Link the issue or milestone task when there is one, and state which
acceptance check the change satisfies. Report commands and results you actually
ran; distinguish those from checks reported by another contributor or agent.

Review is a conversation about correctness, clarity and fit with the project’s
direction. Please respond to questions and update the pull request when a
change is agreed. If you disagree with a suggestion, explain your reasoning;
the goal is to reach a result we can maintain together.

## Need a hand?

Open a GitHub issue with what you were trying to do, what you expected, and
what happened instead. Include the command or steps that reproduce it and any
relevant error output. For a larger idea, describe the problem first so we can
agree on a useful direction before implementation.
