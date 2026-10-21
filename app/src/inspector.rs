//! The Inspector: the surface that shows what is selected and asks for changes
//! to it.
//!
//! # What this module owns
//!
//! A *surface*, not a store. Three things live here:
//!
//! 1. The vocabulary every row is built from — [`Value`], [`Availability`],
//!    [`NumberSpec`], [`Property`], [`Paint`], [`Focus`].
//! 2. The interaction grammars — [`NumberEdit`] for typing and [`Scrub`] for
//!    dragging — as pure functions of what happened, so they can be pinned down
//!    in tests without a window.
//! 3. The widgets that draw them, and the composition that decides which rows
//!    belong to which selection.
//!
//! # What this module must never own
//!
//! Document state, and any way to change it. Every control here ends in
//! [`Inspector::commit_number`] or [`Inspector::commit_paint`], which hand a
//! request to the engine — the one place that owns geometry, appearance and
//! the history boundary. That boundary is why adding rotation, constraints,
//! alignment or auto-layout is a new [`Property`] and a new row rather than an
//! edit to the engine or to a gesture handler:
//!
//! - one typed value is one semantic operation, because it is one engine
//!   command;
//! - one drag is one semantic operation, because [`Scrub`] holds an unrecorded
//!   gesture and the engine records it when the drag ends — this module never
//!   sees the individual moves, so it cannot record one per frame;
//! - a drag that ends where it started records nothing, and Escape abandons a
//!   gesture rather than committing it, because both are already decisions the
//!   engine makes.
//!
//! # The document this panel reads
//!
//! The values come from the figcraft engine, through
//! [`crate::engine::inspector_model`]: the panel reads the engine's
//! [`Document`](figcraft_doc::Document) and writes through the engine's
//! command surface (`node.set`, `object.rename`, …). Nothing here reads the
//! retired runtime's document, and nothing here writes to it.
//!
//! # The three states of a property
//!
//! A property of a *selection* is never just a value. It is one of three
//! things — set, unset, or mixed — and all three are first class.
//! [`Value::across`] folds a selection's readings into exactly one of them, so
//! `Mixed` can never be produced by a row that forgot to check, and can never
//! be a number the user did not choose and could not act on.
//!
//! The three states are not equally editable, and the difference is the point:
//! paint can be set across a mixed selection because "make these all this
//! colour" is one operation, while geometry cannot be, because a box has one
//! position and a mixed selection has no agreed one to show. That asymmetry
//! lives in [`Inspector::availability`], not in each row.

use gpui::{
    div, prelude::*, px, rgb, AnyElement, Context, Div, Entity, FontWeight, IntoElement,
    MouseButton, Render, SharedString, Stateful, Window,
};
use serde_json::Value as JsonValue;

use std::cell::RefCell;
use std::rc::Rc;

use crate::engine::canvas::EngineCanvas;
use crate::engine::inspector_model as model;
use crate::engine::panel_a11y::{
    self as panel_a11y, A11yAction, A11yHost, A11yModel, A11yPanelState,
};
use crate::engine::panel_assets::{
    self as panel_assets, AssetsAction, AssetsHost, AssetsModel, AssetsPanelState,
};
use crate::engine::panel_colorpicker::{
    self as panel_colorpicker, ColorAction, ColorHost, ColorPickerState,
};
use crate::engine::panel_components::{
    self as panel_components, ComponentAction, ComponentsHost, ComponentsModel,
    ComponentsPanelState,
};
use crate::engine::panel_devmode::{
    self as panel_devmode, CategoryFilter, DevAction, DevHost, DevModeModel, DevPanelState,
};
use crate::engine::panel_effects::EffectsPanel;
use crate::engine::panel_export::{
    self as panel_export, ExportAction, ExportHost, ExportModel, ExportPanelState,
};
use crate::engine::panel_prototyping::{
    self as panel_prototyping, ProtoAction, ProtoHost, ProtoModel, ProtoPanelState,
};
use crate::engine::panel_styles::{
    self as panel_styles, StylesAction, StylesHost, StylesModel, StylesPanelState,
};
use crate::engine::panel_typography::{
    self as panel_typography, TypographyAction, TypographyHost, TypographyModel,
    TypographyPanelState,
};
use crate::engine::panel_variables::{
    self as panel_variables, VariablesAction, VariablesHost, VariablesModel, VariablesPanelState,
};
use crate::theme;

/// The word for a property the selected objects disagree about.
///
/// `selection.md` is explicit that this is a state and not a number: a sidebar
/// "shows a value when uniform and a 'mixed' state otherwise". Reusing one word
/// everywhere is what makes that true across every section rather than only in
/// the one that remembers.
const MIXED: &str = "Mixed";

/// The word for a property nobody has set.
const UNSET: &str = "None";

/// The word for a layout property nobody has set.
///
/// The engine's word for it, not the panel's. A paint nobody filled is "None"
/// here, but the layout section reads the engine's own model, and a gap that
/// is not there is "Not set" — the same word the engine's own panel uses for a
/// value it does not have.
const NOT_SET: &str = "Not set";

/// The word for a gap the flow makes up rather than the user typing.
///
/// The engine's own word: a layout whose items are spread by the free space
/// has no gap of its own, and says so rather than showing a number that is
/// not in use.
const AUTO: &str = "Auto";

/// The mark for a number that exists but has no value to print.
const NO_VALUE: &str = "—";

/// Why a geometry field on a mixed selection is read-only.
///
/// A number here would have to be an average, and an average is a position the
/// user never chose. The field says so instead of showing one.
const MIXED_GEOMETRY: &str = "Mixed selection has no agreed position";

/// Why a stroke width is unavailable when there is no stroke.
const NO_STROKE: &str = "No stroke to be wide";

// ═══════════════════════════════════════════════════════════════════════════
// Sections
// ═══════════════════════════════════════════════════════════════════════════

/// How many collapsible sections the Inspector can have.
///
/// Fixed rather than growable, so a section's slot can be baked into the click
/// closure that toggles it.
pub const SECTION_COUNT: usize = 10;

/// A collapsible section, addressed by a slot.
///
/// The *name* of a section travels with the call and the slot is what a click
/// writes, so a section can never collapse the wrong one by being renamed or
/// moved. The slot is private: only the constants below can name one, so adding
/// a section without giving it a slot is a compile error rather than an index
/// panic inside a render, where nothing can catch it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionSlot(usize);

impl SectionSlot {
    pub const POSITION: Self = Self(0);
    pub const LAYOUT: Self = Self(1);
    pub const APPEARANCE: Self = Self(2);
    pub const TYPOGRAPHY: Self = Self(3);
    pub const EFFECTS: Self = Self(4);
    pub const EXPORT: Self = Self(5);
    pub const STYLES: Self = Self(6);
    pub const VARIABLES: Self = Self(7);
    pub const ASSETS: Self = Self(8);
    pub const A11Y: Self = Self(9);

    fn index(self) -> usize {
        self.0
    }
}

/// Which sections are collapsed.
///
/// One flag per slot, in section order. Collapsed is presentation only: it is
/// never in the document, and collapsing a section cannot change what any row
/// says.
#[derive(Clone, Copy, Debug)]
pub struct Sections {
    collapsed: [bool; SECTION_COUNT],
}

impl Sections {
    pub const fn new() -> Self {
        Self {
            collapsed: [false; SECTION_COUNT],
        }
    }

    pub fn is_collapsed(&self, slot: SectionSlot) -> bool {
        self.collapsed[slot.index()]
    }

    pub fn toggle(&mut self, slot: SectionSlot) {
        self.collapsed[slot.index()] = !self.collapsed[slot.index()];
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The three states of a property
// ═══════════════════════════════════════════════════════════════════════════

/// What one property is across a whole selection.
///
/// Three states, never two. [`Value::Set`] is a value every selected object
/// agrees on, [`Value::Unset`] is agreement that the property is off or was
/// never authored, and [`Value::Mixed`] is the absence of agreement. A panel
/// that only had a value and a nothing would have to invent a third thing at
/// every row that faced a disagreement, and inventing it per row is how a panel
/// starts showing numbers the user never chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value<T> {
    Set(T),
    Unset,
    Mixed,
}

impl<T: PartialEq> Value<T> {
    /// Fold a selection's per-object readings into the one state that is honest
    /// for all of them.
    ///
    /// `None` for an object means "this property is not set on that object",
    /// which is an answer rather than missing data — so the fold is over
    /// `Option<T>` and never has to invent a default.
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
                // A disagreement can only ever widen. Once two objects
                // differ, no later agreement can make the selection
                // uniform again, so `Mixed` absorbs everything after it.
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

    /// Whether the selection disagrees about this property.
    ///
    /// Taken by reference because `is_*` is a question about the value, not a
    /// consuming transform of it. `Value` is `Copy`, so the borrow costs nothing
    /// and the name stays the clearest one at the call site.
    pub fn is_mixed(&self) -> bool {
        matches!(self, Self::Mixed)
    }

    /// Whether the selection agrees the property is off or was never authored.
    ///
    /// Taken by reference because `is_*` is a question about the value, not a
    /// consuming transform of it.
    pub fn is_unset(&self) -> bool {
        matches!(self, Self::Unset)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Availability
// ═══════════════════════════════════════════════════════════════════════════

/// Whether a control can be used on the selection in front of it, and why not.
///
/// The reason is not decoration. Affinity's own rule is that an action which
/// does not apply is *disabled* rather than silently absent, and Figma's is that
/// a property with no meaning for this layer says so — vertical text alignment
/// only works on fixed-size text layers. A row that cannot answer these three
/// questions has no way to tell the truth about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// The control works here.
    Supported,
    /// The control is real and has a value, but does not apply to this
    /// selection right now. The value stays readable.
    Disabled(&'static str),
}

impl Availability {
    pub fn is_editable(self) -> bool {
        matches!(self, Self::Supported)
    }

    /// Why this control cannot be used, in the words the panel shows.
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Supported => None,
            Self::Disabled(reason) => Some(reason),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Numbers
// ═══════════════════════════════════════════════════════════════════════════

/// How many decimal places a number is written with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    /// Whole units.
    Whole,
    /// Whole units, or one decimal place when the value is not whole.
    Tenth,
}

impl Precision {
    fn decimals(self) -> usize {
        match self {
            Self::Whole => 0,
            Self::Tenth => 1,
        }
    }
}

/// Everything about a number that is not its value.
///
/// Held in one place so that "pixels", "a proportion" and "a length with a
/// floor" cannot each invent their own rounding, their own suffix, or their own
/// idea of what a drag step is. The unit the user reads is `scale` away from
/// the unit the document stores, which is the only reason opacity needs a spec at
/// all rather than a special case.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumberSpec {
    /// How far one pixel of drag moves the value, in display units.
    pub step: f32,
    /// The finest step a drag may stop on, in display units.
    ///
    /// A drag that could settle between two values that read the same would make
    /// the field flicker, so a scrub lands on the quantum rather than wherever
    /// the pointer happened to be.
    pub step_quantum: f32,
    pub precision: Precision,
    /// Appended to the formatted number: `%`, or nothing.
    pub suffix: &'static str,
    /// Display units per stored unit. 100 for a proportion stored as 0 to 1.
    pub scale: f32,
    /// The display-unit floor this property cannot go below, if it has one.
    pub min: Option<f32>,
    pub max: Option<f32>,
}

impl NumberSpec {
    /// A length in pixels, with a floor.
    ///
    /// A floor of `None` is a position, which may sit off the left edge. A floor
    /// of one pixel is a size, because a zero-sized box cannot be hit again, so
    /// shrinking one to nothing would make it unrecoverable rather than small.
    const fn length(min: Option<f32>) -> Self {
        Self {
            step: 1.0,
            step_quantum: 1.0,
            precision: Precision::Tenth,
            suffix: "",
            scale: 1.0,
            min,
            max: None,
        }
    }

    /// A proportion from 0 to 100, stored as 0 to 1.
    const fn percent() -> Self {
        Self {
            step: 1.0,
            step_quantum: 1.0,
            precision: Precision::Whole,
            suffix: "%",
            scale: 100.0,
            min: Some(0.0),
            max: Some(100.0),
        }
    }

    /// A count of things: whole numbers, and at least one.
    ///
    /// A count is not a length, so it does not take a decimal place: half a
    /// column is not a column.
    const fn count() -> Self {
        Self {
            step: 1.0,
            step_quantum: 1.0,
            precision: Precision::Whole,
            suffix: "",
            scale: 1.0,
            min: Some(1.0),
            max: None,
        }
    }

    /// The value as the user reads it.
    pub fn display(self, stored: f32) -> f32 {
        stored * self.scale
    }

    /// What the user typed, back into the unit the document stores.
    pub fn stored(self, typed: f32) -> f32 {
        typed / self.scale
    }

    /// How much one pixel of drag moves the value, in stored units.
    fn drag_step(self) -> f32 {
        self.step / self.scale
    }

    /// Keep a display value inside this property's own range.
    pub fn clamp_display(self, display: f32) -> f32 {
        let display = match self.min {
            Some(min) => display.max(min),
            None => display,
        };
        match self.max {
            Some(max) => display.min(max),
            None => display,
        }
    }

    /// Round a display value onto the quantum a drag may stop on.
    pub fn snap(self, display: f32) -> f32 {
        if self.step_quantum <= 0.0 {
            return display;
        }
        (display / self.step_quantum).round() * self.step_quantum
    }
}

/// The bare number, in the units the user reads, with no unit attached.
///
/// This is what a field being typed holds. The unit is decoration the field adds
/// when it is not being edited: a buffer that carried `%` would not parse, so
/// the arrows would stop working the moment a value was typed.
fn bare_number(display: f32, spec: NumberSpec) -> String {
    let decimals = spec.precision.decimals();
    let text = format!("{display:.decimals$}");
    // `12.0` is not a different number from `12`, and a panel that spells it two
    // ways makes the eye do the comparison instead of the model.
    text.strip_suffix(".0").unwrap_or(&text).to_owned()
}

/// The one way a number is written.
///
/// Geometry, radius, stroke width, opacity and font size all render through
/// here, so the panel cannot end up with two rounding rules for two kinds of
/// value — which is what it had, and why `0` meant different things in
/// different rows.
pub fn format_number(value: f32, spec: NumberSpec) -> String {
    let display = spec.clamp_display(spec.display(value));
    match spec.suffix.is_empty() {
        true => bare_number(display, spec),
        false => format!("{}{}", bare_number(display, spec), spec.suffix),
    }
}

/// The words a number field uses for the states its value can be in.
///
/// The panel's own numbers say "—" for a value nobody set and "Mixed" for a
/// disagreement. A layout number has the engine's words instead: a gap the
/// flow makes up is "Auto", and a padding nobody has set is "Not set".
///
/// One place for the words, so the layout section and the rest of the panel
/// cannot drift into two vocabularies for the same three states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NumberWords {
    /// The word for a value nobody has set.
    unset: &'static str,
    /// The word for a disagreement.
    mixed: &'static str,
    /// A word that stands in for the value entirely.
    ///
    /// `Some("Auto")` for a gap the layout makes up: the gap is not a number
    /// the user typed, so a number there would be a number nobody chose.
    automatic: Option<&'static str>,
}

impl NumberWords {
    /// The panel's own words.
    const fn panel() -> Self {
        Self {
            unset: NO_VALUE,
            mixed: MIXED,
            automatic: None,
        }
    }

    /// The engine's words, for the layout section.
    const fn layout() -> Self {
        Self {
            unset: NOT_SET,
            mixed: MIXED,
            automatic: None,
        }
    }

    /// The word this field shows for a value.
    fn shown(self, value: Value<f32>, spec: NumberSpec) -> String {
        match value {
            Value::Set(number) => match self.automatic {
                Some(word) => word.to_owned(),
                None => format_number(number, spec),
            },
            Value::Unset => self.unset.to_owned(),
            Value::Mixed => self.mixed.to_owned(),
        }
    }
}

/// A number being typed into a field.
///
/// A buffer, not a second copy of the property: the value becomes real only when
/// Enter hands it to the engine as one semantic operation, exactly as a layer
/// rename does. `select_all` is the whole of the convention and it is the
/// convention every product in the corpus uses — the value the user sees
/// arrives selected, so the first keystroke replaces it rather than appending
/// to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumberEdit {
    pub focus: Focus,
    buffer: String,
    select_all: bool,
}

/// What a keystroke did to an open number field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditEffect {
    /// The buffer changed; the field is still open.
    Continue,
    /// The field closed and this text should be committed, if it parses.
    Commit(String),
    /// The field closed and nothing should reach the document.
    Abandon,
}

impl NumberEdit {
    /// Open a field for typing, with its current text selected.
    pub fn editing(focus: Focus, current: String) -> Self {
        Self {
            focus,
            buffer: current,
            select_all: true,
        }
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Apply one keystroke to the open field.
    ///
    /// The whole grammar in one pure function, because it is a grammar: Enter
    /// commits, Escape abandons without touching the document, the first
    /// printable key replaces the selected text, and a modifier combination is
    /// an editor shortcut rather than a character.
    pub fn apply_key(&mut self, key: &str) -> EditEffect {
        match key {
            "enter" | "return" => {
                let buffer = std::mem::take(&mut self.buffer);
                self.select_all = false;
                EditEffect::Commit(buffer)
            }
            "escape" => {
                self.close();
                EditEffect::Abandon
            }
            "backspace" => {
                // With the text selected, Backspace deletes the selection — the
                // same rule typing follows. Treating it as "delete one
                // character" would leave the user staring at the rest of the old
                // number after the key they expected to clear it.
                if self.select_all {
                    self.buffer.clear();
                    self.select_all = false;
                } else {
                    self.buffer.pop();
                }
                EditEffect::Continue
            }
            // Arrows are a nudge, not a character. The caller reads the arrow
            // itself so the step it uses is the one the editor's ladder chose.
            "up" | "down" | "left" | "right" => EditEffect::Continue,
            other if other.chars().count() == 1 => {
                if self.select_all {
                    self.buffer.clear();
                    self.select_all = false;
                }
                self.buffer.push_str(other);
                EditEffect::Continue
            }
            _ => EditEffect::Continue,
        }
    }

    /// Move the typed value by whole steps while the field is open.
    ///
    /// Only when the buffer is a number. An arrow pressed in the middle of
    /// typing `-` or `1.` must not silently drop the digits the user entered,
    /// so a buffer that does not parse simply keeps the key.
    pub fn nudge(&mut self, delta: f32) -> bool {
        let Ok(current) = self.buffer.trim().parse::<f32>() else {
            return false;
        };
        // The buffer already holds what the user reads, so the step is added in
        // those units and no scale conversion happens here.
        let spec = self.focus.property.spec();
        self.buffer = bare_number(spec.clamp_display(current + delta), spec);
        self.select_all = false;
        true
    }

    fn close(&mut self) {
        self.buffer.clear();
        self.select_all = false;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Scrubbing
// ═══════════════════════════════════════════════════════════════════════════

/// A continuous edit in progress.
///
/// The value is a pure function of the value the gesture started at and how far
/// the pointer has moved. Nothing accumulates frame to frame, which is what
/// `transformation.md` asks for and what makes the gesture safe to re-derive on
/// any frame: a dropped pointer event cannot leave the field disagreeing with
/// the document, because the field is not remembering where it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scrub {
    property: Property,
    object: model::EngineObjectId,
    /// The box the layer had when the drag started. It remembers where the box
    /// was, which is the only way a drag can be re-derived from its first pixel.
    before: model::Geometry,
    start_x: f32,
    start_value: f32,
}

impl Scrub {
    /// Open a scrub against a gesture the engine has already started.
    ///
    /// The baseline is read from the gesture rather than from the widget, and
    /// the widget is not asked at all: by the time the pointer goes down it may
    /// already have moved into the field, so anything read here would be late.
    pub fn begin(
        property: Property,
        object: model::EngineObjectId,
        before: model::Geometry,
        start_x: f32,
    ) -> Self {
        Self {
            property,
            object,
            before,
            start_x,
            start_value: property.read_geometry(before),
        }
    }

    /// Where the field should read, given where the pointer is.
    pub fn value_at(&self, screen_x: f32, spec: NumberSpec) -> f32 {
        let dragged = self.start_value + (screen_x - self.start_x) * spec.drag_step();
        spec.snap(spec.clamp_display(spec.display(dragged)))
    }

    /// The box this scrub is describing, given where the pointer is.
    pub fn geometry_at(&self, screen_x: f32) -> model::Geometry {
        self.property
            .write(self.before, self.value_at(screen_x, self.property.spec()))
    }

    pub fn property(&self) -> Property {
        self.property
    }

    pub fn object(&self) -> model::EngineObjectId {
        self.object
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Properties
// ═══════════════════════════════════════════════════════════════════════════

/// One property the Inspector can show, named by what it means.
///
/// Not by where it is drawn and not by which section holds it. Adding rotation,
/// a corner-radius pair, a constraint or an alignment is a new variant here plus
/// a row that uses it: the read, the write, the spec and the element id all come
/// from this one declaration, and none of them reach into the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Property {
    X,
    Y,
    Width,
    Height,
    BorderRadius,
    Opacity,
    StrokeWidth,
    FontSize,
    /// The gap between the items of an auto layout.
    Gap,
    /// The gap between the wrapped rows or columns of an auto layout.
    CrossGap,
    /// The padding on the left and the right of an auto layout.
    PaddingHorizontal,
    /// The padding at the top and the bottom of an auto layout.
    PaddingVertical,
    /// The number of columns in a grid auto layout.
    GridColumns,
    /// The number of rows in a grid auto layout.
    GridRows,
}

impl Property {
    /// The short label that fits inside a boxed field.
    ///
    /// Rows that have room for a word pass their own label instead; this is the
    /// abbreviated form for the two-up geometry boxes, where `Width` would not
    /// fit beside a value.
    pub fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Width => "W",
            Self::Height => "H",
            Self::BorderRadius => "Radius",
            Self::Opacity => "Opacity",
            Self::StrokeWidth => "Width",
            Self::FontSize => "Size",
            Self::Gap => "Gap",
            Self::CrossGap => "Gap",
            // The panel is 264 points wide, and a padding field has to fit
            // beside its neighbour with the value in it: "Padding H" and "Not
            // set" together are wider than the row, and a clipped value is a
            // value the user cannot read.
            Self::PaddingHorizontal => "Pad H",
            Self::PaddingVertical => "Pad V",
            Self::GridColumns => "Columns",
            Self::GridRows => "Rows",
        }
    }

    /// A short stable name, used for element ids and for nothing else.
    pub fn slug(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Y => "y",
            Self::Width => "width",
            Self::Height => "height",
            Self::BorderRadius => "radius",
            Self::Opacity => "opacity",
            Self::StrokeWidth => "stroke-width",
            Self::FontSize => "font-size",
            Self::Gap => "gap",
            Self::CrossGap => "cross-gap",
            Self::PaddingHorizontal => "padding-h",
            Self::PaddingVertical => "padding-v",
            Self::GridColumns => "grid-columns",
            Self::GridRows => "grid-rows",
        }
    }

