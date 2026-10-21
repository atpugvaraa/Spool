//! The typography section of the Design panel, ported from figcraft's egui frontend.
//!
//! # What this module owns
//!
//! Two things, in one file because they are one thing:
//!
//! 1. **The model.** [`TypographyModel`] reads the engine's [`Document`] and turns
//!    the selected text layers into what the panel shows, and
//!    [`typography_command`] turns a change into the engine's own command.
//! 2. **The panel.** [`section`] draws that model and asks the engine for the
//!    changes the user makes.
//!
//! # The reference
//!
//! figcraft's typography panel — `crates/ui-egui/src/typography.rs` — is the
//! specification. Its rows, their order, their labels, its defaults, its empty
//! state and its enable/disable behaviour are what this port reproduces. The
//! engine's own spellings are the contract: every value the panel writes goes
//! out as a `node.set` prop (or a `text.*` command) with the name and the value
//! spelling `vendor/figcraft/crates/engine/src/cmd/props.rs` documents.
//!
//! # The three states, per property
//!
//! A value, a disagreement, and an absence. [`TypographyValue`] is the fold, and
//! it is folded **per property**: one differing font size must not hide the line
//! height, the letter spacing and the alignment, which the selection still
//! agrees about. "Mixed" is the word for a disagreement and "Not set" the word
//! for an absence — figcraft's words.
//!
//! figcraft folds the whole style struct at once, so one difference hides the
//! entire block. Spool's Inspector shows three states for every property, and
//! the model here follows it.
//!
//! # The empty state
//!
//! figcraft draws **nothing at all** when the selection has no text layer in
//! it. This port does the same: [`TypographyModel::has_text`] is false and
//! [`section`] returns an empty element. The Inspector offers the section only
//! when there is text, so an empty section is never a thing the user sees.
//!
//! # Commands, not mutations
//!
//! Every change leaves through the engine's command surface: `node.set`,
//! `text.style`, `text.listStyle`, `text.maxLines`, `text.setFeatures`,
//! `text.setVariations`, `text.onPath`, `text.pathOptions` and
//! `text.removeFromPath`. Nothing here writes to a [`Document`] directly, so an
//! edit made from this panel, from the CLI and from an agent are the same edit
//! and land in the same history.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use figcraft_doc::color::Color;
use figcraft_doc::{
    DecorationStyle, Document, LeadingTrim, LetterSpacing, LineHeight, ListStyle, NodeId, NodeKind,
    TextAlignH, TextAlignV, TextAutoResize, TextCase, TextData, TextDecoration, TextStyle,
};
use gpui::{
    div, prelude::*, px, rgb, AnyElement, Context, Div, FontWeight, IntoElement, SharedString,
};
use serde_json::{Map, Value as JsonValue};

use crate::theme;

// ═══════════════════════════════════════════════════════════════════════════
// The words
// ═══════════════════════════════════════════════════════════════════════════

/// The word for a property the selected layers disagree about.
///
/// figcraft's word, and the Inspector's: a value and a nothing would leave a
/// panel free to invent a third thing at every row that faced a disagreement.
pub const MIXED: &str = "Mixed";

/// The word for a property nobody has set.
///
/// figcraft's word, not Spool's: a property that is not there is "Not set",
/// which says that there is nothing to read rather than that the answer is
/// zero.
pub const NOT_SET: &str = "Not set";

/// The word for a value the engine makes up rather than the user typing.
///
/// The engine's own word: a line height of "auto" is a value the engine
/// resolves, and saying "auto" says so.
pub const AUTO: &str = "Auto";

/// Why a paragraph indent cannot be set on this text.
///
/// figcraft's own reason: the indent hangs off the aligned edge, and a centred
/// or justified line has no such edge.
pub const NO_INDENT: &str = "Paragraph indent applies to left-aligned text";

/// Why the decoration's colour is not set.
///
/// The decoration has no colour of its own, so it takes the text's.
pub const INHERITED_COLOUR: &str = "Takes the text's colour";

// ═══════════════════════════════════════════════════════════════════════════
// The three states
// ═══════════════════════════════════════════════════════════════════════════

/// One property of the selection's typography, in one of three states.
///
/// Three states, never two: a value every selected layer agrees on, no value
/// at all, and a disagreement. The Inspector's own properties have the same
/// three and the same words, so "Mixed" means one thing in the panel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TypographyValue<T> {
    /// Every selected layer agrees on this value.
    Set(T),
    /// No selected layer has this value.
    #[default]
    Unset,
    /// The selected layers disagree.
    Mixed,
}

impl<T: PartialEq> TypographyValue<T> {
    /// Fold a selection's readings into the one state that is honest for all
    /// of them.
    ///
    /// `None` for a layer means "this property is not set on that layer",
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

    /// Whether the selection disagrees about this property.
    pub fn is_mixed(&self) -> bool {
        matches!(self, Self::Mixed)
    }

    /// Whether no selected layer has this value.
    pub fn is_unset(&self) -> bool {
        matches!(self, Self::Unset)
    }

    /// The value, when the selection agrees on one.
    pub fn set(&self) -> Option<&T> {
        match self {
            Self::Set(value) => Some(value),
            _ => None,
        }
    }
}

impl<T> TypographyValue<T> {
    /// The word this value is shown as.
    ///
    /// The value itself, "Not set" for an absence, "Mixed" for a
    /// disagreement. One place, so no row can invent a fourth word.
    pub fn label(&self, label: impl Fn(&T) -> String) -> String {
        match self {
            Self::Set(value) => label(value),
            Self::Unset => NOT_SET.to_owned(),
            Self::Mixed => MIXED.to_owned(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The engine's own value spellings
// ═══════════════════════════════════════════════════════════════════════════
//
// Every one of these is the spelling `vendor/figcraft/crates/engine/src/cmd/props.rs`
// accepts, read from there and not guessed. The engine's `enum_of` accepts
// camelCase, snake_case and spaced variants, but the panel writes the spelling
// the engine's own documentation uses, so a command read back is the command
// that was written.

/// The engine's spelling of a horizontal alignment.
pub fn align_value(align: TextAlignH) -> &'static str {
    match align {
        TextAlignH::Left => "left",
        TextAlignH::Center => "center",
        TextAlignH::Right => "right",
        TextAlignH::Justified => "justified",
    }
}

/// The engine's spelling of a vertical alignment.
pub fn align_vertical_value(align: TextAlignV) -> &'static str {
    match align {
        TextAlignV::Top => "top",
        TextAlignV::Middle => "middle",
        TextAlignV::Bottom => "bottom",
    }
}

/// The engine's spelling of a text box's resizing.
pub fn auto_resize_value(resize: TextAutoResize) -> &'static str {
    match resize {
        TextAutoResize::WidthAndHeight => "widthAndHeight",
        TextAutoResize::Height => "height",
        TextAutoResize::None => "none",
        TextAutoResize::Truncate => "truncate",
    }
}

/// The engine's spelling of a decoration.
pub fn decoration_value(decoration: TextDecoration) -> &'static str {
    match decoration {
        TextDecoration::None => "none",
        TextDecoration::Underline => "underline",
        TextDecoration::Strikethrough => "strikethrough",
    }
}

/// The engine's spelling of a decoration's style.
pub fn decoration_style_value(style: DecorationStyle) -> &'static str {
    match style {
        DecorationStyle::Solid => "solid",
        DecorationStyle::Dotted => "dotted",
        DecorationStyle::Wavy => "wavy",
    }
}

/// The engine's spelling of a letter case.
pub fn case_value(case: TextCase) -> &'static str {
    match case {
        TextCase::Original => "original",
        TextCase::Upper => "upper",
        TextCase::Lower => "lower",
        TextCase::Title => "title",
        TextCase::SmallCaps => "smallCaps",
    }
}

/// The engine's spelling of a list style.
pub fn list_style_value(style: ListStyle) -> &'static str {
    match style {
        ListStyle::None => "none",
        ListStyle::Bulleted => "bulleted",
        ListStyle::Numbered => "numbered",
    }
}

/// The engine's spelling of a vertical trim.
pub fn leading_trim_value(trim: LeadingTrim) -> &'static str {
    match trim {
        LeadingTrim::None => "none",
        LeadingTrim::CapHeight => "capHeight",
    }
}

/// The engine's spelling of a line height: `auto`, a number of pixels, or a
/// percentage of the font size.
pub fn line_height_value(height: LineHeight) -> JsonValue {
    match height {
        LineHeight::Auto => JsonValue::String("auto".to_owned()),
        LineHeight::Pixels(px) => number(px),
        LineHeight::Percent(percent) => {
            JsonValue::String(format!("{}%", figcraft_doc::trim_num(percent)))
        }
    }
}

/// The engine's spelling of a letter spacing: a number of pixels, or a
/// percentage of the font size.
pub fn letter_spacing_value(spacing: LetterSpacing) -> JsonValue {
    match spacing {
        LetterSpacing::Pixels(px) => number(px),
        LetterSpacing::Percent(percent) => {
            JsonValue::String(format!("{}%", figcraft_doc::trim_num(percent)))
        }
    }
}

/// A number, as a command parameter.
fn number(value: f64) -> JsonValue {
    serde_json::Number::from_f64(value)
        .map(JsonValue::Number)
        .unwrap_or(JsonValue::Null)
}

// ═══════════════════════════════════════════════════════════════════════════
// The model
// ═══════════════════════════════════════════════════════════════════════════

/// The characters of the text being edited.
///
/// While a range of a text layer is selected, the character edits apply to
/// that range and the panel reads that range's styles — figcraft's rule, and
/// the engine's: `text.style` and the `text.*` commands all take the range
/// themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRange {
    /// The text layer being edited.
    pub id: NodeId,
    /// The first character of the range.
    pub start: usize,
    /// The character after the last one.
    pub end: usize,
}

/// The OpenType features the panel offers, in figcraft's order.
///
/// The tag, the label, and whether the feature is on by default in fonts. The
/// engine records an override only when the user turns a feature away from the
/// font's default, so a tag the engine does not record shows the default.
pub const FEATURES: [(&str, &str, bool); 12] = [
    ("liga", "Standard ligatures", true),
    ("calt", "Contextual alternates", true),
    ("tnum", "Tabular numbers", false),
    ("frac", "Fractions", false),
    ("case", "Case-sensitive forms", false),
    ("zero", "Slashed zero", false),
    ("ss01", "Stylistic set 1", false),
    ("ss02", "Stylistic set 2", false),
    ("ss03", "Stylistic set 3", false),
    ("ss04", "Stylistic set 4", false),
    ("ss05", "Stylistic set 5", false),
    ("smcp", "Small capitals", false),
];

/// The typography of the selection, as the panel shows it.
///
/// Every property is folded on its own, so a disagreement about one of them
/// says so and leaves the others alone.
#[derive(Clone, Debug, Default)]
pub struct TypographyModel {
    /// The text layers the panel is showing.
    ///
    /// The selection's text layers only: a shape in the selection has no
    /// typography, and figcraft leaves it out of the panel entirely.
    pub ids: Vec<NodeId>,
    /// Whether the selection has any text layer at all.
    ///
    /// False is the empty state: figcraft draws nothing when there is no text,
    /// and so does this.
    pub has_text: bool,
    /// The characters being edited, if a range is selected.
    pub range: Option<TextRange>,
    /// The styles the panel reads: the range's spans, or every span of every
    /// selected text layer.
    pub styles: Vec<TextStyle>,
    /// The text the layers hold.
    pub characters: TypographyValue<String>,
    /// The font family.
    pub font_family: TypographyValue<String>,
    /// The font style: "Regular", "Bold", "Semi Bold Italic"…
    pub font_style: TypographyValue<String>,
    /// The font size, in points.
    pub font_size: TypographyValue<f64>,
    /// The line height: automatic, a number of pixels, or a percentage.
    pub line_height: TypographyValue<LineHeight>,
    /// The letter spacing: a number of pixels, or a percentage.
    pub letter_spacing: TypographyValue<LetterSpacing>,
    /// The space between the paragraphs.
    pub paragraph_spacing: TypographyValue<f64>,
    /// The indent of the paragraphs.
    pub paragraph_indent: TypographyValue<f64>,
    /// The list style of the paragraphs.
    pub list_style: TypographyValue<ListStyle>,
    /// The space between the items of a list.
    pub list_spacing: TypographyValue<f64>,
    /// Whether the punctuation at a line's edge sits outside the box.
    pub hanging_punctuation: TypographyValue<bool>,
    /// Whether the list markers sit outside the box.
    pub hanging_list: TypographyValue<bool>,
    /// What bounds the text box vertically.
    pub leading_trim: TypographyValue<LeadingTrim>,
    /// The number of lines the text is cut to, when truncating.
    pub max_lines: TypographyValue<u32>,
    /// The horizontal alignment.
    pub text_align: TypographyValue<TextAlignH>,
    /// The vertical alignment.
    pub text_align_vertical: TypographyValue<TextAlignV>,
    /// How the text box resizes as its content changes.
    pub text_auto_resize: TypographyValue<TextAutoResize>,
    /// The decoration: none, an underline or a strikethrough.
    pub decoration: TypographyValue<TextDecoration>,
    /// How the decoration is drawn.
    pub decoration_style: TypographyValue<DecorationStyle>,
    /// How wide the decoration is.
    pub decoration_thickness: TypographyValue<LineHeight>,
    /// Where the decoration sits, from the baseline down to its top edge.
    pub decoration_offset: TypographyValue<LineHeight>,
    /// Whether the decoration breaks around descenders.
    pub decoration_skip_ink: TypographyValue<bool>,
    /// The decoration's own colour; `None` when it takes the text's.
    pub decoration_color: TypographyValue<Option<Color>>,
    /// The letter case.
    pub text_case: TypographyValue<TextCase>,
    /// The OpenType feature overrides the engine records, per tag.
    pub features: BTreeMap<String, TypographyValue<bool>>,
    /// The variable font axis overrides the engine records, per tag.
    pub variations: BTreeMap<String, TypographyValue<f64>>,
    /// Whether the text follows a path.
    pub on_path: TypographyValue<bool>,
    /// Where the text starts along the path.
    pub path_start: TypographyValue<f64>,
    /// Whether the text runs on the other side of the path.
    pub path_flip: TypographyValue<bool>,
    /// Whether a text layer and a path layer are both selected, so the text
    /// can be put on the path.
    pub can_put_on_path: bool,
}

