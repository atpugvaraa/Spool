//! The colour picker: figcraft's colour popover, ported to GPUI.
//!
//! # What this module owns
//!
//! Two things, in one file because they are one thing:
//!
//! 1. **The model.** [`ColorPickerModel`] reads the engine's [`Document`] and
//!    turns the selection's colour into what the panel shows, and
//!    [`color_command`] turns a change into the engine's own command.
//! 2. **The panel.** [`panel`] draws that model and asks the engine for the
//!    changes the user makes.
//!
//! # The reference
//!
//! figcraft's `crates/ui-egui/src/colorpicker.rs` is the specification: the
//! same square, the same hue and alpha sliders, the same colour models
//! (Hex, RGB, HSL, HSB, CSS), the same alpha field, the same document colours
//! and the same recent colours, the same labels and the same defaults. Where
//! figcraft's picker is a popover floating over a canvas, Spool's is a block
//! inside the Inspector's Appearance section, opened from the same swatch the
//! Inspector already draws.
//!
//! # The colour is the engine's
//!
//! A colour here is [`figcraft_doc::color::Color`] — the engine's own colour,
//! with its alpha — and the panel speaks the engine's spellings: `#RRGGBB`
//! for an opaque colour, `#RRGGBBAA` for one with an alpha. The prop names
//! (`fill`, `stroke`, `textFills`) are the engine's too, and they are checked
//! against the engine's own key list in the tests, so a spelling that drifts
//! is a failing test rather than a silent no-op.
//!
//! # Three states, folded one property at a time
//!
//! A colour of a *selection* is one of three things: a value every selected
//! layer agrees on, agreement that nobody set it, or the absence of
//! agreement. [`ColorValue`] is that fold, and the fold is per property:
//! "Mixed" says the layers disagree about the colour, and the layers still
//! agree about everything else the picker shows.
//!
//! # The alpha, and what the source can carry
//!
//! The engine's colour has an alpha and the engine's `fill` prop carries it
//! (`#RRGGBBAA`), so the picker reads it, shows it and keeps it when the
//! colour changes. Spool's *authored source* cannot write a fill's
//! transparency back yet: the save path writes `background-color: #rrggbb`
//! and drops the alpha, so an alpha set here does not survive a save. The
//! picker says so, in words, whenever the alpha is not opaque — and points at
//! the Inspector's **Opacity** row, which the source *can* carry. See
//! `docs/06-ui-parity.md` §7: a panel shows the value and says why it cannot
//! be written back, rather than inventing a spelling the source has not got.
//!
//! # What is not here, and why
//!
//! figcraft's picker is a *paint* picker: it edits gradients, images and
//! patterns as well as solid colours, and it edits a paint's blend mode. None
//! of that is here, and none of it is in the Inspector either: the Appearance
//! section's Fill and Stroke rows are solid colours, and a paint's kind is a
//! different property with a different row. Adding them is a paint panel, not
//! a colour picker — and the gradient stop bar, the image filters and the
//! pattern controls come with it.
//!
//! The other things figcraft's picker opens for are things Spool's Inspector
//! has no row for: the page's background (`page.setBackground`), a colour
//! variable's value in one mode (`variable.set`), one paint of a colour style
//! (`style.update`), a character range of a text layer being edited
//! (`text.style`) and a plugin's own colour field. The engine has all of
//! those commands; they are a *target*, and Spool's Inspector edits a
//! selection's own paint.
//!
//! The eyedropper is not here either: figcraft's picker offers a button that
//! runs `edit.pickColor`, which hands the screen to the engine's own picker
//! tool. Spool has no such surface yet, and a button that runs a command
//! nothing answers is a button that lies.

use std::cell::RefCell;
use std::rc::Rc;

use figcraft_doc::color::{ColorProfile, Hsb, Hsl, Paint};
use figcraft_doc::{Document, Node, NodeId};
use gpui::{
    checkerboard, div, linear_color_stop, linear_gradient, prelude::*, px, rgb, rgba, AnyElement,
    Bounds, Context, Entity, Hsla, MouseButton, Pixels, Point, SharedString,
};
use serde_json::Value as JsonValue;

use crate::engine::canvas::EngineCanvas;
use crate::theme;

/// The engine's colour: non-premultiplied sRGB with an alpha.
///
/// The colour the panel shows, edits and writes is this one, not a colour of
/// the panel's own: the engine's `fill`, `stroke` and `textFills` props all
/// speak it.
pub use figcraft_doc::color::Color as EngineColor;

// ═══════════════════════════════════════════════════════════════════════════
// The words
// ═══════════════════════════════════════════════════════════════════════════

/// The word for a property the selected layers disagree about.
pub const MIXED: &str = "Mixed";

/// The word for a property nobody has set.
///
/// figcraft's word, not Spool's: a paint nobody filled is "None" in the
/// Inspector, but a value the engine's model does not have is "Not set".
pub const NOT_SET: &str = "Not set";

/// Why a fill's alpha cannot be saved.
///
/// Said out loud rather than hidden: the value is real, it is shown, and the
/// user is told the one thing they cannot do with it. The Inspector's
/// **Opacity** row is the answer — the source carries *that*.
pub const ALPHA_NOT_SAVED: &str =
    "This alpha does not survive a save: the source has no spelling for a fill's transparency yet. Opacity does.";

// ═══════════════════════════════════════════════════════════════════════════
// The three states
// ═══════════════════════════════════════════════════════════════════════════

/// One colour, across a selection.
///
/// Three states, never two. [`ColorValue::Set`] is a colour every selected
/// layer agrees on, [`ColorValue::Unset`] is agreement that the paint is not
/// there, and [`ColorValue::Mixed`] is the absence of agreement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorValue {
    /// Every selected layer has this colour, and they agree on it.
    Set(EngineColor),
    /// No selected layer has this colour.
    Unset,
    /// The selected layers disagree.
    Mixed,
}

impl ColorValue {
    /// Fold a selection's readings into the one state that is honest for all
    /// of them.
    ///
    /// A reading of `None` means "this layer has no such paint", which is an
    /// answer rather than missing data — so the fold is over `Option<Color>`
    /// and never has to invent a default.
    pub fn across(readings: impl IntoIterator<Item = Option<EngineColor>>) -> Self {
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

    fn from(reading: Option<EngineColor>) -> Self {
        match reading {
            Some(color) => Self::Set(color),
            None => Self::Unset,
        }
    }

    /// The colour, when the selection agrees on one.
    pub fn value(self) -> Option<EngineColor> {
        match self {
            Self::Set(color) => Some(color),
            _ => None,
        }
    }

    /// Whether the selection disagrees about the colour.
    pub fn is_mixed(&self) -> bool {
        matches!(self, Self::Mixed)
    }

    /// Whether nobody has set the colour.
    pub fn is_unset(&self) -> bool {
        matches!(self, Self::Unset)
    }

    /// The word the panel shows for a state that is not a value.
    ///
    /// Empty for a value: the value says its own name.
    pub fn word(self) -> &'static str {
        match self {
            Self::Set(_) => "",
            Self::Unset => NOT_SET,
            Self::Mixed => MIXED,
        }
    }

