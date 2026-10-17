# Editor, runtime, operations, and history

## Layers

1. **Source project:** HTML/CSS/SVG and `lamine.yaml`; saved and human-readable.
2. **Document model:** validated persistent semantic nodes with stable IDs and source provenance.
3. **Operation layer:** typed commands with validation, before/after values, and explicit failure. This is the only mutation entry point.
4. **History:** a durable semantic intent journal, with structural-sharing snapshots as the in-memory undo mechanism. No-op operations do not enter history; a new committed edit clears redo.
5. **Editor state:** transient selection, camera, active tool, gestures, text caret, and guides.
6. **Runtime/renderer:** disposable geometry, hit-test data, layout, and GPUI drawing. It observes document revisions and rebuilds only affected derived state.

## Operation contract

An operation resolves a stable Spool ID, validates its target and source ownership, computes a source/metadata patch, applies it as one unit, and returns a reversible semantic record. The same operation must be callable from a pointer gesture, keyboard action, menu, automation, or future agent. A failed operation changes neither project source nor metadata nor history.

### The command registry

Operations are addressed by a **stable string id** — `node.create`, `align.left`,
`component.detach` — rather than by an in-language enum. Each registered command
declares its id, a label, parameter documentation for agents, and whether it edits
the document.

This is not a stylistic preference. It is what makes the rule above enforceable:
a UI, a CLI, a JSON control channel and an MCP server can all name the same
operation, so "every menu item, tool and drag is a command" becomes a checkable
property instead of an aspiration. Panels must never mutate the document directly.

**No command may open a dialog or prompt.** A command that needs user input
returns a request; the surface that invoked it decides how to ask. Programmatic
callers — CLI, MCP, future agents — drop those requests rather than blocking on
a UI they do not have.

Pointer gestures and text editing are not exempt. A drag reduces to the same
document edits and is journalled by the same path as a menu click.

> Historical note: this document previously scoped the work to "identity/binding
> plus one semantic edit that exercises the whole path (rename a node in
> metadata)". That slice landed. It is retained here only so the sequence stays
> readable; it no longer describes the current state.

## Interaction and history

Continuous gestures may update transient previews. Pointer-up commits one operation; Escape restores the pre-gesture value without creating an entry. History stores IDs and semantic values, never byte offsets. Re-resolve source locations at application time.

### Undo mechanism and the durable intent record

**Amended 2026-10-17.** The original wording was "Undo applies the operation's
`before` state; redo applies `after`." That prescribed a particular undo
*mechanism* — an inverse operation per edit — which turned out to be the wrong
requirement to write down, and it was amended rather than quietly dropped.

The requirement is that history be **durable, semantic, and free of parser
offsets**. How undo is *computed in memory* is an implementation decision. Spool
now uses two mechanisms, and both are required:

- **In-memory undo is a structural-sharing document snapshot.** Adopting the
  figcraft engine brought `Arc`-based snapshot history with it. This is the
  undo mechanism. It is correct for every command in the registry without each
  command having to author an inverse, and it makes deep and compound history
  correct by construction rather than by review.
- **The durable record is a semantic intent journal.** Every commit also
  records an intent: the command id, its parameters, a human label, the
  affected node ids, and a summary of the source patch. This is what history
  *means*. It serializes, survives process restart, and is inspectable.

The two are not alternatives. A snapshot that is not accompanied by an intent
record loses the reason for the edit and cannot be replayed by an agent; an
intent journal that is also an undo mechanism has to carry inverses for the
entire command surface, which is where correctness risk actually lives.

Consequences that are binding on implementation:

- History entries must expose the intent, not only a label. An agent must be
  able to ask what a given undo step did, and re-run it.
- The intent journal is part of the saved project. An agent's session must
  outlive the process that produced it.
- Snapshots remain an in-memory mechanism. They are never serialized, and
  deleting them must not lose user work.
- No byte offsets in either the intent record or the snapshot. Source
  locations are re-resolved at application time.

## Runtime and renderer

The runtime is constructed from validated source and metadata, and may cache resolved layout and paint data. It does not rewrite canonical source merely by loading. GPUI is the current frontend, not the document model. Keep renderer-independent operations and model tests possible without opening a window.

## Failure and external edits

Parsing, validation, and source patching return structured errors. Do not partially apply multi-file edits. External edits invalidate affected bindings and derived data; preserve the user's bytes and report conflicts for explicit resolution. Never silently pick one of multiple matching CSS declarations.