    /// The element id for this property's control.
    ///
    /// Keyed by name and by object, never by position in a section, so adding a
    /// property between two others cannot leave one field's element state behind
    /// on another's control.
    pub fn control_id(self, object: Option<model::EngineObjectId>) -> SharedString {
        SharedString::from(format!(
            "inspector-{}",
            match object {
                Some(object) => format!("{}-{}", object.0, self.slug()),
                None => format!("selection-{}", self.slug()),
            }
        ))
    }

    /// Whether this property lives in the object's box rather than in its paint.
    pub fn is_geometry(self) -> bool {
        matches!(self, Self::X | Self::Y | Self::Width | Self::Height)
    }

    /// The box property this is, when it is one.
    ///
    /// `None` for a property of the paint: those are written with a style edit
    /// rather than with a key of the box.
    pub fn box_property(self) -> Option<model::BoxProperty> {
        match self {
            Self::X => Some(model::BoxProperty::X),
            Self::Y => Some(model::BoxProperty::Y),
            Self::Width => Some(model::BoxProperty::Width),
            Self::Height => Some(model::BoxProperty::Height),
            Self::BorderRadius
            | Self::Opacity
            | Self::StrokeWidth
            | Self::FontSize
            | Self::Gap
            | Self::CrossGap
            | Self::PaddingHorizontal
            | Self::PaddingVertical
            | Self::GridColumns
            | Self::GridRows => None,
        }
    }

    /// Whether this property has a horizontal axis the arrow keys can move it
    /// along.
    pub fn is_horizontal(self) -> bool {
        matches!(self, Self::X | Self::Width)
    }

    /// Whether this property can be driven continuously.
    ///
    /// Only geometry can today, because only geometry has an operation that
    /// records a change when the user lets go rather than on every frame. A
    /// field without one gets typing and presets instead of a scrub that would
    /// record an operation per pointer movement — the widget is the same either
    /// way; only the affordance differs.
    pub fn is_continuous(self) -> bool {
        self.is_geometry()
    }

    /// How this property is written down for one object.
    ///
    /// `None` is a real answer, not missing data: it is how "nobody authored a
    /// font size" stays distinguishable from "the font size is zero".
    pub fn read(self, object: &model::DesignObject) -> Option<f32> {
        match self {
            Self::X => Some(object.position.x),
            Self::Y => Some(object.position.y),
            Self::Width => Some(object.size.width),
            Self::Height => Some(object.size.height),
            // `None` when the corners disagree: one radius field cannot show a
            // radius the user never chose.
            Self::BorderRadius => object.border_radius,
            Self::Opacity => Some(object.opacity),
            // A width with no stroke is not a width of zero. Keeping the two
            // apart is what lets the panel disable the field with a reason
            // instead of inviting the user to author a stroke that is not there.
            Self::StrokeWidth => object.stroke.map(|stroke| stroke.width),
            Self::FontSize => object.font_size,
            // The layout's numbers belong to the frame's auto layout. A frame
            // without one has no gap at all, which is not a gap of zero: the
            // engine's model says the layout is not there, and so does this.
            Self::Gap => object.auto_layout.map(|layout| layout.gap),
            Self::CrossGap => object.auto_layout.map(|layout| layout.cross_gap),
            // The two sides that have to agree for one number to stand for
            // them. Sides that disagree are a disagreement, not an absence.
            Self::PaddingHorizontal => object
                .auto_layout
                .and_then(|layout| agreeing(layout.padding[3], layout.padding[1])),
            Self::PaddingVertical => object
                .auto_layout
                .and_then(|layout| agreeing(layout.padding[0], layout.padding[2])),
            Self::GridColumns => object.auto_layout.map(|layout| layout.grid_columns as f32),
            Self::GridRows => object.auto_layout.map(|layout| layout.grid_rows as f32),
        }
    }

    /// How this property is spelled for one geometry.
    ///
    /// Every variant is named, including the ones that have no place in a box.
    /// A wildcard here used to answer `0.0` for anything it did not recognise,
    /// so a property added later would read zero rather than fail to compile —
    /// and a field showing `0` for a property it has never heard of looks like a
    /// real value. Naming them makes the compiler the thing that notices.
    fn read_geometry(self, geometry: model::Geometry) -> f32 {
        match self {
            Self::X => geometry.position.x,
            Self::Y => geometry.position.y,
            Self::Width => geometry.size.width,
            Self::Height => geometry.size.height,
            // Paint, typography and layout properties are not box numbers. The
            // panel only asks for geometry where `is_continuous` is true, which
            // is exactly the geometry set, so this arm is unreachable from the
            // panel today — and saying so explicitly is what keeps a new
            // property from inheriting a silent zero.
            Self::BorderRadius
            | Self::Opacity
            | Self::StrokeWidth
            | Self::FontSize
            | Self::Gap
            | Self::CrossGap
            | Self::PaddingHorizontal
            | Self::PaddingVertical
            | Self::GridColumns
            | Self::GridRows => 0.0,
        }
    }

    /// The same geometry with this property replaced.
    ///
    /// A size cannot go below one pixel however the value arrived: a zero-sized
    /// box cannot be clicked again, so shrinking one to nothing would make it
    /// unrecoverable rather than small. The spec stops the user being offered
    /// the value; this is the document's own guarantee that it cannot happen.
    pub fn write(self, geometry: model::Geometry, value: f32) -> model::Geometry {
        let mut next = geometry;
        match self {
            Self::X => next.position.x = value,
            Self::Y => next.position.y = value,
            Self::Width => next.size.width = floor_size(value),
            Self::Height => next.size.height = floor_size(value),
            // As in `read_geometry`: named rather than wildcard, so writing a
            // number into a property with no box to write it into has to be a
            // deliberate act instead of a silently ignored one.
            Self::BorderRadius
            | Self::Opacity
            | Self::StrokeWidth
            | Self::FontSize
            | Self::Gap
            | Self::CrossGap
            | Self::PaddingHorizontal
            | Self::PaddingVertical
            | Self::GridColumns
            | Self::GridRows => {}
        }
        next
    }

    /// The style edit this property commits as, if it is paint rather than
    /// geometry.
    pub fn style_edit(self, value: f32) -> Option<model::StyleEdit> {
        Some(match self {
            Self::BorderRadius => model::StyleEdit::BorderRadius(value),
            Self::Opacity => model::StyleEdit::Opacity(value),
            Self::StrokeWidth => model::StyleEdit::StrokeWidth(value),
            Self::FontSize => model::StyleEdit::FontSize(value),
            // A geometry property commits a box command instead, so it has
            // no style edit. `None` is the real answer rather than a fallthrough:
            // naming the four is what makes a ninth variant a compile error here
            // instead of a property that quietly never commits.
            Self::X | Self::Y | Self::Width | Self::Height => return None,
            // A layout property commits a layout edit instead, for the same
            // reason.
            Self::Gap
            | Self::CrossGap
            | Self::PaddingHorizontal
            | Self::PaddingVertical
            | Self::GridColumns
            | Self::GridRows => return None,
        })
    }

    /// The layout edit this property commits as, if it is part of an auto
    /// layout rather than of the box or of the paint.
    pub fn layout_edit(self, value: f32) -> Option<model::LayoutEdit> {
        Some(match self {
            Self::Gap => model::LayoutEdit::Gap(value),
            Self::CrossGap => model::LayoutEdit::CrossGap(value),
            Self::PaddingHorizontal => model::LayoutEdit::PaddingHorizontal(value),
            Self::PaddingVertical => model::LayoutEdit::PaddingVertical(value),
            Self::GridColumns => model::LayoutEdit::GridColumns(value),
            Self::GridRows => model::LayoutEdit::GridRows(value),
            // Everything else commits a box command or a style edit, so it has
            // no layout edit. `None` is the real answer rather than a
            // fallthrough: naming the rest is what makes a seventh layout
            // variant a compile error here instead of a property that quietly
            // never commits.
            Self::X
            | Self::Y
            | Self::Width
            | Self::Height
            | Self::BorderRadius
            | Self::Opacity
            | Self::StrokeWidth
            | Self::FontSize => return None,
        })
    }

    pub fn spec(self) -> NumberSpec {
        match self {
            // A position has no floor: a box may sit off the left edge.
            Self::X | Self::Y => NumberSpec::length(None),
            Self::Width | Self::Height => NumberSpec::length(Some(1.0)),
            Self::BorderRadius | Self::StrokeWidth => NumberSpec::length(Some(0.0)),
            // Zero is not a size anything can be, so a font size has a floor
            // even though nothing about the renderer requires one.
            Self::FontSize => NumberSpec::length(Some(1.0)),
            Self::Opacity => NumberSpec::percent(),
            // A gap or a padding is a distance and cannot be negative.
            Self::Gap | Self::CrossGap | Self::PaddingHorizontal | Self::PaddingVertical => {
                NumberSpec::length(Some(0.0))
            }
            // A count of tracks is a whole number of things.
            Self::GridColumns | Self::GridRows => NumberSpec::count(),
        }
    }
}

/// The one number two sides agree on, when they agree.
///
/// The two sides of a padding that disagree are a disagreement, not an absence:
/// the padding is set, and the panel simply cannot show one number for it.
fn agreeing(first: f32, second: f32) -> Option<f32> {
    (first == second).then_some(first)
}

fn floor_size(value: f32) -> f32 {
    value.max(1.0)
}

/// Which property the keyboard is pointed at, and at what.
///
/// `object` names the box a geometry edit applies to. A paint edit applies to
/// whatever is selected and so carries no object at all rather than a convenient
/// one: a field that remembered an object it did not mean would be a bug waiting
/// for a selection change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Focus {
    pub object: Option<model::EngineObjectId>,
    pub property: Property,
}

/// Read a property across a whole selection.
///
/// The single place a number becomes a three-state value, so a property cannot
/// be shown with one selection rule in one row and another in the next.
fn selected_value(selected: &[model::DesignObject], property: Property) -> Value<f32> {
    Value::across(selected.iter().map(|object| property.read(object)))
}

// ═══════════════════════════════════════════════════════════════════════════
// Paint
// ═══════════════════════════════════════════════════════════════════════════

/// Which paint a control owns. Every one of them has an off state, a colour,
/// and a place in the appearance or typography section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleProperty {
    Fill,
    Stroke,
    TextColor,
}

impl StyleProperty {
    pub fn slug(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Stroke => "stroke",
            Self::TextColor => "text-color",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fill => "Fill",
            Self::Stroke => "Stroke",
            Self::TextColor => "Text colour",
        }
    }

    /// The edit that applies, or removes, this paint.
    pub fn edit(self, color: Option<model::Color>) -> model::StyleEdit {
        match self {
            Self::Fill => model::StyleEdit::Fill(color),
            Self::Stroke => model::StyleEdit::Stroke(color),
            Self::TextColor => model::StyleEdit::TextColor(color),
        }
    }
}

/// A paint property as the selection presents it.
///
/// Two independent three-states, because "is this filled" and "what colour is
/// it" disagree on different selections and for different reasons. A row that
/// derived the colour's state from whether the paint was on would call a
/// half-filled selection uniform, which is the mistake the three-state model
/// exists to prevent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paint {
    /// Whether the paint is applied at all.
    pub present: Value<bool>,
    /// The colour it is applied with.
    pub color: Value<model::Color>,
    /// The first colour the paint *is* applied with, even when the selection
    /// disagrees.
    ///
    /// Offered when switching the paint on for a selection that has none:
    /// "fill these with something" needs a colour, and this is the only one the
    /// selection actually has.
    pub representative: Option<model::Color>,
}

impl Paint {
    /// Read one paint across a selection.
    pub fn read(
        selected: &[model::DesignObject],
        read: impl Fn(&model::DesignObject) -> Option<model::Color>,
    ) -> Self {
        let color = Value::across(selected.iter().map(&read));
        Self {
            present: Value::across(selected.iter().map(|object| Some(read(object).is_some()))),
            color,
            representative: selected.iter().find_map(&read),
        }
    }

    /// What the colour trigger reads.
    pub fn label(&self) -> String {
        match self.color {
            Value::Set(color) => color_hex(color),
            Value::Unset => UNSET.to_owned(),
            Value::Mixed => MIXED.to_owned(),
        }
    }

    /// The colour to put on the swatch: the agreed one, else a representative,
    /// else the panel's own surface so an absent paint looks absent rather than
    /// looking like black.
    pub fn swatch(&self, fallback: u32) -> u32 {
        match self.color {
            Value::Set(color) => color.to_rgb(),
            _ => self.representative.map_or(fallback, model::Color::to_rgb),
        }
    }
}

pub fn color_hex(color: model::Color) -> String {
    color.to_hex()
}

// ═══════════════════════════════════════════════════════════════════════════
// Auto layout
// ═══════════════════════════════════════════════════════════════════════════

/// The index of the cell nothing is on.
///
/// The picker offers three cells on each axis, and a cell can be off because
/// the selection disagrees about where the items sit rather than because the
/// items sit at the end.
const NOTHING: i32 = -1;

/// The index that lights a whole axis.
///
/// An auto gap spreads the items along the whole main axis, so every cell on
/// that axis is on; wrapped lines spread across the flow light the whole cross
/// axis the same way.
const SPREAD: i32 = 9;

/// Why a layout row cannot be used on a layer that cannot hold a layout.
const NO_FRAME: &str = "Auto layout needs a frame";

/// Why a layout row cannot be used on a frame that has no auto layout.
const NO_AUTO_LAYOUT: &str = "This layer has no auto layout";

/// Why a layout row cannot be used on a selection that disagrees about it.
const MIXED_AUTO_LAYOUT: &str = "Mixed selection has no agreed auto layout";

/// What the selection's auto layout says.
///
/// One fold per property, so a disagreement is a disagreement about *that*
/// property: two frames that flow the same way but space their children
/// differently agree about the direction and disagree about the gap.
///
/// The three states come from the engine's model rather than from a flag the
/// panel keeps: a frame with no auto layout has no gap, no padding and no
/// alignment at all, which is not the same as a gap of zero.
#[derive(Clone, Copy, Debug)]
struct LayoutState {
    /// Whether the selected layers have an auto layout.
    present: Value<bool>,
    mode: Value<model::LayoutMode>,
    wrap: Value<bool>,
    gap: Value<f32>,
    cross_gap: Value<f32>,
    padding_horizontal: Value<f32>,
    padding_vertical: Value<f32>,
    main_align: Value<model::MainAlign>,
    cross_align: Value<model::CrossAlign>,
    content_align: Value<model::ContentAlign>,
    grid_columns: Value<f32>,
    grid_rows: Value<f32>,
}

impl LayoutState {
    /// Read the auto layout of a selection.
    fn read(selected: &[model::DesignObject]) -> Self {
        let present = Value::across(
            selected
                .iter()
                .map(|object| Some(object.auto_layout.is_some())),
        );
        let numbers = |read: fn(&model::AutoLayout) -> f32| {
            Value::across(
                selected
                    .iter()
                    .map(|object| object.auto_layout.as_ref().map(read)),
            )
        };
        let padding = |read: fn(&model::AutoLayout) -> (f32, f32)| {
            Value::across(selected.iter().map(|object| {
                object.auto_layout.as_ref().and_then(|layout| {
                    let (first, second) = read(layout);
                    agreeing(first, second)
                })
            }))
        };
        Self {
            present,
            mode: Value::across(
                selected
                    .iter()
                    .map(|object| object.auto_layout.as_ref().map(|layout| layout.mode)),
            ),
            wrap: Value::across(
                selected
                    .iter()
                    .map(|object| object.auto_layout.as_ref().map(|layout| layout.wrap)),
            ),
            gap: numbers(|layout| layout.gap),
            cross_gap: numbers(|layout| layout.cross_gap),
            // The sides that disagree are a disagreement, not an absence: the
            // padding is there, and the panel cannot show one number for it.
            padding_horizontal: disagreeing(
                padding(|layout| (layout.padding[3], layout.padding[1])),
                present,
            ),
            padding_vertical: disagreeing(
                padding(|layout| (layout.padding[0], layout.padding[2])),
                present,
            ),
            main_align: Value::across(
                selected
                    .iter()
                    .map(|object| object.auto_layout.as_ref().map(|layout| layout.main_align)),
            ),
            cross_align: Value::across(
                selected
                    .iter()
                    .map(|object| object.auto_layout.as_ref().map(|layout| layout.cross_align)),
            ),
            content_align: Value::across(selected.iter().map(|object| {
                object
                    .auto_layout
                    .as_ref()
                    .map(|layout| layout.content_align)
            })),
            grid_columns: numbers(|layout| layout.grid_columns as f32),
            grid_rows: numbers(|layout| layout.grid_rows as f32),
        }
    }

    /// Whether the flow wraps.
    ///
    /// A grid has no wrap: its tracks are its lines.
    fn wraps(&self) -> bool {
        self.wrap == Value::Set(true) && !self.is_grid()
    }

    /// Whether the flow is a grid.
    fn is_grid(&self) -> bool {
        self.mode == Value::Set(model::LayoutMode::Grid)
    }

    /// Whether the items are spread along the main axis by the free space.
    fn auto_gap(&self) -> bool {
        matches!(self.main_align, Value::Set(align) if align.is_auto_gap())
    }

    /// Whether the wrapped lines are spread across the flow.
    fn spread_lines(&self) -> bool {
        !self.is_grid()
            && self.wrap == Value::Set(true)
            && self.content_align == Value::Set(model::ContentAlign::SpaceBetween)
    }
}

/// A padding whose sides disagree is a disagreement, not an absence.
///
/// Only when there is an auto layout to disagree about: a frame without one
/// has no padding at all, and that is a different answer.
fn disagreeing(value: Value<f32>, present: Value<bool>) -> Value<f32> {
    match present {
        Value::Set(true) => match value {
            Value::Unset => Value::Mixed,
            other => other,
        },
        _ => value,
    }
}

