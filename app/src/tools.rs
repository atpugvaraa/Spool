//! The editor's tools, and the ladder that walks a selection.
//!
//! These two lived in the retired hand-written runtime (`canvas.rs`) and are
//! the only things from it that were never about the document: a *tool* names
//! what a pointer gesture will do, and a *traversal* names one step through
//! the hierarchy. Neither is document state and neither is runtime state — they
//! are vocabulary, and the vocabulary outlives the runtime it was written in.
//!
//! So they live here now, in a module of their own, and both the toolbar and
//! the command table speak them. The editor's runtime is the figcraft engine;
//! the names in [`Tool`] are Spool's, and [`crate::shell::engine_tool_name`]
//! is where they are translated to the engine's.

/// A tool the editor offers.
///
/// Named for what the user is about to do, not for the widget on the toolbar:
/// the icon and the letter are presentation, and they are decided in the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// The pointer selects and moves. The engine calls this `move`.
    Select,
    /// Draw a frame.
    Frame,
    /// Draw a rectangle.
    Rectangle,
    /// Draw an ellipse.
    Ellipse,
    /// Draw a vector path.
    Pen,
    /// Place text.
    Text,
    /// Leave a comment.
    Comment,
}

/// A keyboard step through the selection.
///
/// Named for the move it makes rather than the key that reaches it, so the
/// grammar can be tested without a window and the bindings can change without
/// touching the rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Traversal {
    /// The next or previous sibling. Figma's `Tab` / `⇧Tab`.
    Sibling(bool),
    /// Into the first child. Figma's `Enter`.
    Descend,
    /// Out to the parent. Figma's `⇧Enter`.
    Ascend,
}
