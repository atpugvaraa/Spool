//! The Assets section: the images a document holds, and the image fill of the
//! selection.
//!
//! # What this module owns
//!
//! Two things, in one file because they are one thing:
//!
//! 1. **The model.** [`AssetsModel`] reads the engine's [`Document`] and turns
//!    the document's images and the selection's image fill into what the
//!    panel shows. [`AssetsAction`] turns a change into the engine's own
//!    command.
//! 2. **The panel.** [`assets_section`] draws that model and asks the engine
//!    for the changes the user makes.
//!
//! # The reference
//!
//! figcraft's assets panel — `crates/ui-egui/src/assets_panel.rs` — is the
//! specification. Its shape is the shape here: a search field over the list, a
//! title, the assets as tiles with a label under each, rows that a click
//! applies to the selection, an empty state that says whether the file has
//! nothing or the search found nothing, and the reason a control is
//! unavailable said out loud.
//!
//! # What this port covers, and what it does not
//!
//! figcraft's assets panel lists a file's **components** and **styles**, and
//! the **libraries** it has enabled. In Spool the first two already have
//! their own homes — `engine/panel_components.rs` and `engine/panel_styles.rs`
//! — and `lamine.yaml` has no libraries section at all, so there is nothing
//! for a library list to read.
//!
//! What is left is the asset kind the engine holds and Spool's source cannot
//! carry: the **image**. `figcraft_doc::Document::images` is a map from a
//! content hash to the encoded bytes, and the engine's commands cover placing
//! an image (`image.place`) and putting one on a layer (`node.set` with an
//! image fill). That is what this section lists.
//!
//! | figcraft's assets panel | here |
//! |---|---|
//! | the search field | ported: it filters the list |
//! | the asset tiles, with a label under each | ported: the images, with their hash under each |
//! | the thumbnail in the tile | **a stand-in**: the image's dimensions and format. See below. |
//! | clicking a style row applies it to the selection | ported: clicking an image puts it on the selection's fill |
//! | clicking a component tile places an instance | ported as a button: it places a layer that paints the image |
//! | "No components yet." / "No matching components" | ported: "No images yet." / "No matching images" |
//! | the library sections, the updates banner, the libraries view | **not ported**: `lamine.yaml` has no libraries, and a figcraft-server library is a feature Spool has no source for |
//! | dragging a tile onto the canvas | **not ported**: the Inspector has no canvas to drop onto |
//!
//! # The thumbnail is a stand-in, and it says so
//!
//! figcraft's tile draws the asset's own pixels: `assets_panel::thumbnail`
//! renders the component with `figcraft_render` and uploads the result as a
//! texture. Spool's Design panel cannot do that: the section is a function
//! over a `Context<T>` with no canvas and no texture of its own, and the
//! engine's renderer is the canvas's business, not the Inspector's.
//!
//! So the tile draws what the engine knows about the image instead of its
//! pixels: its **dimensions** and its **format**, which is what
//! `figcraft_doc::ImageData` holds. It is not a thumbnail and it does not
//! pretend to be one — it is the image's own facts, in the tile.
//!
//! # The source cannot carry an image
//!
//! Spool is source-backed: the authored HTML and CSS are the source of truth
//! and `lamine.yaml` holds the structure. **Neither can express an image
//! today.**
//!
//! - `crates/spool-source/src/reader.rs` compiles `background-color` and
//!   nothing else: `background-image` is not in the reader's applied set
//!   (`OWNER_A_APPLIED`), so `background-image: url(...)` is reported as
//!   authored but uncompiled, and an `<img>` compiles to a node with no paint.
//! - `lamine.yaml` has no image field. Its `image_presets` are adjustment
//!   presets, not pictures.
//! - `app/src/engine/source_edits.rs` writes a layer's fill back as
//!   `background-color`, which is a solid colour.
//!
//! So an image applied or placed here is a real change to the editor's
//! document, and it does not survive a save. The section says so, where the
//! change is made, rather than leaving the user to find out by saving. See
//! `docs/06-ui-parity.md` §7.
//!
//! # Three states, folded one property at a time
//!
//! A property of a *selection* is one of three things: a value every selected
//! layer agrees on, agreement that nobody set it, or the absence of
//! agreement. [`AssetValue`] is that fold, and the fold is per property: the
//! image and the scale mode are two properties, and layers that disagree
//! about the image may still agree about how it fills.
//!
//! # Commands, not mutations
//!
//! Every change leaves through the engine's command surface: `node.set` for
//! the fill and `image.place` for a new layer. Nothing here writes to a
//! [`Document`] directly, so an edit made from this panel, from the CLI and
//! from an agent are the same edit and land in the same history.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use figcraft_doc::color::{ImageScaleMode, Paint, PaintKind};
use figcraft_doc::{Document, Node, NodeId};
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, SharedString};
use serde_json::{json, Value};

use crate::theme;

// ═══════════════════════════════════════════════════════════════════════════
// The words
// ═══════════════════════════════════════════════════════════════════════════

/// The word for a property the selected layers disagree about.
///
/// The Inspector's own word, and figcraft's: a value and a nothing would
/// leave a panel free to invent a third thing at every row that faced a
/// disagreement.
pub const MIXED: &str = "Mixed";

/// The word for a property nobody has set.
///
/// figcraft's word, not Spool's: a property that is not there is "Not set",
/// which says that there is nothing to read rather than that the answer is
/// zero.
pub const NOT_SET: &str = "Not set";

/// The empty state of a file that holds no image.
pub const NO_IMAGES: &str = "No images yet.";

/// The empty state of a search that found nothing.
pub const NO_MATCHING_IMAGES: &str = "No matching images";

/// The reason an image cannot be put on a layer that is not selected.
///
/// figcraft's own words, one noun changed: it says "Select layers to apply
/// the style to" when a style row is clicked with nothing selected.
pub const SELECT_LAYERS: &str = "Select layers to apply an image.";

