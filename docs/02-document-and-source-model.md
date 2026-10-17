# Spool document and source model

## Authorities

The project source consists of HTML, CSS, and SVG files. Those files define authored content and visual implementation. `lamine.yaml` indexes Spool concepts that markup alone cannot reliably supply. The native Rust document/runtime is compiled from those files and the index and may be discarded and rebuilt.

```text
HTML / CSS / SVG ─┐
                  ├─ load + validate ─> native runtime ─> GPUI renderer
lamine.yaml ──────┘
        semantic operation ─> source + metadata updates ─> history
```

### The document model is compiled in both directions

The in-memory document model is the vendored figcraft `Document`
(`vendor/figcraft/crates/doc`). That is an implementation choice, and it changes
the *shape* of the pipeline, not its authority.

The arrow above now runs both ways. Source is compiled into the document model,
and the document model is serialized back out to source. This is a larger surface
than "compile once, then edit in memory", and it is accepted deliberately:

- **One model, one implementation.** Spool does not maintain a second, smaller
  document type whose properties are a subset of the engine's. Divergence between
  two models is how "the inspector shows a value the renderer ignores" happens.
- **The disposability guarantee is unchanged and is the thing to test.** The
  document model still holds no authority. If `Document` is deleted and rebuilt
  from the project, the project is unchanged. Here "the project" means authored
  HTML/CSS/SVG **and** `lamine.yaml` together — that pair is the authority, and
  losing either loses project state. If the model cannot be rebuilt from that
  pair without loss, Spool has violated this document, regardless of what the
  code looks like. This is a testable property and must be covered by one.
  Note the consequence for Owner B: because those properties have no CSS
  spelling, their only re-entry path is `lamine.yaml`. `lamine.yaml` is not
  optional bookkeeping; it is half the project.
- **Round-tripping is therefore a correctness requirement, not a feature.** A
  property that the model holds but the writer cannot express is a property Spool
  does not yet support. It must be reported as unsupported rather than silently
  held in memory and lost on save.

## Property ownership

**Amended 2026-10-17.** This section is normative and did not previously exist.
Adopting the figcraft document model raised a question the previous text did not
anticipate: the model carries roughly 130 persisted properties, and a large
fraction of them have no honest CSS representation. Without a rule, the two
available failure modes are to push design-tool semantics into authored CSS where
they do not belong, or to let `lamine.yaml` quietly become the second project
database that the non-negotiable boundaries forbid.

Every persisted property has exactly one owner. Assigning a property to more
than one owner is a defect, not a redundancy.

### Owner A — authored in HTML/CSS/SVG

Has a real CSS or HTML/SVG spelling. Authored source owns it, and Spool writes
back to the author's existing span.

Geometry and box: position and size, `width`/`height`, `min-*`/`max-*`,
`margin`, `padding`, `box-sizing`, `overflow`, `overflow-x`/`-y`,
`opacity`, `mix-blend-mode` (the eighteen real modes), `transform`, `display`,
`clip` as `overflow: hidden`, z-order as document order.

Paint: solid fills, solid strokes, stroke weight, per-side stroke widths,
stroke dash, `border-radius`.

Text: content, font family, font style, size, weight, line height, letter
spacing, text align, text case, decoration, paragraph indent and spacing,
hanging punctuation, max lines, OpenType features, variable-font axes, links,
solid text colour.

Gradients (linear and radial), images, image scale mode and rotation, blend
modes, opacity, blur, drop shadow.

### Owner B — Spool-specific facts in `lamine.yaml`

No honest CSS representation. This is exactly the category the existing rule
already permits — "Spool-only facts" that markup cannot supply — and this
section exists to make the boundary checkable rather than a matter of taste.

Identity and structure: `id`, `name`, `kind`, `parent`, ordered `children`,
and the source binding.

Resize behaviour: constraints, `constrain_proportions`.

Auto-layout semantics that CSS cannot state: `hug`/`fill` sizing, grid
auto-layout, track sizing, content alignment, strokes-in-layout,
canvas stacking (`reverse_z`), groups and sections, and shape-generator
parameters (polygon and star counts, ellipse arc angles).

Paint and stroke semantics CSS cannot state: stroke alignment
(inside/center/outside), arrowhead and filled caps, variable-width stroke
profiles, brushes, disabled entries mid-stack, corner smoothing, and
`PassThrough` as an isolation flag rather than a blend value.

Effects with no CSS: noise, texture, glass, progressive blur, and the
blend/behind flags on shadows.

