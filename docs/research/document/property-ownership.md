# Property ownership: figcraft document model → Spool source

> **Added:** 2026-10-17 · **Status:** evidence for the normative rule in
> [../../02-document-and-source-model.md](../../02-document-and-source-model.md)
> · **Subject:** `vendor/figcraft/crates/doc` at
> `9e28e119548745adf9dbf09e96588ce79a9d83d9`

This document is evidence, not a rule. The rule lives in `docs/02`. Where the
two appear to disagree, `docs/02` wins and this document is wrong.

## Scope

Every persisted field of the figcraft `Document`, classified A (CSS-authorable),
B (`lamine.yaml`), C (derived) or D (session-only), per the four owners defined
in `docs/02`.

Structural note: `crates/doc` has **no** session-state fields. Selection, camera,
hover, caret, gestures and previews live in `figcraft-engine` and the undo
history, outside the document. The only persisted-but-D fields are the id
allocator cursor and the `#[serde(skip)]` dirty set.

Owners are assigned per *group* here rather than per field. The groups are the
unit a contributor actually reasons about, and field-level enumeration is
mechanical from the source: `crates/doc/src/props.rs` (1490 lines) for strokes,
effects, corners, constraints, auto-layout, grids and export; `node.rs` (787) for
`Node` and `NodeKind`; `text.rs` (785) for text; `proto.rs` (954) for prototype;
`styles.rs` (290) for styles and variables; `vector.rs` (681) for vector
networks; `color/src/lib.rs` for paint.

## Assignment

### Node — Owner A

`visible` · `opacity` · `blend` (the eighteen real modes) · `transform` ·
`width` · `height` · `fills` (solid, gradient, image) · `strokes` (solid, box) ·
`stroke.weight` · `stroke.sides` · `stroke.cap_start`/`cap_end` (None/Round/
Square only) · `stroke.join` · `stroke.miter_limit` · `stroke.dashes` ·
`corners.radii` · `clip` · `overflow` · `parent` · `children` (document order
*is* paint order) · `Paint.opacity` · `Paint.blend` · gradient stops and handles ·
`ImagePaint.{image,scale_mode,scale,rotation}` · `ImageFilters.{contrast,
saturation}` · `BlendMode` (all but `PassThrough`).

### Node — Owner B

