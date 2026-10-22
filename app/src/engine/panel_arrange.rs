//! The Arrange surface: alignment, order, distribution and boolean operations.
//!
//! Ported from figcraft's `ui-egui/src/right.rs` — the *Alignment* row of the
//! Position section, the *Distribute and tidy* menu beside it and the
//! *Boolean groups* control in the layer header — and from the four order
//! commands in `ui-egui/src/menus.rs`.
//!
//! # What figcraft does, row by row
//!
//! | figcraft | here |
//! |---|---|
//! | the *Alignment* label and the six align buttons, in two strips of three | [`alignment_row`] |
//! | `align.*`, enabled only when `cmd::arrange::can_align` says so | the model's `align` availability, with figcraft's own reason |
//! | the *Distribute and tidy* button and its three entries | [`distribute_row`] |
//! | the *Boolean groups* button, whose icon shows the operation in use | [`boolean_row`] |
//! | *Bring to front*, *Bring forward*, *Send backward*, *Send to back* | [`order_row`] |
//!
//! # The engine does the work
//!
//! Every control is an engine command with the engine's own parameters:
//! `align.left`, `align.hcenter`, `align.right`, `align.top`, `align.vcenter`,
//! `align.bottom`, `arrange.front`, `arrange.forward`, `arrange.backward`,
//! `arrange.back`, `distribute.horizontal`, `distribute.vertical`,
//! `arrange.tidy`, `bool.union`, `bool.subtract`, `bool.intersect`,
//! `bool.exclude` and `bool.flatten`. Nothing here changes the document
//! directly: a change made in this panel and a change made from the CLI are
//! the same change, and they land in the same history.
//!
//! # What the source can carry, and what it cannot
//!
//! Spool is source-backed, so a control that changes something the save path
//! cannot write back is a control that lies. The save path
//! (`app/src/engine/source_edits.rs`) compares a layer's styles, its text and
//! its geometry, and writes the hierarchy into `lamine.yaml`:
//!
//! - **Alignment, distribution and tidying** move a layer. That is geometry,
//!   and the geometry comparison writes it back as a placement.
//! - **The order commands** change a layer's place among its siblings. The
//!   paint order is `lamine.yaml`'s ordered `children`, which the reader
//!   compiles, so the change survives a reopen.
//! - **A boolean operation** makes a new kind of layer — a boolean group —
//!   and `kind_for` in the save path refuses it, so the group and its
//!   operands are reported as unsupported rather than written. The operation
//!   therefore stays in the editor's document, and this panel says so
//!   instead of implying the file already holds it.
//!
//! # The three states
//!
//! The one value this surface shows is the boolean operation in use, and it
//! has three states like every other: the operation every selected layer
//! agrees on, *Mixed* when they disagree, and *Not set* when the selection is
//! not a boolean at all. The fold is per property: a mixed operation says so
//! under the operation, and the alignment and the order are still what they
//! were.
//!
//! # A control that is off says why
//!
//! Every control here can be off, and every one of them says so, in words,
//! where it is: the alignment that a lone layer cannot use, the distribute
//! entry that needs a third layer, the boolean button that needs two shapes.
//! The reasons are the engine's own rules, read the way the engine reads
//! them — `cmd::arrange::can_align` for the alignment, `booleans::apply`'s
//! rule for the combination, and the layer counts the engine's commands
//! themselves apply.
//!
//! # Not ported
//!
//! - **The hover tips.** Spool has no tooltip widget, so the buttons carry
//!   their names in their debug selectors and in the short labels beside the
//!   icons.
//! - **The keyboard shortcuts.** The engine's keymap already binds
//!   `align.left` and the rest; this panel does not rebind them.

use std::cell::RefCell;
use std::rc::Rc;

use figcraft_doc::{BoolOp, Document, NodeId};
use figcraft_engine::{booleans, cmd::arrange as engine_arrange, cmd::edit as engine_edit};
use gpui::{
    div, px, rgb, svg, AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, Stateful, StatefulInteractiveElement, Styled, Svg,
};
use serde_json::{json, Value};

use crate::inspector::Availability;
use crate::theme;

// ═══════════════════════════════════════════════════════════════════════════
// Words
// ═══════════════════════════════════════════════════════════════════════════

/// The word for a property the selected layers disagree about.
pub const MIXED: &str = "Mixed";

/// The word for a property nobody has set.
pub const NOT_SET: &str = "Not set";

/// The panel's empty state for a selection of nothing.
const NO_SELECTION: &str = "No selection";

/// What the panel says when there is nothing selected.
const NO_SELECTION_NOTE: &str = "Select an object on the canvas or in Layers.";

/// Why the alignment buttons cannot be used. figcraft's own words.
const ALIGN_REASON: &str =
    "A layer on its own aligns to its frame: select more layers, or one in a frame";

/// Why the distribution cannot space the selection out.
const DISTRIBUTE_REASON: &str = "Select three or more layers to space them out";

/// Why the selection cannot be tidied.
const TIDY_REASON: &str = "Select two or more layers to tidy them";

/// Why the order buttons cannot be used.
const ORDER_REASON: &str = "Select a layer to change its order";

/// Why one layer cannot be combined with anything.
const COMBINE_REASON: &str = "Select two or more shapes to combine";

/// Why nothing can be combined.
const COMBINE_ONE_REASON: &str = "Select a shape to combine";

/// Why a section cannot be combined with anything.
const COMBINE_SECTION_REASON: &str = "A section cannot go in a boolean group";

/// What the save path does with a boolean operation today.
const BOOLEAN_SAVE_NOTE: &str =
    "A boolean operation is not written back to the source yet, so a change made here stays in \
     the editor's document.";

// ═══════════════════════════════════════════════════════════════════════════
// The model
// ═══════════════════════════════════════════════════════════════════════════

/// The boolean operation the selection uses, folded per property.
///
/// Three states, like every other value in the Inspector: the operation every
/// selected layer agrees on, the absence of agreement, and the absence of a
/// boolean at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BooleanValue {
    /// Every selected layer combines the same way.
    Set(BoolOp),
    /// The selection is not a boolean at all.
    Unset,
    /// The selected layers combine differently.
    Mixed,
}

impl BooleanValue {
    /// The word the panel shows for a state that is not a value.
    pub fn word(self) -> &'static str {
        match self {
            Self::Set(_) => "",
            Self::Unset => NOT_SET,
            Self::Mixed => MIXED,
        }
    }
}

/// What the Arrange surface shows about the selection.
///
/// A *model*, not a widget: it reads the engine's document and says what the
/// panel would draw, so the reading can be tested without a window and the
/// panel can be drawn without reading.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrangeModel {
    /// The layers the model was read for.
    pub ids: Vec<NodeId>,
    /// Whether the alignment buttons can be used.
    pub align: Availability,
    /// Whether the distribute button can open its menu.
    pub distribute: Availability,
    /// Whether the selection can be spaced out horizontally.
    pub distribute_horizontal: Availability,
    /// Whether the selection can be spaced out vertically.
    pub distribute_vertical: Availability,
    /// Whether the selection can be tidied.
    pub tidy: Availability,
    /// Whether the order buttons can be used.
    pub order: Availability,
    /// Whether the selection can be combined.
    pub boolean: Availability,
    /// The boolean operation in use.
    pub boolean_value: BooleanValue,
}

impl Default for ArrangeModel {
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            align: Availability::Disabled(ALIGN_REASON),
            distribute: Availability::Disabled(TIDY_REASON),
            distribute_horizontal: Availability::Disabled(DISTRIBUTE_REASON),
            distribute_vertical: Availability::Disabled(DISTRIBUTE_REASON),
            tidy: Availability::Disabled(TIDY_REASON),
            order: Availability::Disabled(ORDER_REASON),
            boolean: Availability::Disabled(COMBINE_ONE_REASON),
            boolean_value: BooleanValue::Unset,
        }
    }
}