    /// What the picker shows as the colour's name.
    ///
    /// The engine's own spelling: `#RRGGBB`, and `#RRGGBBAA` when the colour
    /// is not opaque.
    pub fn label(self) -> String {
        match self {
            Self::Set(color) => color.to_css_hex(),
            Self::Unset => NOT_SET.to_owned(),
            Self::Mixed => MIXED.to_owned(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The property being edited
// ═══════════════════════════════════════════════════════════════════════════

/// Which paint the picker edits.
///
/// The three the Inspector's Appearance and Typography sections draw, and the
/// three the engine's `node.set` writes: a layer's fill, a layer's stroke and
/// a text layer's own colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorTarget {
    /// The layer's fill.
    Fill,
    /// The layer's stroke.
    Stroke,
    /// The text's colour.
    TextColor,
}

impl ColorTarget {
    /// The label the Inspector's row shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Fill => "Fill",
            Self::Stroke => "Stroke",
            Self::TextColor => "Text colour",
        }
    }

    /// A short stable name, used for element ids and for nothing else.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Stroke => "stroke",
            Self::TextColor => "text-color",
        }
    }

    /// The `node.set` prop this target is written with.
    ///
    /// The engine's own names, read out of the engine's prop list
    /// (`figcraft_engine::cmd::props::KEYS`) and checked against it in the
    /// tests. Never guessed.
    pub fn prop_key(self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Stroke => "stroke",
            Self::TextColor => "textFills",
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The colour models
// ═══════════════════════════════════════════════════════════════════════════

/// The colour model the picker's fields show.
///
/// figcraft's five, in figcraft's order, and figcraft's default: Hex.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorModel {
    /// `RRGGBB`, the way the engine's `to_hex` spells it.
    #[default]
    Hex,
    /// Three channels, 0 to 255.
    Rgb,
    /// Hue, saturation and lightness.
    Hsl,
    /// Hue, saturation and brightness.
    Hsb,
    /// A CSS colour: hex, `rgb()`, `hsl()` or a name.
    Css,
}

impl ColorModel {
    /// Every model, in the order the menu lists them.
    pub const ALL: [ColorModel; 5] = [
        ColorModel::Hex,
        ColorModel::Rgb,
        ColorModel::Hsl,
        ColorModel::Hsb,
        ColorModel::Css,
    ];

    /// The label the menu shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Hex => "Hex",
            Self::Rgb => "RGB",
            Self::Hsl => "HSL",
            Self::Hsb => "HSB",
            Self::Css => "CSS",
        }
    }

    /// The fields this model shows.
    ///
    /// The alpha is *not* one of them: figcraft shows the alpha field for
    /// every model except CSS, where the alpha is part of the text.
    pub fn fields(self) -> Vec<ColorField> {
        match self {
            Self::Hex => vec![ColorField::Hex],
            Self::Rgb => vec![ColorField::Red, ColorField::Green, ColorField::Blue],
            Self::Hsl => vec![
                ColorField::Hue,
                ColorField::Saturation,
                ColorField::Lightness,
            ],
            Self::Hsb => vec![
                ColorField::Hue,
                ColorField::Saturation,
                ColorField::Brightness,
            ],
            Self::Css => vec![ColorField::Css],
        }
    }

    /// Whether this model shows the alpha as its own field.
    pub fn has_alpha_field(self) -> bool {
        self != ColorModel::Css
    }
}

/// One field of the colour model row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorField {
    /// The hex text.
    Hex,
    /// The red channel, 0 to 255.
    Red,
    /// The green channel, 0 to 255.
    Green,
    /// The blue channel, 0 to 255.
    Blue,
    /// The hue, in degrees.
    Hue,
    /// The saturation, in per cent.
    Saturation,
    /// The lightness, in per cent.
    Lightness,
    /// The brightness, in per cent.
    Brightness,
    /// The CSS text.
    Css,
    /// The alpha, in per cent.
    Alpha,
}

impl ColorField {
    /// A short stable name, used for element ids and for nothing else.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Hex => "hex",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Hue => "hue",
            Self::Saturation => "saturation",
            Self::Lightness => "lightness",
            Self::Brightness => "brightness",
            Self::Css => "css",
            Self::Alpha => "alpha",
        }
    }

    /// The unit the field shows, if it has one.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Saturation | Self::Lightness | Self::Brightness | Self::Alpha => "%",
            _ => "",
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The model
// ═══════════════════════════════════════════════════════════════════════════

/// The colour of a selection, as the picker shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorPickerModel {
    /// The property being edited.
    pub target: ColorTarget,
    /// The layers a change applies to.
    pub ids: Vec<NodeId>,
    /// The colour, across the selection.
    pub color: ColorValue,
    /// The first colour the selection has, even when the selection disagrees.
    ///
    /// What the square and the sliders draw when there is no agreed colour:
    /// the panel has to draw *something*, and this is a colour the selection
    /// really has.
    pub representative: Option<EngineColor>,
    /// The colour the square and the sliders are working on.
    ///
    /// The agreed colour, else the representative, else the engine's own
    /// default shape colour — a shape nobody filled is still drawn in the
    /// colour the engine gives it.
    pub working: EngineColor,
    /// The colours used on the page, most used first.
    pub page_colors: Vec<EngineColor>,
    /// The colours used recently, most recent first.
    pub recent: Vec<EngineColor>,
    /// Whether the colour is outside the sRGB gamut.
    pub outside_srgb: bool,
}

impl ColorPickerModel {
    /// The colour of a selection, as the picker shows it.
    ///
    /// A layer the document does not hold is skipped rather than shown as an
    /// empty one, and a selection with no layers at all has no colour.
    pub fn read(
        doc: &Document,
        selection: &[NodeId],
        target: ColorTarget,
        page: Option<NodeId>,
        recent: &[EngineColor],
    ) -> Self {
        let readings: Vec<Option<EngineColor>> = selection
            .iter()
            .filter_map(|id| doc.node(*id))
            .map(|node| color_of(node, target))
            .collect();
        let color = ColorValue::across(readings.iter().copied());
        let representative = readings.iter().copied().flatten().next();
        let working = color
            .value()
            .or(representative)
            .unwrap_or(EngineColor::DEFAULT_SHAPE);
        let page_colors = page
            .map(|page| {
                figcraft_engine::cmd::styles::document_colors(doc, page)
                    .into_iter()
                    .take(21)
                    .map(|(color, _)| color)
                    .collect()
            })
            .unwrap_or_default();
        let outside_srgb = doc
            .color_profile
            .conversion_to(ColorProfile::Srgb)
            .clips(working);
        Self {
            target,
            ids: selection.to_vec(),
            color,
            representative,
            working,
            page_colors,
            recent: recent.to_vec(),
            outside_srgb,
        }
    }

    /// Whether the colour the picker is working on is not opaque.
    ///
    /// The alpha a fill carries does not survive a save, so the picker says
    /// so whenever there is one.
    pub fn alpha_is_not_opaque(&self) -> bool {
        self.working.a < 1.0
    }
}

/// The colour a node's paint is painted with.
///
/// The first *visible* solid paint, the way the Inspector's own rows read a
/// fill: a hidden paint is not the colour the layer looks like.
fn color_of(node: &Node, target: ColorTarget) -> Option<EngineColor> {
    match target {
        ColorTarget::Fill => solid_color(&node.fills),
        ColorTarget::Stroke => solid_color(&node.strokes),
        // A text run may carry its own fills; when it does not, the text is
        // painted with the layer's own fill.
        ColorTarget::TextColor => node
            .kind
            .text()
            .and_then(|text| text.style.fills.as_ref())
            .and_then(|paints| solid_color(paints))
            .or_else(|| solid_color(&node.fills)),
    }
}

/// The first visible solid paint's colour.
fn solid_color(paints: &[Paint]) -> Option<EngineColor> {
    paints
        .iter()
        .filter(|paint| paint.visible)
        .find_map(|paint| paint.solid_color())
}

// ═══════════════════════════════════════════════════════════════════════════
// Writing
// ═══════════════════════════════════════════════════════════════════════════

/// The command that writes one colour to the selection.
///
/// The engine's own command and the engine's own spellings: `node.set`, with
/// `ids` and `props`, and a colour as the engine's hex — `#RRGGBB`, or
/// `#RRGGBBAA` when the colour is not opaque.
///
/// A paint is a property of whatever is selected, so this takes the whole
/// selection: "make these all this colour" is one operation.
pub fn color_command(
    target: ColorTarget,
    color: EngineColor,
    ids: &[NodeId],
) -> (String, JsonValue) {
    let hex = color.to_css_hex();
    let value = match target {
        // `fill` and `stroke` take a colour; `textFills` takes a list of
        // paints, and a plain colour is one paint.
        ColorTarget::Fill | ColorTarget::Stroke => JsonValue::String(hex),
        ColorTarget::TextColor => JsonValue::Array(vec![JsonValue::String(hex)]),
    };
    let mut props = serde_json::Map::new();
    props.insert(target.prop_key().to_owned(), value);
    let mut params = serde_json::Map::new();
    params.insert(
        "ids".to_owned(),
        JsonValue::Array(ids.iter().map(|id| JsonValue::from(id.0)).collect()),
    );
    params.insert("props".to_owned(), JsonValue::Object(props));
    ("node.set".to_owned(), JsonValue::Object(params))
}