/// Whether the layout rows can be used, and why not.
///
/// The engine's own rules: an auto layout needs a frame, and a gap belongs to
/// a frame that has one. A frame without one is offered the button that adds
/// it, rather than a control that would add one as a side effect of a number.
fn layout_availability(state: &LayoutState, frames: bool) -> Availability {
    if !frames {
        return Availability::Disabled(NO_FRAME);
    }
    match state.present {
        Value::Set(true) => Availability::Supported,
        // Agreement that there is no auto layout, and no reading at all, are
        // the same answer here: there is no layout to put a gap in.
        Value::Set(false) | Value::Unset => Availability::Disabled(NO_AUTO_LAYOUT),
        Value::Mixed => Availability::Disabled(MIXED_AUTO_LAYOUT),
    }
}

/// Which layout menu is open, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayoutMenu {
    /// How the gap between the items is made.
    GapMode,
    /// How the gap between the wrapped lines is made.
    CrossGapMode,
}

/// One cell of the alignment picker.
///
/// Where it sits in the grid, whether it is on, and which way the flow runs —
/// the three things that decide what the cell means and what it draws.
#[derive(Clone, Copy, Debug)]
struct AlignmentCell {
    /// The main axis's cell: 0 is the start, 1 the middle, 2 the end.
    main: i32,
    /// The cross axis's cell, the same way.
    cross: i32,
    /// Whether the selection agrees this is where the items sit.
    on: bool,
    /// Whether the flow runs across rather than down.
    horizontal: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
// The layer name
// ═══════════════════════════════════════════════════════════════════════════

/// A layer name being edited.
///
/// A buffer, not a second name store: the name only becomes real when Enter
/// hands it to the engine as one semantic operation. Escape and an empty name
/// abandon it, leaving the document untouched.
struct RenameSession {
    id: model::EngineObjectId,
    buffer: String,
    select_all: bool,
}

/// What a key did to an open rename.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RenameEffect {
    /// The buffer changed; the rename is still open.
    Continue,
    /// The rename closed and this name should be committed, if it is not empty.
    Commit(String),
    /// The rename closed and nothing should reach the document.
    Abandon,
}

impl RenameSession {
    /// Open a rename.
    ///
    /// An empty buffer is the right one when the user clicked the name: the
    /// caret is already where the text goes, and `select_all` still means the
    /// first keystroke replaces rather than appends.
    fn opening(id: model::EngineObjectId) -> Self {
        Self {
            id,
            buffer: String::new(),
            select_all: true,
        }
    }

    /// Open a rename on a name that already exists.
    fn replacing(id: model::EngineObjectId, buffer: String) -> Self {
        Self {
            id,
            buffer,
            select_all: true,
        }
    }

    /// Apply one keystroke to the open rename.
    fn apply_key(&mut self, key: &str) -> RenameEffect {
        match key {
            "enter" | "return" => {
                let name = self.buffer.clone();
                self.close();
                RenameEffect::Commit(name)
            }
            "escape" => {
                self.close();
                RenameEffect::Abandon
            }
            "backspace" => {
                if self.select_all {
                    self.buffer.clear();
                    self.select_all = false;
                } else {
                    self.buffer.pop();
                }
                RenameEffect::Continue
            }
            // A modifier combination is an editor shortcut, not a character. It
            // is swallowed so it cannot leak into the name.
            other if other.chars().count() == 1 => {
                if self.select_all {
                    self.buffer.clear();
                    self.select_all = false;
                }
                self.buffer.push_str(other);
                RenameEffect::Continue
            }
            _ => RenameEffect::Continue,
        }
    }

    fn close(&mut self) {
        self.buffer.clear();
        self.select_all = false;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The Inspector
// ═══════════════════════════════════════════════════════════════════════════

/// The tabs across the top of the Inspector.
///
/// Labels only; the chrome is [`segmented`], which is also what a paint's
/// on/off uses, so the two cannot drift into two looks for one gesture.
const TABS: [(&str, bool); 4] = [
    ("Design", true),
    ("Prototype", true),
    ("Dev Mode", true),
    ("AI", true),
];

/// Swatches offered by every colour control.
///
/// A fixed palette rather than a colour wheel, because a wheel is a second
/// editing model and a picker the document model cannot round-trip is worse
/// than a small set of values it can definitely express.
const PALETTE: [(&str, u32); 9] = [
    ("Ink", theme::INK),
    ("Paper", theme::PAPER),
    ("Gray", 0x777d84),
    ("Red", 0xc45d5d),
    ("Orange", 0xd2844d),
    ("Yellow", 0xd5b957),
    ("Green", theme::SAGE),
    ("Blue", 0x6689c7),
    ("Purple", 0x9576b8),
];

/// The Inspector.
///
/// Owns interaction state — which section is collapsed, which field the keyboard
/// is pointed at, what is being typed, what is being dragged — and owns no
/// document state at all. Every edit leaves through [`Inspector::commit_number`]
/// or [`Inspector::commit_paint`], which hand it to the engine.
pub struct Inspector {
    /// The engine canvas: the document this panel reads and writes.
    ///
    /// `None` until the shell attaches it: the shell builds the Inspector
    /// before it builds the engine canvas, so the two are joined afterwards by
    /// [`Inspector::set_engine_canvas`].
    engine: Option<Entity<EngineCanvas>>,
    /// The effects section's own state. It is an ENTITY, because a field being
    /// typed into needs a focus handle and a keyboard.
    effects: Option<Entity<EffectsPanel>>,
    /// The components section's state: which of its menus is open.
    ///
    /// The section is a function, not an entity, so the state lives here and
    /// the section is handed a handle to it.
    components_state: Rc<RefCell<ComponentsPanelState>>,
    /// The export section's state: which row is being edited, and what is being
    /// typed into it.
    export_state: Rc<RefCell<ExportPanelState>>,
    /// The styles section's state: which slot's browser is open, what is being
    /// typed into it, and which folders are folded away.
    styles_state: Rc<RefCell<StylesPanelState>>,
    /// The assets section's state: which image is being edited and what is
    /// being typed into it.
    assets_state: Rc<RefCell<AssetsPanelState>>,
    /// The Dev Mode tab's own state: which menu is open, what is being typed
    /// into it, and which annotations the canvas shows.
    devmode_state: Rc<RefCell<DevPanelState>>,
    /// The Accessibility section's state: what is being typed into the name.
    a11y_state: Rc<RefCell<A11yPanelState>>,
    /// The Prototype tab's own state: which menu is open, what is being typed
    /// into it, and which interaction's editor is open.
    prototyping_state: Rc<RefCell<ProtoPanelState>>,
    /// The variables section's state: which collection and group the table
    /// shows, what is being typed into it, and which menu is open.
    variables_state: Rc<RefCell<VariablesPanelState>>,
    /// The typography section's state: which menu is open and what is being
    /// typed into it.
    typography_state: Rc<RefCell<TypographyPanelState>>,
    /// The colour picker's state: the colour being edited and the draft.
    color_state: Rc<RefCell<ColorPickerState>>,
    tab: usize,
    sections: Sections,
    /// The property the arrow keys move, once a field has been touched.
    ///
    /// Runtime state, and it says so: a field that remembers where the pointer
    /// was would be a second copy of the document.
    focus: Option<Focus>,
    /// The field being typed into, if one is open.
    edit: Option<NumberEdit>,
    /// The drag in progress, if one is open.
    scrub: Option<Scrub>,
    /// The layer name being typed, if one is open.
    rename: Option<RenameSession>,
    /// Which colour control has its palette open, if any.
    palette: Option<StyleProperty>,
    /// Which layout menu is open, if any.
    layout_menu: Option<LayoutMenu>,
    /// Repaints that must outlive the constructor.
    ///
    /// The Inspector is a view of the document: an engine edit has to repaint it
    /// or a field shows a value the engine has already moved past. Held, not
    /// dropped, because a GPUI subscription detaches its observer when it goes.
    #[allow(dead_code)]
    observations: Vec<gpui::Subscription>,
}

impl Inspector {
    /// The Inspector, before the engine canvas is attached.
    ///
    /// The engine's canvas is not read yet: the panel reads the engine's
    /// document and writes through the engine's commands, and the shell builds
    /// the Inspector before it builds the engine canvas, so the two are joined
    /// afterwards by [`Inspector::set_engine_canvas`].
    pub fn new() -> Self {
        Self {
            engine: None,
            effects: None,
            components_state: Rc::new(RefCell::new(ComponentsPanelState::default())),
            export_state: Rc::new(RefCell::new(ExportPanelState::default())),
            styles_state: Rc::new(RefCell::new(StylesPanelState::default())),
            variables_state: Rc::new(RefCell::new(VariablesPanelState::default())),
            prototyping_state: Rc::new(RefCell::new(ProtoPanelState::new())),
            devmode_state: Rc::new(RefCell::new(DevPanelState::new())),
            a11y_state: Rc::new(RefCell::new(A11yPanelState::default())),
            assets_state: Rc::new(RefCell::new(AssetsPanelState::default())),
            typography_state: Rc::new(RefCell::new(TypographyPanelState::default())),
            color_state: Rc::new(RefCell::new(ColorPickerState::default())),
            tab: 0,
            sections: Sections::new(),
            focus: None,
            edit: None,
            scrub: None,
            rename: None,
            palette: None,
            layout_menu: None,
            observations: Vec::new(),
        }
    }

    /// Attach the engine canvas the Inspector reads and writes through.
    ///
    /// The engine canvas owns the session both panels share, so attaching it is
    /// what makes this panel a view of the engine's document rather than of
    /// anything else. The Inspector observes it, so an edit made on the canvas
    /// repaints the panel and the panel shows what is selected.
    ///
    /// The shell owns the wiring and calls this once it has built the engine
    /// canvas, which it builds after the Inspector.
    #[allow(dead_code)]
    pub fn set_engine_canvas(&mut self, engine: Entity<EngineCanvas>, cx: &mut Context<Self>) {
        let observation = cx.observe(&engine, |_, _, cx| cx.notify());
        self.observations.push(observation);
        // The effects section is an ENTITY, because a field being typed into
        // needs a focus handle and the keyboard. It is built here, once, from the
        // same engine canvas this panel reads.
        self.effects = Some(cx.new(|cx| EffectsPanel::new(engine.clone(), cx)));
        self.engine = Some(engine);
        cx.notify();
    }

    /// Open a rename on a layer another surface asked about.
    ///
    /// The panel states the request and the surface that owns the editing model
    /// opens it, so a name typed from either side is the same buffer, the same
    /// commit, and the same Escape behaviour. The existing name arrives
    /// selected, because a rename that appends to the old name is a different
    /// command from renaming.
    ///
    /// The layer is named by the engine's identity. There is no other id space
    /// to name it by, and a request in a form the engine cannot answer is not
    /// a rename of some other layer.
    pub fn open_rename(&mut self, id: model::EngineObjectId, cx: &mut Context<Self>) {
        let Some(name) = self.name_of(id, cx) else {
            return;
        };
        self.rename = Some(RenameSession::replacing(id, name));
    }

    /// Which layer the engine is renaming, if any.
    #[allow(dead_code)]
    pub fn renaming_node(&self) -> Option<model::EngineObjectId> {
        self.rename.as_ref().map(|session| session.id)
    }

    /// Whether the Inspector is part-way through something Escape could give up.
    ///
    /// Read-only, and kept deliberately in step with
    /// [`Self::cancel_in_flight`]: the editor's Escape ladder reads this to
    /// decide which rung is on top, and only then asks the Inspector to act on
    /// it. Two separate questions, so the ladder can be decided from state
    /// rather than from whatever happened to be cancelled first.
    pub fn is_busy(&self) -> bool {
        self.edit.is_some()
            || self.rename.is_some()
            || self.scrub.is_some()
            // The Export section's own fields: a size, a suffix or a bleed
            // being typed. It is a text buffer like any other, and the editor's
            // Escape ladder has to know it is open.
            || self.export_state.borrow().is_busy()
            || self.styles_state.borrow().is_busy()
            || self.typography_state.borrow().is_busy()
            || self.color_state.borrow().is_busy()
            || self.assets_state.borrow().is_busy()
            || self.devmode_state.borrow().is_busy()
            || self.a11y_state.borrow().is_busy()
    }

    /// Give up whatever the Inspector is part-way through.
    ///
    /// A text buffer before a gesture, in the order the editor's Escape ladder
    /// implies: what the user typed into is above what they are dragging.
    /// Returns whether anything was given up, so the ladder knows the key was
    /// consumed.
    pub fn cancel_in_flight(&mut self, cx: &mut Context<Self>) -> bool {
        // The two are mutually exclusive — opening either closes the other — so
        // short-circuiting cannot leave one open.
        if self.edit.take().is_some() || self.rename.take().is_some() {
            cx.notify();
            return true;
        }
        if let Some(scrub) = self.scrub.take() {
            // Abandoning restores the engine's document and records nothing:
            // the same promise the engine keeps for a canvas drag.
            let _ = scrub;
            self.cancel_gesture(cx);
            cx.notify();
            return true;
        }
        // The Export section's draft. The read and the write are SEPARATE
        // statements: a RefCell borrow alive across the `if` body would panic.
        let export_cancelled = self.export_state.borrow_mut().cancel();
        if export_cancelled {
            cx.notify();
            return true;
        }
        // The styles, typography and colour sections' own drafts. The read and
        // the write are SEPARATE statements: a RefCell borrow alive across the
        // body would panic.
        let styles_cancelled = self.styles_state.borrow_mut().cancel();
        if styles_cancelled {
            cx.notify();
            return true;
        }
        let typography_cancelled = self.typography_state.borrow_mut().cancel();
        if typography_cancelled {
            cx.notify();
            return true;
        }
        let colour_cancelled = self.color_state.borrow_mut().cancel();
        if colour_cancelled {
            cx.notify();
            return true;
        }
        let assets_cancelled = self.assets_state.borrow_mut().cancel();
        if assets_cancelled {
            cx.notify();
            return true;
        }
        let devmode_cancelled = self.devmode_state.borrow_mut().cancel();
        if devmode_cancelled {
            cx.notify();
            return true;
        }
        let a11y_cancelled = self.a11y_state.borrow_mut().cancel();
        if a11y_cancelled {
            cx.notify();
            return true;
        }
        false
    }

    /// Keys the Inspector claims before the editor's own shortcuts.
    ///
    /// A text buffer is not an editor shortcut: typing `f` into a name must not
    /// also switch to the frame tool, and typing `12` into a field must not also
    /// select the rectangle tool. So the Inspector gets first refusal on every
    /// key and declines the ones it has nothing open for.
    ///
    /// Returns whether the key was consumed. Escape is never claimed here: it
    /// belongs to the editor's ladder, which walks down from the topmost thing,
    /// and a text buffer that answered it first would make the rung it belongs
    /// to depend on which listener saw the key first.
    pub fn handle_key(&mut self, key: &str, shift: bool, cx: &mut Context<Self>) -> bool {
        if key == "escape" {
            return false;
        }
        // The Export section's own text fields: a size, a suffix or a bleed
        // being typed. The section owns the buffer; the Inspector routes the key
        // to it and runs the change it asks for.
        if self.export_state.borrow().is_busy() {
            let action = self.export_state.borrow_mut().handle_key(key);
            if let Some(action) = action {
                self.run_export_action(action, cx);
            }
            cx.notify();
            return true;
        }
        if self.styles_state.borrow().is_busy() {
            let action = self.styles_state.borrow_mut().handle_key(key);
            if let Some(action) = action {
                self.run_styles_action(action, cx);
            }
            cx.notify();
            return true;
        }
        if self.typography_state.borrow().is_busy() {
            let action = self.typography_state.borrow_mut().handle_key(key);
            if let Some(action) = action {
                self.run_typography_action(action, cx);
            }
            cx.notify();
            return true;
        }
        if self.devmode_state.borrow().is_busy() {
            let action = self.devmode_state.borrow_mut().handle_key(key);
            if let Some(action) = action {
                self.run_dev_action(action, cx);
            }
            cx.notify();
            return true;
        }
        if self.a11y_state.borrow().is_busy() {
            let action = self.a11y_state.borrow_mut().handle_key(key);
            if let Some(action) = action {
                self.run_a11y_action(action, cx);
            }
            cx.notify();
            return true;
        }
        if self.assets_state.borrow().is_busy() {
            let action = self.assets_state.borrow_mut().handle_key(key);
            if let Some(action) = action {
                self.run_assets_action(action, cx);
            }
            cx.notify();
            return true;
        }
        if self.color_state.borrow().is_busy() {
            let action = self.color_state.borrow_mut().handle_key(key);
            if let Some(action) = action {
                self.run_color_action(action, cx);
            }
            cx.notify();
            return true;
        }
        if self.rename.is_some() {
            return self.rename_key(key, cx);
        }
        if self.edit.is_some() {
            return self.edit_key(key, shift, cx);
        }
        self.arrow_key(key, shift, cx)
    }

    /// One keystroke into the open number field.
    fn edit_key(&mut self, key: &str, shift: bool, cx: &mut Context<Self>) -> bool {
        // Arrows nudge a typed value instead of being characters, and `⇧` makes
        // the nudge ten steps the way it does everywhere else in the editor.
        let arrow = match key {
            "up" => Some(-1.0),
            "down" => Some(1.0),
            "left" => Some(-1.0),
            "right" => Some(1.0),
            _ => None,
        };
        if let Some(sign) = arrow {
            let step = if shift { 10.0 } else { 1.0 };
            if let Some(edit) = self.edit.as_mut() {
                edit.nudge(sign * step);
            }
            cx.notify();
            return true;
        }
        let Some(edit) = self.edit.as_mut() else {
            return false;
        };
        let focus = edit.focus;
        match edit.apply_key(key) {
            EditEffect::Continue => cx.notify(),
            EditEffect::Commit(text) => {
                self.edit = None;
                self.commit_text(focus, &text, cx);
            }
            EditEffect::Abandon => cx.notify(),
        }
        true
    }

    /// One keystroke into the open layer name.
    fn rename_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(session) = self.rename.as_mut() else {
            return false;
        };
        let id = session.id;
        let effect = session.apply_key(key);
        if effect == RenameEffect::Continue {
            cx.notify();
            return true;
        }
        self.rename = None;
        if let RenameEffect::Commit(name) = effect {
            let name = name.trim().to_owned();
            if !name.is_empty() {
                let (command, params) = model::rename_command(id, &name);
                self.run(&command, params, cx);
            }
        }
        cx.notify();
        true
    }

    /// Arrow keys moving the field the user last touched.
    ///
    /// One key press is one semantic operation, the same as one field drag. The
    /// field owns only its own axis: `↑` on the X field is a request for
    /// vertical movement, and the editor's own nudge is better placed to answer
    /// it than a horizontal field is.
    fn arrow_key(&mut self, key: &str, shift: bool, cx: &mut Context<Self>) -> bool {
        let Some(focus) = self.focus else {
            return false;
        };
        let step = if shift { 10.0 } else { 1.0 };
        let (vertical, delta) = match key {
            "left" => (false, -step),
            "right" => (false, step),
            "up" => (true, -step),
            "down" => (true, step),
            _ => return false,
        };
        if vertical == focus.property.is_horizontal() {
            return false;
        }
        let Some(object) = focus.object else {
            return false;
        };
        let spec = focus.property.spec();
        let Some(current) = self.geometry_of(object, cx) else {
            return false;
        };
        let display = spec.display(focus.property.read_geometry(current)) + delta;
        self.commit_number(focus, display, cx);
        true
    }

    // ── The engine, read ─────────────────────────────────────────────────────

    /// The layers the engine has selected, with the properties the panel shows.
    fn selected(&self, cx: &mut Context<Self>) -> Vec<model::DesignObject> {
        self.selected_objects(cx)
    }

    /// The layers the engine has selected, with the properties the panel shows.
    ///
    /// The panel shows what the engine has selected, so this is the one place
    /// that answer comes from: the engine's session, read live. The canvas
    /// draws its selection overlay from the same session, so the two cannot
    /// be about different layers.
    pub fn selected_objects(&self, app: &gpui::App) -> Vec<model::DesignObject> {
        let Some(engine) = self.engine.as_ref() else {
            return Vec::new();
        };
        engine.read(app).with_session(|session| {
            let Ok(doc) = session.doc() else {
                return Vec::new();
            };
            model::selected_objects(doc, &session.selection())
        })
    }

    /// The layers the engine has selected, by identity.
    fn selection(&self, cx: &mut Context<Self>) -> Vec<model::EngineObjectId> {
        let Some(engine) = self.engine.as_ref() else {
            return Vec::new();
        };
        engine.read(cx).selection()
    }

    /// The words the layout section shows, in the order it shows them.
    ///
    /// For the window-level tests: what the user would read for the layout
    /// rows — a number, "Mixed", "Not set" or "Auto" — so a test can say what
    /// the panel says without reading pixels.
    #[cfg(test)]
    pub fn test_layout_words(&self, cx: &mut Context<Self>) -> Vec<String> {
        let state = LayoutState::read(&self.selected(cx));
        let gap = NumberWords {
            automatic: state.auto_gap().then_some(AUTO),
            ..NumberWords::layout()
        };
        vec![
            gap.shown(state.gap, Property::Gap.spec()),
            NumberWords::layout()
                .shown(state.padding_horizontal, Property::PaddingHorizontal.spec()),
            NumberWords::layout().shown(state.padding_vertical, Property::PaddingVertical.spec()),
        ]
    }

    /// The box of one layer, in the frame the layer is in.
    fn geometry_of(
        &self,
        id: model::EngineObjectId,
        cx: &mut Context<Self>,
    ) -> Option<model::Geometry> {
        let engine = self.engine.as_ref()?;
        engine.read(cx).with_session(|session| {
            let doc = session.doc().ok()?;
            model::geometry(doc, id)
        })
    }

    /// The name one layer has in the engine's document.
    fn name_of(&self, id: model::EngineObjectId, cx: &mut Context<Self>) -> Option<String> {
        let engine = self.engine.as_ref()?;
        engine.read(cx).with_session(|session| {
            let doc = session.doc().ok()?;
            doc.node(id).map(|node| node.name.clone())
        })
    }

    // ── The engine, written ──────────────────────────────────────────────────

    /// Run one engine command.
    ///
    /// The engine's commands are the operation surface: the panel, the toolbar,
    /// the keymap, the command palette, the CLI and a future agent all dispatch
    /// command ids, so an edit made by a click and an edit made by a shortcut
    /// go through the same code and produce the same history.
    fn run(&mut self, id: &str, params: JsonValue, cx: &mut Context<Self>) -> bool {
        let Some(engine) = self.engine.clone() else {
            return false;
        };
        engine.update(cx, |canvas, cx| canvas.run_engine_command(id, params, cx))
    }

    /// The effects section: the selected layer's effects, in the engine's
    /// vocabulary.
    ///
    /// It is an entity, so the section's own state -- which row is open, which
    /// field is being typed into -- lives with it and keeps its focus.
    fn effects_section(&self, _cx: &mut Context<Self>) -> AnyElement {
        match self.effects.clone() {
            Some(panel) => panel.into_any_element(),
            None => empty_note("No effects yet.").into_any_element(),
        }
    }

    /// The export section: the selected layer's export settings.
    fn export_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(engine) = self.engine.clone() else {
            return empty_note("Add export setting").into_any_element();
        };
        let model: ExportModel = engine.read(cx).with_session(|session| {
            let Ok(doc) = session.doc() else {
                return ExportModel::default();
            };
            panel_export::selected_exports(doc, &session.selection())
        });
        let state = self.export_state.clone();
        panel_export::section(&model, &state, cx).into_any_element()
    }