/// The Arrange surface's reading of the selection.
///
/// The engine's own rules, not a second set of them: whether the selection
/// can be aligned is `cmd::arrange::can_align`, and whether it can be
/// combined is the engine's own boolean rule — a boolean group changes its
/// own operation, layers inside one change theirs, and anything else is
/// wrapped in a new group, which takes two shapes to be a boolean at all.
pub fn selected_arrange(doc: &Document, selection: &[NodeId]) -> ArrangeModel {
    let ids: Vec<NodeId> = selection.to_vec();
    let count = ids.len();
    let align = if engine_arrange::can_align(doc, &ids) {
        Availability::Supported
    } else {
        Availability::Disabled(ALIGN_REASON)
    };
    // The engine spaces three layers and tidies two, so the menu opens for
    // two and each of its entries says what it needs.
    let distribute = if count >= 2 {
        Availability::Supported
    } else {
        Availability::Disabled(TIDY_REASON)
    };
    let distribute_horizontal = if count >= 3 {
        Availability::Supported
    } else {
        Availability::Disabled(DISTRIBUTE_REASON)
    };
    let distribute_vertical = distribute_horizontal;
    let tidy = if count >= 2 {
        Availability::Supported
    } else {
        Availability::Disabled(TIDY_REASON)
    };
    let order = if count >= 1 {
        Availability::Supported
    } else {
        Availability::Disabled(ORDER_REASON)
    };
    let boolean = match combine_blocker(doc, &ids) {
        None => Availability::Supported,
        Some(reason) => Availability::Disabled(reason),
    };
    ArrangeModel {
        ids,
        align,
        distribute,
        distribute_horizontal,
        distribute_vertical,
        tidy,
        order,
        boolean,
        boolean_value: boolean_value(doc, selection),
    }
}

/// Why the selection cannot be combined into a boolean, if it cannot.
///
/// The engine's rule, read the same way `booleans::apply` reads it: one
/// boolean group changes its own operation, layers inside a boolean group
/// change theirs, and anything else is wrapped in a new group — which takes
/// at least two shapes to be a boolean at all, and refuses a section, because
/// a section cannot go in a group or a frame.
fn combine_blocker(doc: &Document, ids: &[NodeId]) -> Option<&'static str> {
    if ids.is_empty() {
        return Some(COMBINE_ONE_REASON);
    }
    if let [one] = ids {
        if booleans::is_group(doc, *one) {
            return None;
        }
    }
    if ids.iter().all(|id| booleans::group_of(doc, *id).is_some()) {
        return None;
    }
    if engine_edit::roots(doc, ids)
        .iter()
        .any(|id| is_section(doc, *id))
    {
        return Some(COMBINE_SECTION_REASON);
    }
    if ids.len() < 2 {
        return Some(COMBINE_REASON);
    }
    None
}

/// Whether a layer is a section.
fn is_section(doc: &Document, id: NodeId) -> bool {
    doc.node(id)
        .is_some_and(|node| matches!(node.kind, figcraft_doc::NodeKind::Section { .. }))
}

/// The boolean operation the selection uses.
///
/// The engine's own answer (`booleans::shown_op`), and the one thing it does
/// not say is *why* there is no answer: a selection of ordinary layers has no
/// operation at all, while the operands of a boolean group that disagree have
/// two.
fn boolean_value(doc: &Document, ids: &[NodeId]) -> BooleanValue {
    if let Some(op) = booleans::shown_op(doc, ids) {
        return BooleanValue::Set(op);
    }
    let about_booleans = !ids.is_empty()
        && (ids.iter().any(|id| booleans::is_group(doc, *id))
            || ids.iter().all(|id| booleans::group_of(doc, *id).is_some()));
    if about_booleans {
        BooleanValue::Mixed
    } else {
        BooleanValue::Unset
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The commands
// ═══════════════════════════════════════════════════════════════════════════

/// The six ways a selection can be aligned.
///
/// The engine's own ids, in figcraft's order: the horizontal strip first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    /// The left edges line up.
    Left,
    /// The horizontal centres line up.
    HorizontalCenter,
    /// The right edges line up.
    Right,
    /// The top edges line up.
    Top,
    /// The vertical centres line up.
    VerticalCenter,
    /// The bottom edges line up.
    Bottom,
}

impl Align {
    /// Every alignment, in figcraft's order.
    pub const ALL: [Align; 6] = [
        Align::Left,
        Align::HorizontalCenter,
        Align::Right,
        Align::Top,
        Align::VerticalCenter,
        Align::Bottom,
    ];

    /// The engine's command id for this alignment.
    pub fn command(self) -> &'static str {
        match self {
            Self::Left => "align.left",
            Self::HorizontalCenter => "align.hcenter",
            Self::Right => "align.right",
            Self::Top => "align.top",
            Self::VerticalCenter => "align.vcenter",
            Self::Bottom => "align.bottom",
        }
    }

    /// The name figcraft gives the button.
    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Align left",
            Self::HorizontalCenter => "Align horizontal centers",
            Self::Right => "Align right",
            Self::Top => "Align top",
            Self::VerticalCenter => "Align vertical centers",
            Self::Bottom => "Align bottom",
        }
    }

    /// The short name the button carries beside its icon.
    ///
    /// Spool has no tooltip widget, so a button that is only an icon would
    /// have no name at all. figcraft's own names do not fit a third of a
    /// 264-point panel, so these are the shortest words that say which
    /// alignment they are; the full name is in the button's debug selector.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::HorizontalCenter => "Centre",
            Self::Right => "Right",
            Self::Top => "Top",
            Self::VerticalCenter => "Middle",
            Self::Bottom => "Bottom",
        }
    }

    /// The icon the button carries.
    pub fn icon(self) -> ArrangeIcon {
        match self {
            Self::Left => ArrangeIcon::AlignLeft,
            Self::HorizontalCenter => ArrangeIcon::AlignHorizontalCenter,
            Self::Right => ArrangeIcon::AlignRight,
            Self::Top => ArrangeIcon::AlignTop,
            Self::VerticalCenter => ArrangeIcon::AlignVerticalCenter,
            Self::Bottom => ArrangeIcon::AlignBottom,
        }
    }

    /// The name the button is known by in the panel.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::HorizontalCenter => "hcenter",
            Self::Right => "right",
            Self::Top => "top",
            Self::VerticalCenter => "vcenter",
            Self::Bottom => "bottom",
        }
    }
}

/// The four ways a selection's place among its siblings can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// In front of every sibling.
    Front,
    /// One step in front.
    Forward,
    /// One step behind.
    Backward,
    /// Behind every sibling.
    Back,
}

impl Order {
    /// Every order command, in figcraft's order.
    pub const ALL: [Order; 4] = [Order::Front, Order::Forward, Order::Backward, Order::Back];

    /// The engine's command id for this order.
    pub fn command(self) -> &'static str {
        match self {
            Self::Front => "arrange.front",
            Self::Forward => "arrange.forward",
            Self::Backward => "arrange.backward",
            Self::Back => "arrange.back",
        }
    }

    /// The name figcraft gives the command.
    pub fn label(self) -> &'static str {
        match self {
            Self::Front => "Bring to front",
            Self::Forward => "Bring forward",
            Self::Backward => "Send backward",
            Self::Back => "Send to back",
        }
    }
}

/// The three ways a selection can be spaced out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spacing {
    /// Even horizontal spacing.
    Horizontal,
    /// Even vertical spacing.
    Vertical,
    /// A tidy grid of rows and columns.
    Tidy,
}

impl Spacing {
    /// Every spacing command, in figcraft's order.
    pub const ALL: [Spacing; 3] = [Spacing::Horizontal, Spacing::Vertical, Spacing::Tidy];

    /// The engine's command id for this spacing.
    pub fn command(self) -> &'static str {
        match self {
            Self::Horizontal => "distribute.horizontal",
            Self::Vertical => "distribute.vertical",
            Self::Tidy => "arrange.tidy",
        }
    }

    /// The name figcraft gives the command.
    pub fn label(self) -> &'static str {
        match self {
            Self::Horizontal => "Distribute horizontal spacing",
            Self::Vertical => "Distribute vertical spacing",
            Self::Tidy => "Tidy up",
        }
    }
}

