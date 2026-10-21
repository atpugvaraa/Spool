//! The Export section: figcraft's export-settings panel, ported to GPUI.
//!
//! # What this module owns
//!
//! Two things, kept apart on purpose:
//!
//! 1. [`ExportModel`] — what the selection's export settings *are*, read out
//!    of the engine's document, and the engine commands that change them.
//! 2. [`section`] — the drawing of that model: the rows, the options, the
//!    labels, the defaults and the empty state.
//!
//! # The engine owns the document
//!
//! Nothing here changes a setting directly. Every change is an engine command
//! — `export.add`, `export.update`, `export.remove`, and `node.set` for the
//! one case the engine has no other command for — built with the engine's own
//! parameter names and value spellings. A change made from this panel and a
//! change made from the CLI are the same change and land in the same history.
//!
//! # Three states, folded one property at a time
//!
//! A property of a *selection* is one of three things: a value every selected
//! layer agrees on, agreement that nobody set it, or the absence of
//! agreement. [`ExportValue`] is that fold, and the fold is per property: a
//! disagreement about the format says "Mixed" in the format field and leaves
//! the size and the options alone. Folding the whole setting instead would
//! hide every other property behind the one that differs.
//!
//! # The words
//!
//! "Mixed" and "Not set" are the Inspector's words, reused rather than
//! reinvented: a panel that says "Mixed" in one section and "Various" in
//! another makes the reader learn two vocabularies for one thing.
//!
//! # What is not here
//!
//! figcraft's export panel ends with a rendered preview of the layer and an
//! "Export N layers" button. Neither is a *setting*, and neither belongs to a
//! section of the Inspector: the preview needs the layer's own pixels, and the
//! button needs a file dialog. Spool's File menu already dispatches the
//! engine's own `file.export` and `file.exportSvg` commands, so the export
//! itself is one keystroke away; what the Inspector owns is the settings that
//! decide what an export would be.

use std::cell::RefCell;
use std::rc::Rc;

use figcraft_doc::{Document, ExportFormat, ExportSetting, ExportSize, NodeId, PrintSettings};
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, SharedString};
use serde_json::{json, Value};

use crate::theme;

// ═══════════════════════════════════════════════════════════════════════════
// Words
// ═══════════════════════════════════════════════════════════════════════════

/// The word for a property the selected layers disagree about.
pub const MIXED: &str = "Mixed";

/// The word for a property nobody has set.
///
/// The engine's word, not the panel's: a paint nobody filled is "None" in the
/// Inspector, but a value the engine's model does not have is "Not set".
pub const NOT_SET: &str = "Not set";

// ═══════════════════════════════════════════════════════════════════════════
// The three states
// ═══════════════════════════════════════════════════════════════════════════

/// One property's state across the selection.
///
/// Three states, never two. [`ExportValue::Set`] is a value every selected
/// layer agrees on, [`ExportValue::Unset`] is agreement that the property was
/// never authored, and [`ExportValue::Mixed`] is the absence of agreement. A
/// panel that only had a value and a nothing would have to invent a third
/// thing at every row that faced a disagreement, and inventing it per row is
/// how a panel starts showing values the user never chose.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportValue<T> {
    /// Every selected layer has this value, and they agree on it.
    Set(T),
    /// No selected layer has this value.
    Unset,
    /// The selected layers disagree.
    Mixed,
}

/// A copyable value is a copyable state: the flags and the numbers the panel
/// reads are small, and copying one is cheaper than cloning one.
impl<T: Copy> Copy for ExportValue<T> {}

impl<T: PartialEq> ExportValue<T> {
    /// Fold a selection's readings into the one state that is honest for all
    /// of them.
    ///
    /// A reading of `None` means "this property is not set on that layer",
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
                // no later agreement can make the selection uniform again, so
                // `Mixed` absorbs everything after it.
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

    /// Whether nobody has set it.
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

// ═══════════════════════════════════════════════════════════════════════════
// The model
// ═══════════════════════════════════════════════════════════════════════════

/// One export setting, as the selection presents it.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportRow {
    /// Which setting this is: its index in the engine's list.
    pub index: usize,
    /// The size the export is made at: "2x", "512w", "512h".
    pub size: ExportValue<ExportSize>,
    /// The suffix the exported file's name gets after the layer's name.
    ///
    /// An empty suffix is a suffix nobody set, so it reads as
    /// [`ExportValue::Unset`] rather than as an empty string.
    pub suffix: ExportValue<String>,
    /// The file format.
    pub format: ExportValue<ExportFormat>,
    /// Only the layer itself, not everything that shows in its area.
    pub contents_only: ExportValue<bool>,
    /// The layer's own box, cutting off what it paints outside it.
    pub include_bounding_box: ExportValue<bool>,
    /// SVG: the text as outlines rather than as `<text>` elements.
    pub svg_outline_text: ExportValue<bool>,
    /// SVG: the layer names as `id` attributes.
    pub svg_id_attribute: ExportValue<bool>,
    /// SVG: the strokes as `stroke` attributes where that is exact.
    pub svg_simplify_stroke: ExportValue<bool>,
    /// The print settings, for a PDF.
    pub print: PrintRow,
}

/// A PDF setting's print settings, as the selection presents them.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintRow {
    /// The bleed around each page, in points.
    pub bleed: ExportValue<f64>,
    /// Crop marks outside the bleed, at the trim's corners.
    pub crop_marks: ExportValue<bool>,
    /// Bleed marks outside the bleed, at the bleed's corners.
    pub bleed_marks: ExportValue<bool>,
    /// CMYK colour, converted from RGB.
    pub cmyk: ExportValue<bool>,
}

/// The export settings of the selection, as the panel shows them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExportModel {
    /// The layers the model was read for.
    pub ids: Vec<NodeId>,
    /// One row per export setting, in the engine's order.
    pub rows: Vec<ExportRow>,
    /// Whether the selection disagrees about the settings as a whole.
    ///
    /// The engine's own comparison: the layers' lists of settings are not all
    /// the same. This is what the "+" button replaces. The rows themselves are
    /// folded property by property, so one difference does not hide the rest.
    pub mixed: bool,
}

impl ExportModel {
    /// The command that adds an export setting to the selection.
    ///
    /// Without a setting of its own, the engine adds the one that follows the
    /// last: 1x first, then one scale step up with a matching "@2x"-style
    /// suffix. A selection whose settings disagree gets one default setting
    /// instead, which is the only way to make a mixed selection agree.
    pub fn add_command(&self) -> (String, Value) {
        add_command(&self.ids, self.mixed)
    }

    /// The command that removes one export setting from the selection.
    pub fn remove_command(&self, index: usize) -> (String, Value) {
        remove_command(&self.ids, index)
    }

    /// The command that changes one property of one export setting.
    pub fn update_command(&self, index: usize, edit: &ExportEdit) -> (String, Value) {
        update_command(&self.ids, index, edit)
    }
}

/// The export settings of the selected layers, as the panel shows them.
///
/// Every property is folded on its own: a disagreement about the format says
/// "Mixed" in the format field and leaves the size and the options alone.
/// Folding the whole setting instead would hide every other property behind
/// the one that differs.
pub fn selected_exports(doc: &Document, selection: &[NodeId]) -> ExportModel {
    let lists: Vec<&[ExportSetting]> = selection
        .iter()
        .filter_map(|id| doc.node(*id))
        .map(|node| node.exports.as_slice())
        .collect();
    let count = lists.iter().map(|list| list.len()).max().unwrap_or(0);
    let rows = (0..count).map(|index| row_at(&lists, index)).collect();
    let mixed = lists.len() > 1 && lists.iter().any(|list| *list != lists[0]);
    ExportModel {
        ids: selection.to_vec(),
        rows,
        mixed,
    }
}