    /// The components section: what the selected layer is, in the components
    /// vocabulary.
    ///
    /// The model is read from the engine's document, the same one the rest of
    /// the panel reads, so the two cannot be about different layers.
    fn components_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(engine) = self.engine.clone() else {
            return div().into_any_element();
        };
        let model = engine.read(cx).with_session(|session| {
            let Ok(doc) = session.doc() else {
                return ComponentsModel::empty();
            };
            ComponentsModel::read(doc, &session.selection())
        });
        let state = self.components_state.clone();
        panel_components::components_section(&model, &state, cx)
    }

    /// The styles section: the selected layers' style links, and the browser
    /// of the styles a slot offers.
    fn styles_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(engine) = self.engine.clone() else {
            return empty_note("No styles yet.").into_any_element();
        };
        let model: StylesModel = engine.read(cx).with_session(|session| {
            let Ok(doc) = session.doc() else {
                return StylesModel::default();
            };
            StylesModel::read(doc, &session.selection())
        });
        let state = self.styles_state.clone();
        panel_styles::styles_section(&model, &state, cx).into_any_element()
    }

    /// The assets section: the document's images, and the selection's own image
    /// fill.
    fn assets_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(engine) = self.engine.clone() else {
            return empty_note("No images yet.").into_any_element();
        };
        let model: AssetsModel = engine.read(cx).with_session(|session| {
            let Ok(doc) = session.doc() else {
                return AssetsModel::default();
            };
            AssetsModel::read(doc, &session.selection())
        });
        let state = self.assets_state.clone();
        panel_assets::assets_section(&model, &state, cx).into_any_element()
    }

    /// Open one of the Inspector's tabs.
    ///
    /// The Prototype tab is a MODE as well as a panel: the canvas draws the
    /// prototype's connections while it is open, so the canvas is told.
    pub fn open_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.tab = index;
        if let Some(engine) = self.engine.clone() {
            engine.update(cx, |canvas, cx| {
                canvas.set_prototype_mode(index == 1, cx);
                canvas.set_dev_mode(index == panel_devmode::TAB_DEV_MODE, cx);
            });
        }
        cx.notify();
    }

    /// Dev Mode on and off.
    ///
    /// figcraft's `view.devMode` switches the right-hand tab between Inspect
    /// and Design; this is the same answer.
    pub fn toggle_dev_mode(&mut self, cx: &mut Context<Self>) {
        let next = panel_devmode::dev_mode_tab(self.tab);
        self.open_tab(next, cx);
    }

    /// The Dev Mode tab: the layer's handoff status, its annotations, its dev
    /// resources, its inspected properties and its assets -- or, with nothing
    /// selected, what is ready for dev.
    fn dev_mode_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(engine) = self.engine.clone() else {
            return empty_note("No Dev Mode yet.").into_any_element();
        };
        let state = self.devmode_state.clone();
        let color_format = state.borrow().color_format;
        let model: DevModeModel = engine.read(cx).with_session(|session| {
            let Ok(doc) = session.doc() else {
                return DevModeModel::default();
            };
            let Ok(page) = session.page() else {
                return DevModeModel::default();
            };
            let focused = session.ds().ok().and_then(|ds| ds.focused());
            DevModeModel::read(doc, page, &session.selection(), focused, color_format)
        });
        panel_devmode::dev_mode_panel(&model, &state, cx).into_any_element()
    }

    /// The Accessibility section: what a screen reader reads of the selection.
    fn a11y_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(engine) = self.engine.clone() else {
            return empty_note("Select an object on the canvas or in Layers.").into_any_element();
        };
        let model: A11yModel = engine.read(cx).with_session(|session| {
            let Ok(doc) = session.doc() else {
                return A11yModel::default();
            };
            A11yModel::read(doc, &session.selection())
        });
        let state = self.a11y_state.clone();
        panel_a11y::a11y_section(&model, &state, cx).into_any_element()
    }

    /// The Prototype tab: the page's flows, the selected layers' interactions
    /// and the prototype's settings.
    fn prototype_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(engine) = self.engine.clone() else {
            return empty_note("No prototype yet.").into_any_element();
        };
        let model: ProtoModel = engine.read(cx).with_session(|session| {
            let Ok(doc) = session.doc() else {
                return ProtoModel::default();
            };
            let Ok(page) = session.page() else {
                return ProtoModel::default();
            };
            ProtoModel::read(doc, page, &session.selection())
        });
        let state = self.prototyping_state.clone();
        panel_prototyping::proto_panel(&model, &state, cx).into_any_element()
    }

    /// The variables section: the document's variables, and the selected
    /// layers' own bindings and modes.
    fn variables_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(engine) = self.engine.clone() else {
            return empty_note("No variables yet.").into_any_element();
        };
        let model: VariablesModel = engine.read(cx).with_session(|session| {
            let Ok(doc) = session.doc() else {
                return VariablesModel::default();
            };
            VariablesModel::read(doc, &session.selection())
        });
        let state = self.variables_state.clone();
        panel_variables::variables_section(&model, &state, cx).into_any_element()
    }

    /// The typography section: the selection's text layers and their type.
    fn typography_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(engine) = self.engine.clone() else {
            return empty_note("This layer has no text").into_any_element();
        };
        // The model reads the EDITED RANGE: a selected range's styles are what
        // the panel shows, not the whole layer's.
        let model: TypographyModel = engine.read(cx).with_session(TypographyModel::from_session);
        if !model.has_text {
            return empty_note("This layer has no text").into_any_element();
        }
        let state = self.typography_state.clone();
        panel_typography::section(&model, &state, cx).into_any_element()
    }

    /// Run a Dev Mode change through the engine.
    fn run_dev_action(&mut self, action: DevAction, cx: &mut Context<Self>) {
        let (command, params) = action.command();
        self.run(&command, params, cx);
    }

    /// Run an accessibility change through the engine.
    fn run_a11y_action(&mut self, action: A11yAction, cx: &mut Context<Self>) {
        let (command, params) = action.command();
        self.run(&command, params, cx);
    }

    /// Run a styles change through the engine.
    fn run_styles_action(&mut self, action: StylesAction, cx: &mut Context<Self>) {
        let (command, params) = action.command();
        self.run(&command, params, cx);
    }

    /// Run a typography change through the engine.
    fn run_typography_action(&mut self, action: TypographyAction, cx: &mut Context<Self>) {
        let (command, params) = action.command();
        self.run(&command, params, cx);
    }

    /// Run a colour change through the engine.
    fn run_color_action(&mut self, action: ColorAction, cx: &mut Context<Self>) {
        let (command, params) = action.command();
        self.run(&command, params, cx);
    }

    /// Run a component change through the engine.
    ///
    /// The engine's commands are the operation surface, so a change made from
    /// the section is the same change the CLI or a shortcut would make, and it
    /// lands in the same history.
    fn run_component_action(&mut self, action: ComponentAction, cx: &mut Context<Self>) {
        let (command, params) = action.command();
        self.run(&command, params, cx);
    }

    /// Run an export change through the engine.
    ///
    /// The engine's commands are the operation surface, so a change made from
    /// the section is the same change the CLI or a shortcut would make, and it
    /// lands in the same history.
    fn run_export_action(&mut self, action: ExportAction, cx: &mut Context<Self>) {
        let (command, params) = action.command();
        self.run(&command, params, cx);
    }

    /// Write one property of one layer's box.
    fn commit_box(
        &mut self,
        property: model::BoxProperty,
        value: f32,
        object: model::EngineObjectId,
        cx: &mut Context<Self>,
    ) {
        let (command, params) = model::box_command(property, value, object);
        self.run(&command, params, cx);
    }

    /// Open a gesture: the engine holds one undo step open for it.
    ///
    /// The engine's own tools do this for a drag on the canvas, and the history
    /// reads the same either way: one drag, one step.
    fn begin_gesture(&mut self, label: &str, cx: &mut Context<Self>) -> bool {
        let Some(engine) = self.engine.clone() else {
            return false;
        };
        engine.update(cx, |canvas, cx| {
            canvas.with_session_mut(cx, |session| session.begin_interaction(label).is_ok())
        })
    }

    /// Close a gesture as one undo step.
    fn commit_gesture(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        engine.update(cx, |canvas, cx| {
            canvas.with_session_mut(cx, |session| session.commit_interaction().is_ok())
        });
    }

    /// Abandon a gesture, restoring the document and recording nothing.
    fn cancel_gesture(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        engine.update(cx, |canvas, cx| {
            canvas.with_session_mut(cx, |session| session.cancel_interaction().is_ok())
        });
    }

    // ── The one way a number reaches the document ──────────────────────────

    /// Commit a number the user read in display units.
    ///
    /// Every numeric control ends here, so "one edit is one semantic operation"
    /// is a property of this function rather than something each row has to
    /// remember. Geometry is per-object because a box has one position; paint is
    /// a property of whatever is selected, which is why the same field can be
    /// typed for one object and for forty.
    fn commit_number(&mut self, focus: Focus, display: f32, cx: &mut Context<Self>) {
        let spec = focus.property.spec();
        let stored = spec.stored(spec.clamp_display(display));
        if let Some(property) = focus.property.box_property() {
            let Some(object) = focus.object else {
                return;
            };
            let Some(before) = self.geometry_of(object, cx) else {
                return;
            };
            let next = focus.property.write(before, stored);
            let value = focus.property.read_geometry(next);
            self.commit_box(property, value, object, cx);
        } else if let Some(edit) = focus.property.style_edit(stored) {
            self.commit_style(edit, cx);
        } else if let Some(edit) = focus.property.layout_edit(stored) {
            self.commit_layout(edit, cx);
        }
    }

    /// Commit typed text, if it is a number this property can hold.
    ///
    /// Text that does not parse is not an error the user needs told about; it is
    /// a keystroke that produced something that is not a value, and the field
    /// simply closes.
    fn commit_text(&mut self, focus: Focus, text: &str, cx: &mut Context<Self>) {
        let Ok(typed) = text.trim().parse::<f32>() else {
            return;
        };
        let spec = focus.property.spec();
        if !typed.is_finite() {
            return;
        }
        self.commit_number(focus, spec.clamp_display(typed), cx);
    }

    /// Commit a style change, which is always one operation across the
    /// selection.
    ///
    /// The one exit for paint, so a preset chip, a colour trigger and a
    /// segmented choice all produce the same single history entry rather than
    /// each deciding for itself what "one edit" means.
    fn commit_style(&mut self, edit: model::StyleEdit, cx: &mut Context<Self>) {
        let ids = self.selection(cx);
        let (command, params) = model::style_command(&edit, &ids);
        self.run(&command, params, cx);
    }

    /// Commit a change to the selection's auto layout.
    ///
    /// The one exit for the layout rows, so a direction, a gap and a padding
    /// all produce the same single history entry rather than each deciding for
    /// itself what "one edit" means.
    ///
    /// An auto layout belongs to the frame rather than to one of its children,
    /// so this applies to the whole selection: "make these all flow this way"
    /// is one operation.
    fn commit_layout(&mut self, edit: model::LayoutEdit, cx: &mut Context<Self>) {
        let ids = self.selection(cx);
        let (command, params) = model::layout_command(&edit, &ids);
        self.run(&command, params, cx);
    }

    /// Commit a paint, which is a style change with the colour already decided.
    fn commit_paint(
        &mut self,
        property: StyleProperty,
        color: Option<model::Color>,
        cx: &mut Context<Self>,
    ) {
        self.commit_style(property.edit(color), cx);
    }

    // ── Scrubbing ───────────────────────────────────────────────────────────

    fn begin_scrub(&mut self, focus: Focus, screen_x: f32, cx: &mut Context<Self>) {
        // Pressing the pointer anywhere ends whatever was open in this panel: a
        // previous drag is finished rather than abandoned, because the pointer
        // went down somewhere else, and a rename loses to a drag the same way.
        self.end_scrub(cx);
        self.rename = None;
        let Some(object) = focus.object else {
            return;
        };
        let Some(property) = focus.property.box_property() else {
            return;
        };
        let Some(before) = self.geometry_of(object, cx) else {
            return;
        };
        // The engine holds the gesture open, so the whole drag is one undo step
        // when it ends and none at all if it is abandoned.
        if !self.begin_gesture(model::gesture_label(property), cx) {
            return;
        }
        self.focus = Some(focus);
        self.scrub = Some(Scrub::begin(focus.property, object, before, screen_x));
        cx.notify();
    }

    fn move_scrub(&mut self, screen_x: f32, cx: &mut Context<Self>) {
        let Some(scrub) = self.scrub else {
            return;
        };
        let Some(property) = scrub.property().box_property() else {
            return;
        };
        // Recomputed from where the gesture started, never accumulated, so the
        // field and the document cannot drift apart over a long drag.
        let next = scrub.geometry_at(screen_x);
        let value = scrub.property().read_geometry(next);
        self.commit_box(property, value, scrub.object(), cx);
    }

    fn end_scrub(&mut self, cx: &mut Context<Self>) {
        if self.scrub.take().is_none() {
            return;
        }
        // One semantic operation for the whole drag, or none at all if it landed
        // where it started — the engine decides which, exactly as it does for a
        // drag that began on the canvas itself.
        self.commit_gesture(cx);
    }

    // ── Opening a field ─────────────────────────────────────────────────────

    /// Open a field for typing, with its current text selected.
    fn open_edit(&mut self, focus: Focus, cx: &mut Context<Self>) {
        // Closing the others first is what makes the interaction states
        // mutually exclusive: a rename, a typed number and a drag are three
        // different things to be doing, and the Escape ladder above them is
        // simpler when at most one of them can be true.
        self.cancel_in_flight(cx);
        let selected = self.selected(cx);
        let text = match selected_value(&selected, focus.property) {
            Value::Set(value) => format_number(value, focus.property.spec()),
            // `Mixed` and `Unset` both open empty. A field the user cannot read
            // has nothing to select, and the first keystroke supplies it.
            _ => String::new(),
        };
        self.focus = Some(focus);
        self.edit = Some(NumberEdit::editing(focus, text));
        cx.notify();
    }

    /// Drop interaction state the current selection has invalidated.
    ///
    /// A field can be open when the selection changes underneath it — clicking
    /// the canvas replaces the selection — and a field left editing an object
    /// that is no longer selected would commit a number into the wrong place, or
    /// into nothing at all.
    fn reconcile(&mut self, selected: &[model::DesignObject]) {
        let gone = |object: Option<model::EngineObjectId>| match object {
            Some(object) => !selected.iter().any(|other| other.id == object),
            // A field with no object is a paint field, and paint applies to
            // whatever is selected — so a changed selection never invalidates it.
            None => false,
        };
        if self
            .edit
            .as_ref()
            .is_some_and(|edit| gone(edit.focus.object))
        {
            self.edit = None;
        }
        if self.focus.is_some_and(|focus| gone(focus.object)) {
            self.focus = None;
        }
    }

    /// Whether a control can be used, and why not.
    ///
    /// The asymmetry is the interesting part. Paint can be set across a mixed
    /// selection because "make these all this colour" is one operation, so a
    /// mixed colour is still editable. Geometry cannot, because there is no one
    /// position to show and therefore no one position to set.
    fn availability(&self, property: Property, value: &Value<f32>, count: usize) -> Availability {
        if count > 1 && property.is_geometry() {
            return Availability::Disabled(MIXED_GEOMETRY);
        }
        if property == Property::StrokeWidth && value.is_unset() {
            return Availability::Disabled(NO_STROKE);
        }
        Availability::Supported
    }
}

impl AssetsHost for Inspector {
    /// Run an assets change through the engine.
    fn run_assets_action(&mut self, action: AssetsAction, cx: &mut Context<Self>) {
        let (command, params) = action.command();
        self.run(&command, params, cx);
    }
}

impl DevHost for Inspector {
    /// Run a Dev Mode change through the engine.
    fn run_dev_action(&mut self, action: DevAction, cx: &mut Context<Self>) {
        Inspector::run_dev_action(self, action, cx);
    }

    /// The canvas draws the callouts the filter selects, so the canvas is told.
    fn set_dev_category_filter(&mut self, filter: CategoryFilter, cx: &mut Context<Self>) {
        if let Some(engine) = self.engine.clone() {
            engine.update(cx, |canvas, cx| canvas.set_dev_category_filter(filter, cx));
        }
    }
}

impl A11yHost for Inspector {
    /// Run an accessibility change through the engine.
    fn run_a11y_action(&mut self, action: A11yAction, cx: &mut Context<Self>) {
        Inspector::run_a11y_action(self, action, cx);
    }
}

impl ProtoHost for Inspector {
    /// Run a prototype change through the engine.
    fn run_proto_action(&mut self, action: ProtoAction, cx: &mut Context<Self>) {
        let (command, params) = action.command();
        self.run(&command, params, cx);
    }
}

impl StylesHost for Inspector {
    /// Run a styles change through the engine.
    fn run_styles_action(&mut self, action: StylesAction, cx: &mut Context<Self>) {
        Inspector::run_styles_action(self, action, cx);
    }
}

impl VariablesHost for Inspector {
    /// Run a variables change through the engine.
    fn run_variables_action(&mut self, action: VariablesAction, cx: &mut Context<Self>) {
        let (command, params) = action.command();
        self.run(&command, params, cx);
    }
}

impl TypographyHost for Inspector {
    /// Run a typography change through the engine.
    fn run_typography_action(&mut self, action: TypographyAction, cx: &mut Context<Self>) {
        Inspector::run_typography_action(self, action, cx);
    }
}

impl ColorHost for Inspector {
    /// Run a colour change through the engine.
    fn run_color_action(&mut self, action: ColorAction, cx: &mut Context<Self>) {
        Inspector::run_color_action(self, action, cx);
    }