/// A change the Arrange surface can ask for.
///
/// One variant per thing the panel can change, so a change the engine has no
/// command for cannot be written down as though it had one.
#[derive(Clone, Debug, PartialEq)]
pub enum ArrangeAction {
    /// Align the selection.
    Align {
        /// The layers to align.
        ids: Vec<NodeId>,
        /// Which way.
        alignment: Align,
    },
    /// Change the selection's place among its siblings.
    Order {
        /// The layers to move.
        ids: Vec<NodeId>,
        /// Where to.
        order: Order,
    },
    /// Space the selection out.
    Spacing {
        /// The layers to space.
        ids: Vec<NodeId>,
        /// How.
        spacing: Spacing,
    },
    /// Combine the selection with a boolean operation.
    Boolean {
        /// The layers to combine.
        ids: Vec<NodeId>,
        /// Which operation.
        operation: BoolOp,
    },
    /// Flatten the selection into one vector.
    Flatten {
        /// The layers to flatten.
        ids: Vec<NodeId>,
    },
}

impl ArrangeAction {
    /// The engine's command id and parameters for this change.
    ///
    /// The engine's own names, so a change made from the panel is the same
    /// change the CLI, the keymap and an agent would make.
    pub fn command(&self) -> (String, Value) {
        match self {
            Self::Align { ids, alignment } => (alignment.command().to_owned(), ids_params(ids)),
            Self::Order { ids, order } => (order.command().to_owned(), ids_params(ids)),
            Self::Spacing { ids, spacing } => (spacing.command().to_owned(), ids_params(ids)),
            Self::Boolean { ids, operation } => {
                (boolean_command(*operation).to_owned(), ids_params(ids))
            }
            Self::Flatten { ids } => ("bool.flatten".to_owned(), ids_params(ids)),
        }
    }
}

/// The engine's command id for a boolean operation.
fn boolean_command(operation: BoolOp) -> &'static str {
    match operation {
        BoolOp::Union => "bool.union",
        BoolOp::Subtract => "bool.subtract",
        BoolOp::Intersect => "bool.intersect",
        BoolOp::Exclude => "bool.exclude",
    }
}

/// The parameters a command of this surface takes.
///
/// Every one of them takes the layers it acts on, and nothing else: the
/// engine's `ids` parameter is a list of node ids, and the engine's
/// `targets` reads it before it falls back to the session's selection.
fn ids_params(ids: &[NodeId]) -> Value {
    json!({ "ids": ids.iter().map(|id| id.0).collect::<Vec<_>>() })
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel's own state
// ═══════════════════════════════════════════════════════════════════════════

/// Which menu of the surface is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrangeMenu {
    /// The distribute and tidy menu.
    Distribute,
    /// The boolean operations menu.
    Boolean,
}

/// The panel's own state.
///
/// The state is a cell the surface that hosts the panel owns, so a menu stays
/// open across repaints while the panel itself stays a function.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArrangePanelState {
    /// Which menu is open, if any.
    pub open: Option<ArrangeMenu>,
}

impl ArrangePanelState {
    /// The state a section starts in.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the section is part-way through something.
    ///
    /// Read-only, and kept in step with [`Self::cancel`]: the editor's Escape
    /// ladder reads this to decide which rung is on top, and only then asks
    /// the section to act on it.
    pub fn is_busy(&self) -> bool {
        self.open.is_some()
    }

    /// Give up whatever the section was part-way through.
    ///
    /// Returns whether anything was given up, so the ladder knows the key was
    /// consumed.
    pub fn cancel(&mut self) -> bool {
        self.open.take().is_some()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// What a surface that hosts the Arrange panel must be able to do.
///
/// The panel draws and asks; the surface owns the engine and runs the changes.
/// The panel never touches the document itself, so the engine's command
/// surface stays the one operation boundary.
pub trait ArrangeHost: Sized + 'static {
    /// Run a change through the engine.
    fn run_arrange_action(&mut self, action: ArrangeAction, cx: &mut Context<Self>);
}

/// The Arrange section, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own state,
/// and the surface that runs the changes. It returns an element, so the
/// Inspector can put it in a section of its own and a test can put it in a
/// window on its own.
pub fn section<T: ArrangeHost>(
    model: &ArrangeModel,
    state: &Rc<RefCell<ArrangePanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_3().px(px(14.0)).py(px(12.0));

    // Nothing selected: there is nothing to align, order or combine, and
    // saying so is better than a wall of controls that cannot be used.
    if model.ids.is_empty() {
        return column
            .child(
                div()
                    .id(SharedString::from("arrange-empty"))
                    .debug_selector(|| "arrange-empty".to_owned())
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(theme::TEXT))
                    .child(NO_SELECTION),
            )
            .child(note("no-selection", NO_SELECTION_NOTE))
            .into_any_element();
    }

    column = column.child(alignment_row(model, cx));
    column = column.child(distribute_row(model, state, cx));
    column = column.child(order_row(model, cx));
    column = column.child(boolean_row(model, state, cx));
    column.into_any_element()
}

/// The alignment row: the six ways a selection can be aligned.
///
/// Two strips of three, in figcraft's order: the horizontal alignments first
/// and the vertical ones under them.
fn alignment_row<T: ArrangeHost>(model: &ArrangeModel, cx: &mut Context<T>) -> impl IntoElement {
    let mut column = div().flex().flex_col().gap_2();
    column = column.child(sublabel("Alignment"));
    for strip in Align::ALL.chunks(3) {
        let mut row = div().flex().gap_1();
        for alignment in strip {
            row = row.child(align_button(*alignment, model, cx));
        }
        column = column.child(row);
    }
    if let Some(reason) = model.align.reason() {
        column = column.child(note("align", reason));
    }
    column
}

/// One alignment button: the icon, the short name, and the command.
fn align_button<T: ArrangeHost>(
    alignment: Align,
    model: &ArrangeModel,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let selector = format!("arrange-align-{}", alignment.slug());
    let action = ArrangeAction::Align {
        ids: model.ids.clone(),
        alignment,
    };
    let mut button = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex_1()
        .flex()
        .items_center()
        .gap_1()
        .h(px(26.0))
        .px(px(6.0))
        .rounded_sm()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .child(icon(alignment.icon(), 14.0, theme::TEXT_SECONDARY))
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(alignment.short_label()),
        );
    if model.align.is_editable() {
        button = button
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .on_click(cx.listener(move |host, _event, _window, cx| {
                host.run_arrange_action(action.clone(), cx);
            }));
    } else {
        button = button.cursor_default().opacity(0.5);
    }
    button
}

/// The distribute row: the button that opens the spacing menu.
///
/// figcraft puts this button beside the alignment buttons and shows it only
/// when more than two layers are selected. Spool shows it whenever the
/// selection can be tidied and says what each of its three commands needs,
/// because a control that is hidden cannot say why it is missing.
fn distribute_row<T: ArrangeHost>(
    model: &ArrangeModel,
    state: &Rc<RefCell<ArrangePanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let mut column = div().flex().flex_col().gap_2();
    let selector = "arrange-distribute".to_owned();
    let open = state.borrow().open == Some(ArrangeMenu::Distribute);
    let mut button = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_2()
        .h(px(26.0))
        .px(px(8.0))
        .rounded_sm()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .child(icon(
            ArrangeIcon::DistributeHorizontal,
            14.0,
            theme::TEXT_SECONDARY,
        ))
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child("Distribute and tidy"),
        );
    if model.distribute.is_editable() {
        let shared = Rc::clone(state);
        button = button
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.open = if state.open == Some(ArrangeMenu::Distribute) {
                        None
                    } else {
                        Some(ArrangeMenu::Distribute)
                    };
                }
                cx.notify();
            }));
    } else {
        button = button.cursor_default().opacity(0.5);
    }
    column = column.child(button);
    if open {
        column = column.child(distribute_menu(model, cx));
    }
    if let Some(reason) = model.distribute.reason() {
        column = column.child(note("distribute", reason));
    }
    column
}

/// The spacing menu: the three commands that space a selection out.
fn distribute_menu<T: ArrangeHost>(model: &ArrangeModel, cx: &mut Context<T>) -> impl IntoElement {
    let mut menu = div()
        .flex()
        .flex_col()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::SURFACE))
        .px(px(6.0))
        .py(px(4.0));
    for spacing in Spacing::ALL {
        let availability = match spacing {
            Spacing::Horizontal => model.distribute_horizontal,
            Spacing::Vertical => model.distribute_vertical,
            Spacing::Tidy => model.tidy,
        };
        menu = menu.child(spacing_entry(spacing, availability, model, cx));
    }
    menu
}