/// The export settings of one layer, as the panel shows them.
///
/// `None` for a layer the document does not hold: a selection that outlived
/// its layer is reported rather than shown as an empty one.
pub fn exports_of(doc: &Document, id: NodeId) -> Option<ExportModel> {
    doc.node(id)?;
    Some(selected_exports(doc, std::slice::from_ref(&id)))
}

/// One setting, read across the selection.
fn row_at(lists: &[&[ExportSetting]], index: usize) -> ExportRow {
    ExportRow {
        index,
        size: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).map(|s| s.size)),
        ),
        suffix: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).and_then(suffix_of)),
        ),
        format: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).map(|s| s.format)),
        ),
        contents_only: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).map(|s| s.contents_only)),
        ),
        include_bounding_box: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).map(|s| s.include_bounding_box)),
        ),
        svg_outline_text: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).map(|s| s.svg_outline_text)),
        ),
        svg_id_attribute: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).map(|s| s.svg_id_attribute)),
        ),
        svg_simplify_stroke: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).map(|s| s.svg_simplify_stroke)),
        ),
        print: print_at(lists, index),
    }
}

/// One setting's print settings, read across the selection.
fn print_at(lists: &[&[ExportSetting]], index: usize) -> PrintRow {
    PrintRow {
        bleed: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).map(|s| s.print.bleed())),
        ),
        crop_marks: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).map(|s| s.print.crop_marks)),
        ),
        bleed_marks: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).map(|s| s.print.bleed_marks)),
        ),
        cmyk: ExportValue::across(
            lists
                .iter()
                .map(|list| setting_at(list, index).map(|s| s.print.cmyk)),
        ),
    }
}

/// The setting at `index`, if the layer has one.
fn setting_at(list: &[ExportSetting], index: usize) -> Option<&ExportSetting> {
    list.get(index)
}

/// The suffix of a setting, when it has one.
///
/// An empty suffix is the engine's default, and the engine's default means
/// "no suffix": a file name with nothing after it. Reading it as "not set" is
/// what lets the panel say so instead of showing an empty field.
fn suffix_of(setting: &ExportSetting) -> Option<String> {
    if setting.suffix.is_empty() {
        None
    } else {
        Some(setting.suffix.clone())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The commands
// ═══════════════════════════════════════════════════════════════════════════

/// A change the export panel can ask for.
///
/// One variant per thing the panel can change, so a change the engine has no
/// command for cannot be written down as though it had one.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportAction {
    /// Add an export setting to the selection.
    Add {
        /// The layers to add it to.
        ids: Vec<NodeId>,
        /// Whether the selection's settings disagree.
        ///
        /// A selection that disagrees is replaced by one default setting: that
        /// is the only way to make it agree.
        mixed: bool,
    },
    /// Remove the export setting at an index.
    Remove {
        /// The layers to remove it from.
        ids: Vec<NodeId>,
        /// Which setting.
        index: usize,
    },
    /// Change one property of one export setting.
    Update {
        /// The layers to change it on.
        ids: Vec<NodeId>,
        /// Which setting.
        index: usize,
        /// What to change.
        edit: ExportEdit,
    },
}

impl ExportAction {
    /// The engine's command id and parameters for this change.
    ///
    /// The engine's own spellings, so a change made from the panel is the
    /// same change the CLI, the keymap and an agent would make.
    pub fn command(&self) -> (String, Value) {
        match self {
            Self::Add { ids, mixed } => add_command(ids, *mixed),
            Self::Remove { ids, index } => remove_command(ids, *index),
            Self::Update { ids, index, edit } => update_command(ids, *index, edit),
        }
    }
}

/// One change to one export setting.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportEdit {
    /// The size the export is made at.
    Size(ExportSize),
    /// The suffix the exported file's name gets.
    Suffix(String),
    /// The file format.
    Format(ExportFormat),
    /// Only the layer itself.
    ContentsOnly(bool),
    /// The layer's own box.
    IncludeBoundingBox(bool),
    /// SVG: the text as outlines.
    SvgOutlineText(bool),
    /// SVG: the layer names as `id` attributes.
    SvgIdAttribute(bool),
    /// SVG: the strokes as `stroke` attributes.
    SvgSimplifyStroke(bool),
    /// One of the print settings, for a PDF.
    Print(PrintEdit),
}

/// One change to a PDF setting's print settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrintEdit {
    /// The bleed around each page, in points.
    Bleed(f64),
    /// Crop marks outside the bleed.
    CropMarks(bool),
    /// Bleed marks outside the bleed.
    BleedMarks(bool),
    /// CMYK colour instead of RGB.
    Cmyk(bool),
}

/// The command that adds an export setting to a selection.
pub fn add_command(ids: &[NodeId], mixed: bool) -> (String, Value) {
    if mixed {
        (
            "node.set".to_owned(),
            json!({
                "ids": ids,
                "props": { "exports": [default_setting()] }
            }),
        )
    } else {
        ("export.add".to_owned(), json!({ "ids": ids }))
    }
}

/// The command that removes one export setting from a selection.
pub fn remove_command(ids: &[NodeId], index: usize) -> (String, Value) {
    (
        "export.remove".to_owned(),
        json!({ "ids": ids, "index": index }),
    )
}

/// The command that changes one property of one export setting.
pub fn update_command(ids: &[NodeId], index: usize, edit: &ExportEdit) -> (String, Value) {
    ("export.update".to_owned(), update_params(ids, index, edit))
}

/// The parameters of an `export.update` that changes one property.
///
/// The engine's own names and value spellings: `size` is "2x" or "512w",
/// `format` is the file extension, the options are camelCase, and the print
/// options are `bleed`, `cropMarks`, `bleedMarks` and `cmyk`.
///
/// One property per command, because the engine merges: a key that is not in
/// the parameters keeps the value the setting already had. That is what lets a
/// selection that disagrees about the bleed still turn the crop marks on
/// without the panel having to invent a bleed for it.
fn update_params(ids: &[NodeId], index: usize, edit: &ExportEdit) -> Value {
    let mut params = json!({ "ids": ids, "index": index });
    match edit {
        ExportEdit::Size(size) => params["size"] = json!(size.label()),
        ExportEdit::Suffix(suffix) => params["suffix"] = json!(suffix),
        ExportEdit::Format(format) => params["format"] = json!(format.ext()),
        ExportEdit::ContentsOnly(on) => params["contentsOnly"] = json!(on),
        ExportEdit::IncludeBoundingBox(on) => params["includeBoundingBox"] = json!(on),
        ExportEdit::SvgOutlineText(on) => params["svgOutlineText"] = json!(on),
        ExportEdit::SvgIdAttribute(on) => params["svgIdAttribute"] = json!(on),
        ExportEdit::SvgSimplifyStroke(on) => params["svgSimplifyStroke"] = json!(on),
        ExportEdit::Print(print) => match print {
            PrintEdit::Bleed(bleed) => params["bleed"] = json!(bleed),
            PrintEdit::CropMarks(on) => params["cropMarks"] = json!(on),
            PrintEdit::BleedMarks(on) => params["bleedMarks"] = json!(on),
            PrintEdit::Cmyk(on) => params["cmyk"] = json!(on),
        },
    }
    params
}