/// Why the scale mode cannot be chosen.
///
/// The scale mode is written as part of the fill, and a fill names one image:
/// it can only be chosen when every selected layer paints the same one.
pub const SCALE_NEEDS_ONE_IMAGE: &str =
    "The scale is set once every selected layer paints the same image.";

/// What the source can and cannot carry.
///
/// Spool is source-backed: the authored HTML and CSS are the source of truth
/// and `lamine.yaml` holds the structure, and neither can express an image
/// yet. The reader compiles `background-color` and nothing else, the metadata
/// has no image field, and the save path writes a fill back as a colour.
///
/// So an image applied or placed here is a real change to the editor's
/// document and it does not survive a save. Said where the change is made,
/// rather than left for the user to find out by saving.
pub const SOURCE_NOTE: &str =
    "Spool's source cannot carry an image yet: the CSS reader compiles background-color and \
     nothing else, lamine.yaml has no image field, and the save path writes a fill back as a \
     colour. An image applied or placed here stays in the editor's document.";

// ═══════════════════════════════════════════════════════════════════════════
// The three states
// ═══════════════════════════════════════════════════════════════════════════

/// One property's state across a selection.
///
/// Three states, never two: a value every selected layer agrees on, agreement
/// that nobody set it, and the absence of agreement. The Inspector's other
/// properties have the same three, and the same words, so "Mixed" means one
/// thing in the panel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum AssetValue<T> {
    /// Every selected layer has this value, and they agree on it.
    Set(T),
    /// No selected layer has this value.
    #[default]
    Unset,
    /// The selected layers disagree.
    Mixed,
}

impl<T: PartialEq> AssetValue<T> {
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

// ═══════════════════════════════════════════════════════════════════════════
// The model
// ═══════════════════════════════════════════════════════════════════════════

/// One image the document holds.
///
/// The engine's own facts and nothing else: the content hash it is filed
/// under, the size it decodes to and the bytes themselves.
#[derive(Clone)]
pub struct AssetEntry {
    /// The engine's id for the image: the hash of its bytes.
    pub hash: String,
    /// The image's own width, in pixels.
    pub width: u32,
    /// The image's own height, in pixels.
    pub height: u32,
    /// The format the bytes are in: "PNG", "JPEG", "GIF", "WebP", "BMP" or
    /// "TIFF".
    pub format: &'static str,
    /// The encoded bytes.
    ///
    /// The document already holds them; this is the same allocation, so
    /// reading the model costs a pointer and not a copy.
    pub bytes: Arc<Vec<u8>>,
    /// How many layers paint this image.
    pub used: usize,
}

impl AssetEntry {
    /// The hash, shortened to what a tile can show.
    ///
    /// The engine has no name for an image — it files them by content hash —
    /// so the hash is the only handle there is.
    pub fn short_hash(&self) -> &str {
        let end = self.hash.len().min(8);
        &self.hash[..end]
    }

    /// The image's size, as the tile shows it: "1024 × 768".
    pub fn size_label(&self) -> String {
        format!("{} × {}", self.width, self.height)
    }

    /// Whether the search matches this image.
    ///
    /// The hash, the format and the size, in lower case, as figcraft's search
    /// matches a name.
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || self.hash.to_lowercase().contains(&query)
            || self.format.to_lowercase().contains(&query)
            || self.size_label().to_lowercase().contains(&query)
    }
}

/// The bytes are not shown: an image is thousands of numbers, and a debug
/// line that carries them is unreadable.
impl std::fmt::Debug for AssetEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetEntry")
            .field("hash", &self.hash)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .field("bytes", &self.bytes.len())
            .field("used", &self.used)
            .finish()
    }
}

impl PartialEq for AssetEntry {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
            && self.width == other.width
            && self.height == other.height
            && self.format == other.format
            && self.bytes == other.bytes
            && self.used == other.used
    }
}

/// What the Assets section shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssetsModel {
    /// The layers the model was read for.
    pub ids: Vec<NodeId>,
    /// The images the document holds, in the engine's order.
    ///
    /// The engine files them by content hash, so this is in hash order.
    pub images: Vec<AssetEntry>,
    /// The image the selection paints, across the selection.
    pub fill: AssetValue<String>,
    /// How that image fills its box, across the selection.
    ///
    /// A property of its own: layers that disagree about the image may still
    /// agree about the scale.
    pub mode: AssetValue<ImageScaleMode>,
}

impl AssetsModel {
    /// The images and the selection's image fill, as the panel shows them.
    ///
    /// A layer the document does not hold is skipped rather than shown as an
    /// empty one, and a selection with nothing selected paints nothing.
    pub fn read(doc: &Document, selection: &[NodeId]) -> Self {
        let nodes: Vec<&Node> = selection.iter().filter_map(|id| doc.node(*id)).collect();
        let readings: Vec<Option<(String, ImageScaleMode)>> =
            nodes.iter().map(|node| image_fill(node)).collect();
        // The fold is per property: the image and the scale are two.
        let fill = AssetValue::across(
            readings
                .iter()
                .map(|reading| reading.as_ref().map(|(hash, _)| hash.clone())),
        );
        let mode = AssetValue::across(
            readings
                .iter()
                .map(|reading| reading.as_ref().map(|(_, mode)| *mode)),
        );
        let used = usage_counts(doc);
        let images = doc
            .images
            .iter()
            .map(|(hash, data)| AssetEntry {
                hash: hash.clone(),
                width: data.width,
                height: data.height,
                format: format_of(&data.bytes),
                bytes: Arc::clone(&data.bytes),
                used: used.get(hash).copied().unwrap_or(0),
            })
            .collect();
        Self {
            ids: selection.to_vec(),
            images,
            fill,
            mode,
        }
    }