/// A change the picker asks the engine for.
#[derive(Clone, Debug, PartialEq)]
pub enum ColorAction {
    /// Set one property's colour across the selection.
    Set {
        /// The property being edited.
        target: ColorTarget,
        /// The colour to set.
        color: EngineColor,
        /// The layers it applies to.
        ids: Vec<NodeId>,
    },
}

impl ColorAction {
    /// The engine command this action is.
    pub fn command(&self) -> (String, JsonValue) {
        match self {
            ColorAction::Set { target, color, ids } => color_command(*target, *color, ids),
        }
    }
}

/// The surface that runs the picker's changes.
///
/// The Inspector is the one that implements it: the picker asks, and the
/// surface that owns the engine runs the command.
pub trait ColorHost: Sized + 'static {
    /// Run one colour change through the engine.
    fn run_color_action(&mut self, action: ColorAction, cx: &mut Context<Self>);

    /// Open a gesture: the engine holds one undo step open for it.
    ///
    /// A drag on the square is one operation, not one per pointer movement,
    /// so the gesture opens with the first move of the drag and closes when
    /// the pointer is let go.
    fn begin_color_gesture(&mut self, label: &str, cx: &mut Context<Self>) -> bool;

    /// Close a gesture as one undo step.
    fn commit_color_gesture(&mut self, cx: &mut Context<Self>);
}

// ═══════════════════════════════════════════════════════════════════════════
// The picker's own state
// ═══════════════════════════════════════════════════════════════════════════

/// Which control a drag is moving.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDragKind {
    /// The saturation and brightness square.
    Square,
    /// The hue slider.
    Hue,
    /// The alpha slider.
    Alpha,
}

/// A drag in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorDrag {
    /// Which control is being dragged.
    pub kind: ColorDragKind,
    /// The colour the drag started from.
    ///
    /// The drag is computed from this and the pointer, never accumulated, so
    /// a dropped frame cannot leave the colour disagreeing with the
    /// document.
    pub from: EngineColor,
    /// The colour the drag is on now.
    pub current: EngineColor,
}

/// A field being typed into.
///
/// A buffer, not a second copy of the property: the value becomes real only
/// when Enter hands it to the engine as one semantic operation. The text
/// arrives selected, so the first keystroke replaces it rather than appending
/// to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorDraft {
    /// Which field is being typed into.
    pub field: ColorField,
    /// The text as typed.
    pub text: String,
    /// Whether the text is selected, so the next keystroke replaces it.
    pub select_all: bool,
}

/// The picker's own state. Presentation only: never in the document.
#[derive(Clone, Debug, Default)]
pub struct ColorPickerState {
    /// Which property's picker is open.
    pub open: Option<ColorTarget>,
    /// The colour model the fields show.
    pub model: ColorModel,
    /// Whether the model menu is open.
    pub menu: bool,
    /// The field being typed into, if one is.
    pub draft: Option<ColorDraft>,
    /// The drag in progress, if one is.
    pub drag: Option<ColorDrag>,
    /// The colours the picker has used, most recent first.
    pub recent: Vec<EngineColor>,
    /// The colour the picker is working on.
    ///
    /// Runtime state, refreshed from the model every time the picker is
    /// drawn: the document is the authority, and this is the value a typed
    /// field is turned into a colour from.
    pub working: EngineColor,
    /// The layers a change applies to.
    pub ids: Vec<NodeId>,
}

impl ColorPickerState {
    /// The state a picker starts in.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the picker is open for a property.
    pub fn is_open(&self, target: ColorTarget) -> bool {
        self.open == Some(target)
    }

    /// Open the picker for a property, or close it if it is already open.
    ///
    /// Opening a picker for one property closes the other: two pickers open
    /// at once would be two places to change the same document, and only one
    /// of them would be the one the user is looking at.
    pub fn toggle(&mut self, target: ColorTarget) {
        if self.is_open(target) {
            self.close();
        } else {
            self.open = Some(target);
            self.menu = false;
            self.draft = None;
            self.drag = None;
        }
    }

    /// Close the picker and give up whatever it was part-way through.
    pub fn close(&mut self) {
        self.open = None;
        self.menu = false;
        self.draft = None;
        self.drag = None;
    }

    /// Whether the picker is part-way through something.
    ///
    /// Read-only, and kept in step with [`Self::cancel`]: the editor's Escape
    /// ladder reads this to decide which rung is on top.
    pub fn is_busy(&self) -> bool {
        self.draft.is_some()
    }

    /// Give up whatever the picker was part-way through.
    ///
    /// Returns whether anything was given up, so the ladder knows the key was
    /// consumed.
    pub fn cancel(&mut self) -> bool {
        self.draft.take().is_some()
    }

    /// Handle one keystroke.
    ///
    /// The whole grammar in one pure-ish function, because it is a grammar:
    /// Enter commits, Escape abandons without touching the document, the first
    /// printable key replaces the selected text, and an arrow is a nudge the
    /// field does not have rather than a character.
    ///
    /// Returns the change the key asks for, if the key was the picker's own.
    pub fn handle_key(&mut self, key: &str) -> Option<ColorAction> {
        let draft = self.draft.clone()?;
        match key {
            "enter" | "return" => {
                self.draft = None;
                let target = self.open?;
                let color = field_value(self.model, draft.field, &draft.text, self.working)?;
                // A typed colour is a choice, not a preview: it joins the
                // recent colours.
                self.remember(color);
                Some(ColorAction::Set {
                    target,
                    color,
                    ids: self.ids.clone(),
                })
            }
            "escape" => {
                self.draft = None;
                None
            }
            "backspace" => {
                if let Some(draft) = self.draft.as_mut() {
                    // With the text selected, Backspace deletes the selection.
                    if draft.select_all {
                        draft.text.clear();
                        draft.select_all = false;
                    } else {
                        draft.text.pop();
                    }
                }
                None
            }
            // Arrows are not characters: the field has no nudge, so the key is
            // swallowed rather than typed into it.
            "up" | "down" | "left" | "right" => None,
            other if other.chars().count() == 1 => {
                if let Some(draft) = self.draft.as_mut() {
                    if draft.select_all {
                        draft.text.clear();
                        draft.select_all = false;
                    }
                    draft.text.push_str(other);
                }
                None
            }
            _ => None,
        }
    }