/// The engine's default export setting, in the engine's own spelling.
///
/// 1x, no suffix and PNG, with every option at its default — which is what
/// the engine writes out, because an option that is at its default is not
/// written at all.
fn default_setting() -> Value {
    json!({ "size": { "by": "scale", "value": 1.0 }, "suffix": "", "format": "png" })
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel's own state
// ═══════════════════════════════════════════════════════════════════════════

/// Which menu of the section is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportMenu {
    /// The size presets of one setting.
    Size(usize),
    /// The format list of one setting.
    Format(usize),
    /// The advanced options of one setting.
    Options(usize),
    /// The print settings of one PDF setting.
    Print(usize),
}

/// Which field is being typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportField {
    /// The size of one setting.
    Size(usize),
    /// The suffix of one setting.
    Suffix(usize),
    /// The bleed of one PDF setting.
    Bleed(usize),
}

/// The text being typed into one field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportDraft {
    /// Which field is being typed into.
    pub field: ExportField,
    /// What has been typed.
    pub text: String,
    /// The layers the edit applies to.
    ///
    /// Taken when the field is opened, because a commit is a command and a
    /// command names its layers: the selection at the moment of the edit.
    pub ids: Vec<NodeId>,
    /// Whether the text is selected, so the first key replaces it.
    pub select_all: bool,
}

/// The panel's own state.
///
/// The state is a cell the surface that hosts the panel owns, so a menu stays
/// open across repaints while the panel itself stays a function.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportPanelState {
    /// Which menu is open, if any.
    pub open: Option<ExportMenu>,
    /// The text being typed, if any.
    pub draft: Option<ExportDraft>,
}

impl ExportPanelState {
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
        self.draft.is_some()
    }

    /// Give up whatever the section was part-way through.
    ///
    /// Returns whether anything was given up, so the ladder knows the key was
    /// consumed.
    pub fn cancel(&mut self) -> bool {
        self.draft.take().is_some()
    }

    /// Handle one keystroke.
    ///
    /// Returns the change the key asks for, if the key was the section's own.
    /// Escape is not claimed: it belongs to the editor's ladder, which walks
    /// down from the topmost thing, and this draft is one of the rungs the
    /// ladder knows about.
    pub fn handle_key(&mut self, key: &str) -> Option<ExportAction> {
        let draft = self.draft.as_mut()?;
        match key {
            "enter" | "return" => {
                let draft = self.draft.take()?;
                commit_draft(&draft)
            }
            "backspace" => {
                // With the text selected, Backspace deletes the selection —
                // the same rule typing follows. Treating it as "delete one
                // character" would leave the user staring at the rest of the
                // old value after the key they expected to clear it.
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
/// `None` for a value the engine would refuse: a size that is not a size and
/// a bleed longer than two inches leave the setting exactly as it was.
fn commit_draft(draft: &ExportDraft) -> Option<ExportAction> {
    let index = match draft.field {
        ExportField::Size(index) | ExportField::Suffix(index) | ExportField::Bleed(index) => index,
    };
    let edit = match draft.field {
        ExportField::Size(_) => ExportEdit::Size(ExportSize::parse(&draft.text)?),
        ExportField::Suffix(_) => ExportEdit::Suffix(draft.text.clone()),
        ExportField::Bleed(_) => {
            let bleed = PrintSettings::parse_length(&draft.text)?;
            if bleed > PrintSettings::MAX_BLEED {
                return None;
            }
            ExportEdit::Print(PrintEdit::Bleed(bleed))
        }
    };
    Some(ExportAction::Update {
        ids: draft.ids.clone(),
        index,
        edit,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// What a surface that hosts the export panel must be able to do.
///
/// The panel draws and asks; the surface owns the engine and runs the changes.
/// The panel never touches the document itself, so the engine's command surface
/// stays the one operation boundary.
pub trait ExportHost: Sized + 'static {
    /// Run a change through the engine.
    fn run_export_action(&mut self, action: ExportAction, cx: &mut Context<Self>);
}

/// The Export section, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own state,
/// and the surface that runs the changes. It returns an element, so the
/// Inspector can put it in a section of its own and a test can put it in a
/// window on its own.
pub fn section<T: ExportHost>(
    model: &ExportModel,
    state: &Rc<RefCell<ExportPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2().px(px(14.0)).py(px(12.0));
    column = column.child(add_row(model, cx));
    if model.mixed {
        column = column.child(note("Click + to replace mixed content"));
    }
    for row in &model.rows {
        column = column.child(setting_row(model, row, state, cx));
    }
    column.into_any_element()
}

/// The row that adds an export setting.
///
/// figcraft puts the "+" in the section header; Spool's Inspector draws its
/// own header, so the button is the first row of the body — the same control,
/// in the place Spool has for it.
fn add_row<T: ExportHost>(model: &ExportModel, cx: &mut Context<T>) -> impl IntoElement {
    let label = if model.mixed {
        "Replace mixed export settings"
    } else {
        "Add export setting"
    };
    let action = ExportAction::Add {
        ids: model.ids.clone(),
        mixed: model.mixed,
    };
    div()
        .id("export-add")
        .debug_selector(|| "export-add".to_owned())
        .flex()
        .items_center()
        .gap_2()
        .px(px(10.0))
        .py(px(9.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .child("＋")
        .child(label)
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_export_action(action.clone(), cx);
        }))
}

/// One export setting: its size, suffix, format, options and remove button.
/// A PDF setting has its print settings underneath.
fn setting_row<T: ExportHost>(
    model: &ExportModel,
    row: &ExportRow,
    state: &Rc<RefCell<ExportPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let mut block = column()
        .gap_2()
        .id(SharedString::from(format!("export-setting-{index}")))
        .debug_selector(move || format!("export-setting-{index}"));
    block = block.child(
        div()
            .flex()
            .gap_2()
            .child(size_field(model, row, state, cx))
            .child(suffix_field(model, row, state, cx)),
    );
    block = block.child(
        div()
            .flex()
            .gap_2()
            .child(format_field(row, state, cx))
            .child(options_button(row, state, cx))
            .child(remove_button(model, row, cx)),
    );
    let open = state.borrow().open;
    if open == Some(ExportMenu::Size(index)) {
        block = block.child(size_menu(model, row, cx));
    }
    if open == Some(ExportMenu::Format(index)) {
        block = block.child(format_menu(model, row, cx));
    }
    if open == Some(ExportMenu::Options(index)) {
        block = block.child(options_menu(model, row, cx));
    }
    if row.format.value() == Some(&ExportFormat::Pdf) {
        block = block.child(print_row(model, row, state, cx));
    }
    block
}

/// The size field: the size, typed into or picked from the presets.
fn size_field<T: ExportHost>(
    model: &ExportModel,
    row: &ExportRow,
    state: &Rc<RefCell<ExportPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let current = match &row.size {
        ExportValue::Set(size) => Some(size.label()),
        _ => None,
    };
    let value_text = match &row.size {
        ExportValue::Set(size) => size.label(),
        ExportValue::Unset => NOT_SET.to_owned(),
        ExportValue::Mixed => MIXED.to_owned(),
    };
    let shown = field_text(state, ExportField::Size(index), value_text.clone());
    let editing = is_editing(state, ExportField::Size(index));
    // Typing into the field replaces the size; the chevron beside it picks
    // one of the presets instead.
    let draft = ExportDraft {
        field: ExportField::Size(index),
        text: current.unwrap_or_default(),
        ids: model.ids.clone(),
        select_all: true,
    };
    let shared = Rc::clone(state);
    let field = field_box(
        shown,
        Some(78.0),
        editing,
        format!("export-size-value-{index}-{value_text}"),
    )
    .id(SharedString::from(format!("export-size-{index}")))
    .debug_selector(move || format!("export-size-{index}"))
    .on_click(cx.listener(move |_host, _event, _window, cx| {
        {
            let mut state = shared.borrow_mut();
            state.open = None;
            state.draft = Some(draft.clone());
        }
        cx.notify();
    }));
    let shared = Rc::clone(state);
    let chevron = div()
        .id(SharedString::from(format!("export-size-menu-{index}")))
        .debug_selector(move || format!("export-size-menu-{index}"))
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .cursor_pointer()
        .child("▾")
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            // The chevron is inside the field, so the field's own click would
            // otherwise open the editor as well as the menu.
            cx.stop_propagation();
            {
                let mut state = shared.borrow_mut();
                state.open = if state.open == Some(ExportMenu::Size(index)) {
                    None
                } else {
                    Some(ExportMenu::Size(index))
                };
                state.draft = None;
            }
            cx.notify();
        }));
    field.child(chevron)
}

/// The suffix field: what the exported file's name gets after the layer's.
fn suffix_field<T: ExportHost>(
    model: &ExportModel,
    row: &ExportRow,
    state: &Rc<RefCell<ExportPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let current = match &row.suffix {
        ExportValue::Set(suffix) => Some(suffix.clone()),
        _ => None,
    };
    let value_text = match &row.suffix {
        ExportValue::Set(suffix) => suffix.clone(),
        ExportValue::Unset => NOT_SET.to_owned(),
        ExportValue::Mixed => MIXED.to_owned(),
    };
    let shown = field_text(state, ExportField::Suffix(index), value_text.clone());
    let editing = is_editing(state, ExportField::Suffix(index));
    let draft = ExportDraft {
        field: ExportField::Suffix(index),
        text: current.unwrap_or_default(),
        ids: model.ids.clone(),
        select_all: true,
    };
    let shared = Rc::clone(state);
    field_box(
        shown,
        None,
        editing,
        format!("export-suffix-value-{index}-{value_text}"),
    )
    .id(SharedString::from(format!("export-suffix-{index}")))
    .debug_selector(move || format!("export-suffix-{index}"))
    .on_click(cx.listener(move |_host, _event, _window, cx| {
        {
            let mut state = shared.borrow_mut();
            state.open = None;
            state.draft = Some(draft.clone());
        }
        cx.notify();
    }))
}

/// The format field: the file format, picked from a list.
fn format_field<T: ExportHost>(
    row: &ExportRow,
    state: &Rc<RefCell<ExportPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let value_text = match &row.format {
        ExportValue::Set(format) => format.label().to_owned(),
        ExportValue::Unset => NOT_SET.to_owned(),
        ExportValue::Mixed => MIXED.to_owned(),
    };
    let shared = Rc::clone(state);
    field_box(
        value_text.clone(),
        None,
        false,
        format!("export-format-value-{index}-{value_text}"),
    )
    .id(SharedString::from(format!("export-format-{index}")))
    .debug_selector(move || format!("export-format-{index}"))
    .cursor_pointer()
    .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
    .on_click(cx.listener(move |_host, _event, _window, cx| {
        {
            let mut state = shared.borrow_mut();
            state.open = if state.open == Some(ExportMenu::Format(index)) {
                None
            } else {
                Some(ExportMenu::Format(index))
            };
            state.draft = None;
        }
        cx.notify();
    }))
}

/// The button that opens a setting's advanced options.
fn options_button<T: ExportHost>(
    row: &ExportRow,
    state: &Rc<RefCell<ExportPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let shared = Rc::clone(state);
    div()
        .id(SharedString::from(format!("export-options-{index}")))
        .debug_selector(move || format!("export-options-{index}"))
        .flex()
        .items_center()
        .justify_center()
        .w(px(24.0))
        .py(px(6.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .child("⋯")
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = if state.open == Some(ExportMenu::Options(index)) {
                    None
                } else {
                    Some(ExportMenu::Options(index))
                };
                state.draft = None;
            }
            cx.notify();
        }))
}