    /// The images the search matches.
    pub fn images_for(&self, query: &str) -> Vec<&AssetEntry> {
        self.images
            .iter()
            .filter(|image| image.matches(query))
            .collect()
    }

    /// Whether the document holds any image at all.
    ///
    /// This is what tells "no matching images" from "no images yet": a search
    /// that found nothing is not the same as a file with no images in it.
    pub fn has_any(&self) -> bool {
        !self.images.is_empty()
    }

    /// The scale mode a new image takes.
    ///
    /// The mode the selection already uses, when it agrees on one, and the
    /// engine's own default otherwise.
    pub fn default_mode(&self) -> ImageScaleMode {
        self.mode.value().copied().unwrap_or_default()
    }
}

/// The image a layer paints, and how it fills.
///
/// The first image paint among the layer's fills: a layer paints one fill
/// with the image in the Inspector, and the engine's `image.adjust` command
/// calls the rest of them "every image paint".
fn image_fill(node: &Node) -> Option<(String, ImageScaleMode)> {
    node.fills.iter().find_map(|paint| match &paint.kind {
        PaintKind::Image(image) => Some((image.image.clone(), image.scale_mode)),
        _ => None,
    })
}

/// How many layers paint each image.
///
/// The engine's own reading: `figcraft_doc::library::node_images` collects the
/// image hashes a node paints, from its fills, its strokes and its text.
fn usage_counts(doc: &Document) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for node in doc.nodes() {
        let mut images = BTreeSet::new();
        figcraft_doc::library::node_images(node, &mut images);
        for hash in images {
            *counts.entry(hash).or_insert(0) += 1;
        }
    }
    counts
}

/// The format an image's bytes are in.
///
/// The same crate the engine decodes the image with, so the format the panel
/// names is the format the engine saw.
fn format_of(bytes: &[u8]) -> &'static str {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => "PNG",
        Ok(image::ImageFormat::Jpeg) => "JPEG",
        Ok(image::ImageFormat::Gif) => "GIF",
        Ok(image::ImageFormat::WebP) => "WebP",
        Ok(image::ImageFormat::Bmp) => "BMP",
        Ok(image::ImageFormat::Tiff) => "TIFF",
        _ => "Image",
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The commands
// ═══════════════════════════════════════════════════════════════════════════

/// A change the Assets section can ask for.
///
/// One variant per thing the section can change, so a change the engine has
/// no command for cannot be written down as though it had one.
#[derive(Clone, Debug, PartialEq)]
pub enum AssetsAction {
    /// Put an image on the selection's fill.
    Apply {
        /// The layers to put it on.
        ids: Vec<NodeId>,
        /// The image: the engine's content hash.
        image: String,
        /// How it fills the layer.
        mode: ImageScaleMode,
    },
    /// Change how the selection's image fills its box.
    ///
    /// The same command as [`AssetsAction::Apply`] with another scale: the
    /// scale is part of the fill, and the fill names one image.
    SetMode {
        /// The layers to change.
        ids: Vec<NodeId>,
        /// The image they all paint.
        image: String,
        /// How it fills them.
        mode: ImageScaleMode,
    },
    /// Take the image off the selection.
    Clear {
        /// The layers to clear.
        ids: Vec<NodeId>,
    },
    /// Place an image the document holds as a new layer.
    Place {
        /// The image: the engine's content hash.
        image: String,
        /// The image's bytes, as the base64 `image.place` takes.
        data: String,
        /// How it fills the new layer.
        mode: ImageScaleMode,
    },
}

impl AssetsAction {
    /// The engine's command id and parameters for this change.
    ///
    /// The engine's own spellings, so a change made from the panel is the
    /// same change the CLI, the keymap and an agent would make.
    pub fn command(&self) -> (String, Value) {
        match self {
            Self::Apply { ids, image, mode } | Self::SetMode { ids, image, mode } => (
                "node.set".to_owned(),
                json!({
                    "ids": ids.iter().map(|id| id.0).collect::<Vec<_>>(),
                    "props": { "fills": [image_paint(image, *mode)] },
                }),
            ),
            Self::Clear { ids } => (
                "node.set".to_owned(),
                json!({
                    "ids": ids.iter().map(|id| id.0).collect::<Vec<_>>(),
                    "props": { "fills": [] },
                }),
            ),
            // `image.place` derives the hash from the bytes and returns it, so
            // the hash the action carries is not a parameter.
            Self::Place {
                image: _,
                data,
                mode,
            } => (
                "image.place".to_owned(),
                json!({ "data": data, "mode": mode_name(*mode) }),
            ),
        }
    }
}

/// The engine's own paint for an image.
///
/// The engine's `Paint`, serialized by the engine, so the panel cannot spell
/// a field the engine does not have.
fn image_paint(image: &str, mode: ImageScaleMode) -> Value {
    serde_json::to_value(Paint::image(image, mode)).unwrap_or(Value::Null)
}

/// The engine's own name for a scale mode.
///
/// The same word `image.place`'s `mode` parameter takes, and the same one the
/// engine's `ImageScaleMode` serializes as.
fn mode_name(mode: ImageScaleMode) -> &'static str {
    match mode {
        ImageScaleMode::Fill => "fill",
        ImageScaleMode::Fit => "fit",
        ImageScaleMode::Crop => "crop",
        ImageScaleMode::Tile => "tile",
    }
}

/// The name the panel shows for a scale mode.
///
/// figcraft's paint picker's own words.
fn mode_label(mode: ImageScaleMode) -> &'static str {
    match mode {
        ImageScaleMode::Fill => "Fill",
        ImageScaleMode::Fit => "Fit",
        ImageScaleMode::Crop => "Crop",
        ImageScaleMode::Tile => "Tile",
    }
}