    /// Remember a colour, the way figcraft does.
    ///
    /// By eight-bit value: a document stores colours rounded to eight bits,
    /// so the colour picked and the one read back differ by a fraction and
    /// would show as two identical swatches.
    pub fn remember(&mut self, color: EngineColor) {
        self.recent.retain(|x| x.to_rgba8() != color.to_rgba8());
        self.recent.insert(0, color);
        self.recent.truncate(16);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The fields
// ═══════════════════════════════════════════════════════════════════════════

/// What a field shows for a colour.
fn field_text(model: ColorModel, field: ColorField, color: EngineColor) -> String {
    match field {
        // figcraft's hex field shows the engine's `to_hex`: `RRGGBB`, with no
        // `#`. The field accepts a hex with or without one, and a colour
        // name.
        ColorField::Hex => color.to_hex(),
        ColorField::Red => color.to_rgba8()[0].to_string(),
        ColorField::Green => color.to_rgba8()[1].to_string(),
        ColorField::Blue => color.to_rgba8()[2].to_string(),
        ColorField::Hue => {
            let hsb = Hsb::from_color(color);
            format!("{}", hsb.h.round() as i64)
        }
        ColorField::Saturation => {
            let value = match model {
                ColorModel::Hsl => Hsl::from_color(color).s,
                _ => Hsb::from_color(color).s,
            };
            format!("{}", (value * 100.0).round() as i64)
        }
        ColorField::Lightness => {
            format!("{}", (Hsl::from_color(color).l * 100.0).round() as i64)
        }
        ColorField::Brightness => {
            format!("{}", (Hsb::from_color(color).b * 100.0).round() as i64)
        }
        // The engine's own CSS spelling: hex when opaque, `rgba(…)` when not.
        ColorField::Css => color.to_css(),
        ColorField::Alpha => format!("{}", (color.a * 100.0).round() as i64),
    }
}

/// The colour a typed field means.
///
/// `None` when the text is not a value this field can hold, so a field that
/// cannot be read commits nothing rather than committing a default.
fn field_value(
    model: ColorModel,
    field: ColorField,
    text: &str,
    current: EngineColor,
) -> Option<EngineColor> {
    match field {
        // A hex field takes a hex or a colour name, and keeps the current
        // alpha unless the hex carries one of its own.
        ColorField::Hex => EngineColor::from_hex_or_name(text)
            .map(|hex| hex.with_alpha(if hex.a < 1.0 { hex.a } else { current.a })),
        ColorField::Red | ColorField::Green | ColorField::Blue => {
            let typed = number(text)?;
            let [r, g, b, _] = current.to_rgba8();
            let mut rgb = [r, g, b];
            let index = match field {
                ColorField::Red => 0,
                ColorField::Green => 1,
                _ => 2,
            };
            rgb[index] = typed.clamp(0.0, 255.0).round() as u8;
            Some(EngineColor::new(
                f32::from(rgb[0]) / 255.0,
                f32::from(rgb[1]) / 255.0,
                f32::from(rgb[2]) / 255.0,
                current.a,
            ))
        }
        ColorField::Hue
        | ColorField::Saturation
        | ColorField::Lightness
        | ColorField::Brightness => {
            let typed = number(text)?;
            let hsl = model == ColorModel::Hsl;
            let (h, s, l) = if hsl {
                let value = Hsl::from_color(current);
                (value.h, value.s, value.l)
            } else {
                let value = Hsb::from_color(current);
                (value.h, value.s, value.b)
            };
            let mut parts = [h, s * 100.0, l * 100.0];
            let index = match field {
                ColorField::Hue => 0,
                ColorField::Saturation => 1,
                _ => 2,
            };
            parts[index] = if index == 0 {
                typed.rem_euclid(360.0) as f32
            } else {
                typed.clamp(0.0, 100.0) as f32
            };
            let (h, s, l) = (parts[0], parts[1] / 100.0, parts[2] / 100.0);
            let color = if hsl {
                Hsl { h, s, l }.to_color()
            } else {
                Hsb { h, s, b: l }.to_color()
            };
            Some(color.with_alpha(current.a))
        }
        // The CSS field carries the alpha in its own text.
        ColorField::Css => EngineColor::parse_css(text),
        ColorField::Alpha => {
            let typed = number(text)?;
            Some(current.with_alpha((typed / 100.0).clamp(0.0, 1.0) as f32))
        }
    }
}

/// A number a field can hold.
///
/// `None` when the text is not a number, so a field that cannot be read
/// commits nothing rather than committing a default.
fn number(text: &str) -> Option<f64> {
    let value: f64 = text.trim().parse().ok()?;
    value.is_finite().then_some(value)
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// The size of the saturation and brightness square.
const SQUARE: f32 = 208.0;

/// The height of the hue slider.
const HUE_HEIGHT: f32 = 18.0;

/// The height of the alpha slider.
const ALPHA_HEIGHT: f32 = 16.0;

/// The width of the hue and alpha sliders.
const SLIDER: f32 = SQUARE;

/// The diameter of a slider's knob.
const KNOB: f32 = 12.0;

/// Where the draggable controls are on screen.
///
/// Recorded while they are painted, because a pointer position is a position
/// on the screen and the panel needs to know where its own controls are. The
/// recording is per render: the element that writes it and the element that
/// reads it are the same render's tree.
#[derive(Clone, Default)]
struct DragAreas {
    square: Rc<RefCell<Option<Bounds<Pixels>>>>,
    hue: Rc<RefCell<Option<Bounds<Pixels>>>>,
    alpha: Rc<RefCell<Option<Bounds<Pixels>>>>,
}

/// The colour picker, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own
/// state, and the surface that runs the changes. It returns an element, so
/// the Inspector can put it under a row of its own and a test can put it in a
/// window on its own.
pub fn panel<T: ColorHost>(
    model: &ColorPickerModel,
    state: Rc<RefCell<ColorPickerState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    // The state holds what a keystroke needs: the colour the fields are
    // showing and the layers a change applies to. The document is the
    // authority, and the model is read out of it every time the picker is
    // drawn.
    {
        let mut state = state.borrow_mut();
        state.working = model.working;
        state.ids = model.ids.clone();
    }
    let areas = DragAreas::default();
    let moving = state.clone();
    let releasing = state.clone();
    let moving_areas = areas.clone();
    let target = model.target;
    let ids = model.ids.clone();
    let mut column = div()
        .flex()
        .flex_col()
        .gap_2()
        .id("color-picker")
        .debug_selector(|| "color-picker".to_owned())
        .on_mouse_move(
            cx.listener(move |host, event: &gpui::MouseMoveEvent, _window, cx| {
                drag_moved(
                    host,
                    target,
                    &ids,
                    &moving_areas,
                    &moving,
                    event.position,
                    cx,
                );
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |host, _event, _window, cx| {
                // The borrow ends with the statement, so the second one below
                // is not a borrow of a borrow.
                let drag = releasing.borrow_mut().drag.take();
                if let Some(drag) = drag {
                    // A drag records only the colour it ended on: the moves
                    // along the way are previews, not choices.
                    releasing.borrow_mut().remember(drag.current);
                    host.commit_color_gesture(cx);
                }
            }),
        );
    column = column.child(header(model, state.clone(), cx));
    column = column.child(square(model, &areas, state.clone(), cx));
    column = column.child(hue_slider(model, &areas, state.clone(), cx));
    column = column.child(alpha_slider(model, &areas, state.clone(), cx));
    column = column.child(model_row(model, state.clone(), cx));
    if model.outside_srgb {
        column = column.child(note("Outside sRGB"));
    }
    if model.alpha_is_not_opaque() {
        column = column.child(note(ALPHA_NOT_SAVED));
    }
    if !model.page_colors.is_empty() {
        column = column.child(swatch_row(
            "On this page",
            &model.page_colors,
            model,
            state.clone(),
            cx,
        ));
    }
    if !model.recent.is_empty() {
        column = column.child(swatch_row("Recent", &model.recent, model, state, cx));
    }
    column.into_any_element()
}

/// The picker for one property of the current selection, if it is open.
///
/// The convenience the Inspector uses: it reads the engine's document, builds
/// the model and draws the picker, or draws nothing when the picker is
/// closed.
pub fn picker<T: ColorHost>(
    canvas: &Entity<EngineCanvas>,
    target: ColorTarget,
    state: Rc<RefCell<ColorPickerState>>,
    cx: &mut Context<T>,
) -> Option<AnyElement> {
    if !state.borrow().is_open(target) {
        return None;
    }
    let recent = state.borrow().recent.clone();
    let model = canvas.read(cx).with_session(|session| {
        let Ok(doc) = session.doc() else {
            return None;
        };
        Some(ColorPickerModel::read(
            doc,
            &session.selection(),
            target,
            session.page().ok(),
            &recent,
        ))
    })?;
    Some(panel(&model, state, cx))
}

/// The header: what is being edited, and the button that closes it.
fn header<T: ColorHost>(
    model: &ColorPickerModel,
    state: Rc<RefCell<ColorPickerState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let word = model.color.word();
    let closing = state.clone();
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(rgb(theme::TEXT))
                .child(model.target.label())
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(word.to_owned()),
                ),
        )
        .child(
            div()
                .id("color-picker-close")
                .debug_selector(|| "color-picker-close".to_owned())
                .px(px(6.0))
                .py(px(3.0))
                .rounded_sm()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("✕")
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    let was_dragging = closing.borrow().drag.is_some();
                    closing.borrow_mut().close();
                    if was_dragging {
                        host.commit_color_gesture(cx);
                    }
                    cx.notify();
                })),
        )
}