/// The button that removes a setting.
fn remove_button<T: ExportHost>(
    model: &ExportModel,
    row: &ExportRow,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let action = ExportAction::Remove {
        ids: model.ids.clone(),
        index,
    };
    div()
        .id(SharedString::from(format!("export-remove-{index}")))
        .debug_selector(move || format!("export-remove-{index}"))
        .flex()
        .items_center()
        .justify_center()
        .w(px(24.0))
        .py(px(6.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
        .child("−")
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_export_action(action.clone(), cx);
        }))
}

/// The size presets, for picking a size.
fn size_menu<T: ExportHost>(
    model: &ExportModel,
    row: &ExportRow,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let mut choices = Vec::new();
    for scale in ExportSize::SCALES {
        let size = ExportSize::Scale { value: scale };
        choices.push(Choice {
            label: size.label(),
            selector: format!("export-size-preset-{index}-{}", size.label()),
            action: ExportAction::Update {
                ids: model.ids.clone(),
                index,
                edit: ExportEdit::Size(size),
            },
        });
    }
    for size in [
        ExportSize::Width { value: 512.0 },
        ExportSize::Height { value: 512.0 },
    ] {
        choices.push(Choice {
            label: size.label(),
            selector: format!("export-size-preset-{index}-{}", size.label()),
            action: ExportAction::Update {
                ids: model.ids.clone(),
                index,
                edit: ExportEdit::Size(size),
            },
        });
    }
    menu(choices, cx)
}

/// The formats, for picking one.
fn format_menu<T: ExportHost>(
    model: &ExportModel,
    row: &ExportRow,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let mut choices = Vec::new();
    for format in ExportFormat::ALL {
        choices.push(Choice {
            label: format.label().to_owned(),
            selector: format!("export-format-choice-{index}-{}", format.ext()),
            action: ExportAction::Update {
                ids: model.ids.clone(),
                index,
                edit: ExportEdit::Format(format),
            },
        });
    }
    menu(choices, cx)
}

/// A setting's advanced options: contents only, include bounding box, and for
/// an SVG the three SVG options.
fn options_menu<T: ExportHost>(
    model: &ExportModel,
    row: &ExportRow,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let ids = model.ids.clone();
    let mut list = menu_box();
    list = list.child(checkbox_row(
        "Contents only",
        row.contents_only,
        ExportEdit::ContentsOnly,
        ids.clone(),
        index,
        cx,
    ));
    list = list.child(checkbox_row(
        "Include bounding box",
        row.include_bounding_box,
        ExportEdit::IncludeBoundingBox,
        ids.clone(),
        index,
        cx,
    ));
    if row.format.value() == Some(&ExportFormat::Svg) {
        list = list.child(checkbox_row(
            "Outline text",
            row.svg_outline_text,
            ExportEdit::SvgOutlineText,
            ids.clone(),
            index,
            cx,
        ));
        list = list.child(checkbox_row(
            "Include id attribute",
            row.svg_id_attribute,
            ExportEdit::SvgIdAttribute,
            ids.clone(),
            index,
            cx,
        ));
        list = list.child(checkbox_row(
            "Simplify stroke",
            row.svg_simplify_stroke,
            ExportEdit::SvgSimplifyStroke,
            ids,
            index,
            cx,
        ));
    }
    list
}