impl TypographyModel {
    /// Read the model out of the engine's document.
    ///
    /// The selection's text layers are the panel's subject; a shape in the
    /// selection has no typography and is left out, exactly as figcraft leaves
    /// it out.
    pub fn read(doc: &Document, selection: &[NodeId], range: Option<TextRange>) -> Self {
        let ids: Vec<NodeId> = selection
            .iter()
            .copied()
            .filter(|id| matches!(doc.node(*id).map(|n| &n.kind), Some(NodeKind::Text(_))))
            .collect();
        if ids.is_empty() {
            return Self::default();
        }
        let styles = target_styles(doc, &ids, range);
        let texts: Vec<(NodeId, &TextData)> = ids
            .iter()
            .filter_map(|id| doc.node(*id).and_then(|n| n.kind.text()).map(|t| (*id, t)))
            .collect();

        // The character properties are folded over the styles and the layer
        // properties over the text layers: one property at a time, so a
        // disagreement about one of them says so and leaves the others alone.

        // The OpenType features the engine records an override for. A tag the
        // engine does not record is not set: the font's own default applies.
        let mut features: BTreeMap<String, TypographyValue<bool>> = BTreeMap::new();
        for (tag, _, _) in FEATURES {
            features.insert(
                tag.to_owned(),
                TypographyValue::across(texts.iter().map(|(_, t)| t.features.get(tag).copied())),
            );
        }
        // The variable font axes the engine records an override for.
        let mut axis_tags: BTreeSet<String> = BTreeSet::new();
        for style in &styles {
            axis_tags.extend(style.variations.keys().cloned());
        }
        let mut variations: BTreeMap<String, TypographyValue<f64>> = BTreeMap::new();
        for tag in &axis_tags {
            variations.insert(
                tag.clone(),
                TypographyValue::across(styles.iter().map(|s| s.variations.get(tag).copied())),
            );
        }

        // The text on a path: the path is part of the text layer, so it is
        // read from the layers rather than from the styles.
        let paths: Vec<Option<&figcraft_doc::TextPath>> =
            texts.iter().map(|(_, t)| t.path.as_deref()).collect();
        let on_path = TypographyValue::across(paths.iter().map(|p| Some(p.is_some())));
        let path_start = TypographyValue::across(paths.iter().map(|p| p.map(|p| p.start())));
        let path_flip = TypographyValue::across(paths.iter().map(|p| p.map(|p| p.flip)));

        // A text layer and a path layer, both selected: the text can be put
        // on the path.
        let can_put_on_path = ids.len() == 1
            && selection.len() == 2
            && selection.iter().any(|id| {
                !ids.contains(id)
                    && doc
                        .node(*id)
                        .is_some_and(|n| figcraft_doc::geometry::stroke_geometry(n).is_some())
            });

        Self {
            has_text: true,
            ids,
            range,
            characters: fold_texts(&texts, |_id, t| Some(t.characters.clone())),
            font_family: fold_styles(&styles, |s| Some(s.font_family.clone())),
            font_style: fold_styles(&styles, |s| Some(s.font_style.clone())),
            font_size: fold_styles(&styles, |s| Some(s.font_size)),
            line_height: fold_styles(&styles, |s| Some(s.line_height)),
            letter_spacing: fold_styles(&styles, |s| Some(s.letter_spacing)),
            paragraph_spacing: fold_texts(&texts, |_id, t| Some(t.paragraph_spacing)),
            paragraph_indent: fold_texts(&texts, |_id, t| Some(t.paragraph_indent)),
            list_style: fold_texts(&texts, |id, t| {
                // While a range is selected, the list of the paragraphs it
                // touches is the list of the range; otherwise it is the list of
                // the whole text.
                let (start, end) = match range {
                    Some(r) if r.id == id => (r.start, r.end),
                    _ => (0, t.char_count()),
                };
                t.common_para_list(start, end).map(|p| p.list)
            }),
            list_spacing: fold_texts(&texts, |_id, t| Some(t.list_spacing)),
            hanging_punctuation: fold_texts(&texts, |_id, t| Some(t.hanging_punctuation)),
            hanging_list: fold_texts(&texts, |_id, t| Some(t.hanging_list)),
            leading_trim: fold_texts(&texts, |_id, t| Some(t.leading_trim)),
            max_lines: fold_texts(&texts, |_id, t| Some(t.max_lines)),
            text_align: fold_texts(&texts, |_id, t| Some(t.align_h)),
            text_align_vertical: fold_texts(&texts, |_id, t| Some(t.align_v)),
            text_auto_resize: fold_texts(&texts, |_id, t| Some(t.auto_resize)),
            decoration: fold_styles(&styles, |s| Some(s.decoration)),
            decoration_style: fold_styles(&styles, |s| Some(s.look().style)),
            decoration_thickness: fold_styles(&styles, |s| Some(s.look().thickness)),
            decoration_offset: fold_styles(&styles, |s| Some(s.look().offset)),
            decoration_skip_ink: fold_styles(&styles, |s| Some(s.look().skip_ink)),
            decoration_color: fold_styles(&styles, |s| Some(s.look().color)),
            text_case: fold_styles(&styles, |s| Some(s.case)),
            features,
            variations,
            on_path,
            path_start,
            path_flip,
            can_put_on_path,
            styles,
        }
    }

    /// Read the model out of an engine session.
    ///
    /// The session's own selection, and the range of the text being edited if
    /// there is one — the engine's own answer, not a flag kept on the side.
    pub fn from_session(session: &figcraft_engine::Session) -> Self {
        let Ok(doc) = session.doc() else {
            return Self::default();
        };
        let range = figcraft_engine::textedit::selected_range(session)
            .map(|(id, start, end)| TextRange { id, start, end });
        Self::read(doc, &session.selection(), range)
    }

    /// Whether the panel has anything to show.
    ///
    /// False is the empty state: the selection has no text layer in it, and
    /// figcraft draws nothing at all in that case.
    pub fn is_empty(&self) -> bool {
        !self.has_text
    }

    /// The layers a change applies to, and the characters of it, if any.
    pub fn target(&self) -> TypographyTarget {
        TypographyTarget {
            ids: self.ids.clone(),
            range: self.range,
        }
    }
}

/// Fold the character styles' readings of one property.
fn fold_styles<T: PartialEq>(
    styles: &[TextStyle],
    read: impl Fn(&TextStyle) -> Option<T>,
) -> TypographyValue<T> {
    TypographyValue::across(styles.iter().map(read))
}

/// Fold the text layers' readings of one property.
fn fold_texts<T: PartialEq>(
    texts: &[(NodeId, &TextData)],
    read: impl Fn(NodeId, &TextData) -> Option<T>,
) -> TypographyValue<T> {
    TypographyValue::across(texts.iter().map(|(id, t)| read(*id, t)))
}

/// The character styles the panel shows: the edited range's spans, else every
/// span of every selected text layer.
fn target_styles(doc: &Document, ids: &[NodeId], range: Option<TextRange>) -> Vec<TextStyle> {
    let mut out: Vec<TextStyle> = Vec::new();
    let mut push = |style: &TextStyle| {
        if !out.contains(style) {
            out.push(style.clone());
        }
    };
    for id in ids {
        let Some(text) = doc.node(*id).and_then(|n| n.kind.text()) else {
            continue;
        };
        match range {
            Some(r) if r.id == *id && r.start < r.end => {
                for (start, end, style) in text.spans() {
                    if start < r.end && end > r.start {
                        push(style);
                    }
                }
            }
            _ => {
                for (_, _, style) in text.spans() {
                    push(style);
                }
            }
        }
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// The edits
// ═══════════════════════════════════════════════════════════════════════════

/// One change to the selection's typography.
///
/// One variant per thing the panel can change, so a change the engine has no
/// command for cannot be written down as though it had one.
#[derive(Clone, Debug, PartialEq)]
pub enum TypographyEdit {
    /// The font family.
    FontFamily(String),
    /// The font style.
    FontStyle(String),
    /// The font size.
    FontSize(f64),
    /// The line height.
    LineHeight(LineHeight),
    /// The letter spacing.
    LetterSpacing(LetterSpacing),
    /// The space between the paragraphs.
    ParagraphSpacing(f64),
    /// The indent of the paragraphs.
    ParagraphIndent(f64),
    /// The list style of the paragraphs.
    ListStyle(ListStyle),
    /// The space between the items of a list.
    ListSpacing(f64),
    /// Whether the punctuation at a line's edge sits outside the box.
    HangingPunctuation(bool),
    /// Whether the list markers sit outside the box.
    HangingList(bool),
    /// What bounds the text box vertically.
    LeadingTrim(LeadingTrim),
    /// The number of lines the text is cut to.
    MaxLines(u32),
    /// The horizontal alignment.
    TextAlign(TextAlignH),
    /// The vertical alignment.
    TextAlignVertical(TextAlignV),
    /// How the text box resizes as its content changes.
    TextAutoResize(TextAutoResize),
    /// The decoration.
    TextDecoration(TextDecoration),
    /// How the decoration is drawn.
    TextDecorationStyle(DecorationStyle),
    /// How wide the decoration is.
    TextDecorationThickness(LineHeight),
    /// Where the decoration sits.
    TextDecorationOffset(LineHeight),
    /// Whether the decoration breaks around descenders.
    TextDecorationSkipInk(bool),
    /// The decoration's own colour; `None` takes the text's.
    TextDecorationColor(Option<Color>),
    /// The letter case.
    TextCase(TextCase),
    /// One OpenType feature; `None` is the font's own default.
    Feature {
        /// The four-character OpenType tag.
        tag: String,
        /// Whether the feature is on; `None` goes back to the font's default.
        on: Option<bool>,
    },
    /// One variable font axis; `None` goes back to the style's value.
    Variation {
        /// The four-character OpenType axis tag.
        tag: String,
        /// The axis value; `None` goes back to the style's value.
        value: Option<f64>,
    },
    /// Where the text starts along the path, from 0 to 1.
    PathStart(f64),
    /// Whether the text runs on the other side of the path.
    PathFlip(bool),
    /// Take the text off the path.
    RemoveFromPath,
    /// Put the text on the path.
    PutOnPath,
}

impl TypographyEdit {
    /// The `node.set` key and value this edit writes, when it writes one.
    ///
    /// `None` for the edits that go out as a `text.*` command instead: the
    /// list style, the max lines, the OpenType features, the variable axes and
    /// the text on a path all have commands of their own, and the engine's own
    /// commands do more than the bare prop would (a max line count turns the
    /// text into a truncating one; a feature toggle merges rather than
    /// replacing the map).
    pub fn prop(&self) -> Option<(&'static str, JsonValue)> {
        Some(match self {
            Self::FontFamily(family) => ("fontFamily", JsonValue::String(family.clone())),
            Self::FontStyle(style) => ("fontStyle", JsonValue::String(style.clone())),
            Self::FontSize(size) => ("fontSize", number(*size)),
            Self::LineHeight(height) => ("lineHeight", line_height_value(*height)),
            Self::LetterSpacing(spacing) => ("letterSpacing", letter_spacing_value(*spacing)),
            Self::ParagraphSpacing(spacing) => ("paragraphSpacing", number(*spacing)),
            Self::ParagraphIndent(indent) => ("paragraphIndent", number(*indent)),
            Self::ListSpacing(spacing) => ("listSpacing", number(*spacing)),
            Self::HangingPunctuation(on) => ("hangingPunctuation", JsonValue::Bool(*on)),
            Self::HangingList(on) => ("hangingList", JsonValue::Bool(*on)),
            Self::LeadingTrim(trim) => (
                "leadingTrim",
                JsonValue::String(leading_trim_value(*trim).to_owned()),
            ),
            Self::TextAlign(align) => (
                "textAlign",
                JsonValue::String(align_value(*align).to_owned()),
            ),
            Self::TextAlignVertical(align) => (
                "textAlignVertical",
                JsonValue::String(align_vertical_value(*align).to_owned()),
            ),
            Self::TextAutoResize(resize) => (
                "textAutoResize",
                JsonValue::String(auto_resize_value(*resize).to_owned()),
            ),
            Self::TextDecoration(decoration) => (
                "textDecoration",
                JsonValue::String(decoration_value(*decoration).to_owned()),
            ),
            Self::TextDecorationStyle(style) => (
                "textDecorationStyle",
                JsonValue::String(decoration_style_value(*style).to_owned()),
            ),
            Self::TextDecorationThickness(thickness) => {
                ("textDecorationThickness", line_height_value(*thickness))
            }
            Self::TextDecorationOffset(offset) => {
                ("textDecorationOffset", line_height_value(*offset))
            }
            Self::TextDecorationSkipInk(skip) => ("textDecorationSkipInk", JsonValue::Bool(*skip)),
            Self::TextDecorationColor(color) => (
                "textDecorationColor",
                match color {
                    Some(color) => JsonValue::String(format!("#{}", color.to_hex())),
                    None => JsonValue::String("auto".to_owned()),
                },
            ),
            Self::TextCase(case) => ("textCase", JsonValue::String(case_value(*case).to_owned())),
            Self::ListStyle(_)
            | Self::MaxLines(_)
            | Self::Feature { .. }
            | Self::Variation { .. }
            | Self::PathStart(_)
            | Self::PathFlip(_)
            | Self::RemoveFromPath
            | Self::PutOnPath => return None,
        })
    }

    /// Whether this edit is about the *characters* rather than the layer.
    ///
    /// A character edit applies to the selected range while a range is
    /// selected; a layer edit is about the text layer as a whole.
    pub fn is_character_edit(&self) -> bool {
        matches!(
            self,
            Self::FontFamily(_)
                | Self::FontStyle(_)
                | Self::FontSize(_)
                | Self::LineHeight(_)
                | Self::LetterSpacing(_)
                | Self::TextDecoration(_)
                | Self::TextDecorationStyle(_)
                | Self::TextDecorationThickness(_)
                | Self::TextDecorationOffset(_)
                | Self::TextDecorationSkipInk(_)
                | Self::TextDecorationColor(_)
                | Self::TextCase(_)
        )
    }
}

/// The layers a change applies to, and the characters of it, if any.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypographyTarget {
    /// The layers the change applies to.
    pub ids: Vec<NodeId>,
    /// The characters of the text being edited, if a range is selected.
    pub range: Option<TextRange>,
}

/// The command that makes one change to the selection's typography.
///
/// The engine's own commands, with the engine's own parameters:
///
/// - `node.set` takes `ids` and `props`, and the props are the text props
///   `vendor/figcraft/crates/engine/src/cmd/props.rs` documents.
/// - `text.style` takes `id`, `start`, `end` and the character props, and is
///   what a range edit goes through.
/// - `text.listStyle` takes `ids` and `style`.
/// - `text.maxLines` takes `ids` and `lines`.
/// - `text.setFeatures` takes `ids` and `features`.
/// - `text.setVariations` takes `ids` and `variations`.
/// - `text.pathOptions` takes `ids` and `start` or `flip`.
/// - `text.removeFromPath` and `text.onPath` take `ids`.
pub fn typography_command(edit: &TypographyEdit, target: &TypographyTarget) -> (String, JsonValue) {
    let ids = id_list(&target.ids);
    match edit {
        TypographyEdit::ListStyle(style) => (
            "text.listStyle".to_owned(),
            params(vec![
                ("ids", ids),
                (
                    "style",
                    JsonValue::String(list_style_value(*style).to_owned()),
                ),
            ]),
        ),
        TypographyEdit::MaxLines(lines) => (
            "text.maxLines".to_owned(),
            params(vec![("ids", ids), ("lines", number(f64::from(*lines)))]),
        ),
        TypographyEdit::Feature { tag, on } => {
            let mut features = Map::new();
            features.insert(
                tag.clone(),
                match on {
                    Some(on) => JsonValue::Bool(*on),
                    None => JsonValue::Null,
                },
            );
            (
                "text.setFeatures".to_owned(),
                params(vec![
                    ("ids", ids),
                    ("features", JsonValue::Object(features)),
                ]),
            )
        }
        TypographyEdit::Variation { tag, value } => {
            let mut variations = Map::new();
            variations.insert(
                tag.clone(),
                match value {
                    Some(value) => number(*value),
                    None => JsonValue::Null,
                },
            );
            (
                "text.setVariations".to_owned(),
                params(vec![
                    ("ids", ids),
                    ("variations", JsonValue::Object(variations)),
                ]),
            )
        }
        TypographyEdit::PathStart(start) => (
            "text.pathOptions".to_owned(),
            params(vec![("ids", ids), ("start", number(*start))]),
        ),
        TypographyEdit::PathFlip(flip) => (
            "text.pathOptions".to_owned(),
            params(vec![("ids", ids), ("flip", JsonValue::Bool(*flip))]),
        ),
        TypographyEdit::RemoveFromPath => {
            ("text.removeFromPath".to_owned(), params(vec![("ids", ids)]))
        }
        TypographyEdit::PutOnPath => ("text.onPath".to_owned(), params(vec![("ids", ids)])),
        _ => {
            let Some((key, value)) = edit.prop() else {
                return ("node.set".to_owned(), JsonValue::Null);
            };
            // A character edit while a range is selected goes through
            // `text.style`, which takes the range and the characters' props.
            match target.range.filter(|_| edit.is_character_edit()) {
                Some(range) => {
                    let mut props = Map::new();
                    props.insert("id".to_owned(), JsonValue::from(range.id.0));
                    props.insert("start".to_owned(), JsonValue::from(range.start));
                    props.insert("end".to_owned(), JsonValue::from(range.end));
                    props.insert(key.to_owned(), value);
                    ("text.style".to_owned(), JsonValue::Object(props))
                }
                None => {
                    let mut props = Map::new();
                    props.insert(key.to_owned(), value);
                    (
                        "node.set".to_owned(),
                        params(vec![("ids", ids), ("props", JsonValue::Object(props))]),
                    )
                }
            }
        }
    }
}

/// One change the panel asks the engine to make.
#[derive(Clone, Debug, PartialEq)]
pub enum TypographyAction {
    /// One change to the selection's typography.
    Edit {
        /// The change.
        edit: TypographyEdit,
        /// The layers it applies to.
        target: TypographyTarget,
    },
}

impl TypographyAction {
    /// The command that makes this change.
    pub fn command(&self) -> (String, JsonValue) {
        match self {
            Self::Edit { edit, target } => typography_command(edit, target),
        }
    }
}

/// A JSON object, built one entry at a time.
fn params(entries: Vec<(&str, JsonValue)>) -> JsonValue {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_owned(), value);
    }
    JsonValue::Object(map)
}