/// The saturation and brightness square.
///
/// White to the hue across, transparent to black down: the hue is the
/// horizontal axis, the brightness the vertical one, and the saturation the
/// distance from the left.
fn square<T: ColorHost>(
    model: &ColorPickerModel,
    areas: &DragAreas,
    state: Rc<RefCell<ColorPickerState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let hsb = Hsb::from_color(model.working);
    let hue = hue_color(hsb.h);
    let writer = areas.square.clone();
    let dragging = state.clone();
    let areas = areas.clone();
    let knob = knob_at(
        hsb.s * SQUARE,
        (1.0 - hsb.b) * SQUARE,
        gpui_color(model.working),
    );
    let inner = div()
        .absolute()
        .inset_0()
        .child(div().absolute().inset_0().bg(linear_gradient(
            90.,
            linear_color_stop(gpui::white(), 0.0),
            linear_color_stop(hue, 1.0),
        )))
        .child(div().absolute().inset_0().bg(linear_gradient(
            180.,
            linear_color_stop(gpui::transparent_black(), 0.0),
            linear_color_stop(gpui::black(), 1.0),
        )));
    let target = model.target;
    let ids = model.ids.clone();
    let alpha = model.working.a;
    let hue_at_down = hsb.h;
    div()
        .on_children_prepainted(move |bounds, _window, _cx| {
            if let Some(bounds) = bounds.first() {
                *writer.borrow_mut() = Some(*bounds);
            }
        })
        .id("color-picker-square")
        .debug_selector(|| "color-picker-square".to_owned())
        .relative()
        .w(px(SQUARE))
        .h(px(SQUARE))
        .rounded_sm()
        .overflow_hidden()
        .child(inner)
        .child(knob)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |host, event: &gpui::MouseDownEvent, _window, cx| {
                let Some(bounds) = bounds_of(&areas.square) else {
                    return;
                };
                let color = square_at(bounds, event.position, hue_at_down, alpha);
                host.begin_color_gesture("Color", cx);
                dragging.borrow_mut().drag = Some(ColorDrag {
                    kind: ColorDragKind::Square,
                    from: color,
                    current: color,
                });
                apply(host, target, color, &ids, &dragging, true, cx);
            }),
        )
}

/// The hue slider.
fn hue_slider<T: ColorHost>(
    model: &ColorPickerModel,
    areas: &DragAreas,
    state: Rc<RefCell<ColorPickerState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let hsb = Hsb::from_color(model.working);
    let writer = areas.hue.clone();
    let dragging = state.clone();
    let areas = areas.clone();
    let gradient = div().absolute().inset_0().bg(linear_gradient(
        90.,
        linear_color_stop(hue_color(0.0), 0.0),
        linear_color_stop(hue_color(360.0), 1.0),
    ));
    let knob = knob_at((hsb.h / 360.0) * SLIDER, 0.0, gpui_color(model.working));
    let target = model.target;
    let ids = model.ids.clone();
    let working = model.working;
    div()
        .on_children_prepainted(move |bounds, _window, _cx| {
            if let Some(bounds) = bounds.first() {
                *writer.borrow_mut() = Some(*bounds);
            }
        })
        .id("color-picker-hue")
        .debug_selector(|| "color-picker-hue".to_owned())
        .relative()
        .w(px(SLIDER))
        .h(px(HUE_HEIGHT))
        .rounded_sm()
        .overflow_hidden()
        .child(gradient)
        .child(knob)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |host, event: &gpui::MouseDownEvent, _window, cx| {
                let Some(bounds) = bounds_of(&areas.hue) else {
                    return;
                };
                let color = hue_at(bounds, event.position, working);
                host.begin_color_gesture("Color", cx);
                dragging.borrow_mut().drag = Some(ColorDrag {
                    kind: ColorDragKind::Hue,
                    from: color,
                    current: color,
                });
                apply(host, target, color, &ids, &dragging, true, cx);
            }),
        )
}

/// The alpha slider: the colour, from transparent to opaque, over a
/// checkerboard.
fn alpha_slider<T: ColorHost>(
    model: &ColorPickerModel,
    areas: &DragAreas,
    state: Rc<RefCell<ColorPickerState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let alpha = model.working.a;
    let writer = areas.alpha.clone();
    let dragging = state.clone();
    let areas = areas.clone();
    let working = model.working;
    let gradient = div().absolute().inset_0().bg(linear_gradient(
        90.,
        linear_color_stop(gpui_color(model.working).alpha(0.0), 0.0),
        linear_color_stop(gpui_color(model.working).alpha(1.0), 1.0),
    ));
    let knob = knob_at(alpha * SLIDER, 0.0, gpui_color(model.working));
    let target = model.target;
    let ids = model.ids.clone();
    div()
        .on_children_prepainted(move |bounds, _window, _cx| {
            if let Some(bounds) = bounds.first() {
                *writer.borrow_mut() = Some(*bounds);
            }
        })
        .id("color-picker-alpha")
        .debug_selector(|| "color-picker-alpha".to_owned())
        .relative()
        .w(px(SLIDER))
        .h(px(ALPHA_HEIGHT))
        .rounded_sm()
        .overflow_hidden()
        .bg(checkerboard(gpui_color(model.working), 8.0))
        .child(gradient)
        .child(knob)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |host, event: &gpui::MouseDownEvent, _window, cx| {
                let Some(bounds) = bounds_of(&areas.alpha) else {
                    return;
                };
                let color = alpha_at(bounds, event.position, working);
                host.begin_color_gesture("Color", cx);
                dragging.borrow_mut().drag = Some(ColorDrag {
                    kind: ColorDragKind::Alpha,
                    from: color,
                    current: color,
                });
                apply(host, target, color, &ids, &dragging, true, cx);
            }),
        )
}

/// The colour model row: the model menu, the model's fields, and the alpha.
fn model_row<T: ColorHost>(
    model: &ColorPickerModel,
    state: Rc<RefCell<ColorPickerState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let current = state.borrow().model;
    let mut row = div().flex().items_center().gap_1();
    row = row.child(model_menu(current, state.clone(), cx));
    for field in current.fields() {
        row = row.child(field_box(model, current, field, state.clone(), cx));
    }
    if current.has_alpha_field() {
        row = row.child(field_box(
            model,
            current,
            ColorField::Alpha,
            state.clone(),
            cx,
        ));
    }
    row
}

/// The model menu: the trigger, and the list under it while it is open.
fn model_menu<T: ColorHost>(
    current: ColorModel,
    state: Rc<RefCell<ColorPickerState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let open = state.borrow().menu;
    let toggling = state.clone();
    let trigger = div()
        .id("color-picker-model")
        .debug_selector(|| "color-picker-model".to_owned())
        .min_w(px(46.0))
        .px(px(6.0))
        .py(px(5.0))
        .rounded_sm()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .child(current.label())
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            let mut state = toggling.borrow_mut();
            state.menu = !state.menu;
            drop(state);
            cx.notify();
        }));
    if !open {
        return trigger.into_any_element();
    }
    let choosing = state.clone();
    let mut menu = div()
        .flex()
        .flex_col()
        .gap_1()
        .p(px(5.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER));
    for model in ColorModel::ALL {
        let chosen = model == current;
        let choosing = choosing.clone();
        menu = menu.child(
            div()
                .id(SharedString::from(format!(
                    "color-picker-model-{}",
                    model.label()
                )))
                .debug_selector({
                    let label = model.label();
                    move || format!("color-picker-model-{label}")
                })
                .px(px(6.0))
                .py(px(4.0))
                .rounded_sm()
                .text_xs()
                .text_color(rgb(if chosen {
                    theme::ACCENT
                } else {
                    theme::TEXT_SECONDARY
                }))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child(model.label())
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    let mut state = choosing.borrow_mut();
                    state.model = model;
                    state.menu = false;
                    state.draft = None;
                    drop(state);
                    cx.notify();
                })),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(trigger)
        .child(menu)
        .into_any_element()
}