/// One entry of the spacing menu.
fn spacing_entry<T: ArrangeHost>(
    spacing: Spacing,
    availability: Availability,
    model: &ArrangeModel,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let selector = format!("arrange-distribute-{}", spacing.command());
    let action = ArrangeAction::Spacing {
        ids: model.ids.clone(),
        spacing,
    };
    let mut entry = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap_1()
        .px(px(6.0))
        .py(px(4.0))
        .rounded_sm()
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(spacing.label().to_owned());
    if availability.is_editable() {
        entry = entry
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .on_click(cx.listener(move |host, _event, _window, cx| {
                host.run_arrange_action(action.clone(), cx);
            }));
    } else {
        entry = entry.cursor_default().opacity(0.5);
    }
    // A disabled entry says why it is disabled, in words, under itself.
    if let Some(reason) = availability.reason() {
        entry = entry.child(note(&format!("spacing-{}", spacing.command()), reason));
    }
    entry
}

/// The order row: the four commands that move a selection among its
/// siblings.
fn order_row<T: ArrangeHost>(model: &ArrangeModel, cx: &mut Context<T>) -> impl IntoElement {
    let mut column = div().flex().flex_col().gap_2();
    column = column.child(sublabel("Order"));
    for pair in Order::ALL.chunks(2) {
        let mut row = div().flex().gap_1();
        for order in pair {
            row = row.child(order_button(*order, model, cx));
        }
        column = column.child(row);
    }
    if let Some(reason) = model.order.reason() {
        column = column.child(note("order", reason));
    }
    column
}

/// One order button.
fn order_button<T: ArrangeHost>(
    order: Order,
    model: &ArrangeModel,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let selector = format!("arrange-order-{}", order.command());
    let action = ArrangeAction::Order {
        ids: model.ids.clone(),
        order,
    };
    let mut button = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex_1()
        .flex()
        .items_center()
        .h(px(26.0))
        .px(px(8.0))
        .rounded_sm()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(order.label().to_owned());
    if model.order.is_editable() {
        button = button
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .on_click(cx.listener(move |host, _event, _window, cx| {
                host.run_arrange_action(action.clone(), cx);
            }));
    } else {
        button = button.cursor_default().opacity(0.5);
    }
    button
}

/// The boolean row: the operation in use, and the button that changes it.
fn boolean_row<T: ArrangeHost>(
    model: &ArrangeModel,
    state: &Rc<RefCell<ArrangePanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let mut column = div().flex().flex_col().gap_2();
    column = column.child(sublabel("Boolean"));

    // The operation in use: a value, and a read-only one. figcraft shows it
    // as the button's icon; Spool has no icon-only buttons that say what
    // they are, so the value is spelled out.
    let shown = match model.boolean_value {
        BooleanValue::Set(op) => op.label().to_owned(),
        BooleanValue::Unset => NOT_SET.to_owned(),
        BooleanValue::Mixed => MIXED.to_owned(),
    };
    column = column.child(read_only("Operation", &shown));

    let selector = "arrange-boolean".to_owned();
    let open = state.borrow().open == Some(ArrangeMenu::Boolean);
    let mut button = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_2()
        .h(px(26.0))
        .px(px(8.0))
        .rounded_sm()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .child(icon(
            boolean_icon(model.boolean_value),
            14.0,
            theme::TEXT_SECONDARY,
        ))
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child("Boolean groups"),
        );
    if model.boolean.is_editable() {
        let shared = Rc::clone(state);
        button = button
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.open = if state.open == Some(ArrangeMenu::Boolean) {
                        None
                    } else {
                        Some(ArrangeMenu::Boolean)
                    };
                }
                cx.notify();
            }));
    } else {
        button = button.cursor_default().opacity(0.5);
    }
    column = column.child(button);
    if open {
        column = column.child(boolean_menu(model, cx));
    }
    if let Some(reason) = model.boolean.reason() {
        column = column.child(note("boolean", reason));
    }
    // The save path cannot write a boolean operation back, so the panel says
    // so where the change is made rather than leaving the user to find out
    // by saving.
    column = column.child(note("boolean-save", BOOLEAN_SAVE_NOTE));
    column
}

/// The icon the boolean button carries: the operation in use, as figcraft
/// shows it.
fn boolean_icon(value: BooleanValue) -> ArrangeIcon {
    match value {
        BooleanValue::Set(BoolOp::Union) | BooleanValue::Unset | BooleanValue::Mixed => {
            ArrangeIcon::Union
        }
        BooleanValue::Set(BoolOp::Subtract) => ArrangeIcon::Subtract,
        BooleanValue::Set(BoolOp::Intersect) => ArrangeIcon::Intersect,
        BooleanValue::Set(BoolOp::Exclude) => ArrangeIcon::Exclude,
    }
}

/// The boolean menu: the four operations and the flattening.
fn boolean_menu<T: ArrangeHost>(model: &ArrangeModel, cx: &mut Context<T>) -> impl IntoElement {
    let mut menu = div()
        .flex()
        .flex_col()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::SURFACE))
        .px(px(6.0))
        .py(px(4.0));
    for operation in [
        BoolOp::Union,
        BoolOp::Subtract,
        BoolOp::Intersect,
        BoolOp::Exclude,
    ] {
        menu = menu.child(boolean_entry(operation, model, cx));
    }
    menu = menu.child(flatten_entry(model, cx));
    menu
}

/// One entry of the boolean menu.
fn boolean_entry<T: ArrangeHost>(
    operation: BoolOp,
    model: &ArrangeModel,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let selector = format!("arrange-bool-{}", boolean_command(operation));
    let action = ArrangeAction::Boolean {
        ids: model.ids.clone(),
        operation,
    };
    let mut entry = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_2()
        .px(px(6.0))
        .py(px(4.0))
        .rounded_sm()
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(icon(operation_icon(operation), 12.0, theme::TEXT_SECONDARY))
        .child(operation.label().to_owned());
    if model.boolean.is_editable() {
        entry = entry
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .on_click(cx.listener(move |host, _event, _window, cx| {
                host.run_arrange_action(action.clone(), cx);
            }));
    } else {
        entry = entry.cursor_default().opacity(0.5);
    }
    entry
}

/// The flattening entry of the boolean menu.
fn flatten_entry<T: ArrangeHost>(model: &ArrangeModel, cx: &mut Context<T>) -> impl IntoElement {
    let selector = "arrange-bool-flatten".to_owned();
    let action = ArrangeAction::Flatten {
        ids: model.ids.clone(),
    };
    let mut entry = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .px(px(6.0))
        .py(px(4.0))
        .rounded_sm()
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child("Flatten");
    if model.boolean.is_editable() {
        entry = entry
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .on_click(cx.listener(move |host, _event, _window, cx| {
                host.run_arrange_action(action.clone(), cx);
            }));
    } else {
        entry = entry.cursor_default().opacity(0.5);
    }
    entry
}

/// The icon a boolean operation carries.
fn operation_icon(operation: BoolOp) -> ArrangeIcon {
    match operation {
        BoolOp::Union => ArrangeIcon::Union,
        BoolOp::Subtract => ArrangeIcon::Subtract,
        BoolOp::Intersect => ArrangeIcon::Intersect,
        BoolOp::Exclude => ArrangeIcon::Exclude,
    }
}

/// A run of text above the controls it names.
fn sublabel(text: &'static str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A line of text under the rows it belongs to.
///
/// The name is what makes the line findable: two lines that say the same
/// thing are still two lines, and each keeps its own name.
fn note(name: &str, text: &str) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("arrange-note-{name}")))
        .debug_selector(|| format!("arrange-note-{name}"))
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A value the panel shows but will not edit.
///
/// The value carries its own name in the panel's debug selectors, so a test
/// can say which value is on screen and not only that some value is.
fn read_only(label: &'static str, value: &str) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("arrange-read-only-{label}")))
        .debug_selector(|| format!("arrange-read-only-{label}"))
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px(px(8.0))
        .py(px(6.0))
        .rounded_sm()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(label.to_owned()),
        )
        .child(
            div()
                .id(SharedString::from(format!("arrange-value-{label}-{value}")))
                .debug_selector(|| format!("arrange-value-{label}-{value}"))
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(value.to_owned()),
        )
}