/// The `ids` array a command takes.
fn id_list(ids: &[NodeId]) -> JsonValue {
    JsonValue::Array(ids.iter().map(|id| JsonValue::from(id.0)).collect())
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel's own state
// ═══════════════════════════════════════════════════════════════════════════

/// Which menu of the panel is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypographyMenu {
    /// The list of font families.
    FontFamily,
    /// The list of the family's styles.
    FontStyle,
    /// The Type settings.
    Settings,
    /// The variable font's named instances.
    Instance,
}

/// Which tab of the Type settings is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TypographyTab {
    /// Decoration, case, lists, indentation, truncation.
    #[default]
    Basics,
    /// The OpenType features.
    Details,
    /// The variable font's axes.
    Variable,
}

/// Which field is being typed into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypographyField {
    /// The line height.
    LineHeight,
    /// The letter spacing.
    LetterSpacing,
    /// The space between the paragraphs.
    ParagraphSpacing,
    /// The indent of the paragraphs.
    ParagraphIndent,
    /// The space between the items of a list.
    ListSpacing,
    /// How wide the decoration is.
    DecorationThickness,
    /// Where the decoration sits.
    DecorationOffset,
    /// The number of lines the text is cut to.
    MaxLines,
    /// Where the text starts along the path.
    PathStart,
}

/// The unit a spacing field's current value uses.
///
/// The engine's own units: a number of pixels, a percentage of the font size,
/// or the automatic value. A bare number typed into a field keeps the unit the
/// current value has, which is what figcraft does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpacingUnit {
    /// A number of pixels.
    Pixels,
    /// A percentage of the font size.
    Percent,
    /// The engine's automatic value.
    Auto,
}

/// The text being typed into one field.
///
/// A buffer, not a second copy of the property: the value becomes real only
/// when Enter hands it to the engine as one semantic operation. The text
/// arrives selected, so the first keystroke replaces it rather than appending
/// to it.
#[derive(Clone, Debug, PartialEq)]
pub struct TypographyDraft {
    /// Which field is being typed into.
    pub field: TypographyField,
    /// The text as typed.
    pub text: String,
    /// Whether the text is selected, so the next keystroke replaces it.
    pub select_all: bool,
    /// The layers the change applies to.
    pub target: TypographyTarget,
    /// The unit the current value uses, so a bare number keeps it.
    pub unit: Option<SpacingUnit>,
}

/// The panel's own state.
///
/// The state is a cell the surface that hosts the panel owns, so a menu stays
/// open across repaints while the panel itself stays a function.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TypographyPanelState {
    /// Which menu is open, if any.
    pub open: Option<TypographyMenu>,
    /// The text being typed, if any.
    pub draft: Option<TypographyDraft>,
    /// The families the user picked recently, most recent first.
    pub recent_families: Vec<String>,
    /// Which tab of the Type settings is open.
    pub tab: TypographyTab,
}

impl TypographyPanelState {
    /// The state a section starts in.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the section is part-way through something.
    pub fn is_busy(&self) -> bool {
        self.draft.is_some()
    }

    /// Give up whatever the section was part-way through.
    ///
    /// Returns whether anything was given up, so the editor's Escape ladder
    /// knows the key was consumed.
    pub fn cancel(&mut self) -> bool {
        self.draft.take().is_some()
    }

    /// Handle one keystroke.
    ///
    /// Returns the change the key asks for, if the key was the section's own.
    /// Escape is not claimed: it belongs to the editor's ladder, which walks
    /// down from the topmost thing.
    pub fn handle_key(&mut self, key: &str) -> Option<TypographyAction> {
        let draft = self.draft.as_mut()?;
        match key {
            "enter" | "return" => {
                let draft = self.draft.take()?;
                commit_draft(&draft)
            }
            "escape" => {
                self.draft.take();
                None
            }
            "backspace" => {
                // With the text selected, Backspace deletes the selection —
                // the same rule typing follows.
                if draft.select_all {
                    draft.text.clear();
                    draft.select_all = false;
                } else {
                    draft.text.pop();
                }
                None
            }
            // Arrows are not characters. The section has no nudge to make, so
            // the key is swallowed rather than typed into the field.
            "up" | "down" | "left" | "right" => None,
            other if other.chars().count() == 1 => {
                if draft.select_all {
                    draft.text.clear();
                    draft.select_all = false;
                }
                draft.text.push_str(other);
                None
            }
            // A modifier combination is an editor shortcut, not a character.
            _ => None,
        }
    }
}

/// The change a typed value asks for, if it is one the engine accepts.
///
/// `None` for a value the engine would refuse: text that is not a number
/// leaves the property exactly as it was.
fn commit_draft(draft: &TypographyDraft) -> Option<TypographyAction> {
    let edit = match draft.field {
        TypographyField::LineHeight => {
            TypographyEdit::LineHeight(parse_line_height(&draft.text, draft.unit)?)
        }
        TypographyField::LetterSpacing => {
            TypographyEdit::LetterSpacing(parse_letter_spacing(&draft.text, draft.unit)?)
        }
        TypographyField::ParagraphSpacing => {
            TypographyEdit::ParagraphSpacing(parse_number(&draft.text)?)
        }
        TypographyField::ParagraphIndent => {
            TypographyEdit::ParagraphIndent(parse_number(&draft.text)?)
        }
        TypographyField::ListSpacing => TypographyEdit::ListSpacing(parse_number(&draft.text)?),
        TypographyField::DecorationThickness => {
            TypographyEdit::TextDecorationThickness(parse_decoration_length(&draft.text)?)
        }
        TypographyField::DecorationOffset => {
            TypographyEdit::TextDecorationOffset(parse_decoration_length(&draft.text)?)
        }
        TypographyField::MaxLines => {
            let lines = parse_number(&draft.text)?;
            if lines < 0.0 {
                return None;
            }
            TypographyEdit::MaxLines(lines.round() as u32)
        }
        TypographyField::PathStart => {
            let percent = parse_number(&draft.text)?;
            if !(0.0..=100.0).contains(&percent) {
                return None;
            }
            TypographyEdit::PathStart(percent / 100.0)
        }
    };
    Some(TypographyAction::Edit {
        edit,
        target: draft.target.clone(),
    })
}

/// A number the user typed.
fn parse_number(text: &str) -> Option<f64> {
    let value: f64 = text.trim().parse().ok()?;
    if value.is_finite() {
        Some(value.max(0.0))
    } else {
        None
    }
}

/// "Auto", "24", "150%" → the engine's line height.
///
/// A bare number keeps the current unit when it is a percentage, else means
/// pixels. Empty text means Auto, except on a field with no current value
/// (`unit` is `None`), where nothing typed is an untouched field and commits
/// nothing.
pub fn parse_line_height(text: &str, unit: Option<SpacingUnit>) -> Option<LineHeight> {
    let text = text.trim();
    if text.is_empty() && unit.is_none() {
        return None;
    }
    if text.is_empty() || text.eq_ignore_ascii_case("auto") {
        return Some(LineHeight::Auto);
    }
    if let Some(percent) = text.strip_suffix('%') {
        let value: f64 = percent.trim().parse().ok()?;
        if !value.is_finite() {
            return None;
        }
        return Some(LineHeight::Percent(value.max(0.0)));
    }
    if let Some(px) = text.strip_suffix("px") {
        let value: f64 = px.trim().parse().ok()?;
        if !value.is_finite() {
            return None;
        }
        return Some(LineHeight::Pixels(value.max(0.0)));
    }
    let value: f64 = text.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    match unit {
        Some(SpacingUnit::Percent) => Some(LineHeight::Percent(value.max(0.0))),
        _ => Some(LineHeight::Pixels(value.max(0.0))),
    }
}

/// "2%", "1.5px", "-0.5" → the engine's letter spacing.
///
/// A bare number keeps the current unit (percent by default).
pub fn parse_letter_spacing(text: &str, unit: Option<SpacingUnit>) -> Option<LetterSpacing> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(percent) = text.strip_suffix('%') {
        let value: f64 = percent.trim().parse().ok()?;
        if !value.is_finite() {
            return None;
        }
        return Some(LetterSpacing::Percent(value));
    }
    if let Some(px) = text.strip_suffix("px") {
        let value: f64 = px.trim().parse().ok()?;
        if !value.is_finite() {
            return None;
        }
        return Some(LetterSpacing::Pixels(value));
    }
    let value: f64 = text.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    match unit {
        Some(SpacingUnit::Pixels) => Some(LetterSpacing::Pixels(value)),
        _ => Some(LetterSpacing::Percent(value)),
    }
}

/// "Auto", "2", "10%" → the engine's decoration length.
///
/// The engine's own grammar for `textDecorationThickness` and
/// `textDecorationOffset`: automatic, a number of pixels, or a percentage of
/// the font size.
pub fn parse_decoration_length(text: &str) -> Option<LineHeight> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("auto") {
        return Some(LineHeight::Auto);
    }
    if let Some(percent) = text.strip_suffix('%') {
        let value: f64 = percent.trim().parse().ok()?;
        if !value.is_finite() {
            return None;
        }
        return Some(LineHeight::Percent(value));
    }
    if let Some(px) = text.strip_suffix("px") {
        let value: f64 = px.trim().parse().ok()?;
        if !value.is_finite() {
            return None;
        }
        return Some(LineHeight::Pixels(value));
    }
    let value: f64 = text.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    Some(LineHeight::Pixels(value))
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// What a surface that hosts the typography panel must be able to do.
///
/// The panel draws and asks; the surface owns the engine and runs the changes.
/// The panel never touches the document itself, so the engine's command surface
/// stays the one operation boundary.
pub trait TypographyHost: Sized + 'static {
    /// Run one change through the engine.
    fn run_typography_action(&mut self, action: TypographyAction, cx: &mut Context<Self>);
}

/// The Typography section, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own state,
/// and the surface that runs the changes. It returns an element, so the
/// Inspector can put it in a section of its own and a test can put it in a
/// window on its own.
///
/// The empty state is figcraft's: a selection with no text layer in it draws
/// nothing at all.
pub fn section<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    if model.is_empty() {
        return div().into_any_element();
    }
    let mut column = div().flex().flex_col().gap_2().px(px(14.0)).py(px(12.0));
    column = column.child(text_row(model));
    column = column.child(family_row(model, state, cx));
    column = column.child(style_row(model, state, cx));
    column = column.child(size_row(model, cx));
    column = column.child(spacing_row(model, state, cx));
    column = column.child(paragraph_row(model, state, cx));
    column = column.child(alignment_row(model, state, cx));
    if state.borrow().open == Some(TypographyMenu::Settings) {
        column = column.child(settings_block(model, state, cx));
    }
    if model.on_path.set().is_some() || model.can_put_on_path {
        column = column.child(path_row(model, state, cx));
    }
    column.into_any_element()
}