/// The engine's four scale modes, in the engine's order.
const MODES: [ImageScaleMode; 4] = [
    ImageScaleMode::Fill,
    ImageScaleMode::Fit,
    ImageScaleMode::Crop,
    ImageScaleMode::Tile,
];

/// The bytes of an image, as the base64 `image.place` takes.
///
/// The standard alphabet, with the padding the engine's decoder expects. The
/// engine's own decoder is the one that reads it, so this is its standard and
/// not a variant of it.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = u32::from(chunk.get(1).copied().unwrap_or(0));
        let third = u32::from(chunk.get(2).copied().unwrap_or(0));
        let triple = (first << 16) | (second << 8) | third;
        out.push(char::from(ALPHABET[((triple >> 18) & 0x3f) as usize]));
        out.push(char::from(ALPHABET[((triple >> 12) & 0x3f) as usize]));
        out.push(if chunk.len() > 1 {
            char::from(ALPHABET[((triple >> 6) & 0x3f) as usize])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(ALPHABET[(triple & 0x3f) as usize])
        } else {
            '='
        });
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel's own state
// ═══════════════════════════════════════════════════════════════════════════

/// Which field is being typed into.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AssetField {
    /// The list's search field.
    #[default]
    Search,
}

/// A value being typed into a field.
///
/// A buffer, not a second copy of the list: the value becomes real only when
/// Enter hands it over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssetDraft {
    /// Which field.
    pub field: AssetField,
    /// The text as typed.
    pub text: String,
    /// Whether the text is selected, so the first keystroke replaces it.
    pub select_all: bool,
}

/// The panel's own state.
///
/// Presentation only: never in the document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssetsPanelState {
    /// The search text the list is filtered by.
    pub query: String,
    /// The field being typed into, if any.
    pub draft: Option<AssetDraft>,
}

impl AssetsPanelState {
    /// The state a section starts in.
    pub fn new() -> Self {
        Self::default()
    }