    /// Open a gesture: the engine holds ONE undo step open for it.
    ///
    /// A drag on the colour square is one operation, not one per pointer
    /// movement, and the engine's own canvas drags work the same way -- so the
    /// history reads the same either way.
    fn begin_color_gesture(&mut self, label: &str, cx: &mut Context<Self>) -> bool {
        self.begin_gesture(label, cx)
    }

    /// Close a gesture as one undo step.
    fn commit_color_gesture(&mut self, cx: &mut Context<Self>) {
        self.commit_gesture(cx);
    }
}

impl ExportHost for Inspector {
    /// Run an export change through the engine.
    fn run_export_action(&mut self, action: ExportAction, cx: &mut Context<Self>) {
        Inspector::run_export_action(self, action, cx);
    }
}

impl ComponentsHost for Inspector {
    /// Run a component change through the engine.
    fn run_component_action(&mut self, action: ComponentAction, cx: &mut Context<Self>) {
        Inspector::run_component_action(self, action, cx);
    }
}

impl Render for Inspector {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::diagnostics::count("inspector_build", 1);
        let start = crate::diagnostics::start();
        let selected = self.selected(cx);
        crate::diagnostics::record("inspector_projection", start);
        crate::diagnostics::count("inspector_objects_copied", selected.len() as u64);
        self.reconcile(&selected);

        let body = match self.tab {
            1 => self.prototype_body(cx),
            2 => self.dev_mode_body(cx),
            3 => ai_body(),
            _ => self.design_body(&selected, cx),
        };

        div()
            .flex()
            .flex_col()
            .w(px(264.0))
            .flex_shrink_0()
            .bg(rgb(theme::SURFACE))
            .border_l_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .child(self.tabs(cx))
            .child(
                div()
                    .id("inspector-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}

impl Inspector {
    // ── Chrome ──────────────────────────────────────────────────────────────

    /// The tab strip, drawn by the same segmented control as a paint's on/off.
    fn tabs(&self, cx: &mut Context<Self>) -> Div {
        segmented(
            "inspector-tab",
            &TABS,
            Some(self.tab),
            None,
            true,
            |index, this: &mut Inspector, cx: &mut Context<Inspector>| {
                this.open_tab(index, cx);
                cx.notify();
            },
            cx,
        )
    }

    /// A collapsible section, its header always drawn and its body only when
    /// open.
    ///
    /// The slot decides *which* section collapses and the title says what it is
    /// called. Deriving the name from the slot instead is how a section ends up
    /// labelled "Effects" while showing font sizes.
    fn inspector_section(
        &self,
        title: &'static str,
        slot: SectionSlot,
        body: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> Div {
        self.inspector_section_with(title, slot, None, body, cx)
    }

    /// A collapsible section with a button in its header.
    ///
    /// The same section, with one control beside the collapse chevron: the
    /// layout section's "add auto layout" is a header button because it is
    /// about the section as a whole rather than about one row of it.
    fn inspector_section_with(
        &self,
        title: &'static str,
        slot: SectionSlot,
        action: Option<AnyElement>,
        body: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> Div {
        let collapsed = self.sections.is_collapsed(slot);
        let mut section = column()
            .border_b_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .child(
                div()
                    .id(SharedString::from(format!("section-{}", slot.index())))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(14.0))
                    .py(px(12.0))
                    .hover(|style| style.bg(rgb(theme::SURFACE_RAISED)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sections.toggle(slot);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(theme::TEXT))
                            .child(title),
                    )
                    .child({
                        let mut right = div().flex().items_center().gap_2();
                        if let Some(action) = action {
                            right = right.child(action);
                        }
                        right.child(
                            div()
                                .text_sm()
                                .text_color(rgb(theme::TEXT_MUTED))
                                .child(if collapsed { "＋" } else { "−" }),
                        )
                    }),
            );
        if !collapsed {
            section = section.child(body);
        }
        section
    }

    // ── Composition ─────────────────────────────────────────────────────────

    fn design_body(&self, selected: &[model::DesignObject], cx: &mut Context<Self>) -> AnyElement {
        match selected {
            [] => section_body()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(theme::TEXT))
                        .child("No selection"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child("Select an object on the canvas or in Layers."),
                )
                .child(self.variables_section(cx))
                .into_any_element(),
            [object] => {
                let selection = std::slice::from_ref(object);
                // Typography appears only when the object has text, and the
                // content travels with it. Figma groups them for the same
                // reason: a font size with nothing to set is a control that
                // cannot mean anything.
                let typography = match object.text_content.as_deref() {
                    Some(text) => {
                        let text = text.to_owned();
                        column()
                            .child(text_block(&text))
                            .child(self.typography_fields(selection, cx))
                            .child(self.typography_section(cx))
                    }
                    None => section_body().child(empty_note("This layer has no text")),
                };
                column()
                    .child(self.layer_header(object, cx))
                    .child(self.position_section(selection, cx))
                    .child(self.layout_section(selection, cx))
                    .child(self.inspector_section(
                        "Appearance",
                        SectionSlot::APPEARANCE,
                        self.appearance_fields(selection, cx),
                        cx,
                    ))
                    .child(self.inspector_section(
                        "Typography",
                        SectionSlot::TYPOGRAPHY,
                        typography,
                        cx,
                    ))
                    .child(self.inspector_section(
                        "Effects",
                        SectionSlot::EFFECTS,
                        self.effects_section(cx),
                        cx,
                    ))
                    .child(self.inspector_section(
                        "Export",
                        SectionSlot::EXPORT,
                        self.export_section(cx),
                        cx,
                    ))
                    .child(self.inspector_section(
                        "Styles",
                        SectionSlot::STYLES,
                        self.styles_section(cx),
                        cx,
                    ))
                    .child(self.inspector_section(
                        "Assets",
                        SectionSlot::ASSETS,
                        self.assets_section(cx),
                        cx,
                    ))
                    .child(self.inspector_section(
                        "Accessibility",
                        SectionSlot::A11Y,
                        self.a11y_section(cx),
                        cx,
                    ))
                    .child(self.inspector_section(
                        "Variables",
                        SectionSlot::VARIABLES,
                        self.variables_section(cx),
                        cx,
                    ))
                    .child(self.components_section(cx))
                    .into_any_element()
            }
            _ => {
                let mut sections = column()
                    .child(multi_header(selected.len()))
                    .child(self.position_section(selected, cx))
                    // The layout section is here too: a selection of several
                    // frames has one layout each, and a disagreement about it
                    // is a disagreement the panel has to say out loud.
                    .child(self.layout_section(selected, cx))
                    .child(self.inspector_section(
                        "Appearance",
                        SectionSlot::APPEARANCE,
                        self.appearance_fields(selected, cx),
                        cx,
                    ));
                // Typography is offered when any selected object has text: a
                // mixed selection of shapes and words still has words in it, and
                // setting one colour applies to all of them at once.
                if selected.iter().any(|object| object.text_content.is_some()) {
                    sections = sections.child(
                        self.inspector_section(
                            "Typography",
                            SectionSlot::TYPOGRAPHY,
                            column()
                                .child(self.typography_fields(selected, cx))
                                .child(self.typography_section(cx)),
                            cx,
                        ),
                    );
                }
                // The Effects section is here for the MULTI-selection, and that
                // is the whole point of the per-property fold: this is where
                // "Mixed" is reachable. A single selection can never disagree
                // with itself.
                sections = sections.child(self.inspector_section(
                    "Effects",
                    SectionSlot::EFFECTS,
                    self.effects_section(cx),
                    cx,
                ));
                // The Accessibility section is here for the MULTI-selection too:
                // that is where "Mixed" is reachable.
                sections = sections.child(self.inspector_section(
                    "Accessibility",
                    SectionSlot::A11Y,
                    self.a11y_section(cx),
                    cx,
                ));
                sections.into_any_element()
            }
        }
    }

    fn layer_header(&self, object: &model::DesignObject, cx: &mut Context<Self>) -> Div {
        column().child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px(px(14.0))
                .py(px(13.0))
                .border_b_1()
                .border_color(rgb(theme::BORDER_SOFT))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(theme::TEXT_MUTED))
                                .child("▱"),
                        )
                        .child(self.object_name(object, cx)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(object.kind.clone()),
                ),
        )
    }

