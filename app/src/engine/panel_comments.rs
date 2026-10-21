//! The Comments section: figcraft's comments panel, ported to GPUI.
//!
//! # What this module owns
//!
//! Two things, in one file because they are one thing:
//!
//! 1. **The model.** [`CommentsModel`] reads the engine's [`Document`] and turns
//!    the document's comment threads, and the selected layers' own comment,
//!    into what the panel shows.
//! 2. **The panel.** [`comments_section`] draws that model and asks the engine
//!    for the changes the user makes.
//!
//! # The reference
//!
//! figcraft's comments UI — `crates/ui-egui/src/comments.rs` — is the
//! specification. The **Comments panel** (the one that stands in place of the
//! Design panel while the Comment tool is active) is the part this module
//! ports: its list, its rows, its filter and sort menu, its search field, its
//! labels, its actions and its two empty states. The engine's own model is the
//! arbiter: [`figcraft_doc::Comment`] says what a thread is, and the engine's
//! commands say what a change is.
//!
//! # The source boundary
//!
//! **Spool's source can express a comment.** A comment is an Owner B fact —
//! the design-tool fact that HTML and CSS cannot state — and `lamine.yaml` v2
//! has the place for it: the **document-level `comments:` list**, whose
//! entries are the engine's own `figcraft_doc::Comment` values, verbatim.
//!
//! A comment names the layer it belongs to by id, not by nesting: a node
//! record has no comments field, and a thread carries `Comment::anchor` (the
//! layer it follows) and `Comment::page` (the page it is pinned to). The
//! compile path reads the list into the engine's document
//! (`spool_source::reader`), so a project that carries its comments opens with
//! them intact.
//!
//! **The save path does not write them back.** It carries the document-level
//! fields across untouched, so a comment made or changed here stays in the
//! editor's document. That is said where the change is made, rather than left
//! for the user to find out by saving — the same note the Variables section
//! carries for shared styles.
//!
//! # Where this differs from figcraft, and why
//!
//! - **The list is the document's, not the page's.** figcraft's panel queries
//!   every page (`Query { page: None, .. }`) and names the page in a row's
//!   meta line when the thread is on another page. Spool's panel does the
//!   same, so the engine's own list and the panel's list are one list.
//! - **The selected layers get their own block.** figcraft's panel is
//!   document-only; Spool's Inspector is about the selection, so the section
//!   also reports the selected layers' own comment — with the Inspector's
//!   three states, folded **per property**.
//! - **The actions are always shown.** figcraft reveals a message's buttons on
//!   hover; a hover-only control cannot be reached without a pointer.
//!
//! # Commands, not mutations
//!
//! Every change leaves through the engine's command surface: `comment.open`,
//! `comment.resolve`, `comment.reply`, `comment.edit`, `comment.react`,
//! `comment.delete`, `comment.markRead` and `comment.link`. Nothing here
//! writes to a [`Document`] directly, so an edit made from this panel, from the
//! CLI and from an agent are the same edit and land in the same history.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::rc::Rc;

use figcraft_doc::{Comment, Document, NodeId};
use figcraft_engine::cmd::comment::{self, Filter, Sort};
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, FocusHandle, SharedString};
use serde_json::{json, Value};

use crate::theme;

// ═══════════════════════════════════════════════════════════════════════════
// The words
// ═══════════════════════════════════════════════════════════════════════════

/// The word for a property the selected layers disagree about.
///
/// The Inspector's own word, and figcraft's: a value and a nothing would leave
/// a panel free to invent a third thing at every row that faced a
/// disagreement.
pub const MIXED: &str = "Mixed";

/// The word for a property nobody has set.
///
/// figcraft's word, not Spool's: a property that is not there is "Not set",
/// which says that there is nothing to read rather than that the answer is
/// zero.
pub const NOT_SET: &str = "Not set";

/// figcraft's own words, word for word, for a document with no comments.
pub const NO_COMMENTS_YET: &str = "No comments yet";

/// figcraft's own words, word for word, for a search that finds nothing.
pub const NO_MATCHING_COMMENTS: &str = "No matching comments";

/// The second line of figcraft's "no comments yet" state.
pub const CLICK_TO_LEAVE_ONE: &str = "Click anywhere on the canvas to leave one.";

/// The second line of figcraft's "no matching comments" state.
pub const TRY_ANOTHER_SEARCH: &str = "Try another search or filter.";

/// The hint on the search field.
pub const SEARCH_COMMENTS: &str = "Search comments";

/// The way back to figcraft's usual filter.
pub const SHOW_OPEN: &str = "Show open";

/// What the save path does with a comment today.
///
/// The metadata can carry one — `lamine.yaml` has a document-level `comments:`
/// list of the engine's own comment values — and the compile path reads it. The
/// save path does not write it back: it carries the document-level fields
/// across untouched. Said where the change is made, rather than left for the
/// user to find out by saving.
pub const SAVE_PATH_NOTE: &str =
    "Comments are not written back to lamine.yaml yet, so a comment made or edited here stays \
     in the editor's document.";

// ═══════════════════════════════════════════════════════════════════════════
// The three states
// ═══════════════════════════════════════════════════════════════════════════

/// One property's state across a selection.
///
/// Three states, never two: a value every selected layer agrees on, agreement
/// that nobody set it, and the absence of agreement. The Inspector's own
/// properties have the same three, and the same words, so "Mixed" means one
/// thing in the panel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CommentState<T> {
    /// Every selected layer has this value, and they agree on it.
    Set(T),
    /// No selected layer has this value.
    #[default]
    Unset,
    /// The selected layers disagree.
    Mixed,
}

impl<T: PartialEq> CommentState<T> {
    /// Fold a selection's readings into the one state that is honest for all
    /// of them.
    ///
    /// `None` for a layer means "this layer has no comment", which is an
    /// answer rather than missing data — so the fold is over `Option<T>` and
    /// never has to invent a default.
    pub fn across(readings: impl IntoIterator<Item = Option<T>>) -> Self {
        let mut readings = readings.into_iter();
        // An empty selection has no reading at all. `Unset` is the honest
        // answer: nothing is set, and nothing claims to be.
        let Some(first) = readings.next() else {
            return Self::Unset;
        };
        readings.fold(Self::from(first), |state, next| {
            let next = Self::from(next);
            match (state, next) {
                // A disagreement can only ever widen. Once two layers differ,
                // no later agreement can make the selection uniform again.
                (Self::Mixed, _) | (_, Self::Mixed) => Self::Mixed,
                (state, next) if state == next => state,
                _ => Self::Mixed,
            }
        })
    }

    fn from(reading: Option<T>) -> Self {
        match reading {
            Some(value) => Self::Set(value),
            None => Self::Unset,
        }
    }

    /// The value, when the selection agrees on one.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Set(value) => Some(value),
            _ => None,
        }
    }

    /// Whether the selection disagrees about this property.
    pub fn is_mixed(&self) -> bool {
        matches!(self, Self::Mixed)
    }

    /// Whether no selected layer has this property set.
    pub fn is_unset(&self) -> bool {
        matches!(self, Self::Unset)
    }

    /// The word the panel shows for a state that is not a value.
    ///
    /// Empty for a value: the value says its own name.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Set(_) => "",
            Self::Unset => NOT_SET,
            Self::Mixed => MIXED,
        }
    }
}