// ═══════════════════════════════════════════════════════════════════════════
// The icons
// ═══════════════════════════════════════════════════════════════════════════

/// The icons the Arrange surface draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrangeIcon {
    /// Align the left edges.
    AlignLeft,
    /// Align the horizontal centres.
    AlignHorizontalCenter,
    /// Align the right edges.
    AlignRight,
    /// Align the top edges.
    AlignTop,
    /// Align the vertical centres.
    AlignVerticalCenter,
    /// Align the bottom edges.
    AlignBottom,
    /// Space layers out horizontally.
    DistributeHorizontal,
    /// The union of two shapes.
    Union,
    /// One shape cut out of another.
    Subtract,
    /// The overlap of two shapes.
    Intersect,
    /// Two shapes with the overlap cut out of both.
    Exclude,
}

impl ArrangeIcon {
    /// The SVG body of the icon, on figcraft's 16×16 grid.
    ///
    /// The path data is transcribed from
    /// `vendor/figcraft/crates/ui-egui/src/icons.rs`, the same way
    /// `engine::layers` transcribed it for the Layers panel: a line becomes a
    /// path, a filled rectangle a filled `<rect>`, an outlined one a stroked
    /// `<rect>`. The paint colour is `#000` throughout, because GPUI's SVG
    /// path keeps only the alpha and tints with the element's text colour.
    pub fn body(self) -> &'static str {
        match self {
            Self::AlignLeft => {
                "<path d=\"M2.5 2V14\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" \
                 stroke-linecap=\"round\"/>\
                 <rect x=\"4\" y=\"4\" width=\"9\" height=\"3\" fill=\"#000\"/>\
                 <rect x=\"4\" y=\"9\" width=\"5\" height=\"3\" fill=\"#000\"/>"
            }
            Self::AlignHorizontalCenter => {
                "<path d=\"M7.5 2V14\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" \
                 stroke-linecap=\"round\"/>\
                 <rect x=\"2\" y=\"4\" width=\"11\" height=\"3\" fill=\"#000\"/>\
                 <rect x=\"4\" y=\"9\" width=\"7\" height=\"3\" fill=\"#000\"/>"
            }
            Self::AlignRight => {
                "<path d=\"M13.5 2V14\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" \
                 stroke-linecap=\"round\"/>\
                 <rect x=\"3\" y=\"4\" width=\"9\" height=\"3\" fill=\"#000\"/>\
                 <rect x=\"7\" y=\"9\" width=\"5\" height=\"3\" fill=\"#000\"/>"
            }
            Self::AlignTop => {
                "<path d=\"M2 2.5H14\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" \
                 stroke-linecap=\"round\"/>\
                 <rect x=\"4\" y=\"4\" width=\"3\" height=\"9\" fill=\"#000\"/>\
                 <rect x=\"9\" y=\"4\" width=\"3\" height=\"5\" fill=\"#000\"/>"
            }
            Self::AlignVerticalCenter => {
                "<path d=\"M2 7.5H14\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" \
                 stroke-linecap=\"round\"/>\
                 <rect x=\"4\" y=\"2\" width=\"3\" height=\"11\" fill=\"#000\"/>\
                 <rect x=\"9\" y=\"4\" width=\"3\" height=\"7\" fill=\"#000\"/>"
            }
            Self::AlignBottom => {
                "<path d=\"M2 13.5H14\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" \
                 stroke-linecap=\"round\"/>\
                 <rect x=\"4\" y=\"3\" width=\"3\" height=\"9\" fill=\"#000\"/>\
                 <rect x=\"9\" y=\"7\" width=\"3\" height=\"5\" fill=\"#000\"/>"
            }
            Self::DistributeHorizontal => {
                "<path d=\"M2.5 2V14M13.5 2V14\" stroke=\"#000\" stroke-width=\"1.1\" fill=\"none\" \
                 stroke-linecap=\"round\"/>\
                 <rect x=\"6\" y=\"4\" width=\"4\" height=\"8\" fill=\"#000\"/>"
            }
            Self::Union => {
                "<rect x=\"2.5\" y=\"2.5\" width=\"7\" height=\"7\" stroke=\"#000\" \
                 stroke-width=\"1.1\" fill=\"none\"/>\
                 <rect x=\"6.5\" y=\"6.5\" width=\"7\" height=\"7\" stroke=\"#000\" \
                 stroke-width=\"1.1\" fill=\"none\"/>"
            }
            Self::Subtract => {
                "<rect x=\"2\" y=\"2\" width=\"8\" height=\"8\" fill=\"#000\"/>\
                 <rect x=\"6.5\" y=\"6.5\" width=\"7\" height=\"7\" stroke=\"#000\" \
                 stroke-width=\"1.1\" fill=\"none\"/>"
            }
            Self::Intersect => {
                "<rect x=\"2.5\" y=\"2.5\" width=\"7\" height=\"7\" stroke=\"#000\" \
                 stroke-width=\"1.1\" fill=\"none\"/>\
                 <rect x=\"6.5\" y=\"6.5\" width=\"7\" height=\"7\" stroke=\"#000\" \
                 stroke-width=\"1.1\" fill=\"none\"/>\
                 <rect x=\"6\" y=\"6\" width=\"4\" height=\"4\" fill=\"#000\"/>"
            }
            Self::Exclude => {
                "<rect x=\"2\" y=\"2\" width=\"8\" height=\"4\" fill=\"#000\"/>\
                 <rect x=\"2\" y=\"6\" width=\"4\" height=\"4\" fill=\"#000\"/>\
                 <rect x=\"10\" y=\"6\" width=\"4\" height=\"8\" fill=\"#000\"/>\
                 <rect x=\"6\" y=\"10\" width=\"4\" height=\"4\" fill=\"#000\"/>\
                 <rect x=\"6.5\" y=\"6.5\" width=\"3\" height=\"3\" stroke=\"#000\" \
                 stroke-width=\"1.1\" fill=\"none\"/>"
            }
        }
    }
}

/// The icon as a standalone SVG document on a 16×16 viewBox.
pub fn icon_svg(icon: ArrangeIcon) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\" width=\"16\" height=\"16\">{}</svg>",
        icon.body()
    )
}