/// A PDF setting's print settings: a summary that opens them.
fn print_row<T: ExportHost>(
    model: &ExportModel,
    row: &ExportRow,
    state: &Rc<RefCell<ExportPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let summary = print_summary(&row.print);
    let shared = Rc::clone(state);
    let mut block = column().gap_1();
    block = block.child(
        div()
            .id(SharedString::from(format!("export-print-{index}")))
            .debug_selector(move || format!("export-print-{index}"))
            .flex()
            .items_center()
            .gap_2()
            .px(px(8.0))
            .py(px(6.0))
            .rounded_md()
            .bg(rgb(theme::WINDOW))
            .border_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .child(div().flex_1().child(summary))
            .child("⋯")
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.open = if state.open == Some(ExportMenu::Print(index)) {
                        None
                    } else {
                        Some(ExportMenu::Print(index))
                    };
                    state.draft = None;
                }
                cx.notify();
            })),
    );
    if state.borrow().open == Some(ExportMenu::Print(index)) {
        block = block.child(print_options(model, row, state, cx));
    }
    block
}

/// A PDF setting's print options: the bleed, the marks and the colour.
fn print_options<T: ExportHost>(
    model: &ExportModel,
    row: &ExportRow,
    state: &Rc<RefCell<ExportPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let ids = model.ids.clone();
    let mut list = menu_box();
    list = list.child(bleed_field(model, row, state, cx));
    list = list.child(checkbox_row(
        "Crop marks",
        row.print.crop_marks,
        |on| ExportEdit::Print(PrintEdit::CropMarks(on)),
        ids.clone(),
        index,
        cx,
    ));
    list = list.child(checkbox_row(
        "Bleed marks",
        row.print.bleed_marks,
        |on| ExportEdit::Print(PrintEdit::BleedMarks(on)),
        ids.clone(),
        index,
        cx,
    ));
    list = list.child(checkbox_row(
        "CMYK colour",
        row.print.cmyk,
        |on| ExportEdit::Print(PrintEdit::Cmyk(on)),
        ids,
        index,
        cx,
    ));
    // Bleed marks with no bleed are marks with nothing to mark: the engine
    // would draw them outside a bleed that is not there.
    let bleed = row.print.bleed.value().copied().unwrap_or(0.0);
    if row.print.bleed_marks.value() == Some(&true) && bleed <= 0.0 {
        list = list.child(note("Bleed marks need a bleed"));
    }
    list
}

/// The bleed field: a length with a unit, typed in.
fn bleed_field<T: ExportHost>(
    model: &ExportModel,
    row: &ExportRow,
    state: &Rc<RefCell<ExportPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = row.index;
    let current = row
        .print
        .bleed
        .value()
        .copied()
        .map(PrintSettings::length_label);
    let value_text = match &row.print.bleed {
        ExportValue::Set(bleed) => PrintSettings::length_label(*bleed),
        ExportValue::Unset => NOT_SET.to_owned(),
        ExportValue::Mixed => MIXED.to_owned(),
    };
    let shown = field_text(state, ExportField::Bleed(index), value_text.clone());
    let editing = is_editing(state, ExportField::Bleed(index));
    let draft = ExportDraft {
        field: ExportField::Bleed(index),
        text: current.unwrap_or_default(),
        ids: model.ids.clone(),
        select_all: true,
    };
    let shared = Rc::clone(state);
    let field = field_box(
        shown,
        None,
        editing,
        format!("export-bleed-value-{index}-{value_text}"),
    )
    .id(SharedString::from(format!("export-bleed-{index}")))
    .debug_selector(move || format!("export-bleed-{index}"))
    .on_click(cx.listener(move |_host, _event, _window, cx| {
        {
            let mut state = shared.borrow_mut();
            state.open = None;
            state.draft = Some(draft.clone());
        }
        cx.notify();
    }));
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child("Bleed"),
        )
        .child(field)
}

/// One option: a box that is on, off, or neither, and its label.
fn checkbox_row<T: ExportHost>(
    label: &'static str,
    value: ExportValue<bool>,
    make_edit: impl Fn(bool) -> ExportEdit + 'static,
    ids: Vec<NodeId>,
    index: usize,
    cx: &mut Context<T>,
) -> impl IntoElement {
    // A disagreement has no answer to invert, so turning the option on is the
    // one answer that means something for a selection that disagrees.
    let on = value.value().copied().unwrap_or(false);
    let next = !on;
    let action = ExportAction::Update {
        ids,
        index,
        edit: make_edit(next),
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .id(SharedString::from(format!("export-option-{label}")))
        .debug_selector(move || format!("export-option-{label}"))
        .cursor_pointer()
        .child(checkbox_box(value))
        .child(
            div()
                .flex_1()
                .id(SharedString::from(format!("export-text-{label}")))
                .debug_selector(move || format!("export-text-{label}"))
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(label),
        )
        .child(if value.is_mixed() {
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(MIXED)
        } else {
            div()
        })
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_export_action(action.clone(), cx);
        }))
}

/// The box of a checkbox: on, off, or neither.
fn checkbox_box(value: ExportValue<bool>) -> Div {
    let (fill, mark) = match value {
        ExportValue::Set(true) => (theme::ACCENT, "✓"),
        ExportValue::Set(false) => (theme::WINDOW, ""),
        // A disagreement is drawn as neither on nor off, so the eye reads the
        // word beside it as the answer.
        ExportValue::Mixed => (theme::ACCENT_WASH, "–"),
        ExportValue::Unset => (theme::WINDOW, ""),
    };
    div()
        .size(px(13.0))
        .rounded_sm()
        .bg(rgb(fill))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(rgb(theme::TEXT))
        .child(mark)
}

/// The print settings, as one line: "3 mm bleed, crop marks, CMYK".
///
/// figcraft's own summary, word for word. A disagreement has no summary, so a
/// mixed selection says "Mixed" instead of one layer's answer.
fn print_summary(print: &PrintRow) -> String {
    if print.bleed.is_mixed()
        || print.crop_marks.is_mixed()
        || print.bleed_marks.is_mixed()
        || print.cmyk.is_mixed()
    {
        return MIXED.to_owned();
    }
    let bleed = print.bleed.value().copied().unwrap_or(0.0);
    let mut parts: Vec<String> = Vec::new();
    if bleed > 0.0 {
        parts.push(format!("{} bleed", PrintSettings::length_label(bleed)));
    }
    if print.crop_marks.value() == Some(&true) {
        parts.push("crop marks".to_owned());
    }
    if print.bleed_marks.value() == Some(&true) && bleed > 0.0 {
        parts.push("bleed marks".to_owned());
    }
    parts.push(match print.cmyk.value() {
        Some(true) => "CMYK".to_owned(),
        _ => "RGB".to_owned(),
    });
    if bleed <= 0.0 && !print_marks(print) {
        parts.push("no bleed".to_owned());
    }
    let joined = parts.join(", ");
    let mut chars = joined.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => joined,
    }
}

/// Whether the print settings draw any marks.
fn print_marks(print: &PrintRow) -> bool {
    let bleed = print.bleed.value().copied().unwrap_or(0.0);
    print.crop_marks.value() == Some(&true)
        || (print.bleed_marks.value() == Some(&true) && bleed > 0.0)
}

/// One choice in a menu.
struct Choice {
    /// The text the choice shows.
    label: String,
    /// The name a test looks the choice up by.
    selector: String,
    /// What choosing it does.
    action: ExportAction,
}

