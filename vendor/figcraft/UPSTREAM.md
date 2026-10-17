# figcraft — vendored upstream

This directory is an **unmodified import of [storytold/figcraft](https://github.com/storytold/figcraft)**,
the open-source Rust Figma-class design editor that Spool adopts as its engine.

| | |
|---|---|
| Upstream | `https://github.com/storytold/figcraft` |
| Imported revision | `9e28e119548745adf9dbf09e96588ce79a9d83d9` |
| Upstream commit date | 2026-10-17 |
| Upstream commit subject | `M3: number variables in the text content picker (#629) (#725)` |
| Upstream version | `0.6.0` |
| License | MIT **OR** Apache-2.0 (dual) |

## Why it is here

Figcraft is roughly 216,000 lines of Rust and already implements a complete
Figma-class design editor: a persistent document model, auto-layout, text
shaping, a CPU renderer, SVG/PDF/PNG/JPG/WebP import and export, `.fig` and
Figma-REST import, components, variables, prototyping, Dev Mode, and a
555-command registry that every surface (UI, CLI, control channel, MCP server)
dispatches through.

Rebuilding that would take Spool years. Adopting it gives Spool the engine
immediately, and lets Spool spend its effort on what is actually distinctive:
**source-backed editing** — authored HTML/CSS/SVG plus `lamine.yaml`.

## Why only part of it is compiled

Spool's frontend is **GPUI**, not egui. Figcraft's layering is enforced upstream
by `cargo xtask layers`: nothing below the UI layer depends on egui, eframe,
winit or rfd. Spool therefore compiles the **engine side only**:

```text
figcraft-geom      figcraft-color      figcraft-doc       figcraft-layout
figcraft-text      figcraft-pathops    figcraft-render    figcraft-format
figcraft-svg       figcraft-pdf        figcraft-collab    figcraft-plugin
figcraft-engine
```

Deliberately **not** compiled by Spool:

| Directory | Reason |
|---|---|
| `crates/ui-egui` | egui/eframe frontend. Not reusable on GPUI; its egui-free logic modules (`field_math`, `i18n`, `bidi`, `cjk`, `theme`) are the reference while porting the panels. |
| `apps/figcraft` | eframe + wgpu desktop host. |
| `apps/figcraft-web` | WASM build. Out of current scope. |
| `apps/figcraft-server` | Collaboration server. Out of current scope. |
| `xtask` | Upstream release/CI tooling. Spool has its own. |

The directories are kept intact rather than deleted, so that
`git subtree pull` produces a clean diff and so upstream reference material
(especially `docs/*-parity.md` and `docs/gaps.md`) stays available.

## Local modifications

Exactly one, and it is mechanical:

- `Cargo.toml` renamed to `Cargo.toml.figcraft-upstream`. Figcraft's root manifest
  declares its own workspace; two workspace roots cannot nest. Spool's root
  manifest lists the vendored crates as members instead.
- `plugins/` removed. It is a separate Cargo workspace (wasm32-only plugin SDK
  examples) and is excluded from Figcraft's own build.

**No vendored Rust source has been modified.** Spool's changes to the engine
layer are additive and live in Spool-owned crates that depend on these.

## Updating

```sh
# from the Spool repository root
git subtree pull --prefix vendor/figcraft figcraft <new-sha>
```

Re-apply the two mechanical modifications above if the new revision changed
`Cargo.toml` or reintroduced `plugins/`, then verify:

```sh
cargo check -p figcraft-engine --all-targets
cargo test -p figcraft-doc -p figcraft-engine
```

## Licensing

Figcraft is `MIT OR Apache-2.0`, the same license Spool releases under. Spool
inherits whichever option is chosen.

- `LICENSE-MIT` and `LICENSE-APACHE` in this directory are the upstream license
  texts and must be preserved verbatim in any redistribution.
- `NOTICE` and `ATTRIBUTION.md` describe upstream third-party material and are
  reproduced here from upstream.
- **The ArtCraft name, wordmark and logos under `docs/brand/` are trademarks of
  the ArtCraft Team, are *not* open source, and are not covered by the upstream
  license.** Spool does not redistribute or use them. `docs/brand/` is retained
  only as part of the verbatim upstream import and must not be reused in Spool
  product surfaces.
- Upstream's optional embedded fonts (`CRAFT_FONTS_DIR`) are **not** included;
  Spool relies on system fonts. See upstream `NOTICE` for the font licenses that
  would apply if that build input is ever adopted.

Figcraft's `.fig` importer/exporter (`crates/format/src/fig/`) is an
independent reverse-engineering effort against the public `.fig` container
format. It is flagged for legal review before any public Spool release.