/// An icon element, drawn from the SVG bytes for `icon`.
fn icon(icon: ArrangeIcon, size_px: f32, color: u32) -> Svg {
    svg()
        .data(icon_svg(icon).as_bytes())
        .w(px(size_px))
        .h(px(size_px))
        .text_color(rgb(color))
        .flex_shrink_0()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use figcraft_doc::NodeKind;
    use figcraft_engine::{find_command, params::id_of, Session};
    use gpui::{point, size, Entity, Modifiers, Render, TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;

    // ── The documents the model reads ──────────────────────────────────────

    /// A document with one rectangle on its page.
    fn one_rectangle() -> (Document, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.pages[0];
        let id = doc
            .create(page, NodeKind::Rectangle, "Hero", 0.0, 0.0, 100.0, 100.0)
            .expect("a rectangle");
        (doc, id)
    }

    /// A document with two rectangles on its page, one of them well to the right.
    fn two_rectangles() -> (Document, NodeId, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.pages[0];
        let first = doc
            .create(page, NodeKind::Rectangle, "First", 0.0, 0.0, 100.0, 100.0)
            .expect("a rectangle");
        let second = doc
            .create(
                page,
                NodeKind::Rectangle,
                "Second",
                200.0,
                50.0,
                100.0,
                100.0,
            )
            .expect("a rectangle");
        (doc, first, second)
    }

    /// A document with three rectangles on its page.
    fn three_rectangles() -> (Document, Vec<NodeId>) {
        let mut doc = Document::new("test");
        let page = doc.pages[0];
        let mut ids = Vec::new();
        for (index, x) in [0.0, 50.0, 300.0].into_iter().enumerate() {
            let id = doc
                .create(
                    page,
                    NodeKind::Rectangle,
                    &format!("Rectangle {index}"),
                    x,
                    0.0,
                    10.0,
                    10.0,
                )
                .expect("a rectangle");
            ids.push(id);
        }
        (doc, ids)
    }

    /// A session over two overlapping rectangles, and the boolean group the
    /// engine makes of them.
    ///
    /// The group is the engine's own: the panel never makes one itself.
    fn boolean_group() -> (Session, NodeId, NodeId, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.pages[0];
        let first = doc
            .create(page, NodeKind::Rectangle, "First", 0.0, 0.0, 100.0, 100.0)
            .expect("a rectangle");
        let second = doc
            .create(
                page,
                NodeKind::Rectangle,
                "Second",
                50.0,
                50.0,
                100.0,
                100.0,
            )
            .expect("a rectangle");
        let mut session = Session::with_document(doc);
        let result = session
            .execute("bool.subtract", &json!({ "ids": [first.0, second.0] }))
            .expect("the engine makes the boolean group");
        let group = id_of(&result["id"]).expect("the group's id");
        (session, group, first, second)
    }

    // ── The model: what the buttons are for ────────────────────────────────

    #[test]
    fn nothing_selected_enables_nothing_and_says_why() {
        let (doc, _id) = one_rectangle();
        let model = selected_arrange(&doc, &[]);
        assert!(model.ids.is_empty(), "nothing is selected");
        assert_eq!(model.align, Availability::Disabled(ALIGN_REASON));
        assert_eq!(model.distribute, Availability::Disabled(TIDY_REASON));
        assert_eq!(
            model.distribute_horizontal,
            Availability::Disabled(DISTRIBUTE_REASON)
        );
        assert_eq!(
            model.distribute_vertical,
            Availability::Disabled(DISTRIBUTE_REASON)
        );
        assert_eq!(model.tidy, Availability::Disabled(TIDY_REASON));
        assert_eq!(model.order, Availability::Disabled(ORDER_REASON));
        assert_eq!(model.boolean, Availability::Disabled(COMBINE_ONE_REASON));
        assert_eq!(model.boolean_value, BooleanValue::Unset);
    }

    #[test]
    fn every_disabled_control_says_why_out_loud() {
        let (doc, _id) = one_rectangle();
        let model = selected_arrange(&doc, &[]);
        for (name, availability) in [
            ("align", model.align),
            ("distribute", model.distribute),
            ("distribute horizontal", model.distribute_horizontal),
            ("distribute vertical", model.distribute_vertical),
            ("tidy", model.tidy),
            ("order", model.order),
            ("boolean", model.boolean),
        ] {
            let reason = availability
                .reason()
                .unwrap_or_else(|| panic!("{name} is disabled and says nothing about why"));
            assert!(!reason.trim().is_empty(), "{name} says nothing");
        }
    }

    #[test]
    fn a_layer_alone_on_the_page_cannot_be_aligned() {
        // figcraft's own rule: a layer with no frame to align to has nothing
        // to align to, so the alignment buttons are off and say so.
        let (doc, id) = one_rectangle();
        let model = selected_arrange(&doc, &[id]);
        assert_eq!(
            model.align,
            Availability::Disabled(ALIGN_REASON),
            "a layer on the page has nothing to align to"
        );
        assert_eq!(
            model.align.reason(),
            Some("A layer on its own aligns to its frame: select more layers, or one in a frame"),
            "figcraft's own words"
        );
    }

    #[test]
    fn a_layer_in_a_frame_aligns_to_its_frame() {
        let mut doc = Document::new("test");
        let page = doc.pages[0];
        let frame = doc
            .create(page, NodeKind::Frame, "Frame", 0.0, 0.0, 400.0, 400.0)
            .expect("a frame");
        let inner = doc
            .create(frame, NodeKind::Rectangle, "Inner", 10.0, 10.0, 50.0, 50.0)
            .expect("a rectangle");
        let model = selected_arrange(&doc, &[inner]);
        assert_eq!(
            model.align,
            Availability::Supported,
            "a layer in a frame has its frame to align to"
        );
    }

    #[test]
    fn two_layers_on_the_page_can_be_aligned() {
        let (doc, first, second) = two_rectangles();
        let model = selected_arrange(&doc, &[first, second]);
        assert_eq!(model.align, Availability::Supported);
    }

    #[test]
    fn the_order_buttons_need_one_layer() {
        let (doc, id) = one_rectangle();
        assert_eq!(
            selected_arrange(&doc, &[]).order,
            Availability::Disabled(ORDER_REASON)
        );
        assert_eq!(selected_arrange(&doc, &[id]).order, Availability::Supported);
    }

    #[test]
    fn the_distribute_menu_opens_for_two_and_its_entries_need_three() {
        let (doc, first, second) = two_rectangles();
        let model = selected_arrange(&doc, &[first, second]);
        assert_eq!(
            model.distribute,
            Availability::Supported,
            "the menu holds a tidy-up, which two layers are enough for"
        );
        assert_eq!(
            model.tidy,
            Availability::Supported,
            "two layers can be tidied"
        );
        assert_eq!(
            model.distribute_horizontal,
            Availability::Disabled(DISTRIBUTE_REASON),
            "the engine spaces three layers and no fewer"
        );
        assert_eq!(
            model.distribute_vertical,
            Availability::Disabled(DISTRIBUTE_REASON)
        );

        let (doc, ids) = three_rectangles();
        let model = selected_arrange(&doc, &ids);
        assert_eq!(model.distribute, Availability::Supported);
        assert_eq!(model.distribute_horizontal, Availability::Supported);
        assert_eq!(model.distribute_vertical, Availability::Supported);
        assert_eq!(model.tidy, Availability::Supported);
    }

    #[test]
    fn two_shapes_can_be_combined_and_one_cannot() {
        let (doc, first, second) = two_rectangles();
        assert_eq!(
            selected_arrange(&doc, &[first]).boolean,
            Availability::Disabled(COMBINE_REASON),
            "one shape cannot be combined with anything"
        );
        assert_eq!(
            selected_arrange(&doc, &[first, second]).boolean,
            Availability::Supported
        );
    }

    #[test]
    fn a_section_cannot_be_combined_with_anything() {
        // The engine refuses to wrap a section in a group, so the panel must
        // not offer to.
        let mut doc = Document::new("test");
        let page = doc.pages[0];
        let section = doc
            .create(
                page,
                NodeKind::Section {
                    contents_hidden: false,
                },
                "Section",
                0.0,
                0.0,
                200.0,
                200.0,
            )
            .expect("a section");
        let other = doc
            .create(page, NodeKind::Rectangle, "Other", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let model = selected_arrange(&doc, &[section, other]);
        assert_eq!(
            model.boolean,
            Availability::Disabled(COMBINE_SECTION_REASON),
            "the engine would refuse the group the panel is offering"
        );
    }

    #[test]
    fn a_boolean_group_shows_the_operation_it_uses() {
        let (session, group, _first, _second) = boolean_group();
        let doc = session.doc().expect("a document");
        let model = selected_arrange(doc, &[group]);
        assert_eq!(
            model.boolean_value,
            BooleanValue::Set(BoolOp::Subtract),
            "the operation the group uses"
        );
        assert_eq!(
            model.boolean,
            Availability::Supported,
            "a boolean group can change its own operation"
        );
    }

    #[test]
    fn operands_that_disagree_read_as_mixed() {
        let (mut session, group, first, second) = boolean_group();
        // The top operand gets an operation of its own, so the two disagree.
        session
            .execute("bool.union", &json!({ "ids": [second.0] }))
            .expect("the operand's own operation");
        let doc = session.doc().expect("a document");
        let model = selected_arrange(doc, &[first, second]);
        assert_eq!(
            model.boolean_value,
            BooleanValue::Mixed,
            "the two operands do not agree on how they combine"
        );
        assert_eq!(
            model.boolean,
            Availability::Supported,
            "operands can still change their own operation"
        );
        let _ = group;
    }

    #[test]
    fn ordinary_layers_have_no_boolean_operation() {
        let (doc, first, second) = two_rectangles();
        let model = selected_arrange(&doc, &[first, second]);
        assert_eq!(
            model.boolean_value,
            BooleanValue::Unset,
            "two ordinary layers are not a boolean at all"
        );
        assert_eq!(model.boolean_value.word(), NOT_SET);
    }

    #[test]
    fn the_mixed_operation_does_not_hide_the_rest_of_the_panel() {
        // The fold is per property: the operation is mixed, and the
        // alignment and the order are still what they were.
        let (mut session, _group, first, second) = boolean_group();
        session
            .execute("bool.union", &json!({ "ids": [second.0] }))
            .expect("the operand's own operation");
        let doc = session.doc().expect("a document");
        let model = selected_arrange(doc, &[first, second]);
        assert_eq!(model.boolean_value, BooleanValue::Mixed);
        assert_eq!(model.boolean_value.word(), MIXED);
        assert_eq!(
            model.order,
            Availability::Supported,
            "one property disagreeing does not disable the others"
        );
        assert_eq!(model.align, Availability::Supported);
    }

    // ── The commands ───────────────────────────────────────────────────────

    #[test]
    fn every_command_the_panel_names_is_the_engines_own() {
        let mut ids = Vec::new();
        for alignment in Align::ALL {
            ids.push(alignment.command());
        }
        for order in Order::ALL {
            ids.push(order.command());
        }
        for spacing in Spacing::ALL {
            ids.push(spacing.command());
        }
        for operation in [
            BoolOp::Union,
            BoolOp::Subtract,
            BoolOp::Intersect,
            BoolOp::Exclude,
        ] {
            ids.push(boolean_command(operation));
        }
        ids.push("bool.flatten");
        for id in ids {
            assert!(
                find_command(id).is_some(),
                "the engine has no command called {id}"
            );
        }
    }

    #[test]
    fn the_engines_own_labels_are_the_labels_the_panel_shows() {
        for alignment in Align::ALL {
            let spec = find_command(alignment.command()).expect("the engine's command");
            assert_eq!(
                alignment.label(),
                spec.label,
                "{} is labelled differently by the engine",
                alignment.command()
            );
        }
        for order in Order::ALL {
            let spec = find_command(order.command()).expect("the engine's command");
            assert_eq!(order.label(), spec.label, "{}", order.command());
        }
        for spacing in Spacing::ALL {
            let spec = find_command(spacing.command()).expect("the engine's command");
            assert_eq!(spacing.label(), spec.label, "{}", spacing.command());
        }
    }

    #[test]
    fn the_boolean_operations_are_named_the_way_figcraft_names_them() {
        // figcraft's menu says the operation's own name, not the command's.
        assert_eq!(BoolOp::Union.label(), "Union");
        assert_eq!(BoolOp::Subtract.label(), "Subtract");
        assert_eq!(BoolOp::Intersect.label(), "Intersect");
        assert_eq!(BoolOp::Exclude.label(), "Exclude");
        assert_eq!(boolean_command(BoolOp::Union), "bool.union");
        assert_eq!(boolean_command(BoolOp::Subtract), "bool.subtract");
        assert_eq!(boolean_command(BoolOp::Intersect), "bool.intersect");
        assert_eq!(boolean_command(BoolOp::Exclude), "bool.exclude");
    }

    #[test]
    fn the_panel_builds_the_engines_param_names() {
        let ids = vec![NodeId(7), NodeId(9)];
        let (command, params) = ArrangeAction::Align {
            ids: ids.clone(),
            alignment: Align::Left,
        }
        .command();
        assert_eq!(command, "align.left");
        assert_eq!(params, json!({ "ids": [7, 9] }));

        let (command, params) = ArrangeAction::Order {
            ids: ids.clone(),
            order: Order::Front,
        }
        .command();
        assert_eq!(command, "arrange.front");
        assert_eq!(params, json!({ "ids": [7, 9] }));

        let (command, params) = ArrangeAction::Spacing {
            ids: ids.clone(),
            spacing: Spacing::Tidy,
        }
        .command();
        assert_eq!(command, "arrange.tidy");
        assert_eq!(params, json!({ "ids": [7, 9] }));

        let (command, params) = ArrangeAction::Boolean {
            ids: ids.clone(),
            operation: BoolOp::Intersect,
        }
        .command();
        assert_eq!(command, "bool.intersect");
        assert_eq!(params, json!({ "ids": [7, 9] }));

        let (command, params) = ArrangeAction::Flatten { ids }.command();
        assert_eq!(command, "bool.flatten");
        assert_eq!(params, json!({ "ids": [7, 9] }));
    }

    #[test]
    fn the_engines_align_command_moves_the_layers_the_panel_names() {
        let (mut session, first, second) = {
            let mut doc = Document::new("test");
            let page = doc.pages[0];
            let first = doc
                .create(page, NodeKind::Rectangle, "First", 0.0, 0.0, 100.0, 100.0)
                .expect("a rectangle");
            let second = doc
                .create(
                    page,
                    NodeKind::Rectangle,
                    "Second",
                    200.0,
                    50.0,
                    100.0,
                    100.0,
                )
                .expect("a rectangle");
            (Session::with_document(doc), first, second)
        };
        let (command, params) = ArrangeAction::Align {
            ids: vec![first, second],
            alignment: Align::Left,
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the engine accepts the panel's command");
        let doc = session.doc().expect("a document");
        let moved = doc.world_bounds(second).expect("the second layer's box");
        assert_eq!(
            moved.x0, 0.0,
            "the second layer moved to the first one's edge"
        );
        assert_eq!(moved.y0, 50.0, "and only sideways");
    }

    #[test]
    fn the_engines_order_command_reorders_the_siblings() {
        let (mut session, ids) = {
            let (doc, ids) = three_rectangles();
            (Session::with_document(doc), ids)
        };
        let (command, params) = ArrangeAction::Order {
            ids: vec![ids[0]],
            order: Order::Back,
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the engine accepts the panel's command");
        let doc = session.doc().expect("a document");
        let page = doc.pages[0];
        let children = doc.node(page).expect("the page").children.clone();
        assert_eq!(
            children.first().copied(),
            Some(ids[0]),
            "the layer went to the back"
        );
    }

    #[test]
    fn the_engines_distribute_command_spaces_the_selection_out() {
        let (mut session, ids) = {
            let (doc, ids) = three_rectangles();
            (Session::with_document(doc), ids)
        };
        let (command, params) = ArrangeAction::Spacing {
            ids: ids.clone(),
            spacing: Spacing::Horizontal,
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the engine accepts the panel's command");
        let doc = session.doc().expect("a document");
        let middle = doc.world_bounds(ids[1]).expect("the middle layer's box");
        // 0, 50 and 300 become 0, 150 and 300: the gaps are even.
        assert_eq!(middle.x0, 150.0);
    }

    #[test]
    fn the_engines_boolean_command_makes_a_boolean_group() {
        let (mut session, first, second) = {
            let mut doc = Document::new("test");
            let page = doc.pages[0];
            let first = doc
                .create(page, NodeKind::Rectangle, "First", 0.0, 0.0, 100.0, 100.0)
                .expect("a rectangle");
            let second = doc
                .create(
                    page,
                    NodeKind::Rectangle,
                    "Second",
                    50.0,
                    50.0,
                    100.0,
                    100.0,
                )
                .expect("a rectangle");
            (Session::with_document(doc), first, second)
        };
        let (command, params) = ArrangeAction::Boolean {
            ids: vec![first, second],
            operation: BoolOp::Union,
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the engine accepts the panel's command");
        let doc = session.doc().expect("a document");
        let group = doc
            .node(first)
            .expect("the first layer")
            .parent
            .expect("the group the two layers went into");
        assert!(
            booleans::is_group(doc, group),
            "the engine made a boolean group"
        );
    }

    // ── The panel's own state ──────────────────────────────────────────────

    #[test]
    fn the_panel_state_remembers_which_menu_is_open() {
        let mut state = ArrangePanelState::new();
        assert!(!state.is_busy(), "nothing is open to begin with");
        state.open = Some(ArrangeMenu::Distribute);
        assert!(state.is_busy(), "a menu is open");
        assert!(state.cancel(), "the escape was the section's");
        assert!(!state.is_busy());
        assert!(!state.cancel(), "and the next one is not");
    }

    // ── The icons ──────────────────────────────────────────────────────────

    #[test]
    fn every_icon_emits_one_well_formed_svg() {
        for icon in [
            ArrangeIcon::AlignLeft,
            ArrangeIcon::AlignHorizontalCenter,
            ArrangeIcon::AlignRight,
            ArrangeIcon::AlignTop,
            ArrangeIcon::AlignVerticalCenter,
            ArrangeIcon::AlignBottom,
            ArrangeIcon::DistributeHorizontal,
            ArrangeIcon::Union,
            ArrangeIcon::Subtract,
            ArrangeIcon::Intersect,
            ArrangeIcon::Exclude,
        ] {
            let markup = icon_svg(icon);
            assert!(
                markup
                    .starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\""),
                "{icon:?}"
            );
            assert!(markup.ends_with("</svg>"), "{icon:?}");
            assert!(markup.contains("#000"), "{icon:?} paints nothing");
            assert_eq!(
                markup.matches('<').count(),
                markup.matches('>').count(),
                "{icon:?}"
            );
            assert!(!markup.contains("NaN"), "{icon:?}");
        }
    }

    // ── The panel, in a window ─────────────────────────────────────────────

    /// A host for the section in a window: it draws the section, remembers
    /// what the section asked for, and does what it is asked.
    struct Host {
        model: ArrangeModel,
        state: Rc<RefCell<ArrangePanelState>>,
        asked: Rc<RefCell<Vec<ArrangeAction>>>,
    }

    impl Host {
        fn new(model: ArrangeModel) -> Self {
            Self {
                model,
                state: Rc::new(RefCell::new(ArrangePanelState::new())),
                asked: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl ArrangeHost for Host {
        fn run_arrange_action(&mut self, action: ArrangeAction, cx: &mut Context<Self>) {
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
            section(&model, &state, cx)
        }
    }

    /// Open a window with the section in it, over the given model.
    fn open_section(
        cx: &mut TestAppContext,
        model: ArrangeModel,
    ) -> (Entity<Host>, &mut VisualTestContext) {
        cx.add_window_view(|_, _| Host::new(model))
    }

    /// Draw the section in a window and settle.
    fn draw(root: &Entity<Host>, window: &mut VisualTestContext) {
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(320.0), px(1000.0)),
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

    /// The model a selection of two rectangles gives.
    fn two_layer_model() -> ArrangeModel {
        let (doc, first, second) = two_rectangles();
        selected_arrange(&doc, &[first, second])
    }

    #[gpui::test]
    fn the_section_draws_the_six_alignments_in_a_window(cx: &mut TestAppContext) {
        let (root, window) = open_section(cx, two_layer_model());
        draw(&root, window);

        for alignment in Align::ALL {
            let selector: &'static str =
                Box::leak(format!("arrange-align-{}", alignment.slug()).into_boxed_str());
            assert!(
                window.debug_bounds(selector).is_some(),
                "the section draws {alignment:?}"
            );
        }
        // The order row, and the distribute button beside the alignments.
        assert!(window.debug_bounds("arrange-order-arrange.front").is_some());
        assert!(window.debug_bounds("arrange-distribute").is_some());
        assert!(window.debug_bounds("arrange-boolean").is_some());
    }

    #[gpui::test]
    fn clicking_an_alignment_asks_the_engine_to_align(cx: &mut TestAppContext) {
        let (root, window) = open_section(cx, two_layer_model());
        draw(&root, window);

        let button = bounds(window, "arrange-align-left");
        window.simulate_click(button.center(), Modifiers::none());
        window.run_until_parked();

        let asked = root.read_with(cx, |host, _| host.asked.borrow().clone());
        assert_eq!(asked.len(), 1, "the section asked for one change");
        let (command, params) = asked[0].command();
        assert_eq!(command, "align.left");
        let model = two_layer_model();
        assert_eq!(
            params,
            json!({ "ids": model.ids.iter().map(|id| id.0).collect::<Vec<_>>() })
        );
    }

    #[gpui::test]
    fn a_disabled_alignment_button_asks_for_nothing(cx: &mut TestAppContext) {
        let (doc, id) = one_rectangle();
        let model = selected_arrange(&doc, &[id]);
        assert_eq!(model.align, Availability::Disabled(ALIGN_REASON));
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let button = bounds(window, "arrange-align-left");
        window.simulate_click(button.center(), Modifiers::none());
        window.run_until_parked();

        // And it says why, in words, where it is.
        assert!(
            window.debug_bounds("arrange-note-align").is_some(),
            "the disabled alignment says why it is off"
        );

        let asked = root.read_with(cx, |host, _| host.asked.borrow().clone());
        assert!(asked.is_empty(), "a disabled button asks for nothing");
    }

    #[gpui::test]
    fn nothing_selected_shows_the_empty_state(cx: &mut TestAppContext) {
        let (doc, _id) = one_rectangle();
        let model = selected_arrange(&doc, &[]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        assert!(
            window.debug_bounds("arrange-empty").is_some(),
            "the section says that nothing is selected"
        );
        assert!(
            window.debug_bounds("arrange-align-left").is_none(),
            "and offers no alignment for a selection of nothing"
        );
    }

    #[gpui::test]
    fn the_distribute_menu_offers_the_three_commands(cx: &mut TestAppContext) {
        let (doc, ids) = three_rectangles();
        let model = selected_arrange(&doc, &ids);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        // The menu is closed to begin with.
        assert!(
            window
                .debug_bounds("arrange-distribute-distribute.horizontal")
                .is_none(),
            "the menu is closed"
        );

        let button = bounds(window, "arrange-distribute");
        window.simulate_click(button.center(), Modifiers::none());
        window.run_until_parked();

        for spacing in Spacing::ALL {
            let selector: &'static str =
                Box::leak(format!("arrange-distribute-{}", spacing.command()).into_boxed_str());
            assert!(
                window.debug_bounds(selector).is_some(),
                "the menu offers {}",
                spacing.label()
            );
        }
    }

    #[gpui::test]
    fn a_distribute_entry_that_cannot_run_says_why(cx: &mut TestAppContext) {
        let (doc, first, second) = two_rectangles();
        let model = selected_arrange(&doc, &[first, second]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let button = bounds(window, "arrange-distribute");
        window.simulate_click(button.center(), Modifiers::none());
        window.run_until_parked();

        assert!(
            window
                .debug_bounds("arrange-note-spacing-distribute.horizontal")
                .is_some(),
            "the entry that needs three layers says so"
        );
    }

    #[gpui::test]
    fn the_boolean_menu_offers_the_four_operations_and_the_flattening(cx: &mut TestAppContext) {
        let (doc, first, second) = two_rectangles();
        let model = selected_arrange(&doc, &[first, second]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let button = bounds(window, "arrange-boolean");
        window.simulate_click(button.center(), Modifiers::none());
        window.run_until_parked();

        for operation in [
            BoolOp::Union,
            BoolOp::Subtract,
            BoolOp::Intersect,
            BoolOp::Exclude,
        ] {
            let selector: &'static str =
                Box::leak(format!("arrange-bool-{}", boolean_command(operation)).into_boxed_str());
            assert!(
                window.debug_bounds(selector).is_some(),
                "the menu offers {operation:?}"
            );
        }
        assert!(
            window.debug_bounds("arrange-bool-flatten").is_some(),
            "the menu offers the flattening"
        );
    }

    #[gpui::test]
    fn the_operation_in_use_is_shown_read_only(cx: &mut TestAppContext) {
        let (session, group, _first, _second) = boolean_group();
        let doc = session.doc().expect("a document");
        let model = selected_arrange(doc, &[group]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        assert!(
            window
                .debug_bounds("arrange-value-Operation-Subtract")
                .is_some(),
            "the panel shows the operation the boolean group uses"
        );
    }

    #[gpui::test]
    fn a_boolean_operation_nobody_set_says_so(cx: &mut TestAppContext) {
        let (doc, first, second) = two_rectangles();
        let model = selected_arrange(&doc, &[first, second]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        assert!(
            window
                .debug_bounds("arrange-value-Operation-Not set")
                .is_some(),
            "two ordinary layers are not a boolean, and the panel says so"
        );
    }
}