/// A menu: a list of choices under the control that opened it.
fn menu<T: ExportHost>(choices: Vec<Choice>, cx: &mut Context<T>) -> impl IntoElement {
    let mut list = menu_box();
    for choice in choices {
        let action = choice.action.clone();
        let selector = choice.selector.clone();
        let item = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .px(px(6.0))
            .py(px(5.0))
            .rounded_sm()
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child(choice.label)
            .on_click(cx.listener(move |host, _event, _window, cx| {
                host.run_export_action(action.clone(), cx);
            }));
        list = list.child(item);
    }
    list
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

/// The chrome every field of the section shares: a bordered box with the value
/// in it, and a caret while the value is being typed.
///
/// `value_selector` names the element that draws the value, so a test can ask
/// what the section *shows* rather than which function drew it.
fn field_box(text: String, width: Option<f32>, editing: bool, value_selector: String) -> Div {
    let value = div()
        .id(SharedString::from(value_selector.clone()))
        .debug_selector(move || value_selector.clone())
        .text_color(rgb(theme::TEXT_SECONDARY))
        .child(text);
    let field = div()
        .flex()
        .flex_1()
        .items_center()
        .justify_between()
        .gap_2()
        .px(px(8.0))
        .py(px(6.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(if editing {
            theme::ACCENT
        } else {
            theme::BORDER_SOFT
        }))
        .text_xs()
        .child(value);
    match width {
        Some(width) => field.w(px(width)).flex_none(),
        None => field,
    }
}

/// What a field shows: the text being typed, or the value.
fn field_text(state: &Rc<RefCell<ExportPanelState>>, field: ExportField, value: String) -> String {
    let state = state.borrow();
    match state.draft.as_ref() {
        Some(draft) if draft.field == field => format!("{}|", draft.text),
        _ => value,
    }
}

/// Whether a field is being typed into.
fn is_editing(state: &Rc<RefCell<ExportPanelState>>, field: ExportField) -> bool {
    state
        .borrow()
        .draft
        .as_ref()
        .is_some_and(|draft| draft.field == field)
}

/// A line of text under a control.
fn note(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A plain vertical stack.
fn column() -> Div {
    div().flex().flex_col()
}

#[cfg(test)]
mod tests {
    use super::*;
    use figcraft_doc::{NodeKind, TextData};
    use figcraft_engine::Session;
    use gpui::{point, size, Entity, IntoElement, Render, TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;

    // ── Reading the engine's values ─────────────────────────────────────────

    /// A session over a document with one rectangle on it.
    fn session_with_rectangle() -> (Session, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(page, NodeKind::Rectangle, "Hero", 0.0, 0.0, 100.0, 100.0)
            .expect("a rectangle");
        (Session::with_document(doc), id)
    }

    /// A session over a document with one rectangle that has two export
    /// settings: an SVG at 2x and a PDF with print options.
    fn session_with_exports() -> (Session, NodeId) {
        let (mut session, id) = session_with_rectangle();
        session
            .execute(
                "export.add",
                &json!({
                    "ids": [id.0],
                    "setting": { "size": "2x", "suffix": "@2x", "format": "svg" }
                }),
            )
            .expect("the SVG export setting");
        session
            .execute(
                "export.add",
                &json!({
                    "ids": [id.0],
                    "setting": {
                        "size": "512w",
                        "format": "pdf",
                        "bleed": "3mm",
                        "cropMarks": true,
                        "cmyk": true
                    }
                }),
            )
            .expect("the PDF export setting");
        (session, id)
    }

    /// The model of one layer's export settings.
    fn model_of(session: &Session, id: NodeId) -> ExportModel {
        let doc = session.doc().expect("a document");
        selected_exports(doc, &[id])
    }

    #[test]
    fn a_layer_with_export_settings_reads_the_engines_values_for_them() {
        let (session, id) = session_with_exports();
        let model = model_of(&session, id);
        assert_eq!(model.rows.len(), 2, "one row per export setting");
        assert!(!model.mixed, "one layer cannot disagree with itself");

        let first = &model.rows[0];
        assert_eq!(
            first.size,
            ExportValue::Set(ExportSize::Scale { value: 2.0 }),
            "the size is the engine's size"
        );
        assert_eq!(
            first.suffix,
            ExportValue::Set("@2x".to_owned()),
            "the suffix is the engine's suffix"
        );
        assert_eq!(
            first.format,
            ExportValue::Set(ExportFormat::Svg),
            "the format is the engine's format"
        );
        assert_eq!(
            first.contents_only,
            ExportValue::Set(true),
            "the engine's default is contents only"
        );
        assert_eq!(
            first.include_bounding_box,
            ExportValue::Set(false),
            "the engine's default is not to include the bounding box"
        );
        assert_eq!(
            first.svg_outline_text,
            ExportValue::Set(true),
            "the engine's default is outlined text"
        );
        assert_eq!(
            first.svg_id_attribute,
            ExportValue::Set(false),
            "the engine's default is no id attribute"
        );
        assert_eq!(
            first.svg_simplify_stroke,
            ExportValue::Set(true),
            "the engine's default is a simplified stroke"
        );

        let second = &model.rows[1];
        assert_eq!(
            second.size,
            ExportValue::Set(ExportSize::Width { value: 512.0 }),
            "the width is the engine's width"
        );
        assert_eq!(
            second.format,
            ExportValue::Set(ExportFormat::Pdf),
            "the format is the engine's format"
        );
        assert_eq!(
            second.print.bleed,
            ExportValue::Set(PrintSettings::parse_length("3mm").expect("a length")),
            "the bleed is the engine's bleed, in points"
        );
        assert_eq!(second.print.crop_marks, ExportValue::Set(true));
        assert_eq!(second.print.bleed_marks, ExportValue::Set(false));
        assert_eq!(second.print.cmyk, ExportValue::Set(true));
    }

    #[test]
    fn a_command_for_a_change_carries_the_engines_param_names_and_values() {
        let (session, id) = session_with_exports();
        let model = model_of(&session, id);

        let (command, params) = model.update_command(0, &ExportEdit::Format(ExportFormat::Pdf));
        assert_eq!(command, "export.update");
        assert_eq!(params["ids"], json!([id.0]));
        assert_eq!(params["index"], json!(0));
        assert_eq!(
            params["format"],
            json!("pdf"),
            "the engine spells a format as its file extension"
        );

        let (_, params) =
            model.update_command(0, &ExportEdit::Size(ExportSize::Scale { value: 3.0 }));
        assert_eq!(
            params["size"],
            json!("3x"),
            "the engine spells a size as its label"
        );

        let (_, params) = model.update_command(0, &ExportEdit::Suffix("-dark".to_owned()));
        assert_eq!(params["suffix"], json!("-dark"));

        let (_, params) = model.update_command(0, &ExportEdit::ContentsOnly(false));
        assert_eq!(
            params["contentsOnly"],
            json!(false),
            "the engine's own name for the option"
        );

        let (_, params) = model.update_command(0, &ExportEdit::IncludeBoundingBox(true));
        assert_eq!(params["includeBoundingBox"], json!(true));

        let (_, params) = model.update_command(0, &ExportEdit::SvgOutlineText(false));
        assert_eq!(params["svgOutlineText"], json!(false));

        let (_, params) = model.update_command(0, &ExportEdit::SvgIdAttribute(true));
        assert_eq!(params["svgIdAttribute"], json!(true));

        let (_, params) = model.update_command(0, &ExportEdit::SvgSimplifyStroke(false));
        assert_eq!(params["svgSimplifyStroke"], json!(false));

        let (_, params) = model.update_command(1, &ExportEdit::Print(PrintEdit::Bleed(12.0)));
        assert_eq!(params["bleed"], json!(12.0), "the bleed is in points");

        let (_, params) = model.update_command(1, &ExportEdit::Print(PrintEdit::CropMarks(true)));
        assert_eq!(params["cropMarks"], json!(true));

        let (_, params) = model.update_command(1, &ExportEdit::Print(PrintEdit::BleedMarks(true)));
        assert_eq!(params["bleedMarks"], json!(true));

        let (_, params) = model.update_command(1, &ExportEdit::Print(PrintEdit::Cmyk(true)));
        assert_eq!(params["cmyk"], json!(true));

        let (command, params) = model.remove_command(0);
        assert_eq!(command, "export.remove");
        assert_eq!(params["index"], json!(0));
    }

    #[test]
    fn an_action_carries_the_same_command_the_model_builds() {
        let (session, id) = session_with_exports();
        let model = model_of(&session, id);
        let action = ExportAction::Update {
            ids: model.ids.clone(),
            index: 0,
            edit: ExportEdit::Format(ExportFormat::Pdf),
        };
        assert_eq!(
            action.command(),
            model.update_command(0, &ExportEdit::Format(ExportFormat::Pdf)),
            "the action and the model build the same command"
        );
    }

    #[test]
    fn the_engine_accepts_the_commands_the_panel_builds() {
        let (mut session, id) = session_with_exports();
        let model = model_of(&session, id);
        let (command, params) = model.update_command(0, &ExportEdit::Format(ExportFormat::Webp));
        session
            .execute(&command, &params)
            .expect("the engine accepts the panel's command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(
            node.exports[0].format,
            ExportFormat::Webp,
            "the engine's document holds the format the panel wrote"
        );

        let (command, params) =
            model.update_command(0, &ExportEdit::Size(ExportSize::Scale { value: 3.0 }));
        session
            .execute(&command, &params)
            .expect("the engine accepts the panel's command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(node.exports[0].size, ExportSize::Scale { value: 3.0 });

        let (command, params) = model.update_command(0, &ExportEdit::ContentsOnly(false));
        session
            .execute(&command, &params)
            .expect("the engine accepts the panel's command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert!(!node.exports[0].contents_only);
    }

    #[test]
    fn adding_a_setting_uses_the_engines_own_command() {
        let (mut session, id) = session_with_rectangle();
        let model = model_of(&session, id);
        let (command, params) = model.add_command();
        assert_eq!(command, "export.add");
        assert_eq!(params["ids"], json!([id.0]));
        session
            .execute(&command, &params)
            .expect("the engine accepts the panel's command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(node.exports.len(), 1);
        assert_eq!(node.exports[0].size, ExportSize::Scale { value: 1.0 });
        assert_eq!(node.exports[0].format, ExportFormat::Png);
    }

    #[test]
    fn replacing_mixed_settings_writes_the_engines_default_setting() {
        let (mut session, id) = session_with_exports();
        // A second layer whose settings differ: the selection now disagrees.
        let page = session
            .doc()
            .expect("a document")
            .page_of(id)
            .expect("a page");
        let other = session
            .doc_mut()
            .expect("a document")
            .create(page, NodeKind::Rectangle, "Other", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        session
            .execute("export.add", &json!({ "ids": [other.0] }))
            .expect("the other layer gets a setting");

        let doc = session.doc().expect("a document");
        let model = selected_exports(doc, &[id, other]);
        assert!(model.mixed, "the two layers disagree about their settings");
        let (command, params) = model.add_command();
        assert_eq!(
            command, "node.set",
            "a mixed selection is replaced by one default setting"
        );
        assert_eq!(
            params["props"]["exports"],
            json!([{ "size": { "by": "scale", "value": 1.0 }, "suffix": "", "format": "png" }]),
            "the engine's default setting, in the engine's own spelling"
        );
        session
            .execute(&command, &params)
            .expect("the engine accepts the panel's command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(node.exports.len(), 1);
        assert_eq!(node.exports[0], ExportSetting::default());
    }

    // ── A selection that disagrees ──────────────────────────────────────────

    /// A session over a document with two layers whose export settings differ.
    fn session_with_two_layers() -> (Session, NodeId, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let first = doc
            .create(page, NodeKind::Rectangle, "First", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let second = doc
            .create(page, NodeKind::Rectangle, "Second", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let mut session = Session::with_document(doc);
        session
            .execute(
                "export.add",
                &json!({ "ids": [first.0], "setting": { "size": "1x", "format": "png" } }),
            )
            .expect("the first layer's setting");
        session
            .execute(
                "export.add",
                &json!({ "ids": [second.0], "setting": { "size": "2x", "format": "svg" } }),
            )
            .expect("the second layer's setting");
        (session, first, second)
    }

    #[test]
    fn a_mixed_selection_reads_as_mixed_for_the_property_that_differs() {
        let (session, first, second) = session_with_two_layers();
        let doc = session.doc().expect("a document");
        let model = selected_exports(doc, &[first, second]);
        assert_eq!(model.rows.len(), 1, "both layers have one setting");
        let row = &model.rows[0];

        assert_eq!(
            row.size,
            ExportValue::Mixed,
            "the two sizes differ, so there is no size to show"
        );
        assert_eq!(
            row.format,
            ExportValue::Mixed,
            "the two formats differ, so there is no format to show"
        );
        assert_eq!(
            row.contents_only,
            ExportValue::Set(true),
            "the layers agree on this one, so it keeps its value"
        );
        assert_eq!(
            row.include_bounding_box,
            ExportValue::Set(false),
            "the layers agree on this one, so it keeps its value"
        );
        assert_eq!(
            row.svg_outline_text,
            ExportValue::Set(true),
            "the layers agree on this one, so it keeps its value"
        );
        assert_eq!(
            row.suffix,
            ExportValue::Unset,
            "neither layer has a suffix, so nobody set one"
        );
    }

    #[test]
    fn one_property_differing_does_not_hide_the_others() {
        let (session, first, second) = session_with_two_layers();
        let doc = session.doc().expect("a document");
        let model = selected_exports(doc, &[first, second]);
        let row = &model.rows[0];
        // The size is a disagreement and the flags are not: folding the whole
        // setting instead would have hidden the flags behind it.
        assert!(row.size.is_mixed());
        assert!(!row.contents_only.is_mixed());
        assert_eq!(row.contents_only, ExportValue::Set(true));
    }

    #[test]
    fn a_layer_with_no_export_settings_has_no_rows_at_all() {
        let (session, id) = session_with_rectangle();
        let model = model_of(&session, id);
        assert!(
            model.rows.is_empty(),
            "an export nobody set is the empty state, not a row of empty values"
        );
        assert!(!model.mixed);
    }

    #[test]
    fn a_layer_that_is_gone_is_not_shown_as_an_empty_one() {
        let (session, _id) = session_with_rectangle();
        let doc = session.doc().expect("a document");
        assert!(
            exports_of(doc, NodeId(9999)).is_none(),
            "a selection that outlived its layer is reported, not invented"
        );
    }

    // ── The panel's own state ───────────────────────────────────────────────

    #[test]
    fn typing_a_size_commits_it_and_typing_a_nonsense_size_does_not() {
        let mut state = ExportPanelState::new();
        state.draft = Some(ExportDraft {
            field: ExportField::Size(0),
            text: "2x".to_owned(),
            ids: vec![NodeId(7)],
            select_all: true,
        });
        let action = state.handle_key("enter").expect("the key is the section's");
        let (command, params) = action.command();
        assert_eq!(command, "export.update");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["index"], json!(0));
        assert_eq!(params["size"], json!("2x"));
        assert!(state.draft.is_none(), "the field closed");

        let mut state = ExportPanelState::new();
        state.draft = Some(ExportDraft {
            field: ExportField::Size(0),
            text: "huge".to_owned(),
            ids: vec![NodeId(7)],
            select_all: true,
        });
        assert!(
            state.handle_key("enter").is_none(),
            "a size the engine would refuse is not written"
        );
    }

    #[test]
    fn typing_a_suffix_commits_the_text_that_was_typed() {
        let mut state = ExportPanelState::new();
        state.draft = Some(ExportDraft {
            field: ExportField::Suffix(1),
            text: "-dark".to_owned(),
            ids: vec![NodeId(3)],
            select_all: true,
        });
        let action = state.handle_key("enter").expect("the key is the section's");
        let (_, params) = action.command();
        assert_eq!(params["index"], json!(1));
        assert_eq!(params["suffix"], json!("-dark"));
    }

    #[test]
    fn the_first_key_replaces_the_text_that_was_there() {
        let mut state = ExportPanelState::new();
        state.draft = Some(ExportDraft {
            field: ExportField::Suffix(0),
            text: "old".to_owned(),
            ids: vec![NodeId(3)],
            select_all: true,
        });
        state.handle_key("x");
        let draft = state.draft.as_ref().expect("the field is still open");
        assert_eq!(draft.text, "x");
    }

    #[test]
    fn a_key_with_no_field_open_is_not_the_sections() {
        let mut state = ExportPanelState::new();
        assert!(state.handle_key("a").is_none());
        assert!(!state.is_busy());
    }

    #[test]
    fn the_hosts_key_forwarding_reads_and_writes_the_state_in_turn() {
        // The surface that hosts the panel owns the state as a cell, so the
        // reading and the writing must not overlap: the borrow that asks
        // whether a field is open has to be gone before the borrow that types
        // into it. This is that pattern, run for real.
        let state = Rc::new(RefCell::new(ExportPanelState::new()));
        state.borrow_mut().draft = Some(ExportDraft {
            field: ExportField::Suffix(0),
            text: "-dark".to_owned(),
            ids: vec![NodeId(5)],
            select_all: true,
        });

        let typing = state.borrow().is_busy();
        assert!(typing, "a field is open, so the key is the section's");
        let action = state.borrow_mut().handle_key("enter");
        let action = action.expect("the typed suffix is a change");
        let (command, params) = action.command();
        assert_eq!(command, "export.update");
        assert_eq!(params["ids"], json!([5]));
        assert_eq!(params["suffix"], json!("-dark"));
        assert!(!state.borrow().is_busy(), "the field closed");
    }

    // ── The panel, in a window ──────────────────────────────────────────────

    /// A host for the section in a window: it draws the section, remembers
    /// what the section asked for, and does what it is asked.
    struct Host {
        model: ExportModel,
        state: Rc<RefCell<ExportPanelState>>,
        asked: Rc<RefCell<Vec<ExportAction>>>,
    }

    impl Host {
        fn new(model: ExportModel) -> Self {
            Self {
                model,
                state: Rc::new(RefCell::new(ExportPanelState::new())),
                asked: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl ExportHost for Host {
        fn run_export_action(&mut self, action: ExportAction, cx: &mut Context<Self>) {
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
        model: ExportModel,
    ) -> (Entity<Host>, &mut VisualTestContext) {
        cx.add_window_view(|_, _| Host::new(model))
    }

    /// Draw the section in a window and settle.
    fn draw(root: &Entity<Host>, window: &mut VisualTestContext) {
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(900.0)),
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

    #[gpui::test]
    fn the_section_draws_figcrafts_labels_in_a_window(cx: &mut TestAppContext) {
        let (session, id) = session_with_exports();
        let model = model_of(&session, id);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        // The "+" that adds a setting, and the setting's own fields.
        assert!(
            window.debug_bounds("export-add").is_some(),
            "the section offers to add an export setting"
        );
        assert!(
            window.debug_bounds("export-size-0").is_some(),
            "the setting's size field"
        );
        assert!(
            window.debug_bounds("export-suffix-0").is_some(),
            "the setting's suffix field"
        );
        assert!(
            window.debug_bounds("export-format-0").is_some(),
            "the setting's format field"
        );

        // The values the engine holds are the values the section draws: the
        // element that draws "2x" and the element that draws "SVG" are on
        // screen, and their names say what they say.
        for text in ["export-size-value-0-2x", "export-suffix-value-0-@2x"] {
            let selector: &'static str = Box::leak(text.to_owned().into_boxed_str());
            assert!(
                window.debug_bounds(selector).is_some(),
                "the section draws {text}"
            );
        }
        let selector: &'static str =
            Box::leak("export-format-value-0-SVG".to_owned().into_boxed_str());
        assert!(
            window.debug_bounds(selector).is_some(),
            "the section draws the format the engine holds"
        );
        // The second setting has no suffix, and a suffix nobody set says so
        // rather than showing an empty field.
        let selector: &'static str =
            Box::leak("export-suffix-value-1-Not set".to_owned().into_boxed_str());
        assert!(
            window.debug_bounds(selector).is_some(),
            "the section says that nobody set the suffix"
        );
    }

    #[gpui::test]
    fn the_advanced_options_show_figcrafts_labels_when_they_are_opened(cx: &mut TestAppContext) {
        let (session, id) = session_with_exports();
        let model = model_of(&session, id);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        // The options are behind the "⋯" button, and the button opens them.
        let button = bounds(window, "export-options-0");
        window.simulate_click(button.center(), gpui::Modifiers::none());
        window.run_until_parked();

        // figcraft's own labels, word for word. The selector names the element
        // that draws the text, so this is a claim about what is on screen.
        for label in ["Contents only", "Include bounding box", "Outline text"] {
            let selector: &'static str = Box::leak(format!("export-text-{label}").into_boxed_str());
            assert!(
                window.debug_bounds(selector).is_some(),
                "the section draws figcraft's {label} option"
            );
        }
    }

    #[gpui::test]
    fn an_export_nobody_set_shows_the_empty_state(cx: &mut TestAppContext) {
        let (session, id) = session_with_rectangle();
        let model = model_of(&session, id);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        assert!(
            window.debug_bounds("export-add").is_some(),
            "an export nobody set offers to add one"
        );
        assert!(
            window.debug_bounds("export-size-0").is_none(),
            "and shows no settings, because there are none"
        );
    }

    #[gpui::test]
    fn adding_a_setting_asks_the_engine_to_add_one(cx: &mut TestAppContext) {
        let (session, id) = session_with_rectangle();
        let model = model_of(&session, id);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let button = bounds(window, "export-add");
        window.simulate_click(button.center(), gpui::Modifiers::none());
        window.run_until_parked();

        let asked = root.read_with(cx, |host, _| host.asked.borrow().clone());
        assert_eq!(asked.len(), 1, "the section asked for one change");
        let (command, params) = asked[0].command();
        assert_eq!(command, "export.add");
        assert_eq!(params["ids"], json!([id.0]));
    }

    #[test]
    fn a_text_layer_is_not_an_export_setting() {
        // A guard on the model's shape: the export rows come from the engine's
        // own list, whatever the layer is.
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(
                page,
                NodeKind::Text(TextData {
                    characters: "Hello".to_owned(),
                    ..Default::default()
                }),
                "Text",
                0.0,
                0.0,
                10.0,
                10.0,
            )
            .expect("a text layer");
        let session = Session::with_document(doc);
        let model = model_of(&session, id);
        assert!(model.rows.is_empty());
    }
}