    /// The text the list is filtered by right now.
    ///
    /// While the search field is open the list follows what has been typed,
    /// which is how figcraft's search behaves: the list narrows as the query
    /// is written, not when it is committed.
    pub fn filter(&self) -> &str {
        match self.draft.as_ref() {
            Some(draft) if draft.field == AssetField::Search => &draft.text,
            _ => &self.query,
        }
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
    pub fn handle_key(&mut self, key: &str) -> Option<AssetsAction> {
        let draft = self.draft.as_mut()?;
        match key {
            "enter" | "return" => {
                let draft = self.draft.take()?;
                // The search is not an edit of the document: it is the list's
                // own filter, so it lands in the state.
                if draft.field == AssetField::Search {
                    self.query = draft.text;
                }
                None
            }
            "backspace" => {
                // With the text selected, Backspace deletes the selection.
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

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// What a surface that hosts the Assets section must be able to do.
///
/// The panel draws and asks; the surface owns the engine and runs the changes.
/// The panel never touches the document itself, so the engine's command
/// surface stays the one operation boundary.
pub trait AssetsHost: Sized + 'static {
    /// Run a change through the engine.
    fn run_assets_action(&mut self, action: AssetsAction, cx: &mut Context<Self>);
}

/// The Assets section, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own state,
/// and the surface that runs the changes. It returns an element, so the
/// Inspector can put it in a section of its own and a test can put it in a
/// window on its own.
pub fn assets_section<T: AssetsHost>(
    model: &AssetsModel,
    state: &Rc<RefCell<AssetsPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2().px(px(14.0)).py(px(12.0));
    column = column.child(fill_row(model, cx));
    column = column.child(scale_row(model, cx));
    column = column.child(images_block(model, state, cx));
    column = column.child(caption(SOURCE_NOTE));
    column.into_any_element()
}

/// The row that says which image the selection paints.
fn fill_row<T: AssetsHost>(model: &AssetsModel, cx: &mut Context<T>) -> AnyElement {
    let (shown, has_image) = match &model.fill {
        AssetValue::Set(hash) => {
            let short = model
                .images
                .iter()
                .find(|image| &image.hash == hash)
                .map(|image| image.short_hash().to_owned())
                .unwrap_or_else(|| hash.chars().take(8).collect());
            (short, true)
        }
        AssetValue::Unset => (NOT_SET.to_owned(), false),
        AssetValue::Mixed => (MIXED.to_owned(), false),
    };
    let value_selector = format!("assets-fill-value-{shown}");
    let mut row = div()
        .id(SharedString::from("assets-fill"))
        .debug_selector(|| "assets-fill".to_owned())
        .flex()
        .items_center()
        .gap_2()
        .px(px(8.0))
        .py(px(7.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .child(div().text_color(rgb(theme::TEXT_MUTED)).child("Image"))
        .child(
            div()
                .id(SharedString::from(value_selector.clone()))
                .debug_selector(move || value_selector.clone())
                .flex_1()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(shown),
        );
    // The way to take the image off is only there when there is one image to
    // take off: the layers agree on it, so the panel knows what it removes.
    if has_image {
        let ids = model.ids.clone();
        row = row.child(
            div()
                .id(SharedString::from("assets-fill-clear"))
                .debug_selector(|| "assets-fill-clear".to_owned())
                .flex()
                .items_center()
                .justify_center()
                .w(px(20.0))
                .h(px(20.0))
                .rounded_sm()
                .text_color(rgb(theme::TEXT_MUTED))
                .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                .cursor_pointer()
                .child("×")
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    cx.stop_propagation();
                    host.run_assets_action(AssetsAction::Clear { ids: ids.clone() }, cx);
                })),
        );
    }
    row.into_any_element()
}

/// The row that says how the image fills, and the way to choose.
fn scale_row<T: AssetsHost>(model: &AssetsModel, cx: &mut Context<T>) -> AnyElement {
    let shown = match &model.mode {
        AssetValue::Set(mode) => mode_label(*mode).to_owned(),
        AssetValue::Unset => NOT_SET.to_owned(),
        AssetValue::Mixed => MIXED.to_owned(),
    };
    let value_selector = format!("assets-scale-value-{shown}");
    let mut column = div().flex().flex_col().gap_1();
    let mut row = div()
        .id(SharedString::from("assets-scale"))
        .debug_selector(|| "assets-scale".to_owned())
        .flex()
        .items_center()
        .gap_2()
        .px(px(8.0))
        .py(px(7.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .child(div().text_color(rgb(theme::TEXT_MUTED)).child("Scale"))
        .child(
            div()
                .id(SharedString::from(value_selector.clone()))
                .debug_selector(move || value_selector.clone())
                .flex_1()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(shown),
        );
    // The scale is part of the fill, and a fill names one image: it can only
    // be chosen when every selected layer paints the same one.
    let image = match &model.fill {
        AssetValue::Set(hash) => Some(hash.clone()),
        _ => None,
    };
    match image {
        Some(image) => {
            let mut modes = div().flex().items_center().gap_1().px(px(8.0));
            for mode in MODES {
                let current = model.mode.value() == Some(&mode);
                let ids = model.ids.clone();
                let name = image.clone();
                let chip = div()
                    .id(SharedString::from(format!(
                        "assets-mode-{}",
                        mode_name(mode)
                    )))
                    .debug_selector(move || format!("assets-mode-{}", mode_name(mode)))
                    .px(px(8.0))
                    .py(px(4.0))
                    .rounded_md()
                    .bg(rgb(if current {
                        theme::ACCENT_WASH
                    } else {
                        theme::SURFACE
                    }))
                    .border_1()
                    .border_color(rgb(if current {
                        theme::ACCENT
                    } else {
                        theme::BORDER_SOFT
                    }))
                    .text_xs()
                    .text_color(rgb(if current {
                        theme::TEXT
                    } else {
                        theme::TEXT_SECONDARY
                    }))
                    .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                    .cursor_pointer()
                    .child(mode_label(mode))
                    .on_click(cx.listener(move |host, _event, _window, cx| {
                        host.run_assets_action(
                            AssetsAction::SetMode {
                                ids: ids.clone(),
                                image: name.clone(),
                                mode,
                            },
                            cx,
                        );
                    }));
                modes = modes.child(chip);
            }
            row = row.child(modes);
        }
        None => {
            column = column.child(caption(SCALE_NEEDS_ONE_IMAGE));
        }
    }
    column = column.child(row);
    column.into_any_element()
}

/// The images: the title, the search, the tiles, and the empty state.
fn images_block<T: AssetsHost>(
    model: &AssetsModel,
    state: &Rc<RefCell<AssetsPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_2();
    column = column.child(
        div()
            .id(SharedString::from("assets-title"))
            .debug_selector(|| "assets-title".to_owned())
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgb(theme::TEXT))
            .child("Images"),
    );
    // figcraft toasts "Select layers to apply the style to" when a style row
    // is clicked with nothing selected. The Inspector has no toast, so the
    // reason is said here instead.
    if model.ids.is_empty() {
        column = column.child(caption(SELECT_LAYERS));
    }
    column = column.child(search_field(state, cx));
    let query = state.borrow().filter().to_owned();
    let images = model.images_for(&query);
    if images.is_empty() {
        column = column.child(empty_state(model));
        return column.into_any_element();
    }
    let mut rows = div().flex().flex_col().gap_2();
    for chunk in images.chunks(2) {
        let mut row = div().flex().gap_2();
        for image in chunk {
            row = row.child(tile(model, image, cx));
        }
        rows = rows.child(row);
    }
    column = column.child(rows);
    column.into_any_element()
}

/// The search field.
fn search_field<T: AssetsHost>(
    state: &Rc<RefCell<AssetsPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let draft = state
        .borrow()
        .draft
        .as_ref()
        .filter(|draft| draft.field == AssetField::Search)
        .map(|draft| draft.text.clone());
    let shown = draft.unwrap_or_else(|| {
        let query = state.borrow().query.clone();
        if query.is_empty() {
            "Search".to_owned()
        } else {
            query
        }
    });
    let shared = Rc::clone(state);
    div()
        .id(SharedString::from("assets-search"))
        .debug_selector(|| "assets-search".to_owned())
        .flex()
        .items_center()
        .px(px(8.0))
        .py(px(7.0))
        .rounded_md()
        .bg(rgb(theme::WINDOW))
        .border_1()
        .border_color(rgb(theme::BORDER_SOFT))
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .cursor_text()
        .child(shown)
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.draft = Some(AssetDraft {
                    field: AssetField::Search,
                    text: state.query.clone(),
                    select_all: true,
                });
            }
            cx.notify();
        }))
        .into_any_element()
}

/// The empty state, word for word as figcraft says it.
fn empty_state(model: &AssetsModel) -> AnyElement {
    let hint = if model.has_any() {
        NO_MATCHING_IMAGES
    } else {
        NO_IMAGES
    };
    let selector = format!("assets-empty-{hint}");
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(hint.to_owned())
        .into_any_element()
}