Whole subsystems: components, instances, slots, variants, component property
definitions and overrides; variables, collections, modes, aliases, bindings;
shared styles and style bindings; prototype triggers, actions, transitions,
easing and flows; export settings and print settings; layout grid overlays;
Dev Mode status, annotations, code links and resources; guides; slices;
comments; libraries; editor lock; document colour swatches; colour profile;
plugin and agent data.

### Owner C — derived, never stored as independent truth

Computed from other properties. Must never be written to source as an
independent value, and must be recomputable after deleting any cache.

Layout results; group and boolean-group bounds; materialised instance children;
dev-mode snapshots; hit-test and paint-reach data; image intrinsic dimensions;
thumbnails; published-library copies; the node index.

### Owner D — session state, never persisted

Selection, camera, hover, caret, gestures, previews, text-edit state, the id
allocator's client component, the dirty set, and in-memory undo snapshots.

### The rule this section exists to enforce

`lamine.yaml` must not duplicate anything in Owner A. If a value can be read
from authored CSS, it is not in `lamine.yaml` — even if keeping it there would
be more convenient. Conversely, a value in Owner B must not be smuggled into a
CSS custom property as a cache, and must not be reimplemented as a CSS
structure that a browser would then misinterpret.

The exhaustive per-field table, including the exact CSS spelling chosen for
each Owner A property and the reasoning for every ambiguous Owner A/B call, is
in `docs/research/document/property-ownership.md`. That table is evidence; this
section is the rule.

## Auto-layout is authored as flex

**Decision, 2026-10-17.** Authored `display: flex` is the on-disk representation
of auto-layout: `flex-direction`, `flex-wrap`, `gap`, `row-gap`, `column-gap`,
`padding`, `justify-content` and `align-items` are all Owner A. Sizing
(`hug`/`fill`), grid auto-layout and the rest of Owner B stay in `lamine.yaml`.

This is load-bearing and worth stating plainly, because the alternative leaves
Spool with two unrelated layout systems and authored source that lies about what
the editor will do. Mapping flex declarations onto the engine's auto-layout means
the source a person reads is the source that determines the result.

It is not free, and the following are accepted costs rather than oversights:

- The engine's auto-layout is Figma's, not CSS flexbox. It has no `flex-basis`,
  no `order`, no `float`, and no intrinsic-vs-explicit sizing precedence
  identical to the spec. Divergences must be characterised and documented as
  they are found, not smoothed over.
- `hug` and `fill` are not CSS. See Owner B.
- Figma's grid auto-layout is not CSS Grid. See Owner B.

## `lamine.yaml`

One record per managed node. The required record is: immutable `id`, unique human `name`, semantic `kind`, optional `parent`, ordered `children`, and `source.file` plus `source.selector` (or a source range when a selector is not stable). IDs are opaque and never derived from names, DOM order, or CSS classes. Renaming changes only the name. Reordering changes hierarchy, not identity. Duplicate IDs, names, dangling links, and ambiguous source bindings fail validation.

The file may contain Spool-only facts such as frame identity, mask/group semantics, or cached child count when a feature proves it needs them. It must not own text, geometry, fills, typography, or other values already authored in HTML/CSS/SVG. Derived counts and runtime caches are rebuildable and must not become competing truth.

## Source ownership and provenance

Each runtime node retains its stable Spool ID and a source binding. A semantic edit records which source file and authored construct it changes. Source ranges are derived from the current parse and are never durable identity. External source edits trigger re-parse and re-binding; missing or ambiguous identities are reported, not guessed. Source preservation is a correctness property: edits must leave bytes outside their intended source span unchanged.

## Runtime boundary

Persistent `Document` contains source references, metadata, and source-derived semantic nodes. `EditorState` contains selection, camera, tools, hover, gestures, caret, and temporary guides. Renderer structures, indexes, layout results, and caches are runtime-only. No GPUI entity or view state is serialized.

## Initial format contract

> **Historical, 2026-10-10.** This section scoped the first implementation to
> "explicit Spool IDs and source bindings for a deliberately small HTML subset"
> and disclaimed general CSS cascade support. That shipped. The disclaimers are
> still true of the *parser* that existed then and are superseded by the
> vendored engine's own parser and layout. What carries forward, unchanged, is
> the last sentence, which is a requirement rather than a description:

Any cache is optional; deleting it cannot lose user work. Unsupported, external,
or ambiguous CSS must remain untouched until a dedicated ownership rule exists —
and the ownership table above is where those rules now live. A property with no
rule in that table is not writable, and saying so is preferable to guessing.
