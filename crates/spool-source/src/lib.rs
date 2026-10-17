//! Spool's source-backed layer.
//!
//! The engine document model is disposable. This crate is what makes it
//! disposable: it compiles authored HTML/CSS/SVG plus `lamine.yaml` into that
//! model, and compiles the model back out again.
//!
//! ```text
//! HTML / CSS / SVG ─┐
//!                   ├─> figcraft_doc::Document ─> engine ─> renderer
//! lamine.yaml ──────┘
//! ```
//!
//! # The rule this crate exists to enforce
//!
//! Every persisted property has exactly one owner. The categories are defined in
//! `docs/02-document-and-source-model.md` and the evidence for each assignment is
//! in `docs/research/document/property-ownership.md`:
//!
//! | Owner | Where it lives |
//! |---|---|
//! | **A** | authored HTML/CSS/SVG |
//! | **B** | Spool-specific facts in `lamine.yaml` |
//! | **C** | derived; recomputable, never stored as independent truth |
//! | **D** | session state; never persisted |
//!
//! This crate owns the **B** side and the compile in both directions. Assigning a
//! property to two owners is a defect.
//!
//! # Identity
//!
//! There are two ids, and they are not interchangeable.
//!
//! The **model id** (`figcraft_doc::NodeId`, a `u64`) is the engine's identity.
//! It is owned by `lamine.yaml`, is opaque, and is never derived from a name,
//! DOM order, a CSS class, or the source key. The engine uses it everywhere and
//! cannot be made to use anything else without editing vendored code, which
//! [`Agents.md`](../../Agents.md) forbids.
//!
//! The **source key** (a string) is the authored identity. It is what appears in
//! markup as `data-spool-id="<key>"`, and it is what a person's selector in
//! `lamine.yaml` resolves against. It is chosen by whoever authored the markup
//! and may be renamed freely without touching the model.
//!
//! Keeping them separate is what lets an existing project with readable keys like
//! `spool-frame-root` adopt the engine without rewriting its markup, while still
//! giving the engine the numeric identity it requires.
//!
//! [`Agents.md`]: ../../Agents.md

#![deny(unsafe_code)]

pub mod manifest;

pub use manifest::{Manifest, ManifestError, NodeRecord, SourceBinding};