/// One image, as a tile.
///
/// The tile is the stand-in for figcraft's thumbnail: the image's own facts —
/// its dimensions and its format — where figcraft would draw its pixels. The
/// label under it is the engine's handle for the image, which is its content
/// hash.
fn tile<T: AssetsHost>(model: &AssetsModel, image: &AssetEntry, cx: &mut Context<T>) -> AnyElement {
    let selector = format!("assets-tile-{}", image.short_hash());
    let short = image.short_hash().to_owned();
    let size_selector = format!("assets-tile-{short}-size");
    let size = image.size_label();
    // The click applies the image to the selection. The values it needs are
    // taken out of the model before the closure, so nothing borrowed lives
    // longer than the click.
    let ids = model.ids.clone();
    let hash = image.hash.clone();
    let mode = model.default_mode();
    let mut column = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .w(px(124.0))
        .cursor_pointer()
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_assets_action(
                AssetsAction::Apply {
                    ids: ids.clone(),
                    image: hash.clone(),
                    mode,
                },
                cx,
            );
        }))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .w(px(116.0))
                .h(px(64.0))
                .rounded_md()
                .bg(rgb(theme::SURFACE))
                .border_1()
                .border_color(rgb(theme::BORDER_SOFT))
                .child(
                    div()
                        .id(SharedString::from(size_selector.clone()))
                        .debug_selector(move || size_selector.clone())
                        .text_xs()
                        .text_color(rgb(theme::TEXT_SECONDARY))
                        .child(size),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(image.format),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT))
                .child(short.clone()),
        );
    if image.used > 0 {
        column = column.child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(format!("used by {} layers", image.used)),
        );
    }
    // The way to place the image as a layer of its own. It is a button
    // rather than the tile's own click, because the tile's click applies the
    // image to the selection and the two are different things.
    let hash = image.hash.clone();
    let bytes = Arc::clone(&image.bytes);
    let mode = model.default_mode();
    let place_selector = format!("assets-tile-{short}-place");
    column = column.child(
        div()
            .id(SharedString::from(place_selector.clone()))
            .debug_selector(move || place_selector.clone())
            .flex()
            .items_center()
            .justify_center()
            .w(px(20.0))
            .h(px(20.0))
            .rounded_sm()
            .text_color(rgb(theme::TEXT_MUTED))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("＋")
            .on_click(cx.listener(move |host, _event, _window, cx| {
                // The button is inside the tile, and the tile applies the
                // image: without this the click would do both.
                cx.stop_propagation();
                host.run_assets_action(
                    AssetsAction::Place {
                        image: hash.clone(),
                        data: base64(&bytes),
                        mode,
                    },
                    cx,
                );
            })),
    );
    column.into_any_element()
}