/// The text the layers hold.
///
/// figcraft's first row: the text, with the variable it may be bound to. Spool
/// has no variable binding in this panel, so the row shows the text itself.
fn text_row(model: &TypographyModel) -> Div {
    let text = model.characters.label(|text| {
        let first = text.lines().next().unwrap_or_default();
        if text.lines().count() > 1 {
            format!("{first}…")
        } else {
            first.to_owned()
        }
    });
    field_chrome("Text", &text, false)
}

/// The font family, and the list of families to pick from.
fn family_row<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let open = state.borrow().open == Some(TypographyMenu::FontFamily);
    let label = model.font_family.label(|family| family.clone());
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(dropdown_trigger(
        "typography-family",
        "Family",
        &label,
        open,
        state,
        cx,
    ));
    if open {
        column = column.child(family_menu(model, state, cx));
    }
    column
}

/// The list of font families, most recently used first.
///
/// figcraft's order: the families the user picked recently, then every family
/// the engine knows.
fn family_menu<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let mut column = div()
        .flex()
        .flex_col()
        .gap_1()
        .p(px(7.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER));
    let families = font_families();
    if families.is_empty() {
        column = column.child(note("No fonts match"));
        return column;
    }
    let recent: Vec<String> = state
        .borrow()
        .recent_families
        .iter()
        .filter(|family| families.iter().any(|f| f.eq_ignore_ascii_case(family)))
        .cloned()
        .collect();
    if !recent.is_empty() {
        column = column.child(menu_header("Recent"));
        for family in recent {
            column = column.child(family_item(&family, model, state, cx));
        }
        column = column.child(menu_header("All fonts"));
    }
    for family in families {
        column = column.child(family_item(&family, model, state, cx));
    }
    column
}

/// One family in the list.
fn family_item<T: TypographyHost>(
    family: &str,
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let current = model
        .font_family
        .set()
        .is_some_and(|f| f.eq_ignore_ascii_case(family));
    let target = model.target();
    let chosen = family.to_owned();
    let shared = Rc::clone(state);
    div()
        .id(SharedString::from(format!("typography-family-{family}")))
        .debug_selector(move || format!("typography-family-{family}"))
        .flex()
        .items_center()
        .gap_1()
        .px(px(6.0))
        .py(px(4.0))
        .rounded_sm()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .cursor_pointer()
        .text_xs()
        .text_color(rgb(if current {
            theme::ACCENT
        } else {
            theme::TEXT_SECONDARY
        }))
        .child(if current { "✓ " } else { "  " })
        .child(family.to_owned())
        .on_click(cx.listener(move |host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = None;
                state
                    .recent_families
                    .retain(|f| !f.eq_ignore_ascii_case(&chosen));
                state.recent_families.insert(0, chosen.clone());
                state.recent_families.truncate(8);
            }
            let edit = TypographyEdit::FontFamily(chosen.clone());
            host.run_typography_action(
                TypographyAction::Edit {
                    edit,
                    target: target.clone(),
                },
                cx,
            );
        }))
}

/// The font style, and the list of the family's styles to pick from.
fn style_row<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let open = state.borrow().open == Some(TypographyMenu::FontStyle);
    let label = model.font_style.label(|style| style.clone());
    let mut column = div().flex().flex_col().gap_1();
    column = column.child(dropdown_trigger(
        "typography-style",
        "Style",
        &label,
        open,
        state,
        cx,
    ));
    if open {
        column = column.child(style_menu(model, state, cx));
    }
    column
}

/// The list of the family's styles.
fn style_menu<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let mut column = div()
        .flex()
        .flex_col()
        .gap_1()
        .p(px(7.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER));
    // A style belongs to a family: with no family, there is no list to show.
    let Some(family) = model.font_family.set() else {
        column = column.child(note("Pick one font family first"));
        return column;
    };
    let styles = figcraft_text::family_styles(family);
    if styles.is_empty() {
        column = column.child(note("Font not installed"));
        return column;
    }
    let current = model.font_style.set().cloned();
    for style in styles {
        let chosen = style.clone();
        let label = style.clone();
        let marked = current.as_deref() == Some(style.as_str());
        let shared = Rc::clone(state);
        let target = model.target();
        column = column.child(
            div()
                .id(SharedString::from(format!("typography-style-{label}")))
                .debug_selector(move || format!("typography-style-{label}"))
                .flex()
                .items_center()
                .gap_1()
                .px(px(6.0))
                .py(px(4.0))
                .rounded_sm()
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .text_xs()
                .text_color(rgb(if marked {
                    theme::ACCENT
                } else {
                    theme::TEXT_SECONDARY
                }))
                .child(if marked { "✓ " } else { "  " })
                .child(style.clone())
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    {
                        let mut state = shared.borrow_mut();
                        state.open = None;
                    }
                    let edit = TypographyEdit::FontStyle(chosen.clone());
                    host.run_typography_action(
                        TypographyAction::Edit {
                            edit,
                            target: target.clone(),
                        },
                        cx,
                    );
                })),
        );
    }
    column
}

/// The font size.
///
/// The Inspector's own row: the value and the sizes it offers as one-click
/// chips. The panel does not draw a second, different number widget for a
/// property the Inspector already owns — the chip commits the same
/// `node.set` prop the Inspector's own size row commits.
fn size_row<T: TypographyHost>(model: &TypographyModel, cx: &mut Context<T>) -> Div {
    let property = crate::inspector::Property::FontSize;
    let spec = property.spec();
    let value = match model.font_size {
        TypographyValue::Set(size) => crate::inspector::format_number(size as f32, spec),
        TypographyValue::Unset => NOT_SET.to_owned(),
        TypographyValue::Mixed => MIXED.to_owned(),
    };
    let editable = !model.ids.is_empty();
    let mut row = div()
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
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(value),
        );
    for preset in ["12", "16", "24", "32"] {
        // The chip says what the user reads, so the value it commits is that
        // number put back through the spec.
        let stored = spec.stored(preset.parse::<f32>().unwrap_or_default());
        let edit = TypographyEdit::FontSize(f64::from(stored));
        let enabled = editable;
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
            .child(preset.to_string());
        let chip = if enabled {
            chip.cursor_pointer()
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        } else {
            chip.cursor_default().opacity(0.55)
        };
        let target = model.target();
        row = row.child(chip.on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_typography_action(
                TypographyAction::Edit {
                    edit: edit.clone(),
                    target: target.clone(),
                },
                cx,
            );
        })));
    }
    row
}

/// The line height and the letter spacing.
fn spacing_row<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let line_height = model.line_height.label(|height| match height {
        LineHeight::Auto => AUTO.to_owned(),
        LineHeight::Pixels(px) => figcraft_doc::trim_num(*px),
        LineHeight::Percent(percent) => format!("{}%", figcraft_doc::trim_num(*percent)),
    });
    let letter_spacing = model
        .letter_spacing
        .label(|spacing| figcraft_doc::trim_num(spacing_label(*spacing)));
    div()
        .flex()
        .gap_2()
        .child(spacing_field(
            TypedField {
                id: "typography-line-height",
                label: "Line height",
                field: TypographyField::LineHeight,
                unit: unit_of(model.line_height.set()),
            },
            &line_height,
            model,
            state,
            cx,
        ))
        .child(spacing_field(
            TypedField {
                id: "typography-letter-spacing",
                label: "Letter spacing",
                field: TypographyField::LetterSpacing,
                unit: spacing_unit_of(model.letter_spacing.set()),
            },
            &letter_spacing,
            model,
            state,
            cx,
        ))
}

/// The space between the paragraphs, and how the text box resizes.
fn paragraph_row<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let spacing = model
        .paragraph_spacing
        .label(|spacing| figcraft_doc::trim_num(*spacing));
    let resize = model.text_auto_resize.set().copied();
    let active = resize.map(|resize| match resize {
        TextAutoResize::WidthAndHeight => 0,
        TextAutoResize::Height => 1,
        TextAutoResize::None => 2,
        TextAutoResize::Truncate => 3,
    });
    let target = Rc::new(model.target());
    let mut row = div().flex().gap_2();
    row = row.child(spacing_field(
        TypedField {
            id: "typography-paragraph-spacing",
            label: "Paragraph spacing",
            field: TypographyField::ParagraphSpacing,
            unit: Some(SpacingUnit::Pixels),
        },
        &spacing,
        model,
        state,
        cx,
    ));
    row = row.child(segmented(
        "typography-resize",
        &[
            ("Auto width", true),
            ("Auto height", true),
            ("Fixed size", true),
            ("Truncate", true),
        ],
        active,
        None,
        {
            let target = Rc::clone(&target);
            move |index: usize, host: &mut T, cx: &mut Context<T>| {
                let value = [
                    TextAutoResize::WidthAndHeight,
                    TextAutoResize::Height,
                    TextAutoResize::None,
                    TextAutoResize::Truncate,
                ][index.min(3)];
                let edit = TypographyEdit::TextAutoResize(value);
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit,
                        target: (*target).clone(),
                    },
                    cx,
                );
            }
        },
        cx,
    ));
    row
}

/// The alignment of the text, and the Type settings.
fn alignment_row<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let align = model.text_align.set().copied();
    let active = align.map(|align| match align {
        TextAlignH::Left => 0,
        TextAlignH::Center => 1,
        TextAlignH::Right => 2,
        TextAlignH::Justified => 3,
    });
    let vertical = model.text_align_vertical.set().copied();
    let vertical_active = vertical.map(|align| match align {
        TextAlignV::Top => 0,
        TextAlignV::Middle => 1,
        TextAlignV::Bottom => 2,
    });
    let target = Rc::new(model.target());
    let open = state.borrow().open == Some(TypographyMenu::Settings);
    let mut row = div().flex().items_center().gap_2();
    row = row.child(segmented(
        "typography-align",
        &[
            ("Left", true),
            ("Center", true),
            ("Right", true),
            ("Justify", true),
        ],
        active,
        None,
        {
            let target = Rc::clone(&target);
            move |index: usize, host: &mut T, cx: &mut Context<T>| {
                let value = [
                    TextAlignH::Left,
                    TextAlignH::Center,
                    TextAlignH::Right,
                    TextAlignH::Justified,
                ][index.min(3)];
                let edit = TypographyEdit::TextAlign(value);
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit,
                        target: (*target).clone(),
                    },
                    cx,
                );
            }
        },
        cx,
    ));
    row = row.child(segmented(
        "typography-align-vertical",
        &[("Top", true), ("Middle", true), ("Bottom", true)],
        vertical_active,
        None,
        {
            let target = Rc::clone(&target);
            move |index: usize, host: &mut T, cx: &mut Context<T>| {
                let value = [TextAlignV::Top, TextAlignV::Middle, TextAlignV::Bottom][index.min(2)];
                let edit = TypographyEdit::TextAlignVertical(value);
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit,
                        target: (*target).clone(),
                    },
                    cx,
                );
            }
        },
        cx,
    ));
    let shared = Rc::clone(state);
    row = row.child(
        div()
            .id(SharedString::from("typography-settings"))
            .debug_selector(|| "typography-settings".to_owned())
            .px(px(7.0))
            .py(px(5.0))
            .rounded_sm()
            .bg(rgb(if open {
                theme::ACCENT_WASH
            } else {
                theme::WINDOW
            }))
            .border_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("⋯")
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                let mut state = shared.borrow_mut();
                state.open = if state.open == Some(TypographyMenu::Settings) {
                    None
                } else {
                    Some(TypographyMenu::Settings)
                };
                drop(state);
                cx.notify();
            })),
    );
    row
}

/// The Type settings: the basics, the OpenType features and the variable axes.
fn settings_block<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let tab = state.borrow().tab;
    let face = variable_face(model);
    let mut column = div()
        .flex()
        .flex_col()
        .gap_2()
        .p(px(7.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER));
    // The Variable tab is only there for a variable font.
    let tabs: [(&str, TypographyTab); 3] = [
        ("Basics", TypographyTab::Basics),
        ("Details", TypographyTab::Details),
        ("Variable", TypographyTab::Variable),
    ];
    let mut strip = div().flex().items_center().gap_2();
    for (label, value) in tabs {
        // The Variable tab needs a variable font; without one it is not there.
        if value == TypographyTab::Variable && face.is_none() {
            continue;
        }
        let shared = Rc::clone(state);
        let on = tab == value;
        strip = strip.child(
            div()
                .id(SharedString::from(format!("typography-tab-{label}")))
                .debug_selector(move || format!("typography-tab-{label}"))
                .px(px(6.0))
                .py(px(4.0))
                .rounded_sm()
                .text_xs()
                .font_weight(if on {
                    FontWeight::MEDIUM
                } else {
                    FontWeight::NORMAL
                })
                .text_color(rgb(if on {
                    theme::TEXT
                } else {
                    theme::TEXT_SECONDARY
                }))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child(label)
                .on_click(cx.listener(move |_host, _event, _window, cx| {
                    shared.borrow_mut().tab = value;
                    cx.notify();
                })),
        );
    }
    column = column.child(strip);
    column = column.child(match tab {
        TypographyTab::Basics => basics_block(model, state, cx),
        TypographyTab::Details => details_block(model, cx),
        TypographyTab::Variable => variable_block(model, state, cx),
    });
    column
}