/// One property's value.
///
/// The three kinds a comment's properties hold: a name or a piece of text, a
/// count, and a yes or a no.
#[derive(Clone, Debug, PartialEq)]
pub enum CommentValue {
    /// A name or a piece of text.
    Text(String),
    /// A count.
    Count(usize),
    /// A yes or a no.
    Flag(bool),
}

/// One property of a layer's comment.
///
/// The properties the engine's own [`Comment`] holds, named the engine's way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentProperty {
    /// Who wrote the comment.
    Author,
    /// What the comment says.
    Text,
    /// How many replies the thread has.
    Replies,
    /// Whether the thread is resolved.
    Resolved,
    /// Whether the thread is unread for the user.
    Unread,
}

impl CommentProperty {
    /// Every property the panel reports, in the order it reports them.
    pub const ALL: [CommentProperty; 5] = [
        CommentProperty::Author,
        CommentProperty::Text,
        CommentProperty::Replies,
        CommentProperty::Resolved,
        CommentProperty::Unread,
    ];

    /// The property's label, as the panel shows it.
    pub fn label(self) -> &'static str {
        match self {
            CommentProperty::Author => "Author",
            CommentProperty::Text => "Comment",
            CommentProperty::Replies => "Replies",
            CommentProperty::Resolved => "Resolved",
            CommentProperty::Unread => "Unread",
        }
    }

    /// The property's name in a control id, for a test to look it up.
    pub fn slug(self) -> &'static str {
        match self {
            CommentProperty::Author => "author",
            CommentProperty::Text => "comment",
            CommentProperty::Replies => "replies",
            CommentProperty::Resolved => "resolved",
            CommentProperty::Unread => "unread",
        }
    }

    /// The property's value in one thread.
    pub fn value(self, thread: &CommentThread) -> CommentValue {
        match self {
            CommentProperty::Author => CommentValue::Text(thread.author().to_owned()),
            CommentProperty::Text => CommentValue::Text(thread.text().to_owned()),
            CommentProperty::Replies => CommentValue::Count(thread.replies()),
            CommentProperty::Resolved => CommentValue::Flag(thread.resolved()),
            CommentProperty::Unread => CommentValue::Flag(thread.unread),
        }
    }

    /// The value, as the panel shows it.
    pub fn display(self, value: &CommentValue) -> String {
        match (self, value) {
            (CommentProperty::Replies, CommentValue::Count(count)) => count.to_string(),
            (CommentProperty::Resolved, CommentValue::Flag(resolved)) => {
                if *resolved { "Resolved" } else { "Unresolved" }.to_owned()
            }
            (CommentProperty::Unread, CommentValue::Flag(unread)) => {
                if *unread { "Unread" } else { "Read" }.to_owned()
            }
            (_, CommentValue::Text(text)) => text.clone(),
            (_, CommentValue::Count(count)) => count.to_string(),
            (_, CommentValue::Flag(flag)) => if *flag { "Yes" } else { "No" }.to_owned(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The model
// ═══════════════════════════════════════════════════════════════════════════

/// One comment thread, as the panel shows it.
///
/// The engine's own [`Comment`], with the names of the layers it is pinned to
/// read out of the document — the panel shows a layer's name, never an id.
#[derive(Clone, Debug, PartialEq)]
pub struct CommentThread {
    /// The engine's own thread.
    pub comment: Comment,
    /// The page the thread is pinned to.
    pub page: NodeId,
    /// That page's name.
    pub page_name: String,
    /// The top-level frame the thread is on, when it is on one.
    ///
    /// The engine's own answer: [`comment::frame_of`].
    pub frame: Option<NodeId>,
    /// That frame's name.
    pub frame_name: Option<String>,
    /// Whether the thread is unread for the user.
    pub unread: bool,
}

impl CommentThread {
    /// Read one thread out of the document.
    fn read(doc: &Document, comment: &Comment, user: &str) -> Self {
        let frame = comment::frame_of(doc, comment);
        Self {
            comment: comment.clone(),
            page: comment.page,
            page_name: doc
                .node(comment.page)
                .map(|node| node.name.clone())
                .unwrap_or_default(),
            frame,
            frame_name: frame
                .and_then(|frame| doc.node(frame))
                .map(|node| node.name.clone()),
            unread: comment.is_unread(user),
        }
    }

    /// Who wrote the thread.
    pub fn author(&self) -> &str {
        &self.comment.first.author
    }

    /// What the thread says.
    pub fn text(&self) -> &str {
        &self.comment.first.text
    }

    /// How many replies the thread has.
    pub fn replies(&self) -> usize {
        self.comment.replies.len()
    }

    /// Whether the thread is resolved.
    pub fn resolved(&self) -> bool {
        self.comment.resolved
    }

    /// When the thread last had a message.
    pub fn last_activity(&self) -> u64 {
        self.comment.last_activity()
    }

    /// Whether the user wrote in the thread or is named in it.
    pub fn is_mine(&self, user: &str) -> bool {
        self.comment.involves(user)
    }

    /// Whether the thread matches a search.
    ///
    /// The engine's own rule: a message's text, a message's author, or the
    /// name of the frame the thread is on.
    pub fn matches(&self, needle: &str) -> bool {
        let has = |text: &str| text.to_lowercase().contains(needle);
        self.comment
            .messages()
            .any(|message| has(&message.text) || has(&message.author))
            || self.frame_name.as_ref().is_some_and(|name| has(name))
    }
}

/// One selected layer, and the comment that is its own.
///
/// A layer's comment is its **first** thread: the first thread the engine
/// holds that belongs to it, in the engine's own order. A layer with no
/// thread has no comment, and that is a state and not a missing answer.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerComment {
    /// The layer.
    pub node: NodeId,
    /// The layer's name.
    pub name: String,
    /// Whether the layer is a page.
    pub is_page: bool,
    /// The comment that is the layer's, if it has one.
    pub comment: Option<CommentThread>,
}

impl LayerComment {
    /// Read one layer's comment out of the document's threads.
    fn read(doc: &Document, threads: &[CommentThread], node: NodeId) -> Self {
        let layer = doc.node(node);
        let is_page = layer.is_some_and(|node| node.is_page());
        // The engine's own rules for what a thread belongs to: it follows the
        // layer, it is on the layer's frame, or it is pinned to the layer's
        // page.
        let comment = threads
            .iter()
            .find(|thread| {
                thread.comment.anchor == Some(node)
                    || thread.frame == Some(node)
                    || (is_page && thread.comment.page == node)
            })
            .cloned();
        Self {
            node,
            name: layer.map(|node| node.name.clone()).unwrap_or_default(),
            is_page,
            comment,
        }
    }
}

/// What the Comments section shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommentsModel {
    /// The document's threads, in the engine's own order.
    pub threads: Vec<CommentThread>,
    /// The selected layers, with the comment that is theirs.
    pub layers: Vec<LayerComment>,
    /// The page the editor is on.
    ///
    /// `None` when the editor has no page: every row then names its page, the
    /// way figcraft's panel does for a thread on another page.
    pub page: Option<NodeId>,
    /// The name comments are signed with.
    pub user: String,
    /// The document's pages, in the engine's order.
    pages: Vec<NodeId>,
}

impl CommentsModel {
    /// The document's comments, and the selected layers' own comment.
    ///
    /// A comment on a page the document does not hold any more is not a
    /// comment the panel can show, so it is left out — the engine's own rule.
    pub fn read(doc: &Document, selection: &[NodeId], page: Option<NodeId>, user: &str) -> Self {
        let threads: Vec<CommentThread> = doc
            .comments
            .iter()
            .filter(|comment| doc.pages.contains(&comment.page))
            .map(|comment| CommentThread::read(doc, comment, user))
            .collect();
        let layers = selection
            .iter()
            .map(|node| LayerComment::read(doc, &threads, *node))
            .collect();
        Self {
            threads,
            layers,
            page,
            user: user.to_owned(),
            pages: doc.pages.clone(),
        }
    }

    /// The threads the list shows: the ones the filter keeps, in the sort's
    /// order.
    ///
    /// The engine's own rules, in the engine's own order: the filter is
    /// [`Filter::keeps`], the search matches a message, an author or the
    /// frame's name, and the sort is by the thread's last activity or by the
    /// page it is on.
    pub fn visible(&self, filter: Filter, sort: Sort, query: &str) -> Vec<&CommentThread> {
        let needle = query.trim().to_lowercase();
        let mut list: Vec<&CommentThread> = self
            .threads
            .iter()
            .filter(|thread| filter.keeps(&thread.comment, &self.user))
            .filter(|thread| needle.is_empty() || thread.matches(&needle))
            .collect();
        match sort {
            Sort::Date => list.sort_by_key(|thread| {
                (Reverse(thread.last_activity()), Reverse(thread.comment.id))
            }),
            Sort::Page => list.sort_by_key(|thread| {
                (
                    self.page_index(thread.page),
                    Reverse(thread.last_activity()),
                    Reverse(thread.comment.id),
                )
            }),
        }
        list
    }

    /// The reading of one property across the selection.
    pub fn reading(&self, property: CommentProperty) -> CommentState<CommentValue> {
        CommentState::across(
            self.layers
                .iter()
                .map(|layer| layer.comment.as_ref().map(|thread| property.value(thread))),
        )
    }

    /// Whether the document holds a comment at all.
    ///
    /// figcraft's own test for which empty state to show.
    pub fn has_any(&self) -> bool {
        !self.threads.is_empty()
    }

    /// The empty state the list shows, word for word.
    ///
    /// figcraft's two: a document with no comments says so, and a document
    /// that has some the filter or the search leaves out says that instead.
    pub fn empty_state(&self) -> (&'static str, &'static str) {
        if self.has_any() {
            (NO_MATCHING_COMMENTS, TRY_ANOTHER_SEARCH)
        } else {
            (NO_COMMENTS_YET, CLICK_TO_LEAVE_ONE)
        }
    }

    /// Where a page sits in the document's page order.
    fn page_index(&self, page: NodeId) -> usize {
        self.pages
            .iter()
            .position(|known| *known == page)
            .unwrap_or(usize::MAX)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The commands
// ═══════════════════════════════════════════════════════════════════════════

/// A change the Comments section can ask for.
///
/// One variant per thing the section can change, so a change the engine has
/// no command for cannot be written down as though it had one.
#[derive(Clone, Debug, PartialEq)]
pub enum CommentsAction {
    /// Open a thread and bring it into view.
    Open {
        /// The thread.
        id: u64,
    },
    /// Resolve a thread, or leave it unresolved.
    Resolve {
        /// The thread.
        id: u64,
        /// Whether the thread is resolved.
        resolved: bool,
    },
    /// Reply to a thread.
    Reply {
        /// The thread.
        id: u64,
        /// The reply.
        text: String,
    },
    /// Change a message of a thread.
    Edit {
        /// The thread.
        id: u64,
        /// The message's new text.
        text: String,
        /// Which message: the thread's first one, or a reply by index.
        ///
        /// `None` is the thread's first message — the engine's own spelling,
        /// which omits `reply` rather than sending a zero.
        reply: Option<usize>,
    },
    /// React to a message of a thread.
    React {
        /// The thread.
        id: u64,
        /// The emoji.
        emoji: String,
        /// Which message: the thread's first one, or a reply by index.
        reply: Option<usize>,
    },
    /// Delete a thread, or one of its replies.
    Delete {
        /// The thread.
        id: u64,
        /// Which message: the thread's first one, or a reply by index.
        ///
        /// `None` deletes the whole thread.
        reply: Option<usize>,
    },
    /// Mark a thread read, or unread.
    MarkRead {
        /// The thread.
        id: u64,
        /// Whether the thread is read.
        read: bool,
    },
    /// Mark every thread read.
    MarkAllRead,
    /// Copy a thread's link.
    Link {
        /// The thread.
        id: u64,
    },
}

impl CommentsAction {
    /// The engine's command id and parameters for this change.
    ///
    /// The engine's own ids and the engine's own parameter spellings, so a
    /// change made from the panel is the same change the CLI, the keymap and
    /// an agent would make.
    pub fn command(&self) -> (String, Value) {
        match self {
            Self::Open { id } => (
                "comment.open".to_owned(),
                json!({ "id": *id, "view": "center" }),
            ),
            Self::Resolve { id, resolved } => (
                "comment.resolve".to_owned(),
                json!({ "id": *id, "resolved": *resolved }),
            ),
            Self::Reply { id, text } => (
                "comment.reply".to_owned(),
                json!({ "id": *id, "text": text }),
            ),
            Self::Edit { id, text, reply } => {
                let mut params = json!({ "id": *id, "text": text });
                if let Some(reply) = reply {
                    params["reply"] = json!(reply);
                }
                ("comment.edit".to_owned(), params)
            }
            Self::React { id, emoji, reply } => {
                let mut params = json!({ "id": *id, "emoji": emoji });
                if let Some(reply) = reply {
                    params["reply"] = json!(reply);
                }
                ("comment.react".to_owned(), params)
            }
            Self::Delete { id, reply } => {
                let mut params = json!({ "id": *id });
                if let Some(reply) = reply {
                    params["reply"] = json!(reply);
                }
                ("comment.delete".to_owned(), params)
            }
            Self::MarkRead { id, read } => (
                "comment.markRead".to_owned(),
                json!({ "id": *id, "read": *read }),
            ),
            Self::MarkAllRead => ("comment.markRead".to_owned(), json!({})),
            Self::Link { id } => ("comment.link".to_owned(), json!({ "id": *id })),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// What a surface that hosts the Comments section must be able to do.
///
/// The panel draws and asks; the surface owns the engine and runs the changes.
/// The panel never touches the document itself, so the engine's command
/// surface stays the one operation boundary.
pub trait CommentsHost: Sized + 'static {
    /// Run a change through the engine.
    fn run_comments_action(&mut self, action: CommentsAction, cx: &mut Context<Self>);
}

/// Which menu of the panel is open.
///
/// One menu at a time: a second one opening closes the first, which is what a
/// menu that opens another does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommentsMenu {
    /// The filter and sort menu in the header.
    Header,
    /// The actions on one thread.
    Thread(u64),
}

/// The panel's own state.
///
/// Presentation only: never in the document.
#[derive(Clone, Debug, PartialEq)]
pub struct CommentsPanelState {
    /// The filter the list shows.
    pub filter: Filter,
    /// The sort the list is in.
    pub sort: Sort,
    /// The search text the list is filtered by.
    ///
    /// The search narrows the list as it is typed, the way figcraft's does.
    pub query: String,
    /// Which menu is open, if any.
    pub menu: Option<CommentsMenu>,
    /// The keyboard focus the search field takes.
    focus: Option<FocusHandle>,
}

impl Default for CommentsPanelState {
    fn default() -> Self {
        Self {
            // figcraft's panel opens on the open threads, sorted by date.
            filter: Filter::Open,
            sort: Sort::Date,
            query: String::new(),
            menu: None,
            focus: None,
        }
    }
}

impl CommentsPanelState {
    /// The state a section starts in.
    pub fn new() -> Self {
        Self::default()
    }
}

/// The Comments section, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own state,
/// and the surface that runs the changes. It returns an element, so the
/// Inspector can put it in a section of its own and a test can put it in a
/// window on its own.
pub fn comments_section<T: CommentsHost>(
    model: &CommentsModel,
    state: &Rc<RefCell<CommentsPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let focus = focus_of(state, cx);
    let (filter, sort, query) = {
        let state = state.borrow();
        (state.filter, state.sort, state.query.clone())
    };
    let mut column = div().flex().flex_col().gap_2().px(px(14.0)).py(px(12.0));
    column = column.child(header(state, cx));
    if state.borrow().menu == Some(CommentsMenu::Header) {
        column = column.child(header_menu(state, cx));
    }
    column = column.child(search_field(state, focus.as_ref(), cx));
    // Another filter than the usual one says so, with a way back.
    if filter != Filter::Open {
        column = column.child(filter_notice(state, cx));
    }
    let threads = model.visible(filter, sort, &query);
    if threads.is_empty() {
        column = column.child(empty_state(model, cx));
    } else {
        for thread in threads {
            column = column.child(thread_row(model, thread, state, cx));
            if state.borrow().menu == Some(CommentsMenu::Thread(thread.comment.id)) {
                column = column.child(thread_menu(model, thread, state, cx));
            }
        }
    }
    column = column.child(selection_block(model, cx));
    column = column.child(note(SAVE_PATH_NOTE));
    column.into_any_element()
}

/// The keyboard focus the search field takes.
///
/// Made once, when the section is first drawn: a focus handle belongs to the
/// window it was made in, so a new one on every frame would lose the focus the
/// user had.
fn focus_of<T: CommentsHost>(
    state: &Rc<RefCell<CommentsPanelState>>,
    cx: &mut Context<T>,
) -> Option<FocusHandle> {
    let mut state = state.borrow_mut();
    if state.focus.is_none() {
        state.focus = Some(cx.focus_handle());
    }
    state.focus.clone()
}

/// The header: the title, and the way to filter and sort.
fn header<T: CommentsHost>(
    state: &Rc<RefCell<CommentsPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let open = state.borrow().menu == Some(CommentsMenu::Header);
    let shared = Rc::clone(state);
    let mut row = div().flex().items_center().justify_between().gap_2();
    row = row.child(
        div()
            .id(SharedString::from("comments-title"))
            .debug_selector(|| "comments-title".to_owned())
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgb(theme::TEXT))
            .child("Comments"),
    );
    row = row.child(
        div()
            .id(SharedString::from("comments-menu"))
            .debug_selector(|| "comments-menu".to_owned())
            .flex()
            .items_center()
            .justify_center()
            .w(px(20.0))
            .h(px(20.0))
            .rounded_sm()
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child(if open { "▾" } else { "⋯" })
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.menu = if open {
                        None
                    } else {
                        Some(CommentsMenu::Header)
                    };
                }
                cx.notify();
            })),
    );
    row.into_any_element()
}