    /// The object's name, or the rename buffer while one is open.
    ///
    /// Clicking the name opens a rename; the typed text is a buffer in this
    /// view and becomes a real name only when Enter hands it to the engine as
    /// one semantic operation. Escape and an empty name abandon it, leaving the
    /// document untouched.
    fn object_name(&self, object: &model::DesignObject, cx: &mut Context<Self>) -> Stateful<Div> {
        let renaming = self
            .rename
            .as_ref()
            .filter(|session| session.id == object.id)
            .map(|session| session.buffer.clone());
        let name = div()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(if renaming.is_some() {
                theme::ACCENT
            } else {
                theme::TEXT
            }))
            .child(match &renaming {
                Some(buffer) => format!("{buffer}|"),
                None => object.name.clone(),
            });
        let id = object.id;
        let already_open = renaming.is_some();
        let name = if already_open {
            name.cursor_text()
        } else {
            name.cursor_pointer()
        };
        name.id(SharedString::from(format!("rename-{}", object.id.0)))
            // A test looks the name up by the engine's node id, so a test can
            // say which layer the panel is showing. A no-op in a release build.
            .debug_selector(|| format!("rename-{}", object.id.0))
            .on_click(cx.listener(move |this, _, _, cx| {
                if already_open {
                    return;
                }
                this.rename = Some(RenameSession::opening(id));
                cx.notify();
            }))
    }

    // ── Sections ────────────────────────────────────────────────────────────

    fn position_section(&self, selected: &[model::DesignObject], cx: &mut Context<Self>) -> Div {
        let object = selected.first().map(|object| object.id);
        let count = selected.len();
        let body = section_body()
            .child(field_pair(
                self.panel_number_field(
                    Property::X,
                    object,
                    selected_value(selected, Property::X),
                    count,
                    cx,
                ),
                self.panel_number_field(
                    Property::Y,
                    object,
                    selected_value(selected, Property::Y),
                    count,
                    cx,
                ),
            ))
            .child(field_pair(
                self.panel_number_field(
                    Property::Width,
                    object,
                    selected_value(selected, Property::Width),
                    count,
                    cx,
                ),
                self.panel_number_field(
                    Property::Height,
                    object,
                    selected_value(selected, Property::Height),
                    count,
                    cx,
                ),
            ));
        self.inspector_section("Position and size", SectionSlot::POSITION, body, cx)
    }

    /// The layout section: the auto layout of what is selected.
    ///
    /// The engine's own model, read through the model and written through the
    /// engine's commands — the same contract the engine's own design panel
    /// uses. Every row is one of the engine's layout properties, and every one
    /// of them says which of the three states it is in.
    fn layout_section(&self, selected: &[model::DesignObject], cx: &mut Context<Self>) -> Div {
        let state = LayoutState::read(selected);
        let frames = !selected.is_empty() && selected.iter().all(|object| object.frame_like);
        let availability = layout_availability(&state, frames);
        // The engine's own name for the section: "Auto layout" once there is
        // one, "Layout" before that.
        let title = match state.present {
            Value::Set(true) => "Auto layout",
            _ => "Layout",
        };
        let body = self.layout_body(&state, availability, cx);
        let action = self.layout_action(&state, frames, cx);
        self.inspector_section_with(title, SectionSlot::LAYOUT, action, body, cx)
    }

    /// The header's button: add or remove the auto layout.
    ///
    /// The engine's own commands, because they are the ones that know what
    /// adding a layout means: the gap and the padding are read off the
    /// children rather than guessed at.
    fn layout_action(
        &self,
        state: &LayoutState,
        frames: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let ids = self.selection(cx);
        if ids.is_empty() {
            return None;
        }
        let (command, label, selector) = match state.present {
            // Adding a layout to a frame that already has one would wrap it in
            // a new frame instead, so the button is only offered when there is
            // none to add.
            Value::Set(false) if frames => {
                ("layout.add", "＋ Add auto layout", "inspector-layout-add")
            }
            Value::Set(true) | Value::Mixed => (
                "layout.remove",
                "− Remove auto layout",
                "inspector-layout-remove",
            ),
            _ => return None,
        };
        let params = serde_json::json!({
            "ids": ids.iter().map(|id| id.0).collect::<Vec<_>>(),
        });
        Some(
            div()
                .id(SharedString::from(selector))
                .debug_selector(|| selector.to_owned())
                .px(px(6.0))
                .py(px(3.0))
                .rounded_sm()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    // The header is a click target too: without this the click
                    // would also collapse the section it belongs to.
                    cx.stop_propagation();
                    this.run(command, params.clone(), cx);
                }))
                .into_any_element(),
        )
    }

    /// The layout rows: the direction, the alignment, and the spacing.
    fn layout_body(
        &self,
        state: &LayoutState,
        availability: Availability,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut body = section_body();
        // The reason the rows cannot be used, said out loud. A dimmed row with
        // no explanation is a row the user has to guess about.
        if let Some(reason) = availability.reason() {
            body = body.child(disabled_note(reason));
        }
        body = body.child(self.layout_direction(state, availability, cx));
        body = body.child(self.layout_alignment(state, availability, cx));
        body = body.child(self.layout_spacing(state, availability, cx));
        body
    }

    /// The direction of the flow, and whether it wraps.
    ///
    /// The engine's three directions, in the engine's own order: vertical,
    /// horizontal and grid.
    fn layout_direction(
        &self,
        state: &LayoutState,
        availability: Availability,
        cx: &mut Context<Self>,
    ) -> Div {
        let enabled = availability.is_editable();
        let selected = match state.mode {
            Value::Set(model::LayoutMode::Vertical) => Some(0),
            Value::Set(model::LayoutMode::Horizontal) => Some(1),
            Value::Set(model::LayoutMode::Grid) => Some(2),
            _ => None,
        };
        let word = if state.mode.is_mixed() {
            Some(MIXED)
        } else {
            None
        };
        let direction = segmented(
            "inspector-layout-direction",
            &[
                ("Vertical", enabled),
                ("Horizontal", enabled),
                ("Grid", enabled),
            ],
            selected,
            word,
            false,
            |index, this: &mut Inspector, cx: &mut Context<Inspector>| {
                let mode = match index {
                    0 => model::LayoutMode::Vertical,
                    1 => model::LayoutMode::Horizontal,
                    _ => model::LayoutMode::Grid,
                };
                this.commit_layout(model::LayoutEdit::Mode(mode), cx);
            },
            cx,
        );
        // The direction and the wrap are two things, so they are two rows: the
        // direction's three words are as wide as the panel allows, and the wrap
        // would run off the edge beside them.
        let mut rows = column().gap_2();
        rows = rows.child(div().flex().items_center().gap_2().child(direction));
        // Wrap belongs to the flow: a horizontal one wraps into rows, a
        // vertical one into columns. A grid has no wrap to offer.
        if !state.is_grid() {
            rows = rows.child(self.layout_wrap(state, enabled, cx));
        }
        rows
    }

    /// The wrap toggle.
    fn layout_wrap(&self, state: &LayoutState, enabled: bool, cx: &mut Context<Self>) -> Div {
        let on = state.wrap == Value::Set(true);
        let word = if state.wrap.is_mixed() {
            Some(MIXED)
        } else {
            None
        };
        segmented(
            "inspector-layout-wrap",
            &[("Wrap", enabled)],
            if on { Some(0) } else { None },
            word,
            false,
            move |_, this: &mut Inspector, cx: &mut Context<Inspector>| {
                this.commit_layout(model::LayoutEdit::Wrap(!on), cx);
            },
            cx,
        )
    }

    /// The alignment of the items, and the gap between them.
    fn layout_alignment(
        &self,
        state: &LayoutState,
        availability: Availability,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut body = column().gap_2();
        body = body.child(
            div()
                .flex()
                .gap_2()
                .child(self.alignment_picker(state, availability, cx))
                .child(self.gap_column(state, availability, cx)),
        );
        // Text baselines: a horizontal stack only, because a vertical one
        // stacks the text on top of itself.
        if matches!(state.mode, Value::Set(model::LayoutMode::Horizontal)) {
            body = body.child(self.layout_baselines(state, availability, cx));
        }
        body
    }

    /// The text baselines toggle.
    fn layout_baselines(
        &self,
        state: &LayoutState,
        availability: Availability,
        cx: &mut Context<Self>,
    ) -> Div {
        let enabled = availability.is_editable();
        let on = state.cross_align == Value::Set(model::CrossAlign::Baseline);
        let word = if state.cross_align.is_mixed() {
            Some(MIXED)
        } else {
            None
        };
        segmented(
            "inspector-layout-baselines",
            &[("Align text baselines", enabled)],
            if on { Some(0) } else { None },
            word,
            false,
            move |_, this: &mut Inspector, cx: &mut Context<Inspector>| {
                let align = if on {
                    model::CrossAlign::Min
                } else {
                    model::CrossAlign::Baseline
                };
                this.commit_layout(model::LayoutEdit::CrossAlign(align), cx);
            },
            cx,
        )
    }

    /// The alignment picker: the main axis across, the cross axis down.
    ///
    /// Nine cells, each a main alignment and a cross alignment together. An
    /// auto gap keeps its mode — the free space is still spread out — so the
    /// cells only pick the cross alignment there.
    fn alignment_picker(
        &self,
        state: &LayoutState,
        availability: Availability,
        cx: &mut Context<Self>,
    ) -> Div {
        let enabled = availability.is_editable();
        // A vertical flow runs down, so its main axis is the column.
        let horizontal = !matches!(state.mode, Value::Set(model::LayoutMode::Vertical));
        let main = match state.main_align {
            Value::Set(align) if align.is_auto_gap() => SPREAD,
            Value::Set(model::MainAlign::Min) => 0,
            Value::Set(model::MainAlign::Center) => 1,
            Value::Set(model::MainAlign::Max) => 2,
            _ => NOTHING,
        };
        // Wrapped lines spread across the flow light the whole cross axis, the
        // way an auto gap lights the whole main one.
        let cross = if state.spread_lines() {
            SPREAD
        } else {
            match state.cross_align {
                Value::Set(model::CrossAlign::Min) | Value::Set(model::CrossAlign::Baseline) => 0,
                Value::Set(model::CrossAlign::Center) => 1,
                Value::Set(model::CrossAlign::Max) => 2,
                _ => NOTHING,
            }
        };
        let mut picker = column().gap_1();
        for row in 0..3 {
            let mut line = div().flex().gap_1();
            for column in 0..3 {
                let (main_i, cross_i) = if horizontal {
                    (column, row)
                } else {
                    (row, column)
                };
                let on =
                    (main == main_i || main == SPREAD) && (cross == cross_i || cross == SPREAD);
                line = line.child(self.alignment_cell(
                    state,
                    AlignmentCell {
                        main: main_i,
                        cross: cross_i,
                        on,
                        horizontal,
                    },
                    enabled,
                    cx,
                ));
            }
            picker = picker.child(line);
        }
        picker
    }

    /// One cell of the alignment picker.
    fn alignment_cell(
        &self,
        state: &LayoutState,
        cell: AlignmentCell,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let grid = state.is_grid();
        let current = match state.main_align {
            Value::Set(align) => Some(align),
            _ => None,
        };
        // An auto gap keeps its mode: the cells pick the cross alignment.
        let keeps = !grid && current.is_some_and(|align| align.is_auto_gap());
        let selector = format!("inspector-layout-align-{}-{}", cell.main, cell.cross);
        let mark = if cell.on {
            div()
                .w(px(if cell.horizontal { 4.0 } else { 12.0 }))
                .h(px(if cell.horizontal { 12.0 } else { 4.0 }))
                .rounded_sm()
                .bg(rgb(theme::ACCENT))
        } else {
            div()
                .w(px(3.0))
                .h(px(3.0))
                .rounded_full()
                .bg(rgb(theme::TEXT_MUTED))
        };
        let cell_div = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .w(px(22.0))
            .h(px(20.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .child(mark);
        let cell_div = if enabled {
            cell_div.cursor_pointer()
        } else {
            cell_div.cursor_default().opacity(0.55)
        };
        if !enabled {
            // A dimmed cell is not a cell that works: the click handler is
            // what makes it live, so a disabled one does not get one.
            return cell_div;
        }
        cell_div.on_click(cx.listener(move |this, _, _, cx| {
            let main = if keeps {
                current
            } else {
                model::MainAlign::from_index(cell.main as usize)
            };
            let cross = model::CrossAlign::from_index(cell.cross as usize);
            if let (Some(main), Some(cross)) = (main, cross) {
                this.commit_layout(model::LayoutEdit::Alignment { main, cross }, cx);
            }
        }))
    }

    /// The gap between the items, and whether the flow makes it up.
    fn gap_column(
        &self,
        state: &LayoutState,
        availability: Availability,
        cx: &mut Context<Self>,
    ) -> Div {
        let words = NumberWords {
            automatic: state.auto_gap().then_some(AUTO),
            ..NumberWords::layout()
        };
        let gap = self.number_field(Property::Gap, None, state.gap, availability, words, cx);
        let mode = self.gap_mode(state, availability, cx);
        column().flex_1().gap_2().child(gap).child(mode)
    }

    /// How the gap between the items is made.
    ///
    /// The engine couples the two: an auto gap *is* a main alignment, so
    /// choosing one here is choosing how the items sit along the flow.
    fn gap_mode(
        &self,
        state: &LayoutState,
        availability: Availability,
        cx: &mut Context<Self>,
    ) -> Div {
        let label = match state.main_align {
            Value::Set(model::MainAlign::SpaceBetween) => "Space between",
            Value::Set(model::MainAlign::SpaceAround) => "Space around",
            Value::Set(model::MainAlign::SpaceEvenly) => "Space evenly",
            Value::Set(_) => "Fixed gap",
            Value::Unset => NOT_SET,
            Value::Mixed => MIXED,
        };
        self.layout_menu(
            LayoutMenu::GapMode,
            "inspector-layout-gap-mode",
            label,
            availability,
            vec![
                ("Fixed", model::LayoutEdit::MainAlign(model::MainAlign::Min)),
                (
                    "Auto (space between)",
                    model::LayoutEdit::MainAlign(model::MainAlign::SpaceBetween),
                ),
                (
                    "Auto (space evenly)",
                    model::LayoutEdit::MainAlign(model::MainAlign::SpaceEvenly),
                ),
                (
                    "Auto (space around)",
                    model::LayoutEdit::MainAlign(model::MainAlign::SpaceAround),
                ),
            ],
            cx,
        )
    }

    /// The padding, and the rest of the spacing.
    fn layout_spacing(
        &self,
        state: &LayoutState,
        availability: Availability,
        cx: &mut Context<Self>,
    ) -> Div {
        let words = NumberWords::layout();
        let mut body = column().gap_2();
        body = body.child(field_pair(
            self.number_field(
                Property::PaddingHorizontal,
                None,
                state.padding_horizontal,
                availability,
                words,
                cx,
            ),
            self.number_field(
                Property::PaddingVertical,
                None,
                state.padding_vertical,
                availability,
                words,
                cx,
            ),
        ));
        // A wrapped flow has a gap between its lines as well as between its
        // items.
        if state.wraps() {
            body = body.child(self.cross_gap_row(state, availability, cx));
        }
        // A grid has tracks rather than a flow.
        if state.is_grid() {
            body = body.child(field_pair(
                self.number_field(
                    Property::GridColumns,
                    None,
                    state.grid_columns,
                    availability,
                    words,
                    cx,
                ),
                self.number_field(
                    Property::GridRows,
                    None,
                    state.grid_rows,
                    availability,
                    words,
                    cx,
                ),
            ));
        }
        body
    }

    /// The gap between the wrapped lines, and whether the flow makes it up.
    fn cross_gap_row(
        &self,
        state: &LayoutState,
        availability: Availability,
        cx: &mut Context<Self>,
    ) -> Div {
        let vertical = matches!(state.mode, Value::Set(model::LayoutMode::Vertical));
        let name = if vertical { "Column gap" } else { "Row gap" };
        let spread = state.content_align == Value::Set(model::ContentAlign::SpaceBetween);
        let words = NumberWords {
            automatic: spread.then_some(AUTO),
            ..NumberWords::layout()
        };
        let label = match state.content_align {
            Value::Set(model::ContentAlign::SpaceBetween) => "Space between",
            Value::Set(_) => "Fixed gap",
            Value::Unset => NOT_SET,
            Value::Mixed => MIXED,
        };
        let gap = self.number_field(
            Property::CrossGap,
            None,
            state.cross_gap,
            availability,
            words,
            cx,
        );
        let mode = self.layout_menu(
            LayoutMenu::CrossGapMode,
            "inspector-layout-cross-gap-mode",
            label,
            availability,
            vec![
                (
                    "Fixed",
                    model::LayoutEdit::ContentAlign(model::ContentAlign::Auto),
                ),
                (
                    "Auto (space between)",
                    model::LayoutEdit::ContentAlign(model::ContentAlign::SpaceBetween),
                ),
            ],
            cx,
        );
        column()
            .gap_1()
            .child(sublabel(name))
            .child(field_pair(gap, mode))
    }

    /// A menu of choices: the trigger, and the list under it while it is open.
    fn layout_menu(
        &self,
        kind: LayoutMenu,
        selector: &'static str,
        label: &str,
        availability: Availability,
        options: Vec<(&'static str, model::LayoutEdit)>,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.layout_menu == Some(kind);
        let enabled = availability.is_editable();
        let trigger = div()
            .id(SharedString::from(selector))
            .debug_selector(|| selector.to_owned())
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px(px(8.0))
            .py(px(8.0))
            .rounded_md()
            .bg(rgb(theme::WINDOW))
            .border_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .text_xs()
            .child(
                div()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child(label.to_owned()),
            )
            .child(div().text_color(rgb(theme::TEXT_MUTED)).child(if open {
                "▴"
            } else {
                "▾"
            }));
        let trigger = if enabled {
            trigger
                .cursor_pointer()
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        } else {
            trigger.cursor_default().opacity(0.55)
        };
        let mut menu = column().gap_1().child(trigger.on_click(cx.listener(
            move |this: &mut Inspector, _, _, cx: &mut Context<Inspector>| {
                if !enabled {
                    return;
                }
                this.layout_menu = if this.layout_menu == Some(kind) {
                    None
                } else {
                    Some(kind)
                };
                cx.notify();
            },
        )));
        if open && enabled {
            let mut list = div()
                .flex()
                .flex_col()
                .gap_1()
                .p(px(6.0))
                .rounded_md()
                .bg(rgb(theme::WINDOW))
                .border_1()
                .border_color(rgb(theme::BORDER));
            for (index, (name, edit)) in options.into_iter().enumerate() {
                let item = div()
                    .id(SharedString::from(format!("{selector}-item-{index}")))
                    .debug_selector(move || format!("{selector}-item-{index}"))
                    .px(px(6.0))
                    .py(px(5.0))
                    .rounded_sm()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                    .cursor_pointer()
                    .child(name)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.layout_menu = None;
                        this.commit_layout(edit, cx);
                    }));
                list = list.child(item);
            }
            menu = menu.child(list);
        }
        menu
    }

    fn appearance_fields(&self, selected: &[model::DesignObject], cx: &mut Context<Self>) -> Div {
        let count = selected.len();
        let fill = Paint::read(selected, |object| object.fill);
        let stroke = Paint::read(selected, |object| object.stroke.map(|stroke| stroke.color));
        section_body()
            .child(self.paint_block(StyleProperty::Fill, fill, theme::SURFACE_RAISED, cx))
            .child(self.paint_block(StyleProperty::Stroke, stroke, theme::BORDER, cx))
            .child(self.preset_row(
                "Width",
                Property::StrokeWidth,
                selected_value(selected, Property::StrokeWidth),
                count,
                &["1", "2", "4", "8"],
                cx,
            ))
            .child(self.preset_row(
                "Radius",
                Property::BorderRadius,
                selected_value(selected, Property::BorderRadius),
                count,
                &["0", "4", "8", "16"],
                cx,
            ))
            .child(self.preset_row(
                "Opacity",
                Property::Opacity,
                selected_value(selected, Property::Opacity),
                count,
                &["25", "50", "75", "100"],
                cx,
            ))
    }

    fn typography_fields(&self, selected: &[model::DesignObject], cx: &mut Context<Self>) -> Div {
        // Only the text colour. The SIZE row moved into the typography panel, in
        // figcraft's position -- and it moved rather than being copied, because
        // two number widgets for one property is two owners of one value.
        let color = Paint::read(selected, |object| object.text_color);
        section_body().child(self.paint_block(StyleProperty::TextColor, color, theme::TEXT, cx))
    }

    // ── Numbers ─────────────────────────────────────────────────────────────

    /// One labelled number.
    ///
    /// The same control for every numeric property, which is what makes the
    /// differences between properties honest: a field is editable, disabled or
    /// unsupported because the property said so, not because a row forgot to
    /// check. The affordances follow from what the document model can do —
    /// dragging where there is an unrecorded-then-committed operation, typing
    /// everywhere, and nothing at all where neither applies.
    fn number_field(
        &self,
        property: Property,
        object: Option<model::EngineObjectId>,
        value: Value<f32>,
        availability: Availability,
        words: NumberWords,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let spec = property.spec();
        let focus = Focus { object, property };
        let editable = availability.is_editable();
        let scrubbable = editable && property.is_continuous();

        let editing = self
            .edit
            .as_ref()
            .filter(|edit| edit.focus == focus)
            .map(|edit| SharedString::from(edit.buffer().to_owned()));
        let shown = words.shown(value, spec);
        let text = editing.unwrap_or_else(|| SharedString::from(shown));
        let dragging = self.scrub.is_some() && self.focus == Some(focus);
        let border = if dragging {
            theme::ACCENT
        } else {
            theme::BORDER_SOFT
        };

        let field = field_chrome(
            SharedString::from(property.label()),
            text,
            border,
            !editable,
        );
        let field = if scrubbable {
            field.cursor_ew_resize()
        } else {
            field.cursor_default()
        };

        field
            .id(property.control_id(object))
            // A test looks a field up by the engine's node id and the property,
            // so a test can say which layer's field it is looking at. A no-op in
            // a release build.
            .debug_selector(|| property.control_id(object).to_string())
            // A single click points the arrow keys at the field. Typing is a
            // double click, because a click is also how a drag starts, and a
            // control that opened an editor at the end of every scrub would make
            // scrubbing unusable.
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if !editable {
                    return;
                }
                if event.click_count() >= 2 {
                    this.open_edit(focus, cx);
                } else {
                    this.focus = Some(focus);
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    if scrubbable {
                        this.begin_scrub(focus, event.position.x.into(), cx);
                    }
                }),
            )
            .on_mouse_move(
                cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                    let owns = this.scrub.is_some_and(|scrub| {
                        scrub.property() == property
                            && object.is_some_and(|object| scrub.object() == object)
                    });
                    if owns {
                        this.move_scrub(event.position.x.into(), cx);
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.end_scrub(cx);
                }),
            )
    }

    /// One labelled number, with the panel's own availability and words.
    ///
    /// The shape every numeric row outside the layout section takes: the
    /// availability comes from the property itself and the words are the
    /// panel's. The layout section says which of its own numbers it means, so
    /// it uses [`Self::number_field`] directly.
    fn panel_number_field(
        &self,
        property: Property,
        object: Option<model::EngineObjectId>,
        value: Value<f32>,
        count: usize,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let availability = self.availability(property, &value, count);
        self.number_field(
            property,
            object,
            value,
            availability,
            NumberWords::panel(),
            cx,
        )
    }

    /// A number offered as one-click values.
    ///
    /// Presets rather than a scrub for paint: only geometry has an operation
    /// that records when the user lets go, and a drag on a field without one
    /// would be one history entry per pointer movement. Every preset is a value
    /// the style model can definitely express, so every chip is a control whose
    /// edit saves — and one click is one semantic operation, the same as a drag.
    fn preset_row(
        &self,
        label: &'static str,
        property: Property,
        value: Value<f32>,
        count: usize,
        presets: &'static [&'static str],
        cx: &mut Context<Self>,
    ) -> Div {
        let spec = property.spec();
        let availability = self.availability(property, &value, count);
        let editable = availability.is_editable();
        let text = match value {
            Value::Set(number) => format_number(number, spec),
            Value::Unset => NO_VALUE.to_owned(),
            Value::Mixed => MIXED.to_owned(),
        };
        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child(label),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(text),
            );
        for preset in presets {
            // The chip says what the user reads, so the value it commits is that
            // number put back through the spec. Opacity reads `100` and stores
            // `1.0`; without the round trip the presets would be off by a
            // hundred.
            let stored = spec.stored(preset.parse::<f32>().unwrap_or_default());
            let edit = property.style_edit(stored);
            let enabled = editable && edit.is_some();
            let chip = div()
                .id(SharedString::from(format!(
                    "preset-{}-{}",
                    property.slug(),
                    preset
                )))
                .px(px(7.0))
                .py(px(5.0))
                .rounded_sm()
                .bg(rgb(theme::WINDOW))
                .border_1()
                .border_color(rgb(theme::BORDER_SOFT))
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(*preset);
            let chip = if enabled {
                chip.cursor_pointer()
                    .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            } else {
                chip.cursor_default().opacity(0.55)
            };
            row = row.child(chip.on_click(cx.listener(move |this, _, _, cx| {
                if let Some(edit) = edit {
                    this.commit_style(edit, cx);
                }
            })));
        }
        row
    }

    // ── Paint ───────────────────────────────────────────────────────────────

    /// A paint row with its palette, so the two are one block that opens and
    /// closes together.
    fn paint_block(
        &self,
        property: StyleProperty,
        paint: Paint,
        fallback: u32,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut block = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(self.paint_row(property, paint, fallback, cx));
        // The engine's colour picker, under the row that opened it.
        if let Some(picker) = self.color_picker(property, cx) {
            block = block.child(picker);
        }
        block
    }

    /// One paint: whether it is on, and the colour it is on with.
    ///
    /// The two are separate controls because they disagree independently: a
    /// selection can agree on "filled" and disagree on the colour, or agree on
    /// neither. Reading one from the other is how a panel ends up calling a
    /// half-filled selection uniform.
    fn paint_row(
        &self,
        property: StyleProperty,
        paint: Paint,
        fallback: u32,
        cx: &mut Context<Self>,
    ) -> Div {
        let color = model::Color::from_rgb(paint.swatch(fallback));
        let selected = match paint.present {
            Value::Set(true) => Some(0),
            Value::Set(false) => Some(1),
            _ => None,
        };
        let state = if paint.present.is_mixed() {
            Some(MIXED)
        } else {
            None
        };
        // A mixed presence is still editable: switching a paint on or off
        // applies to the whole selection, which is one operation and the reason
        // `selection.md` says setting a value applies to all. So no option is
        // selected and the word `Mixed` says why.
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child(property.label()),
            )
            .child(segmented(
                &format!("{}-state", property.slug()),
                &[("On", true), ("Off", true)],
                selected,
                state,
                false,
                move |index: usize, this: &mut Inspector, cx: &mut Context<Inspector>| {
                    let color = if index == 0 { Some(color) } else { None };
                    this.commit_paint(property, color, cx);
                },
                cx,
            ))
            .child(
                div()
                    .id(SharedString::from(format!(
                        "{}-color-trigger",
                        property.slug()
                    )))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px(px(6.0))
                    .py(px(5.0))
                    .rounded_sm()
                    .bg(rgb(theme::WINDOW))
                    .border_1()
                    .border_color(rgb(theme::BORDER_SOFT))
                    .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // The engine's colour picker, not the old static
                        // palette: it edits the colour the engine draws, and
                        // its drag is one undo step.
                        let target = match property {
                            StyleProperty::Fill => panel_colorpicker::ColorTarget::Fill,
                            StyleProperty::Stroke => panel_colorpicker::ColorTarget::Stroke,
                            StyleProperty::TextColor => panel_colorpicker::ColorTarget::TextColor,
                        };
                        this.palette = None;
                        this.color_state.borrow_mut().toggle(target);
                        cx.notify();
                    }))
                    .child(div().size(px(12.0)).rounded_sm().bg(rgb(color.to_rgb())))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme::TEXT_SECONDARY))
                            .child(paint.label()),
                    ),
            )
    }

    /// The engine's colour picker for one paint, if it is open.
    fn color_picker(&self, property: StyleProperty, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(engine) = self.engine.clone() else {
            return None;
        };
        let target = match property {
            StyleProperty::Fill => panel_colorpicker::ColorTarget::Fill,
            StyleProperty::Stroke => panel_colorpicker::ColorTarget::Stroke,
            StyleProperty::TextColor => panel_colorpicker::ColorTarget::TextColor,
        };
        panel_colorpicker::picker(&engine, target, self.color_state.clone(), cx)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Shared chrome
// ═══════════════════════════════════════════════════════════════════════════

/// A plain vertical stack. The base every section and section body is built on.
fn column() -> Div {
    div().flex().flex_col()
}

/// The body of a section: one column, one gap, one padding.
///
/// One function so a section body cannot pick its own spacing. It had three —
/// `12`, `13`, and a nested `12` inside a `16` — which is the kind of drift that
/// makes a panel look assembled rather than drawn.
fn section_body() -> Div {
    div().flex().flex_col().gap_2().px(px(14.0)).py(px(12.0))
}

/// Two fields side by side.
fn field_pair(left: impl IntoElement, right: impl IntoElement) -> Div {
    div().flex().gap_2().child(left).child(right)
}

/// The shape every labelled field shares: a label on the left, a value on the
/// right, one border, one padding, one type size.
///
/// One function so a preset row and a scrubbable field cannot drift apart into
/// two looks for the same thing. `dimmed` is how a field that cannot be used
/// says so — the value stays readable, which matters, because a mixed selection
/// still has a value worth reading.
fn field_chrome(label: SharedString, value: SharedString, border: u32, dimmed: bool) -> Div {
    let field = div()
        .flex()
        .flex_1()
        .items_center()
        .justify_between()
        .gap_2()
        .px(px(8.0))
        .py(px(8.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(border))
        .text_xs()
        .child(div().text_color(rgb(theme::TEXT_MUTED)).child(label))
        .child(div().text_color(rgb(theme::TEXT_SECONDARY)).child(value));
    if dimmed {
        field.opacity(0.55)
    } else {
        field
    }
}

/// A run of text above the controls it labels.
///
/// The engine's own panel labels a group of rows this way, and the reason
/// holds: a row gap with nothing to say which gap it is would be a control
/// that cannot mean anything.
fn sublabel(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// The reason a row of controls cannot be used.
///
/// Said rather than hidden: a dimmed control with no explanation is a control
/// the user has to guess about, and the reason is usually the one thing that
/// tells them what to do next.
fn disabled_note(reason: &'static str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(reason.to_owned())
}

/// A section with nothing in it yet.
///
/// The same note shape everywhere a section has no controls, so "nothing here
/// yet" is one visual thing rather than a sentence written four ways.
fn empty_note(message: &'static str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .px(px(14.0))
        .py(px(12.0))
        .child(message)
}

/// A run of text above the controls that change it.
///
/// Figma stacks a text field above its own typography rows, and the reason
/// holds: a font size with nothing to set is a control that cannot mean anything.
fn text_block(content: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .px(px(14.0))
        .py(px(13.0))
        .border_b_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child("Text content"),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(theme::TEXT))
                .child(content.to_owned()),
        )
}

/// The header a multi-selection gets instead of a layer name.
fn multi_header(count: usize) -> Div {
    section_body()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(theme::TEXT))
                .child(format!("{count} objects selected")),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child("Style changes apply to the entire selection."),
        )
}