/// The Type settings ▸ Basics: the decoration, the case, the lists, the
/// indentation, the truncation.
fn basics_block<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let mut column = div().flex().flex_col().gap_2();
    let target = Rc::new(model.target());

    // The decoration, and how it is drawn.
    let decoration = model.decoration.set().copied();
    let active = decoration.map(|decoration| match decoration {
        TextDecoration::None => 0,
        TextDecoration::Underline => 1,
        TextDecoration::Strikethrough => 2,
    });
    column = column.child(caption("Decoration"));
    column = column.child(segmented(
        "typography-decoration",
        &[("None", true), ("Underline", true), ("Strikethrough", true)],
        active,
        None,
        {
            let target = Rc::clone(&target);
            move |index: usize, host: &mut T, cx: &mut Context<T>| {
                let value = [
                    TextDecoration::None,
                    TextDecoration::Underline,
                    TextDecoration::Strikethrough,
                ][index.min(2)];
                let edit = TypographyEdit::TextDecoration(value);
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit,
                        target: (*target).clone(),
                    },
                    cx,
                );
            }
        },
        cx,
    ));

    // The decoration's own look, when there is a line to draw.
    if let Some(decoration @ (TextDecoration::Underline | TextDecoration::Strikethrough)) =
        decoration
    {
        let style_active = model.decoration_style.set().map(|style| match style {
            DecorationStyle::Solid => 0,
            DecorationStyle::Dotted => 1,
            DecorationStyle::Wavy => 2,
        });
        column = column.child(caption("Decoration style"));
        column = column.child(segmented(
            "typography-decoration-style",
            &[("Solid", true), ("Dotted", true), ("Wavy", true)],
            style_active,
            None,
            {
                let target = Rc::clone(&target);
                move |index: usize, host: &mut T, cx: &mut Context<T>| {
                    let value = [
                        DecorationStyle::Solid,
                        DecorationStyle::Dotted,
                        DecorationStyle::Wavy,
                    ][index.min(2)];
                    let edit = TypographyEdit::TextDecorationStyle(value);
                    host.run_typography_action(
                        TypographyAction::Edit {
                            edit,
                            target: (*target).clone(),
                        },
                        cx,
                    );
                }
            },
            cx,
        ));
        let thickness = model
            .decoration_thickness
            .label(|thickness| match thickness {
                LineHeight::Auto => AUTO.to_owned(),
                LineHeight::Pixels(px) => figcraft_doc::trim_num(*px),
                LineHeight::Percent(percent) => {
                    format!("{}%", figcraft_doc::trim_num(*percent))
                }
            });
        column = column.child(spacing_field(
            TypedField {
                id: "typography-decoration-thickness",
                label: "Thickness",
                field: TypographyField::DecorationThickness,
                unit: Some(SpacingUnit::Pixels),
            },
            &thickness,
            model,
            state,
            cx,
        ));
        // The offset belongs to an underline: a strikethrough has no edge to
        // hang off.
        if decoration == TextDecoration::Underline {
            let offset = model.decoration_offset.label(|offset| match offset {
                LineHeight::Auto => AUTO.to_owned(),
                LineHeight::Pixels(px) => figcraft_doc::trim_num(*px),
                LineHeight::Percent(percent) => {
                    format!("{}%", figcraft_doc::trim_num(*percent))
                }
            });
            column = column.child(spacing_field(
                TypedField {
                    id: "typography-decoration-offset",
                    label: "Offset",
                    field: TypographyField::DecorationOffset,
                    unit: Some(SpacingUnit::Pixels),
                },
                &offset,
                model,
                state,
                cx,
            ));
            let skip = model.decoration_skip_ink.set().copied();
            column = column.child(checkbox(
                "typography-skip-ink",
                "Skip ink",
                skip,
                {
                    let target = Rc::clone(&target);
                    move |on: bool, host: &mut T, cx: &mut Context<T>| {
                        let edit = TypographyEdit::TextDecorationSkipInk(on);
                        host.run_typography_action(
                            TypographyAction::Edit {
                                edit,
                                target: (*target).clone(),
                            },
                            cx,
                        );
                    }
                },
                cx,
            ));
        }
        // The decoration's colour: the engine records none of its own when the
        // line takes the text's colour, and the panel says so.
        let colour = match &model.decoration_color {
            TypographyValue::Set(Some(colour)) => format!("#{}", colour.to_hex()),
            TypographyValue::Set(None) => NOT_SET.to_owned(),
            TypographyValue::Unset => NOT_SET.to_owned(),
            TypographyValue::Mixed => MIXED.to_owned(),
        };
        column = column.child(field_chrome("Colour", &colour, false));
        if matches!(model.decoration_color, TypographyValue::Set(None)) {
            column = column.child(disabled_note(INHERITED_COLOUR));
        }
    }

    // The letter case.
    let case = model.text_case.set().copied();
    let case_active = case.map(|case| match case {
        TextCase::Original => 0,
        TextCase::Upper => 1,
        TextCase::Lower => 2,
        TextCase::Title => 3,
        TextCase::SmallCaps => 4,
    });
    column = column.child(caption("Letter case"));
    column = column.child(segmented(
        "typography-case",
        &[
            ("As typed", true),
            ("Uppercase", true),
            ("Lowercase", true),
            ("Title case", true),
            ("Small caps", true),
        ],
        case_active,
        None,
        {
            let target = Rc::clone(&target);
            move |index: usize, host: &mut T, cx: &mut Context<T>| {
                let value = [
                    TextCase::Original,
                    TextCase::Upper,
                    TextCase::Lower,
                    TextCase::Title,
                    TextCase::SmallCaps,
                ][index.min(4)];
                let edit = TypographyEdit::TextCase(value);
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit,
                        target: (*target).clone(),
                    },
                    cx,
                );
            }
        },
        cx,
    ));

    // The list.
    let list = model.list_style.set().copied();
    let list_active = list.map(|list| match list {
        ListStyle::None => 0,
        ListStyle::Bulleted => 1,
        ListStyle::Numbered => 2,
    });
    column = column.child(caption("List style"));
    column = column.child(segmented(
        "typography-list",
        &[("No list", true), ("Bulleted", true), ("Numbered", true)],
        list_active,
        None,
        {
            let target = Rc::clone(&target);
            move |index: usize, host: &mut T, cx: &mut Context<T>| {
                let value =
                    [ListStyle::None, ListStyle::Bulleted, ListStyle::Numbered][index.min(2)];
                let edit = TypographyEdit::ListStyle(value);
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit,
                        target: (*target).clone(),
                    },
                    cx,
                );
            }
        },
        cx,
    ));
    let list_spacing = model
        .list_spacing
        .label(|spacing| figcraft_doc::trim_num(*spacing));
    column = column.child(spacing_field(
        TypedField {
            id: "typography-list-spacing",
            label: "List spacing",
            field: TypographyField::ListSpacing,
            unit: Some(SpacingUnit::Pixels),
        },
        &list_spacing,
        model,
        state,
        cx,
    ));

    // The paragraph indent. It hangs off the aligned edge, so a centred or
    // justified line has no edge to hang it off: the field stays visible and
    // says why it cannot be used.
    let indent = model
        .paragraph_indent
        .label(|indent| figcraft_doc::trim_num(*indent));
    let start_aligned = model
        .text_align
        .set()
        .is_some_and(|align| *align != TextAlignH::Center && *align != TextAlignH::Justified);
    let indent_field = spacing_field(
        TypedField {
            id: "typography-paragraph-indent",
            label: "Paragraph indent",
            field: TypographyField::ParagraphIndent,
            unit: Some(SpacingUnit::Pixels),
        },
        &indent,
        model,
        state,
        cx,
    );
    let indent_field: AnyElement = if start_aligned {
        indent_field.into_any_element()
    } else {
        div().opacity(0.55).child(indent_field).into_any_element()
    };
    column = column.child(indent_field);
    if !start_aligned {
        column = column.child(disabled_note(NO_INDENT));
    }

    // The hanging punctuation and the hanging list.
    let hanging_punctuation = model.hanging_punctuation.set().copied();
    column = column.child(checkbox(
        "typography-hanging-punctuation",
        "Hanging punctuation",
        hanging_punctuation,
        {
            let target = Rc::clone(&target);
            move |on: bool, host: &mut T, cx: &mut Context<T>| {
                let edit = TypographyEdit::HangingPunctuation(on);
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit,
                        target: (*target).clone(),
                    },
                    cx,
                );
            }
        },
        cx,
    ));
    let hanging_list = model.hanging_list.set().copied();
    column = column.child(checkbox(
        "typography-hanging-list",
        "Hanging list",
        hanging_list,
        {
            let target = Rc::clone(&target);
            move |on: bool, host: &mut T, cx: &mut Context<T>| {
                let edit = TypographyEdit::HangingList(on);
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit,
                        target: (*target).clone(),
                    },
                    cx,
                );
            }
        },
        cx,
    ));

    // The vertical trim.
    let trim = model.leading_trim.set().copied();
    let trim_active = trim.map(|trim| usize::from(trim == LeadingTrim::CapHeight));
    column = column.child(caption("Vertical trim"));
    column = column.child(segmented(
        "typography-leading-trim",
        &[("Standard", true), ("Cap height", true)],
        trim_active,
        None,
        {
            let target = Rc::clone(&target);
            move |index: usize, host: &mut T, cx: &mut Context<T>| {
                let value = if index == 1 {
                    LeadingTrim::CapHeight
                } else {
                    LeadingTrim::None
                };
                let edit = TypographyEdit::LeadingTrim(value);
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit,
                        target: (*target).clone(),
                    },
                    cx,
                );
            }
        },
        cx,
    ));

    // The truncation.
    let resize = model.text_auto_resize.set().copied();
    let truncate_active = resize.map(|resize| usize::from(resize == TextAutoResize::Truncate));
    column = column.child(caption("Truncate text"));
    column = column.child(segmented(
        "typography-truncate",
        &[("Off", true), ("Ellipsis", true)],
        truncate_active,
        None,
        {
            let target = Rc::clone(&target);
            move |index: usize, host: &mut T, cx: &mut Context<T>| {
                // Turning the truncation off leaves the auto-sizing the text had.
                let value = if index == 1 {
                    TextAutoResize::Truncate
                } else {
                    TextAutoResize::None
                };
                let edit = TypographyEdit::TextAutoResize(value);
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit,
                        target: (*target).clone(),
                    },
                    cx,
                );
            }
        },
        cx,
    ));
    // The max lines only matter while the text is truncated.
    if resize == Some(TextAutoResize::Truncate) {
        let max_lines = model.max_lines.label(|lines| {
            if *lines == 0 {
                AUTO.to_owned()
            } else {
                figcraft_doc::trim_num(f64::from(*lines))
            }
        });
        column = column.child(spacing_field(
            TypedField {
                id: "typography-max-lines",
                label: "Max lines",
                field: TypographyField::MaxLines,
                unit: Some(SpacingUnit::Pixels),
            },
            &max_lines,
            model,
            state,
            cx,
        ));
    }
    column
}

/// The Type settings ▸ Details: the OpenType features the font has.
fn details_block<T: TypographyHost>(model: &TypographyModel, cx: &mut Context<T>) -> Div {
    let mut column = div().flex().flex_col().gap_1();
    // The face the text is set in, and "Mixed fonts" when the selection does
    // not agree on one.
    let face_name = model
        .font_family
        .set()
        .zip(model.font_style.set())
        .map(|(family, style)| format!("{family} {style}"))
        .unwrap_or_else(|| MIXED.to_owned());
    column = column.child(
        div()
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .child(face_name),
    );
    let face = model
        .font_family
        .set()
        .zip(model.font_style.set())
        .and_then(|(family, style)| figcraft_text::find_face(family, style));
    let target = Rc::new(model.target());
    for (tag, label, default_on) in FEATURES {
        let value = model
            .features
            .get(tag)
            .copied()
            .unwrap_or(TypographyValue::Unset);
        // A feature the engine does not record is the font's own default.
        let on = match value {
            TypographyValue::Set(on) => Some(on),
            TypographyValue::Unset => Some(default_on),
            TypographyValue::Mixed => None,
        };
        // A font that does not have the feature cannot turn it on.
        let supported = match face.as_ref() {
            None => true,
            Some(face) => match <[u8; 4]>::try_from(tag.as_bytes()) {
                Ok(bytes) => face.has_feature(&bytes),
                Err(_) => false,
            },
        };
        column = column.child(feature_row(
            tag,
            label,
            on,
            supported,
            {
                let target = Rc::clone(&target);
                move |on: bool, host: &mut T, cx: &mut Context<T>| {
                    // Back to the font's default clears the override.
                    let want = !on;
                    let value = if want == default_on { None } else { Some(want) };
                    let edit = TypographyEdit::Feature {
                        tag: tag.to_owned(),
                        on: value,
                    };
                    host.run_typography_action(
                        TypographyAction::Edit {
                            edit,
                            target: (*target).clone(),
                        },
                        cx,
                    );
                }
            },
            cx,
        ));
    }
    column
}

/// The Type settings ▸ Variable: the font's named instances and its axes.
fn variable_block<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> Div {
    let mut column = div().flex().flex_col().gap_2();
    let Some(face) = variable_face(model) else {
        column = column.child(note("Mixed fonts"));
        return column;
    };
    column = column.child(
        div()
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .child(face.family.clone()),
    );
    let target = model.target();

    // The named instances: the one the text shows, or "Custom" once an axis
    // has moved.
    if !face.instances.is_empty() {
        let open = state.borrow().open == Some(TypographyMenu::Instance);
        let current = model
            .font_style
            .set()
            .map(|style| style.to_owned())
            .unwrap_or_else(|| MIXED.to_owned());
        let label = match model.variations.values().collect::<Vec<_>>() {
            values if values.iter().all(|v| matches!(v, TypographyValue::Unset)) => current,
            _ => "Custom".to_owned(),
        };
        let mut block = div().flex().flex_col().gap_1();
        block = block.child(dropdown_trigger(
            "typography-instance",
            "Named instance",
            &label,
            open,
            state,
            cx,
        ));
        if open {
            let mut menu = div()
                .flex()
                .flex_col()
                .gap_1()
                .p(px(7.0))
                .rounded_md()
                .bg(rgb(theme::WINDOW))
                .border_1()
                .border_color(rgb(theme::BORDER));
            for instance in &face.instances {
                let name = instance.name.clone();
                let target = target.clone();
                let label = name.clone();
                menu = menu.child(
                    div()
                        .id(SharedString::from(format!("typography-instance-{label}")))
                        .debug_selector(move || format!("typography-instance-{label}"))
                        .px(px(6.0))
                        .py(px(4.0))
                        .rounded_sm()
                        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                        .cursor_pointer()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_SECONDARY))
                        .child(name.clone())
                        .on_click(cx.listener(move |host, _event, _window, cx| {
                            // A named instance brings its own axis values with
                            // it, so the custom ones go.
                            let edit = TypographyEdit::FontStyle(name.clone());
                            host.run_typography_action(
                                TypographyAction::Edit {
                                    edit,
                                    target: target.clone(),
                                },
                                cx,
                            );
                        })),
                );
            }
            block = block.child(menu);
        }
        column = column.child(block);
    }

    // The axes, one row each: the axis name, its value and its range.
    for (index, axis) in face.axes.iter().enumerate() {
        if axis.hidden {
            continue;
        }
        let value = axis_value(model, &face, index);
        let label = match value {
            Some(value) => figcraft_doc::trim_num(value),
            None => MIXED.to_owned(),
        };
        column = column.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_SECONDARY))
                        .child(axis.name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(label),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(format!(
                            "{}–{}",
                            figcraft_doc::trim_num(axis.min),
                            figcraft_doc::trim_num(axis.max)
                        )),
                ),
        );
    }
    column
}

/// The value an axis has, when the selection agrees on one.
///
/// The value the face implies for the style, which is the style's own value
/// when it has one.
fn axis_value(
    model: &TypographyModel,
    face: &Arc<figcraft_text::Face>,
    index: usize,
) -> Option<f64> {
    let mut values = model.styles.iter().map(|style| {
        figcraft_text::find_face(&style.font_family, &style.font_style)
            .filter(|found| found.id == face.id)
            .map(|found| found.axis_values(style))
            .and_then(|values| values.get(index).copied())
    });
    let first = values.next()??;
    if values.all(|value| value.is_some_and(|value| (value - first).abs() < 1e-9)) {
        Some(first)
    } else {
        None
    }
}