/// The filter and sort menu, and the way to mark everything read.
fn header_menu<T: CommentsHost>(
    state: &Rc<RefCell<CommentsPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut menu = menu_box();
    for filter in Filter::ALL {
        let selected = state.borrow().filter == filter;
        let shared = Rc::clone(state);
        let label = format!("comments-filter-{}", filter.id());
        menu = menu.child(
            div()
                .id(SharedString::from(label.clone()))
                .debug_selector(move || label.clone())
                .px(px(6.0))
                .py(px(5.0))
                .rounded_sm()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child(if selected {
                    format!("✓ {}", filter.label())
                } else {
                    filter.label().to_owned()
                })
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.filter = filter;
                        state.menu = None;
                    }
                    cx.notify();
                })),
        );
    }
    menu = menu.child(separator());
    for sort in Sort::ALL {
        let selected = state.borrow().sort == sort;
        let shared = Rc::clone(state);
        let label = format!("comments-sort-{}", sort.id());
        menu = menu.child(
            div()
                .id(SharedString::from(label.clone()))
                .debug_selector(move || label.clone())
                .px(px(6.0))
                .py(px(5.0))
                .rounded_sm()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child(if selected {
                    format!("✓ {}", sort.label())
                } else {
                    sort.label().to_owned()
                })
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.sort = sort;
                        state.menu = None;
                    }
                    cx.notify();
                })),
        );
    }
    menu = menu.child(separator());
    menu = menu.child(menu_item(
        "comments-mark-all-read",
        "Mark all as read",
        CommentsAction::MarkAllRead,
        cx,
    ));
    menu.into_any_element()
}