`id` · `name` · `locked` · `constraints` · `layout.mode` (Grid variant) ·
`layout.content_align` · `layout.grid_*` · `layout.strokes_included` ·
`layout.reverse_z` · `child_layout.sizing_h`/`sizing_v` · `child_layout.grid_*`
· `child_layout.{min,max}_*` where a `Hug`/`Fill` makes them binding rather than
informational · `is_mask` · `mask_type` · `grids` · `guides` · `exports` ·
`interactions` · `scroll_position` (Fixed) · `styles` · `bound` · `var_modes` ·
`bool_op` · `constrain_proportions` · `source` · `exposed` · `dev_*` ·
`overrides` · `prop_refs` · `plugin_data` · `annotations` · `code_links` ·
`dev_resources` · `NodeKind::Group` · `Section.contents_hidden` ·
`Shape-generator parameters (`Polygon.count`, `Star.{count,inner_ratio}`,
`Ellipse.{start,sweep,inner}`) · `VectorNetwork.vertices[].{radius,mirror,cap,
join}`.

### Node — Owner C

Group and boolean bounds (`bounds_follow_children`) · `paint_order()` ·
`own_paint_bounds` · `grow_by_effects` · `outline_reach` · `stroke_runs` ·
materialised instance children · dev snapshots · `PatternCell`.

### Node — Owner D

None on `Node`. All session state is in the engine.

### Document level

Owner A: image bytes. Owner B: `name`, `pages`, `styles`, `variables`,
`document_colors`, `comments`, `libraries`, `color_profile`, `image_presets`,
`annotation_categories`. Owner C: the `nodes` index, `published`, `dev_snapshots`.
Owner D: `current_page`, `dirty`, `IdAlloc.client`.

### Effects — split

| Effect | Owner | Spelling |
|---|---|---|
| Drop shadow | A | `box-shadow: x y blur spread color` |
| Inner shadow | B | CSS has none; the `clip-path` + inverted-shadow trick fails on transparent and non-rectangular regions |
| Layer blur | A | `filter: blur()` |
| Background blur | A | `backdrop-filter: blur()` |
| Noise, texture, glass | B | no CSS |
| `Shadow.blend`, `Shadow.show_behind`, `Effect.visible` | B | no CSS |

### Text — split

Owner A: `characters` · `runs` (inline ranges) · `font_family` · `font_size` ·
`line_height` · `letter_spacing` · `align_h` · `decoration` and most of
`decoration_look` · `case` · solid `fills` · `link` · `variations` · `features` ·
`paragraph_indent` · `paragraph_spacing` · `hanging_punctuation` · `max_lines`.

Owner B: `auto_resize` · `align_v` · `list` and `paragraphs` (the per-paragraph
mixed model is not `list-style-type`) · `list_spacing` · `hanging_list` ·
`leading_trim` · `path` (`TextPath.start`, `flip`) · `style_id` ·
`DecorationLook.offset` (figcraft's semantics are its own model).

## The ambiguous cases, and why they went the way they did

Ordered by how much damage getting them wrong would do.

**Stroke alignment is Owner B, always.** `paint-order: stroke fill` does *not*
place the stroke band relative to the outline — it only decides painting order,
which matters when stroke and fill differ in colour. Figma's inside/center/
outside changes *geometry*: an inside stroke eats into the fill, an outside
stroke expands past the box, and paint bounds depend on it. Mapping
`align: outside` onto `paint-order` silently moves every border inward and
changes export bounding boxes. This is the single most mis-mappable property in
the model.

**Width and height are Owner A for sized nodes, Owner C for groups.**
For a group or a boolean group the box is *derived* from children. Persisting it
would freeze a stale layout result into the source file, where a later child
edit would not update it.

**`transform` is Owner A, but reads translate-only.** Today Spool writes a move
as `position: absolute; left; top` and uses `transform` only for a genuine visual
offset (`app/src/visual.rs`, `translate_of`). The engine's `transform` is a full
`Affine` including rotation and scale. Widening the read path without deciding
what writes rotation and scale would make `transform` a property that can be
silently dropped.

**`PassThrough` is an isolation flag, not a blend mode.** It means "do not
isolate this group's children". The CSS equivalent of it is the *absence* of
`mix-blend-mode`, `opacity`, `filter`, `transform` and `clip-path` on the
container. Authoring `mix-blend-mode: normal` would isolate a frame's children
and change how every grandchild composites.

**`Corners.smoothing` is Owner B.** Corner smoothing has no cross-browser CSS
property. Encoding it by rewriting the radius would be a different design.

**Arrowhead stroke caps are Owner B.** `None`/`Round`/`Square` map to
`stroke-linecap`. Mapping `TriangleArrow` onto `round` destroys arrowheads.

**Constraints are Owner B.** They describe how a child responds when its parent
resizes. `aspect-ratio` gives part of `Scale` and nothing of
left/right/left-right/center.

**Masks are Owner B.** CSS `mask` plus `mask-mode` covers the alpha and
luminance *value* choice. Three things do not survive: the mask applies to all
siblings above it rather than to its own box; `Vector` mode ignores the mask's
own fills and opacity; and it is a stacking operation rather than per-element
paint. A CSS rendering of a mask is a projection of it, never its owner.

**Effects that are hidden mid-stack are Owner B.** A disabled entry in the middle
of a paint or effect list has no CSS spelling — the only way to express it in CSS
is to delete it.

## What CSS genuinely cannot express

Grouped so the claim can be checked rather than accepted.

1. Sizing — `hug` and `fill`. `max-content` only approximates hug; `flex: 1` is
   not Figma's fill.
2. Resize constraints.
3. Figma grid auto-layout — fixed count, rows growing to fit, implicit tracks,
   and a `Hug` track size.
4. Layout grid overlays — these are a design-time guide, not a layout engine.
   CSS Grid is a layout system and is not equivalent.
5. Stroke alignment, arrowhead caps, variable-width strokes and brushes.
6. Corner smoothing.
7. Noise, texture, glass, progressive blur.
8. Boolean geometry — `clip-path` is not a substitute.
9. Shape-generator parameters — the *points* are authored SVG, the generator
   seed is not.
10. Vector handle-mirroring constraints.
11. Sibling-scoped masking.
12. Colour profile as a document-level interpretation switch.
13. Variables — CSS custom properties are untyped, mode-less and alias-free.
14. Shared style assets.
15. Components, instances, slots, variants, property overrides.
16. Prototype — triggers with delays and gamepads, smart animate, overlays,
    conditional actions, variable-setting actions, springs.
17. Export settings and print settings.
18. Dev Mode, guides, slices, comments, libraries, editor lock, document
    swatches.

## What Spool's existing implementation actually covers

Cross-referenced against `app/src/style.rs` (`SUPPORTED_PROPERTIES`, 34 entries)
and `app/src/visual.rs` (block-flow layout).

**Spool today round-trips roughly 12 of its 34 supported properties** end to
end: geometry, `opacity`, `background-color`, `color`, `font-size`,
uniform `border-radius`, `display: none`, `transform: translate`, and
margin/padding/min/max as read-only. Stroke width and colour are read from
source but have **no demonstrated write path**.

Of the four categories:

| Category | Status today |
|---|---|
| A — CSS-authored | ~12 properties round-trip. Gradients, images, effects, per-corner radii and multi-value paints do not. |
| B — `lamine.yaml` | **No home at all.** Spool's metadata record is `id`, `name`, `kind`, `parent`, `children`, `source` only. |
| C — derived | Block-flow layout is implemented in `visual.rs` and superseded by the engine's layout. |
| D — session | Already correct. |

## Structural gaps that will bite first

These are ordering constraints on the implementation, not a worklist.

1. **`stroke-width` has no authored spelling.** The inspector exposes a
   stroke-width control; `stroke-width` is not in `SUPPORTED_PROPERTIES`, and the
   only CSS stroke Spool can express is the uniform `border` shorthand. A
   stroke-width edit does not persist.
2. **Per-corner radii cannot round-trip.** `Corners.radii` is four values;
   Spool's runtime carries a single `f32`, and `style::parse_length` accepts one
   length. This is a type-level mismatch, not a missing feature.
3. **Text has two properties and no box.** The model carries thirteen text-style
   fields and a full paragraph model; Spool carries `font-size` and `color`.
4. **A text box is one line at `font-size × 1.4`.** No inline layout, no
   wrapping. Every text-sizing property is therefore structurally
   unrepresentable until the engine's text and layout crates are wired in —
   which the vendoring now makes possible.
5. **Adding the flex longhands is the highest-value single extension**, and it
   is bounded and well specified. It is also the prerequisite for the auto-layout
   decision in `docs/02`.
6. **Owner B has no storage.** This is the largest piece of new work and it is
   not optional: roughly half the model has nowhere to live.

## Reproducing this

```sh
# Owner assignments, per group
rg -n 'pub [a-z_]+:' vendor/figcraft/crates/doc/src/props.rs

# Spool's current CSS surface
rg -n -A40 'SUPPORTED_PROPERTIES' app/src/style.rs

# Spool's current layout scope
rg -n -B2 -A12 'block flow' app/src/visual.rs
```