/// The variable font the shown styles use, if any.
fn variable_face(model: &TypographyModel) -> Option<Arc<figcraft_text::Face>> {
    let family = model.font_family.set()?.clone();
    let style = model.font_style.set()?.clone();
    let face = figcraft_text::find_face(&family, &style)?;
    (face.is_variable() && face.family.eq_ignore_ascii_case(&family)).then_some(face)
}

/// The text on a path: where it starts, which side it runs on, and taking it
/// off the path; or, with a text and a path selected, putting the text on the
/// path.
fn path_row<T: TypographyHost>(
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let target = Rc::new(model.target());
    if !model.on_path.set().copied().unwrap_or(false) {
        // With a text layer and a path layer both selected, the text can go on
        // the path.
        let edit = TypographyEdit::PutOnPath;
        let put = Rc::clone(&target);
        return div()
            .id(SharedString::from("typography-put-on-path"))
            .debug_selector(|| "typography-put-on-path".to_owned())
            .px(px(10.0))
            .py(px(9.0))
            .rounded_md()
            .bg(rgb(theme::WINDOW))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("Put text on path")
            .on_click(cx.listener(move |host, _event, _window, cx| {
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit: edit.clone(),
                        target: (*put).clone(),
                    },
                    cx,
                );
            }))
            .into_any_element();
    }
    let start = model
        .path_start
        .label(|start| format!("{}%", figcraft_doc::trim_num(start * 100.0)));
    let flip = model.path_flip.set().copied();
    let mut row = div().flex().gap_2();
    row = row.child(spacing_field(
        TypedField {
            id: "typography-path-start",
            label: "Start on path",
            field: TypographyField::PathStart,
            unit: Some(SpacingUnit::Percent),
        },
        &start,
        model,
        state,
        cx,
    ));
    row = row.child(checkbox(
        "typography-path-flip",
        "Flip",
        flip,
        {
            let target = Rc::clone(&target);
            move |on: bool, host: &mut T, cx: &mut Context<T>| {
                let edit = TypographyEdit::PathFlip(on);
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit,
                        target: (*target).clone(),
                    },
                    cx,
                );
            }
        },
        cx,
    ));
    let edit = TypographyEdit::RemoveFromPath;
    let remove = Rc::clone(&target);
    row = row.child(
        div()
            .id(SharedString::from("typography-remove-from-path"))
            .debug_selector(|| "typography-remove-from-path".to_owned())
            .px(px(7.0))
            .py(px(5.0))
            .rounded_sm()
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("Remove from path")
            .on_click(cx.listener(move |host, _event, _window, cx| {
                host.run_typography_action(
                    TypographyAction::Edit {
                        edit: edit.clone(),
                        target: (*remove).clone(),
                    },
                    cx,
                );
            })),
    );
    row.into_any_element()
}

// ═══════════════════════════════════════════════════════════════════════════
// The widgets
// ═══════════════════════════════════════════════════════════════════════════

/// The shape every labelled field shares: a label on the left, a value on the
/// right, one border, one padding, one type size.
///
/// The Inspector's own shape, so a field in this panel and a field in the
/// Inspector are one thing and not two.
fn field_chrome(label: &str, value: &str, dimmed: bool) -> Div {
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
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .child(
            div()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(label.to_owned()),
        )
        .child(
            div()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(value.to_owned()),
        );
    if dimmed {
        field.opacity(0.55)
    } else {
        field
    }
}

/// A field that opens a menu.
fn dropdown_trigger<T: TypographyHost>(
    id: &'static str,
    label: &'static str,
    value: &str,
    open: bool,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let shared = Rc::clone(state);
    let selector = format!("{id}-trigger");
    field_chrome(label, value, false)
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .border_color(rgb(if open {
            theme::ACCENT
        } else {
            theme::BORDER_SOFT
        }))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            let mut state = shared.borrow_mut();
            let menu = match id {
                "typography-family" => TypographyMenu::FontFamily,
                "typography-style" => TypographyMenu::FontStyle,
                "typography-instance" => TypographyMenu::Instance,
                _ => TypographyMenu::Settings,
            };
            state.open = if state.open == Some(menu) {
                None
            } else {
                Some(menu)
            };
            drop(state);
            cx.notify();
        }))
}

/// One field that can be typed into.
///
/// The field's own description, in one place: which element it is, what it
/// says, which value it holds and the unit that value uses.
#[derive(Clone, Debug)]
struct TypedField {
    /// The element id, for a test to look it up.
    id: &'static str,
    /// The label the field shows.
    label: &'static str,
    /// Which field it is.
    field: TypographyField,
    /// The unit the current value uses, so a bare number keeps it.
    unit: Option<SpacingUnit>,
}

/// A field that can be typed into.
///
/// The value is the field's own; the click opens the buffer, and the
/// Inspector's key routing types into it.
fn spacing_field<T: TypographyHost>(
    spec: TypedField,
    value: &str,
    model: &TypographyModel,
    state: &Rc<RefCell<TypographyPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let shared = Rc::clone(state);
    let selector = format!("{}-field", spec.id);
    let target = model.target();
    // The buffer starts as the value the field shows, so the text that is
    // already there is what the first keystroke replaces.
    let text = value.to_owned();
    field_chrome(spec.label, value, false)
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .cursor_text()
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            let mut state = shared.borrow_mut();
            state.draft = Some(TypographyDraft {
                field: spec.field.clone(),
                text: text.clone(),
                select_all: true,
                target: target.clone(),
                unit: spec.unit,
            });
            drop(state);
            cx.notify();
        }))
}

/// The action a segmented control runs when one of its cells is picked.
///
/// A cell, because every option in a row is the same action with a different
/// index: the cells share one action rather than each owning a copy of it.
type SegmentedAction<T> = Rc<dyn Fn(usize, &mut T, &mut Context<T>)>;

/// The action a tick box runs when it is clicked.
type ToggleAction<T> = Rc<dyn Fn(bool, &mut T, &mut Context<T>)>;