/// The search field.
///
/// The list narrows as the text is written, the way figcraft's search does,
/// so there is nothing to commit.
fn search_field<T: CommentsHost>(
    state: &Rc<RefCell<CommentsPanelState>>,
    focus: Option<&FocusHandle>,
    cx: &mut Context<T>,
) -> AnyElement {
    let query = state.borrow().query.clone();
    let empty = query.is_empty();
    let shown = if empty {
        SEARCH_COMMENTS.to_owned()
    } else {
        query.clone()
    };
    let shared = Rc::clone(state);
    let mut field = div()
        .id(SharedString::from("comments-search"))
        .debug_selector(|| "comments-search".to_owned())
        .flex()
        .items_center()
        .px(px(8.0))
        .py(px(7.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .text_color(if empty {
            rgb(theme::TEXT_MUTED)
        } else {
            rgb(theme::TEXT_SECONDARY)
        })
        .cursor_text()
        .child(shown);
    if let Some(focus) = focus {
        field = field.track_focus(focus);
        field = field.on_key_down(cx.listener(
            move |_host, event: &gpui::KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.as_str();
                let typed =
                    if event.keystroke.modifiers.control || event.keystroke.modifiers.platform {
                        None
                    } else {
                        // The character the key produced, when the platform says which
                        // one it was. A key that is a single character and carries none
                        // is that character: the named keys all have longer names.
                        event.keystroke.key_char.clone().or_else(|| {
                            if key.chars().count() == 1 {
                                Some(key.to_owned())
                            } else {
                                None
                            }
                        })
                    };
                let mut state = shared.borrow_mut();
                let handled = match key {
                    "backspace" => {
                        state.query.pop();
                        true
                    }
                    // Escape clears the search, and only while there is one to
                    // clear: an empty search is the shell's Escape to answer.
                    "escape" if !state.query.is_empty() => {
                        state.query.clear();
                        true
                    }
                    _ => match typed {
                        Some(typed) if typed.chars().count() == 1 => {
                            state.query.push_str(&typed);
                            true
                        }
                        _ => false,
                    },
                };
                drop(state);
                if handled {
                    cx.notify();
                    cx.stop_propagation();
                }
            },
        ));
    }
    field.into_any_element()
}

/// The notice that another filter is on, with the way back to the usual one.
fn filter_notice<T: CommentsHost>(
    state: &Rc<RefCell<CommentsPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let filter = state.borrow().filter;
    let shared = Rc::clone(state);
    div()
        .id(SharedString::from("comments-notice"))
        .debug_selector(|| "comments-notice".to_owned())
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(filter.label().to_owned())
        .child(
            div()
                .id(SharedString::from("comments-notice-open"))
                .debug_selector(|| "comments-notice-open".to_owned())
                .text_color(rgb(theme::ACCENT))
                .cursor_pointer()
                .child(SHOW_OPEN)
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.filter = Filter::Open;
                    }
                    cx.notify();
                })),
        )
        .into_any_element()
}

