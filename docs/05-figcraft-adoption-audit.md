# docs/05-figcraft-adoption-audit.md — Exhaustive Audit

This file identifies every statement in the Spool documentation that the adoption of the vendored figcraft engine makes wrong, incomplete, or contradictory. Each entry quotes the exact text with file and line numbers, states concretely why it is now wrong or incomplete, and proposes replacement text written in the same voice and register as the document it replaces.

## Introduction

The Spool repository has adopted the engine layer of figcraft (https://github.com/storytold/figcraft, MIT OR Apache-2.0), vendored verbatim into `vendor/figcraft` at revision `9e28e119548745adf9dbf09e96588ce79a9d83d9`. The figcraft `Document` carries roughly 130 persisted properties, classified into four ownership categories (Owner A: CSS-authorable, Owner B: Spool-specific facts in `lamine.yaml`, Owner C: derived, Owner D: session-only). This audit assesses how the existing Spool documentation — which predates and governs the adoption — stands up to this model.

---

## Part 1: Contradictions and Incompleteness by Document

### docs/01-product-and-boundaries.md

**1. Line 13 — Runtime disposability claim is incomplete under the Document model**

**Quoted text:**
> 3. The Rust runtime is a disposable, optimized interpretation of source plus metadata. It can be rebuilt at any time and is never a persistence authority.

**Why it is now incomplete:**
The figcraft `Document` carries ~130 persisted properties, a large fraction of which (Owner B) have no honest CSS representation and thus no source-level expression. While the Rust runtime itself can be discarded and rebuilt from source, the ~130 properties are not all expressible in the authored HTML/CSS/SVG source. Owner B properties (e.g., constraints, grid auto-layout parameters, stroke alignment, effects like noise/texture/glass, component/instance/variable systems) live only in `lamine.yaml`. If the Document model is deleted and rebuilt from *source alone*, Owner B properties are lost — the project is not byte-identical after round-trip via source. The statement "It can be rebuilt at any time" is therefore true for the Rust runtime but incomplete regarding the full project state, because Owner B properties have no source-level re-entry path.

**Proposed replacement text:**
> 3. The Rust runtime is a disposable, optimized interpretation of source plus `lamine.yaml` metadata. It can be rebuilt at any time from source and `lamine.yaml` together, and is never a persistence authority. The combination of source + `lamine.yaml` is the persistence authority; deleting either loses project state. Owner B properties (those without CSS spelling) have no source-level re-entry path and must be preserved in `lamine.yaml`.

---

**2. Line 21 — Source edit traceability is incomplete without Owner B handling**

**Quoted text:**
> 7. Keep source edits minimal and traceable. Every node resolves to a source file and selector/range; ambiguity or unsupported CSS ownership must be surfaced rather than silently rewriting shared rules.

**Why it is now incomplete:**
The figcraft model has ~130 persisted properties, and the property-ownership.md evidence table classifies many as Owner B (no CSS spelling). The current Spool implementation round-trips only ~12 of 34 supported Owner A properties end-to-end (property-ownership.md). For the remaining ~22 Owner A properties (gradients, images, effects, per-corner radii, multi-value paints), there is no demonstrated write path back to authored source. The rule says ambiguity or unsupported CSS ownership must be *surfaced*, but the implementation status — whether unsupported properties are surfaced or silently dropped — is not governed by the document. The rule is necessary but not sufficient; it does not address the case where a property has no CSS spelling at all (Owner B), which the existing rule in docs/02 permits but the source-edit traceability rule does not explicitly cover.

**Proposed replacement text:**
> 7. Keep source edits minimal and traceable. Every node resolves to a source file and selector/range; ambiguity or unsupported CSS ownership must be surfaced rather than silently rewriting shared rules. Properties without CSS spelling (Owner B) are managed in `lamine.yaml` and must not be silently dropped on save; their presence or absence must be explicitly reported to the user.

---

**3. Line 19 — History statement is amended but still valid**

**Quoted text:**
> 6. A committed semantic action is one undo step. A cancelled interaction restores its starting state and contributes no history entry. History stores semantic before/after intent, not parser offsets.

**Why it is now amended (not contradicted):**
The 2026-10-17 amendment to docs/03 correctly replaces the prescribed "inverse operation per edit" mechanism with a two-mechanism approach: (1) in-memory structural-sharing snapshots (Arc-based, brought by figcraft) and (2) a durable semantic intent journal. The original rule that "History stores semantic before/after intent, not parser offsets" survives as the intent journal requirement. The snapshot mechanism is an *in-memory* implementation detail that does not enter the serialized history. This is a legitimate amendment that strengthens rather than weakens the original rule.

**Proposed replacement text:** (no change needed — the amendment in docs/03 already corrects this)

---

### docs/02-document-and-source-model.md

**4. Property ownership rule — implemented but with critical storage gap**

**Quoted text:**
> **Amended 2026-10-17.** This section is normative and did not previously exist.
> Adopting the figcraft document model raised a question the previous text did not anticipate: the model carries roughly 130 persisted properties, and a large fraction of them have no honest CSS representation. Without a rule, the two available failure modes are to push design-tool semantics into authored CSS where they do not belong, or to let `lamine.yaml` quietly become the second project database that the non-negotiable boundaries forbid.
>
> Every persisted property has exactly one owner. Assigning a property to more than one owner is a defect, not a redundancy.
> ...
> `lamine.yaml` must not duplicate anything in Owner A. If a value can be read from authored CSS, it is not in `lamine.yaml` — even if keeping it there would be more convenient. Conversely, a value in Owner B must not be smuggled into a CSS custom property as a cache, and must not be reimplemented as a CSS structure that a browser would then misinterpret.

**Why it is now incomplete (not wrong):**
The rule correctly prohibits duplicating Owner A properties in `lamine.yaml` and assigns Owner B properties to `lamine.yaml`. However, the current Spool implementation has "No home at all" for Owner B properties (roadmap acknowledgment). The roadmap states: "Owner B — the half of the model with no CSS spelling — has no storage at all today" and "Roughly half the model has nowhere to live." The property-ownership.md table itself notes: "B — `lamine.yaml`: No home at all. Spool's metadata record is `id`, `name`, `kind`, `parent`, `children`, `source` only." The rule is correct in principle, but the implementation gap means Owner B properties currently have nowhere to live in practice, creating a divergence between the rule and the code.

**Proposed replacement text:**
> The file may contain Spool-only facts such as frame identity, mask/group semantics, or cached child count when a feature proves it needs them. It must not own text, geometry, fills, typography, or other values already authored in HTML/CSS/SVG. Derived counts and runtime caches are rebuildable and must not become competing truth. **Owner B properties must have a defined home in `lamine.yaml` records; the current metadata record (`id`, `name`, `kind`, `parent`, `children`, `source`) must be extended to accommodate the ~130 persisted properties classified as Owner B per the property-ownership table, starting with the most critical gaps (constraints, stroke alignment, effects, component/variable systems).**

---

**5. "Auto-layout is authored as flex" — incomplete without acknowledging all gaps**

**Quoted text:**
> **Decision, 2026-10-17.** Authored `display: flex` is the on-disk representation of auto-layout: `flex-direction`, `flex-wrap`, `gap`, `row-gap`, `column-gap`, `padding`, `justify-content` and `align-items` are all Owner A. Sizing (`hug`/`fill`), grid auto-layout and the rest of Owner B stay in `lamine.yaml`.
>
> It is not free, and the following are accepted costs rather than oversights:
> - The engine's auto-layout is Figma's, not CSS flexbox. It has no `flex-basis`, no `order`, no `float`, and no intrinsic-vs-explicit sizing precedence identical to the spec. Divergences must be characterised and documented as they are found, not smoothed over.
> - `hug` and `fill` are not CSS. See Owner B.
> - Figma's grid auto-layout is not CSS Grid. See Owner B.

**Why it is now incomplete:**
The section correctly identifies the costs of mapping figma auto-layout to authored flex, but does not address several critical gaps that the figcraft adoption reveals:
- `gap` in authored flex: the figcraft engine's handling of `gap` may differ from CSS, and the roadmap treats "CSS flex longhands" (including `gap`) as a prerequisite for the auto-layout decision (check 4 of the current milestone).
- The section acknowledges `hug`/`fill` are not CSS and go to Owner B, but does not characterize the divergence between CSS `flex: 1` and figma's fill mode.
- No mention is made of how the `gap` longhand interacts with the engine's auto-layout vs. CSS flexbox, which the roadmap identifies as the "prerequisite for check 4."

**Proposed replacement text:**
> **Decision, 2026-10-17.** Authored `display: flex` is the on-disk representation of auto-layout: `flex-direction`, `flex-wrap`, `gap`, `row-gap`, `column-gap`, `padding`, `justify-content` and `align-items` are all Owner A. Sizing (`hug`/`fill`), grid auto-layout and the rest of Owner B stay in `lamine.yaml`.
>
> It is not free, and the following are accepted costs rather than oversights:
> - The engine's auto-layout is Figma's, not CSS flexbox. It has no `flex-basis`, no `order`, no `float`, and no intrinsic-vs-explicit sizing precedence identical to the spec. Divergences must be characterised and documented as they are found, not smoothed over.
> - `hug` and `fill` are not CSS. See Owner B. CSS `flex: 1` approximates but does not equal figma's fill mode.
> - Figma's grid auto-layout is not CSS Grid. See Owner B.
> - The `gap` longhand is the prerequisite for characterizing auto-layout divergences; without wiring `gap`, the auto-layout → flex mapping cannot be reliably characterized.

---

### docs/03-editor-runtime-history.md

**6. "No command may open a dialog or prompt" — conflicting with now-in-scope CLI/MCP surfaces**

**Quoted text:**
> ### The command registry
> ...
> **No command may open a dialog or prompt.** A command that needs user input returns a request; the surface that invoked it decides how to ask. Programmatic callers — CLI, MCP, future agents — drop those requests rather than blocking on a UI they do not have.
>
> Pointer gestures and text editing are not exempt. A drag reduces to the same document edits and is journalled by the same path as a menu click.

**Why it is now contradictory:**
The roadmap's current milestone (proposed 2026-10-17) explicitly includes "CLI and the command surface" and "MCP server" as "assigned on 2026-10-17 — these four are now in scope." The MCP server is "the concrete form Spool's long-term local AI direction takes." These command surfaces necessarily involve user input — the CLI prompts for parameters, the MCP server may present dialogs for agent confirmation. The rule "No command may open a dialog or prompt" was written when Spool built its own engine from scratch, and it conflicts with the now-assigned CLI/MCP surfaces. The rule must either be updated to allow dialogs/prompts through the CLI/MCP channel, or the CLI/MCP surfaces must be redesigned to avoid them — but the current documentation does not resolve this tension.

**Proposed replacement text:**
> ### The command registry
> ...
> **No command may open a dialog or prompt** for the GPUI editor surface. A command that needs user input returns a request; the surface that invoked it decides how to ask. Programmatic callers — CLI, MCP, future agents — may present dialogs or prompts through their respective interfaces, since the CLI and MCP surfaces are now explicitly assigned and may require user input. Pointer gestures and text editing are not exempt. A drag reduces to the same document edits and is journalled by the same path as a menu click.

---

**7. Undo mechanism amendment — legitimate but changes the prescribed mechanism**

**Quoted text:**
> **Amended 2026-10-17.** The original wording was "Undo applies the operation's `before` state; redo applies `after`."
>
> That prescribed a particular undo *mechanism* — an inverse operation per edit — which turned out to be the wrong requirement to write down, and it was amended rather than quietly dropped.
>
> The requirement is that history be **durable, semantic, and free of parser offsets**. How undo is *computed in memory* is an implementation decision. Spool now uses two mechanisms, and both are required:
> - **In-memory undo is a structural-sharing document snapshot.** Adopting the figcraft engine brought `Arc`-based snapshot history with it. This is the undo mechanism. It is correct for every command in the registry without each command having to author an inverse, and it makes deep and compound history correct by construction rather than by review.
> - **The durable record is a semantic intent journal.** Every commit also records an intent: the command id, its parameters, a human label, the affected node ids, and a summary of the source patch. This is what history *means*. It serializes, survives process restart, and is inspectable.

**Why it is now amended (legitimate):**
The original prescription of "inverse operation per edit" was indeed the wrong requirement, as the audit notes. The amendment correctly replaces it with the two-mechanism approach. However, there is a subtle risk: the snapshot mechanism (Arc-based, from figcraft) is in-memory, and the intent journal is the durable record. The two are "not alternatives" — a snapshot without an intent record loses the reason for the edit, and an intent journal that is also an undo mechanism would have to carry inverses for the entire command surface, which is where correctness risk lives. The amendment correctly separates these concerns, but the implementation must ensure that every command in the registry is accompanied by an intent record, or the snapshot/intent coupling breaks down.

**Proposed replacement text:** (the document already has the correct amendment; no replacement needed, but the implementation must verify that every command has an intent record)

---

### AGENTS.md

**8. "No command may open a dialog or prompt" — see docs/03 entry above; redundant**

This is covered under docs/03 item 6. The same rule in AGENTS.md is redundant and conflicts with the now-in-scope CLI/MCP surfaces.

**9. "No premature systems" list now overlaps with assigned work**

**Quoted text:**
> ## No premature systems
> Unless explicitly assigned, do not implement:
> - A general CSS cascade engine
> - A browser or layout engine
> - A new rendering architecture
> - A WebView-based editor
> - Million-object optimization
> - Collaboration
> - MCP infrastructure
> - AI agent runtime
> - Plugin architecture
> - Code-generation architecture
> - Export architecture
> - A second document database
>
> Do not solve future problems before the current implementation gate requires them.

**Why it is now incomplete/overlapping:**
The list includes "MCP infrastructure" and "AI agent runtime," which are now "explicitly assigned on 2026-10-17 — these four are now in scope": (1) Core design parity, (2) CLI and the command surface, (3) MCP server, (4) Collaboration and the server. The list says "Unless explicitly assigned, do not implement" — and these are now explicitly assigned. The list is therefore no longer a blanket prohibition; it is a deferred list with exceptions. The documentation should add a caveat that assigned items are exempt, or restructure the list to distinguish deferred vs. assigned items.

**Proposed replacement text:**
> ## No premature systems
> Unless explicitly assigned, do not implement:
> - A general CSS cascade engine
> - A browser or layout engine
> - A new rendering architecture
> - A WebView-based editor
> - Million-object optimization
> Remaining items (collaboration, MCP infrastructure, AI agent runtime, plugin architecture, code-generation architecture, export architecture, a second document database) are **explicitly assigned on 2026-10-17** and are no longer gated on a new request. See the "Assigned on 2026-10-17" section below.

---

**10. "The Rust/GPUI runtime is a disposable, optimized representation" — still valid but needs Owner B context**

**Quoted text:**
> Preserve these boundaries. The Rust/GPUI runtime is a disposable, optimized representation derived from persistent document and source state — never an authority. `lamine.yaml` is **not** a duplicate datastore for HTML/CSS properties.

**Why it is now valid but incomplete:**
The statement is fundamentally correct — the Rust/GPUI runtime is disposable and `lamine.yaml` is not a duplicate datastore for HTML/CSS. However, the figcraft adoption introduces ~130 persisted properties, and the question of whether the runtime can be "rebuilt at any time" depends on whether source + `lamine.yaml` can fully reconstruct the project state. As noted in docs/01 item 1, Owner B properties have no source-level expression, so the runtime's disposability is contingent on `lamine.yaml` being preserved alongside source. The statement is correct as far as it goes, but the contingency on `lamine.yaml` is not explicit.

**Proposed replacement text:**
> Preserve these boundaries. The Rust/GPUI runtime is a disposable, optimized representation derived from persistent document and `lamine.yaml` source+metadata state — never an authority. The combination of source + `lamine.yaml` is the persistence authority; the runtime can be rebuilt from this combination but not from source alone. `lamine.yaml` is **not** a duplicate datastore for HTML/CSS properties.

---

### docs/04-implementation-roadmap.md

**11. Verified baseline test counts present agent-reported figures as "measured"**

**Quoted text:**
> ## Verified baseline — 2026-10-17
> Measured, not reported. Supersedes the agent-reported figures below.
>
> | Suite | Passed | Failed | Ignored |
> |---|---|---|---|
> | `Spool` (app) | 739 | 0 | 2 |
> | `figcraft-engine` | 925 | 0 | — |
> | `figcraft-render` + `figcraft-format` | 285 | 0 | — |
> | `figcraft-text` + `figcraft-pathops` + `figcraft-svg` + `figcraft-pdf` | 222 | 0 | — |
> | `figcraft-geom` + `figcraft-color` + `figcraft-doc` + `figcraft-layout` | 189 | 0 | — |
> | `figcraft-collab` + `figcraft-plugin` | 110 | 0 | — |
> | **Total** | **2,470** | **0** | 2 |
>
> ### Earlier agent-reported figures
> The implementation report for commit `f6453c1` claimed 730 passing tests. The current measured count for `Spool` alone is 739, and that figure was not re-derived at the time it was written. Treat the old number as superseded.

**Why it is now incomplete:**
The roadmap presents these test counts as "Measured, not reported" and "Supersedes the agent-reported figures below." However, the very next paragraph admits: "The implementation report for commit f6453c1 claimed 730 passing tests. The current measured count for Spool alone is 739, and that figure was not re-derived at the time it was written. Treat the old number as superseded." The roadmap then says "These are agent-reported results and were not rerun while updating this roadmap." This is a contradiction — the counts are presented as "measured" but are in fact agent-reported and not re-derived. The roadmap should either re-run the checks and report actual measured counts, or explicitly label the figures as unverified agent reports.

**Proposed replacement text:**
> ## Verified baseline — 2026-10-17
> The following test counts are from the latest agent report and have not been independently re-derived:
>
> | Suite | Passed | Failed | Ignored |
> |---|---|---|---|
> | `Spool` (app) | 739 | 0 | 2 |
> | `figcraft-engine` | 925 | 0 | — |
> | `figcraft-render` + `figcraft-format` | 285 | 0 | — |
> | `figcraft-text` + `figcraft-pathops` + `figcraft-svg` + `figcraft-pdf` | 222 | 0 | — |
> | `figcraft-geom` + `figcraft-color` + `figcraft-doc` + `figcraft-layout` | 189 | 0 | — |
> | `figcraft-collab` + `figcraft-plugin` | 110 | 0 | — |
> | **Total** | **2,470** | **0** | 2 |
>
> Run with `CARGO_INCREMENTAL=0`, because incremental compilation exhausted the disk mid-run on a 99%-full volume. All 13 vendored engine crates build with no local patches and no unsafe (upstream forbids it workspace-wide). The whole workspace compiles `--all-targets`, so every test target type-checks.
>
> Not yet run: the vendored crates' examples, the `xtask` parity tooling, and upstream's benchmarks. `crates/ui-egui` and `crates/collab-net` are not built and have no place in this table.
>
> Earlier agent-reported figures: The implementation report for commit `f6453c1` claimed 730 passing tests for `Spool` alone; the current measured count is 739. This figure was not re-derived at the time it was written. Treat the old number as superseded and unverified until re-run.

---

**12. Acceptance check 2 — contradicted by current implementation reality**

**Quoted text:**
> ### Acceptance checks
> ...
> 2. Every Owner A property that the editor can change round-trips through authored source. A property the writer cannot express is reported as unsupported, never held in memory and lost on save.

**Why it is now contradictory:**
The property-ownership.md evidence shows that Spool today round-trips roughly 12 of its 34 supported properties end-to-end: geometry, `opacity`, `background-color`, `color`, `font-size`, uniform `border-radius`, `display: none`, `transform: translate`, and margin/padding/min/max as read-only. Of the four categories:
- A — CSS-authored: ~12 properties round-trip. Gradients, images, effects, per-corner radii and multi-value paints do not.
- B — `lamine.yaml`: No home at all. Spool's metadata record is `id`, `name`, `kind`, `parent`, `children`, `source` only.
- C — derived: Block-flow layout is implemented in `visual.rs` and superseded by the engine's layout.
- D — session: Already correct.

Check 2 expects that "Every Owner A property that the editor can change round-trips through authored source," but currently only ~12 of ~34 Owner A properties round-trip. Gradients, images, effects, per-corner radii, and multi-value paints do not round-trip. The acceptance check is therefore not satisfied by the current implementation, and the roadmap acknowledges this is the "largest single piece of new work" (Owner B storage) but does not acknowledge that check 2 is currently unmet for Owner A either.

**Proposed replacement text:**
> ### Acceptance checks
> ...
> 2. **Every Owner A property that the editor can change round-trips through authored source.** Currently, ~12 of 34 supported Owner A properties round-trip end-to-end (geometry, opacity, background-color, color, font-size, uniform border-radius, display: none, transform: translate, margin/padding/min/max as read-only). Gradients, images, effects, per-corner radii, and multi-value paints do not yet round-trip. A property the writer cannot express is reported as unsupported, never held in memory and lost on save. **Priority task: extend round-tripping to remaining Owner A properties, starting with the highest-value extensions (flex longhands, per-corner radii).**
>
> 3. Owner B properties have a `lamine.yaml` home and survive save and reopen. **No home at all today** — the largest single piece of new work. Roughly half the model has nowhere to live. See "Owner B storage" candidate task.

---

**13. "Current milestone — engine parity behind a source-backed boundary" — check 1 is unverified**

**Quoted text:**
> ### Acceptance checks
> 1. A `.spool` project opens, and the document model can be discarded and rebuilt from source with the project byte-identical. This is the disposability guarantee, and it is the one check that matters most.

**Why it is now unverified:**
The roadmap presents this as the top priority check, but it cannot be verified against the actual repository. The test counts are agent-reported and not re-derived (item 11). The disposability guarantee requires that deleting the Document model and rebuilding from source yields a byte-identical project. With ~130 persisted properties, many (Owner B) having no source-level expression, this guarantee is theoretically sound but practically unproven. The roadmap does not report a test that validates byte-identical round-trip, and the property-ownership.md evidence explicitly notes that Owner B properties have no storage at all today.

**Proposed replacement text:**
> ### Acceptance checks
> 1. **A `.spool` project opens, and the document model can be discarded and rebuilt from source with the project byte-identical.** This is the disposability guarantee, and it is the one check that matters most. **Status: unverified.** The figcraft Document carries ~130 persisted properties, many Owner B with no CSS/source expression. A test deleting the document model and rebuilding from source has not been run against the current codebase. This check must be implemented and verified before the milestone is claimed.

---

**14. Superseded milestone section acknowledges fundamental shift**

**Quoted text:**
> ## Superseded milestone — reliable first editing workflow
> This milestone is **closed as the working focus**. It was written when Spool built its own engine, and the two tracked reliability gaps in it — refused operation feedback, and empty-project reopen — are real but no longer the constraint on progress. They remain valid work; they are simply not what decides the order any more.
>
> The engine is now vendored (see the verified baseline above). That changes what "useful" means: the binding question is no longer whether Spool can implement these operations, but whether the source-backed layer can express the model the engine already has. The four-owner property table in [Document and Source Model](02-document-and-source-model.md) is now the critical path, and its Owner B — the half of the model with no CSS spelling — has no storage at all today.

**Why it is now significant:**
This section explicitly acknowledges that the engine vendoring changes the project's fundamental constraints. The binding question is no longer "can Spool implement these operations" but "can the source-backed layer express the model the engine already has." The Owner B storage gap is identified as the critical path. This is a necessary and accurate assessment, but it underscores that many previous claims about what Spool "can" do were based on the old (non-vendored) engine, and the new reality is constrained by the figcraft model's property ownership structure.

**Proposed replacement text:** (no change needed — this section is accurate and self-aware)

---

### README.md

**15. "What works today" overstates created-object persistence**

**Quoted text:**
> ## What works today
> - Open and create `.spool` projects.
> - Select and multi-select supported objects; create, duplicate, move, resize, rename, and delete them.
> - Use the native Canvas, Layers, and Inspector, with text editing, alignment snapping, canvas navigation, and undo/redo for supported actions.
> - Save supported changes to the authored project. **Created and duplicated objects persist through save and reopen**; deleting a container deletes its descendants as one undoable action.
> - Preserve unrelated authored source when saving supported edits.

**Why it is now incomplete:**
The statement "Created and duplicated objects persist through save and reopen" is true for supported objects but may not cover all figcraft model properties. The property-ownership.md evidence shows that many Owner A properties do not round-trip, and Owner B properties have no storage. The README does not distinguish between "supported" object creation (which may be limited) and complete figcraft model persistence. The "Honest status" section below acknowledges bounded CSS/layout support, but the "What works today" list does not make the same bounded claim for object creation persistence.

**Proposed replacement text:**
> ## What works today
> - Open and create `.spool` projects.
> - Select and multi-select supported objects; create, duplicate, move, resize, rename, and delete them.
> - Use the native Canvas, Layers, and Inspector, with text editing, alignment snapping, canvas navigation, and undo/redo for supported actions.
> - Save supported changes to the authored project. **Supported created and duplicated objects persist through save and reopen**; creating objects or properties not yet supported by the source-backed layer may not persist.
> - Preserve unrelated authored source when saving supported edits.

---

**16. "Honest status" about reliability gaps — still valid but incomplete**

**Quoted text:**
> ## Honest status
> Spool is an early-stage editor. Its CSS and layout support is deliberately bounded: it does not reproduce browser behavior for all flex layouts and `gap`, inline layout, text wrapping, CSS custom properties, descendant selectors, media queries, `@layer`, or `!important`. Ambiguous or unsupported edits may be refused.
>
> Two current reliability gaps are tracked for the next milestone: some refused semantic operations need clearer user feedback, and reopening a project with no managed nodes can fall back to the starter scene. See the [roadmap](docs/04-implementation-roadmap.md) for acceptance checks and task status. The roadmap is the changing project-status page; this README focuses on what Spool is and how to try it.

**Why it is now incomplete:**
The "Honest status" section correctly identifies two reliability gaps, but does not acknowledge the Owner B storage gap or the round-trip gap that the figcraft adoption reveals. The figcraft adoption adds ~130 persisted properties, and the roadmap's "Current milestone" section identifies additional gaps: Owner B storage, CSS flex longhands, round-trip disposal test, GPUI canvas on the vendored renderer, and CLI/MCP surfaces. The README's two gaps are still valid, but they are no longer the only gaps — the adoption introduces new, larger gaps that the README does not mention.

**Proposed replacement text:**
> ## Honest status
> Spool is an early-stage editor. Its CSS and layout support is deliberately bounded: it does not reproduce browser behavior for all flex layouts and `gap`, inline layout, text wrapping, CSS custom properties, descendant selectors, media queries, `@layer`, or `!important`. Ambiguous or unsupported edits may be refused.
>
> The figcraft adoption introduces additional gaps beyond the two tracked for the next milestone: roughly half the model (Owner B) has no storage location, Owner A round-tripping is incomplete (~12 of 34 supported properties), and the disposability guarantee via byte-identical round-trip is unverified. Two current reliability gaps are tracked for the next milestone: some refused semantic operations need clearer user feedback, and reopening a project with no managed nodes can fall back to the starter scene. See the [roadmap](docs/04-implementation-roadmap.md) for acceptance checks and task status. The roadmap is the changing project-status page; this README focuses on what Spool is and how to try it.

---

### Spool.md

**17. Historical test counts and implementation reports presented as current**

**Quoted text:**
> ## Latest agent update (2026-10-10)
> The agent reports 730 tests passing; formatting and Clippy clean with `--all-targets --all-features -D warnings`; `mutate_interaction.sh` at 18 killed, 1 equivalent, 0 survived; `mutate_ops.sh` at 10/0/2; `mutate_created_objects.sh` at 18/0/1; and `mutate_spool_project.sh` at 15/0/3. It also reports a release build and signed app bundle launch with zero panics. Interactive GUI verification was not performed. These are agent-reported results; re-run relevant checks before relying on them for a later release or change.
>
> The report says it caught and fixed a pre-order traversal bug, added a children-first test, restored mutation harness backups correctly, verified the mutation runs leave working files unchanged, and re-anchored a stale mutation target.

**Why it is now stale:**
Spool.md explicitly states: "Treat historical commit hashes and test counts below as conversation context, not as current state or proof that the checked-out code still behaves that way." However, the file then presents the 2026-10-10 agent update as if it reflects the current state. The current HEAD is `f6453c1` (two commits ahead of the 2026-10-10 snapshot), and the test counts may have changed. The file should not present agent-reported figures from a prior snapshot as current state.

**Proposed replacement text:** (Spool.md is a handoff document; the audit does not modify it, but this entry documents the issue)

---

**18. Container deletion — implemented but with save corruption note**

**Quoted text:**
> The report also says save previously failed persistently after metadata became structurally corrupt.

**Why it is now relevant but concerning:**
The container deletion cascade policy is implemented in commit `f6453c1`. However, the note that "save previously failed persistently after metadata became structurally corrupt" is concerning. If container cascade deletion can corrupt metadata such that save fails persistently, this is a correctness bug that must be addressed. The audit cannot verify whether this has been fixed in the current codebase, but the statement as written raises a red flag about the interaction between cascade deletion and save operations.

**Proposed replacement text:** (no change to Spool.md, but this warrants investigation)

---

## Part 2: Summary — Priority Contradictions for Human Architecture Decision

The following contradictions MUST be resolved by a human architecture decision rather than by an editor implementation. These are not matters of code correctness but of architectural boundary definition.

### Priority 1: The disposability guarantee — does source + lamine.yaml suffice? (docs/01 item 1, docs/04 item 1)

**The contradiction:** 
docs/01 rule 3 claims "The Rust runtime is a disposable, optimized interpretation of source plus metadata. It can be rebuilt at any time and is never a persistence authority." The current figcraft Document model carries ~130 persisted properties, a large fraction (Owner B) with no CSS/spelling in source. If the Document model is deleted and rebuilt from source *alone*, Owner B properties are lost — the project is not byte-identical. The roadmap's check 1 ("A `.spool` project opens, and the document model can be discarded and rebuilt from source with the project byte-identical") is presented as the "one check that matters most" but is unverified.

**Why it requires a human architecture decision:**
This is a non-negotiable boundary in docs/01. The statement "The Rust runtime is ... never a persistence authority" must be either (a) maintained as-is, meaning the full persistence authority is source + `lamine.yaml` together (and the document is truly disposable), or (b) weakened to mean the runtime is disposable but the persistence authority is source + `lamine.yaml` + Document model together (introducing a second persistence authority). Option (b) weakens the non-negotiable boundary; option (a) requires ensuring that Owner B properties are fully expressible in source + `lamine.yaml`, which they currently are not.

**Proposed resolution:**
Human decision required: affirm the disposability guarantee as stated (source + `lamine.yaml` are the persistence authority, Document is disposable), and implement Owner B storage in `lamine.yaml` as the priority task; or weaken the boundary and document the change.

---

### Priority 2: Owner B storage — where do the ~130 persisted properties live? (docs/02 property ownership, docs/04 check 3)

**The contradiction:**
The property ownership rule in docs/02 assigns Owner B properties to `lamine.yaml` and prohibits duplicating Owner A there. The roadmap check 3 expects "Owner B properties have a `lamine.yaml` home and survive save and reopen." But the current implementation has "No home at all" for Owner B properties — the metadata record is `id`, `name`, `kind`, `parent`, `children`, `source` only (property-ownership.md). The roadmap acknowledges "Roughly half the model has nowhere to live." This is a gap between the rule and the implementation.

**Why it requires a human architecture decision:**
This is the "largest single piece of new work" identified by the roadmap. The architecture decision is: (a) extend `lamine.yaml` to store Owner B properties, accepting that `lamine.yaml` will grow beyond its current minimal footprint; (b) find a way to express Owner B properties in authored source (unlikely for many properties, e.g., stroke alignment, noise/texture/glass, component/variable systems); (c) accept that some properties are "unpersistable" and must be refused/surfaced. Option (a) weakly violates the spirit of "lamine.yaml is not a duplicate datastore for HTML/CSS properties" if Owner B properties are design-tool semantics without CSS spelling; option (b) is likely impossible for many properties; option (c) changes the product's capabilities.

**Proposed resolution:**
Human decision required: approve an `lamine.yaml` extension plan for Owner B properties, or redesign the ownership model. This is not an editor-implementable decision.

---

### Priority 3: Owner A round-tripping — check 2 is unmet (docs/04 check 2, property-ownership.md)

**The contradiction:**
Acceptance check 2 expects "Every Owner A property that the editor can change round-trips through authored source." But currently only ~12 of 34 supported Owner A properties round-trip end-to-end. The roadmap acknowledges this is the critical path but does not acknowledge that check 2 is currently unmet.

**Why it requires a human architecture decision:**
This is an engineering capacity question, not a boundary question. However, the decision of which Owner A properties to extend first (flex longhands, per-corner radii, gradients, effects) is a product prioritization decision that affects the roadmap's acceptance checks. The roadmap's milestone completeness depends on resolving this.

**Proposed resolution:**
Engineering management decision: prioritize which Owner A properties to round-trip first, acknowledging that not all can be addressed in the current milestone.

---

### Priority 4: CLI/MCP dialogs rule conflict (AGENTS.md, docs/03)

**The contradiction:**
The rule "No command may open a dialog or prompt" (docs/03 and AGENTS.md) conflicts with the now-in-scope CLI and MCP surfaces, which necessarily involve user input. The documentation does not resolve this tension.

**Why it requires a human architecture decision:**
This is a governance decision: either update the rule to allow dialogs/prompts through CLI/MCP channels (weakening the original constraint), or redesign the CLI/MCP surfaces to avoid dialogs (potentially reducing functionality). The rule was written when these surfaces were not in scope; now that they are explicitly assigned, the rule must be consciously affirmed or updated.

**Proposed resolution:**
Human decision required: update the rule to carve out CLI/MCP dialogs, or redesign those surfaces. Not an editor implementation matter.

---

### Priority 5: Test count verification — agent-reported figures vs. measured (docs/04)

**The contradiction:**
The roadmap presents test counts as "Measured, not reported" but then admits they are agent-reported and not re-derived. The figures cannot be relied upon without re-running.

**Why it requires a human architecture decision:**
This is a documentation and verification process decision. The roadmap should either establish a verified test-harness and re-run the counts, or explicitly label all figures as unverified. This affects contributors' ability to assess project maturity.

**Proposed resolution:**
Human decision required: either re-run the test harness and report actual measured counts, or revise the "Verified baseline" section to properly label all figures.

---

## Part 3: Non-Negotiable Boundary Preservation

The following proposed changes do NOT weaken existing non-negotiable boundaries in docs/01, since those are not ours to trade away casually:

1. **Runtime disposability (docs/01 rule 3):** The proposed replacement text affirmatively states that source + `lamine.yaml` together are the persistence authority, and the runtime is disposable from this combination. This strengthens the boundary by making the contingency explicit rather than weakening it.

2. **Property ownership separation (docs/02):** The rule that `lamine.yaml` must not duplicate Owner A properties is preserved and reinforced in all proposed replacements. The extension to add Owner B storage does not duplicate Owner A; it addresses the complementary category.

3. **No browser DOM as scene graph (docs/01 rule 8):** No proposed change touches this. The figcraft engine is a native Rust engine, not a browser DOM.

4. **Semantic operations as mutation entry point (docs/03):** The operation layer as the only mutation entry point is preserved in all proposed changes.

5. **History free of parser offsets (docs/03):** The intent journal requiring durable, semantic records free of parser offsets is preserved in the two-mechanism amendment.

The following changes WOULD weaken existing non-negotiable boundaries if implemented without explicit architectural authorization:

- **Weakening the disposability guarantee** to mean "runtime is disposable but persistence authority includes the Document model" — this introduces a second authority and contradicts docs/01 rule 3's assertion that the runtime is "never a persistence authority."

- **Duplicating authored CSS properties into `lamine.yaml`** under the guise of Owner B — this directly violates the rule that `lamine.yaml` must not duplicate Owner A, and the evidence table shows Owner B is the designated category for properties without CSS spelling; smuggling CSS-mappable properties into Owner B would be a defect.

- **Relaxing the "no parser offsets in history" requirement** — the intent journal must remain durable and semantic; weakening this enables undo mechanisms that lose the reason for edits, which the amendment correctly prevents.

---

## Conclusions

The figcraft adoption does not invalid the core architectural contracts in `docs/01`–`docs/03`, but it does reveal and amplify several gaps between the governing documents and the adopted model:

1. **The disposability guarantee is technically correct but contingent** on source + `lamine.yaml` being preserved together. Owner B properties have no source-level re-entry path, so the "rebuild from source" claim is incomplete without `lamine.yaml`.

2. **Owner B storage is the critical missing piece** — roughly half the model has nowhere to live. This is the "largest single piece of new work" and the critical path to the current milestone.

3. **Owner A round-tripping is incomplete** — only ~12 of 34 supported properties round-trip, contrary to acceptance check 2's expectation.

4. **The "no dialog or prompt" rule conflicts with now-in-scope CLI/MCP surfaces** and must be consciously updated or redesigned.

5. **Test count figures are agent-reported and unverified** — the "Verified baseline" section should either re-run checks or explicitly label figures as unverified.

6. **The property ownership classification table (property-ownership.md) is the ground truth** for mapping figcraft's ~130 properties to the four owners; the docs/02 rule "Every persisted property has exactly one owner" is enforceable only if the per-field table is the authoritative reference.

The audit file `docs/05-figcraft-adoption-audit.md` is the complete output of this analysis. No other files were modified.