/// A caption: the dim text that names a group.
fn caption(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    use figcraft_doc::{ImageData, NodeKind};
    use figcraft_engine::Session;
    use gpui::{point, size, Entity, VisualTestContext};

    // ── The engine's own model ───────────────────────────────────────────────

    /// A 4×2 PNG: the bytes of a real picture, so the engine can decode them.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x02, 0x08, 0x02, 0x00, 0x00, 0x00, 0xf0,
        0xca, 0xea, 0x34, 0x00, 0x00, 0x00, 0x18, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60,
        0x88, 0x3a, 0x61, 0x13, 0x75, 0xa2, 0x22, 0xea, 0xc4, 0x96, 0xa8, 0x13, 0x0c, 0xc8, 0x1c,
        0x00, 0x90, 0x02, 0x0b, 0xe1, 0xe4, 0x72, 0x2b, 0x34, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
        0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// The hash of [`PNG`]: the engine's name for it.
    const HASH: &str = "9286ec8381c421aba1afe612a257fa2468d761864d30c2f21b7061f3f44e9659";

    /// A second image: the same picture with one byte changed, so it has a
    /// different hash.
    const OTHER_HASH: &str = "aaaaec8381c421aba1afe612a257fa2468d761864d30c2f21b7061f3f44e9659";

    /// A document with one rectangle on it.
    fn document_with() -> (Document, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(page, NodeKind::Rectangle, "Card", 0.0, 0.0, 100.0, 100.0)
            .expect("a rectangle");
        (doc, id)
    }

    /// The document's image store, with [`PNG`] in it.
    fn add_image(doc: &mut Document, hash: &str) {
        doc.images.insert(
            hash.to_owned(),
            ImageData {
                bytes: Arc::new(PNG.to_vec()),
                width: 4,
                height: 2,
            },
        );
    }

    /// A layer that paints [`HASH`].
    fn paint_with(doc: &mut Document, id: NodeId, hash: &str, mode: ImageScaleMode) {
        doc.get_mut(id).expect("the layer").fills = vec![Paint::image(hash, mode)];
    }

    // ── The model ────────────────────────────────────────────────────────────

    #[test]
    fn the_model_reads_the_engines_images() {
        let (mut doc, id) = document_with();
        add_image(&mut doc, HASH);
        add_image(&mut doc, OTHER_HASH);
        let model = AssetsModel::read(&doc, &[id]);

        assert_eq!(model.images.len(), 2, "the document holds two images");
        // The engine files them by content hash, so the list is in hash order.
        let first = &model.images[0];
        assert_eq!(first.hash, HASH);
        assert_eq!(first.width, 4);
        assert_eq!(first.height, 2);
        assert_eq!(first.format, "PNG", "the bytes say what they are");
        assert_eq!(first.size_label(), "4 × 2");
        assert_eq!(first.short_hash(), "9286ec83");
        assert_eq!(first.used, 0, "no layer paints it yet");
        assert_eq!(first.bytes.len(), PNG.len(), "the bytes are the document's");
    }

    #[test]
    fn the_model_counts_the_layers_that_paint_an_image() {
        let (mut doc, id) = document_with();
        add_image(&mut doc, HASH);
        paint_with(&mut doc, id, HASH, ImageScaleMode::Fill);

        let model = AssetsModel::read(&doc, &[id]);
        assert_eq!(model.images[0].used, 1, "the layer paints it");
    }

    #[test]
    fn the_fill_of_a_layer_is_the_engine_own_reading() {
        let (mut doc, id) = document_with();
        add_image(&mut doc, HASH);
        paint_with(&mut doc, id, HASH, ImageScaleMode::Fit);

        let model = AssetsModel::read(&doc, &[id]);
        assert_eq!(
            model.fill,
            AssetValue::Set(HASH.to_owned()),
            "the layer paints that image"
        );
        assert_eq!(
            model.mode,
            AssetValue::Set(ImageScaleMode::Fit),
            "and fills its box that way"
        );
    }

    #[test]
    fn a_layer_with_no_image_is_not_set() {
        let (doc, id) = document_with();
        let model = AssetsModel::read(&doc, &[id]);
        assert_eq!(model.fill, AssetValue::Unset);
        assert_eq!(model.mode, AssetValue::Unset);
        assert_eq!(model.fill.word(), NOT_SET);
    }

    #[test]
    fn layers_that_disagree_are_mixed_per_property() {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let first = doc
            .create(page, NodeKind::Rectangle, "One", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let second = doc
            .create(page, NodeKind::Rectangle, "Two", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        add_image(&mut doc, HASH);
        add_image(&mut doc, OTHER_HASH);
        // The same image on one layer and none on the other: they disagree
        // about the image, and about the scale with it.
        paint_with(&mut doc, first, HASH, ImageScaleMode::Fit);

        let model = AssetsModel::read(&doc, &[first, second]);
        assert_eq!(model.fill, AssetValue::Mixed);
        assert_eq!(model.mode, AssetValue::Mixed);
        assert_eq!(model.fill.word(), MIXED);

        // The same image on both, with different scales: the image agrees and
        // the scale does not. The fold is per property.
        paint_with(&mut doc, second, HASH, ImageScaleMode::Crop);
        let model = AssetsModel::read(&doc, &[first, second]);
        assert_eq!(model.fill, AssetValue::Set(HASH.to_owned()));
        assert_eq!(model.mode, AssetValue::Mixed);
        assert_eq!(model.mode.word(), MIXED);
    }

    #[test]
    fn the_search_matches_the_hash_the_format_and_the_size() {
        let (mut doc, id) = document_with();
        add_image(&mut doc, HASH);
        let model = AssetsModel::read(&doc, &[id]);

        assert_eq!(model.images_for("").len(), 1, "an empty search finds all");
        assert_eq!(model.images_for("9286").len(), 1);
        assert_eq!(model.images_for("png").len(), 1, "the format matches too");
        assert_eq!(model.images_for("4 × 2").len(), 1, "and the size");
        assert!(model.images_for("nothing").is_empty());
        assert!(model.has_any(), "the document holds an image");
    }

    // ── The commands ─────────────────────────────────────────────────────────

    #[test]
    fn a_command_for_a_change_carries_the_engines_own_id_and_params() {
        let (id, params) = AssetsAction::Apply {
            ids: vec![NodeId(7)],
            image: HASH.to_owned(),
            mode: ImageScaleMode::Fit,
        }
        .command();
        assert_eq!(id, "node.set");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["props"]["fills"][0]["type"], json!("image"));
        assert_eq!(params["props"]["fills"][0]["image"], json!(HASH));
        assert_eq!(params["props"]["fills"][0]["scale_mode"], json!("fit"));

        let (id, params) = AssetsAction::SetMode {
            ids: vec![NodeId(7)],
            image: HASH.to_owned(),
            mode: ImageScaleMode::Tile,
        }
        .command();
        assert_eq!(id, "node.set");
        assert_eq!(params["props"]["fills"][0]["scale_mode"], json!("tile"));
        assert_eq!(
            params["props"]["fills"][0]["image"],
            json!(HASH),
            "the scale is part of the fill, so the image stays"
        );

        let (id, params) = AssetsAction::Clear {
            ids: vec![NodeId(7)],
        }
        .command();
        assert_eq!(id, "node.set");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["props"]["fills"], json!([]));

        let (id, params) = AssetsAction::Place {
            image: HASH.to_owned(),
            data: "TWFu".to_owned(),
            mode: ImageScaleMode::Crop,
        }
        .command();
        assert_eq!(id, "image.place");
        assert_eq!(params["data"], json!("TWFu"));
        assert_eq!(params["mode"], json!("crop"));
    }

    #[test]
    fn the_paint_the_panel_sends_is_the_engines_own_paint() {
        // The engine serializes its own `Paint`, so the panel cannot spell a
        // field the engine does not have.
        let paint = image_paint(HASH, ImageScaleMode::Fill);
        assert_eq!(paint["type"], json!("image"));
        assert_eq!(paint["image"], json!(HASH));
        assert_eq!(paint["scale_mode"], json!("fill"));
        assert_eq!(paint["opacity"], json!(1.0));
        assert_eq!(paint["visible"], json!(true));
    }

    #[test]
    fn the_scale_names_are_the_engines_own() {
        // The engine's own serde, so the words cannot drift.
        for mode in MODES {
            assert_eq!(
                serde_json::to_value(mode).expect("the mode serializes"),
                json!(mode_name(mode)),
                "the engine spells {:?} this way",
                mode
            );
        }
        assert_eq!(mode_name(ImageScaleMode::Fill), "fill");
        assert_eq!(mode_name(ImageScaleMode::Fit), "fit");
        assert_eq!(mode_name(ImageScaleMode::Crop), "crop");
        assert_eq!(mode_name(ImageScaleMode::Tile), "tile");
    }

    #[test]
    fn the_engine_accepts_the_commands_the_panel_builds() {
        // The write path, end to end: the panel's command, the engine's
        // command, the engine's document.
        let (mut doc, id) = document_with();
        add_image(&mut doc, HASH);
        let mut session = Session::with_document(doc);
        session
            .execute("select.set", &json!({ "ids": [id.0] }))
            .expect("the layer is selected");

        let (command, params) = AssetsAction::Apply {
            ids: vec![id],
            image: HASH.to_owned(),
            mode: ImageScaleMode::Fit,
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the image is applied");

        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(node.fills.len(), 1, "the layer paints the image");
        let Some(PaintKind::Image(image)) = node.fills.first().map(|paint| &paint.kind) else {
            panic!("the fill is an image");
        };
        assert_eq!(image.image, HASH);
        assert_eq!(image.scale_mode, ImageScaleMode::Fit);

        // And the scale can be changed on its own.
        let (command, params) = AssetsAction::SetMode {
            ids: vec![id],
            image: HASH.to_owned(),
            mode: ImageScaleMode::Tile,
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the scale is changed");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        let Some(PaintKind::Image(image)) = node.fills.first().map(|paint| &paint.kind) else {
            panic!("the fill is an image");
        };
        assert_eq!(image.image, HASH, "the image is still the same one");
        assert_eq!(image.scale_mode, ImageScaleMode::Tile);
    }

    #[test]
    fn the_engine_accepts_the_placing_command() {
        let (doc, _) = document_with();
        let mut session = Session::with_document(doc);
        let (command, params) = AssetsAction::Place {
            image: HASH.to_owned(),
            data: base64(PNG),
            mode: ImageScaleMode::Fill,
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the image is placed");

        let doc = session.doc().expect("a document");
        assert!(
            doc.images.contains_key(HASH),
            "the engine files the image by its content hash"
        );
        let placed = doc
            .nodes()
            .find(|node| {
                node.fills.iter().any(
                    |paint| matches!(&paint.kind, PaintKind::Image(image) if image.image == HASH),
                )
            })
            .expect("the layer that paints the image");
        assert_eq!(placed.width, 4.0, "the layer is the size of the image");
        assert_eq!(placed.height, 2.0);
    }

    #[test]
    fn the_base64_helper_is_the_standard_one() {
        // The vectors from RFC 4648.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"abc"), "YWJj");
        // And it is what the engine's own decoder reads back.
        assert_eq!(base64(PNG), base64(PNG));
        assert!(!base64(PNG).is_empty());
        assert_eq!(base64(PNG).len() % 4, 0, "the length is a multiple of four");
    }

    // ── The panel's own state ────────────────────────────────────────────────

    #[test]
    fn the_search_follows_what_has_been_typed() {
        let mut state = AssetsPanelState::new();
        assert!(!state.is_busy(), "nothing is being typed");
        state.draft = Some(AssetDraft {
            field: AssetField::Search,
            text: String::new(),
            select_all: true,
        });
        assert!(state.is_busy(), "a field is open");
        state.handle_key("9");
        state.handle_key("2");
        assert_eq!(state.filter(), "92", "the list follows the query");
        assert!(
            state.handle_key("enter").is_none(),
            "the search is not an edit of the document"
        );
        assert_eq!(state.query, "92", "and it is committed");
        assert!(!state.is_busy(), "the field closed");
    }

    // ── The panel, in a window ───────────────────────────────────────────────

    /// A host for the section in a window: it draws the section, remembers
    /// what the section asked for, and does what it is asked.
    struct Host {
        model: AssetsModel,
        state: Rc<RefCell<AssetsPanelState>>,
        asked: Rc<RefCell<Vec<AssetsAction>>>,
    }

    impl Host {
        fn new(model: AssetsModel) -> Self {
            Self {
                model,
                state: Rc::new(RefCell::new(AssetsPanelState::new())),
                asked: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl AssetsHost for Host {
        fn run_assets_action(&mut self, action: AssetsAction, cx: &mut Context<Self>) {
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
            assets_section(&model, &state, cx)
        }
    }

    /// Open a window with the section in it, over the given model.
    fn open_section(
        cx: &mut gpui::TestAppContext,
        model: AssetsModel,
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

    #[gpui::test]
    fn the_section_draws_the_images_it_is_given(cx: &mut gpui::TestAppContext) {
        // The render test: the section, on its own, in a real window, showing
        // the document's images.
        let (mut doc, id) = document_with();
        add_image(&mut doc, HASH);
        let model = AssetsModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        // The tile is laid out with a real size.
        let tile = bounds(window, "assets-tile-9286ec83");
        assert!(
            tile.size.width > px(0.0) && tile.size.height > px(0.0),
            "the image's tile is on screen: {tile:?}"
        );
        // The stand-in says what the engine knows about the image.
        let size = bounds(window, "assets-tile-9286ec83-size");
        assert!(
            size.size.width > px(0.0),
            "the image's dimensions are on screen: {size:?}"
        );
        // And the way to place it is there.
        assert!(
            window.debug_bounds("assets-tile-9286ec83-place").is_some(),
            "the tile offers the way to place the image"
        );
    }

    #[gpui::test]
    fn the_section_shows_the_empty_state_when_there_are_no_images(cx: &mut gpui::TestAppContext) {
        // figcraft's empty state, word for word.
        let (doc, id) = document_with();
        let model = AssetsModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let selector: &'static str =
            Box::leak("assets-empty-No images yet.".to_owned().into_boxed_str());
        let empty = bounds(window, selector);
        assert!(
            empty.size.height > px(0.0),
            "the section says figcraft's own words: {empty:?}"
        );
    }

    #[gpui::test]
    fn the_section_shows_the_image_a_node_paints(cx: &mut gpui::TestAppContext) {
        // A node with an image fill: the panel says which image, and how it
        // fills.
        let (mut doc, id) = document_with();
        add_image(&mut doc, HASH);
        paint_with(&mut doc, id, HASH, ImageScaleMode::Fit);
        let model = AssetsModel::read(&doc, &[id]);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let value = bounds(window, "assets-fill-value-9286ec83");
        assert!(
            value.size.width > px(0.0),
            "the image is on screen: {value:?}"
        );
        let scale = bounds(window, "assets-scale-value-Fit");
        assert!(
            scale.size.width > px(0.0),
            "the scale is on screen: {scale:?}"
        );
        // The way to take the image off is there.
        assert!(
            window.debug_bounds("assets-fill-clear").is_some(),
            "the row offers the way to clear the fill"
        );
    }
}