/// One thread: who wrote it, when, what it says, and where it is.
fn thread_row<T: CommentsHost>(
    model: &CommentsModel,
    thread: &CommentThread,
    state: &Rc<RefCell<CommentsPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let id = thread.comment.id;
    let open = state.borrow().menu == Some(CommentsMenu::Thread(id));
    let mut row = div()
        .id(SharedString::from(format!("comments-thread-{id}")))
        .debug_selector(move || format!("comments-thread-{id}"))
        .flex()
        .flex_col()
        .gap_1()
        .py(px(8.0))
        .border_b_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .cursor_pointer();
    // The line the author is on: the unread dot, the avatar, the name, the time.
    let mut head = div().flex().items_center().gap_2();
    if thread.unread {
        head = head.child(div().size(px(7.0)).rounded_full().bg(rgb(theme::ACCENT)));
    }
    head = head.child(avatar(thread.author()));
    head = head.child(
        div()
            .id(SharedString::from(format!("comments-thread-{id}-author")))
            .debug_selector(move || format!("comments-thread-{id}-author"))
            .text_xs()
            .font_weight(if thread.unread {
                gpui::FontWeight::MEDIUM
            } else {
                gpui::FontWeight::NORMAL
            })
            .text_color(rgb(theme::TEXT))
            .child(thread.author().to_owned()),
    );
    head = head.child(
        div()
            .ml_auto()
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .child(ago(thread.last_activity())),
    );
    row = row.child(head);
    // What the comment says.
    row = row.child(
        div()
            .id(SharedString::from(format!("comments-thread-{id}-text")))
            .debug_selector(move || format!("comments-thread-{id}-text"))
            .text_xs()
            .text_color(rgb(theme::TEXT))
            .line_clamp(3)
            .child(thread.text().to_owned()),
    );
    // The thread's frame and page, its replies, and its state.
    let mut meta = Vec::new();
    if let Some(name) = &thread.frame_name {
        meta.push(name.clone());
    }
    // The page is named only when the thread is on another one: the page the
    // editor is on is the one the user is already looking at.
    if model.page != Some(thread.page) {
        meta.push(thread.page_name.clone());
    }
    if thread.replies() > 0 {
        meta.push(plural(thread.replies(), "reply", "replies"));
    }
    if thread.resolved() {
        meta.push("Resolved".to_owned());
    }
    if !meta.is_empty() {
        row = row.child(
            div()
                .id(SharedString::from(format!("comments-thread-{id}-meta")))
                .debug_selector(move || format!("comments-thread-{id}-meta"))
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(meta.join(" · ")),
        );
    }
    // The actions on the thread.
    let shared = Rc::clone(state);
    row = row.child(
        div()
            .id(SharedString::from(format!("comments-thread-{id}-menu")))
            .debug_selector(move || format!("comments-thread-{id}-menu"))
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("⋯")
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                // The menu button is inside the row, and the row opens the
                // thread: the button keeps the click to itself.
                cx.stop_propagation();
                {
                    let mut state = shared.borrow_mut();
                    state.menu = if open {
                        None
                    } else {
                        Some(CommentsMenu::Thread(id))
                    };
                }
                cx.notify();
            })),
    );
    row = row.on_click(cx.listener(move |host, _event, _window, cx| {
        host.run_comments_action(CommentsAction::Open { id }, cx);
    }));
    row.into_any_element()
}

/// The actions on a thread.
fn thread_menu<T: CommentsHost>(
    model: &CommentsModel,
    thread: &CommentThread,
    _state: &Rc<RefCell<CommentsPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let id = thread.comment.id;
    let mut menu = menu_box();
    menu = menu.child(menu_item(
        &format!("comments-thread-{id}-resolve"),
        if thread.resolved() {
            "Mark as unresolved"
        } else {
            "Mark as resolved"
        },
        CommentsAction::Resolve {
            id,
            resolved: !thread.resolved(),
        },
        cx,
    ));
    menu = menu.child(menu_item(
        &format!("comments-thread-{id}-unread"),
        "Mark as unread",
        CommentsAction::MarkRead { id, read: false },
        cx,
    ));
    // Only the thread's author can delete it: the engine's own rule.
    if thread.is_mine(&model.user) {
        menu = menu.child(menu_item(
            &format!("comments-thread-{id}-delete"),
            "Delete thread",
            CommentsAction::Delete { id, reply: None },
            cx,
        ));
    }
    menu.into_any_element()
}

/// The empty state, word for word.
fn empty_state<T: CommentsHost>(model: &CommentsModel, _cx: &mut Context<T>) -> AnyElement {
    let (title, sub) = model.empty_state();
    let mut column = div().flex().flex_col().gap_1().py(px(16.0));
    column = column.child(
        div()
            .id(SharedString::from("comments-empty"))
            .debug_selector(|| "comments-empty".to_owned())
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgb(theme::TEXT_SECONDARY))
            .child(title.to_owned()),
    );
    column = column.child(
        div()
            .id(SharedString::from("comments-empty-sub"))
            .debug_selector(|| "comments-empty-sub".to_owned())
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .child(sub.to_owned()),
    );
    column.into_any_element()
}

/// The selected layers' own comment: every property, with its own reading.
fn selection_block<T: CommentsHost>(model: &CommentsModel, _cx: &mut Context<T>) -> AnyElement {
    let mut column = div().flex().flex_col().gap_1();
    for property in CommentProperty::ALL {
        let reading = model.reading(property);
        let value = match &reading {
            CommentState::Set(value) => property.display(value),
            CommentState::Unset => NOT_SET.to_owned(),
            CommentState::Mixed => MIXED.to_owned(),
        };
        let label = format!("comments-property-{}", property.slug());
        let value_label = format!("{label}-value");
        column = column.child(
            div()
                .id(SharedString::from(label.clone()))
                .debug_selector(move || label.clone())
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .text_color(rgb(theme::TEXT_SECONDARY))
                        .child(property.label().to_owned()),
                )
                .child(
                    div()
                        .id(SharedString::from(value_label.clone()))
                        .debug_selector(move || value_label.clone())
                        .text_color(rgb(theme::TEXT))
                        .child(value),
                ),
        );
    }
    column.into_any_element()
}

/// An author's avatar: their initials in a circle.
fn avatar(author: &str) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .size(px(24.0))
        .rounded_full()
        .bg(rgb(theme::SURFACE_RAISED))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(figcraft_engine::live::initials(author))
}

/// The box every menu is drawn in.
fn menu_box() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p(px(6.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER))
}

/// A line between the parts of a menu.
fn separator() -> Div {
    div().h(px(1.0)).bg(rgb(theme::BORDER))
}

/// One choice in a menu.
fn menu_item<T: CommentsHost>(
    selector: &str,
    label: &str,
    action: CommentsAction,
    cx: &mut Context<T>,
) -> AnyElement {
    div()
        .id(SharedString::from(selector.to_owned()))
        .debug_selector(move || selector.to_owned())
        .px(px(6.0))
        .py(px(5.0))
        .rounded_sm()
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer()
        .child(label.to_owned())
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_comments_action(action.clone(), cx);
        }))
        .into_any_element()
}