/// A row of mutually exclusive choices.
///
/// `selected` is an `Option` rather than an index because "none of these" is a
/// state a choice can be in: a selection where some objects are filled and some
/// are not is not `On` and is not `Off`, and a control that silently selected
/// neither would look broken rather than mixed. `state` is the word that says so.
///
/// `grow` is the one difference between the two shapes it draws — a tab strip
/// that fills the panel, and a compact pair that does not — and it is a
/// parameter rather than a second control because the gesture, the states and the
/// pressed look are the same in both.
fn segmented(
    prefix: &str,
    labels: &[(&'static str, bool)],
    selected: Option<usize>,
    state: Option<&'static str>,
    grow: bool,
    action: impl Fn(usize, &mut Inspector, &mut Context<Inspector>) + Copy + 'static,
    cx: &mut Context<Inspector>,
) -> Div {
    let mut row = div().flex().items_center().gap_1();
    if let Some(state) = state {
        row = row.child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(state),
        );
    }
    for (index, (label, offered)) in labels.iter().enumerate() {
        let pressed = selected == Some(index);
        let enabled = *offered;
        // `action` is `Copy` and captures nothing, so every option below can
        // move its own copy into its own listener without sharing one.
        let option = div()
            .id(SharedString::from(format!("{prefix}-{index}")))
            // A test looks an option up by the control's name and the option's
            // index, so a test can say which choice it is clicking. A no-op in
            // a release build.
            .debug_selector(|| format!("{prefix}-{index}"))
            .flex()
            .items_center()
            .justify_center()
            .py(px(7.0))
            .px(px(8.0))
            .rounded_sm()
            .bg(rgb(if pressed {
                theme::ACCENT_WASH
            } else {
                theme::WINDOW
            }))
            .border_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .text_xs()
            .text_color(rgb(if pressed {
                theme::ACCENT
            } else {
                theme::TEXT_SECONDARY
            }))
            .child(*label);
        let option = if grow { option.flex_1() } else { option };
        let option = if enabled {
            option
                .cursor_pointer()
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        } else {
            option.cursor_default().opacity(0.55)
        };
        row = row.child(option.on_click(cx.listener(
            move |this: &mut Inspector, _: &gpui::ClickEvent, _, cx: &mut Context<Inspector>| {
                if enabled {
                    action(index, this, cx);
                }
            },
        )));
    }
    row
}

// ═══════════════════════════════════════════════════════════════════════════
// The other two tabs
// ═══════════════════════════════════════════════════════════════════════════

fn ai_body() -> AnyElement {
    column()
        .gap_3()
        .p(px(15.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().text_color(rgb(theme::ACCENT)).child("✦"))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(theme::TEXT))
                        .child("Design with context"),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child("Spool can work with the selected Hero section and the styles around it."),
        )
        .child(crate::shell::agent_suggestion("↗", "Make this responsive"))
        .child(crate::shell::agent_suggestion(
            "✳",
            "Suggest spacing improvements",
        ))
        .into_any_element()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The engine's identity for the layer the tests use.
    fn layer() -> model::EngineObjectId {
        model::EngineObjectId(1)
    }

    fn geometry(x: f32, y: f32, width: f32, height: f32) -> model::Geometry {
        model::Geometry {
            position: point(x, y),
            size: size(width, height),
        }
    }

    fn scrub_of(property: Property, before: model::Geometry) -> Scrub {
        Scrub::begin(property, layer(), before, 100.0)
    }

    fn object(
        fill: Option<u32>,
        stroke: Option<(u32, f32)>,
        text_color: Option<u32>,
        font_size: Option<f32>,
    ) -> model::DesignObject {
        model::DesignObject {
            id: layer(),
            name: "Shape".to_string(),
            kind: "Rectangle".to_string(),
            position: point(0.0, 0.0),
            size: size(20.0, 20.0),
            text_content: None,
            text_color: text_color.map(model::Color::from_rgb),
            font_size,
            fill: fill.map(model::Color::from_rgb),
            stroke: stroke.map(|(color, width)| model::Stroke {
                color: model::Color::from_rgb(color),
                width,
            }),
            border_radius: Some(0.0),
            opacity: 1.0,
            auto_layout: None,
            frame_like: false,
        }
    }

    // ── Sections ────────────────────────────────────────────────────────────

    #[test]
    fn every_section_has_its_own_slot_in_range() {
        // A slot is read and written by a closure baked at render time, so an
        // out-of-range or shared slot is a bug that only shows up as the wrong
        // section collapsing. Asserting the slots means adding a section is a
        // compile-and-test question rather than a launch-and-complain one.
        let slots = [
            SectionSlot::POSITION,
            SectionSlot::LAYOUT,
            SectionSlot::APPEARANCE,
            SectionSlot::TYPOGRAPHY,
            SectionSlot::EFFECTS,
            SectionSlot::EXPORT,
            SectionSlot::STYLES,
            SectionSlot::VARIABLES,
            SectionSlot::ASSETS,
            SectionSlot::A11Y,
        ];
        assert_eq!(slots.len(), SECTION_COUNT, "every slot is accounted for");
        for (index, slot) in slots.iter().enumerate() {
            assert!(
                slot.index() < SECTION_COUNT,
                "section {index} is out of range"
            );
            assert!(
                !slots[..index].contains(slot),
                "section {index} shares a slot, so collapsing one collapses another"
            );
        }
    }

    #[test]
    fn collapsing_one_section_leaves_its_neighbours_alone() {
        let mut sections = Sections::new();
        sections.toggle(SectionSlot::LAYOUT);
        assert!(sections.is_collapsed(SectionSlot::LAYOUT));
        for slot in [
            SectionSlot::POSITION,
            SectionSlot::APPEARANCE,
            SectionSlot::TYPOGRAPHY,
            SectionSlot::EFFECTS,
            SectionSlot::EXPORT,
        ] {
            if slot != SectionSlot::LAYOUT {
                assert!(
                    !sections.is_collapsed(slot),
                    "toggling one section changed another"
                );
            }
        }
        sections.toggle(SectionSlot::LAYOUT);
        assert!(!sections.is_collapsed(SectionSlot::LAYOUT));
    }

    // ── The three states ────────────────────────────────────────────────────

    #[test]
    fn a_uniform_selection_has_a_value_and_an_unapplied_one_does_not() {
        assert_eq!(Value::across([Some(4.0), Some(4.0)]), Value::Set(4.0));
        assert_eq!(
            Value::<f32>::across([None, None]),
            Value::Unset,
            "agreement that nothing is set is still agreement"
        );
    }

    #[test]
    fn a_disagreement_is_mixed_and_stays_mixed() {
        assert_eq!(Value::across([Some(1.0), Some(2.0)]), Value::Mixed);
        assert_eq!(
            Value::across([Some(1.0), None]),
            Value::Mixed,
            "one object having a value the other lacks is a disagreement"
        );
        // A later agreement cannot make a selection uniform again: the two
        // objects already differ, so the selection is mixed whatever the rest
        // say. Folding left to right gets this right without buffering.
        assert_eq!(
            Value::across([Some(1.0), Some(2.0), Some(1.0)]),
            Value::Mixed
        );
        assert_eq!(Value::across([Some(1.0), None, Some(1.0)]), Value::Mixed);
    }

    #[test]
    fn an_empty_selection_has_no_value_and_does_not_claim_one() {
        let empty: Vec<Option<f32>> = Vec::new();
        assert_eq!(Value::across(empty), Value::Unset);
    }

    #[test]
    fn a_number_never_reads_as_zero_just_because_nothing_set_it() {
        // `Unset` and `Mixed` are different claims, and both are claims the
        // panel has to be able to make out loud.
        assert!(Value::<f32>::Mixed.is_mixed() && !Value::<f32>::Mixed.is_unset());
        assert!(Value::<f32>::Unset.is_unset() && !Value::<f32>::Unset.is_mixed());
        assert_eq!(
            format_number(0.0, Property::StrokeWidth.spec()),
            "0",
            "a width of zero is a width, and has to be readable as one"
        );
        assert_ne!(
            format_number(0.0, Property::StrokeWidth.spec()),
            NO_VALUE,
            "the same digits are the mark for a width that does not exist"
        );
    }

    // ── Availability ────────────────────────────────────────────────────────

    #[test]
    fn only_supported_is_editable_and_every_other_state_explains_itself() {
        assert!(Availability::Supported.is_editable());
        assert_eq!(Availability::Supported.reason(), None);

        // An unusable control that cannot say why is just a dead rectangle, and
        // a dead rectangle is worse than an absent one because it looks like a
        // promise somebody intends to keep.
        for reason in [
            NO_STROKE,
            MIXED_GEOMETRY,
            NO_FRAME,
            NO_AUTO_LAYOUT,
            MIXED_AUTO_LAYOUT,
        ] {
            assert!(!Availability::Disabled(reason).is_editable());
            assert_eq!(Availability::Disabled(reason).reason(), Some(reason));
        }
    }

    // ── Numbers ─────────────────────────────────────────────────────────────

    #[test]
    fn one_rounding_rule_writes_every_number_in_the_panel() {
        let pixels = NumberSpec::length(None);
        assert_eq!(format_number(0.0, pixels), "0");
        assert_eq!(format_number(100.0, pixels), "100");
        assert_eq!(format_number(-12.5, pixels), "-12.5");
        assert_eq!(
            format_number(12.0, pixels),
            "12",
            "a whole number is not a different number from the same one with a decimal"
        );
        let percent = NumberSpec::percent();
        assert_eq!(format_number(1.0, percent), "100%");
        assert_eq!(format_number(0.5, percent), "50%");
    }

    #[test]
    fn a_number_is_clamped_to_its_own_range_in_display_units() {
        let opacity = Property::Opacity.spec();
        assert_eq!(opacity.display(0.5), 50.0);
        assert_eq!(opacity.stored(50.0), 0.5);
        assert_eq!(
            format_number(2.0, opacity),
            "100%",
            "a value above the maximum reads as the maximum"
        );
        let width = Property::Width.spec();
        assert_eq!(
            format_number(0.0, width),
            "1",
            "a box cannot offer a size it will not accept"
        );
        let radius = Property::BorderRadius.spec();
        assert_eq!(format_number(-4.0, radius), "0");
    }

    #[test]
    fn a_scrub_lands_on_a_step_rather_than_between_two_values_that_read_the_same() {
        let pixels = NumberSpec::length(None);
        let scrub = scrub_of(Property::X, geometry(10.0, 0.0, 20.0, 20.0));
        assert_eq!(scrub.value_at(100.0, pixels), 10.0, "no movement");
        assert_eq!(scrub.value_at(104.0, pixels), 14.0);
        assert_eq!(
            scrub.value_at(104.4, pixels),
            14.0,
            "a drag settles on a whole pixel, not on wherever the pointer stopped"
        );
        assert_eq!(scrub.value_at(96.0, pixels), 6.0);
    }

    #[test]
    fn a_scrub_is_a_function_of_where_it_started_and_not_of_how_many_moves_it_saw() {
        let pixels = NumberSpec::length(None);
        let scrub = scrub_of(Property::X, geometry(10.0, 0.0, 20.0, 20.0));
        let straight = scrub.value_at(110.0, pixels);
        // The same pointer position reached after a detour must read the same.
        let _detour = [
            scrub.value_at(103.0, pixels),
            scrub.value_at(120.0, pixels),
            scrub.value_at(101.0, pixels),
        ];
        assert_eq!(scrub.value_at(110.0, pixels), straight);
    }

    #[test]
    fn a_scrub_describes_the_whole_geometry_and_leaves_the_other_axis_alone() {
        let scrub = scrub_of(Property::X, geometry(10.0, 30.0, 20.0, 40.0));
        let moved = scrub.geometry_at(120.0);
        assert_eq!(moved, geometry(30.0, 30.0, 20.0, 40.0));
        let scrub = scrub_of(Property::Height, geometry(10.0, 30.0, 20.0, 40.0));
        assert_eq!(scrub.geometry_at(100.0), geometry(10.0, 30.0, 20.0, 40.0));
    }

    #[test]
    fn a_size_never_goes_below_the_size_that_can_still_be_hit() {
        let before = geometry(0.0, 0.0, 20.0, 20.0);
        assert_eq!(
            Property::Width.write(before, -5.0).size.width,
            1.0,
            "a zero-sized box cannot be clicked again, so shrinking one to nothing makes it unrecoverable"
        );
        assert_eq!(
            Property::Height.write(before, 0.0).size.height,
            1.0,
            "the floor holds however the value arrived"
        );
        assert_eq!(
            Property::X.write(before, -5.0).position.x,
            -5.0,
            "a position has no floor: a box may sit off the left edge"
        );
    }

    #[test]
    fn a_property_reads_and_writes_the_same_value_back() {
        let before = geometry(10.0, 20.0, 30.0, 40.0);
        for property in [Property::X, Property::Y, Property::Width, Property::Height] {
            let spec = property.spec();
            let next = property.write(before, spec.stored(99.0));
            assert_eq!(
                property.read_geometry(next),
                99.0,
                "{property:?} lost the value it was given"
            );
        }
    }

    #[test]
    fn a_geometry_property_has_no_style_edit_and_a_paint_one_has_no_geometry() {
        for geometry in [Property::X, Property::Y, Property::Width, Property::Height] {
            assert_eq!(
                geometry.style_edit(1.0),
                None,
                "{geometry:?} is a property of the box, not of its paint"
            );
        }
        assert_eq!(
            Property::BorderRadius.style_edit(4.0),
            Some(model::StyleEdit::BorderRadius(4.0))
        );
        assert_eq!(
            Property::Opacity.style_edit(0.5),
            Some(model::StyleEdit::Opacity(0.5))
        );
        assert_eq!(
            Property::FontSize.style_edit(16.0),
            Some(model::StyleEdit::FontSize(16.0))
        );
        assert!(Property::X.is_geometry() && !Property::Opacity.is_geometry());
        assert!(
            Property::X.is_continuous(),
            "only geometry has an operation that records when the user lets go"
        );
        assert!(!Property::Opacity.is_continuous());
    }

    #[test]
    fn a_geometry_property_names_the_engine_key_it_is_written_with() {
        assert_eq!(
            Property::X.box_property(),
            Some(model::BoxProperty::X),
            "X is a property of the box"
        );
        assert_eq!(
            Property::Width.box_property(),
            Some(model::BoxProperty::Width)
        );
        assert_eq!(
            Property::Opacity.box_property(),
            None,
            "opacity is not a property of the box"
        );
        assert_eq!(model::BoxProperty::X.key(), "x");
        assert_eq!(model::BoxProperty::Height.key(), "height");
    }

    #[test]
    fn control_ids_are_stable_and_do_not_collide() {
        let x = Property::X.control_id(Some(layer()));
        let y = Property::Y.control_id(Some(layer()));
        let other_object = Property::X.control_id(Some(model::EngineObjectId(2)));
        let selection = Property::X.control_id(None);

        assert_ne!(x, y, "two properties cannot share one control");
        assert_ne!(
            x, other_object,
            "one property on two objects cannot share one control"
        );
        assert_ne!(
            x, selection,
            "a per-object field is not the selection's field"
        );
        assert_eq!(
            x,
            Property::X.control_id(Some(layer())),
            "an id derived from the name is the same next frame, which is what keeps element state attached to its own field"
        );
    }

    // ── Typing ──────────────────────────────────────────────────────────────

    fn focus() -> Focus {
        Focus {
            object: Some(layer()),
            property: Property::X,
        }
    }

    #[test]
    fn typing_replaces_the_value_that_was_there() {
        // The convention, and the whole difference between "type a number" and
        // "append to the old one": the text arrives selected.
        let mut edit = NumberEdit::editing(focus(), "120".to_owned());
        assert_eq!(edit.apply_key("5"), EditEffect::Continue);
        assert_eq!(edit.buffer(), "5");
        for key in ["0", ".", "5"] {
            edit.apply_key(key);
        }
        assert_eq!(edit.buffer(), "50.5", "only the first key replaced");
    }

    #[test]
    fn enter_commits_the_typed_text() {
        let mut edit = NumberEdit::editing(focus(), "12".to_owned());
        // The first keystroke replaces the value that was there, so this commits
        // a number the user typed rather than one they did not choose.
        edit.apply_key("6");
        edit.apply_key("4");
        assert_eq!(edit.apply_key("enter"), EditEffect::Commit("64".to_owned()));
        assert!(edit.buffer().is_empty(), "and the buffer is gone");
    }

    #[test]
    fn escape_abandons_a_field_without_committing_anything() {
        let mut edit = NumberEdit::editing(focus(), "12".to_owned());
        edit.apply_key("9");
        assert_eq!(edit.buffer(), "9");
        assert_eq!(edit.apply_key("escape"), EditEffect::Abandon);
        assert!(edit.buffer().is_empty());
    }

    #[test]
    fn backspace_clears_selected_text_and_edits_one_character_otherwise() {
        let mut edit = NumberEdit::editing(focus(), "120".to_owned());
        assert_eq!(edit.apply_key("backspace"), EditEffect::Continue);
        assert_eq!(
            edit.buffer(),
            "",
            "with the value selected, Backspace deletes the selection — the same rule typing follows"
        );
        edit.apply_key("7");
        assert_eq!(edit.buffer(), "7");
        edit.apply_key("backspace");
        assert_eq!(
            edit.buffer(),
            "",
            "and once there is no selection it edits one character"
        );
    }

    #[test]
    fn a_modifier_combination_is_swallowed_rather_than_typed_into_the_field() {
        let mut edit = NumberEdit::editing(focus(), String::new());
        for key in ["cmd-d", "cmd-shift-z", "left", "tab"] {
            assert_eq!(edit.apply_key(key), EditEffect::Continue);
        }
        assert_eq!(edit.buffer(), "", "no shortcut leaked into the value");
    }

    #[test]
    fn nudging_a_typed_value_moves_it_and_leaves_the_field_open() {
        let mut edit = NumberEdit::editing(focus(), "10".to_owned());
        assert!(edit.nudge(1.0));
        assert_eq!(edit.buffer(), "11");
        assert!(edit.nudge(-10.0));
        assert_eq!(edit.buffer(), "1");
    }

    #[test]
    fn nudging_refuses_to_invent_a_value_from_text_that_is_not_one_yet() {
        // An arrow pressed in the middle of typing `-` or `1e` must not silently
        // drop the digits the user entered.
        for buffer in ["-", "1e", ""] {
            let mut edit = NumberEdit::editing(focus(), buffer.to_owned());
            assert!(
                !edit.nudge(1.0),
                "nudging {buffer:?} invented a value out of nothing"
            );
            assert_eq!(edit.buffer(), buffer, "and it left the text alone");
        }
    }

    #[test]
    fn a_nudge_stays_inside_the_property_s_range() {
        let mut edit = NumberEdit::editing(
            Focus {
                object: None,
                property: Property::Opacity,
            },
            "100".to_owned(),
        );
        assert!(edit.nudge(10.0));
        assert_eq!(
            edit.buffer(),
            "100",
            "a proportion cannot be nudged past its maximum"
        );
        assert_eq!(
            edit.buffer().parse::<f32>(),
            Ok(100.0),
            "and the buffer stays a bare number, because that is what it has to parse as"
        );
        assert_eq!(
            format_number(1.0, Property::Opacity.spec()),
            "100%",
            "the unit belongs to the field, not to the text being typed into it"
        );
    }

    // ── Paint ───────────────────────────────────────────────────────────────

    fn fill_of(object: &model::DesignObject) -> Option<model::Color> {
        object.fill
    }

    #[test]
    fn a_paint_reports_whether_and_which_independently() {
        // Uniform: both agree.
        let uniform = vec![
            object(Some(theme::PAPER), None, None, None),
            object(Some(theme::PAPER), None, None, None),
        ];
        let paint = Paint::read(&uniform, fill_of);
        assert_eq!(paint.present, Value::Set(true));
        assert_eq!(
            paint.color,
            Value::Set(model::Color::from_rgb(theme::PAPER))
        );
        assert_eq!(
            paint.label(),
            color_hex(model::Color::from_rgb(theme::PAPER))
        );

        // Absent everywhere: agreement that it is off.
        let absent = vec![
            object(None, None, None, None),
            object(None, None, None, None),
        ];
        let paint = Paint::read(&absent, fill_of);
        assert_eq!(paint.present, Value::Set(false));
        assert_eq!(paint.color, Value::Unset);
        assert_eq!(paint.label(), UNSET);
    }

    #[test]
    fn a_half_filled_selection_is_mixed_in_both_readings_but_still_has_a_colour_to_offer() {
        let selected = [
            object(Some(theme::PAPER), None, None, None),
            object(None, None, None, None),
        ];
        let paint = Paint::read(&selected, fill_of);
        assert_eq!(paint.present, Value::Mixed);
        assert_eq!(paint.color, Value::Mixed);
        assert_eq!(paint.label(), MIXED);
        assert_eq!(
            paint.representative,
            Some(model::Color::from_rgb(theme::PAPER)),
            "switching the paint on needs a colour, and this is the only one the selection has"
        );
    }

    #[test]
    fn a_selection_that_agrees_on_being_filled_can_still_disagree_about_the_colour() {
        // Reading the colour's state off whether the paint is on would call this
        // uniform, which is exactly the mistake the three states exist to stop.
        let selected = [
            object(Some(theme::PAPER), None, None, None),
            object(Some(theme::INK), None, None, None),
        ];
        let paint = Paint::read(&selected, fill_of);
        assert_eq!(paint.present, Value::Set(true));
        assert_eq!(paint.color, Value::Mixed);
        assert_eq!(
            paint.representative,
            Some(model::Color::from_rgb(theme::PAPER)),
            "and the toggle offers one of them to fill with"
        );
    }

    #[test]
    fn stroke_width_and_width_are_read_from_the_stroke_not_guessed() {
        let selected = [
            object(Some(theme::PAPER), Some((theme::INK, 1.0)), None, None),
            object(None, Some((theme::INK, 4.0)), None, None),
        ];
        assert_eq!(
            selected_value(&selected, Property::StrokeWidth),
            Value::Mixed,
            "two widths are a disagreement"
        );
        assert_eq!(
            selected_value(&[object(None, None, None, None)], Property::StrokeWidth),
            Value::Unset,
            "a width with no stroke is not a width of zero"
        );
        let uniform = [
            object(Some(theme::PAPER), Some((theme::INK, 2.0)), None, None),
            object(Some(theme::PAPER), Some((theme::INK, 2.0)), None, None),
        ];
        assert_eq!(
            selected_value(&uniform, Property::StrokeWidth),
            Value::Set(2.0)
        );
        assert_eq!(
            selected_value(&uniform, Property::BorderRadius),
            Value::Set(0.0),
            "a property with a value on every object is never unset"
        );
    }

    #[test]
    fn an_unauthored_font_size_stays_distinguishable_from_a_zero_one() {
        let selected = [
            object(None, None, None, None),
            object(None, None, None, Some(16.0)),
        ];
        assert_eq!(selected_value(&selected, Property::FontSize), Value::Mixed);
        assert_eq!(
            selected_value(&[object(None, None, None, None)], Property::FontSize),
            Value::Unset
        );
    }

    // ── Auto layout ─────────────────────────────────────────────────────────

    /// An auto layout, as the model reads one.
    fn layout(gap: f32) -> model::AutoLayout {
        model::AutoLayout {
            mode: model::LayoutMode::Horizontal,
            wrap: false,
            gap,
            cross_gap: 10.0,
            padding: [10.0; 4],
            main_align: model::MainAlign::Min,
            cross_align: model::CrossAlign::Min,
            content_align: model::ContentAlign::Auto,
            grid_columns: 2,
            grid_rows: 2,
        }
    }

    /// A frame, with or without an auto layout.
    fn frame_with(layout: Option<model::AutoLayout>) -> model::DesignObject {
        let mut object = object(None, None, None, None);
        object.auto_layout = layout;
        object.frame_like = true;
        object
    }

    #[test]
    fn a_mixed_selection_shows_mixed_rather_than_one_of_the_values() {
        let selected = [
            frame_with(Some(layout(10.0))),
            frame_with(Some(layout(20.0))),
        ];
        let state = LayoutState::read(&selected);
        assert_eq!(state.gap, Value::Mixed, "two gaps are a disagreement");
        assert_eq!(
            NumberWords::layout().shown(state.gap, Property::Gap.spec()),
            MIXED,
            "and the field says so rather than showing one of the values"
        );
        // The direction the two frames agree about is still shown: a
        // disagreement about the gap is not a disagreement about everything.
        assert_eq!(state.mode, Value::Set(model::LayoutMode::Horizontal));
        assert!(!state.mode.is_mixed());
    }

    #[test]
    fn a_selection_without_an_auto_layout_has_no_gap_to_show() {
        let state = LayoutState::read(&[frame_with(None)]);
        assert_eq!(state.present, Value::Set(false));
        assert_eq!(
            state.gap,
            Value::Unset,
            "a frame without an auto layout has no gap at all, which is not a gap of zero"
        );
        assert_eq!(
            NumberWords::layout().shown(state.gap, Property::Gap.spec()),
            NOT_SET,
            "and the field says so in the engine's words"
        );
    }

    #[test]
    fn a_gap_the_flow_makes_up_says_auto_rather_than_a_number_nobody_chose() {
        let mut layout = layout(10.0);
        layout.main_align = model::MainAlign::SpaceBetween;
        let state = LayoutState::read(&[frame_with(Some(layout))]);
        assert!(state.auto_gap());
        let words = NumberWords {
            automatic: Some(AUTO),
            ..NumberWords::layout()
        };
        assert_eq!(
            words.shown(state.gap, Property::Gap.spec()),
            AUTO,
            "the free space makes this gap up, so the number in the document is not in use"
        );
    }

    #[test]
    fn a_padding_whose_sides_disagree_is_mixed_rather_than_unset() {
        let mut layout = layout(10.0);
        // Left 10, right 8: the padding is there, and there is no one number
        // for it.
        layout.padding = [4.0, 8.0, 4.0, 10.0];
        let state = LayoutState::read(&[frame_with(Some(layout))]);
        assert_eq!(state.padding_horizontal, Value::Mixed);
        assert_eq!(
            state.padding_vertical,
            Value::Set(4.0),
            "the sides that do agree are a value"
        );
    }

    #[test]
    fn the_layout_rows_are_only_editable_when_every_layer_has_a_layout() {
        let with = LayoutState::read(&[frame_with(Some(layout(10.0)))]);
        assert_eq!(
            layout_availability(&with, true),
            Availability::Supported,
            "a frame with an auto layout can have its gap changed"
        );

        let without = LayoutState::read(&[frame_with(None)]);
        assert_eq!(
            layout_availability(&without, true),
            Availability::Disabled(NO_AUTO_LAYOUT),
            "a frame without one cannot: there is no layout to put a gap in"
        );

        let mixed = LayoutState::read(&[frame_with(Some(layout(10.0))), frame_with(None)]);
        assert_eq!(
            layout_availability(&mixed, true),
            Availability::Disabled(MIXED_AUTO_LAYOUT),
            "and a selection that disagrees about it cannot either"
        );

        assert_eq!(
            layout_availability(&with, false),
            Availability::Disabled(NO_FRAME),
            "the engine's own rule: an auto layout needs a frame"
        );
    }

    #[test]
    fn an_auto_layout_is_a_number_of_the_engines_own_words() {
        // The engine's spelling is the contract: `node.set` accepts these and
        // nothing else.
        assert_eq!(model::LayoutMode::Vertical.prop_value(), "vertical");
        assert_eq!(model::LayoutMode::Horizontal.prop_value(), "horizontal");
        assert_eq!(model::LayoutMode::Grid.prop_value(), "grid");
        assert_eq!(model::MainAlign::Min.prop_value(), "min");
        assert_eq!(model::MainAlign::SpaceBetween.prop_value(), "spaceBetween");
        assert_eq!(model::MainAlign::SpaceAround.prop_value(), "spaceAround");
        assert_eq!(model::MainAlign::SpaceEvenly.prop_value(), "spaceEvenly");
        assert_eq!(model::CrossAlign::Baseline.prop_value(), "baseline");
        assert_eq!(model::ContentAlign::Auto.prop_value(), "auto");
        assert_eq!(
            model::ContentAlign::SpaceBetween.prop_value(),
            "spaceBetween"
        );
        assert!(
            model::MainAlign::SpaceBetween.is_auto_gap(),
            "the auto gaps are the ones the free space makes up"
        );
        assert!(!model::MainAlign::Center.is_auto_gap());
    }

    // ── The layer name ──────────────────────────────────────────────────────

    #[test]
    fn a_rename_opens_with_the_old_name_selected_so_the_first_key_replaces_it() {
        let mut session = RenameSession::replacing(layer(), "Headline".into());
        assert_eq!(session.apply_key("H"), RenameEffect::Continue);
        assert_eq!(session.buffer, "H", "the old name is replaced, not kept");
        for key in ["e", "a", "d"] {
            assert_eq!(session.apply_key(key), RenameEffect::Continue);
        }
        assert_eq!(session.buffer, "Head");
        assert_eq!(
            session.apply_key("enter"),
            RenameEffect::Commit("Head".into())
        );
    }

    #[test]
    fn typing_after_the_first_character_appends() {
        let mut session = RenameSession::replacing(layer(), "Headline".into());
        session.apply_key("H");
        session.apply_key("i");
        assert_eq!(session.buffer, "Hi", "only the first key replaced");
    }

    #[test]
    fn escape_abandons_a_rename_without_committing_anything() {
        let mut session = RenameSession::replacing(layer(), "Headline".into());
        session.apply_key("X");
        assert_eq!(session.buffer, "X");
        assert_eq!(
            session.apply_key("escape"),
            RenameEffect::Abandon,
            "nothing reaches the document, so the name is untouched"
        );
        assert!(session.buffer.is_empty(), "and the buffer is gone");
    }

    #[test]
    fn enter_commits_even_an_empty_name_because_trimming_is_the_callers_job() {
        let mut session = RenameSession::replacing(layer(), "Headline".into());
        session.apply_key("backspace");
        assert_eq!(
            session.apply_key("enter"),
            RenameEffect::Commit(String::new()),
            "the caller decides an empty name is not a rename"
        );
    }

    #[test]
    fn a_modifier_combination_is_swallowed_rather_than_typed_into_the_name() {
        // Typing `f` must not also switch to the frame tool, and `cmd-d` must not
        // land in a layer name as the letters "cmd-d".
        let mut session = RenameSession::replacing(layer(), String::new());
        for key in ["cmd-d", "cmd-shift-z", "left", "tab"] {
            assert_eq!(session.apply_key(key), RenameEffect::Continue);
        }
        assert_eq!(session.buffer, "", "no shortcut leaked into the name");
    }

    #[test]
    fn a_name_opened_by_clicking_starts_empty_but_still_replaces_first() {
        let mut session = RenameSession::opening(layer());
        assert_eq!(session.buffer, "");
        session.apply_key("B");
        session.apply_key("o");
        assert_eq!(session.buffer, "Bo");
    }

    // ── Which layer a request names ─────────────────────────────────────────

    #[test]
    fn the_engines_node_id_names_a_layer() {
        // One id space, and it is the engine's. The retired runtime's ids were
        // runtime handles that could not name a layer in the engine's document
        // at all, so there is nothing to compare them against.
        let id = model::EngineObjectId(7);
        assert_eq!(id, model::EngineObjectId(7));
    }

    // ── Which layer a request names ─────────────────────────────────────────

    #[gpui::test]
    fn a_rename_names_the_layer_the_engine_asked_for(cx: &mut gpui::TestAppContext) {
        // The layer a rename names is the engine's own node id. There is one id
        // space: the retired runtime's ids were runtime handles that could not
        // name a layer in the engine's document at all.
        let (root, window, _session, id) = open_panel(cx);
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(900.0)),
            |_, _| root.clone().into_any_element(),
        );
        window.run_until_parked();

        let inspector = window.update(|_, cx| root.read_with(cx, |panel, _| panel.0.clone()));
        window.update(|_, cx| {
            inspector.update(cx, |inspector, cx| {
                inspector.open_rename(model::EngineObjectId(id), cx);
                assert_eq!(
                    inspector.renaming_node(),
                    Some(model::EngineObjectId(id)),
                    "the rename names the engine's layer"
                );
            })
        });
    }

    // ── The panel and the engine, in a window ───────────────────────────────

    /// A session over a document with one rectangle on it, selected.
    fn session_with_a_selected_rectangle() -> (Rc<RefCell<figcraft_engine::Session>>, u64) {
        let mut doc = figcraft_doc::Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(
                page,
                figcraft_doc::NodeKind::Rectangle,
                "Hero",
                10.0,
                20.0,
                30.0,
                40.0,
            )
            .expect("a rectangle");
        let session = Rc::new(RefCell::new(figcraft_engine::Session::with_document(doc)));
        session
            .borrow_mut()
            .execute("select.set", &serde_json::json!({ "ids": [id.0] }))
            .expect("the layer is selected");
        (session, id.0)
    }

    /// A session over a document with one frame on it, selected, with an auto
    /// layout.
    ///
    /// A frame, because an auto layout belongs to a frame: the layout rows are
    /// only real for one.
    fn session_with_a_selected_frame() -> (Rc<RefCell<figcraft_engine::Session>>, u64) {
        frame_session(true)
    }

    /// A session over a document with one frame on it, selected, with no auto
    /// layout.
    fn session_with_a_selected_bare_frame() -> (Rc<RefCell<figcraft_engine::Session>>, u64) {
        frame_session(false)
    }

    /// A session over a document with one frame on it, selected.
    fn frame_session(with_layout: bool) -> (Rc<RefCell<figcraft_engine::Session>>, u64) {
        let mut doc = figcraft_doc::Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(
                page,
                figcraft_doc::NodeKind::Frame,
                "Row",
                10.0,
                20.0,
                300.0,
                200.0,
            )
            .expect("a frame");
        let session = Rc::new(RefCell::new(figcraft_engine::Session::with_document(doc)));
        {
            let mut session = session.borrow_mut();
            if with_layout {
                session
                    .execute("layout.add", &serde_json::json!({ "ids": [id.0] }))
                    .expect("the frame gets an auto layout");
            }
            session
                .execute("select.set", &serde_json::json!({ "ids": [id.0] }))
                .expect("the layer is selected");
        }
        (session, id.0)
    }

    /// A root view that shows the Inspector on its own.
    ///
    /// The panel is normally a part of the shell; a test that wants to see what
    /// it draws needs somewhere to draw it.
    struct Panel(Entity<Inspector>);

    impl Render for Panel {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            self.0.clone()
        }
    }

    /// Open a window with the Inspector in it, over a session with one
    /// selected rectangle.
    fn open_panel(
        cx: &mut gpui::TestAppContext,
    ) -> (
        Entity<Panel>,
        &mut gpui::VisualTestContext,
        Rc<RefCell<figcraft_engine::Session>>,
        u64,
    ) {
        let (session, id) = session_with_a_selected_rectangle();
        open_panel_over(cx, session, id)
    }

    /// Open a window with the Inspector in it, over a session with one
    /// selected frame that has an auto layout.
    fn open_panel_with_a_frame(
        cx: &mut gpui::TestAppContext,
    ) -> (
        Entity<Panel>,
        &mut gpui::VisualTestContext,
        Rc<RefCell<figcraft_engine::Session>>,
        u64,
    ) {
        let (session, id) = session_with_a_selected_frame();
        open_panel_over(cx, session, id)
    }

    /// Open a window with the Inspector in it, over the given session.
    fn open_panel_over(
        cx: &mut gpui::TestAppContext,
        session: Rc<RefCell<figcraft_engine::Session>>,
        id: u64,
    ) -> (
        Entity<Panel>,
        &mut gpui::VisualTestContext,
        Rc<RefCell<figcraft_engine::Session>>,
        u64,
    ) {
        let (root, window) = cx.add_window_view(|_, cx| {
            let engine = cx.new(|cx| EngineCanvas::from_handle(Rc::clone(&session), cx));
            let inspector = cx.new(|_| Inspector::new());
            inspector.update(cx, |inspector, cx| inspector.set_engine_canvas(engine, cx));
            Panel(inspector)
        });
        (root, window, session, id)
    }

    #[gpui::test]
    fn the_panel_shows_the_layer_the_engine_has_selected(cx: &mut gpui::TestAppContext) {
        // The end-to-end claim: the panel reads the *engine's* selection. The
        // element ids carry the engine's node id, so a row that is laid out is a
        // row for the engine's layer and not for something the retired runtime
        // happened to have selected.
        let (root, window, _session, id) = open_panel(cx);
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(900.0)),
            |_, _| root.clone().into_any_element(),
        );
        window.run_until_parked();

        // The selectors are `&'static str`, so the ids are leaked rather than
        // borrowed: a test that cannot name the element it is looking for
        // cannot say whether it is there.
        let name: &'static str = Box::leak(format!("rename-{id}").into_boxed_str());
        let x: &'static str = Box::leak(format!("inspector-{id}-x").into_boxed_str());
        assert!(
            window.debug_bounds(name).is_some(),
            "the panel should show the layer the engine has selected"
        );
        assert!(
            window.debug_bounds(x).is_some(),
            "and the position fields of that layer"
        );
    }

    #[gpui::test]
    fn the_panel_reads_the_engines_values(cx: &mut gpui::TestAppContext) {
        // The values are the engine's: the rectangle sits at 10,20 and is 30
        // wide, and the panel says so.
        let (root, window, _session, id) = open_panel(cx);
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(900.0)),
            |_, _| root.clone().into_any_element(),
        );
        window.run_until_parked();

        let inspector = window.update(|_, cx| root.read_with(cx, |panel, _| panel.0.clone()));
        let selected =
            window.update(|_, cx| inspector.update(cx, |inspector, cx| inspector.selected(cx)));
        assert_eq!(selected.len(), 1, "one layer is selected");
        let object = &selected[0];
        assert_eq!(object.id, model::EngineObjectId(id));
        assert_eq!(object.name, "Hero");
        assert_eq!(object.position.x, 10.0);
        assert_eq!(object.position.y, 20.0);
        assert_eq!(object.size.width, 30.0);
        assert_eq!(object.size.height, 40.0);
    }

    #[gpui::test]
    fn a_number_the_panel_commits_reaches_the_engines_document(cx: &mut gpui::TestAppContext) {
        // The write path, end to end: the panel's own commit, the engine's
        // command, and the engine's document.
        let (root, window, session, id) = open_panel(cx);
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(900.0)),
            |_, _| root.clone().into_any_element(),
        );
        window.run_until_parked();

        let inspector = window.update(|_, cx| root.read_with(cx, |panel, _| panel.0.clone()));
        window.update(|_, cx| {
            inspector.update(cx, |inspector, cx| {
                inspector.commit_number(
                    Focus {
                        object: Some(model::EngineObjectId(id)),
                        property: Property::X,
                    },
                    55.0,
                    cx,
                )
            })
        });

        let session = session.borrow();
        let doc = session.doc().expect("a document");
        let node = doc.node(model::EngineObjectId(id)).expect("the layer");
        assert_eq!(
            doc.frame_relative(model::EngineObjectId(id)).as_coeffs()[4],
            55.0,
            "the engine's document holds the value the panel committed"
        );
        assert_eq!(node.id, model::EngineObjectId(id));
    }

    #[gpui::test]
    fn the_layout_rows_fit_inside_the_panel(cx: &mut gpui::TestAppContext) {
        // The panel is 264 points wide and the layout rows are the widest
        // thing in it. A row that runs off the edge is a row the user cannot
        // read: the value is cut in half, and the control that was supposed to
        // say "Not set" says "Not se" instead.
        let (root, window, _session, _id) = open_panel_with_a_frame(cx);
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(900.0)),
            |_, _| root.clone().into_any_element(),
        );
        window.run_until_parked();

        // The panel's own padding, so the rows are measured against the space
        // they are actually given.
        let edge = px(264.0 - 14.0);
        for selector in [
            "inspector-layout-direction-0",
            "inspector-layout-direction-1",
            "inspector-layout-direction-2",
            "inspector-layout-wrap-0",
            "inspector-layout-gap-mode",
            "inspector-layout-align-1-1",
            "inspector-selection-gap",
            "inspector-selection-padding-h",
            "inspector-selection-padding-v",
        ] {
            let bounds = window
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("the panel draws {selector}"));
            assert!(
                bounds.right() <= edge,
                "{selector} runs off the panel: {bounds:?}"
            );
        }
    }

    #[gpui::test]
    fn a_layout_row_that_cannot_be_used_does_not_change_the_document(
        cx: &mut gpui::TestAppContext,
    ) {
        // A frame with no auto layout: the layout rows are dimmed, and a
        // dimmed row that still writes is a row that lies about being dimmed.
        let (session, id) = session_with_a_selected_bare_frame();
        let (root, window, _session, _id) = open_panel_over(cx, session, id);
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(900.0)),
            |_, _| root.clone().into_any_element(),
        );
        window.run_until_parked();

        let cell = window
            .debug_bounds("inspector-layout-align-1-1")
            .expect("the panel draws the alignment picker");
        window.simulate_click(cell.center(), gpui::Modifiers::none());
        window.run_until_parked();

        let session = _session.borrow();
        let doc = session.doc().expect("a document");
        let node = doc.node(model::EngineObjectId(id)).expect("the frame");
        assert_eq!(
            node.layout, None,
            "a dimmed alignment cell wrote an auto layout into a frame that has none"
        );
    }

    #[gpui::test]
    fn a_style_the_panel_commits_reaches_the_engines_document(cx: &mut gpui::TestAppContext) {
        // The same, for the paint: a style change is one operation across the
        // selection, and it lands in the engine's document.
        let (root, window, session, id) = open_panel(cx);
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(900.0)),
            |_, _| root.clone().into_any_element(),
        );
        window.run_until_parked();

        let inspector = window.update(|_, cx| root.read_with(cx, |panel, _| panel.0.clone()));
        window.update(|_, cx| {
            inspector.update(cx, |inspector, cx| {
                inspector.commit_style(model::StyleEdit::Opacity(0.5), cx)
            })
        });

        let session = session.borrow();
        let doc = session.doc().expect("a document");
        let node = doc.node(model::EngineObjectId(id)).expect("the layer");
        assert_eq!(node.opacity, 0.5, "the engine's document holds the opacity");
    }
}
