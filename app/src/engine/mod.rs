//! The GPUI frontend, built on the vendored figcraft engine.
//!
//! This module tree is the replacement for Spool's original editor runtime. The
//! old implementation spread across `canvas.rs` (16k lines), `visual.rs`,
//! `document_runtime_bridge.rs`, `hierarchy.rs` and `snap.rs`, and implemented
//! geometry, hit-testing, snapping, gestures and painting by hand against a
//! private runtime model.
//!
//! Everything here delegates to the engine instead:
//!
//! ```text
//!   GPUI  ──pointer/keys──>  figcraft_engine::Session::pointer / ::execute
//!                                │
//!                                ├─ figcraft_doc::Document      (the model)
//!                                ├─ figcraft_layout             (auto-layout)
//!                                ├─ figcraft_text               (shaping, layout)
//!                                ├─ figcraft_pathops            (booleans, strokes)
//!                                └─ figcraft_render::Retained   (pixels)
//!                                                 │
//!                                                 └─> GPUI texture + overlays
//! ```
//!
//! The consequence worth stating: **nothing in this tree mutates the document
//! directly.** Every edit is a command id executed through `Session::execute`,
//! which is what keeps the UI, the CLI, the MCP server and a future agent on one
//! operation surface.
//!
//! ## Why a new tree rather than an in-place rewrite
//!
//! The old files are large, heavily tested, and still green. Editing them in
//! place would make every porting mistake indistinguishable from an intended
//! behaviour change. Building beside them keeps the old path runnable until the
//! swap, and makes the swap one reviewable step.
//!
//! ## Ported from egui
//!
//! figcraft's frontend cannot be reused as code — only as specification. What
//! was reusable outright is the engine side: the menu catalog (`catalog::MENU`),
//! the keymap (`keymap::KEYS`) and the command registry are plain data with no
//! egui in them, and are read at runtime rather than copied.
//!
//! ## Two traps in the keymap port
//!
//! GPUI's `Keystroke::unparse` spells the platform modifier `cmd-` / `super-` /
//! `win-`, while figcraft's bindings are always `cmd-*`. Unnormalised, every
//! shortcut would silently fail to match off macOS.
//!
//! GPUI's keystroke parser reads an upper-case single character as Shift+key,
//! so emitting figcraft's `Cmd+K` verbatim would add Shift to every letter
//! shortcut. Both are handled, and both are pinned by tests over the whole table.

pub mod keymap;
pub mod layers;
pub mod menus;
pub mod palette;