/// A caption: the dim text that names a group.
fn note(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

// ═══════════════════════════════════════════════════════════════════════════
// The times
// ═══════════════════════════════════════════════════════════════════════════

/// How long ago a thread was last active, as figcraft words it.
///
/// "now", "5m", "3h", "2d", "3w". Empty for a time the engine does not have
/// or one that has not happened yet.
pub fn ago(created: u64) -> String {
    ago_at(created, comment::now_secs())
}

/// [`ago`], against a given "now".
///
/// The "now" is a parameter so the wording can be pinned by a test: a clock
/// that moves makes a test that says "5m" a test that says "5m" only
/// sometimes.
pub fn ago_at(created: u64, now: u64) -> String {
    if created == 0 || now < created {
        return String::new();
    }
    let ago = now - created;
    match ago {
        0..60 => "now".to_owned(),
        60..3600 => format!("{}m", ago / 60),
        3600..86_400 => format!("{}h", ago / 3600),
        86_400..604_800 => format!("{}d", ago / 86_400),
        _ => format!("{}w", ago / 604_800),
    }
}

/// How long ago a thread was last active, in words.
///
/// "Just now", "5 minutes ago", "Yesterday", "3 weeks ago"…
pub fn ago_long(created: u64) -> String {
    ago_long_at(created, comment::now_secs())
}

/// [`ago_long`], against a given "now".
pub fn ago_long_at(created: u64, now: u64) -> String {
    if created == 0 || now < created {
        return String::new();
    }
    let ago = now - created;
    let count = |divisor: u64| usize::try_from(ago / divisor).unwrap_or(usize::MAX);
    match ago {
        0..60 => "Just now".to_owned(),
        60..3600 => plural(count(60), "minute ago", "minutes ago"),
        3600..86_400 => plural(count(3600), "hour ago", "hours ago"),
        86_400..172_800 => "Yesterday".to_owned(),
        172_800..604_800 => plural(count(86_400), "day ago", "days ago"),
        604_800..2_592_000 => plural(count(604_800), "week ago", "weeks ago"),
        2_592_000..31_536_000 => plural(count(2_592_000), "month ago", "months ago"),
        _ => plural(count(31_536_000), "year ago", "years ago"),
    }
}

/// A count and its noun, in figcraft's own plural rule.
fn plural(count: usize, one: &str, other: &str) -> String {
    if count == 1 {
        format!("{count} {one}")
    } else {
        format!("{count} {other}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use figcraft_doc::{Message, NodeKind};
    use figcraft_engine::cmd::comment::Query;
    use gpui::{point, size, Entity, Render, VisualTestContext};

    // ── The engine's own model ───────────────────────────────────────────────

    /// A document with one page and one rectangle.
    fn document_with() -> (Document, NodeId, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let node = doc
            .create(page, NodeKind::Rectangle, "Card", 0.0, 0.0, 100.0, 100.0)
            .expect("a rectangle");
        (doc, page, node)
    }

    /// A comment, in the engine's own model.
    fn comment(id: u64, page: NodeId, author: &str, text: &str, created: u64) -> Comment {
        Comment {
            id,
            page,
            x: 10.0,
            y: 20.0,
            anchor: None,
            first: Message::new(author, text, created),
            resolved: false,
            replies: Vec::new(),
            read_by: None,
        }
    }

    // ── The model ────────────────────────────────────────────────────────────

    #[test]
    fn the_model_reads_the_engines_comments_for_a_node() {
        let (mut doc, page, node) = document_with();
        let mut thread = comment(7, page, "Ada", "Tighten this", 100);
        thread.anchor = Some(node);
        doc.comments.push(thread);

        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");

        assert_eq!(model.layers.len(), 1);
        let layer = &model.layers[0];
        assert_eq!(layer.node, node);
        assert_eq!(layer.name, "Card");
        let thread = layer.comment.as_ref().expect("the layer's comment");
        assert_eq!(thread.comment.id, 7, "the engine's own thread");
        assert_eq!(thread.author(), "Ada");
        assert_eq!(thread.text(), "Tighten this");
        assert_eq!(thread.page, page);
    }

    #[test]
    fn a_layer_with_no_comment_reads_not_set() {
        let (doc, page, node) = document_with();
        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");

        let reading = model.reading(CommentProperty::Author);
        assert_eq!(reading, CommentState::Unset);
        assert_eq!(reading.word(), NOT_SET);
        assert!(reading.is_unset());
    }

    #[test]
    fn two_layers_whose_comments_differ_read_mixed() {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let first = doc
            .create(page, NodeKind::Rectangle, "One", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let second = doc
            .create(page, NodeKind::Rectangle, "Two", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let mut one = comment(1, page, "Ada", "First", 100);
        one.anchor = Some(first);
        let mut two = comment(2, page, "Sam", "Second", 200);
        two.anchor = Some(second);
        doc.comments.push(one);
        doc.comments.push(two);

        let model = CommentsModel::read(&doc, &[first, second], Some(page), "Ada");

        assert_eq!(
            model.reading(CommentProperty::Author),
            CommentState::Mixed,
            "two layers with two different authors disagree"
        );
        assert_eq!(model.reading(CommentProperty::Author).word(), MIXED);
        // The rest agree, so they are a value and not a disagreement.
        assert_eq!(
            model.reading(CommentProperty::Replies),
            CommentState::Set(CommentValue::Count(0))
        );
    }

    #[test]
    fn a_layer_with_a_comment_and_a_layer_without_read_mixed() {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let first = doc
            .create(page, NodeKind::Rectangle, "One", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let second = doc
            .create(page, NodeKind::Rectangle, "Two", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let mut one = comment(1, page, "Ada", "First", 100);
        one.anchor = Some(first);
        doc.comments.push(one);

        let model = CommentsModel::read(&doc, &[first, second], Some(page), "Ada");

        assert_eq!(
            model.reading(CommentProperty::Author),
            CommentState::Mixed,
            "one layer has a comment and the other has none"
        );
    }

    #[test]
    fn the_list_is_the_engines_own_list() {
        let (mut doc, page, node) = document_with();
        let mut first = comment(1, page, "Ada", "Tighten this", 100);
        first.anchor = Some(node);
        let mut second = comment(2, page, "Sam", "Looks good", 200);
        second.resolved = true;
        doc.comments.push(first);
        doc.comments.push(second);

        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");
        for (filter, sort, query) in [
            (Filter::All, Sort::Date, ""),
            (Filter::Open, Sort::Date, ""),
            (Filter::Resolved, Sort::Page, "good"),
            (Filter::Mine, Sort::Date, "tighten"),
            (Filter::Unread, Sort::Page, ""),
        ] {
            let mine: Vec<u64> = model
                .visible(filter, sort, query)
                .iter()
                .map(|thread| thread.comment.id)
                .collect();
            let theirs: Vec<u64> = comment::query(
                &doc,
                &Query {
                    page: None,
                    frame: None,
                    filter,
                    sort,
                    text: query,
                },
                "Ada",
            )
            .iter()
            .map(|thread| thread.id)
            .collect();
            assert_eq!(
                mine, theirs,
                "the panel's list is the engine's list: filter {filter:?}, sort {sort:?}, query {query:?}"
            );
        }
    }

    #[test]
    fn the_empty_state_says_figcraft_s_own_words() {
        let (mut doc, page, node) = document_with();

        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");
        // The words are figcraft's, written out: a test that compared the
        // words with the constants would agree with any spelling.
        assert_eq!(
            model.empty_state(),
            (
                "No comments yet",
                "Click anywhere on the canvas to leave one."
            )
        );

        doc.comments.push(comment(1, page, "Ada", "Hello", 100));
        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");
        assert_eq!(
            model.empty_state(),
            ("No matching comments", "Try another search or filter."),
            "a document that has comments says the search found nothing"
        );
    }

    // ── The commands ─────────────────────────────────────────────────────────

    #[test]
    fn a_change_carries_the_engines_own_command_id_and_params() {
        let (id, params) = CommentsAction::Open { id: 7 }.command();
        assert_eq!(id, "comment.open");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["view"], json!("center"));

        let (id, params) = CommentsAction::Resolve {
            id: 7,
            resolved: true,
        }
        .command();
        assert_eq!(id, "comment.resolve");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["resolved"], json!(true));

        let (id, params) = CommentsAction::Reply {
            id: 7,
            text: "Done".to_owned(),
        }
        .command();
        assert_eq!(id, "comment.reply");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["text"], json!("Done"));

        let (id, params) = CommentsAction::Link { id: 7 }.command();
        assert_eq!(id, "comment.link");
        assert_eq!(params["id"], json!(7));
    }

    #[test]
    fn a_message_is_named_the_way_the_engine_names_it() {
        // The engine's own spelling: the thread's first message is the one
        // with no `reply`, and a reply is named by its index.
        let (id, params) = CommentsAction::Edit {
            id: 7,
            text: "Tighter".to_owned(),
            reply: None,
        }
        .command();
        assert_eq!(id, "comment.edit");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["text"], json!("Tighter"));
        assert!(
            params.get("reply").is_none(),
            "the first message is named by leaving the index out: {params}"
        );

        let (id, params) = CommentsAction::Edit {
            id: 7,
            text: "Tighter".to_owned(),
            reply: Some(1),
        }
        .command();
        assert_eq!(id, "comment.edit");
        assert_eq!(params["reply"], json!(1));

        let (id, params) = CommentsAction::React {
            id: 7,
            emoji: "👍".to_owned(),
            reply: None,
        }
        .command();
        assert_eq!(id, "comment.react");
        assert_eq!(params["emoji"], json!("👍"));
        assert!(params.get("reply").is_none());

        let (id, params) = CommentsAction::Delete { id: 7, reply: None }.command();
        assert_eq!(id, "comment.delete");
        assert_eq!(params["id"], json!(7));
        assert!(
            params.get("reply").is_none(),
            "no index deletes the whole thread: {params}"
        );

        let (id, params) = CommentsAction::Delete {
            id: 7,
            reply: Some(2),
        }
        .command();
        assert_eq!(id, "comment.delete");
        assert_eq!(params["reply"], json!(2));
    }

    #[test]
    fn the_read_state_commands_are_the_engines_own() {
        let (id, params) = CommentsAction::MarkRead { id: 7, read: false }.command();
        assert_eq!(id, "comment.markRead");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["read"], json!(false));

        // Every thread: the engine's own "omit the id" spelling.
        let (id, params) = CommentsAction::MarkAllRead.command();
        assert_eq!(id, "comment.markRead");
        assert_eq!(params, json!({}));
    }

    #[test]
    fn the_engine_accepts_the_commands_the_panel_asks_for() {
        use figcraft_engine::Session;

        let mut session = Session::new();
        let added = session
            .execute(
                "comment.add",
                &json!({ "x": 10.0, "y": 20.0, "text": "Tighten this" }),
            )
            .expect("a comment");
        let id = added["id"].as_u64().expect("the comment's id");

        // Resolve it, the way the panel does.
        let (command, params) = CommentsAction::Resolve { id, resolved: true }.command();
        session
            .execute(&command, &params)
            .expect("the engine takes the panel's command");
        assert!(
            session
                .doc()
                .expect("the document")
                .comment(id)
                .expect("the thread")
                .resolved
        );

        // And a reply.
        let (command, params) = CommentsAction::Reply {
            id,
            text: "Done".to_owned(),
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the engine takes the panel's command");
        let thread = session
            .doc()
            .expect("the document")
            .comment(id)
            .expect("the thread");
        assert_eq!(thread.replies.len(), 1);
        assert_eq!(thread.replies[0].text, "Done");

        // And the read state.
        let (command, params) = CommentsAction::MarkRead { id, read: true }.command();
        session
            .execute(&command, &params)
            .expect("the engine takes the panel's command");
        assert!(!session
            .doc()
            .expect("the document")
            .comment(id)
            .expect("the thread")
            .is_unread("You"));
    }

    // ── The times ────────────────────────────────────────────────────────────
    #[test]
    fn the_ages_are_figcraft_s_own_words() {
        let now = 1_000_000_000;
        assert_eq!(ago_at(now, now), "now");
        assert_eq!(ago_at(now - 30, now), "now");
        assert_eq!(ago_at(now - 60, now), "1m");
        assert_eq!(ago_at(now - 3600, now), "1h");
        assert_eq!(ago_at(now - 86_400, now), "1d");
        assert_eq!(ago_at(now - 604_800, now), "1w");
        assert_eq!(ago_at(0, now), "", "a thread with no time says nothing");
        assert_eq!(
            ago_at(now + 10, now),
            "",
            "a time that has not happened says nothing"
        );

        assert_eq!(ago_long_at(now, now), "Just now");
        assert_eq!(ago_long_at(now - 60, now), "1 minute ago");
        assert_eq!(ago_long_at(now - 120, now), "2 minutes ago");
        assert_eq!(ago_long_at(now - 3600, now), "1 hour ago");
        assert_eq!(ago_long_at(now - 86_400, now), "Yesterday");
        assert_eq!(ago_long_at(now - 172_800, now), "2 days ago");
        assert_eq!(ago_long_at(now - 2_592_000, now), "1 month ago");
        assert_eq!(ago_long_at(now - 31_536_000, now), "1 year ago");
    }

    #[test]
    fn the_model_reads_the_engine_the_way_the_inspector_reads_it() {
        use figcraft_engine::Session;

        // The Inspector's own read path, on a real session: the document, the
        // selection, the page, and the name comments are signed with.
        let mut session = Session::new();
        session
            .execute(
                "comment.add",
                &json!({ "x": 10.0, "y": 20.0, "text": "Tighten this" }),
            )
            .expect("a comment");
        let model = CommentsModel::read(
            session.doc().expect("the document"),
            &session.selection(),
            session.page().ok(),
            &comment::user_name(&session),
        );
        assert_eq!(model.threads.len(), 1, "the engine's own thread");
        assert_eq!(model.user, "You", "the name the engine signs with");
    }

    // ── The panel, in a real window ──────────────────────────────────────────────

    /// A surface for the section to live on: it draws it, and remembers what it
    /// was asked to do.
    struct Host {
        model: CommentsModel,
        state: Rc<RefCell<CommentsPanelState>>,
        asked: Rc<RefCell<Vec<CommentsAction>>>,
    }

    impl Host {
        fn new(model: CommentsModel) -> Self {
            Self {
                model,
                state: Rc::new(RefCell::new(CommentsPanelState::new())),
                asked: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl CommentsHost for Host {
        fn run_comments_action(&mut self, action: CommentsAction, cx: &mut Context<Self>) {
            self.asked.borrow_mut().push(action);
            cx.notify();
        }
    }

    impl Render for Host {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            cx: &mut Context<Self>,
        ) -> impl IntoElement {
            let model = self.model.clone();
            let state = Rc::clone(&self.state);
            comments_section(&model, &state, cx)
        }
    }

    /// Open a window with the section in it, over the given model.
    fn open_section(
        cx: &mut gpui::TestAppContext,
        model: CommentsModel,
    ) -> (Entity<Host>, &mut VisualTestContext) {
        cx.add_window_view(|_, _| Host::new(model))
    }

    /// Draw the section in the window and settle.
    fn draw(root: &Entity<Host>, window: &mut VisualTestContext) {
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(320.0), px(900.0)),
            |_, _| root.clone().into_any_element(),
        );
        window.run_until_parked();
    }

    /// The bounds of an element the section draws.
    fn bounds(
        window: &mut VisualTestContext,
        selector: &'static str,
    ) -> gpui::Bounds<gpui::Pixels> {
        window
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("the section draws {selector}"))
    }

    /// Type text into whatever has the focus.
    fn type_text(window: &mut VisualTestContext, text: &str) {
        for character in text.chars() {
            let keystroke = gpui::Keystroke::parse(&character.to_string()).expect("a keystroke");
            window.update(|window, cx| window.dispatch_keystroke(keystroke, cx));
        }
        window.run_until_parked();
    }

    /// A document with a comment on a node, as the panel reads it.
    fn document_with_a_comment() -> (Document, NodeId, NodeId) {
        let (mut doc, page, node) = document_with();
        let mut thread = comment(7, page, "Ada", "Tighten this", 100);
        thread.anchor = Some(node);
        doc.comments.push(thread);
        (doc, page, node)
    }

    #[gpui::test]
    fn the_section_draws_the_document_s_comments(cx: &mut gpui::TestAppContext) {
        // The render test: the section, on its own, in a real window, showing the
        // engine's own thread.
        let (doc, page, node) = document_with_a_comment();
        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        // The title is on screen, with a real size.
        let title = bounds(window, "comments-title");
        assert!(
            title.size.height > px(0.0) && title.size.width > px(0.0),
            "the title is on screen: {title:?}"
        );
        // The thread is on screen, with the author and what it says.
        let thread = bounds(window, "comments-thread-7");
        assert!(
            thread.size.height > px(0.0),
            "the thread is on screen: {thread:?}"
        );
        let author = bounds(window, "comments-thread-7-author");
        assert!(
            author.size.height > px(0.0),
            "the thread says who wrote it: {author:?}"
        );
        let text = bounds(window, "comments-thread-7-text");
        assert!(
            text.size.height > px(0.0),
            "the thread says what it says: {text:?}"
        );
        // And the search field is there to narrow the list.
        let search = bounds(window, "comments-search");
        assert!(
            search.size.height > px(0.0),
            "the search field is on screen: {search:?}"
        );
    }

    #[gpui::test]
    fn the_section_shows_the_empty_state_when_there_are_no_comments(cx: &mut gpui::TestAppContext) {
        // figcraft's empty state, word for word.
        let (doc, page, node) = document_with();
        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let empty = bounds(window, "comments-empty");
        assert!(
            empty.size.height > px(0.0),
            "the empty state is on screen: {empty:?}"
        );
        let sub = bounds(window, "comments-empty-sub");
        assert!(sub.size.height > px(0.0), "and it says what to do: {sub:?}");
        assert!(
            window.debug_bounds("comments-thread-7").is_none(),
            "and there is no thread to show"
        );
    }

    #[gpui::test]
    fn the_section_shows_the_node_s_own_comment(cx: &mut gpui::TestAppContext) {
        // The node's own half: the properties of the layer's comment, each with
        // its own reading.
        let (doc, page, node) = document_with_a_comment();
        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let author = bounds(window, "comments-property-author");
        assert!(
            author.size.height > px(0.0),
            "the layer's comment is on screen: {author:?}"
        );
        let value = bounds(window, "comments-property-author-value");
        assert!(
            value.size.height > px(0.0),
            "and it says who wrote it: {value:?}"
        );
        // A layer with no comment says so.
        let (doc, page, node) = document_with();
        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");
        let (root, window) = open_section(cx, model);
        draw(&root, window);
        assert!(
            window
                .debug_bounds("comments-property-author-value")
                .is_some(),
            "a layer with no comment still has a row"
        );
    }

    #[gpui::test]
    fn clicking_a_thread_asks_the_engine_to_open_it(cx: &mut gpui::TestAppContext) {
        let (doc, page, node) = document_with_a_comment();
        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let thread = bounds(window, "comments-thread-7");
        window.simulate_click(thread.center(), gpui::Modifiers::none());
        window.run_until_parked();

        let asked = root.read_with(cx, |host, _| host.asked.borrow().clone());
        assert_eq!(asked.len(), 1, "the section asked for one change");
        assert_eq!(asked[0], CommentsAction::Open { id: 7 });
        // And the command it would run is the engine's own.
        let (command, params) = asked[0].command();
        assert_eq!(command, "comment.open");
        assert_eq!(params["id"], json!(7));
    }

    #[gpui::test]
    fn the_search_narrows_the_list(cx: &mut gpui::TestAppContext) {
        let (mut doc, page, node) = document_with();
        doc.comments
            .push(comment(1, page, "Ada", "Tighten this", 100));
        doc.comments
            .push(comment(2, page, "Sam", "Looks good", 200));
        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        assert!(
            window.debug_bounds("comments-thread-1").is_some(),
            "both threads are on screen"
        );
        assert!(window.debug_bounds("comments-thread-2").is_some());

        // Type into the search field, the way a user does.
        let search = bounds(window, "comments-search");
        window.simulate_click(search.center(), gpui::Modifiers::none());
        window.run_until_parked();
        type_text(window, "tighten");
        draw(&root, window);

        assert!(
            window.debug_bounds("comments-thread-1").is_some(),
            "the thread that matches is still on screen"
        );
        assert!(
            window.debug_bounds("comments-thread-2").is_none(),
            "and the one that does not is gone"
        );
    }

    #[gpui::test]
    fn the_filter_menu_offers_the_engines_own_filters(cx: &mut gpui::TestAppContext) {
        let (doc, page, node) = document_with_a_comment();
        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        // The menu is closed to begin with.
        assert!(window.debug_bounds("comments-filter-open").is_none());

        let menu = bounds(window, "comments-menu");
        window.simulate_click(menu.center(), gpui::Modifiers::none());
        window.run_until_parked();

        for filter in Filter::ALL {
            let selector: &'static str =
                Box::leak(format!("comments-filter-{}", filter.id()).into_boxed_str());
            assert!(
                window.debug_bounds(selector).is_some(),
                "the menu offers {filter:?}"
            );
        }
        for sort in Sort::ALL {
            let selector: &'static str =
                Box::leak(format!("comments-sort-{}", sort.id()).into_boxed_str());
            assert!(
                window.debug_bounds(selector).is_some(),
                "the menu offers {sort:?}"
            );
        }
        assert!(
            window.debug_bounds("comments-mark-all-read").is_some(),
            "and the way to mark everything read"
        );
    }
}