/// A row of mutually exclusive choices.
///
/// The Inspector's own segmented control, so "Mixed" means one thing in the
/// panel and the choices look the same wherever they are.
fn segmented<T: TypographyHost>(
    prefix: &str,
    labels: &[(&'static str, bool)],
    selected: Option<usize>,
    state: Option<&'static str>,
    action: impl Fn(usize, &mut T, &mut Context<T>) + 'static,
    cx: &mut Context<T>,
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
    // The action is shared by every option, so it is a cell the options clone
    // rather than a closure each of them has to own a copy of.
    let action: SegmentedAction<T> = Rc::new(action);
    for (index, (label, offered)) in labels.iter().enumerate() {
        let pressed = selected == Some(index);
        let enabled = *offered;
        let option = div()
            .id(SharedString::from(format!("{prefix}-{index}")))
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
        let option = if enabled {
            option
                .cursor_pointer()
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        } else {
            option.cursor_default().opacity(0.55)
        };
        let action = Rc::clone(&action);
        row = row.child(option.on_click(cx.listener(
            move |host: &mut T, _: &gpui::ClickEvent, _, cx: &mut Context<T>| {
                if enabled {
                    action(index, host, cx);
                }
            },
        )));
    }
    row
}

/// A tick box.
fn checkbox<T: TypographyHost>(
    id: &'static str,
    label: &'static str,
    on: Option<bool>,
    action: impl Fn(bool, &mut T, &mut Context<T>) + 'static,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let marked = on.unwrap_or(false);
    let mixed = on.is_none();
    let box_style = div()
        .size(px(12.0))
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(rgb(theme::TEXT))
        .child(if mixed {
            "–"
        } else if marked {
            "✓"
        } else {
            ""
        });
    let action: ToggleAction<T> = Rc::new(action);
    let action = Rc::clone(&action);
    div()
        .id(SharedString::from(id))
        .debug_selector(|| id.to_owned())
        .flex()
        .items_center()
        .gap_2()
        .py(px(4.0))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .child(box_style)
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(label),
        )
        .on_click(cx.listener(move |host, _event, _window, cx| {
            action(!marked, host, cx);
        }))
}

/// One OpenType feature: its name, its tag and whether it is on.
fn feature_row<T: TypographyHost>(
    tag: &'static str,
    label: &'static str,
    on: Option<bool>,
    supported: bool,
    action: impl Fn(bool, &mut T, &mut Context<T>) + 'static,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let marked = on.unwrap_or(false);
    let mixed = on.is_none();
    let box_style = div()
        .size(px(12.0))
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(rgb(theme::TEXT))
        .child(if mixed {
            "–"
        } else if marked {
            "✓"
        } else {
            ""
        });
    let action: ToggleAction<T> = Rc::new(action);
    let action = Rc::clone(&action);
    div()
        .id(SharedString::from(format!("typography-feature-{tag}")))
        .debug_selector(move || format!("typography-feature-{tag}"))
        .flex()
        .items_center()
        .gap_2()
        .py(px(3.0))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .child(box_style)
        .child(
            div()
                .flex_1()
                .text_xs()
                .text_color(rgb(if supported {
                    theme::TEXT_SECONDARY
                } else {
                    theme::TEXT_MUTED
                }))
                .child(label),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(tag),
        )
        .on_click(cx.listener(move |host, _event, _window, cx| {
            action(!marked, host, cx);
        }))
}

/// A run of text above the controls it labels.
fn caption(text: &'static str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A run of text that says why a row of controls cannot be used.
fn disabled_note(reason: &'static str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(reason.to_owned())
}

/// A run of text inside a menu.
fn note(text: &'static str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A heading inside a menu.
fn menu_header(text: &'static str) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// The unit a spacing value uses.
fn unit_of(value: Option<&LineHeight>) -> Option<SpacingUnit> {
    match value {
        Some(LineHeight::Auto) => Some(SpacingUnit::Auto),
        Some(LineHeight::Pixels(_)) => Some(SpacingUnit::Pixels),
        Some(LineHeight::Percent(_)) => Some(SpacingUnit::Percent),
        None => None,
    }
}

/// The unit a letter spacing uses.
fn spacing_unit_of(value: Option<&LetterSpacing>) -> Option<SpacingUnit> {
    match value {
        Some(LetterSpacing::Pixels(_)) => Some(SpacingUnit::Pixels),
        Some(LetterSpacing::Percent(_)) => Some(SpacingUnit::Percent),
        None => None,
    }
}

/// A letter spacing, as the number the user reads.
fn spacing_label(spacing: LetterSpacing) -> f64 {
    match spacing {
        LetterSpacing::Pixels(px) => px,
        LetterSpacing::Percent(percent) => percent,
    }
}

/// Every font family the engine knows.
///
/// The engine's own font database, the same one the canvas shapes text with.
fn font_families() -> Vec<String> {
    figcraft_text::scan_system_fonts();
    figcraft_text::family_names()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use figcraft_doc::{NodeKind, TextData};
    use figcraft_engine::Session;
    use gpui::{
        point, size, AppContext, Entity, Render, TestAppContext, VisualTestContext, Window,
    };

    /// A document with one text layer on it.
    fn document_with_text(characters: &str) -> (Document, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(
                page,
                NodeKind::Text(TextData::new(characters)),
                "Text",
                0.0,
                0.0,
                100.0,
                30.0,
            )
            .expect("a text layer");
        (doc, id)
    }

    /// A document with one rectangle on it: a layer with no text at all.
    fn document_with_rectangle() -> (Document, NodeId) {
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
                30.0,
            )
            .expect("a rectangle");
        (doc, id)
    }

    /// A session over a document, with the layer selected.
    fn session_with(doc: Document, id: NodeId) -> Session {
        let mut session = Session::with_document(doc);
        session
            .execute("select.set", &serde_json::json!({ "ids": [id.0] }))
            .expect("the layer is selected");
        session
    }

    /// Every property the panel shows, written with the engine's own
    /// spellings.
    ///
    /// The spellings are the ones `vendor/figcraft/crates/engine/src/cmd/props.rs`
    /// documents, and the test below pins them against the engine's own key
    /// list, so a spelling that drifts is a failing test rather than a silent
    /// no-op.
    fn every_property() -> serde_json::Value {
        serde_json::json!({
            "fontFamily": "Inter",
            "fontStyle": "Bold",
            "fontSize": 24.0,
            "lineHeight": "150%",
            "letterSpacing": "2%",
            "textAlign": "center",
            "textAlignVertical": "middle",
            "textAutoResize": "truncate",
            "textDecoration": "underline",
            "textDecorationStyle": "wavy",
            "textDecorationThickness": 3.0,
            "textDecorationOffset": "10%",
            "textDecorationSkipInk": true,
            "textDecorationColor": "#336699",
            "textCase": "upper",
            "paragraphSpacing": 8.0,
            "paragraphIndent": 4.0,
            "listSpacing": 2.0,
            "hangingPunctuation": true,
            "hangingList": true,
            "leadingTrim": "capHeight",
            "maxLines": 3,
            "features": { "tnum": true },
            "fontVariations": { "wght": 550.0 },
        })
    }

    /// The colour the engine read back, in the engine's own form.
    fn decoration_colour() -> Color {
        Color::parse_css("#336699").expect("a colour")
    }

    #[test]
    fn a_text_layer_reads_back_the_engines_values_for_every_property_the_panel_shows() {
        let (doc, id) = document_with_text("Headline");
        let mut session = session_with(doc, id);
        session
            .execute(
                "node.set",
                &serde_json::json!({
                    "ids": [id.0],
                    "props": every_property(),
                }),
            )
            .expect("the engine accepts the properties");
        session
            .execute(
                "text.listStyle",
                &serde_json::json!({ "ids": [id.0], "style": "bulleted" }),
            )
            .expect("the engine accepts the list style");

        let model = TypographyModel::from_session(&session);

        // The character properties.
        assert!(model.has_text);
        assert_eq!(model.ids, vec![id]);
        assert_eq!(
            model.characters,
            TypographyValue::Set("Headline".to_owned())
        );
        assert_eq!(model.font_family, TypographyValue::Set("Inter".to_owned()));
        assert_eq!(model.font_style, TypographyValue::Set("Bold".to_owned()));
        assert_eq!(model.font_size, TypographyValue::Set(24.0));
        assert_eq!(
            model.line_height,
            TypographyValue::Set(LineHeight::Percent(150.0))
        );
        assert_eq!(
            model.letter_spacing,
            TypographyValue::Set(LetterSpacing::Percent(2.0))
        );
        assert_eq!(
            model.decoration,
            TypographyValue::Set(TextDecoration::Underline)
        );
        assert_eq!(
            model.decoration_style,
            TypographyValue::Set(DecorationStyle::Wavy)
        );
        assert_eq!(
            model.decoration_thickness,
            TypographyValue::Set(LineHeight::Pixels(3.0))
        );
        assert_eq!(
            model.decoration_offset,
            TypographyValue::Set(LineHeight::Percent(10.0))
        );
        assert_eq!(model.decoration_skip_ink, TypographyValue::Set(true));
        assert_eq!(
            model.decoration_color,
            TypographyValue::Set(Some(decoration_colour()))
        );
        assert_eq!(model.text_case, TypographyValue::Set(TextCase::Upper));

        // The layer properties.
        assert_eq!(model.paragraph_spacing, TypographyValue::Set(8.0));
        assert_eq!(model.paragraph_indent, TypographyValue::Set(4.0));
        assert_eq!(model.list_style, TypographyValue::Set(ListStyle::Bulleted));
        assert_eq!(model.list_spacing, TypographyValue::Set(2.0));
        assert_eq!(model.hanging_punctuation, TypographyValue::Set(true));
        assert_eq!(model.hanging_list, TypographyValue::Set(true));
        assert_eq!(
            model.leading_trim,
            TypographyValue::Set(LeadingTrim::CapHeight)
        );
        assert_eq!(model.max_lines, TypographyValue::Set(3));
        assert_eq!(model.text_align, TypographyValue::Set(TextAlignH::Center));
        assert_eq!(
            model.text_align_vertical,
            TypographyValue::Set(TextAlignV::Middle)
        );
        assert_eq!(
            model.text_auto_resize,
            TypographyValue::Set(TextAutoResize::Truncate)
        );

        // The engine's own maps: a tag the engine records is set, and a tag it
        // does not record is not set at all.
        assert_eq!(
            model.features.get("tnum"),
            Some(&TypographyValue::Set(true))
        );
        assert_eq!(model.features.get("liga"), Some(&TypographyValue::Unset));
        assert_eq!(
            model.variations.get("wght"),
            Some(&TypographyValue::Set(550.0))
        );
    }

    #[test]
    fn a_command_for_a_change_carries_the_engines_own_prop_name_and_value() {
        let target = TypographyTarget {
            ids: vec![NodeId(3)],
            range: None,
        };
        let (command, params) = typography_command(&TypographyEdit::FontSize(24.0), &target);
        assert_eq!(command, "node.set");
        assert_eq!(params["ids"], serde_json::json!([3]));
        assert_eq!(params["props"]["fontSize"], serde_json::json!(24.0));

        let (_, params) =
            typography_command(&TypographyEdit::FontFamily("Inter".to_owned()), &target);
        assert_eq!(params["props"]["fontFamily"], serde_json::json!("Inter"));

        let (_, params) = typography_command(
            &TypographyEdit::LineHeight(LineHeight::Percent(150.0)),
            &target,
        );
        assert_eq!(params["props"]["lineHeight"], serde_json::json!("150%"));

        let (_, params) =
            typography_command(&TypographyEdit::LineHeight(LineHeight::Auto), &target);
        assert_eq!(params["props"]["lineHeight"], serde_json::json!("auto"));

        let (_, params) = typography_command(
            &TypographyEdit::LetterSpacing(LetterSpacing::Percent(2.0)),
            &target,
        );
        assert_eq!(params["props"]["letterSpacing"], serde_json::json!("2%"));

        let (_, params) = typography_command(
            &TypographyEdit::LetterSpacing(LetterSpacing::Pixels(1.5)),
            &target,
        );
        assert_eq!(params["props"]["letterSpacing"], serde_json::json!(1.5));

        let (_, params) =
            typography_command(&TypographyEdit::TextAlign(TextAlignH::Justified), &target);
        assert_eq!(params["props"]["textAlign"], serde_json::json!("justified"));

        let (_, params) = typography_command(
            &TypographyEdit::TextAlignVertical(TextAlignV::Middle),
            &target,
        );
        assert_eq!(
            params["props"]["textAlignVertical"],
            serde_json::json!("middle")
        );

        let (_, params) = typography_command(
            &TypographyEdit::TextAutoResize(TextAutoResize::WidthAndHeight),
            &target,
        );
        assert_eq!(
            params["props"]["textAutoResize"],
            serde_json::json!("widthAndHeight")
        );

        let (_, params) = typography_command(
            &TypographyEdit::TextDecoration(TextDecoration::Strikethrough),
            &target,
        );
        assert_eq!(
            params["props"]["textDecoration"],
            serde_json::json!("strikethrough")
        );

        let (_, params) = typography_command(
            &TypographyEdit::TextDecorationStyle(DecorationStyle::Dotted),
            &target,
        );
        assert_eq!(
            params["props"]["textDecorationStyle"],
            serde_json::json!("dotted")
        );

        let (_, params) = typography_command(
            &TypographyEdit::TextDecorationThickness(LineHeight::Auto),
            &target,
        );
        assert_eq!(
            params["props"]["textDecorationThickness"],
            serde_json::json!("auto")
        );

        let (_, params) = typography_command(
            &TypographyEdit::TextDecorationOffset(LineHeight::Percent(10.0)),
            &target,
        );
        assert_eq!(
            params["props"]["textDecorationOffset"],
            serde_json::json!("10%")
        );

        let (_, params) = typography_command(&TypographyEdit::TextDecorationSkipInk(true), &target);
        assert_eq!(
            params["props"]["textDecorationSkipInk"],
            serde_json::json!(true)
        );

        let (_, params) = typography_command(
            &TypographyEdit::TextDecorationColor(Some(decoration_colour())),
            &target,
        );
        assert_eq!(
            params["props"]["textDecorationColor"],
            serde_json::json!("#336699")
        );

        let (_, params) = typography_command(&TypographyEdit::TextDecorationColor(None), &target);
        assert_eq!(
            params["props"]["textDecorationColor"],
            serde_json::json!("auto")
        );

        let (_, params) =
            typography_command(&TypographyEdit::TextCase(TextCase::SmallCaps), &target);
        assert_eq!(params["props"]["textCase"], serde_json::json!("smallCaps"));

        let (_, params) = typography_command(
            &TypographyEdit::LeadingTrim(LeadingTrim::CapHeight),
            &target,
        );
        assert_eq!(
            params["props"]["leadingTrim"],
            serde_json::json!("capHeight")
        );

        let (_, params) = typography_command(&TypographyEdit::ParagraphSpacing(8.0), &target);
        assert_eq!(params["props"]["paragraphSpacing"], serde_json::json!(8.0));

        let (_, params) = typography_command(&TypographyEdit::ParagraphIndent(4.0), &target);
        assert_eq!(params["props"]["paragraphIndent"], serde_json::json!(4.0));

        let (_, params) = typography_command(&TypographyEdit::ListSpacing(2.0), &target);
        assert_eq!(params["props"]["listSpacing"], serde_json::json!(2.0));

        let (_, params) = typography_command(&TypographyEdit::HangingPunctuation(true), &target);
        assert_eq!(
            params["props"]["hangingPunctuation"],
            serde_json::json!(true)
        );

        let (_, params) = typography_command(&TypographyEdit::HangingList(true), &target);
        assert_eq!(params["props"]["hangingList"], serde_json::json!(true));
    }

    #[test]
    fn the_commands_that_are_not_node_set_carry_the_engines_own_parameters() {
        let target = TypographyTarget {
            ids: vec![NodeId(3)],
            range: None,
        };
        let (command, params) =
            typography_command(&TypographyEdit::ListStyle(ListStyle::Numbered), &target);
        assert_eq!(command, "text.listStyle");
        assert_eq!(params["ids"], serde_json::json!([3]));
        assert_eq!(params["style"], serde_json::json!("numbered"));

        let (command, params) = typography_command(&TypographyEdit::MaxLines(3), &target);
        assert_eq!(command, "text.maxLines");
        assert_eq!(params["lines"], serde_json::json!(3.0));

        let (command, params) = typography_command(
            &TypographyEdit::Feature {
                tag: "tnum".to_owned(),
                on: Some(true),
            },
            &target,
        );
        assert_eq!(command, "text.setFeatures");
        assert_eq!(params["features"]["tnum"], serde_json::json!(true));

        // Back to the font's default clears the override.
        let (_, params) = typography_command(
            &TypographyEdit::Feature {
                tag: "tnum".to_owned(),
                on: None,
            },
            &target,
        );
        assert_eq!(params["features"]["tnum"], serde_json::Value::Null);

        let (command, params) = typography_command(
            &TypographyEdit::Variation {
                tag: "wght".to_owned(),
                value: Some(550.0),
            },
            &target,
        );
        assert_eq!(command, "text.setVariations");
        assert_eq!(params["variations"]["wght"], serde_json::json!(550.0));

        let (command, params) = typography_command(&TypographyEdit::PathStart(0.25), &target);
        assert_eq!(command, "text.pathOptions");
        assert_eq!(params["start"], serde_json::json!(0.25));

        let (command, params) = typography_command(&TypographyEdit::PathFlip(true), &target);
        assert_eq!(command, "text.pathOptions");
        assert_eq!(params["flip"], serde_json::json!(true));

        let (command, params) = typography_command(&TypographyEdit::RemoveFromPath, &target);
        assert_eq!(command, "text.removeFromPath");
        assert_eq!(params["ids"], serde_json::json!([3]));

        let (command, params) = typography_command(&TypographyEdit::PutOnPath, &target);
        assert_eq!(command, "text.onPath");
        assert_eq!(params["ids"], serde_json::json!([3]));
    }

    /// The spellings are the engine's, pinned against the engine's own list.
    #[test]
    fn every_prop_the_panel_writes_is_one_of_the_engines_own() {
        // The engine's own key list, read as the words it is made of: a
        // substring would let "align" pass for "textAlign" because the list
        // also holds "strokeAlign".
        let keys: Vec<&str> = figcraft_engine::cmd::props::KEYS
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|token| !token.is_empty())
            .collect();
        for edit in every_edit() {
            let Some((key, _)) = edit.prop() else {
                continue;
            };
            assert!(
                keys.contains(&key),
                "`{key}` is not one of the engine's node.set keys"
            );
        }
        // The value spellings, read out of the engine's own key list.
        let listed = figcraft_engine::cmd::props::KEYS;
        for spelling in [
            "left|center|right|justified",
            "widthAndHeight|height|none|truncate",
            "solid|dotted|wavy",
            "original|upper|lower|title|smallCaps",
            "none|capHeight",
            "auto|px|\"150%\"",
        ] {
            assert!(
                listed.contains(spelling),
                "`{spelling}` is not one of the engine's own value spellings"
            );
        }
    }

    /// Every value spelling is the engine's own, because the engine says so.
    ///
    /// The spellings the *panel* writes are written through `node.set` and
    /// read back out of the engine's document: a spelling the engine does not
    /// know is a spelling the panel would get wrong, and a silent no-op rather
    /// than a failure.
    #[test]
    fn every_value_spelling_is_the_engines_own() {
        let (doc, id) = document_with_text("Headline");
        let mut session = Session::with_document(doc);
        let write = |session: &mut Session, key: &str, value: JsonValue| {
            session
                .execute(
                    "node.set",
                    &serde_json::json!({ "ids": [id.0], "props": { key: value } }),
                )
                .expect("the engine accepts the spelling");
        };
        let text = |session: &Session| {
            session
                .doc()
                .expect("a document")
                .node(id)
                .expect("the layer")
                .kind
                .text()
                .expect("a text layer")
                .clone()
        };

        // The horizontal alignment.
        for expected in [
            TextAlignH::Left,
            TextAlignH::Center,
            TextAlignH::Right,
            TextAlignH::Justified,
        ] {
            write(
                &mut session,
                "textAlign",
                JsonValue::String(align_value(expected).to_owned()),
            );
            assert_eq!(text(&session).align_h, expected, "{:?}", expected);
        }

        // The vertical alignment.
        for expected in [TextAlignV::Top, TextAlignV::Middle, TextAlignV::Bottom] {
            write(
                &mut session,
                "textAlignVertical",
                JsonValue::String(align_vertical_value(expected).to_owned()),
            );
            assert_eq!(text(&session).align_v, expected, "{:?}", expected);
        }

        // The resizing.
        for expected in [
            TextAutoResize::WidthAndHeight,
            TextAutoResize::Height,
            TextAutoResize::None,
            TextAutoResize::Truncate,
        ] {
            write(
                &mut session,
                "textAutoResize",
                JsonValue::String(auto_resize_value(expected).to_owned()),
            );
            assert_eq!(text(&session).auto_resize, expected, "{:?}", expected);
        }

        // The decoration.
        for expected in [
            TextDecoration::None,
            TextDecoration::Underline,
            TextDecoration::Strikethrough,
        ] {
            write(
                &mut session,
                "textDecoration",
                JsonValue::String(decoration_value(expected).to_owned()),
            );
            assert_eq!(text(&session).style.decoration, expected, "{:?}", expected);
        }

        // The decoration's style.
        for expected in [
            DecorationStyle::Solid,
            DecorationStyle::Dotted,
            DecorationStyle::Wavy,
        ] {
            write(
                &mut session,
                "textDecorationStyle",
                JsonValue::String(decoration_style_value(expected).to_owned()),
            );
            assert_eq!(
                text(&session).style.look().style,
                expected,
                "{:?}",
                expected
            );
        }

        // The letter case.
        for expected in [
            TextCase::Original,
            TextCase::Upper,
            TextCase::Lower,
            TextCase::Title,
            TextCase::SmallCaps,
        ] {
            write(
                &mut session,
                "textCase",
                JsonValue::String(case_value(expected).to_owned()),
            );
            assert_eq!(text(&session).style.case, expected, "{:?}", expected);
        }

        // The vertical trim.
        for expected in [LeadingTrim::None, LeadingTrim::CapHeight] {
            write(
                &mut session,
                "leadingTrim",
                JsonValue::String(leading_trim_value(expected).to_owned()),
            );
            assert_eq!(text(&session).leading_trim, expected, "{:?}", expected);
        }

        // The line height: automatic, pixels, or a percentage.
        for expected in [
            LineHeight::Auto,
            LineHeight::Pixels(24.0),
            LineHeight::Percent(150.0),
        ] {
            write(&mut session, "lineHeight", line_height_value(expected));
            assert_eq!(text(&session).style.line_height, expected, "{:?}", expected);
        }

        // The letter spacing: pixels, or a percentage.
        for expected in [LetterSpacing::Pixels(1.5), LetterSpacing::Percent(2.0)] {
            write(
                &mut session,
                "letterSpacing",
                letter_spacing_value(expected),
            );
            assert_eq!(
                text(&session).style.letter_spacing,
                expected,
                "{:?}",
                expected
            );
        }

        // The decoration's lengths: automatic, pixels, or a percentage.
        for expected in [
            LineHeight::Auto,
            LineHeight::Pixels(3.0),
            LineHeight::Percent(10.0),
        ] {
            write(
                &mut session,
                "textDecorationThickness",
                line_height_value(expected),
            );
            assert_eq!(
                text(&session).style.look().thickness,
                expected,
                "{:?}",
                expected
            );
            write(
                &mut session,
                "textDecorationOffset",
                line_height_value(expected),
            );
            assert_eq!(
                text(&session).style.look().offset,
                expected,
                "{:?}",
                expected
            );
        }

        // The decoration's colour: a colour, or the text's own.
        write(
            &mut session,
            "textDecorationColor",
            JsonValue::String(format!("#{}", decoration_colour().to_hex())),
        );
        assert_eq!(text(&session).style.look().color, Some(decoration_colour()));
        write(
            &mut session,
            "textDecorationColor",
            JsonValue::String("auto".to_owned()),
        );
        assert_eq!(text(&session).style.look().color, None);

        // The list style goes through the engine's own command.
        for expected in [ListStyle::None, ListStyle::Bulleted, ListStyle::Numbered] {
            session
                .execute(
                    "text.listStyle",
                    &serde_json::json!({
                        "ids": [id.0],
                        "style": list_style_value(expected),
                    }),
                )
                .expect("the engine accepts the list style");
            assert_eq!(text(&session).list, expected, "{:?}", expected);
        }
    }

    /// The commands are the engine's own, so an edit made from the panel and an
    /// edit made from the CLI are the same edit.
    #[test]
    fn every_command_the_panel_runs_is_one_of_the_engines_own() {
        for id in [
            "node.set",
            "text.style",
            "text.listStyle",
            "text.maxLines",
            "text.setFeatures",
            "text.setVariations",
            "text.pathOptions",
            "text.removeFromPath",
            "text.onPath",
        ] {
            assert!(
                figcraft_engine::find_command(id).is_some(),
                "`{id}` is not an engine command"
            );
        }
    }

    /// Every edit the panel can make, so the spellings above cover them all.
    fn every_edit() -> Vec<TypographyEdit> {
        vec![
            TypographyEdit::FontFamily("Inter".to_owned()),
            TypographyEdit::FontStyle("Bold".to_owned()),
            TypographyEdit::FontSize(24.0),
            TypographyEdit::LineHeight(LineHeight::Auto),
            TypographyEdit::LetterSpacing(LetterSpacing::Percent(2.0)),
            TypographyEdit::ParagraphSpacing(8.0),
            TypographyEdit::ParagraphIndent(4.0),
            TypographyEdit::ListStyle(ListStyle::Bulleted),
            TypographyEdit::ListSpacing(2.0),
            TypographyEdit::HangingPunctuation(true),
            TypographyEdit::HangingList(true),
            TypographyEdit::LeadingTrim(LeadingTrim::CapHeight),
            TypographyEdit::MaxLines(3),
            TypographyEdit::TextAlign(TextAlignH::Left),
            TypographyEdit::TextAlignVertical(TextAlignV::Top),
            TypographyEdit::TextAutoResize(TextAutoResize::None),
            TypographyEdit::TextDecoration(TextDecoration::Underline),
            TypographyEdit::TextDecorationStyle(DecorationStyle::Solid),
            TypographyEdit::TextDecorationThickness(LineHeight::Auto),
            TypographyEdit::TextDecorationOffset(LineHeight::Auto),
            TypographyEdit::TextDecorationSkipInk(true),
            TypographyEdit::TextDecorationColor(None),
            TypographyEdit::TextCase(TextCase::Original),
            TypographyEdit::Feature {
                tag: "tnum".to_owned(),
                on: Some(true),
            },
            TypographyEdit::Variation {
                tag: "wght".to_owned(),
                value: Some(550.0),
            },
            TypographyEdit::PathStart(0.5),
            TypographyEdit::PathFlip(true),
            TypographyEdit::RemoveFromPath,
            TypographyEdit::PutOnPath,
        ]
    }

    #[test]
    fn a_mixed_selection_reads_mixed_for_the_property_that_differs() {
        // Two text layers that agree about everything except their size.
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let small = doc
            .create(
                page,
                NodeKind::Text(TextData::new("Small")),
                "Small",
                0.0,
                0.0,
                100.0,
                30.0,
            )
            .expect("a text layer");
        let large = doc
            .create(
                page,
                NodeKind::Text(TextData::new("Large")),
                "Large",
                0.0,
                40.0,
                100.0,
                30.0,
            )
            .expect("a text layer");
        let mut session = Session::with_document(doc);
        session
            .execute(
                "node.set",
                &serde_json::json!({
                    "ids": [small.0],
                    "props": { "fontSize": 24.0, "textAlign": "center" },
                }),
            )
            .expect("the engine accepts the properties");
        session
            .execute(
                "node.set",
                &serde_json::json!({
                    "ids": [large.0],
                    "props": { "fontSize": 32.0, "textAlign": "center" },
                }),
            )
            .expect("the engine accepts the properties");
        session
            .execute(
                "select.set",
                &serde_json::json!({ "ids": [small.0, large.0] }),
            )
            .expect("both layers are selected");

        let model = TypographyModel::from_session(&session);

        // The property that differs is mixed...
        assert_eq!(model.font_size, TypographyValue::Mixed);
        // ...and the ones that agree are their value, because the fold is per
        // property: one differing size must not hide the rest.
        assert_eq!(model.font_family, TypographyValue::Set("Inter".to_owned()));
        assert_eq!(model.text_align, TypographyValue::Set(TextAlignH::Center));
        assert_eq!(
            model.text_align_vertical,
            TypographyValue::Set(TextAlignV::Top)
        );
        assert_eq!(
            model.text_auto_resize,
            TypographyValue::Set(TextAutoResize::WidthAndHeight)
        );
        assert_eq!(model.paragraph_spacing, TypographyValue::Set(0.0));
    }

    #[test]
    fn the_words_are_the_ones_the_inspector_uses() {
        // "Mixed" for a disagreement, "Not set" for an absence.
        assert_eq!(MIXED, "Mixed");
        assert_eq!(NOT_SET, "Not set");
        assert_eq!(AUTO, "Auto");
        let mixed: TypographyValue<f32> = TypographyValue::Mixed;
        assert_eq!(mixed.label(|value| value.to_string()), MIXED);
        let unset: TypographyValue<f32> = TypographyValue::Unset;
        assert_eq!(unset.label(|value| value.to_string()), NOT_SET);
        let set: TypographyValue<f32> = TypographyValue::Set(12.0);
        assert_eq!(set.label(|value| value.to_string()), "12");
    }

    #[test]
    fn the_spacing_fields_take_the_engines_own_units() {
        // "Auto", a number of pixels, or a percentage of the font size.
        assert_eq!(parse_line_height("Auto", None), Some(LineHeight::Auto));
        assert_eq!(
            parse_line_height("150%", Some(SpacingUnit::Auto)),
            Some(LineHeight::Percent(150.0))
        );
        assert_eq!(
            parse_line_height("24", Some(SpacingUnit::Auto)),
            Some(LineHeight::Pixels(24.0))
        );
        assert_eq!(
            parse_line_height("24px", Some(SpacingUnit::Percent)),
            Some(LineHeight::Pixels(24.0))
        );
        // A bare number keeps the unit the current value has.
        assert_eq!(
            parse_line_height("130", Some(SpacingUnit::Percent)),
            Some(LineHeight::Percent(130.0))
        );
        assert_eq!(
            parse_line_height("-5", Some(SpacingUnit::Pixels)),
            Some(LineHeight::Pixels(0.0))
        );
        // An untouched mixed field commits nothing.
        assert_eq!(parse_line_height("", None), None);
        assert_eq!(parse_line_height("abc", None), None);
        assert_eq!(
            parse_line_height("", Some(SpacingUnit::Pixels)),
            Some(LineHeight::Auto)
        );

        assert_eq!(
            parse_letter_spacing("2%", None),
            Some(LetterSpacing::Percent(2.0))
        );
        assert_eq!(
            parse_letter_spacing("1.5px", None),
            Some(LetterSpacing::Pixels(1.5))
        );
        assert_eq!(
            parse_letter_spacing("3", Some(SpacingUnit::Pixels)),
            Some(LetterSpacing::Pixels(3.0))
        );
        assert_eq!(
            parse_letter_spacing("-1", None),
            Some(LetterSpacing::Percent(-1.0))
        );
        assert_eq!(parse_letter_spacing("x", Some(SpacingUnit::Percent)), None);

        // The decoration's lengths: automatic, pixels or a percentage.
        assert_eq!(parse_decoration_length("auto"), Some(LineHeight::Auto));
        assert_eq!(parse_decoration_length("2"), Some(LineHeight::Pixels(2.0)));
        assert_eq!(
            parse_decoration_length("10%"),
            Some(LineHeight::Percent(10.0))
        );
    }

    #[test]
    fn a_typed_value_becomes_one_change_and_an_escape_changes_nothing() {
        let target = TypographyTarget {
            ids: vec![NodeId(3)],
            range: None,
        };
        let mut state = TypographyPanelState::new();
        state.draft = Some(TypographyDraft {
            field: TypographyField::LineHeight,
            text: "150%".to_owned(),
            select_all: true,
            target: target.clone(),
            unit: Some(SpacingUnit::Auto),
        });
        assert!(state.is_busy());
        let action = state
            .handle_key("enter")
            .expect("the typed value is a change");
        assert_eq!(
            action,
            TypographyAction::Edit {
                edit: TypographyEdit::LineHeight(LineHeight::Percent(150.0)),
                target,
            }
        );
        assert!(!state.is_busy());

        // Escape gives the buffer up without touching the document.
        state.draft = Some(TypographyDraft {
            field: TypographyField::LineHeight,
            text: "150%".to_owned(),
            select_all: true,
            target: TypographyTarget::default(),
            unit: Some(SpacingUnit::Auto),
        });
        assert!(state.handle_key("escape").is_none());
        assert!(!state.is_busy());
    }

    // ── In a window ──────────────────────────────────────────────────────────

    /// The panel's host in a test: it runs the changes through the engine.
    struct Panel {
        canvas: Entity<crate::engine::canvas::EngineCanvas>,
        state: Rc<RefCell<TypographyPanelState>>,
        /// The changes the panel asked for, in order.
        actions: Rc<RefCell<Vec<TypographyAction>>>,
    }

    impl TypographyHost for Panel {
        fn run_typography_action(&mut self, action: TypographyAction, cx: &mut Context<Self>) {
            self.actions.borrow_mut().push(action.clone());
            let (id, params) = action.command();
            self.canvas
                .update(cx, |canvas, cx| canvas.run_engine_command(&id, params, cx));
        }
    }

    impl Render for Panel {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let model = self
                .canvas
                .read(cx)
                .with_session(TypographyModel::from_session);
            section(&model, &self.state, cx)
        }
    }

    /// Open a window with the typography panel in it, over the given session.
    fn open_panel(
        cx: &mut TestAppContext,
        session: std::rc::Rc<std::cell::RefCell<Session>>,
    ) -> (Entity<Panel>, &mut VisualTestContext) {
        let (root, window) = cx.add_window_view(move |_, cx| {
            let canvas = cx.new(|cx| crate::engine::canvas::EngineCanvas::from_handle(session, cx));
            Panel {
                canvas,
                state: Rc::new(RefCell::new(TypographyPanelState::new())),
                actions: Rc::new(RefCell::new(Vec::new())),
            }
        });
        (root, window)
    }

    /// Draw the panel in the window.
    fn draw(window: &mut VisualTestContext, panel: &Entity<Panel>) {
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(900.0)),
            |_, _| panel.clone().into_any_element(),
        );
        window.run_until_parked();
    }

    /// A session over a document with one text layer, selected.
    fn session_with_text() -> std::rc::Rc<std::cell::RefCell<Session>> {
        let (doc, id) = document_with_text("Headline");
        let mut session = Session::with_document(doc);
        session
            .execute("select.set", &serde_json::json!({ "ids": [id.0] }))
            .expect("the layer is selected");
        Rc::new(RefCell::new(session))
    }

    #[gpui::test]
    fn the_panel_draws_the_typography_of_a_text_layer(cx: &mut TestAppContext) {
        // The render test: the panel, on its own, in a real window, showing the
        // typography of the text the engine's document holds.
        let session = session_with_text();
        let (panel, window) = open_panel(cx, session);
        draw(window, &panel);

        let bounds = window
            .debug_bounds("typography-family-trigger")
            .expect("the panel draws the font family");
        assert!(
            bounds.size.height > px(0.0),
            "the font family is on screen: {bounds:?}"
        );
        assert!(
            bounds.size.width > px(0.0),
            "the font family is on screen: {bounds:?}"
        );
        // And the rows below it.
        for selector in [
            "typography-style-trigger",
            "typography-line-height-field",
            "typography-letter-spacing-field",
            "typography-paragraph-spacing-field",
            "typography-align-0",
            "typography-align-vertical-0",
            "typography-resize-0",
            "typography-settings",
        ] {
            assert!(
                window.debug_bounds(selector).is_some(),
                "the panel draws {selector}"
            );
        }
    }

    #[gpui::test]
    fn the_panel_draws_nothing_for_a_layer_with_no_text(cx: &mut TestAppContext) {
        // The empty state: figcraft draws nothing at all when the selection
        // has no text layer in it.
        let (doc, id) = document_with_rectangle();
        let mut session = Session::with_document(doc);
        session
            .execute("select.set", &serde_json::json!({ "ids": [id.0] }))
            .expect("the layer is selected");
        let (panel, window) = open_panel(cx, Rc::new(RefCell::new(session)));
        draw(window, &panel);

        assert!(
            window.debug_bounds("typography-family-trigger").is_none(),
            "a layer with no text shows no typography at all"
        );
    }

    #[test]
    fn a_layer_with_no_text_has_no_typography_to_show() {
        // The empty state, read out of the engine's document.
        let (doc, id) = document_with_rectangle();
        let session = session_with(doc, id);
        let model = TypographyModel::from_session(&session);
        assert!(model.is_empty());
        assert!(!model.has_text);
        assert!(model.ids.is_empty());
        assert_eq!(model.font_size, TypographyValue::Unset);
    }
}
