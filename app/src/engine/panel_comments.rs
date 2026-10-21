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

use std::cmp::Reverse;

use figcraft_doc::{Comment, Document, NodeId};
use figcraft_engine::cmd::comment::{self, Filter, Sort};

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
        60..3600 => format!("{} minutes ago", count(60)),
        3600..86_400 => format!("{} hours ago", count(3600)),
        86_400..172_800 => "Yesterday".to_owned(),
        172_800..604_800 => format!("{} days ago", count(86_400)),
        604_800..2_592_000 => format!("{} weeks ago", count(604_800)),
        2_592_000..31_536_000 => format!("{} months ago", count(2_592_000)),
        _ => format!("{} years ago", count(31_536_000)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use figcraft_doc::{Message, NodeKind};
    use figcraft_engine::cmd::comment::Query;

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
        assert_eq!(model.empty_state(), (NO_COMMENTS_YET, CLICK_TO_LEAVE_ONE));

        doc.comments.push(comment(1, page, "Ada", "Hello", 100));
        let model = CommentsModel::read(&doc, &[node], Some(page), "Ada");
        assert_eq!(
            model.empty_state(),
            (NO_MATCHING_COMMENTS, TRY_ANOTHER_SEARCH),
            "a document that has comments says the search found nothing"
        );
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
        assert_eq!(ago_long_at(now - 60, now), "1 minutes ago");
        assert_eq!(ago_long_at(now - 3600, now), "1 hours ago");
        assert_eq!(ago_long_at(now - 86_400, now), "Yesterday");
        assert_eq!(ago_long_at(now - 172_800, now), "2 days ago");
        assert_eq!(ago_long_at(now - 2_592_000, now), "1 months ago");
        assert_eq!(ago_long_at(now - 31_536_000, now), "1 years ago");
    }
}