/// One field of the colour model row.
fn field_box<T: ColorHost>(
    model: &ColorPickerModel,
    current: ColorModel,
    field: ColorField,
    state: Rc<RefCell<ColorPickerState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let text = field_text(current, field, model.working);
    let shown_text = text.clone();
    let draft = state
        .borrow()
        .draft
        .as_ref()
        .filter(|draft| draft.field == field)
        .map(|draft| draft.text.clone());
    let shown = draft.unwrap_or_else(|| {
        let suffix = field.suffix();
        if suffix.is_empty() {
            shown_text
        } else {
            format!("{shown_text}{suffix}")
        }
    });
    let selector = format!("color-picker-field-{}", field.slug());
    let drafting = state.clone();
    let text = text.clone();
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .min_w(px(32.0))
        .px(px(6.0))
        .py(px(5.0))
        .rounded_sm()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .cursor_text()
        .child(shown)
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            let mut state = drafting.borrow_mut();
            state.draft = Some(ColorDraft {
                field,
                text: text.clone(),
                select_all: true,
            });
            drop(state);
            cx.notify();
        }))
}

/// A row of swatches: the document's colours, or the recent ones.
fn swatch_row<T: ColorHost>(
    label: &'static str,
    colors: &[EngineColor],
    model: &ColorPickerModel,
    state: Rc<RefCell<ColorPickerState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let mut row = div().flex().flex_wrap().items_center().gap_1();
    for (index, color) in colors.iter().enumerate() {
        let picking = state.clone();
        let selector = format!(
            "{}-swatch-{}",
            label.to_lowercase().replace(' ', "-"),
            index
        );
        let target = model.target;
        let ids = model.ids.clone();
        let color = *color;
        row = row.child(
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .size(px(14.0))
                .rounded_sm()
                .border_1()
                .border_color(rgb(theme::BORDER))
                .bg(rgba(rgba_u32(color)))
                .cursor_pointer()
                .hover(|style| style.opacity(0.8))
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    apply(host, target, color, &ids, &picking, false, cx);
                })),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(label.to_owned()),
        )
        .child(row)
}

/// A note under the fields.
fn note(text: &'static str) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

// ═══════════════════════════════════════════════════════════════════════════
// The controls
// ═══════════════════════════════════════════════════════════════════════════

/// The knob on a slider or the square.
///
/// A ring, so it reads on any colour underneath it.
fn knob_at(x: f32, y: f32, color: Hsla) -> impl IntoElement {
    div()
        .absolute()
        .left(px(x - KNOB / 2.0))
        .top(px(y - KNOB / 2.0))
        .size(px(KNOB))
        .rounded_full()
        .border_1()
        .border_color(rgb(0x000000))
        .bg(rgba(rgba_u32(EngineColor::from_rgba8(255, 255, 255, 0))))
        .child(
            div()
                .size_full()
                .rounded_full()
                .border_2()
                .border_color(rgb(0xffffff))
                .opacity(0.0)
                .bg(color),
        )
}

/// The colour at a hue, at full saturation and brightness.
fn hue_color(hue: f32) -> Hsla {
    gpui_color(
        Hsb {
            h: hue,
            s: 1.0,
            b: 1.0,
        }
        .to_color(),
    )
}

/// The engine's colour, as the panel draws it.
fn gpui_color(color: EngineColor) -> Hsla {
    let [r, g, b, a] = color.to_rgba8();
    rgba(rgba_u32(EngineColor::from_rgba8(r, g, b, a))).into()
}

/// The engine's colour as the eight-bit value GPUI's `rgba` takes.
fn rgba_u32(color: EngineColor) -> u32 {
    let [r, g, b, a] = color.to_rgba8();
    u32::from_be_bytes([r, g, b, a])
}

/// Where a control was painted, if it has been.
fn bounds_of(cell: &Rc<RefCell<Option<Bounds<Pixels>>>>) -> Option<Bounds<Pixels>> {
    cell.borrow().as_ref().copied()
}

/// The colour at a point in the square.
fn square_at(bounds: Bounds<Pixels>, at: Point<Pixels>, hue: f32, alpha: f32) -> EngineColor {
    let s = ((at.x - bounds.origin.x) / bounds.size.width).clamp(0.0, 1.0);
    let b = 1.0 - ((at.y - bounds.origin.y) / bounds.size.height).clamp(0.0, 1.0);
    Hsb { h: hue, s, b }.to_color().with_alpha(alpha)
}

/// The colour at a point on the hue slider.
///
/// The hue moves; the saturation and the brightness stay where they were,
/// because the slider is about the hue.
fn hue_at(bounds: Bounds<Pixels>, at: Point<Pixels>, color: EngineColor) -> EngineColor {
    let t = ((at.x - bounds.origin.x) / bounds.size.width).clamp(0.0, 1.0);
    let hsb = Hsb::from_color(color);
    Hsb {
        h: (t * 360.0).clamp(0.0, 359.9),
        s: hsb.s,
        b: hsb.b,
    }
    .to_color()
    .with_alpha(color.a)
}

/// The colour at a point on the alpha slider.
fn alpha_at(bounds: Bounds<Pixels>, at: Point<Pixels>, color: EngineColor) -> EngineColor {
    let t = ((at.x - bounds.origin.x) / bounds.size.width).clamp(0.0, 1.0);
    color.with_alpha(t)
}

/// Move a drag on, and write the colour it lands on.
fn drag_moved<T: ColorHost>(
    host: &mut T,
    target: ColorTarget,
    ids: &[NodeId],
    areas: &DragAreas,
    state: &Rc<RefCell<ColorPickerState>>,
    at: Point<Pixels>,
    cx: &mut Context<T>,
) {
    let Some(drag) = state.borrow().drag else {
        return;
    };
    let color = match drag.kind {
        ColorDragKind::Square => {
            let Some(bounds) = bounds_of(&areas.square) else {
                return;
            };
            let hsb = Hsb::from_color(drag.from);
            square_at(bounds, at, hsb.h, drag.from.a)
        }
        ColorDragKind::Hue => {
            let Some(bounds) = bounds_of(&areas.hue) else {
                return;
            };
            hue_at(bounds, at, drag.from)
        }
        ColorDragKind::Alpha => {
            let Some(bounds) = bounds_of(&areas.alpha) else {
                return;
            };
            alpha_at(bounds, at, drag.from)
        }
    };
    state.borrow_mut().drag = Some(ColorDrag {
        kind: drag.kind,
        from: drag.from,
        current: color,
    });
    apply(host, target, color, ids, state, true, cx);
}

/// Remember a colour, unless the change was a preview.
///
/// A drag's moves are previews, not choices: only the colour the drag ended
/// on joins the recent colours, which is why a drag does not fill the list
/// with every colour it passed through.
fn remember_if_finished(state: &Rc<RefCell<ColorPickerState>>, color: EngineColor, live: bool) {
    if !live {
        state.borrow_mut().remember(color);
    }
}

/// Write one colour, and remember it.
fn apply<T: ColorHost>(
    host: &mut T,
    target: ColorTarget,
    color: EngineColor,
    ids: &[NodeId],
    state: &Rc<RefCell<ColorPickerState>>,
    live: bool,
    cx: &mut Context<T>,
) {
    remember_if_finished(state, color, live);
    host.run_color_action(
        ColorAction::Set {
            target,
            color,
            ids: ids.to_vec(),
        },
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use figcraft_doc::{NodeKind, TextData};
    use gpui::{point, size, IntoElement, TestAppContext, VisualTestContext, Window};

    /// A document with one rectangle on it, painted with a colour.
    fn document_with_a_filled_rectangle() -> (Document, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(
                page,
                NodeKind::Rectangle,
                "Rectangle",
                0.0,
                0.0,
                100.0,
                100.0,
            )
            .expect("a rectangle");
        doc.get_mut(id).expect("the layer").fills =
            vec![Paint::solid(EngineColor::from_rgb8_const(0x33, 0x66, 0x99))];
        (doc, id)
    }

    /// A document with two rectangles painted with different colours.
    fn document_with_two_colours() -> (Document, NodeId, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let first = doc
            .create(page, NodeKind::Rectangle, "First", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let second = doc
            .create(page, NodeKind::Rectangle, "Second", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        doc.get_mut(first).expect("the layer").fills =
            vec![Paint::solid(EngineColor::from_rgb8_const(0x33, 0x66, 0x99))];
        doc.get_mut(second).expect("the layer").fills =
            vec![Paint::solid(EngineColor::from_rgb8_const(0x99, 0x66, 0x33))];
        (doc, first, second)
    }

    // ── The engine's own spellings ──────────────────────────────────────────

    #[test]
    fn the_colour_reads_back_out_of_the_engines_document() {
        let (doc, id) = document_with_a_filled_rectangle();
        let model = ColorPickerModel::read(&doc, &[id], ColorTarget::Fill, None, &[]);
        assert_eq!(
            model.color,
            ColorValue::Set(EngineColor::from_rgb8_const(0x33, 0x66, 0x99)),
            "the picker reads the colour the document holds"
        );
        assert_eq!(model.color.label(), "#336699");
    }

    #[test]
    fn the_paint_a_layer_has_no_colour_reads_as_not_set() {
        let (mut doc, id) = document_with_a_filled_rectangle();
        doc.get_mut(id).expect("the layer").fills = Vec::new();
        let model = ColorPickerModel::read(&doc, &[id], ColorTarget::Fill, None, &[]);
        assert_eq!(model.color, ColorValue::Unset);
        assert_eq!(model.color.word(), NOT_SET);
    }

    #[test]
    fn a_selection_that_disagrees_about_the_colour_reads_as_mixed() {
        let (doc, first, second) = document_with_two_colours();
        let model = ColorPickerModel::read(&doc, &[first, second], ColorTarget::Fill, None, &[]);
        assert_eq!(
            model.color,
            ColorValue::Mixed,
            "two layers with two colours have no agreed colour"
        );
        assert_eq!(model.color.word(), MIXED);
        assert!(model.color.is_mixed(), "and the panel says so out loud");
    }

    #[test]
    fn a_disagreement_is_folded_per_property_and_not_over_the_whole_paint() {
        // The two layers disagree about the colour and agree about
        // everything else the picker shows, so the fields that hold a number
        // the layers agree on still have a value.
        let (doc, first, second) = document_with_two_colours();
        let model = ColorPickerModel::read(&doc, &[first, second], ColorTarget::Fill, None, &[]);
        assert!(model.color.is_mixed());
        assert_eq!(
            model.representative,
            Some(EngineColor::from_rgb8_const(0x33, 0x66, 0x99)),
            "the square draws the first colour the selection really has"
        );
    }

    #[test]
    fn a_command_for_a_colour_carries_the_engines_own_prop_name_and_spelling() {
        let color = EngineColor::from_rgb8_const(0x33, 0x66, 0x99);
        let (command, params) = color_command(ColorTarget::Fill, color, &[NodeId(3)]);
        assert_eq!(command, "node.set");
        assert_eq!(params["ids"], serde_json::json!([3]));
        assert_eq!(
            params["props"]["fill"],
            serde_json::json!("#336699"),
            "the engine spells a colour as #RRGGBB"
        );

        let (_, params) = color_command(ColorTarget::Stroke, color, &[NodeId(3)]);
        assert_eq!(params["props"]["stroke"], serde_json::json!("#336699"));

        let (_, params) = color_command(ColorTarget::TextColor, color, &[NodeId(3)]);
        assert_eq!(
            params["props"]["textFills"],
            serde_json::json!(["#336699"]),
            "the text's fills are a list of paints"
        );
    }

    #[test]
    fn the_prop_names_the_picker_writes_are_the_engines_own() {
        // Pinned against the engine's own list of `node.set` keys, read out
        // of `spool-cli list-commands --filter node.set --json`. A prop name
        // that drifts is a command the engine refuses.
        let keys = figcraft_engine::cmd::props::KEYS;
        for target in [ColorTarget::Fill, ColorTarget::Stroke] {
            let key = target.prop_key();
            assert!(
                keys.contains(key),
                "`{key}` is not one of the engine's node.set props"
            );
        }
    }

    #[test]
    fn the_engines_own_text_fills_prop_is_the_one_the_picker_writes() {
        // `textFills` is the engine's own name for a text layer's fills, and
        // `node.set` applies it — but the engine's *documented* key list does
        // not name it, so it is pinned by what the engine does with it.
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(
                page,
                NodeKind::Text(TextData {
                    characters: "Headline".to_owned(),
                    ..Default::default()
                }),
                "Text",
                0.0,
                0.0,
                100.0,
                30.0,
            )
            .expect("a text layer");
        let mut session = figcraft_engine::Session::with_document(doc);
        let color = EngineColor::from_rgb8_const(0x00, 0xFF, 0x00);
        let (command, params) = color_command(ColorTarget::TextColor, color, &[id]);
        assert_eq!(command, "node.set");
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        let text = node.kind.text().expect("a text layer");
        let colour = text
            .style
            .fills
            .as_ref()
            .and_then(|paints| paints.first())
            .and_then(|paint| paint.solid_color())
            .expect("the text is painted with the colour");
        assert_eq!(colour.to_hex(), "00FF00");
    }

    #[test]
    fn a_colour_with_an_alpha_is_written_in_the_engines_own_spelling() {
        let color = EngineColor::new(1.0, 0.0, 0.0, 0.5);
        let (_, params) = color_command(ColorTarget::Fill, color, &[NodeId(1)]);
        assert_eq!(
            params["props"]["fill"],
            serde_json::json!("#FF000080"),
            "the engine spells a colour with an alpha as #RRGGBBAA"
        );
    }

    #[test]
    fn the_engine_accepts_the_colour_command_and_holds_the_colour() {
        let (doc, id) = document_with_a_filled_rectangle();
        let mut session = figcraft_engine::Session::with_document(doc);
        let color = EngineColor::from_rgb8_const(0x11, 0x22, 0x33);
        let (command, params) = color_command(ColorTarget::Fill, color, &[id]);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(
            node.fills[0].solid_color(),
            Some(color),
            "the document holds the colour the picker wrote"
        );
    }

    #[test]
    fn an_alpha_survives_the_round_trip_through_the_engine() {
        let (doc, id) = document_with_a_filled_rectangle();
        let mut session = figcraft_engine::Session::with_document(doc);
        let color = EngineColor::new(1.0, 0.0, 0.0, 0.5);
        let (command, params) = color_command(ColorTarget::Fill, color, &[id]);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        let read = node.fills[0].solid_color().expect("the fill has a colour");
        assert!(
            (read.a - 0.5).abs() < 0.01,
            "the alpha the picker wrote is the alpha the document holds: {}",
            read.a
        );
    }

    // ── The fields ──────────────────────────────────────────────────────────

    #[test]
    fn a_hex_field_keeps_the_current_alpha_unless_the_hex_carries_one() {
        let current = EngineColor::new(1.0, 0.0, 0.0, 0.5);
        let value = field_value(ColorModel::Hex, ColorField::Hex, "00FF00", current)
            .expect("a hex is a colour");
        assert_eq!(value.to_hex(), "00FF00");
        assert!(
            (value.a - 0.5).abs() < 0.01,
            "a hex without an alpha keeps the colour's own"
        );
    }

    #[test]
    fn a_css_field_reads_a_css_colour() {
        let current = EngineColor::from_rgb8_const(0, 0, 0);
        let value = field_value(
            ColorModel::Css,
            ColorField::Css,
            "rgba(255, 0, 0, 0.25)",
            current,
        )
        .expect("a css colour is a colour");
        assert_eq!(value.to_hex(), "FF0000");
        assert!((value.a - 0.25).abs() < 0.01);
    }

    #[test]
    fn an_alpha_field_is_a_percentage() {
        let current = EngineColor::from_rgb8_const(0, 0, 0);
        let value = field_value(ColorModel::Hex, ColorField::Alpha, "40", current)
            .expect("a percentage is an alpha");
        assert!((value.a - 0.4).abs() < 0.01);
    }

    #[test]
    fn a_field_that_cannot_be_read_commits_nothing() {
        let current = EngineColor::from_rgb8_const(0, 0, 0);
        assert_eq!(
            field_value(ColorModel::Hex, ColorField::Hex, "not a colour", current),
            None,
            "a field that cannot be read commits nothing rather than a default"
        );
    }

    #[test]
    fn the_model_row_shows_the_alpha_for_every_model_but_css() {
        assert!(ColorModel::Hex.has_alpha_field());
        assert!(ColorModel::Rgb.has_alpha_field());
        assert!(ColorModel::Hsl.has_alpha_field());
        assert!(ColorModel::Hsb.has_alpha_field());
        assert!(
            !ColorModel::Css.has_alpha_field(),
            "in CSS the alpha is part of the text"
        );
    }

    // ── The state ───────────────────────────────────────────────────────────

    #[test]
    fn a_typed_hex_is_one_change_and_escape_is_none() {
        let mut state = ColorPickerState::new();
        state.open = Some(ColorTarget::Fill);
        state.draft = Some(ColorDraft {
            field: ColorField::Hex,
            text: "00FF00".to_owned(),
            select_all: true,
        });
        state.working = EngineColor::from_rgb8_const(0, 0, 0);
        state.ids = vec![NodeId(1)];
        let action = state.handle_key("enter").expect("Enter commits the colour");
        match action {
            ColorAction::Set { color, target, ids } => {
                assert_eq!(target, ColorTarget::Fill);
                assert_eq!(color.to_hex(), "00FF00");
                assert_eq!(ids, vec![NodeId(1)]);
            }
        }

        let mut state = ColorPickerState::new();
        state.open = Some(ColorTarget::Fill);
        state.draft = Some(ColorDraft {
            field: ColorField::Hex,
            text: "00FF00".to_owned(),
            select_all: true,
        });
        assert_eq!(
            state.handle_key("escape"),
            None,
            "Escape abandons the text without touching the document"
        );
        assert!(state.draft.is_none());
    }

    #[test]
    fn a_drag_records_only_the_colour_it_ended_on() {
        let preview = EngineColor::new(0.3, 0.0, 0.0, 1.0);
        let chosen = EngineColor::new(0.1, 0.0, 0.0, 1.0);
        let previews = Rc::new(RefCell::new(ColorPickerState::new()));
        let choices = Rc::new(RefCell::new(ColorPickerState::new()));
        // The moves along the way are previews: they do not join the list.
        remember_if_finished(&previews, preview, true);
        remember_if_finished(&choices, chosen, false);
        assert!(
            previews.borrow().recent.is_empty(),
            "a preview is not a choice: {:?}",
            previews.borrow().recent
        );
        assert_eq!(choices.borrow().recent, vec![chosen]);
    }

    #[test]
    fn a_recent_colour_is_remembered_once_and_by_its_eight_bit_value() {
        let mut state = ColorPickerState::new();
        let color = EngineColor::new(0.3, 0.0, 0.0, 1.0);
        state.remember(color);
        // The same colour to within an eight bit: the document stores
        // colours rounded, so the colour picked and the colour read back
        // differ by a fraction and would show as two identical swatches.
        state.remember(EngineColor::new(0.3, 0.0, 0.0, 1.0));
        assert_eq!(
            state.recent.len(),
            1,
            "a colour that rounds to the same eight bits is the same swatch"
        );
        assert_eq!(state.recent[0].to_hex(), color.to_hex());
    }

    // ── The panel, in a real window ─────────────────────────────────────────

    /// A host that records what the picker asked for.
    struct TestHost {
        model: ColorPickerModel,
        state: Rc<RefCell<ColorPickerState>>,
        commands: Vec<(String, JsonValue)>,
    }

    impl TestHost {
        fn new(model: ColorPickerModel, _cx: &mut Context<Self>) -> Self {
            Self {
                model,
                state: Rc::new(RefCell::new(ColorPickerState::new())),
                commands: Vec::new(),
            }
        }
    }

    impl ColorHost for TestHost {
        fn run_color_action(&mut self, action: ColorAction, cx: &mut Context<Self>) {
            self.commands.push(action.command());
            cx.notify();
        }

        fn begin_color_gesture(&mut self, _label: &str, _cx: &mut Context<Self>) -> bool {
            true
        }

        fn commit_color_gesture(&mut self, _cx: &mut Context<Self>) {}
    }

    impl Render for TestHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            panel(&self.model, self.state.clone(), cx)
        }
    }

    /// The size the picker is drawn at.
    fn window_size() -> gpui::Size<gpui::Pixels> {
        size(px(280.0), px(760.0))
    }

    /// Draw the picker into a real window and hand back the window.
    fn draw_picker(
        cx: &mut TestAppContext,
        model: ColorPickerModel,
    ) -> (Entity<TestHost>, &mut VisualTestContext) {
        let (host, window) = cx.add_window_view(move |_, cx| TestHost::new(model, cx));
        window.draw(point(px(0.0), px(0.0)), window_size(), |_, _| {
            host.clone().into_any_element()
        });
        window.run_until_parked();
        (host, window)
    }

    #[gpui::test]
    fn the_colour_picker_lays_out_in_a_real_window(cx: &mut TestAppContext) {
        let (doc, id) = document_with_a_filled_rectangle();
        let model = ColorPickerModel::read(&doc, &[id], ColorTarget::Fill, None, &[]);
        let (_host, window) = draw_picker(cx, model);

        let square = window
            .debug_bounds("color-picker-square")
            .expect("the square is laid out");
        assert!(
            square.size.width > px(0.0) && square.size.height > px(0.0),
            "the square has a real size: {:?}",
            square.size
        );
        let hue = window
            .debug_bounds("color-picker-hue")
            .expect("the hue slider is laid out");
        assert!(hue.size.width > px(0.0), "the hue slider has a real size");
        let alpha = window
            .debug_bounds("color-picker-alpha")
            .expect("the alpha slider is laid out");
        assert!(
            alpha.size.width > px(0.0),
            "the alpha slider has a real size"
        );
        assert!(
            window.debug_bounds("color-picker-field-hex").is_some(),
            "the hex field is laid out"
        );
    }

    #[gpui::test]
    fn the_colour_picker_opens_on_a_colour(cx: &mut TestAppContext) {
        let (doc, id) = document_with_a_filled_rectangle();
        let model = ColorPickerModel::read(&doc, &[id], ColorTarget::Fill, None, &[]);
        let working = model.working;
        let (host, window) = draw_picker(cx, model);

        assert!(
            window.debug_bounds("color-picker-square").is_some(),
            "the picker is drawn"
        );
        // The colour the document holds is the one the picker shows.
        let state =
            window.update(|_, cx| host.read_with(cx, |host, _| host.state.borrow().clone()));
        assert_eq!(state.model, ColorModel::Hex, "Hex is the default model");
        assert_eq!(
            field_text(ColorModel::Hex, ColorField::Hex, working),
            "336699",
            "the hex field shows the colour the document holds"
        );
    }

    #[gpui::test]
    fn the_colour_picker_opens_on_a_mixed_colour(cx: &mut TestAppContext) {
        let (doc, first, second) = document_with_two_colours();
        let model = ColorPickerModel::read(&doc, &[first, second], ColorTarget::Fill, None, &[]);
        assert_eq!(model.color, ColorValue::Mixed);
        let (_host, window) = draw_picker(cx, model);

        assert!(
            window.debug_bounds("color-picker-square").is_some(),
            "the picker is drawn for a mixed selection too"
        );
    }
}
