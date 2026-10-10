use gpui::{
    div, fill, point, prelude::*, px as gpui_px, relative, rgb, rgba, size, App, Bounds,
    ClipboardItem, Context, DispatchPhase, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, GlobalElementId, HitboxBehavior, HitboxId, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, PinchEvent, Pixels,
    Point, Render, ScrollWheelEvent, SharedString, Size, Style, TextRun, UTF16Selection, Window,
};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, HashSet},
    ops::Range,
    rc::Rc,
};

gpui::actions!(
    spool_text,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Copy,
        Cut,
    ]
);

use crate::{
    diagnostics,
    hierarchy::Hierarchy,
    operations::{EditSession, OperationError, SemanticOperation, StructureChange},
    snap,
    source_document::{NodeId, PersistentDocument},
    source_document::{SourceBinding, StructuralNode},
    theme,
};

#[cfg(debug_assertions)]
#[path = "canvas_workloads.rs"]
mod workloads;

#[cfg(test)]
#[path = "canvas_benchmarks.rs"]
mod benchmarks;

const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 4.0;
const WORLD_BOUNDS: Size<f32> = size(764.0, 688.0);
const WORLD_CENTER: Point<f32> = point(382.0, 344.0);
const LABEL_HEIGHT: f32 = 24.0;
const MIN_OBJECT_SIZE: f32 = 20.0;

/// How far a duplicate is offset from the object it was copied from.
///
/// The research corpus establishes no canonical value — no product documents an
/// offset distance for either the duplicate keystroke or the modifier-drag — so
/// this is Spool's number and not a claim about Figma.
///
/// It is one constant rather than two literals because there are two routes to a
/// duplicate (`⌘D` and `⌥`-drag) and they must not drift apart: a copy made by
/// dragging has to land in the same place a copy made by keystroke lands, or the
/// same object appears to duplicate differently depending on how you asked.
///
/// The value only has to satisfy two things: be far enough that the copy is
/// visibly not the original, and be small enough that the copy is still
/// recognisably where the original was. 16px does both at the zooms the editor
/// allows.
const DUPLICATE_OFFSET: f32 = 16.0;
const DRAG_THRESHOLD: f32 = 4.0;

/// The side a creation tool makes when the user clicks rather than drags.
///
/// A click has no bounds to honour, so the tool needs an answer of its own. In
/// world units, so it is a size in the document and not in the window: the same
/// 100 units at 50% and at 200%.
///
/// `docs/research/products/figma.md` records the rule this implements — every
/// tool works with a click and with a drag, and the two differ: click gives a
/// default size, drag gives explicit bounds.
const DEFAULT_CREATION_SIZE: f32 = 100.0;

/// The box a Text click leaves.
///
/// Text auto-sizes once it holds text, so this is only the box the caret starts
/// in. It is the size Text click-creation already used.
const DEFAULT_TEXT_WIDTH: f32 = 180.0;
const DEFAULT_TEXT_HEIGHT: f32 = 48.0;
/// Wheel deltas are converted at this many pixels per line.
///
/// Named because both the pan reading and the zoom reading use it, and two
/// independent literals that agree by accident are two literals waiting to stop
/// agreeing.
const WHEEL_LINE_PX: f32 = 24.0;

/// How fast a wheel notch zooms, per pixel of converted delta.
const ZOOM_WHEEL_GAIN: f32 = 0.002;

/// Screen pixels left around content on each side when fitting the viewport.
const FIT_MARGIN: f32 = 48.0;
const RESIZE_HANDLE_SIZE: f32 = 8.0;
const RESIZE_HANDLE_HIT_RADIUS: f32 = 7.0;
/// How far one press of `+` or `-` moves the zoom.
///
/// Figma's step, and the reason it is a ratio rather than an offset: doubling
/// from 7% to 14% is as useless as 240% to 480%, and both are what a fixed
/// number of points produces somewhere in the range.
const ZOOM_STEP: f32 = 1.2;

/// Conservative world-space padding applied to every side of an object's
/// geometry box for Phase 14 viewport culling. An object's element tree is
/// constructed only when its padded box intersects the viewport (inclusive
/// edges, the same axis-aligned model as `diagnostics::intersects`).
///
/// The padding must cover visual overflow beyond the geometry box: frame and
/// starter-artboard labels paint `LABEL_HEIGHT` (24) world units above the
/// box, strokes add a few world units on each side, and text content can
/// overflow its box. All of that overflow is specified in world units scaled
/// by zoom (`px!(value, zoom)`), so a world-space padding stays conservative
/// at every zoom level from `MIN_ZOOM` to `MAX_ZOOM`. 64 world units covers
/// the 24-unit label plus roughly 40 world units (about two to three 14-unit
/// text lines at zoom 1) of additional margin. Objects whose box lies
/// entirely beyond this padding are skipped; everything else is constructed.
const CULL_PADDING: f32 = 64.0;

macro_rules! px {
    ($value:expr, $zoom:ident) => {
        gpui_px($value * $zoom)
    };
}

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    offset: Point<f32>,
    zoom: f32,
    viewport: Size<f32>,
    initialized: bool,
    /// The bounds of the most recent fit request, waiting for a viewport.
    ///
    /// A project is loaded before the first frame, so "frame my content" is
    /// asked while the camera still has a zero-sized viewport. Answering then
    /// means dividing by zero and clamping to the minimum zoom — which is how
    /// opening a project used to leave the user staring at a 10%-zoom speck.
    /// The request is kept and applied the moment a real viewport arrives.
    pending_fit: Option<WorldRect>,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            offset: point(0.0, 0.0),
            zoom: 1.0,
            viewport: size(0.0, 0.0),
            initialized: false,
            pending_fit: None,
        }
    }
}

impl Camera {
    pub fn world_to_screen(&self, world: Point<f32>) -> Point<f32> {
        point(
            (world.x - self.offset.x) * self.zoom,
            (world.y - self.offset.y) * self.zoom,
        )
    }

    pub fn screen_to_world(&self, screen: Point<f32>) -> Point<f32> {
        point(
            screen.x / self.zoom + self.offset.x,
            screen.y / self.zoom + self.offset.y,
        )
    }

    fn resize(&mut self, viewport: Size<f32>) -> bool {
        if viewport == self.viewport {
            return false;
        }
        if self.initialized {
            self.offset.x -= (viewport.width - self.viewport.width) / (2.0 * self.zoom);
            self.offset.y -= (viewport.height - self.viewport.height) / (2.0 * self.zoom);
        } else {
            self.offset = point(
                WORLD_CENTER.x - viewport.width / (2.0 * self.zoom),
                WORLD_CENTER.y - viewport.height / (2.0 * self.zoom),
            );
            self.initialized = true;
        }
        self.viewport = viewport;
        if let Some(bounds) = self.pending_fit.take() {
            self.apply_fit(bounds);
        }
        true
    }

    /// Multiply the zoom by `factor`, keeping the world point under `screen` there.
    ///
    /// Every zoom path goes through this one function — the wheel, a trackpad
    /// pinch, the zoom keys, zoom-to-cursor — so this is where an unusable
    /// factor is refused. A pinch reports its own delta, and a delta of `-1.0`
    /// or worse arrives as a factor of zero or less; a NaN arrives from a
    /// malformed gesture. Either one would otherwise be multiplied straight into
    /// `zoom`, and from there into `offset`, and a single NaN offset makes the
    /// whole canvas disappear with no way back. Ignoring the factor leaves the
    /// camera exactly where it was, which is the only recoverable answer to a
    /// number that does not describe a zoom.
    fn zoom_at(&mut self, factor: f32, screen: Point<f32>) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let world_anchor = self.screen_to_world(screen);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.offset = point(
            world_anchor.x - screen.x / self.zoom,
            world_anchor.y - screen.y / self.zoom,
        );
    }

    /// Set an absolute zoom, keeping `screen` pinned to the same world point.
    fn set_zoom_at(&mut self, zoom: f32, screen: Point<f32>) {
        self.zoom_at(zoom / self.zoom, screen);
    }

    fn set_zoom_at_center(&mut self, zoom: f32) {
        self.zoom_at(
            zoom / self.zoom,
            point(self.viewport.width / 2.0, self.viewport.height / 2.0),
        );
    }

    /// Fit a world-space box into the viewport.
    ///
    /// This is the camera's only fit rule, and it takes the box as an argument
    /// rather than reading a constant. The previous version fitted a hard-coded
    /// `WORLD_BOUNDS`, which was an accidental bound: every product in the
    /// research corpus is either explicitly infinite (Figma, tldraw) or
    /// explicitly bounded (Canva), and a fixed 764x688 is neither — it made
    /// zoom-to-fit mean "show me the prototype's placeholder artboard" instead
    /// of "show me what is in this document".
    fn fit_bounds(&mut self, bounds: WorldRect) {
        self.pending_fit = Some(bounds);
        if self.initialized {
            self.apply_fit(bounds);
            self.pending_fit = None;
        }
    }

    fn apply_fit(&mut self, bounds: WorldRect) {
        let available = size(
            (self.viewport.width - FIT_MARGIN * 2.0).max(1.0),
            (self.viewport.height - FIT_MARGIN * 2.0).max(1.0),
        );
        self.zoom = (available.width / bounds.width().max(1.0))
            .min(available.height / bounds.height().max(1.0))
            // No cap at 100%: fitting a small frame does magnify it, in Figma
            // and in tldraw, because "fit" means fit. `⇧0` is the key for
            // actual size. The floor only stops content becoming a speck.
            .clamp(MIN_ZOOM, MAX_ZOOM);
        self.offset = point(
            bounds.center().x - self.viewport.width / (2.0 * self.zoom),
            bounds.center().y - self.viewport.height / (2.0 * self.zoom),
        );
    }

    /// Fit the placeholder starter scene, for a document with no real content.
    fn fit(&mut self) {
        self.fit_bounds(WorldRect {
            min: point(
                WORLD_CENTER.x - WORLD_BOUNDS.width / 2.0,
                WORLD_CENTER.y - WORLD_BOUNDS.height / 2.0,
            ),
            max: point(
                WORLD_CENTER.x + WORLD_BOUNDS.width / 2.0,
                WORLD_CENTER.y + WORLD_BOUNDS.height / 2.0,
            ),
        });
    }

    fn pan_from(
        &mut self,
        start_offset: Point<f32>,
        start_pointer: Point<f32>,
        pointer: Point<f32>,
    ) {
        let delta = point(pointer.x - start_pointer.x, pointer.y - start_pointer.y);
        self.offset = point(
            start_offset.x - delta.x / self.zoom,
            start_offset.y - delta.y / self.zoom,
        );
    }

    /// Phase 14 conservative culling predicate: true when the object's
    /// geometry can affect the current viewport, in which case its element
    /// tree must be constructed.
    ///
    /// This is exactly the `diagnostics::intersects` model — inclusive
    /// axis-aligned geometry-box intersection with the viewport in screen
    /// space — applied to the object's box expanded by `CULL_PADDING` world
    /// units on every side, so the constructed set is always a superset of
    /// the diagnostic visibility model. Padding is world space, so the
    /// predicate follows the actual camera zoom and offset rather than
    /// assuming zoom = 1.
    ///
    /// Conservative on uncertainty: any input the intersection model cannot
    /// evaluate (non-finite geometry, camera offset, zoom or viewport;
    /// non-positive zoom or viewport; negative size) returns true so that
    /// unevaluable state constructs the object instead of culling it. This
    /// includes the pre-prepaint render where the viewport is still empty.
    /// Could a box here pull a drag that is happening on screen?
    ///
    /// The same predicate the renderer culls with, and for the same reason: a
    /// box the user cannot see is not the thing they meant to line up with, and
    /// tldraw documents the same rule for its candidate set. Without it, an
    /// object parked ten thousand units off-canvas quietly holds a drag in
    /// place — the one kind of snap that reads as the object jumping for no
    /// reason.
    ///
    /// A camera with no viewport yet cannot bound anything, so it answers
    /// "yes". The alternative is a snapping engine that is inert until the
    /// first frame, which is not a state a user should have to notice.
    fn sees(&self, rect: snap::Rect) -> bool {
        if self.viewport.width <= 0.0 || self.viewport.height <= 0.0 {
            return true;
        }
        self.affects_viewport(
            point(rect.left(), rect.top()),
            size(rect.width, rect.height),
        )
    }

    /// Is this screen point inside the viewport?
    ///
    /// A camera with no viewport yet contains nothing, which is the right
    /// answer for an anchor: there is no screen to be inside of, so there is
    /// nothing better to anchor to.
    fn viewport_contains(&self, screen: Point<f32>) -> bool {
        screen.x >= 0.0
            && screen.y >= 0.0
            && screen.x <= self.viewport.width
            && screen.y <= self.viewport.height
    }

    fn affects_viewport(&self, position: Point<f32>, object_size: Size<f32>) -> bool {
        let padded_position = point(position.x - CULL_PADDING, position.y - CULL_PADDING);
        let padded_size = size(
            object_size.width + 2.0 * CULL_PADDING,
            object_size.height + 2.0 * CULL_PADDING,
        );
        let evaluable = self.zoom > 0.0
            && self.viewport.width > 0.0
            && self.viewport.height > 0.0
            && object_size.width >= 0.0
            && object_size.height >= 0.0
            && [
                self.zoom,
                self.viewport.width,
                self.viewport.height,
                self.offset.x,
                self.offset.y,
                padded_position.x,
                padded_position.y,
                padded_size.width,
                padded_size.height,
            ]
            .iter()
            .all(|value| value.is_finite()); // Not evaluable -> construct (conservative); otherwise the padded
                                             // intersection decides.
        !evaluable
            || diagnostics::intersects(
                padded_position,
                padded_size,
                self.offset,
                self.viewport,
                self.zoom,
            )
    }
}

// `Ord` is additive: it lets the runtime projection key a `BTreeMap` by
// `ObjectId` so projection order is deterministic. No existing behaviour
// depends on this, and `ObjectId` remains a runtime lookup key, not identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(pub u64);

impl ObjectId {
    pub const LANDING: Self = Self(1);
    pub const EDITOR: Self = Self(2);
    pub const FEATURES: Self = Self(3);
    pub const MOBILE: Self = Self(4);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectType {
    Frame,
    Rectangle,
    Ellipse,
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Frame,
    Rectangle,
    Ellipse,
    Pen,
    Text,
    Comment,
}

impl Tool {
    fn creates_object(self) -> Option<ObjectType> {
        match self {
            Self::Frame => Some(ObjectType::Frame),
            Self::Rectangle => Some(ObjectType::Rectangle),
            Self::Ellipse => Some(ObjectType::Ellipse),
            Self::Text => Some(ObjectType::Text),
            Self::Select | Self::Pen | Self::Comment => None,
        }
    }
}

impl ObjectType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Frame => "Frame",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Text => "Text",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Color {
    pub const fn from_rgb(value: u32) -> Self {
        Self {
            red: ((value >> 16) & 0xff) as u8,
            green: ((value >> 8) & 0xff) as u8,
            blue: (value & 0xff) as u8,
        }
    }

    pub const fn to_rgb(self) -> u32 {
        ((self.red as u32) << 16) | ((self.green as u32) << 8) | self.blue as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    pub color: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    pub color: Color,
    pub width: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObjectStyle {
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
    /// Corner radius in pixels; zero means square.
    ///
    /// Mirrors CSS `border-radius`. Carried in the style rather than folded
    /// into geometry because it is paint, not layout.
    pub border_radius: f32,
    /// Alpha from 0 to 1. Mirrors CSS `opacity`.
    pub opacity: f32,
}

impl Default for ObjectStyle {
    fn default() -> Self {
        Self {
            fill: None,
            stroke: None,
            border_radius: 0.0,
            opacity: 1.0,
        }
    }
}

/// Everything about one object's appearance that the editor can change.
///
/// One snapshot per object, so undo restores the appearance as it was rather
/// than trying to reverse individual properties. Keeping text colour and font
/// size here — rather than beside it on the object — is what lets one style edit
/// cover "the label got smaller and darker" as a single history entry.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Appearance {
    pub style: ObjectStyle,
    pub text_color: Option<Color>,
    /// Authored CSS `font-size` in pixels, when one was authored.
    pub font_size: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StyleEdit {
    Fill(Option<Color>),
    Stroke(Option<Color>),
    StrokeWidth(f32),
    /// Text colour. `None` removes an explicit colour and lets the renderer
    /// choose one again.
    TextColor(Option<Color>),
    /// Font size in pixels.
    FontSize(f32),
    /// Corner radius in pixels.
    BorderRadius(f32),
    /// Alpha from 0 to 1, clamped.
    Opacity(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesignObject {
    /// Runtime lookup key. Never durable; allocated locally from `next_id`.
    pub id: ObjectId,
    /// Carries a persistent identity, but only sometimes.
    ///
    /// Two distinct cases, and conflating them would let a runtime-local id be
    /// mistaken for durable identity:
    ///
    /// - **Projected.** Objects built by
    ///   `document_runtime_bridge::RuntimeProjection::canvas_objects` carry the
    ///   `NodeId` of a real `lamine.yaml` structural node.
    /// - **Locally minted.** Objects this `Document` creates or duplicates get
    ///   an id from `allocate_node_id` (`spool-node-<16 hex>`). No structural
    ///   node backs them yet and no save path writes them anywhere, so today
    ///   they are runtime-local values that merely share the `NodeId` type.
    ///
    /// `NodeId` here means "opaque, well-formed identifier", not "exists in
    /// the persistent document". Only the projected case may be persisted.
    pub spool_id: NodeId,
    pub name: String,
    pub position: Point<f32>,
    pub size: Size<f32>,
    pub object_type: ObjectType,
    pub text_content: Option<String>,
    /// Authored CSS `color` for this object's text, when source declared one.
    ///
    /// `None` means "nobody authored a colour", which is different from a
    /// painted colour: the renderer picks its own in that case. Keeping the
    /// distinction is what stops a guess from silently overriding source.
    pub text_color: Option<Color>,
    /// Authored CSS `font-size` in pixels, when source declared one.
    pub font_size: Option<f32>,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
    /// CSS `border-radius` in pixels; zero means square corners.
    pub border_radius: f32,
    /// CSS `opacity`, 0 to 1.
    pub opacity: f32,
}

impl DesignObject {
    fn contains(&self, point: Point<f32>) -> bool {
        point.x >= self.position.x
            && point.y >= self.position.y
            && point.x <= self.position.x + self.size.width
            && point.y <= self.position.y + self.size.height
    }

    fn geometry(&self) -> ObjectGeometry {
        ObjectGeometry {
            position: self.position,
            size: self.size,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub position: Point<f32>,
    pub size: Size<f32>,
}

type ObjectGeometry = Geometry;

/// An Inspector geometry change that has not been recorded yet.
///
/// The Inspector owns the widget; the canvas owns the document and the history
/// boundary. Keeping the boundary here is what stops a continuous control from
/// becoming one history entry per pointer movement: the shell drags, and the
/// whole drag is one operation when it ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeometryScrub {
    pub id: ObjectId,
    /// The geometry the object had before the scrub started.
    pub before: Geometry,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GeometryChange {
    pub id: ObjectId,
    pub before: Geometry,
    pub after: Geometry,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StyleChange {
    pub id: ObjectId,
    pub before: Appearance,
    pub after: Appearance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextChange {
    pub id: ObjectId,
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObjectPlacement {
    pub object: DesignObject,
    pub index: usize,
}

/// Which way a command is being replayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayDirection {
    Undo,
    Redo,
}

#[derive(Clone, Debug, PartialEq)]
enum CommandOperation {
    Geometry(Vec<GeometryChange>),
    Style(Vec<StyleChange>),
    Text(Vec<TextChange>),
    Insert(Vec<ObjectPlacement>),
    Delete(Vec<ObjectPlacement>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentCommand {
    operation: CommandOperation,
}

impl DocumentCommand {
    pub fn geometry(changes: Vec<GeometryChange>) -> Self {
        Self {
            operation: CommandOperation::Geometry(changes),
        }
    }

    pub fn style(changes: Vec<StyleChange>) -> Self {
        Self {
            operation: CommandOperation::Style(changes),
        }
    }

    pub fn text(changes: Vec<TextChange>) -> Self {
        Self {
            operation: CommandOperation::Text(changes),
        }
    }

    pub fn insert(objects: Vec<ObjectPlacement>) -> Self {
        Self {
            operation: CommandOperation::Insert(objects),
        }
    }

    pub fn delete(objects: Vec<ObjectPlacement>) -> Self {
        Self {
            operation: CommandOperation::Delete(objects),
        }
    }
}

impl DocumentCommand {
    /// Whether applying this command would change nothing.
    ///
    /// A no-op command never enters history, so a gesture that ends where it
    /// started produces no entry and does not clear redo.
    pub fn is_noop(&self) -> bool {
        match &self.operation {
            CommandOperation::Geometry(changes) => changes.iter().all(|c| c.before == c.after),
            CommandOperation::Style(changes) => changes.iter().all(|c| c.before == c.after),
            CommandOperation::Text(changes) => changes.iter().all(|c| c.before == c.after),
            CommandOperation::Insert(objects) | CommandOperation::Delete(objects) => {
                objects.is_empty()
            }
        }
    }

    /// Check the command can apply, before any state changes.
    pub fn validate(&self, document: &Document) -> Result<(), OperationError> {
        // Only in-place edits need their target present. An insert adds the
        // objects, and a delete has already removed them, so requiring
        // presence there would reject every legitimate command.
        let ids: Vec<ObjectId> = match &self.operation {
            CommandOperation::Geometry(changes) => changes.iter().map(|c| c.id).collect(),
            CommandOperation::Style(changes) => changes.iter().map(|c| c.id).collect(),
            CommandOperation::Text(changes) => changes.iter().map(|c| c.id).collect(),
            CommandOperation::Insert(_) | CommandOperation::Delete(_) => Vec::new(),
        };
        for id in ids {
            if document.object(id).is_none() {
                return Err(OperationError::MissingObject(id));
            }
        }
        Ok(())
    }

    /// Apply the command in one direction. This is the undo/redo mechanism:
    /// each variant replays `before` or `after`, and insert/delete are
    /// symmetric inverses by construction.
    pub fn replay(&self, document: &mut Document, direction: ReplayDirection) {
        let forward = matches!(direction, ReplayDirection::Redo);
        match &self.operation {
            CommandOperation::Geometry(changes) => {
                for change in changes {
                    document.set_geometry(
                        change.id,
                        if forward { change.after } else { change.before },
                    );
                }
            }
            CommandOperation::Style(changes) => {
                for change in changes {
                    document.set_appearance(
                        change.id,
                        if forward { change.after } else { change.before },
                    );
                }
            }
            CommandOperation::Text(changes) => {
                for change in changes {
                    document.set_text_content(
                        change.id,
                        if forward {
                            change.after.clone()
                        } else {
                            change.before.clone()
                        },
                    );
                }
            }
            CommandOperation::Insert(objects) => {
                // Undoing an insert removes what it added; redoing restores it.
                if forward {
                    document.insert_objects(objects);
                } else {
                    let ids: Vec<ObjectId> = objects
                        .iter()
                        .map(|placement| placement.object.id)
                        .collect();
                    document.remove_objects(&ids);
                }
            }
            CommandOperation::Delete(objects) => {
                // Undoing a delete puts the removed objects back.
                if forward {
                    let ids: Vec<ObjectId> = objects
                        .iter()
                        .map(|placement| placement.object.id)
                        .collect();
                    document.remove_objects(&ids);
                } else {
                    document.insert_objects(objects);
                }
            }
        }
    }

    /// A copy with the no-op entries removed.
    ///
    /// Public because the semantic boundary is the right place to trim a
    /// partially-changed command: a multi-object gesture where some objects
    /// did not actually move should record only the ones that did.
    pub fn normalized(mut self) -> Self {
        match &mut self.operation {
            CommandOperation::Geometry(changes) => changes.retain(|c| c.before != c.after),
            CommandOperation::Style(changes) => changes.retain(|c| c.before != c.after),
            CommandOperation::Text(changes) => changes.retain(|c| c.before != c.after),
            CommandOperation::Insert(_) | CommandOperation::Delete(_) => {}
        }
        self
    }
}

#[derive(Clone, Debug)]
pub struct Document {
    objects: Vec<DesignObject>,
    next_id: u64,
    next_node_id: u64,
    next_names: [u64; 4],
    layer_structure_revision: u64,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            objects: vec![
                frame(
                    ObjectId::LANDING,
                    node_id("spool-node-landing"),
                    "Landing",
                    0.0,
                    24.0,
                    430.0,
                    286.0,
                ),
                frame(
                    ObjectId::EDITOR,
                    node_id("spool-node-editor"),
                    "Editor",
                    454.0,
                    24.0,
                    310.0,
                    252.0,
                ),
                frame(
                    ObjectId::FEATURES,
                    node_id("spool-node-features"),
                    "Features",
                    106.0,
                    366.0,
                    394.0,
                    280.0,
                ),
                frame(
                    ObjectId::MOBILE,
                    node_id("spool-node-mobile"),
                    "Mobile",
                    524.0,
                    366.0,
                    192.0,
                    322.0,
                ),
            ],
            next_id: 5,
            next_node_id: 1,
            next_names: [1; 4],
            layer_structure_revision: 0,
        }
    }
}

impl Document {
    /// A document with no objects.
    ///
    /// `Document::default()` is the canvas starter scene, which is populated
    /// with four demonstration frames. Anything that needs a blank document —
    /// a test, or a runtime built purely from a projection — wants this.
    // Test-only: `Document::from_design_objects` is how a runtime built purely
    // from a projection gets its object list, and nothing in the editor needs a
    // blank starter scene. Compiled out of the binary rather than silenced, so
    // "unused" cannot quietly become "shipped".
    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            objects: Vec::new(),
            next_id: 1,
            next_node_id: 1,
            next_names: [1; 4],
            layer_structure_revision: 0,
        }
    }

    /// Build a runtime document from already-projected canvas objects.
    ///
    /// This is the bridge between `RuntimeProjection::canvas_objects` and the
    /// live editor. It is deliberately one-way: the runtime document is a
    /// disposable interpretation, and nothing here writes projected geometry
    /// back into the persistent document.
    ///
    /// The counters are seeded so that anything the user draws afterwards gets
    /// a key above every projected key. Projected keys come from the reserved
    /// range owned by `document_runtime_bridge`, so this only has to clear the
    /// highest one actually present rather than assume a base.
    ///
    /// `next_node_id` restarts at 1 because projected objects keep their real
    /// `NodeId`s; the counter only mints ids for objects created later in this
    /// session.
    pub fn from_design_objects(objects: Vec<DesignObject>) -> Self {
        let next_id = objects
            .iter()
            .map(|object| object.id.0)
            .max()
            .map(|highest| highest + 1)
            .unwrap_or(1);
        // Provisional visibility bridge.
        //
        // A projection currently supplies geometry but no paint: fill and
        // stroke are `None` because the authored CSS has not been read yet. An
        // object with neither is drawn as nothing, so a correctly projected
        // project would be present in the document, in the layers panel, and
        // hit-testable, yet invisible on the canvas.
        //
        // Falling back to the canvas default style keeps the projected scene
        // visible while CSS ownership is still unimplemented. This is
        // deliberately NOT a style claim: it is a placeholder so the pipeline
        // can be seen end to end. When CSS ownership lands it must replace this
        // fallback, not sit beside it.
        let objects = objects
            .into_iter()
            .map(|mut object| {
                if object.fill.is_none() && object.stroke.is_none() {
                    let default = default_style(object.object_type);
                    object.fill = default.fill;
                    object.stroke = default.stroke;
                }
                object
            })
            .collect::<Vec<_>>();
        let mut document = Self {
            objects,
            next_id,
            next_node_id: 1,
            next_names: [1; 4],
            layer_structure_revision: 0,
        };
        // Assign a layer-structure revision per inserted object so layers and
        // other observers see the document as freshly built rather than empty.
        document.layer_structure_revision = document.objects.len() as u64;
        document
    }

    pub fn layer_structure_revision(&self) -> u64 {
        self.layer_structure_revision
    }

    pub fn objects(&self) -> &[DesignObject] {
        &self.objects
    }

    pub fn object(&self, id: ObjectId) -> Option<&DesignObject> {
        self.objects.iter().find(|object| object.id == id)
    }

    pub fn text_content(&self, id: ObjectId) -> Option<&str> {
        self.object(id)?.text_content.as_deref()
    }

    /// Mirror a new name from the persistent document onto the runtime object.
    ///
    /// Not a second place a name can be edited: the persistent document is the
    /// authority, and this only refreshes the copy the Inspector and the layers
    /// list read. Called after a rename, never instead of one.
    pub fn set_object_name(&mut self, id: ObjectId, name: String) -> bool {
        let Some(object) = self.objects.iter_mut().find(|object| object.id == id) else {
            return false;
        };
        if object.name == name {
            return false;
        }
        object.name = name;
        true
    }

    pub fn set_text_content(&mut self, id: ObjectId, text: String) -> bool {
        let Some(object) = self.objects.iter_mut().find(|object| object.id == id) else {
            return false;
        };
        if object.object_type != ObjectType::Text {
            return false;
        }
        object.text_content = Some(text);
        true
    }

    fn allocate_id(&mut self) -> ObjectId {
        let id = ObjectId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Mint an identity for a newly created or duplicated object.
    ///
    /// The counter alone is not sufficient. A loaded project brings its own
    /// `NodeId`s from `lamine.yaml`, and those are arbitrary authored strings —
    /// not necessarily produced by this counter. Restarting the counter at 1 in
    /// [`Document::from_design_objects`] therefore risked minting an identity a
    /// live node already owns, which would make two objects share one durable
    /// identity.
    ///
    /// So the allocator checks against what is actually present and skips
    /// collisions. That is authoritative and deterministic regardless of how
    /// the document was built.
    fn allocate_node_id(&mut self) -> NodeId {
        loop {
            let candidate = node_id(format!("spool-node-{:016x}", self.next_node_id));
            self.next_node_id += 1;
            if !self
                .objects
                .iter()
                .any(|object| object.spool_id == candidate)
            {
                return candidate;
            }
        }
    }

    fn allocate_name(&mut self, object_type: ObjectType) -> String {
        let index = match object_type {
            ObjectType::Frame => 0,
            ObjectType::Rectangle => 1,
            ObjectType::Ellipse => 2,
            ObjectType::Text => 3,
        };
        let number = self.next_names[index];
        self.next_names[index] += 1;
        format!("{} {number}", object_type.label())
    }

    pub fn create_object(
        &mut self,
        object_type: ObjectType,
        position: Point<f32>,
        object_size: Size<f32>,
        text_content: Option<String>,
    ) -> DesignObject {
        let id = self.allocate_id();
        let spool_id = self.allocate_node_id();
        let object = DesignObject {
            id,
            spool_id,
            name: self.allocate_name(object_type),
            position,
            size: size(
                object_size.width.max(MIN_OBJECT_SIZE),
                object_size.height.max(MIN_OBJECT_SIZE),
            ),
            object_type,
            text_content,
            text_color: None,
            font_size: None,
            fill: default_style(object_type).fill,
            stroke: default_style(object_type).stroke,
            border_radius: default_style(object_type).border_radius,
            opacity: 1.0,
        };
        self.insert_object(object.clone(), self.objects.len());
        object
    }

    fn insert_object(&mut self, object: DesignObject, index: usize) -> bool {
        if self.object(object.id).is_some() {
            return false;
        }
        self.objects.insert(index.min(self.objects.len()), object);
        self.layer_structure_revision += 1;
        true
    }

    pub fn insert_objects(&mut self, placements: &[ObjectPlacement]) {
        let mut placements = placements.to_vec();
        placements.sort_by_key(|placement| placement.index);
        for placement in placements {
            self.insert_object(placement.object, placement.index);
        }
    }

    fn placement(&self, id: ObjectId) -> Option<ObjectPlacement> {
        let index = self.objects.iter().position(|object| object.id == id)?;
        Some(ObjectPlacement {
            object: self.objects[index].clone(),
            index,
        })
    }

    pub fn remove_objects(&mut self, ids: &[ObjectId]) -> Vec<ObjectPlacement> {
        let removed: Vec<_> = self
            .objects
            .iter()
            .enumerate()
            .filter(|(_, object)| ids.contains(&object.id))
            .map(|(index, object)| ObjectPlacement {
                object: object.clone(),
                index,
            })
            .collect();
        self.objects.retain(|object| !ids.contains(&object.id));
        self.layer_structure_revision += removed.len() as u64;
        removed
    }

    pub fn duplicate_objects(&mut self, ids: &[ObjectId]) -> Vec<ObjectPlacement> {
        let originals: Vec<_> = self
            .objects
            .iter()
            .filter(|object| ids.contains(&object.id))
            .cloned()
            .collect();
        let mut duplicates = Vec::with_capacity(originals.len());
        for mut object in originals {
            object.id = self.allocate_id();
            object.spool_id = self.allocate_node_id();
            object.name = self.allocate_name(object.object_type);
            object.position = point(
                object.position.x + DUPLICATE_OFFSET,
                object.position.y + DUPLICATE_OFFSET,
            );

            let index = self.objects.len();
            self.insert_object(object.clone(), index);
            duplicates.push(ObjectPlacement { object, index });
        }
        duplicates
    }

    // Test-only reading of an object's paint, used to assert that a replayed
    // style entry restores exactly what it recorded. Production style edits go
    // through `DocumentCommand::style` -> `set_appearance`, which is the single
    // write path, so there is deliberately no second read path to keep in step.
    #[cfg(test)]
    fn style(&self, id: ObjectId) -> Option<ObjectStyle> {
        self.appearance(id).map(|appearance| appearance.style)
    }

    /// The full appearance of an object: paint plus its text styling.
    pub fn appearance(&self, id: ObjectId) -> Option<Appearance> {
        self.object(id).map(|object| Appearance {
            style: ObjectStyle {
                fill: object.fill,
                stroke: object.stroke,
                border_radius: object.border_radius,
                opacity: object.opacity,
            },
            text_color: object.text_color,
            font_size: object.font_size,
        })
    }

    // Test-only convenience over `set_appearance`, for the tests that set paint
    // without a history entry behind it. Production writes style only by
    // replaying `DocumentCommand::style`.
    #[cfg(test)]
    pub fn set_style(&mut self, id: ObjectId, style: ObjectStyle) -> bool {
        let Some(current) = self.appearance(id) else {
            return false;
        };
        self.set_appearance(id, Appearance { style, ..current })
    }

    /// Apply a whole appearance. The single write path for every style edit,
    /// so a replayed history entry restores exactly what it recorded.
    pub fn set_appearance(&mut self, id: ObjectId, appearance: Appearance) -> bool {
        let Some(object) = self.objects.iter_mut().find(|object| object.id == id) else {
            return false;
        };
        object.fill = appearance.style.fill;
        object.stroke = appearance.style.stroke;
        object.border_radius = appearance.style.border_radius;
        object.opacity = appearance.style.opacity;
        object.text_color = appearance.text_color;
        object.font_size = appearance.font_size;
        true
    }

    pub fn set_position(&mut self, id: ObjectId, position: Point<f32>) -> bool {
        let Some(object) = self.objects.iter_mut().find(|object| object.id == id) else {
            return false;
        };
        object.position = position;
        true
    }

    pub fn set_size(&mut self, id: ObjectId, object_size: Size<f32>) -> bool {
        let Some(object) = self.objects.iter_mut().find(|object| object.id == id) else {
            return false;
        };
        object.size = size(
            object_size.width.max(MIN_OBJECT_SIZE),
            object_size.height.max(MIN_OBJECT_SIZE),
        );
        true
    }

    pub fn geometry(&self, id: ObjectId) -> Option<Geometry> {
        self.object(id).map(DesignObject::geometry)
    }

    pub fn set_geometry(&mut self, id: ObjectId, geometry: Geometry) -> bool {
        if !self.set_position(id, geometry.position) {
            return false;
        }
        self.set_size(id, geometry.size)
    }

    pub fn hit_test(&self, world_point: Point<f32>) -> Option<ObjectId> {
        self.objects
            .iter()
            .rev()
            .find(|object| object.contains(world_point))
            .map(|object| object.id)
    }

    fn objects_in(&self, bounds: WorldRect) -> Vec<ObjectId> {
        self.objects
            .iter()
            .filter(|object| bounds.contains_object(object))
            .map(|object| object.id)
            .collect()
    }
}

fn frame(
    id: ObjectId,
    spool_id: NodeId,
    name: &str,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> DesignObject {
    DesignObject {
        id,
        spool_id,
        name: name.to_string(),
        position: point(x, y),
        size: size(width, height),
        object_type: ObjectType::Frame,
        text_content: None,
        text_color: None,
        font_size: None,
        fill: default_style(ObjectType::Frame).fill,
        stroke: default_style(ObjectType::Frame).stroke,
        border_radius: default_style(ObjectType::Frame).border_radius,
        opacity: 1.0,
    }
}

fn node_id(value: impl Into<String>) -> NodeId {
    NodeId::new(value).expect("generated Spool node IDs use the validated identifier alphabet")
}

fn edited_style(mut appearance: Appearance, edit: StyleEdit) -> Appearance {
    let style = &mut appearance.style;
    match edit {
        StyleEdit::TextColor(color) => appearance.text_color = color,
        StyleEdit::FontSize(size) => appearance.font_size = Some(size.max(1.0)),
        StyleEdit::BorderRadius(radius) => style.border_radius = radius.max(0.0),
        StyleEdit::Opacity(alpha) => style.opacity = alpha.clamp(0.0, 1.0),
        StyleEdit::Fill(color) => style.fill = color.map(|color| Fill { color }),
        StyleEdit::Stroke(color) => {
            style.stroke = color.map(|color| Stroke {
                color,
                width: style.stroke.map_or(1.0, |stroke| stroke.width),
            });
        }
        StyleEdit::StrokeWidth(width) => {
            let color = style
                .stroke
                .map_or(Color::from_rgb(theme::BORDER), |stroke| stroke.color);
            style.stroke = Some(Stroke { color, width });
        }
    }
    appearance
}

fn default_style(object_type: ObjectType) -> ObjectStyle {
    ObjectStyle {
        fill: match object_type {
            ObjectType::Frame => Some(Fill {
                color: Color::from_rgb(theme::PAPER),
            }),
            ObjectType::Rectangle | ObjectType::Ellipse => Some(Fill {
                color: Color::from_rgb(theme::SURFACE_RAISED),
            }),
            ObjectType::Text => None,
        },
        stroke: match object_type {
            ObjectType::Text => None,
            _ => Some(Stroke {
                color: Color::from_rgb(theme::BORDER),
                width: 1.0,
            }),
        },
        // Only a shape gets a default radius; giving every object one would
        // look like a decision the editor made on the author's behalf.
        border_radius: match object_type {
            ObjectType::Rectangle | ObjectType::Ellipse => 4.0,
            _ => 0.0,
        },
        opacity: 1.0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorldRect {
    min: Point<f32>,
    max: Point<f32>,
}

impl WorldRect {
    fn from_object(object: &DesignObject) -> Self {
        Self {
            min: object.position,
            max: point(
                object.position.x + object.size.width,
                object.position.y + object.size.height,
            ),
        }
    }

    /// The smallest box containing every object, if there is at least one.
    ///
    /// `None` for an empty document, because "fit nothing" has no honest
    /// answer: the caller falls back to the placeholder scene rather than
    /// inventing a box around the origin.
    fn around(objects: &[DesignObject]) -> Option<Self> {
        let mut bounds: Option<Self> = None;
        for object in objects {
            let object_bounds = Self::from_object(object);
            bounds = Some(match bounds {
                Some(current) => Self {
                    min: point(
                        current.min.x.min(object_bounds.min.x),
                        current.min.y.min(object_bounds.min.y),
                    ),
                    max: point(
                        current.max.x.max(object_bounds.max.x),
                        current.max.y.max(object_bounds.max.y),
                    ),
                },
                None => object_bounds,
            });
        }
        bounds
    }

    fn width(self) -> f32 {
        self.max.x - self.min.x
    }

    fn height(self) -> f32 {
        self.max.y - self.min.y
    }

    fn center(self) -> Point<f32> {
        point(
            (self.min.x + self.max.x) / 2.0,
            (self.min.y + self.max.y) / 2.0,
        )
    }

    fn from_points(start: Point<f32>, end: Point<f32>) -> Self {
        Self {
            min: point(start.x.min(end.x), start.y.min(end.y)),
            max: point(start.x.max(end.x), start.y.max(end.y)),
        }
    }

    fn contains_object(self, object: &DesignObject) -> bool {
        object.position.x >= self.min.x
            && object.position.y >= self.min.y
            && object.position.x + object.size.width <= self.max.x
            && object.position.y + object.size.height <= self.max.y
    }
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

/// The one selection in the editor.
///
/// A flat, ordered run of ids and nothing else. It is deliberately *not* a set
/// with no order: two users who selected the same objects by different routes —
/// click by click, or a marquee, or the layers panel — must end up holding the
/// identical selection, or every consumer that renders or iterates it would
/// have to tolerate a difference that means nothing.
///
/// The invariants are enforced by [`Hierarchy::normalize`] at each mutation, not
/// documented here and hoped for:
///
/// - no duplicates,
/// - no id that has an ancestor also in the selection (tldraw's rule),
/// - document order, not click order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    selected: Vec<ObjectId>,
}

impl Selection {
    /// A hierarchy that knows about exactly `ids`, in the order given.
    ///
    /// Not an *empty* one: [`Hierarchy::canonical`] projects against the
    /// document order, so a hierarchy with nothing in it would silently drop
    /// every id and make "no hierarchy" indistinguishable from "select nothing".
    /// That distinction is the whole reason these tests can stay about a flat
    /// list instead of each building a document.
    #[cfg(test)]
    fn flat_hierarchy(ids: &[ObjectId]) -> Hierarchy {
        let objects: Vec<DesignObject> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                frame(
                    *id,
                    node_id(format!("spool-flat-{index}")),
                    "flat",
                    0.0,
                    0.0,
                    1.0,
                    1.0,
                )
            })
            .collect();
        Hierarchy::build(
            &objects,
            &crate::source_document::LamineStructure::default(),
        )
    }

    /// A selection holding exactly `ids`, deduplicated, in the order given.
    ///
    /// Test-only, and deliberately able to build a selection no production path
    /// can: a selection carrying an ancestor and its descendant together. That
    /// shape is what normalization exists to prevent, so the only way to test
    /// the *cleanup* of it is to be able to write it.
    #[cfg(test)]
    fn flat(ids: Vec<ObjectId>) -> Self {
        let mut unique: Vec<ObjectId> = Vec::with_capacity(ids.len());
        for id in ids {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        Self { selected: unique }
    }

    #[cfg(test)]
    fn click_flat(&mut self, target: Option<ObjectId>, additive: bool) {
        let mut known: Vec<ObjectId> = self.selected.clone();
        if let Some(id) = target {
            if !known.contains(&id) {
                known.push(id);
            }
        }
        let hierarchy = Self::flat_hierarchy(&known);
        self.click(target, additive, &hierarchy);
    }
}

impl Selection {
    pub fn ids(&self) -> &[ObjectId] {
        &self.selected
    }

    pub fn contains(&self, id: ObjectId) -> bool {
        self.selected.contains(&id)
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// The sole selected object, or `None` for an empty or multi-object
    /// selection.
    ///
    /// Read by the surfaces that can only act on exactly one thing — the resize
    /// handles, the single-object readout — so "one and only one" is expressed
    /// once rather than as a length check at each call site.
    pub fn only(&self) -> Option<ObjectId> {
        match self.selected.as_slice() {
            [id] => Some(*id),
            _ => None,
        }
    }

    fn click(&mut self, target: Option<ObjectId>, additive: bool, hierarchy: &Hierarchy) {
        let Some(id) = target else {
            self.selected.clear();
            return;
        };
        if !additive {
            self.replace(vec![id]);
            return;
        }
        // Toggling off has to come before the rebuild: a `Hierarchy` this does
        // not know cannot filter, so it cannot be the thing that decides
        // whether an id was there to remove.
        if let Some(index) = self.selected.iter().position(|selected| *selected == id) {
            self.selected.remove(index);
            return;
        }
        self.add_all([id], hierarchy);
    }

    fn replace(&mut self, ids: Vec<ObjectId>) {
        self.selected = ids;
    }

    /// Replace the selection with `ids`, normalized.
    ///
    /// Every path that *sets* a selection of more than one id goes through here
    /// so the invariants cannot leak from a caller that forgot them.
    fn replace_normalized(&mut self, ids: Vec<ObjectId>, hierarchy: &Hierarchy) {
        self.selected = hierarchy.normalize(&ids);
    }

    fn add_all(&mut self, ids: impl IntoIterator<Item = ObjectId>, hierarchy: &Hierarchy) {
        let mut next = self.selected.clone();
        for id in ids {
            if !next.contains(&id) {
                next.push(id);
            }
        }
        self.selected = hierarchy.normalize(&next);
    }
}

#[derive(Clone, Copy)]
struct CanvasHitbox {
    id: HitboxId,
    origin: Point<Pixels>,
}

#[derive(Clone, Copy)]
struct PanGesture {
    button: MouseButton,
    pointer_start: Point<f32>,
    offset_start: Point<f32>,
}

#[derive(Clone, Copy)]
pub struct ObjectSnapshot {
    pub id: ObjectId,
    pub geometry: ObjectGeometry,
}

#[derive(Clone, Copy)]
enum ClickSelection {
    SelectOnly(ObjectId),
    Toggle(ObjectId),
}

struct MoveGesture {
    pointer_start_screen: Point<f32>,
    pointer_start_world: Point<f32>,
    objects: Vec<ObjectSnapshot>,
    selected_ids: Vec<ObjectId>,
    click_selection: ClickSelection,
    /// `⌘`/`Ctrl` held at press time: place this object freely, ignoring
    /// alignment for the whole gesture.
    ///
    /// Read once, when the gesture starts, rather than sampled per pointer
    /// movement. Figma's `⌘`-drag is one continuous "not this time", and a
    /// magnet that switches off halfway through a drag is the single most
    /// disorienting thing a snapping implementation can do.
    suspend_snap: bool,
    /// `⌥`/`Alt` held at press time: this drag moves *copies*, leaving the
    /// originals where they were.
    ///
    /// Latched at press rather than sampled live, unlike `⇧` on a resize. The
    /// asymmetry is deliberate and follows from what each modifier does:
    /// constraining a resize or suspending a snap is arithmetic, and arithmetic
    /// can be re-derived from the gesture's start state at any moment — which is
    /// exactly what [`Interaction::restore`] does. Creating objects is not
    /// arithmetic; it is the one part of this gesture that cannot be undone by
    /// recomputing a position, so the decision to duplicate has to be made once,
    /// before anything is created.
    duplicate: bool,
    /// The ids this gesture created, so a cancelled gesture can take them back
    /// out.
    ///
    /// Empty unless `duplicate` was set. Escape has to leave the document
    /// exactly as it was, and "restore the geometry" is not enough once the
    /// gesture has also added objects that were never there.
    duplicates: Vec<ObjectId>,
    /// The placements that created them, for the commit.
    ///
    /// Held rather than recomputed so the insert half of the compound is
    /// exactly the insert that already ran. Re-deriving it at commit time would
    /// allocate *new* ids and the history entry would describe objects that do
    /// not exist.
    placements: Vec<ObjectPlacement>,
}

impl MoveGesture {
    /// The one semantic operation this whole gesture commits.
    ///
    /// A plain drag is a single geometry command. An `⌥`-drag is an insert *and*
    /// a geometry change, and [`SemanticOperation::Compound`] is what makes those
    /// one undoable unit — the primitive the foundation pass left in place for
    /// exactly this gesture.
    ///
    /// Order matters and is not arbitrary: undo replays members in reverse, so
    /// the geometry is rewound before the objects it moved are removed. Redoing
    /// in order re-creates the copies at their duplicated position and then moves
    /// them, which lands on the same result whether the gesture was completed by
    /// a mouse-up or replayed from history.
    fn operation(&self, geometry: DocumentCommand) -> SemanticOperation {
        // Normalized before it is wrapped, so a drag that ended where it started
        // contributes no geometry member and the whole compound is correctly seen
        // as the no-op it is.
        let geometry = SemanticOperation::Runtime(geometry.normalized());
        if self.duplicates.is_empty() {
            return geometry;
        }
        // Through the constructor rather than the variant, so there is one way to
        // build a compound and flattening is not something a caller can forget.
        SemanticOperation::compound(vec![
            SemanticOperation::Runtime(DocumentCommand::insert(self.placements.clone())),
            geometry,
        ])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl ResizeHandle {
    const ALL: [Self; 8] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Right,
        Self::BottomRight,
        Self::Bottom,
        Self::BottomLeft,
        Self::Left,
    ];

    fn moves_left(self) -> bool {
        matches!(self, Self::TopLeft | Self::Left | Self::BottomLeft)
    }

    fn moves_right(self) -> bool {
        matches!(self, Self::TopRight | Self::Right | Self::BottomRight)
    }

    fn moves_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::Top | Self::TopRight)
    }

    fn moves_bottom(self) -> bool {
        matches!(self, Self::BottomLeft | Self::Bottom | Self::BottomRight)
    }

    /// Which edges of the box this handle moves.
    ///
    /// A handle never moves both edges on one axis, and that is what lets a
    /// resize snap without a constraint solver: each moving edge is tested
    /// against the candidate lines on its own axis.
    fn moved_edges(self) -> snap::MovedEdges {
        snap::MovedEdges {
            left: self.moves_left(),
            right: self.moves_right(),
            top: self.moves_top(),
            bottom: self.moves_bottom(),
        }
    }

    /// Where this handle sits, in screen pixels.
    ///
    /// Takes the *transform box* rather than a drawn object so that one
    /// function places the handles for a single object and for the union box of
    /// a multi-selection. The handle model does not change with the number of
    /// objects being manipulated, and it must not: eight handles on the union
    /// box is the same eight handles.
    fn screen_position(self, camera: Camera, box_geometry: ObjectGeometry) -> Point<f32> {
        let origin = camera.world_to_screen(box_geometry.position);
        let right = origin.x + box_geometry.size.width * camera.zoom;
        let bottom = origin.y + box_geometry.size.height * camera.zoom;
        match self {
            Self::TopLeft => point(origin.x, origin.y),
            Self::Top => point((origin.x + right) / 2.0, origin.y),
            Self::TopRight => point(right, origin.y),
            Self::Right => point(right, (origin.y + bottom) / 2.0),
            Self::BottomRight => point(right, bottom),
            Self::Bottom => point((origin.x + right) / 2.0, bottom),
            Self::BottomLeft => point(origin.x, bottom),
            Self::Left => point(origin.x, (origin.y + bottom) / 2.0),
        }
    }
}

/// A resize of one object or of a whole selection.
///
/// The gesture manipulates a *transform box* rather than an object. For a
/// single selection the box is that object's geometry, so nothing about the
/// single-object case is special-cased; for a multi-selection it is the union of
/// the members' boxes, which is the rectangle every reference product treats as
/// the thing the handles belong to.
///
/// [DOCUMENTED] tldraw exposes `getSelectionPageBounds()` — "smallest AABB
/// containing all selected shapes" — as the basis for selection transforms, and
/// is the only product in the corpus that documents a selection-bounds model at
/// all. Adopting it here is following that model, not inventing one.
struct ResizeGesture {
    pointer_start_screen: Point<f32>,
    pointer_start_world: Point<f32>,
    /// Every object the gesture owns, as it was when the gesture began.
    members: Vec<ObjectSnapshot>,
    /// The union of `members` at press time: the box the handles were drawn on
    /// and the box every member's new geometry is measured from.
    ///
    /// A gesture-local value, not document state. It is derived from `members`
    /// and is discarded with the gesture; it is never serialized and never
    /// enters history.
    bounds: ObjectGeometry,
    handle: ResizeHandle,
    /// `⌘`/`Ctrl` held at press time, meaning the same thing for a resize as it
    /// does for a move: place this edge freely for the whole gesture.
    ///
    /// Read once, when the gesture starts, for the same reason `MoveGesture`
    /// does — a magnet that switches off halfway through is disorienting, and
    /// the user who wants to place something freely says so before they press.
    suspend_snap: bool,
}

#[derive(Clone, Copy)]
struct CreateGesture {
    object_type: ObjectType,
    pointer_start_screen: Point<f32>,
    pointer_start_world: Point<f32>,
    current_world: Point<f32>,
    /// Whether the pointer has moved at all since the press.
    ///
    /// Not the same question as whether the drag threshold was crossed, and the
    /// difference is the whole of click creation. Sub-threshold movement is still
    /// movement: the user dragged, just not far enough to mean anything by drag,
    /// and it leaves no object behind. A press and release with no movement at all
    /// is a click, and gets the tool's default size.
    ///
    /// `current_world` cannot answer this, because below the threshold it is never
    /// updated — so a 3px jiggle and a perfect click are indistinguishable there.
    moved: bool,
}

#[derive(Clone, Copy)]
struct CreationPreview {
    object_type: ObjectType,
    geometry: Geometry,
}

enum Interaction {
    None,
    PotentialMove(MoveGesture),
    Moving(MoveGesture),
    PotentialResize(ResizeGesture),
    Resizing(ResizeGesture),
    PotentialCreate(CreateGesture),
    Creating(CreateGesture),
}

impl Interaction {
    fn is_active(&self) -> bool {
        !matches!(self, Self::None)
    }

    fn restore(&self, document: &mut Document) {
        match self {
            Self::PotentialMove(gesture) | Self::Moving(gesture) => {
                for object in &gesture.objects {
                    document.set_geometry(object.id, object.geometry);
                }
                // An `⌥`-drag also *created* things. Restoring the geometry of
                // objects that were never in the document would leave the
                // document with objects the user never asked for, so the copies
                // go back out. No history entry either way: a cancelled gesture
                // is not an edit.
                if !gesture.duplicates.is_empty() {
                    document.remove_objects(&gesture.duplicates);
                }
            }
            Self::PotentialResize(gesture) | Self::Resizing(gesture) => {
                for member in &gesture.members {
                    document.set_geometry(member.id, member.geometry);
                }
            }
            Self::None | Self::PotentialCreate(_) | Self::Creating(_) => {}
        }
    }

    fn preview(&self) -> Option<CreationPreview> {
        let Self::Creating(gesture) = self else {
            return None;
        };
        Some(CreationPreview {
            object_type: gesture.object_type,
            geometry: creation_geometry(gesture.pointer_start_world, gesture.current_world),
        })
    }
}

/// The size a click with this tool makes.
///
/// A named answer per tool rather than one number everywhere, because Text is
/// genuinely a different shape: it auto-sizes, so its click box is only a starting
/// box. Text's value is preserved from what click-creation already used.
fn default_creation_size(object_type: ObjectType) -> Size<f32> {
    match object_type {
        ObjectType::Text => size(DEFAULT_TEXT_WIDTH, DEFAULT_TEXT_HEIGHT),
        ObjectType::Frame | ObjectType::Rectangle | ObjectType::Ellipse => {
            size(DEFAULT_CREATION_SIZE, DEFAULT_CREATION_SIZE)
        }
    }
}

fn creation_geometry(start: Point<f32>, current: Point<f32>) -> Geometry {
    let bounds = WorldRect::from_points(start, current);
    Geometry {
        position: bounds.min,
        size: size(
            (bounds.max.x - bounds.min.x).max(MIN_OBJECT_SIZE),
            (bounds.max.y - bounds.min.y).max(MIN_OBJECT_SIZE),
        ),
    }
}

/// Does this gesture ignore alignment for its whole duration?
///
/// `⌘` on macOS, `Ctrl` everywhere else — the same key that means "precise" to
/// every product in the corpus. Read once when the gesture starts rather than
/// sampled per movement: a magnet that switches off halfway through a drag is
/// the most disorienting thing a snapping implementation can do, and the user
/// who wants to place something freely says so before they press.
///
/// Split out from the gesture literal so the rule can be tested without a
/// window. It is one boolean and one `||`, but it is a *convention*, and
/// conventions are exactly the things that rot silently.
fn suspends_snap(modifiers: gpui::Modifiers) -> bool {
    modifiers.platform || modifiers.control
}

/// Is this the deep-select modifier?
///
/// Figma, tldraw, Affinity and Canva all use `Cmd`/`Ctrl`-click to reach past a
/// container and act on the object actually under the pointer. Named
/// separately from [`suspends_snap`] because the two happen to be the same
/// modifier today and are not the same intent: one chooses *what* to select, the
/// other switches *snapping* off for a drag. Sharing the name would make the
/// day one of them changes look like a change to the other.
fn deep_select(modifiers: gpui::Modifiers) -> bool {
    modifiers.platform || modifiers.control
}

/// Which axis a `⇧`-constrained drag is allowed to move on.
///
/// `⇧` constrains a move to one axis, on whichever the pointer has travelled
/// furthest. Figma, tldraw and Affinity all resolve it the same way, and the
/// rule is not "horizontal or vertical" but "the one you clearly meant" — so
/// the dominant component that survives, not a fixed preference.
///
/// Snapping is told the answer rather than left to infer it from a zero
/// component. It has to be: a snap is a correction on an axis the pointer is
/// not otherwise moving, so a magnet left free would undo the constraint the
/// user asked for a moment earlier. A zero is not the same thing as a lock —
/// a purely horizontal drag should still click into line vertically — so the
/// lock has to be stated.
fn dragged_axis(raw: Point<f32>, constrain: bool) -> snap::AxisLock {
    if !constrain {
        return snap::AxisLock::Free;
    }
    if raw.x.abs() >= raw.y.abs() {
        snap::AxisLock::X
    } else {
        snap::AxisLock::Y
    }
}

fn drag_threshold_crossed(start: Point<f32>, current: Point<f32>) -> bool {
    let dx = current.x - start.x;
    let dy = current.y - start.y;
    dx * dx + dy * dy >= DRAG_THRESHOLD * DRAG_THRESHOLD
}

fn movement_delta(camera: Camera, start_world: Point<f32>, screen: Point<f32>) -> Point<f32> {
    let current_world = camera.screen_to_world(screen);
    point(
        current_world.x - start_world.x,
        current_world.y - start_world.y,
    )
}

fn apply_move(document: &mut Document, objects: &[ObjectSnapshot], delta: Point<f32>) {
    for object in objects {
        document.set_position(
            object.id,
            point(
                object.geometry.position.x + delta.x,
                object.geometry.position.y + delta.y,
            ),
        );
    }
}

/// The transform box of a set of objects: the union of their boxes.
///
/// `None` for an empty set, for the same reason [`snap::bounds_of`] is: "the
/// bounds of nothing" has no honest answer, and inventing a box around the
/// origin would put a phantom selection outline at the top-left of the canvas.
///
/// [DOCUMENTED] This is the rectangle a selection is measured by in tldraw,
/// whose `getSelectionPageBounds()` returns the smallest AABB containing all
/// selected shapes. It is also already the rectangle Spool snaps a multi-object
/// move against, so reusing it means the move and the resize agree on what "the
/// selection" is — two different rectangles for the same selection would be a
/// bug waiting to happen.
fn transform_bounds(objects: &[ObjectSnapshot]) -> Option<ObjectGeometry> {
    let rects: Vec<snap::Rect> = objects.iter().map(|o| snap_rect(o.geometry)).collect();
    snap::bounds_of(&rects).map(|rect| ObjectGeometry {
        position: point(rect.x, rect.y),
        size: size(rect.width, rect.height),
    })
}

/// [`transform_bounds`] for a list of ids, for the surfaces that hold ids rather
/// than snapshots — the renderer, which must not mutate anything to draw.
fn transform_bounds_of_ids(document: &Document, ids: &[ObjectId]) -> Option<ObjectGeometry> {
    let snapshots: Vec<ObjectSnapshot> = ids
        .iter()
        .filter_map(|id| {
            document.object(*id).map(|object| ObjectSnapshot {
                id: *id,
                geometry: object.geometry(),
            })
        })
        .collect();
    transform_bounds(&snapshots)
}

/// Map every member of a selection from the transform box it started in to the
/// box the handle produced.
///
/// This is the whole of multi-selection resize: the handle moves one rectangle,
/// and every member is re-expressed inside the new one.
///
/// ```text
/// scale_x = new.width  / old.width
/// scale_y = new.height / old.height
///
/// new.x      = new.x + (old.x - old_bounds.x) * scale_x
/// new.width  = old.width  * scale_x          (and likewise for y)
/// ```
///
/// A member keeps its *relative* position and its *proportion*, so a two-object
/// selection dragged from one corner keeps the gap between the objects in the
/// same proportion to the selection as before.
///
/// [INFERRED — SPOOL DECISION] No product in the research corpus documents what
/// happens to the children of a multi-selection resize. tldraw documents a
/// selection-bounds *model* but not a child-scaling rule; Figma exposes a
/// separate Scale tool for proportional scaling and delegates resizing to its
/// constraint system, which the corpus never describes. Proportional scaling is
/// therefore Spool's choice, chosen because it is the only rule that keeps a
/// selection's internal arrangement recognisable, and because it degrades to
/// exactly the single-object behaviour when the selection has one member.
///
/// Two degenerate cases are handled rather than left to float arithmetic:
///
/// - **A zero-extent axis** (two objects sharing an x, or a zero-width object)
///   has no scale to compute. Such an axis is left alone — positions and sizes
///   pass through unchanged — because the alternative is a division by zero
///   turning a drag into infinity.
/// - **Minimum size** is *not* re-imposed per member here. `Document::set_size`
///   already floors every object at [`MIN_OBJECT_SIZE`] on the way in, exactly as
///   it does for a single-object resize, so the existing policy applies to
///   members for free and inventing a second, different floor here would be the
///   thing that made the two paths disagree.
fn scaled_members(
    bounds: ObjectGeometry,
    resized: ObjectGeometry,
    members: &[ObjectSnapshot],
) -> Vec<(ObjectId, ObjectGeometry)> {
    // A single member *is* the transform box, so the resize already computed its
    // answer and scaling it by `resized / bounds` would only re-derive it in
    // floating point — turning an exact 480.0 into 480.00003 and making an
    // unchanged value look changed to `DocumentCommand::normalized`. One object
    // in means exactly what it always did.
    if let [only] = members {
        return vec![(only.id, resized)];
    }
    // A zero or negative extent is not a scale; treat it as "unchanged".
    let scale = |before: f32, after: f32| {
        if before > f32::EPSILON {
            after / before
        } else {
            1.0
        }
    };
    let scale_x = scale(bounds.size.width, resized.size.width);
    let scale_y = scale(bounds.size.height, resized.size.height);
    members
        .iter()
        .map(|member| {
            let relative_x = member.geometry.position.x - bounds.position.x;
            let relative_y = member.geometry.position.y - bounds.position.y;
            (
                member.id,
                ObjectGeometry {
                    position: point(
                        resized.position.x + relative_x * scale_x,
                        resized.position.y + relative_y * scale_y,
                    ),
                    size: size(
                        member.geometry.size.width * scale_x,
                        member.geometry.size.height * scale_y,
                    ),
                },
            )
        })
        .collect()
}

fn resized_geometry(
    start: ObjectGeometry,
    handle: ResizeHandle,
    delta: Point<f32>,
    proportional: bool,
    from_center: bool,
) -> ObjectGeometry {
    let left = start.position.x;
    let top = start.position.y;
    let right = left + start.size.width;
    let bottom = top + start.size.height;

    // `⇧` preserves the aspect ratio. The pointer's dominant axis decides the
    // scale, which is the same rule the move uses for its dominant axis — one
    // gesture grammar for both, rather than `⇧` meaning something different
    // depending on which handle the pointer happened to grab.
    let delta = if proportional {
        // The pointer picks one axis; that axis's *resulting size* is what the
        // user asked for, and the other axis follows by scale. Scaling the drag
        // vector itself instead would compound the error, because the vector is
        // what already moved the box.
        let width = start.size.width.max(f32::EPSILON);
        let height = start.size.height.max(f32::EPSILON);
        let (driven, other) = if delta.x.abs() >= delta.y.abs() {
            (width, height)
        } else {
            (height, width)
        };
        let requested = driven
            + if delta.x.abs() >= delta.y.abs() {
                delta.x
            } else {
                delta.y
            };
        let scale = (requested / driven).max(MIN_OBJECT_SIZE / other.max(f32::EPSILON));
        point((width * scale) - width, (height * scale) - height)
    } else {
        delta
    };

    // Which edges this drag is allowed to move.
    //
    // Normally a handle moves the one edge it is on and leaves the opposite edge
    // where it is — that is what "anchored to the opposite edge" means.
    //
    // `⌥` resizes about the centre. The edge under the pointer follows the
    // pointer; the edge *opposite* it moves the same distance in the *other*
    // direction, which is what makes the midpoint stay put. Moving both edges the
    // same way would just translate and grow the box and leave the centre drifting
    // off the pointer, which is not what the gesture claims to do.
    //
    // Only an axis the handle actually touches is mirrored. A `Top` handle drags
    // the top edge, so `⌥` on it must widen the box vertically and leave the
    // columns alone even if the pointer wandered sideways on the way there.
    //
    // [DOCUMENTED] Resize-from-centre on `⌥`/`Alt` is Figma's and tldraw's
    // binding. Affinity uses `⌘` for the same thing, and Canva documents none.
    // [SPOOL DECISION] Spool uses `⌥`, matching the majority and leaving `⌘`
    // free for the one thing it already means here — suspending snapping.
    let mirror_x = from_center && (handle.moves_left() || handle.moves_right());
    let mirror_y = from_center && (handle.moves_top() || handle.moves_bottom());

    // The clamps are unchanged from the anchored case and mean the same thing: an
    // edge may not pass the opposite one by less than `MIN_OBJECT_SIZE`, so a box
    // never inverts and never flips.
    let new_left = if handle.moves_left() {
        (left + delta.x).min(right - MIN_OBJECT_SIZE)
    } else if mirror_x {
        (left - delta.x).min(right - MIN_OBJECT_SIZE)
    } else {
        left
    };
    let new_right = if handle.moves_right() {
        (right + delta.x).max(new_left + MIN_OBJECT_SIZE)
    } else if mirror_x {
        (right - delta.x).max(new_left + MIN_OBJECT_SIZE)
    } else {
        right
    };
    let new_top = if handle.moves_top() {
        (top + delta.y).min(bottom - MIN_OBJECT_SIZE)
    } else if mirror_y {
        (top - delta.y).min(bottom - MIN_OBJECT_SIZE)
    } else {
        top
    };
    let new_bottom = if handle.moves_bottom() {
        (bottom + delta.y).max(new_top + MIN_OBJECT_SIZE)
    } else if mirror_y {
        (bottom - delta.y).max(new_top + MIN_OBJECT_SIZE)
    } else {
        bottom
    };

    ObjectGeometry {
        position: point(new_left, new_top),
        size: size(new_right - new_left, new_bottom - new_top),
    }
}

/// The rectangle a geometry is, for the snapping engine to work on.
///
/// One conversion so that every caller describes the same geometry the same way.
/// `Geometry` is the canvas's own pair of position and size; `snap::Rect` is
/// the snapper's, and neither should have to know about the other's shape.
fn snap_rect(geometry: ObjectGeometry) -> snap::Rect {
    snap::Rect::new(
        geometry.position.x,
        geometry.position.y,
        geometry.size.width,
        geometry.size.height,
    )
}

fn geometry_command(document: &Document, snapshots: &[ObjectSnapshot]) -> DocumentCommand {
    let changes = snapshots
        .iter()
        .filter_map(|snapshot| {
            let after = document.geometry(snapshot.id)?;
            (snapshot.geometry != after).then_some(GeometryChange {
                id: snapshot.id,
                before: snapshot.geometry,
                after,
            })
        })
        .collect();
    DocumentCommand::geometry(changes)
}

#[derive(Clone)]
struct MarqueeGesture {
    start: Point<f32>,
    current: Point<f32>,
    additive: bool,
    initial_selection: Vec<ObjectId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TextEditState {
    id: ObjectId,
    original_text: String,
    editing_text: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    pointer_anchor: Option<usize>,
}

/// The runtime appearance an object had the moment its project was opened.
///
/// Used only to decide what a save needs to write. Keeping it derived state on
/// the view — not in the persistent document — is what lets save answer "did the
/// user change this?" without recording anything new in the document.
#[derive(Clone, Debug, PartialEq)]
struct SourceSnapshot {
    position: Point<f32>,
    /// Where the object sat relative to its parent's origin when it was opened.
    ///
    /// The baseline a flow element's move is measured from. A flow box has no
    /// authored position of its own — the flow decides it — so the editor's only
    /// honest way to express "the user moved this" is as a change from the
    /// position the author wrote, not as an absolute coordinate.
    parent_relative: Point<f32>,
    size: Size<f32>,
    text: Option<String>,
    /// How the object looked when it was opened, so save can tell a user edit
    /// from an authored value. The same shape undo restores, which keeps the two
    /// paths from drifting apart.
    appearance: Appearance,
}

/// Component-wise difference between two points.
///
/// Used to express a world position relative to a containing block, which is
/// the coordinate system an authored `left`/`top` lives in.
fn sub_point(a: Point<f32>, b: Point<f32>) -> Point<f32> {
    point(a.x - b.x, a.y - b.y)
}

/// Format a colour the way source spells it.
///
/// Written back into CSS as `#rrggbb`. Alpha is not supported by the style
/// module, so a colour with one is left to the reader rather than written as a
/// value that means something narrower than what the editor holds.
fn css_color(color: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue)
}

pub struct CanvasView {
    camera: Camera,
    /// The single mutation and history boundary for the live editor.
    ///
    /// This used to be two fields here: `document: Document` and
    /// `history: History`, which meant the canvas owned a runtime document
    /// and a history stack beside it while the persistent document and the
    /// semantic operation layer sat unused. Now every committed canvas edit
    /// goes through `EditSession::execute`, so the live path is
    /// canvas -> EditSession -> SemanticOperation -> SemanticHistory.
    ///
    /// No project is loaded yet, so `session.document` is empty metadata.
    /// That is the honest state: there is no `lamine.yaml` behind the starter
    /// scene. When project opening lands, that document is populated here and
    /// metadata operations become reachable from the same stack.
    session: EditSession,
    selection: Selection,
    pan: Option<PanGesture>,
    interaction: Interaction,
    marquee: Option<MarqueeGesture>,
    tool: Tool,
    space_held: bool,
    hitbox: Rc<Cell<Option<CanvasHitbox>>>,
    focus_handle: Option<FocusHandle>,
    text_edit: Option<TextEditState>,
    #[cfg(debug_assertions)]
    workload_started: bool,
    #[cfg(debug_assertions)]
    workload_running: bool,
    /// Where the open project lives on disk, if one is open.
    ///
    /// Kept beside the document rather than inside it: the persistent document
    /// describes content, not where it came from.
    project_root: Option<std::path::PathBuf>,
    /// What each loaded object looked like when the project was opened.
    ///
    /// Save diffs against this. Without it, saving an untouched project would
    /// rewrite every element with absolute geometry — the file would change on
    /// disk for a session in which the user did nothing, which is exactly the
    /// behaviour source-preserving save exists to prevent.
    source_snapshot: BTreeMap<NodeId, SourceSnapshot>,
    /// Every node this session has authored an element for, and the file it
    /// lives in.
    ///
    /// Session state on the view, not document state, and for the same reason the
    /// snapshot is. It exists because a node that has left the document cannot be
    /// asked which file its element was in — yet the save still has to take that
    /// element back out.
    ///
    /// Derived from the structure rather than from a log of deletions, on
    /// purpose. Anything recorded here and absent from the structure now is a node
    /// to un-author, so undoing a creation un-authors it too, for free — a
    /// deletion log would have to be rewound by hand and would not be. A creation
    /// adds itself here when the save authors it.
    authored_elements: BTreeMap<NodeId, String>,
    /// `⇧` is held, so this movement is constrained to one axis.
    ///
    /// Sampled per pointer movement rather than at press time, which is the
    /// opposite of the snap suspension on purpose: a user presses `⇧` halfway
    /// through a drag to straighten it, and expects it to take effect there and
    /// then. It is reset the moment the gesture ends.
    constrain_drag: bool,
    /// `⌥`/`Alt` held during a *resize*: grow and shrink the box about its
    /// centre instead of moving one edge.
    ///
    /// Runtime gesture state, sampled per movement for the same reason
    /// `constrain_drag` is, and cleared with it. It has no meaning during a move
    /// — there, `⌥` means "duplicate" — which is why the two live on the active
    /// gesture rather than in one global "alt is down" flag: the same physical
    /// key means different things depending on what the pointer is already
    /// doing, and a single global flag could not tell them apart.
    center_drag: bool,

    /// The object under the pointer, when the pointer is over one.
    ///
    /// Runtime only, like everything else about hover: not in the document, not
    /// in history, not restored by undo. Cleared whenever the pointer stops
    /// being over an object, so a stale outline can never outlive the cursor.
    hovered: Option<ObjectId>,
    /// Alignment lines currently holding, one per axis at most.
    ///
    /// Pure runtime feedback: they are not in the document, not in history, and
    /// they are cleared the moment nothing is moving. A snap the user cannot
    /// see is the same as an object that jumped for no reason, so the guide is
    /// part of the interaction, not a decoration added afterwards.
    snap_guides: Vec<snap::Guide>,
    /// Debug-only: true pre-gesture positions for the runtime history probe.
    #[cfg(debug_assertions)]
    drag_start_positions: Vec<(f32, f32)>,
}

/// Turn a changed appearance into one source edit per property that moved.
///
/// Only what actually changed becomes an edit. A change the editor cannot
/// express in the supported CSS subset — removing a fill, clearing an authored
/// text colour — produces no edit at all rather than a guessed one.
fn style_edits(
    node: &NodeId,
    before: &Appearance,
    after: &Appearance,
) -> Vec<crate::project_save::SourceEdit> {
    let mut edits = Vec::new();
    let mut push = |property: &str, value: String| {
        edits.push(crate::project_save::SourceEdit::Style {
            node: node.clone(),
            property: property.to_owned(),
            value,
        });
    };

    if before.style.fill != after.style.fill {
        if let Some(fill) = after.style.fill {
            push("background-color", css_color(fill.color));
        }
    }
    if before.style.stroke != after.style.stroke {
        if let Some(stroke) = after.style.stroke {
            push(
                "border",
                format!(
                    "{}px {}",
                    crate::project_save::css_length(stroke.width),
                    css_color(stroke.color)
                ),
            );
        }
    }
    if before.text_color != after.text_color {
        if let Some(color) = after.text_color {
            push("color", css_color(color));
        }
    }
    if before.font_size != after.font_size {
        if let Some(size) = after.font_size {
            push(
                "font-size",
                format!("{}px", crate::project_save::css_length(size)),
            );
        }
    }
    if before.style.border_radius != after.style.border_radius {
        push(
            "border-radius",
            format!(
                "{}px",
                crate::project_save::css_length(after.style.border_radius)
            ),
        );
    }
    if before.style.opacity != after.style.opacity {
        push(
            "opacity",
            crate::project_save::css_length(after.style.opacity),
        );
    }
    edits
}

impl CanvasView {
    pub(crate) fn workload_controls_input(&self) -> bool {
        #[cfg(debug_assertions)]
        {
            self.workload_running
        }
        #[cfg(not(debug_assertions))]
        {
            false
        }
    }

    pub fn new() -> Self {
        Self {
            camera: Camera::default(),
            session: EditSession::new(PersistentDocument::default(), Document::default()),
            selection: Selection::default(),
            pan: None,
            interaction: Interaction::None,
            marquee: None,
            tool: Tool::Select,
            space_held: false,
            hitbox: Rc::new(Cell::new(None)),
            focus_handle: None,
            text_edit: None,
            #[cfg(debug_assertions)]
            workload_started: false,
            #[cfg(debug_assertions)]
            workload_running: false,
            project_root: None,
            source_snapshot: BTreeMap::new(),
            authored_elements: BTreeMap::new(),
            constrain_drag: false,
            center_drag: false,
            hovered: None,
            snap_guides: Vec::new(),
            #[cfg(debug_assertions)]
            drag_start_positions: Vec::new(),
        }
    }

    pub fn new_with_context(cx: &mut Context<Self>) -> Self {
        let mut view = Self::new();
        view.focus_handle = Some(cx.focus_handle());
        #[cfg(debug_assertions)]
        view.install_workload_fixture();
        view
    }

    /// Replace this canvas's contents with a source-backed project.
    ///
    /// This is the seam between opening a project and editing it. The canvas
    /// takes the already-derived runtime document plus the canonical persistent
    /// document and does no parsing itself, so there is exactly one place in
    /// the editor that knows how a project becomes a scene.
    ///
    /// History is reset deliberately: the new document has no relationship to
    /// whatever was open before, and replaying an old operation against it
    /// would be meaningless rather than merely stale.
    pub fn load_project(&mut self, loaded: crate::project_open::LoadedProject) {
        self.project_root = Some(loaded.root.clone());
        self.session = EditSession::new(loaded.document, loaded.runtime);
        // Snapshot the runtime state before any edit, so save can tell an
        // authored value from a change the user made in this session. Taken
        // after the session exists because the snapshot needs each object's
        // parent origin, which is resolved through the runtime.
        self.source_snapshot = self.opened_state();
        // Node identities come back with the project, so the allocator has to
        // start past all of them. Without this, creating an object in a reopened
        // project would hand it an identity the project already uses, and the
        // next load would bind two objects to one element.
        self.session.runtime.next_node_id = next_node_id_after(&self.session.document);
        // What the project was opened with. A save compares this against the
        // structure to decide what has to be taken back out of the source.
        self.authored_elements = self
            .session
            .document
            .structure
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.source.file.clone()))
            .collect();
        // Names are unique across the whole document, not just across the canvas,
        // so the counters have to start past whatever the project already calls its
        // objects. Without this a reopened project hands out `Rectangle 1` again,
        // the structural change is refused for a duplicate name, and the creation
        // silently does nothing.
        seed_object_names(
            &mut self.session.runtime,
            &self.session.document.structure.nodes,
        );
        // A freshly loaded document has no selection, and keeping stale ids
        // would let hit-testing and layers refer to objects that no longer
        // exist.
        self.selection = Selection::default();
        self.interaction = Interaction::None;
        self.marquee = None;
        self.text_edit = None;
        // Projected objects are laid out by the projection's own placeholder
        // rule, so fitting the camera is what actually brings them on screen —
        // and it has to fit *these* objects. `Camera::fit` frames a fixed
        // placeholder box, which is right for the empty starter scene and wrong
        // for a loaded project of any other size, so a small project opened
        // off to one side and a large one overflowed the window.
        self.fit_canvas();
    }

    /// The runtime state the project was opened with, for every managed object.
    ///
    /// Derived state on the view, never in the document: this is how save
    /// answers "did the user change this?" without recording anything new
    /// anywhere.
    fn opened_state(&self) -> BTreeMap<NodeId, SourceSnapshot> {
        self.session
            .runtime
            .objects()
            .iter()
            .map(|object| {
                let parent_origin = self.parent_origin(&object.spool_id);
                (
                    object.spool_id.clone(),
                    SourceSnapshot {
                        position: object.position,
                        parent_relative: sub_point(object.position, parent_origin),
                        size: object.size,
                        text: object.text_content.clone(),
                        appearance: Appearance {
                            style: ObjectStyle {
                                fill: object.fill,
                                stroke: object.stroke,
                                border_radius: object.border_radius,
                                opacity: object.opacity,
                            },
                            text_color: object.text_color,
                            font_size: object.font_size,
                        },
                    },
                )
            })
            .collect()
    }

    /// Where a node's containing block starts, in world coordinates.
    ///
    /// Zero for a root, because a root's containing block is the page. Resolved
    /// through the persistent document rather than the runtime object list so
    /// it is the authored hierarchy that decides, not the order things happen
    /// to be drawn in.
    /// The element to author for a created object.
    ///
    /// Inline, because a created element has no class the author's stylesheet
    /// could own: inventing a rule inside a file the author wrote would be
    /// editing their CSS to describe something they never asked for. Every
    /// declaration here is one the editor will read back, so the object looks the
    /// same after a reopen.
    ///
    /// Authored complete — position and size as well as appearance. A created box
    /// has no authored position for the flow to discover, so it is taken out of
    /// the flow and given coordinates from where the editor dropped it. Every
    /// later move goes through the ordinary geometry path, which respects the
    /// author's own positioning instead of overwriting this.
    fn authored_element_for(&self, object: &DesignObject) -> crate::project_save::NewElement {
        // The editor works in world coordinates and the element is written inside
        // its parent's content, so the position has to be measured from the
        // parent's corner or the object lands twice as far over.
        let origin = self.parent_origin(&object.spool_id);
        let mut declarations = vec![
            ("position".to_owned(), "absolute".to_owned()),
            (
                "left".to_owned(),
                format!(
                    "{}px",
                    crate::project_save::css_length(object.position.x - origin.x)
                ),
            ),
            (
                "top".to_owned(),
                format!(
                    "{}px",
                    crate::project_save::css_length(object.position.y - origin.y)
                ),
            ),
            (
                "width".to_owned(),
                format!("{}px", crate::project_save::css_length(object.size.width)),
            ),
            (
                "height".to_owned(),
                format!("{}px", crate::project_save::css_length(object.size.height)),
            ),
        ];
        if let Some(fill) = object.fill {
            declarations.push(("background-color".to_owned(), css_color(fill.color)));
        }
        if let Some(stroke) = object.stroke {
            declarations.push((
                "border".to_owned(),
                format!("{} solid", css_color(stroke.color)),
            ));
        }
        // A zero radius is the absence of a radius, so it is not authored:
        // `border-radius: 0` would pin a value the author never chose.
        if object.border_radius > 0.0 {
            declarations.push((
                "border-radius".to_owned(),
                format!(
                    "{}px",
                    crate::project_save::css_length(object.border_radius)
                ),
            ));
        }
        if object.opacity < 1.0 {
            declarations.push(("opacity".to_owned(), format!("{}", object.opacity)));
        }
        crate::project_save::NewElement {
            tag: "div".to_owned(),
            text: object.text_content.clone(),
            declarations,
        }
    }

    fn parent_origin(&self, node: &NodeId) -> Point<f32> {
        let parent = self
            .session
            .document
            .structure
            .nodes
            .iter()
            .find(|candidate| &candidate.id == node)
            .and_then(|candidate| candidate.parent.clone());
        let Some(parent) = parent else {
            return point(0.0, 0.0);
        };
        let object = self
            .session
            .runtime
            .objects()
            .iter()
            .find(|object| object.spool_id == parent);
        object
            .and_then(|object| self.session.runtime.geometry(object.id))
            .map(|geometry| geometry.position)
            .unwrap_or(point(0.0, 0.0))
    }

    /// Write the current editor state back to the project's authored source.
    ///
    /// This is the save half of the product loop. It diffs each managed object
    /// against [`Self::source_snapshot`] and emits one [`SourceEdit`] per real
    /// change, and the save layer turns each into the smallest authored edit it
    /// can:
    ///
    /// - geometry becomes a `style` attribute on the element: a size outright,
    ///   and a position as `left`/`top` for a box already out of the flow or
    ///   `transform: translate(...)` for one that is in it
    /// - text replaces the element's authored text
    /// - style rewrites the CSS declaration that already owns the property
    /// - metadata is written by the existing bundle writer
    ///
    /// Objects created during the session have no authored element yet, so they
    /// are reported as unsupported rather than silently dropped. That is a real
    /// limitation of this milestone, not a silent success.
    ///
    /// Returns the outcome, including what could not be written.
    ///
    /// Takes `&mut self` because writing the source changes the baseline the
    /// next write is measured from. See the snapshot refresh at the end.
    pub fn save_project(
        &mut self,
    ) -> Result<crate::project_save::SaveOutcome, crate::project_bundle::BundleError> {
        let mut outcome = crate::project_save::SaveOutcome::default();
        let Some(root) = self.project_root.as_deref() else {
            // No project open: nothing to write, and nothing invented.
            return Ok(outcome);
        };
        let mut edits = Vec::new();
        // Anything the project was opened with and is no longer in the document
        // has to leave the source too, or a reopen brings back an object the user
        // deleted — or an object they undid creating.
        for (node, file) in &self.authored_elements {
            let still_there = self
                .session
                .document
                .structure
                .nodes
                .iter()
                .any(|candidate| &candidate.id == node);
            if !still_there {
                edits.push(crate::project_save::SourceEdit::Remove {
                    node: node.clone(),
                    file: file.clone(),
                });
            }
        }
        for object in self.session.runtime.objects() {
            let known = self
                .session
                .document
                .structure
                .nodes
                .iter()
                .any(|node| node.id == object.spool_id);
            if !known {
                // An object with no node has no binding and therefore nowhere to
                // be authored. Reported rather than invented.
                outcome
                    .unsupported
                    .push(crate::project_save::UnsupportedEdit {
                        node: object.spool_id.clone(),
                        kind: "object",
                        reason: "not in the persistent document, so there is nothing to bind"
                            .into(),
                    });
                continue;
            }
            // No snapshot means the object was not the one that was opened: it was
            // created in this session, so it has an element to author rather than
            // an existing one to edit.
            let Some(before) = self.source_snapshot.get(&object.spool_id) else {
                // Recorded before the edit, because the file is read out of the
                // structure and this is where it is still there.
                if let Some(node) = self
                    .session
                    .document
                    .structure
                    .nodes
                    .iter()
                    .find(|node| node.id == object.spool_id)
                {
                    self.authored_elements
                        .insert(object.spool_id.clone(), node.source.file.clone());
                }
                edits.push(crate::project_save::SourceEdit::Create {
                    node: object.spool_id.clone(),
                    element: self.authored_element_for(object),
                });
                continue;
            };

            let geometry = self.session.runtime.geometry(object.id);
            if let Some(geometry) = geometry {
                // Only what actually changed. Writing an unchanged size back
                // would pin the element to an explicit box the author never
                // wrote, which stops following the stylesheet from then on.
                let moved = before.position != geometry.position;
                let resized = before.size != geometry.size;
                if moved || resized {
                    let parent_origin = self.parent_origin(&object.spool_id);
                    let placement = moved.then(|| {
                        let Some(structure) = self
                            .session
                            .document
                            .structure
                            .nodes
                            .iter()
                            .find(|node| node.id == object.spool_id)
                        else {
                            return crate::project_save::Placement::Flow { dx: 0.0, dy: 0.0 };
                        };
                        // Already out of the flow: keep it that way and write a
                        // position from the containing block, which is the
                        // parent element. The editor works in world
                        // coordinates, so a child has to be written relative to
                        // where its parent now sits or it would jump on reopen.
                        if crate::project_save::is_out_of_flow(&self.session.document, structure)
                            .unwrap_or(false)
                        {
                            return crate::project_save::Placement::ContainingBlock {
                                x: geometry.position.x - parent_origin.x,
                                y: geometry.position.y - parent_origin.y,
                            };
                        }
                        // In the flow: the authored position is wherever the
                        // flow puts this element, so the edit is the change from
                        // that — never an absolute coordinate, which would
                        // delete the author's layout. Measured against the
                        // parent origin *as it is now*, so a child that only
                        // moved because its parent did writes nothing: in the
                        // flow a child's position is its parent's business.
                        let now = sub_point(geometry.position, parent_origin);
                        crate::project_save::Placement::Flow {
                            dx: now.x - before.parent_relative.x,
                            dy: now.y - before.parent_relative.y,
                        }
                    });
                    edits.push(crate::project_save::SourceEdit::Geometry {
                        node: object.spool_id.clone(),
                        placement,
                        width: resized.then_some(geometry.size.width),
                        height: resized.then_some(geometry.size.height),
                    });
                }
            }
            let text = object.text_content.as_ref().filter(|t| !t.is_empty());
            if let Some(text) = text.filter(|text| Some(*text) != before.text.as_ref()) {
                edits.push(crate::project_save::SourceEdit::Text {
                    node: object.spool_id.clone(),
                    text: text.clone(),
                });
            }
            let now = Appearance {
                style: ObjectStyle {
                    fill: object.fill,
                    stroke: object.stroke,
                    border_radius: object.border_radius,
                    opacity: object.opacity,
                },
                text_color: object.text_color,
                font_size: object.font_size,
            };
            edits.extend(style_edits(&object.spool_id, &before.appearance, &now));
        }
        // A multi-file save is not atomic, so a failure can arrive with some files
        // already replaced. The baseline must never move past what actually
        // reached the disk: advancing it for a file that did not land would make
        // the next save skip an edit the user still has open, and advancing it
        // for a file that did land is what prevents the double-apply this
        // snapshot exists to stop.
        //
        // The snapshot is per *node* while a write is per *file*, and the two are
        // not linked, so there is no honest per-node answer to give after a
        // partial write: a node's geometry may have landed in one file while its
        // fill was refused in another. Leaving the whole baseline where it was
        // is the conservative choice — it can leave a file one edit ahead of the
        // record until the project is reopened, which is recoverable, whereas
        // guessing can corrupt an offset that then persists. Reopening re-derives
        // the baseline from the bytes, so that is the documented way back.
        let written = match crate::project_save::save_project(root, &self.session.document, &edits)
        {
            Ok(written) => written,
            Err(failure) => {
                if !failure.written.is_empty() {
                    diagnostics::count("save_partial_write", 1);
                    let unattempted: Vec<&str> = failure
                        .unsupported
                        .iter()
                        .map(|edit| edit.node.as_str())
                        .collect();
                    eprintln!(
                        "spool_save_partial root={} landed={} unattempted={:?} error={}",
                        root.display(),
                        failure.written.len(),
                        unattempted,
                        failure.source
                    );
                }
                return Err(*failure.source);
            }
        };
        let mut written = written;
        written.unsupported.append(&mut outcome.unsupported);

        // The snapshot answers "what does the authored source say?", and the source
        // was just rewritten, so the baseline moves with it — but only as far as
        // the write actually got.
        //
        // Without any refresh the comparison above stays pinned to the state the
        // project was *opened* in, which is right for the first save and wrong
        // for every one after it: undo puts an object back where it was
        // authored, live then matches the opening snapshot, so save concludes
        // nothing changed and writes nothing — leaving the file still describing
        // the move the user just undid.
        //
        // Refusals are honoured per *aspect*, not per node, because a save can
        // write one thing about a node and be refused another: a geometry move
        // that lands beside a fill the editor may not rewrite. Advancing
        // everything would retire the refused edit silently, and advancing
        // nothing would make the next save re-apply the part that did land —
        // which is the double-apply this refresh exists to prevent. So each
        // field keeps whichever answer is true of the bytes now on disk.
        let mut refused: BTreeMap<&NodeId, BTreeSet<&'static str>> = BTreeMap::new();
        for edit in &written.unsupported {
            refused.entry(&edit.node).or_default().insert(edit.kind);
        }
        let previous = std::mem::take(&mut self.source_snapshot);
        let mut next: BTreeMap<NodeId, SourceSnapshot> = BTreeMap::new();
        for (node, mut fresh) in self.opened_state() {
            let Some(kinds) = refused.get(&node) else {
                next.insert(node, fresh);
                continue;
            };
            // `object` and `unknown-node` refuse the node as a whole. With no
            // earlier baseline to fall back on there is nothing truthful to
            // record, so the node keeps no baseline and keeps being reported —
            // which is the honest answer for an object this editor cannot write.
            let whole = kinds.contains("object") || kinds.contains("unknown-node");
            let Some(old) = previous.get(&node) else {
                if !whole {
                    next.insert(node, fresh);
                }
                continue;
            };
            if whole || kinds.contains("geometry") {
                fresh.position = old.position;
                fresh.parent_relative = old.parent_relative;
                fresh.size = old.size;
            }
            if whole || kinds.contains("text") {
                fresh.text = old.text.clone();
            }
            if whole || kinds.contains("style") {
                fresh.appearance = old.appearance;
            }
            next.insert(node, fresh);
        }
        self.source_snapshot = next;
        Ok(written)
    }

    /// The live commit path for every canvas mutation.
    ///
    /// One call is one history entry, however many objects it touched. The
    /// command is normalized first so a gesture that changed nothing, or that
    /// changed only some of a multi-selection, records only the real change.
    /// The canvas has already applied the mutation eagerly, and replaying the
    /// command forward here is safe because every replay is idempotent.
    fn commit(&mut self, command: DocumentCommand) -> bool {
        self.commit_operation(SemanticOperation::Runtime(command))
    }

    /// [`Self::commit`] for a gesture whose edit is more than one command.
    ///
    /// Same single path into `EditSession::execute` — there is no second way for
    /// a canvas edit to reach the document or the history, which is the property
    /// that keeps "one gesture is one history entry" true for the compound case
    /// as well as the simple one.
    fn commit_operation(&mut self, operation: SemanticOperation) -> bool {
        self.session.execute(operation).unwrap_or(false)
    }

    pub fn is_text_editing(&self) -> bool {
        self.text_edit.is_some()
    }

    /// Whether the pointer is in the middle of something Escape should abandon.
    ///
    /// Read-only, and deliberately the same set of states
    /// [`Self::cancel_manipulation`] gives up, so the editor's Escape ladder can
    /// decide which rung is on top from state before anything is cancelled.
    pub fn is_manipulating(&self) -> bool {
        self.interaction.is_active() || self.marquee.is_some() || self.pan.is_some()
    }

    /// Whether a pointer gesture currently owns the input.
    ///
    /// One predicate, because "the drag owns the input" has to have the same
    /// answer for every reader of it. Pointer capture asks it — a drag that lost
    /// capture mid-flight would strand the gesture — and the camera asks it too,
    /// for a reason that is arithmetic rather than cosmetic.
    ///
    /// A gesture measures its movement from a world point captured when the
    /// button went down, and re-projects the pointer through the *current*
    /// camera on every move. A zoom that lands between those two therefore
    /// silently rewrites the distance the user has dragged: pinch from 100% to
    /// 200% halfway through a drag and the object jumps back toward where the
    /// drag began, by half the distance already travelled. Nothing errors and
    /// the committed result looks like a drag the user did not make.
    ///
    /// Freezing the camera for the duration of a gesture is the same rule the
    /// corpus applies to a text buffer — see the scope argument in
    /// [`crate::commands`] — and it is the cheap half of the fix: re-anchoring
    /// every gesture against a camera that may move under it would mean the
    /// gesture's meaning depended on two inputs at once.
    ///
    /// Wider than [`CanvasView::is_manipulating`], which is the Escape ladder's
    /// question. That one deliberately excludes a text-buffer drag: Escape while
    /// drag-selecting text should leave the text session, not just drop the
    /// selection and report that nothing was in flight.
    fn pointer_gesture_active(&self) -> bool {
        self.pan.is_some()
            || self.interaction.is_active()
            || self.marquee.is_some()
            || self
                .text_edit
                .as_ref()
                .is_some_and(|edit| edit.pointer_anchor.is_some())
    }

    /// The object under a world point that has text a user would expect to
    /// edit.
    ///
    /// Not "anything typed as text": a source-backed `<a class="cta">` is
    /// declared as a frame in `lamine.yaml` yet carries a label, and refusing to
    /// edit it would make a correctly loaded project uneditable. The rule is
    /// what the user sees — a non-empty text run — and it is the same rule the
    /// renderer draws by.
    fn editable_text_at(&self, world: Point<f32>) -> Option<ObjectId> {
        let id = self.session.runtime.hit_test(world)?;
        self.session
            .runtime
            .object(id)
            .filter(|object| {
                object
                    .text_content
                    .as_deref()
                    .is_some_and(|text| !text.is_empty())
            })
            .map(|object| object.id)
    }

    fn begin_text_edit(&mut self, id: ObjectId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.session.runtime.text_content(id).map(str::to_owned) else {
            return;
        };
        let end = text.len();
        let hierarchy = self.hierarchy();
        self.selection.click(Some(id), false, &hierarchy);
        self.interaction = Interaction::None;
        self.text_edit = Some(TextEditState {
            id,
            original_text: text.clone(),
            editing_text: text,
            selected_range: end..end,
            selection_reversed: false,
            marked_range: None,
            pointer_anchor: None,
        });
        if let Some(focus_handle) = &self.focus_handle {
            window.focus(focus_handle, cx);
        }
        diagnostics::count("canvas_notify", 1);
        cx.notify();
    }

    pub fn cancel_text_edit(&mut self, cx: &mut Context<Self>) -> bool {
        let cancelled = self.discard_text_edit();
        if cancelled {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
        cancelled
    }

    fn discard_text_edit(&mut self) -> bool {
        self.text_edit.take().is_some()
    }

    pub fn commit_text_edit_session(&mut self, cx: &mut Context<Self>) -> bool {
        let committed = self.commit_text_edit();
        if committed {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
        committed
    }

    fn commit_text_edit(&mut self) -> bool {
        let Some(edit) = self.text_edit.take() else {
            return false;
        };
        if edit.original_text == edit.editing_text {
            return false;
        }
        if !self
            .session
            .runtime
            .set_text_content(edit.id, edit.editing_text.clone())
        {
            return false;
        }
        self.commit(DocumentCommand::text(vec![TextChange {
            id: edit.id,
            before: edit.original_text,
            after: edit.editing_text,
        }]));
        true
    }

    fn edit_cursor(&self) -> Option<usize> {
        let edit = self.text_edit.as_ref()?;
        Some(if edit.selection_reversed {
            edit.selected_range.start
        } else {
            edit.selected_range.end
        })
    }

    fn set_text_selection(&mut self, anchor: usize, caret: usize, cx: &mut Context<Self>) {
        let Some(edit) = self.text_edit.as_mut() else {
            return;
        };
        let (range, reversed) = selection_from_anchor_and_caret(&edit.editing_text, anchor, caret);
        edit.selected_range = range;
        edit.selection_reversed = reversed;
        edit.marked_range = None;
        diagnostics::count("canvas_notify", 1);
        cx.notify();
    }

    fn move_text_cursor(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Some(edit) = self.text_edit.as_mut() else {
            return;
        };
        let offset = utf8_boundary(&edit.editing_text, offset.min(edit.editing_text.len()));
        edit.selected_range = offset..offset;
        edit.selection_reversed = false;
        edit.marked_range = None;
        diagnostics::count("canvas_notify", 1);
        cx.notify();
    }

    fn select_text_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Some(edit) = self.text_edit.as_mut() else {
            return;
        };
        let offset = utf8_boundary(&edit.editing_text, offset.min(edit.editing_text.len()));
        if edit.selection_reversed {
            edit.selected_range.start = offset;
        } else {
            edit.selected_range.end = offset;
        }
        if edit.selected_range.end < edit.selected_range.start {
            edit.selection_reversed = !edit.selection_reversed;
            edit.selected_range = edit.selected_range.end..edit.selected_range.start;
        }
        edit.marked_range = None;
        diagnostics::count("canvas_notify", 1);
        cx.notify();
    }

    fn replace_editing_text(
        &mut self,
        range: Option<Range<usize>>,
        replacement: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.text_edit.as_mut() else {
            return;
        };
        let range = range
            .map(|range| utf16_range_to_utf8(&edit.editing_text, range))
            .or_else(|| edit.marked_range.clone())
            .unwrap_or_else(|| edit.selected_range.clone());
        let start = utf8_boundary(&edit.editing_text, range.start);
        let end = utf8_boundary(&edit.editing_text, range.end);
        edit.editing_text.replace_range(start..end, replacement);
        let cursor = start + replacement.len();
        edit.selected_range = cursor..cursor;
        edit.selection_reversed = false;
        edit.marked_range = None;
        diagnostics::count("canvas_notify", 1);
        cx.notify();
    }

    /// `⌫` inside a text buffer.
    ///
    /// With no buffer open this does nothing at all, because the verb still
    /// exists — it deletes the selection instead, which the editor-wide command
    /// table owns. It used to ring the system bell here, and because GPUI
    /// dispatches a key binding *before* the shell's own key listener, deleting
    /// an object rang an error bell immediately before the object disappeared. A
    /// key that is about to do something must not announce that it is about to
    /// fail.
    fn text_backspace(&mut self, _: &Backspace, _window: &mut Window, cx: &mut Context<Self>) {
        if self.delete_a_character(false) {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
    }

    /// `⌦` inside a text buffer. Falls through to the editor-wide delete when
    /// there is no buffer, for the reason [`Self::text_backspace`] gives.
    fn text_delete(&mut self, _: &Delete, _window: &mut Window, cx: &mut Context<Self>) {
        if self.delete_a_character(true) {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
    }

    /// Delete one character, or the selected range, from the open buffer.
    ///
    /// `forward` is `⌦` rather than `⌫`. Returns whether there was a buffer to
    /// act on, which is what tells the caller not to claim the key: with no
    /// buffer the verb belongs to the selection, not to a caret.
    fn delete_a_character(&mut self, forward: bool) -> bool {
        let Some(edit) = self.text_edit.as_ref() else {
            return false;
        };
        let Some(cursor) = self.edit_cursor() else {
            return false;
        };
        let range = if edit.selected_range.is_empty() {
            let boundary = if forward {
                next_char_boundary(&edit.editing_text, cursor)
            } else {
                previous_char_boundary(&edit.editing_text, cursor)
            };
            boundary..cursor
        } else {
            edit.selected_range.clone()
        };
        let start = utf8_boundary(&edit.editing_text, range.start);
        let end = utf8_boundary(&edit.editing_text, range.end);
        let edit = self.text_edit.as_mut().unwrap();
        edit.editing_text.replace_range(start..end, "");
        edit.selected_range = start..start;
        edit.selection_reversed = false;
        edit.marked_range = None;
        true
    }

    fn text_left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if let (Some(edit), Some(cursor)) = (self.text_edit.as_ref(), self.edit_cursor()) {
            let offset = if edit.selected_range.is_empty() {
                previous_char_boundary(&edit.editing_text, cursor)
            } else {
                edit.selected_range.start
            };
            self.move_text_cursor(offset, cx);
        }
    }

    fn text_right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if let (Some(edit), Some(cursor)) = (self.text_edit.as_ref(), self.edit_cursor()) {
            let offset = if edit.selected_range.is_empty() {
                next_char_boundary(&edit.editing_text, cursor)
            } else {
                edit.selected_range.end
            };
            self.move_text_cursor(offset, cx);
        }
    }

    fn text_select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        if let (Some(edit), Some(cursor)) = (self.text_edit.as_ref(), self.edit_cursor()) {
            self.select_text_to(previous_char_boundary(&edit.editing_text, cursor), cx);
        }
    }

    fn text_select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        if let (Some(edit), Some(cursor)) = (self.text_edit.as_ref(), self.edit_cursor()) {
            self.select_text_to(next_char_boundary(&edit.editing_text, cursor), cx);
        }
    }

    fn text_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(edit) = self.text_edit.as_mut() {
            edit.selected_range = 0..edit.editing_text.len();
            edit.selection_reversed = false;
            edit.marked_range = None;
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
    }

    fn text_home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_text_cursor(0, cx);
    }

    fn text_end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(edit) = &self.text_edit {
            self.move_text_cursor(edit.editing_text.len(), cx);
        }
    }

    fn text_paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_editing_text(None, &text, cx);
        }
    }

    fn text_copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(edit) = &self.text_edit {
            if !edit.selected_range.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    edit.editing_text[edit.selected_range.clone()].to_string(),
                ));
            }
        }
    }

    fn text_cut(&mut self, _: &Cut, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(edit) = &self.text_edit {
            if !edit.selected_range.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    edit.editing_text[edit.selected_range.clone()].to_string(),
                ));
                self.replace_editing_text(None, "", cx);
            }
        }
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn layer_structure_revision(&self) -> u64 {
        self.session.runtime.layer_structure_revision()
    }

    pub fn document_objects(&self) -> &[DesignObject] {
        self.session.runtime.objects()
    }

    /// The canonical, source-backed document.
    ///
    /// Read-only on purpose: metadata is durable and is changed through
    /// semantic operations, never by poking at it from a view.
    pub fn persistent_document(&self) -> &PersistentDocument {
        &self.session.document
    }

    /// The document's drawn hierarchy, projected for this moment.
    ///
    /// Rebuilt on demand rather than cached: it is derived from the persistent
    /// structure and the runtime object list, and a cache here would be a
    /// second place for hierarchy to live and go stale. Selection work happens
    /// on click and on marquee release, not per frame, so the walk is not on a
    /// hot path.
    pub fn hierarchy(&self) -> Hierarchy {
        Hierarchy::build(
            self.session.runtime.objects(),
            &self.session.document.structure,
        )
    }

    /// Move the selection by keyboard, and say whether anything moved.
    ///
    /// Three separate jobs that the corpus splits three ways:
    ///
    /// - `Tab` walks **siblings**, not document order, so a row inside a frame
    ///   is never a sibling of the frame itself.
    /// - `Enter` descends into a container, so the user can get at something a
    ///   plain click deliberately will not select.
    /// - `⇧Enter` ascends, and ascending out of a root is the documented way to
    ///   give up on a deep selection — Figma's `Esc` ladder, reachable without
    ///   losing the root it came from.
    ///
    /// With nothing selected there is no current row to walk from, so a forward
    /// step selects the first root. A step with nowhere to go reports `false`
    /// and leaves the selection alone, which is what lets the shell fall
    /// through to whatever else claimed the key.
    pub fn traverse_selection(&mut self, step: Traversal) -> bool {
        let hierarchy = self.hierarchy();
        let current = match self.selection.only() {
            Some(id) => id,
            None => {
                let Some(first) = hierarchy.order().first().copied() else {
                    return false;
                };
                self.selection.click(Some(first), false, &hierarchy);
                return true;
            }
        };
        let landed = match step {
            Traversal::Sibling(forward) => {
                if forward {
                    hierarchy.next_sibling(current)
                } else {
                    hierarchy.previous_sibling(current)
                }
            }
            Traversal::Descend => hierarchy.first_child(current),
            Traversal::Ascend => hierarchy.parent(current),
        };
        let Some(landed) = landed else {
            return false;
        };
        self.selection.click(Some(landed), false, &hierarchy);
        true
    }

    /// Move the selection by keyboard and repaint if it moved.
    pub fn traverse(&mut self, step: Traversal, cx: &mut Context<Self>) -> bool {
        self.commit_text_edit();
        let moved = self.traverse_selection(step);
        if moved {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
        moved
    }

    /// The disposable runtime document the editor draws and hit-tests.
    ///
    /// Exposed so a project can be checked against what the editor actually
    /// holds, rather than against a separate copy.
    // Test-only today: the open-path tests use it to assert that a loaded
    // project is really hittable and that an unsupported kind is really absent.
    // The editor reads `document_objects` instead. Compiled out of the binary
    // rather than silenced, so it cannot drift into being a second accessor the
    // editor depends on.
    #[cfg(test)]
    pub fn runtime_document(&self) -> &Document {
        &self.session.runtime
    }

    /// The undo stack, for the window-level interaction tests.
    ///
    /// Test-only for the same reason `runtime_document` is: history depth is an
    /// assertion about an interaction's consequences, and the interactions that
    /// matter most here — a click, a key press — cannot be run without a window.
    #[cfg(test)]
    pub fn test_history(&self) -> &crate::operations::SemanticHistory {
        &self.session.history
    }

    /// Whether the canvas currently holds keyboard focus.
    ///
    /// Asked as a question rather than exposing the handle, so that nothing can
    /// take the handle and use it for something other than asking this. It is
    /// the precondition for every editor shortcut, and until the canvas takes
    /// focus on pointer interaction a key press goes to the dispatch tree's
    /// synthetic root instead of to the shell.
    #[cfg(test)]
    pub fn holds_keyboard_focus(&self, window: &Window) -> bool {
        self.focus_handle
            .as_ref()
            .is_some_and(|handle| handle.is_focused(window))
    }

    /// A world point in canvas-local screen coordinates.
    ///
    /// The inverse of `object_screen_center`, for the tests that need to click
    /// somewhere rather than something — empty canvas, mostly.
    #[cfg(test)]
    pub fn world_to_canvas_screen(&self, world: Point<f32>) -> Point<f32> {
        self.camera.world_to_screen(world)
    }

    /// An object's centre in canvas-local screen coordinates.
    ///
    /// For tests that intend to click a particular object. Computing the point
    /// from the camera is the point: a hard-coded coordinate would quietly stop
    /// pointing at the object the moment the layout or zoom changed, and the
    /// test would go on passing while clicking empty canvas.
    #[cfg(test)]
    pub fn object_screen_center(&self, index: usize) -> Option<Point<f32>> {
        let object = self.document_objects().get(index)?;
        let centre = point(
            object.position.x + object.size.width / 2.0,
            object.position.y + object.size.height / 2.0,
        );
        Some(self.camera.world_to_screen(centre))
    }

    /// The tool the canvas is acting with.
    ///
    /// Read by the shell so the toolbar shows the tool that is actually in
    /// effect. Creation completes by changing it, and a second copy in the shell
    /// would be a toolbar that disagrees with the canvas.
    pub fn tool(&self) -> Tool {
        self.tool
    }

    /// Return to the Selection tool after a creation.
    ///
    /// So the next thing the user does — move, resize, rename, duplicate, delete —
    /// acts on the object they just made, without a trip back to the toolbar
    /// first. One method, called from every creation, so a click-created object
    /// and a drag-created one finish identically.
    ///
    /// Deliberately not `set_tool`: that commits an open text edit, and for a Text
    /// click the session this is about to open is not the one to end.
    fn finish_creation(&mut self) {
        self.abandon_interaction();
        self.marquee = None;
        self.clear_gesture_feedback();
        self.tool = Tool::Select;
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.commit_text_edit();
        self.abandon_interaction();
        self.tool = tool;
        self.marquee = None;
        self.clear_gesture_feedback();
    }

    /// Put the document and the selection back the way they were before the
    /// in-flight gesture, and record nothing.
    ///
    /// Abandoning a gesture is one policy, so it lives here once rather than
    /// open-coded at each caller. Restoring the geometry is not the whole of it:
    /// an `⌥`-drag *created* objects, so the copies have to go back out — and the
    /// selection, which the drag handed over to those copies, has to notice they
    /// are gone. A caller that restored the geometry and forgot the rest left
    /// the panel holding ids that named nothing: invisible, but still counted as
    /// "a selection" by the Escape ladder, so the next Escape offered to clear a
    /// selection the user could not see.
    ///
    /// Returns whether there was anything in flight. A marquee or a pan is not a
    /// gesture in this sense and is left alone: neither changed the document, so
    /// there is nothing to put back.
    fn abandon_interaction(&mut self) -> bool {
        if !self.interaction.is_active() {
            return false;
        }
        self.interaction.restore(&mut self.session.runtime);
        self.interaction = Interaction::None;
        self.retain_existing_selection();
        true
    }

    /// Record whatever the in-flight gesture has already done to the document.
    ///
    /// A history key arrives from the keyboard with no pointer position, so a
    /// live gesture cannot be re-derived from one — and it does not have to be.
    /// A drag writes to the runtime as it goes, so the document already holds
    /// where the user actually got to; the only thing missing is the entry.
    ///
    /// This exists because the alternative made one keystroke do two unrelated
    /// things. Discarding the gesture instead of recording it threw away the
    /// edit in flight with no way back, *and then* reverted whatever came before
    /// it. Finishing the gesture first means the key that follows reverts the
    /// thing the user just did, which is the only reading in which it means one
    /// thing — and it is the same operation a pointer-up records, reached
    /// without a pointer.
    ///
    /// Redo takes the same route. Committing clears the redo branch, so a redo
    /// key pressed mid-drag finishes the drag and then finds nothing to redo.
    /// That is the conservative answer: the alternative applies a redo on top of
    /// an edit the user is still in the middle of making.
    fn commit_in_flight_interaction(&mut self) -> bool {
        let interaction = std::mem::replace(&mut self.interaction, Interaction::None);
        let committed = match interaction {
            Interaction::Moving(gesture) => {
                let geometry = geometry_command(&self.session.runtime, &gesture.objects);
                self.commit_operation(gesture.operation(geometry))
            }
            Interaction::Resizing(gesture) => {
                let command = geometry_command(&self.session.runtime, &gesture.members);
                self.commit(command)
            }
            Interaction::Creating(gesture) => {
                let geometry =
                    creation_geometry(gesture.pointer_start_world, gesture.current_world);
                self.commit_creation(gesture.object_type, geometry.position, geometry.size);
                true
            }
            // Nothing has moved yet, so there is nothing to record. Reporting
            // `false` matters: a gesture that never crossed the drag threshold
            // must not become a step the user has to undo twice.
            Interaction::None
            | Interaction::PotentialMove(_)
            | Interaction::PotentialResize(_)
            | Interaction::PotentialCreate(_) => false,
        };
        self.clear_gesture_feedback();
        committed
    }

    /// Undo the last committed semantic operation.
    ///
    /// This is the whole live undo path minus repainting, split out so the
    /// integration tests can drive it without a GPUI window. Everything that
    /// decides *what* changes happens here; [`CanvasView::undo`] only adds the
    /// notification.
    fn undo_history(&mut self) -> bool {
        self.commit_text_edit();
        self.commit_in_flight_interaction();
        let changed = self.session.undo().unwrap_or(false);
        if changed {
            self.retain_existing_selection();
            self.refresh_object_names();
        }
        changed
    }

    /// Redo the last undone operation. See [`CanvasView::undo_history`].
    fn redo_history(&mut self) -> bool {
        self.commit_text_edit();
        self.commit_in_flight_interaction();
        let changed = self.session.redo().unwrap_or(false);
        if changed {
            self.retain_existing_selection();
            self.refresh_object_names();
        }
        changed
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.undo_history();
        if changed {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
        changed
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.redo_history();
        if changed {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
        changed
    }

    pub fn selected_objects(&self) -> Vec<DesignObject> {
        self.selection
            .ids()
            .iter()
            .filter_map(|id| self.session.runtime.object(*id).cloned())
            .collect()
    }

    pub fn set_selected_style(&mut self, edit: StyleEdit, cx: &mut Context<Self>) -> bool {
        self.commit_text_edit();
        let had_interaction = self.interaction.is_active();
        let changed = self.apply_selected_style(edit);
        if changed || had_interaction {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
        changed
    }

    fn apply_selected_style(&mut self, edit: StyleEdit) -> bool {
        self.abandon_interaction();
        let changes: Vec<_> = self
            .selection
            .ids()
            .iter()
            .filter_map(|id| {
                let before = self.session.runtime.appearance(*id)?;
                let after = edited_style(before, edit);
                (before != after).then_some(StyleChange {
                    id: *id,
                    before,
                    after,
                })
            })
            .collect();
        if changes.is_empty() {
            return false;
        }
        for change in &changes {
            self.session.runtime.set_appearance(change.id, change.after);
        }
        self.commit(DocumentCommand::style(changes));
        true
    }

    /// Replace one object's text as a single semantic operation.
    ///
    /// The same command a committed caret edit produces, for callers that
    /// already know the text. It goes through the same history boundary rather
    /// than around it: one call is one undo step, undo restores the exact
    /// previous run of text, and redo re-applies this one. Used by the runtime
    /// probe and by anything that needs to set text without a caret.
    #[cfg(any(test, debug_assertions))]
    pub fn set_object_text(&mut self, id: ObjectId, text: String) -> bool {
        self.commit_text_edit();
        let Some(before) = self.session.runtime.text_content(id).map(str::to_owned) else {
            return false;
        };
        if before == text {
            return false;
        }
        let Some(change) = self
            .session
            .runtime
            .set_text_content(id, text.clone())
            .then_some(TextChange {
                id,
                before,
                after: text,
            })
        else {
            return false;
        };
        self.commit(DocumentCommand::text(vec![change]));
        true
    }

    /// One object's current geometry.
    ///
    /// Read through this rather than reaching into the document from the shell:
    /// the Inspector shows what the runtime holds, which is the same value the
    /// canvas draws and the same one save diffs against.
    pub fn object_geometry(&self, id: ObjectId) -> Option<Geometry> {
        self.session.runtime.geometry(id)
    }

    /// Set one object's geometry as a single semantic operation.
    ///
    /// The Inspector's way of asking the same question a canvas drag asks. It
    /// goes through the same history boundary instead of writing geometry
    /// beside it, so one call is one undo step, a value that lands where it
    /// started records nothing, and the document stays the only place a new
    /// position lives.
    pub fn set_object_geometry(&mut self, id: ObjectId, geometry: Geometry) -> bool {
        self.commit_text_edit();
        let Some(before) = self.session.runtime.geometry(id) else {
            return false;
        };
        self.commit_geometry_change(id, before, geometry)
    }

    /// Start an Inspector geometry change that is not recorded yet.
    ///
    /// Returns the geometry the object had, which the caller has to hand back
    /// on commit or cancel: while a continuous control is being dragged the
    /// runtime already shows the new value, and only this remembers where it
    /// started.
    pub fn begin_geometry_scrub(&mut self, id: ObjectId) -> Option<GeometryScrub> {
        self.commit_text_edit();
        let before = self.session.runtime.geometry(id)?;
        Some(GeometryScrub { id, before })
    }

    /// Move a scrubbing object without recording anything.
    pub fn scrub_geometry(&mut self, scrub: &GeometryScrub, geometry: Geometry) -> bool {
        self.session.runtime.set_geometry(scrub.id, geometry)
    }

    /// Finish a scrub as one semantic operation.
    ///
    /// A scrub that ended where it started records nothing, exactly as a canvas
    /// drag that returns to its origin does.
    pub fn commit_geometry_scrub(&mut self, scrub: GeometryScrub) -> bool {
        let Some(after) = self.session.runtime.geometry(scrub.id) else {
            return false;
        };
        self.commit_geometry_change(scrub.id, scrub.before, after)
    }

    /// Abandon a scrub: the runtime goes back to where it started and nothing is
    /// recorded.
    pub fn cancel_geometry_scrub(&mut self, scrub: GeometryScrub) -> bool {
        self.session.runtime.set_geometry(scrub.id, scrub.before)
    }

    fn commit_geometry_change(&mut self, id: ObjectId, before: Geometry, after: Geometry) -> bool {
        if before == after {
            return false;
        }
        self.session.runtime.set_geometry(id, after);
        self.commit(DocumentCommand::geometry(vec![GeometryChange {
            id,
            before,
            after,
        }]));
        true
    }

    /// Rename a source-backed object, as one semantic operation.
    ///
    /// Goes through [`crate::operations::rename_node_in`] rather than writing a
    /// name into the document beside the history, so a rename is the same kind
    /// of undo step as a move and lands in the metadata file on save like every
    /// other structural edit. The runtime object's name is refreshed from the
    /// document afterwards, because the document is the authority and the
    /// runtime only ever mirrors it.
    ///
    /// A created object has no node to rename yet, so nothing is recorded.
    pub fn rename_object(&mut self, id: ObjectId, name: String) -> bool {
        self.commit_text_edit();
        let Some(object) = self.session.runtime.object(id) else {
            return false;
        };
        let node = object.spool_id.clone();
        let current = object.name.clone();
        if current == name {
            return false;
        }
        let Ok(operation) =
            crate::operations::rename_node_in(&self.session.document, node.clone(), name)
        else {
            return false;
        };
        let changed = self.session.execute(operation).unwrap_or(false);
        if !changed {
            return false;
        }
        // The runtime carries a copy of the name so the Inspector and the layers
        // list can read it without walking the persistent document.
        let renamed = self
            .session
            .document
            .structure
            .nodes
            .iter()
            .find(|candidate| candidate.id == node)
            .map(|candidate| candidate.name.clone());
        if let Some(name) = renamed {
            self.session.runtime.set_object_name(id, name);
        }
        true
    }

    /// Re-mirror the document's node names onto the runtime objects.
    ///
    /// The persistent document is the authority for a name; the runtime copy
    /// exists so the Inspector and the layers list can read one without walking
    /// the document. Anything that can change a name has to refresh that copy —
    /// a rename, an undo, a redo — or the copy quietly becomes a second name
    /// store that disagrees with the one that saves.
    fn refresh_object_names(&mut self) {
        let names: Vec<(NodeId, String)> = self
            .session
            .document
            .structure
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.name.clone()))
            .collect();
        for object in self.session.runtime.objects().to_vec() {
            if let Some((_, name)) = names.iter().find(|(id, _)| *id == object.spool_id) {
                self.session
                    .runtime
                    .set_object_name(object.id, name.clone());
            }
        }
    }

    /// The one entry point for a selection change that did not start on the
    /// canvas — the layers panel, a rename, a restore.
    ///
    /// `id` is taken as given and is *not* climbed to a parent: the panel
    /// already knows which row was clicked, and a row click means that exact
    /// row. The climb belongs to the canvas, where the only thing known about
    /// the target is that the pointer is on top of it.
    pub fn select_object(&mut self, id: ObjectId, additive: bool, cx: &mut Context<Self>) {
        self.commit_text_edit();
        let hierarchy = self.hierarchy();
        self.selection.click(Some(id), additive, &hierarchy);
        diagnostics::count("canvas_notify", 1);
        cx.notify();
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.commit_text_edit();
        let had_marquee = self.marquee.take().is_some();
        if !self.selection.is_empty() {
            self.selection.click(None, false, &self.hierarchy());
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        } else if had_marquee {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
    }

    /// Select every top-level object — `⌘A` in all four products.
    ///
    /// Roots only, not every object. An ancestor and one of its descendants are
    /// both selected at once, and every operation that reads the selection then
    /// has to guess which of the two the user meant; tldraw filters that
    /// combination out of the selection for exactly this reason and Figma warns
    /// against constructing it. Selecting the roots says the same thing and
    /// stays unambiguous.
    ///
    /// Resolved through the persistent document rather than the runtime object
    /// list, so the authored hierarchy decides what nests and not the order
    /// things happen to be drawn in. An object with no structural node at all is
    /// a root: nothing claims it, so nothing contains it.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.select_roots();
        diagnostics::count("canvas_notify", 1);
        cx.notify();
    }

    /// [`Self::select_all`] without the repaint, so the selection rule can be
    /// driven without a window.
    fn select_roots(&mut self) {
        self.commit_text_edit();
        // "Nested" is not the same as "is somebody's parent": a root has no
        // parent, and every node named as a parent is by definition on its way
        // down from a root. A root that *has* children is still a root, so the
        // test has to be "does this object have a drawn parent", not "does some
        // node name it".
        let hierarchy = self.hierarchy();
        let roots: Vec<ObjectId> = self
            .session
            .runtime
            .objects()
            .iter()
            .filter(|object| hierarchy.parent_of(object.id).is_none())
            .map(|object| object.id)
            .collect();
        // Normalized for the same reason the roots are chosen in the first
        // place: a selection holding an ancestor and its descendant is the
        // ambiguous shape `select_roots` exists to avoid, and going through the
        // raw setter here would reintroduce it the moment the two disagreed.
        self.selection.replace_normalized(roots, &hierarchy);
    }

    pub fn zoom_percent(&self) -> u32 {
        (self.camera.zoom * 100.0).round() as u32
    }

    pub fn set_zoom_percent(&mut self, zoom_percent: u32) -> bool {
        if self.camera_is_frozen() {
            return false;
        }
        self.camera.set_zoom_at_center(zoom_percent as f32 / 100.0);
        true
    }

    /// Nudge every selected object by a world-space delta, as one operation.
    ///
    /// Arrow keys, `⇧` for ten times the distance. One press is one history
    /// entry, which is what makes holding an arrow key cheap to undo: the user
    /// presses it four times and presses undo once, not the other way round.
    ///
    /// Deliberately *not* snapped. An arrow key states a position; it does not
    /// ask where the object should be. More concretely, snapping is a magnet
    /// within eight screen pixels, so a nudge of an object that happens to
    /// share a neighbour's edge would be pulled straight back and the key
    /// press would do nothing at all. Every product in the corpus treats the
    /// arrow keys as a direct set for exactly that reason.
    pub fn nudge_selection(&mut self, dx: f32, dy: f32) -> bool {
        self.commit_text_edit();
        let ids = self.selection.ids().to_vec();
        let mut changes = Vec::new();
        for id in &ids {
            if let Some(before) = self.session.runtime.geometry(*id) {
                changes.push((*id, before));
            }
        }
        if changes.is_empty() {
            return false;
        }
        let recorded: Vec<GeometryChange> = changes
            .iter()
            .filter_map(|(id, before)| {
                let after = Geometry {
                    position: point(before.position.x + dx, before.position.y + dy),
                    size: before.size,
                };
                (after != *before).then_some(GeometryChange {
                    id: *id,
                    before: *before,
                    after,
                })
            })
            .collect();
        if recorded.is_empty() {
            return false;
        }
        for change in &recorded {
            self.session.runtime.set_geometry(change.id, change.after);
        }
        self.commit(DocumentCommand::geometry(recorded));
        true
    }

    /// Turn an `⌥`-drag into a drag of copies, and hand the gesture over to them.
    ///
    /// The copies come from [`Document::duplicate_objects`], which is the same
    /// routine `⌘D` uses. That is deliberate: one implementation of "what a copy
    /// of this object is", so a duplicated-by-drag object and a duplicated-by-
    /// keystroke object are the same object as far as the document, the layers
    /// panel, save and undo are concerned. Ids, node ids and names all come from
    /// the document's own allocators, so a copy cannot collide with a live object
    /// and cannot be renumbered by arithmetic.
    ///
    /// The gesture is rewritten to own the copies: its snapshots become the
    /// copies' geometry *as duplicated* — which is the offset position, not the
    /// original — and the selection moves to them. Both matter:
    ///
    /// - the snapshots are the geometry the drag then edits, so the recorded
    ///   `before` is the duplicated position and undo rewinds to it rather than
    ///   to the original's;
    /// - the selection follows the copies, because after an `⌥`-drag the copies
    ///   are what the user is holding. The originals stay selected nowhere, and
    ///   stay exactly where they were.
    ///
    /// The copies land offset by the same 16px the `⌘D` path uses. There is no
    /// canonical Figma offset in the research corpus, so this is Spool's number:
    /// reusing the existing one means the two duplicate routes cannot drift, and
    /// the offset is visible under the original rather than hidden behind it.
    fn duplicate_gesture_selection(&mut self, gesture: &mut MoveGesture) -> bool {
        let source_ids: Vec<ObjectId> = gesture.objects.iter().map(|object| object.id).collect();
        if source_ids.is_empty() {
            return false;
        }
        let placements = self.session.runtime.duplicate_objects(&source_ids);
        if placements.is_empty() {
            return false;
        }
        let duplicate_ids: Vec<ObjectId> = placements
            .iter()
            .map(|placement| placement.object.id)
            .collect();
        gesture.placements = placements;
        gesture.duplicates = duplicate_ids.clone();
        gesture.objects = duplicate_ids
            .iter()
            .filter_map(|id| {
                self.session
                    .runtime
                    .geometry(*id)
                    .map(|geometry| ObjectSnapshot { id: *id, geometry })
            })
            .collect();
        gesture.selected_ids = duplicate_ids;
        true
    }

    /// Move a gesture's objects to where the pointer is, snapping unless the
    /// gesture suspended it.
    ///
    /// The one place a drag's geometry is computed. Updating and releasing both
    /// come through here, which is what makes the object land in the same place
    /// whether the user lets go mid-drag or drops it on the final pixel — a
    /// release that re-derives its own position is how objects used to jump by
    /// a snap width on mouse-up.
    fn drag_gesture_objects(&mut self, gesture: &MoveGesture, screen: Point<f32>) {
        let raw = movement_delta(self.camera, gesture.pointer_start_world, screen);
        let lock = dragged_axis(raw, self.constrain_drag);
        let raw = match lock {
            snap::AxisLock::Free => raw,
            snap::AxisLock::X => point(raw.x, 0.0),
            snap::AxisLock::Y => point(0.0, raw.y),
        };
        let (dx, dy) = if gesture.suspend_snap {
            self.snap_guides.clear();
            (raw.x, raw.y)
        } else {
            // The bounds come from the gesture's own snapshots, never from the
            // live geometry. Reading live geometry would make every pointer
            // movement after the first add the whole drag again on top of the
            // position the previous movement already produced — so the object
            // would accelerate away from the pointer, and the snap would be
            // measured from a place the user never dragged it to.
            let bounds = snap::bounds_of(
                &gesture
                    .objects
                    .iter()
                    .map(|o| snap_rect(o.geometry))
                    .collect::<Vec<_>>(),
            );
            match bounds {
                Some(bounds) => {
                    let ids: Vec<ObjectId> = gesture.objects.iter().map(|o| o.id).collect();
                    self.snap_delta(bounds, &ids, (raw.x, raw.y), lock)
                }
                None => (raw.x, raw.y),
            }
        };
        apply_move(&mut self.session.runtime, &gesture.objects, point(dx, dy));
    }

    /// Resize a gesture's object to where the pointer is, snapping the edges the
    /// handle moves.
    ///
    /// The one place a resize's geometry is computed, for the same reason the
    /// move has one: updating and releasing both come through here, so the
    /// object lands in the same place whether the user lets go mid-drag or drops
    /// it on the final pixel.
    ///
    /// The pointer's delta is snapped and then handed back to `resized_geometry`
    /// rather than a finished rectangle being patched afterwards. That keeps the
    /// minimum-size clamps in one place, and it is what makes the guide
    /// trustworthy: the snapper is told the minimum size up front, so it declines
    /// a correction the clamp would have thrown away rather than claiming an
    /// alignment that never happened.
    fn drag_resize_object(&mut self, gesture: &ResizeGesture, screen: Point<f32>) {
        let start = gesture.bounds;
        let raw = movement_delta(self.camera, gesture.pointer_start_world, screen);
        let proportional = self.constrain_drag;
        // Two reasons a resize asks for no snap, and they are not the same
        // reason. `⌘` is the user saying "this once, freely", which means
        // exactly what it means for a move. `⇧` is arithmetic: it turns the
        // pointer's travel into a scale factor before any edge has a position
        // to align, so there is no single edge whose correction would survive
        // the scale that is about to be applied to it.
        let snapping = !proportional && !gesture.suspend_snap;
        let delta = if snapping {
            self.snap_resize_delta(gesture, start, (raw.x, raw.y))
        } else {
            self.snap_guides.clear();
            (raw.x, raw.y)
        };
        let resized = resized_geometry(
            start,
            gesture.handle,
            point(delta.0, delta.1),
            proportional,
            self.center_drag,
        );
        // The handle moved one rectangle; every member is re-expressed inside it.
        // For a single-object selection this is the identity mapping — one member,
        // and the union box is that object's own box — so the single-object path
        // needs no separate branch here.
        for (id, geometry) in scaled_members(start, resized, &gesture.members) {
            self.session.runtime.set_geometry(id, geometry);
        }
    }

    /// Snap the edges a resize handle is moving, and remember the guides.
    fn snap_resize_delta(
        &mut self,
        gesture: &ResizeGesture,
        start: ObjectGeometry,
        delta: (f32, f32),
    ) -> (f32, f32) {
        // The gesture's own members are excluded as targets, for the same reason
        // a move excludes them: a member's own edge is not something to align
        // to. The box being snapped is the *selection's* box, so a multi-selection
        // resizes against its neighbours as one unit.
        let ids: Vec<ObjectId> = gesture.members.iter().map(|member| member.id).collect();
        let targets = self.snap_targets(&ids);
        let snapped = snap::snap_resize(
            snap_rect(start),
            delta,
            gesture.handle.moved_edges(),
            &targets,
            self.camera.zoom,
            MIN_OBJECT_SIZE,
        );
        self.snap_guides = snapped.guides;
        snapped.delta
    }

    /// Snap a proposed world-space translation of `moving` against everything
    /// else on the canvas, and remember the guides that explain the result.
    ///
    /// The whole selection is one rectangle: a multi-selection moves as a unit
    /// and snaps as a unit, because snapping each object independently makes a
    /// group fly apart. `lock` is any axis the gesture has been constrained away
    /// from, and the guides are left on the view for the renderer — a snap with
    /// nothing to show for it is the one thing the user cannot debug.
    fn snap_delta(
        &mut self,
        bounds: snap::Rect,
        moving: &[ObjectId],
        delta: (f32, f32),
        lock: snap::AxisLock,
    ) -> (f32, f32) {
        if moving.is_empty() {
            self.snap_guides.clear();
            return delta;
        }
        let targets = self.snap_targets(moving);
        let snapped = snap::snap_translation(bounds, delta, &targets, self.camera.zoom, lock);
        self.snap_guides = snapped.guides;
        snapped.delta
    }

    /// Every rectangle a moving selection may snap to.
    ///
    /// An object is never a target for itself, and neither is anything inside
    /// it: a child's edges move with the parent, so aligning to them would
    /// fight the move instead of explaining it. Nor is anything the camera
    /// cannot see — an off-canvas object holding a drag in place is a snap the
    /// user cannot see the cause of.
    fn snap_targets(&self, moving: &[ObjectId]) -> Vec<snap::Rect> {
        let moving_nodes: Vec<&NodeId> = moving
            .iter()
            .filter_map(|id| self.session.runtime.object(*id))
            .map(|object| &object.spool_id)
            .collect();
        self.snap_rects_all()
            .into_iter()
            .filter(|(rect, node)| {
                self.camera.sees(*rect)
                    && !moving_nodes
                        .iter()
                        .any(|candidate| self.is_within(node, candidate))
            })
            .map(|(rect, _)| rect)
            .collect()
    }

    /// Every drawn object as a rectangle plus the document node it came from.
    fn snap_rects_all(&self) -> Vec<(snap::Rect, &NodeId)> {
        self.session
            .runtime
            .objects()
            .iter()
            .map(|object| {
                (
                    snap::Rect::new(
                        object.position.x,
                        object.position.y,
                        object.size.width,
                        object.size.height,
                    ),
                    &object.spool_id,
                )
            })
            .collect()
    }

    /// Is `node` inside `ancestor`, at any depth?
    fn is_within(&self, node: &NodeId, ancestor: &NodeId) -> bool {
        let mut cursor = Some(node.clone());
        while let Some(current) = cursor {
            if &current == ancestor {
                return true;
            }
            cursor = self
                .session
                .document
                .structure
                .nodes
                .iter()
                .find(|candidate| candidate.id == current)
                .and_then(|candidate| candidate.parent.clone());
        }
        false
    }

    /// The alignment lines currently holding, for the renderer.
    #[cfg(any(test, debug_assertions))]
    pub fn snap_guides(&self) -> &[snap::Guide] {
        &self.snap_guides
    }

    /// Frame whatever is in the document, or the placeholder scene when the
    /// document is empty.
    ///
    /// This is what `⇧1` means in every product in the corpus, and it is what a
    /// user means by "zoom to fit": show me my document. It is also what
    /// `load_project` calls, so opening a project frames the project.
    pub fn fit_canvas(&mut self) -> bool {
        if self.camera_is_frozen() {
            return false;
        }
        match WorldRect::around(self.session.runtime.objects()) {
            Some(bounds) => self.camera.fit_bounds(bounds),
            None => self.camera.fit(),
        }
        true
    }

    /// Frame the current selection — `⇧2` in Figma.
    ///
    /// Falls back to fitting everything when nothing is selected, because
    /// zooming to nothing has no meaning and silently doing nothing is worse.
    pub fn zoom_to_selection(&mut self) -> bool {
        if self.camera_is_frozen() {
            return false;
        }
        let selected: Vec<DesignObject> = self
            .selection
            .ids()
            .iter()
            .filter_map(|id| self.session.runtime.object(*id).cloned())
            .collect();
        let Some(bounds) = WorldRect::around(&selected) else {
            return self.fit_canvas();
        };
        self.camera.fit_bounds(bounds);
        true
    }

    /// Zoom to 100% — `⇧0` in Figma, `⌘0` in Canva.
    ///
    /// Anchored on the selection when there is one, because that is the thing
    /// the user is looking at; otherwise on the viewport centre.
    pub fn zoom_to_actual_size(&mut self) -> bool {
        if self.camera_is_frozen() {
            return false;
        }
        let anchor = self.selection_bounds_screen();
        match anchor {
            Some(screen) => self.camera.set_zoom_at(1.0, screen),
            None => self.camera.set_zoom_at_center(1.0),
        }
        true
    }

    /// One zoom step in — `+` in Figma, `=` in tldraw.
    ///
    /// Anchored the same way [`Self::zoom_to_actual_size`] is, because the point
    /// of the key is to get closer to the thing being looked at rather than to
    /// the middle of the window.
    pub fn zoom_in(&mut self) -> bool {
        self.zoom_by(ZOOM_STEP)
    }

    /// One zoom step out — `-` in every product in the corpus.
    pub fn zoom_out(&mut self) -> bool {
        self.zoom_by(1.0 / ZOOM_STEP)
    }

    fn zoom_by(&mut self, factor: f32) -> bool {
        if self.camera_is_frozen() {
            return false;
        }
        match self.selection_bounds_screen() {
            Some(screen) => self.camera.zoom_at(factor, screen),
            None => self.camera.set_zoom_at_center(self.camera.zoom * factor),
        }
        true
    }

    /// Where on screen the selection's centre is — but only if it is on screen.
    ///
    /// `None` for a selection scrolled or panned out of view, which is a
    /// different answer from "nothing is selected" and matters for zoom. An
    /// anchor is a screen position: handed the screen coordinates of something
    /// ten thousand units to the left, `zoom_at` faithfully pins *that* world
    /// point under that screen point, so one press of `+` would swing the
    /// viewport across the document to an object the user cannot see. The
    /// viewport centre is the honest anchor for something that is not there.
    fn selection_bounds_screen(&self) -> Option<Point<f32>> {
        let selected: Vec<DesignObject> = self
            .selection
            .ids()
            .iter()
            .filter_map(|id| self.session.runtime.object(*id).cloned())
            .collect();
        let center = WorldRect::around(&selected)?.center();
        let screen = self.camera.world_to_screen(center);
        self.camera.viewport_contains(screen).then_some(screen)
    }

    /// One wheel event, in the terms the camera works in.
    ///
    /// Figma: the wheel scrolls the canvas, `⇧`+wheel scrolls sideways, and
    /// `⌘`/`Ctrl`+wheel (or a trackpad pinch) zooms. Spool accepted the wheel
    /// only as a zoom gesture, which left a trackpad user with no way to pan at
    /// all.
    ///
    /// Named rather than left inline in the listener so that the behaviour is
    /// reachable from a test. The previous test for the `⇧` rule re-implemented
    /// the arithmetic instead of calling this, which meant it would still have
    /// passed with the handler deleted — a test that cannot fail is worse than
    /// no test, because it reads as coverage.
    fn handle_wheel(
        &mut self,
        delta: gpui::ScrollDelta,
        zoom: bool,
        shift: bool,
        cursor: Point<f32>,
    ) -> bool {
        // The guard lives here rather than in the listener, so that the named
        // production path is the whole behaviour: a caller reaching the wheel
        // cannot get the movement without the rule that withholds it.
        if self.workload_controls_input() || self.camera_is_frozen() {
            return false;
        }
        // One line height for both readings, so a wheel notch and a trackpad
        // pixel are converted by the same rule rather than by two literals that
        // happen to agree today.
        let pixels = delta.pixel_delta(gpui_px(WHEEL_LINE_PX));
        if zoom {
            // Only the vertical component means anything to a zoom, so a purely
            // horizontal wheel event zooms by `exp(0)` — which is nothing, and
            // deliberately so rather than by accident.
            let gain = f32::from(pixels.y);
            self.camera.zoom_at((gain * ZOOM_WHEEL_GAIN).exp(), cursor);
            return true;
        }
        let mut screen_delta = point(f32::from(pixels.x), f32::from(pixels.y));
        if shift {
            // `⇧` turns a vertical scroll into a horizontal one, which is what
            // every editor does because a trackpad only scrolls vertically by
            // default.
            //
            // The two components are *summed* rather than swapped. A swap also
            // rewrites the horizontal component, so a genuine diagonal scroll — a
            // trackpad user's two fingers rarely produce a perfectly vertical
            // delta — ends up moving less sideways and more vertically than the
            // user asked for, on the one axis `⇧` is supposed to be suppressing.
            screen_delta = point(screen_delta.x + screen_delta.y, 0.0);
        }
        self.pan_by(screen_delta)
    }

    /// Whether the camera is unavailable because a pointer gesture owns the input.
    ///
    /// One question for every reader. The wheel and the pinch are gated at their
    /// handlers; this is the same rule for the keys, the zoom menu and the
    /// arrow-key pan fallback, because the coordinate frame a gesture measures
    /// against is the one the camera defines — a `+` pressed mid-drag rewrites
    /// the distance already dragged exactly as a pinch does.
    ///
    /// Answered inside the canvas rather than in the shell so that no camera
    /// command can reach the camera ungated from a caller that has never heard
    /// of this rule.
    ///
    /// [`Camera::resize`] is deliberately *not* behind this. A window resize has
    /// to be honoured or the projection stops matching the viewport it is drawing
    /// into, which is a worse failure than a drag that lands oddly; it is left
    /// alone rather than half-fixed here.
    fn camera_is_frozen(&self) -> bool {
        self.pointer_gesture_active()
    }

    /// Pan by a screen-space delta, for wheel scrolling.
    ///
    /// Exposed so the wheel path and the space/middle-drag path share one
    /// camera rule instead of two.
    pub fn pan_by(&mut self, screen_delta: Point<f32>) -> bool {
        if self.camera_is_frozen() {
            return false;
        }
        self.camera.offset.x -= screen_delta.x / self.camera.zoom;
        self.camera.offset.y -= screen_delta.y / self.camera.zoom;
        true
    }

    pub fn set_space_held(&mut self, held: bool) {
        self.space_held = held;
    }

    /// Would a press of this button pan rather than select or transform?
    ///
    /// The middle button is a pan on its own; the left button is one only while
    /// space is held. Split out from [`Self::begin_pan`] because the rule is the
    /// whole of what a stuck space bar breaks — a left-drag that pans instead of
    /// moving an object — and that consequence has to be assertable without a
    /// window and a synthesised `MouseDownEvent`.
    fn pans_with(&self, button: MouseButton) -> bool {
        button == MouseButton::Middle || (button == MouseButton::Left && self.space_held)
    }

    fn begin_pan(&mut self, button: MouseButton, event: &MouseDownEvent, window: &mut Window) {
        if self.workload_controls_input() {
            return;
        }
        let pointer_start = point(f32::from(event.position.x), f32::from(event.position.y));
        if self.begin_pan_from(button, pointer_start) {
            self.capture_pointer(window);
        }
    }

    /// Give the pointer to a pan, from the camera offset it starts at.
    ///
    /// Split from [`Self::begin_pan`] so the policy is reachable from a test: the
    /// mouse event is the only thing the listener has that this does not, and
    /// keeping the event out of it is what lets the interesting half — what
    /// happens to a gesture already in flight — be checked at all.
    ///
    /// A pan starting on top of a live move, resize or creation has to give that
    /// gesture up properly. Assigning `interaction = None` on its own left the
    /// transient geometry it had already written applied to the document with no
    /// history entry behind it and no snapshots left to restore from — an edit the
    /// user could neither see in the undo stack nor get back.
    ///
    /// Abandoning rather than committing is the choice every other interrupting
    /// command makes, and it is the safe one: a pan means the pointer is wanted
    /// for something else, so the drag in flight was not completed.
    fn begin_pan_from(&mut self, button: MouseButton, pointer_start: Point<f32>) -> bool {
        if !self.pans_with(button) {
            return false;
        }
        self.abandon_interaction();
        self.marquee = None;
        self.pan = Some(PanGesture {
            button,
            pointer_start,
            offset_start: self.camera.offset,
        });
        true
    }

    fn begin_text_pointer_selection(
        &mut self,
        screen: Point<f32>,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(edit) = self.text_edit.as_ref() else {
            return false;
        };
        let Some(caret) = self.text_offset_at_screen(edit.id, screen, window) else {
            return false;
        };
        let anchor = if shift {
            selection_anchor(&edit.selected_range, edit.selection_reversed)
        } else {
            caret
        };
        self.set_text_selection(anchor, caret, cx);
        if let Some(edit) = self.text_edit.as_mut() {
            edit.pointer_anchor = Some(anchor);
        }
        self.capture_pointer(window);
        true
    }

    fn update_text_pointer_selection(
        &mut self,
        screen: Point<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(anchor) = self.text_edit.as_ref().and_then(|edit| edit.pointer_anchor) else {
            return false;
        };
        let Some(id) = self.text_edit.as_ref().map(|edit| edit.id) else {
            return false;
        };
        let Some(caret) = self.text_offset_at_screen(id, screen, window) else {
            return false;
        };
        self.set_text_selection(anchor, caret, cx);
        true
    }

    fn text_offset_at_screen(
        &self,
        id: ObjectId,
        screen: Point<f32>,
        window: &mut Window,
    ) -> Option<usize> {
        let edit = self.text_edit.as_ref().filter(|edit| edit.id == id)?;
        let object = self.session.runtime.object(id)?;
        let local = screen_to_object_local(self.camera, screen, object.position);
        text_offset_at_local_point(&edit.editing_text, local, self.camera.zoom, window)
    }

    fn begin_left_interaction(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workload_controls_input() {
            return;
        }
        self.begin_pan(MouseButton::Left, event, window);
        if self.pan.is_some() {
            return;
        }

        let screen = self.cursor_in_viewport(event.position);
        let world = self.camera.screen_to_world(screen);
        self.marquee = None;

        if let Some(editing_id) = self.text_edit.as_ref().map(|edit| edit.id) {
            let inside_editing_object =
                self.session
                    .runtime
                    .object(editing_id)
                    .is_some_and(|object| {
                        world.x >= object.position.x
                            && world.x <= object.position.x + object.size.width
                            && world.y >= object.position.y
                            && world.y <= object.position.y + object.size.height
                    });
            if inside_editing_object {
                if let Some(focus_handle) = &self.focus_handle {
                    window.focus(focus_handle, cx);
                }
                self.begin_text_pointer_selection(screen, event.modifiers.shift, window, cx);
                cx.stop_propagation();
                diagnostics::count("canvas_notify", 1);
                cx.notify();
                return;
            }
            self.commit_text_edit();
        }

        if self.tool == Tool::Text {
            if let Some(id) = self.editable_text_at(world) {
                self.begin_text_edit(id, window, cx);
                self.begin_text_pointer_selection(screen, event.modifiers.shift, window, cx);
                cx.stop_propagation();
                return;
            }
        }
        if self.tool == Tool::Select && event.click_count >= 2 {
            if let Some(id) = self.editable_text_at(world) {
                self.begin_text_edit(id, window, cx);
                self.begin_text_pointer_selection(screen, event.modifiers.shift, window, cx);
                cx.stop_propagation();
                return;
            }
        }

        if let Some(object_type) = self.tool.creates_object() {
            self.interaction = Interaction::PotentialCreate(CreateGesture {
                object_type,
                pointer_start_screen: screen,
                pointer_start_world: world,
                current_world: world,
                moved: false,
            });
            self.capture_pointer(window);
            return;
        }
        if self.tool != Tool::Select {
            return;
        }

        if let Some(handle) = self.hit_test_resize_handle(screen) {
            // The gesture is built from the whole selection, not from
            // `selection.ids()[0]`: the handles belong to the selection's union
            // box, and that box is equally correct when it is one object's box.
            let members = self.selected_snapshots();
            if let Some(bounds) = transform_bounds(&members) {
                self.interaction = Interaction::PotentialResize(ResizeGesture {
                    pointer_start_screen: screen,
                    pointer_start_world: world,
                    members,
                    bounds,
                    handle,
                    suspend_snap: suspends_snap(event.modifiers),
                });
                self.capture_pointer(window);
            }
            return;
        }

        let hierarchy = self.hierarchy();
        if let Some(hit) = self.session.runtime.hit_test(world) {
            // What the pointer landed on and what the gesture acts on are two
            // different objects, and conflating them is the single most
            // disorienting thing a nested canvas can do.
            //
            // A plain click climbs to the topmost drawn ancestor (Figma's depth
            // 0), so clicking a button inside a card selects and drags the card.
            // `Cmd`/`Ctrl` is the documented deep-select in all four products and
            // skips the climb.
            //
            // When the *ancestor* is already selected, the gesture acts on the
            // existing selection rather than collapsing to one object: clicking
            // any part of a selected multi-selection is how a user starts a
            // drag of all of it, and a second click inside a selected frame must
            // drag the frame rather than replace the selection with one child.
            let target = hierarchy.selection_target(hit, deep_select(event.modifiers));
            let target_is_selected = self.selection.contains(target);
            let selected_ids = if target_is_selected {
                self.selection.ids().to_vec()
            } else if event.modifiers.shift {
                hierarchy.normalize(
                    &self
                        .selection
                        .ids()
                        .iter()
                        .copied()
                        .chain(std::iter::once(target))
                        .collect::<Vec<_>>(),
                )
            } else {
                vec![target]
            };
            let objects = selected_ids
                .iter()
                .filter_map(|selected_id| {
                    self.session
                        .runtime
                        .object(*selected_id)
                        .map(|object| ObjectSnapshot {
                            id: *selected_id,
                            geometry: object.geometry(),
                        })
                })
                .collect();
            // Deliberately no `window.focus` here. GPUI already moves focus to a
            // `track_focus` element on mouse-down, and the viewport has one, so
            // an explicit call would only restate it — and an editor shortcut
            // still depends on it, because a key event reaches only the path from
            // the focused node up to the root. What keeps the canvas reachable is
            // the `track_focus` in `render`, which is what the window tests
            // mutate.
            self.interaction = Interaction::PotentialMove(MoveGesture {
                pointer_start_screen: screen,
                pointer_start_world: world,
                objects,
                selected_ids,
                click_selection: if event.modifiers.shift {
                    ClickSelection::Toggle(target)
                } else {
                    ClickSelection::SelectOnly(target)
                },
                suspend_snap: suspends_snap(event.modifiers),
                // Read here, at press, and not later: whether this gesture
                // creates objects is not something that may change halfway
                // through. See `MoveGesture::duplicate`.
                duplicate: event.modifiers.alt,
                duplicates: Vec::new(),
                placements: Vec::new(),
            });
            self.capture_pointer(window);
            return;
        }

        self.interaction = Interaction::None;
        let initial_selection = self.selection.ids().to_vec();
        if !event.modifiers.shift {
            self.selection.click(None, false, &self.hierarchy());
        }
        self.marquee = Some(MarqueeGesture {
            start: world,
            current: world,
            additive: event.modifiers.shift,
            initial_selection,
        });
        self.capture_pointer(window);
        diagnostics::count("canvas_notify", 1);
        cx.notify();
    }

    fn capture_pointer(&self, window: &mut Window) {
        if let Some(hitbox) = self.hitbox.get() {
            window.capture_pointer(hitbox.id);
        }
    }

    /// Track what the pointer is over, and say whether the picture changed.
    ///
    /// Returns false when the hover did not change so the caller can skip a
    /// repaint: a pointer moving over empty canvas, or over the same object,
    /// costs nothing.
    ///
    /// Nothing that is being dragged is hoverable, and neither is a text
    /// editor — an outline flashing on and off as the caret moves would be
    /// noise, not feedback.
    fn update_hover(&mut self, screen: Point<f32>) -> bool {
        let hovered = if self.tool == Tool::Select
            && !self.interaction.is_active()
            && self.pan.is_none()
            && self.marquee.is_none()
            && self.text_edit.is_none()
            && self.hit_test_resize_handle(screen).is_none()
        {
            self.session
                .runtime
                .hit_test(self.camera.screen_to_world(screen))
        } else {
            None
        };
        if hovered == self.hovered {
            return false;
        }
        self.hovered = hovered;
        true
    }

    /// What the pointer is currently over, for the status bar.
    #[cfg(any(test, debug_assertions))]
    pub fn hovered(&self) -> Option<ObjectId> {
        self.hovered
    }

    /// Every selected object, as it is right now.
    fn selected_snapshots(&self) -> Vec<ObjectSnapshot> {
        self.selection
            .ids()
            .iter()
            .filter_map(|id| {
                self.session
                    .runtime
                    .object(*id)
                    .map(|object| ObjectSnapshot {
                        id: *id,
                        geometry: object.geometry(),
                    })
            })
            .collect()
    }

    /// The transform box of the current selection, in world coordinates.
    ///
    /// `None` for an empty selection. This is the single answer to "what does
    /// the selection measure", shared by the handles, the hit test and the
    /// resize gesture, so the box the user can grab is by construction the box
    /// the drag will act on.
    ///
    /// Runtime state, derived on demand: it is never stored on the document,
    /// never serialized, and never a history entry.
    pub fn selection_transform_bounds(&self) -> Option<ObjectGeometry> {
        transform_bounds(&self.selected_snapshots())
    }

    fn hit_test_resize_handle(&self, screen: Point<f32>) -> Option<ResizeHandle> {
        // Hit-tested against the selection's union box, which is where the
        // handles are drawn. Restricting this to a single object is what used to
        // make a multi-selection impossible to resize at all.
        let bounds = self.selection_transform_bounds()?;
        ResizeHandle::ALL.into_iter().find(|handle| {
            let handle_position = handle.screen_position(self.camera, bounds);
            (screen.x - handle_position.x).abs() <= RESIZE_HANDLE_HIT_RADIUS
                && (screen.y - handle_position.y).abs() <= RESIZE_HANDLE_HIT_RADIUS
        })
    }

    #[cfg(any(test, debug_assertions))]
    fn update_interaction(&mut self, screen: Point<f32>) -> bool {
        self.update_interaction_with(screen, false, false)
    }

    /// Advance a gesture, with the live modifier state supplied by the caller.
    ///
    /// `update_interaction` is the modifier-free form the tests drive; the
    /// pointer path passes the real key state so `⇧`-constrain and `⌥`-from-centre
    /// can be sampled live. Both are pure arithmetic, so unlike the `⌥`-duplicate
    /// decision they may legitimately change mid-drag.
    fn update_interaction_with(
        &mut self,
        screen: Point<f32>,
        constrain: bool,
        from_center: bool,
    ) -> bool {
        self.constrain_drag = constrain;
        self.center_drag = from_center;
        let interaction = std::mem::replace(&mut self.interaction, Interaction::None);
        match interaction {
            Interaction::None => false,
            Interaction::PotentialMove(mut gesture) => {
                if !drag_threshold_crossed(gesture.pointer_start_screen, screen) {
                    self.interaction = Interaction::PotentialMove(gesture);
                    return false;
                }
                // The copies are made here, as the drag becomes live, rather than
                // at press. Press is too early: `⌥`-click without a drag must stay
                // a click, and materialising objects on every `⌥`-click would
                // leave a copy behind for a gesture the user abandoned. Past the
                // threshold the user has committed to moving something, which is
                // the same moment a move starts changing the document.
                // Making the copies is a side effect; the answer to "did this
                // become a drag" is that the threshold was crossed, either way.
                if gesture.duplicate {
                    self.duplicate_gesture_selection(&mut gesture);
                }
                self.selection
                    .replace_normalized(gesture.selected_ids.clone(), &self.hierarchy());
                self.drag_gesture_objects(&gesture, screen);
                self.interaction = Interaction::Moving(gesture);
                true
            }
            Interaction::Moving(gesture) => {
                self.drag_gesture_objects(&gesture, screen);
                self.interaction = Interaction::Moving(gesture);
                true
            }
            Interaction::PotentialResize(gesture) => {
                if !drag_threshold_crossed(gesture.pointer_start_screen, screen) {
                    self.interaction = Interaction::PotentialResize(gesture);
                    return false;
                }
                self.drag_resize_object(&gesture, screen);
                self.interaction = Interaction::Resizing(gesture);
                true
            }
            Interaction::Resizing(gesture) => {
                self.drag_resize_object(&gesture, screen);
                self.interaction = Interaction::Resizing(gesture);
                true
            }
            Interaction::PotentialCreate(mut gesture) => {
                if !drag_threshold_crossed(gesture.pointer_start_screen, screen) {
                    gesture.moved = gesture.moved || gesture.pointer_start_screen != screen;
                    self.interaction = Interaction::PotentialCreate(gesture);
                    return false;
                }
                gesture.current_world = self.camera.screen_to_world(screen);
                self.interaction = Interaction::Creating(gesture);
                true
            }
            Interaction::Creating(mut gesture) => {
                gesture.current_world = self.camera.screen_to_world(screen);
                self.interaction = Interaction::Creating(gesture);
                true
            }
        }
    }

    /// Drop every piece of feedback that only exists during a gesture.
    ///
    /// Guides and hover are runtime state with no document meaning, so the one
    /// thing they all need is a single place that ends them. Clearing them at
    /// each call site instead is how a guide ends up outliving the drag that
    /// drew it — a magenta line across the artwork that nothing will remove
    /// until the next gesture happens to overwrite it.
    fn clear_gesture_feedback(&mut self) {
        self.snap_guides.clear();
        self.hovered = None;
        self.constrain_drag = false;
        self.center_drag = false;
    }

    fn finish_interaction(&mut self, screen: Point<f32>) {
        let interaction = std::mem::replace(&mut self.interaction, Interaction::None);
        match interaction {
            Interaction::PotentialMove(gesture) => {
                let hierarchy = self.hierarchy();
                match gesture.click_selection {
                    ClickSelection::SelectOnly(id) => {
                        self.selection.click(Some(id), false, &hierarchy)
                    }
                    ClickSelection::Toggle(id) => self.selection.click(Some(id), true, &hierarchy),
                }
            }
            Interaction::Moving(gesture) => {
                self.drag_gesture_objects(&gesture, screen);
                let geometry = geometry_command(&self.session.runtime, &gesture.objects);
                self.commit_operation(gesture.operation(geometry));
            }
            Interaction::PotentialResize(_) => {}
            Interaction::Resizing(gesture) => {
                self.drag_resize_object(&gesture, screen);
                // One command covering every member, so a multi-selection resize
                // is one history entry and undo restores the whole selection
                // together rather than one object per press.
                let command = geometry_command(&self.session.runtime, &gesture.members);
                self.commit(command);
            }
            Interaction::PotentialCreate(gesture) => {
                if gesture.object_type == ObjectType::Text {
                    // Text has always answered a click, and a drag reaches
                    // `Creating` below instead.
                    self.commit_creation(
                        gesture.object_type,
                        gesture.pointer_start_world,
                        default_creation_size(ObjectType::Text),
                    );
                } else if drag_threshold_crossed(gesture.pointer_start_screen, screen) {
                    let current_world = self.camera.screen_to_world(screen);
                    let geometry = creation_geometry(gesture.pointer_start_world, current_world);
                    self.commit_creation(gesture.object_type, geometry.position, geometry.size);
                } else if !gesture.moved {
                    // A click: pressed and released without moving. The object is
                    // placed at the same point a drag would start from, so the two
                    // share one anchor — its top-left, because
                    // `creation_geometry` puts a drag's top-left at the press.
                    self.commit_creation(
                        gesture.object_type,
                        gesture.pointer_start_world,
                        default_creation_size(gesture.object_type),
                    );
                }
            }
            Interaction::Creating(gesture) => {
                let current_world = self.camera.screen_to_world(screen);
                let geometry = creation_geometry(gesture.pointer_start_world, current_world);
                self.commit_creation(gesture.object_type, geometry.position, geometry.size);
            }
            Interaction::None => {}
        }
        self.clear_gesture_feedback();
    }

    fn commit_creation(
        &mut self,
        object_type: ObjectType,
        position: Point<f32>,
        object_size: Size<f32>,
    ) {
        let text_content = (object_type == ObjectType::Text).then(|| "Type something".to_string());
        let object =
            self.session
                .runtime
                .create_object(object_type, position, object_size, text_content);
        let placement = self.session.runtime.placement(object.id).unwrap();
        self.selection
            .click(Some(object.id), false, &self.hierarchy());
        self.commit_created(vec![placement]);
        // Text keeps its tool: the session the pointer-up handler opens is
        // announced by the tool still being Text, and the tool returns to
        // Selection once that session is open. Every other tool is finished here.
        if object_type != ObjectType::Text {
            self.finish_creation();
        }
    }

    /// Record new objects in the document *and* on the canvas, as one history
    /// entry.
    ///
    /// Two halves, because a created object has two existences. The canvas needs
    /// the shape in order to draw it, and the persistent document needs the node
    /// in order to write it to `lamine.yaml` and bind it to an authored element.
    /// Committing only the runtime half is exactly what made a created object
    /// vanish on reopen. Committing them as one compound is what keeps "one
    /// gesture is one history entry" true now that both halves exist.
    fn commit_created(&mut self, placements: Vec<ObjectPlacement>) {
        let mut operations = Vec::with_capacity(placements.len() * 2);
        for placement in &placements {
            let Some(node) = self.structure_for_new(&placement.object) else {
                continue;
            };
            operations.push(SemanticOperation::Structure(StructureChange::Insert {
                node,
                node_index: self.session.document.structure.nodes.len(),
                child_index: None,
            }));
        }
        // A project that is open but cannot take the node is a document in a
        // state this milestone cannot fix; the object is still drawn, and the save
        // reports it rather than pretending it was written. No project open at all
        // is the starter scene, where there is nothing to persist to and a
        // runtime-only object is the whole truth.
        operations.push(SemanticOperation::Runtime(DocumentCommand::insert(
            placements,
        )));
        self.commit_operation(SemanticOperation::compound(operations));
    }

    /// The persistent node for an object that does not have one yet.
    fn structure_for_new(&self, object: &DesignObject) -> Option<StructuralNode> {
        let (parent, file) = self.creation_site(object)?;
        Some(StructuralNode {
            id: object.spool_id.clone(),
            name: object.name.clone(),
            kind: kind_for(object.object_type).to_owned(),
            parent,
            children: Vec::new(),
            source: SourceBinding {
                file,
                selector: format!("[data-spool-id=\"{}\"]", object.spool_id.as_str()),
            },
        })
    }

    /// Where a newly created object is authored: its parent node and the file its
    /// element goes into.
    ///
    /// Both come from the selected container, which is the same rule the canvas
    /// already uses to decide what a click acts on. A rectangle drawn inside a
    /// frame therefore becomes that frame's child in the document as well as on
    /// the canvas, instead of being a second answer to "where does this belong?".
    fn creation_site(&self, object: &DesignObject) -> Option<(Option<NodeId>, String)> {
        let chosen = self
            .selection
            .ids()
            .first()
            .copied()
            .filter(|id| *id != object.id)
            .and_then(|id| self.session.runtime.object(id))
            .and_then(|parent| {
                self.session
                    .document
                    .structure
                    .nodes
                    .iter()
                    .find(|node| node.id == parent.spool_id)
                    .map(|node| (Some(node.id.clone()), node.source.file.clone()))
            });
        // Nothing suitable selected: the project's top-level frame, which is the
        // container the whole document hangs from.
        chosen.or_else(|| {
            self.session
                .document
                .structure
                .nodes
                .iter()
                .find(|node| node.parent.is_none())
                .map(|root| (Some(root.id.clone()), root.source.file.clone()))
        })
    }

    pub fn delete_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let had_interaction = self.interaction.is_active();
        let changed = self.delete_selected_objects();
        if changed || had_interaction {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
        changed
    }

    fn delete_selected_objects(&mut self) -> bool {
        self.commit_text_edit();
        self.abandon_interaction();
        let ids = self.selection.ids().to_vec();

        // A container takes its contents with it.
        //
        // The authored source is what decides this rather than taste: a child is
        // nested inside its container's element, so removing the container's
        // element already removes the child's element with it. Keeping the child
        // would leave a record in `lamine.yaml` for an element that is no longer
        // in the source — a dangling binding, which is the same class of
        // corruption as the dangling parent this avoids. So the subtree goes, and
        // it goes in post-order so that no node is ever removed while another
        // still names it as parent.
        let doomed = self.subtree_of_selection(&ids);
        let doomed_object_ids: Vec<ObjectId> = doomed
            .iter()
            .filter_map(|node| self.runtime_object_for(node.id.as_str()))
            .collect();
        // Whatever the runtime holds for the selected objects too: a node can be
        // in the structure without a drawn object of its own.
        let mut removed_ids = doomed_object_ids;
        for id in &ids {
            if let Some(node_id) = self.runtime_node_for(*id) {
                if !doomed.iter().any(|node| node.id == node_id) {
                    removed_ids.push(*id);
                }
            }
        }
        let deleted = self.session.runtime.remove_objects(&removed_ids);
        if deleted.is_empty() {
            self.retain_existing_selection();
            return false;
        }

        // The runtime half and the document half, as one entry. The node has to
        // leave `lamine.yaml` and lose its authored element too, or a reopen
        // would bring back an object the user deleted.
        let mut operations = Vec::with_capacity(deleted.len() * 2);
        for node in doomed {
            let Some(index) = self
                .session
                .document
                .structure
                .nodes
                .iter()
                .position(|candidate| candidate.id == node.id)
            else {
                continue;
            };
            operations.push(SemanticOperation::Structure(StructureChange::Remove {
                node,
                node_index: index,
                child_index: None,
            }));
        }
        operations.push(SemanticOperation::Runtime(DocumentCommand::delete(deleted)));
        self.commit_operation(SemanticOperation::compound(operations));
        self.retain_existing_selection();
        true
    }

    /// The structure nodes a deletion of `ids` has to take with it, post-order.
    ///
    /// Every selected node's subtree, deduplicated: selecting a container and
    /// something inside it names the same node twice, and a removal is refused
    /// for a node that is already gone.
    fn subtree_of_selection(&self, ids: &[ObjectId]) -> Vec<StructuralNode> {
        let structure = &self.session.document.structure;
        let mut seen: HashSet<NodeId> = HashSet::new();
        let mut doomed: Vec<StructuralNode> = Vec::new();
        for node in ids.iter().filter_map(|id| self.runtime_node_for(*id)) {
            for candidate in structure.subtree_post_order(&node) {
                if seen.insert(candidate.id.clone()) {
                    doomed.push(candidate);
                }
            }
        }
        doomed
    }

    /// The structure identity behind a runtime object, if it has one.
    fn runtime_node_for(&self, id: ObjectId) -> Option<NodeId> {
        self.session
            .runtime
            .object(id)
            .map(|object| object.spool_id.clone())
    }

    /// The runtime object drawn for a structure node, if it has one.
    ///
    /// A node can exist in `lamine.yaml` with nothing drawn for it, and those
    /// still have to come out of the document.
    fn runtime_object_for(&self, spool_id: &str) -> Option<ObjectId> {
        self.session
            .runtime
            .objects()
            .iter()
            .find(|object| object.spool_id.as_str() == spool_id)
            .map(|object| object.id)
    }

    pub fn duplicate_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let had_interaction = self.interaction.is_active();
        let changed = self.duplicate_selected_objects();
        if changed || had_interaction {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
        changed
    }

    fn duplicate_selected_objects(&mut self) -> bool {
        self.commit_text_edit();
        self.abandon_interaction();
        let ids = self.selection.ids().to_vec();
        let duplicates = self.session.runtime.duplicate_objects(&ids);
        if duplicates.is_empty() {
            self.retain_existing_selection();
            return false;
        }
        let duplicate_ids = duplicates
            .iter()
            .map(|placement| placement.object.id)
            .collect();
        self.selection
            .replace_normalized(duplicate_ids, &self.hierarchy());
        // The same path as creation, because a duplicate is a created object.
        self.commit_created(duplicates);
        true
    }

    fn retain_existing_selection(&mut self) {
        let existing = self
            .selection
            .ids()
            .iter()
            .copied()
            .filter(|id| self.session.runtime.object(*id).is_some())
            .collect();
        self.selection
            .replace_normalized(existing, &self.hierarchy());
    }

    fn cancel_interaction(&mut self) -> bool {
        if !self.abandon_interaction() {
            return false;
        }
        self.clear_gesture_feedback();
        true
    }

    /// Give up whatever the pointer is currently doing.
    ///
    /// Every gesture the pointer can start is covered, because Escape means
    /// "leave the thing I am in the middle of" and the pointer is in the middle
    /// of something for most of a drag. A marquee or a pan is dropped outright
    /// rather than merely abandoned: neither has changed the document, so there
    /// is nothing to restore and nothing to record.
    pub fn cancel_manipulation(&mut self, cx: &mut Context<Self>) -> bool {
        let cancelled = self.cancel_gesture();
        if cancelled {
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
        cancelled
    }

    fn cancel_gesture(&mut self) -> bool {
        let cancelled = self.cancel_interaction();
        // Both are taken, never short-circuited: a marquee and a pan cannot be
        // in flight at once, but a rule that leaves one of them held because the
        // other happened to be `Some` is a rule that depends on which came first.
        let marquee = self.marquee.take().is_some();
        let pan = self.pan.take().is_some();
        cancelled | marquee | pan
    }

    fn finish_marquee(&mut self, screen: Point<f32>) {
        let Some(marquee) = self.marquee.take() else {
            return;
        };
        let end = self.camera.screen_to_world(screen);
        let screen_delta = point(
            (end.x - marquee.start.x).abs() * self.camera.zoom,
            (end.y - marquee.start.y).abs() * self.camera.zoom,
        );
        if screen_delta.x.max(screen_delta.y) < 3.0 {
            if !marquee.additive {
                self.selection.click(None, false, &self.hierarchy());
            }
            return;
        }

        let hierarchy = self.hierarchy();
        let contained = self
            .session
            .runtime
            .objects_in(WorldRect::from_points(marquee.start, end));
        if marquee.additive {
            // Built from the snapshot taken at press, not from whatever the
            // selection holds now, so the run is exactly "what was selected,
            // plus what the marquee caught".
            self.selection
                .replace_normalized(marquee.initial_selection, &hierarchy);
            self.selection.add_all(contained, &hierarchy);
        } else {
            self.selection.replace_normalized(contained, &hierarchy);
        }
    }

    fn cursor_in_viewport(&self, position: Point<Pixels>) -> Point<f32> {
        let origin = self
            .hitbox
            .get()
            .map_or(point(gpui_px(0.0), gpui_px(0.0)), |hitbox| hitbox.origin);
        point(
            f32::from(position.x - origin.x),
            f32::from(position.y - origin.y),
        )
    }

    fn render_grid(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let mut spacing = 32.0;
        while spacing * self.camera.zoom < 24.0 {
            spacing *= 2.0;
        }
        while spacing * self.camera.zoom > 48.0 {
            spacing *= 0.5;
        }
        let min_x = self.camera.offset.x;
        let min_y = self.camera.offset.y;
        let max_x = min_x + self.camera.viewport.width / self.camera.zoom;
        let max_y = min_y + self.camera.viewport.height / self.camera.zoom;
        let first_x = (min_x / spacing).ceil() as i32;
        let last_x = (max_x / spacing).floor() as i32;
        let first_y = (min_y / spacing).ceil() as i32;
        let last_y = (max_y / spacing).floor() as i32;
        let dot_size = (1.5 * self.camera.zoom).clamp(1.0, 2.0);

        for grid_y in first_y..=last_y {
            let y = (grid_y as f32 * spacing - self.camera.offset.y) * self.camera.zoom;
            for grid_x in first_x..=last_x {
                let x = (grid_x as f32 * spacing - self.camera.offset.x) * self.camera.zoom;
                let dot_bounds = Bounds {
                    origin: point(bounds.origin.x + gpui_px(x), bounds.origin.y + gpui_px(y)),
                    size: size(gpui_px(dot_size), gpui_px(dot_size)),
                };
                window.paint_quad(gpui::fill(dot_bounds, rgb(0x171a1e)));
            }
        }
    }
}

impl Render for CanvasView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(debug_assertions)]
        self.start_workload(_window, cx);
        diagnostics::count("canvas_render", 1);
        diagnostics::count(
            "viewport_width_sum_px",
            self.camera.viewport.width.round() as u64,
        );
        diagnostics::count(
            "viewport_height_sum_px",
            self.camera.viewport.height.round() as u64,
        );
        diagnostics::count(
            "camera_zoom_sum_milli",
            (self.camera.zoom * 1000.0).round() as u64,
        );
        let render_start = diagnostics::start();
        let entity = cx.entity();
        let entity_for_prepaint = entity.clone();
        let entity_for_paint = entity.clone();
        let hitbox_slot = self.hitbox.clone();
        let current_camera = self.camera;
        let document = &self.session.runtime;
        let selection = &self.selection;
        let marquee = self.marquee.clone();
        let preview = self.interaction.preview();
        let text_edit = self.text_edit.clone();
        let focus_handle = self.focus_handle.clone();
        let input_entity = entity.clone();
        let mut viewport = div()
            .id("canvas-viewport")
            // GPUI's own hook for locating an element's bounds from a test, and a
            // no-op in release builds. It is what lets the window-level
            // interaction tests click the object they mean to click instead of
            // guessing a coordinate — see `src/interaction_window_tests.rs`.
            .debug_selector(|| "canvas-viewport".to_string())
            .relative()
            .flex_1()
            .overflow_hidden()
            .bg(rgb(theme::CANVAS))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, window, _| {
                    this.begin_pan(MouseButton::Middle, event, window);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.begin_left_interaction(event, window, cx);
                }),
            )
            .on_action(cx.listener(Self::text_backspace))
            .on_action(cx.listener(Self::text_delete))
            .on_action(cx.listener(Self::text_left))
            .on_action(cx.listener(Self::text_right))
            .on_action(cx.listener(Self::text_select_left))
            .on_action(cx.listener(Self::text_select_right))
            .on_action(cx.listener(Self::text_select_all))
            .on_action(cx.listener(Self::text_home))
            .on_action(cx.listener(Self::text_end))
            .on_action(cx.listener(Self::text_paste))
            .on_action(cx.listener(Self::text_copy))
            .on_action(cx.listener(Self::text_cut))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                let cursor = this.cursor_in_viewport(event.position);
                if this.handle_wheel(
                    event.delta,
                    event.modifiers.control || event.modifiers.platform,
                    event.modifiers.shift,
                    cursor,
                ) {
                    diagnostics::count("canvas_notify", 1);
                    cx.notify();
                }
            }))
            .on_pinch(cx.listener(|this, event: &PinchEvent, _, cx| {
                // A trackpad pinch is the easy way to zoom without noticing you
                // did it mid-drag, so this is the case that matters — and it asks
                // the same question the wheel now asks.
                if this.workload_controls_input() || this.camera_is_frozen() {
                    return;
                }
                let cursor = this.cursor_in_viewport(event.position);
                this.camera.zoom_at(1.0 + event.delta, cursor);
                diagnostics::count("canvas_notify", 1);
                cx.notify();
            }))
            .child(
                gpui::canvas(
                    move |bounds, window, cx| {
                        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                        hitbox_slot.set(Some(CanvasHitbox {
                            id: hitbox.id,
                            origin: bounds.origin,
                        }));
                        entity_for_prepaint.update(cx, |this, cx| {
                            let viewport =
                                size(f32::from(bounds.size.width), f32::from(bounds.size.height));
                            if this.camera.resize(viewport) {
                                diagnostics::count("camera_resize_notify", 1);
                                diagnostics::count("canvas_notify", 1);
                                cx.notify();
                            }
                        });
                        hitbox
                    },
                    move |bounds, hitbox, window, cx| {
                        let view_state = entity_for_paint.read(cx);
                        if !view_state.workload_controls_input()
                            && view_state.pointer_gesture_active()
                        {
                            window.capture_pointer(hitbox.id);
                        }
                        view_state.render_grid(bounds, window);
                        let view = entity_for_paint.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            view.update(cx, |this, cx| {
                                if this.workload_controls_input() {
                                    return;
                                }
                                if let Some(pan) = this.pan {
                                    this.camera.pan_from(
                                        pan.offset_start,
                                        pan.pointer_start,
                                        point(
                                            f32::from(event.position.x),
                                            f32::from(event.position.y),
                                        ),
                                    );
                                    diagnostics::count("canvas_notify", 1);
                                    cx.notify();
                                } else if this
                                    .text_edit
                                    .as_ref()
                                    .is_some_and(|edit| edit.pointer_anchor.is_some())
                                {
                                    let screen = this.cursor_in_viewport(event.position);
                                    this.update_text_pointer_selection(screen, window, cx);
                                } else if this.interaction.is_active() {
                                    let screen = this.cursor_in_viewport(event.position);
                                    if this.update_interaction_with(
                                        screen,
                                        event.modifiers.shift,
                                        event.modifiers.alt,
                                    ) {
                                        diagnostics::count("canvas_notify", 1);
                                        cx.notify();
                                    }
                                } else if this.marquee.is_some() {
                                    let screen = this.cursor_in_viewport(event.position);
                                    let world = this.camera.screen_to_world(screen);
                                    if let Some(marquee) = this.marquee.as_mut() {
                                        marquee.current = world;
                                    }
                                    diagnostics::count("canvas_notify", 1);
                                    cx.notify();
                                } else if this.update_hover(this.cursor_in_viewport(event.position))
                                {
                                    diagnostics::count("canvas_notify", 1);
                                    cx.notify();
                                }
                            });
                        });
                        let view = entity_for_paint.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            view.update(cx, |this, cx| {
                                if this.workload_controls_input() {
                                    return;
                                }
                                if event.button == MouseButton::Left
                                    && this
                                        .text_edit
                                        .as_ref()
                                        .is_some_and(|edit| edit.pointer_anchor.is_some())
                                {
                                    let screen = this.cursor_in_viewport(event.position);
                                    this.update_text_pointer_selection(screen, window, cx);
                                    if let Some(edit) = this.text_edit.as_mut() {
                                        edit.pointer_anchor = None;
                                    }
                                    diagnostics::count("canvas_notify", 1);
                                    cx.notify();
                                } else if this.pan.is_some_and(|pan| pan.button == event.button) {
                                    this.pan = None;
                                    diagnostics::count("canvas_notify", 1);
                                    cx.notify();
                                } else if event.button == MouseButton::Left
                                    && this.interaction.is_active()
                                {
                                    let screen = this.cursor_in_viewport(event.position);
                                    this.finish_interaction(screen);
                                    if this.tool == Tool::Text {
                                        if let Some(id) = this.selection.ids().last().copied() {
                                            if this.session.runtime.object(id).is_some_and(
                                                |object| object.object_type == ObjectType::Text,
                                            ) {
                                                this.begin_text_edit(id, window, cx);
                                                // The session is open, so the
                                                // creation is over: back to the
                                                // Selection tool like every
                                                // other one.
                                                this.tool = Tool::Select;
                                            }
                                        }
                                    }
                                    diagnostics::count("canvas_notify", 1);
                                    cx.notify();
                                } else if event.button == MouseButton::Left
                                    && this.marquee.is_some()
                                {
                                    let screen = this.cursor_in_viewport(event.position);
                                    this.finish_marquee(screen);
                                    diagnostics::count("canvas_notify", 1);
                                    cx.notify();
                                }
                            });
                        });
                    },
                )
                .absolute()
                .top(gpui_px(0.0))
                .left(gpui_px(0.0))
                .right(gpui_px(0.0))
                .bottom(gpui_px(0.0)),
            )
            .child(artboards(
                current_camera,
                document,
                selection,
                marquee,
                preview,
                GestureFeedback {
                    snap_guides: self.snap_guides.clone(),
                    hovered: self.hovered,
                },
                TextInputRenderContext {
                    edit: text_edit,
                    focus_handle: focus_handle.clone(),
                    entity: input_entity,
                },
            ));
        if let Some(focus_handle) = focus_handle.as_ref() {
            viewport = viewport.track_focus(focus_handle);
        }
        diagnostics::record("canvas_render_build", render_start);
        viewport
    }
}

/// The runtime-only things an in-flight gesture is drawing.
///
/// Grouped rather than passed as two more parameters because they share a
/// lifetime exactly: both exist only while something is being dragged, and
/// neither means anything once it is over.
#[derive(Clone, Debug, Default)]
struct GestureFeedback {
    snap_guides: Vec<snap::Guide>,
    hovered: Option<ObjectId>,
}

struct TextInputRenderContext {
    edit: Option<TextEditState>,
    focus_handle: Option<FocusHandle>,
    entity: Entity<CanvasView>,
}

struct CanvasTextInput {
    view: Entity<CanvasView>,
    focus_handle: FocusHandle,
    text: String,
    selection: Range<usize>,
    cursor: usize,
    zoom: f32,
}

struct CanvasTextPrepaint {
    lines: Vec<(gpui::ShapedLine, usize)>,
    selection: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for CanvasTextInput {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for CanvasTextInput {
    type RequestLayoutState = ();
    type PrepaintState = CanvasTextPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let text_style = window.text_style();
        let font_size = gpui_px(14.0 * self.zoom);
        let line_height = gpui_px(18.0 * self.zoom);
        let mut lines = Vec::new();
        let mut selection = Vec::new();
        let mut line_start = 0;
        let mut cursor_quad = None;

        for (line_index, line_text) in self.text.split('\n').enumerate() {
            let shared_text = SharedString::from(line_text.to_owned());
            let run = TextRun {
                len: line_text.len(),
                font: text_style.font(),
                color: rgb(theme::TEXT).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line = window
                .text_system()
                .shape_line(shared_text, font_size, &[run], None);
            let line_end = line_start + line_text.len();
            let line_y = bounds.top() + line_height * line_index as f32;

            if !self.selection.is_empty() {
                let start = self.selection.start.max(line_start).min(line_end);
                let end = self.selection.end.max(line_start).min(line_end);
                if start < end {
                    selection.push(fill(
                        Bounds::from_corners(
                            point(bounds.left() + line.x_for_index(start - line_start), line_y),
                            point(
                                bounds.left() + line.x_for_index(end - line_start),
                                line_y + line_height,
                            ),
                        ),
                        rgba(0x553d74c8),
                    ));
                }
            } else if self.cursor >= line_start && self.cursor <= line_end {
                let x = bounds.left() + line.x_for_index(self.cursor - line_start);
                cursor_quad = Some(fill(
                    Bounds::new(point(x, line_y), size(gpui_px(1.5), line_height)),
                    rgb(theme::ACCENT),
                ));
            }

            lines.push((line, line_index));
            line_start = line_end + 1;
        }

        CanvasTextPrepaint {
            lines,
            selection,
            cursor: cursor_quad,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        for selection in prepaint.selection.drain(..) {
            window.paint_quad(selection);
        }
        let line_height = gpui_px(18.0 * self.zoom);
        for (line, line_index) in prepaint.lines.drain(..) {
            let origin = point(
                bounds.left(),
                bounds.top() + line_height * line_index as f32,
            );
            let _ = line.paint(origin, line_height, gpui::TextAlign::Left, None, window, cx);
        }
        if self.focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }
    }
}

impl EntityInputHandler for CanvasView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let edit = self.text_edit.as_ref()?;
        let range = utf16_range_to_utf8(&edit.editing_text, range_utf16);
        adjusted_range.replace(utf8_range_to_utf16(&edit.editing_text, range.clone()));
        Some(edit.editing_text[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let edit = self.text_edit.as_ref()?;
        Some(UTF16Selection {
            range: utf8_range_to_utf16(&edit.editing_text, edit.selected_range.clone()),
            reversed: edit.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let edit = self.text_edit.as_ref()?;
        edit.marked_range
            .as_ref()
            .map(|range| utf8_range_to_utf16(&edit.editing_text, range.clone()))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(edit) = self.text_edit.as_mut() {
            edit.marked_range = None;
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_editing_text(range, text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_editing_text(range, new_text, cx);
        if let Some(edit) = self.text_edit.as_mut() {
            let marked_end = edit.selected_range.end;
            let marked_start = marked_end.saturating_sub(new_text.len());
            edit.marked_range = (!new_text.is_empty()).then_some(marked_start..marked_end);
            if let Some(selected) = new_selected_range {
                let selected = utf16_range_to_utf8(new_text, selected);
                edit.selected_range = marked_start + selected.start..marked_start + selected.end;
            }
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let edit = self.text_edit.as_ref()?;
        let range = utf16_range_to_utf8(&edit.editing_text, range_utf16);
        let prefix = &edit.editing_text[..range.start];
        let line_index = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        let column = prefix[line_start..].chars().count();
        let line_height = gpui_px(18.0 * self.camera.zoom);
        let x = element_bounds.left() + gpui_px(column as f32 * 8.0 * self.camera.zoom);
        let y = element_bounds.top() + line_height * line_index as f32;
        Some(Bounds::new(point(x, y), size(gpui_px(1.5), line_height)))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(edit) = self.text_edit.as_mut() {
            edit.selected_range = utf16_range_to_utf8(&edit.editing_text, range_utf16);
            edit.selection_reversed = false;
            diagnostics::count("canvas_notify", 1);
            cx.notify();
        }
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.text_edit.as_ref()?.editing_text.encode_utf16().count())
    }

    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.text_edit.is_some()
    }
}

fn screen_to_object_local(
    camera: Camera,
    screen: Point<f32>,
    object_position: Point<f32>,
) -> Point<f32> {
    let world = camera.screen_to_world(screen);
    point(
        (world.x - object_position.x) * camera.zoom,
        (world.y - object_position.y) * camera.zoom,
    )
}

fn selection_from_anchor_and_caret(
    text: &str,
    anchor: usize,
    caret: usize,
) -> (Range<usize>, bool) {
    let anchor = utf8_boundary(text, anchor.min(text.len()));
    let caret = utf8_boundary(text, caret.min(text.len()));
    (anchor.min(caret)..anchor.max(caret), caret < anchor)
}

fn selection_anchor(range: &Range<usize>, reversed: bool) -> usize {
    if reversed {
        range.end
    } else {
        range.start
    }
}

fn nearest_boundary_from_positions(text: &str, positions: &[(usize, f32)], x: f32) -> usize {
    positions
        .iter()
        .filter(|(index, _)| text.is_char_boundary(*index))
        .min_by(|(left_index, left_x), (right_index, right_x)| {
            (left_x - x)
                .abs()
                .total_cmp(&(right_x - x).abs())
                .then_with(|| left_index.cmp(right_index))
        })
        .map_or(0, |(index, _)| *index)
}

fn text_offset_at_local_point(
    text: &str,
    local: Point<f32>,
    zoom: f32,
    window: &mut Window,
) -> Option<usize> {
    let line_height = 18.0 * zoom;
    let lines = text.split('\n').collect::<Vec<_>>();
    let line_index = if line_height <= 0.0 {
        0
    } else {
        ((local.y / line_height).round() as isize).clamp(0, lines.len() as isize - 1) as usize
    };
    let text_style = window.text_style();
    let line_text = lines[line_index];
    let run = TextRun {
        len: line_text.len(),
        font: text_style.font(),
        color: rgb(theme::TEXT).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(
        SharedString::from(line_text.to_owned()),
        gpui_px(14.0 * zoom),
        &[run],
        None,
    );
    let positions = line_text
        .char_indices()
        .map(|(index, _)| (index, f32::from(line.x_for_index(index))))
        .chain(std::iter::once((
            line_text.len(),
            f32::from(line.x_for_index(line_text.len())),
        )))
        .collect::<Vec<_>>();
    let line_offset = lines[..line_index]
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>();
    Some(line_offset + nearest_boundary_from_positions(line_text, &positions, local.x))
}

fn utf8_boundary(text: &str, offset: usize) -> usize {
    let mut boundary = offset.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn utf16_to_utf8(text: &str, offset: usize) -> usize {
    let mut utf16_offset = 0;
    let mut utf8_offset = 0;
    for character in text.chars() {
        if utf16_offset + character.len_utf16() > offset {
            break;
        }
        utf16_offset += character.len_utf16();
        utf8_offset += character.len_utf8();
    }
    utf8_offset
}

fn utf8_to_utf16(text: &str, offset: usize) -> usize {
    text[..utf8_boundary(text, offset)].encode_utf16().count()
}

fn utf16_range_to_utf8(text: &str, range: Range<usize>) -> Range<usize> {
    let length = text.encode_utf16().count();
    let start = range.start.min(length);
    let end = range.end.min(length).max(start);
    utf16_to_utf8(text, start)..utf16_to_utf8(text, end)
}

fn utf8_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    utf8_to_utf16(text, range.start)..utf8_to_utf16(text, range.end)
}

fn previous_char_boundary(text: &str, offset: usize) -> usize {
    text[..utf8_boundary(text, offset)]
        .char_indices()
        .last()
        .map_or(0, |(index, _)| index)
}

fn next_char_boundary(text: &str, offset: usize) -> usize {
    let offset = utf8_boundary(text, offset);
    text[offset..]
        .chars()
        .next()
        .map_or(text.len(), |character| offset + character.len_utf8())
}

fn render_text_input(
    camera: Camera,
    object: &DesignObject,
    edit: &TextEditState,
    focus_handle: FocusHandle,
    view: Entity<CanvasView>,
    zoom: f32,
) -> impl IntoElement {
    let origin = camera.world_to_screen(object.position);
    div()
        .absolute()
        .left(gpui_px(origin.x))
        .top(gpui_px(origin.y))
        .w(gpui_px(object.size.width * zoom))
        .h(gpui_px(object.size.height * zoom))
        .overflow_hidden()
        .track_focus(&focus_handle)
        .child(CanvasTextInput {
            view,
            focus_handle,
            text: edit.editing_text.clone(),
            selection: edit.selected_range.clone(),
            cursor: if edit.selection_reversed {
                edit.selected_range.start
            } else {
                edit.selected_range.end
            },
            zoom,
        })
}

/// Phase 14 construction decision for one document object.
///
/// The actively edited text object is always constructed so its editing
/// overlay, focus tracking and caret stay coherent regardless of camera
/// position. Everything else is gated by the padded viewport predicate.
///
/// Selection is deliberately not a parameter: selection outlines and resize
/// handles are interaction chrome constructed in their own loops inside
/// `artboards`, independent of the base element, so culling an unselected or
/// selected object's base element never removes its chrome and never requires
/// retaining it.
fn should_construct(
    camera: Camera,
    object: &DesignObject,
    editing_object: Option<ObjectId>,
) -> bool {
    editing_object == Some(object.id) || camera.affects_viewport(object.position, object.size)
}

fn artboards(
    camera: Camera,
    document: &Document,
    selection: &Selection,
    marquee: Option<MarqueeGesture>,
    preview: Option<CreationPreview>,
    feedback: GestureFeedback,
    text_input: TextInputRenderContext,
) -> impl IntoElement {
    // Keep the diagnostic-only visibility scan outside the construction timer.
    let visibility_start = diagnostics::start();
    let visible = if diagnostics::enabled() {
        document
            .objects()
            .iter()
            .filter(|object| {
                diagnostics::intersects(
                    object.position,
                    object.size,
                    camera.offset,
                    camera.viewport,
                    camera.zoom,
                )
            })
            .count() as u64
    } else {
        0
    };
    diagnostics::record("visibility_scan", visibility_start);
    let build_start = diagnostics::start();
    let zoom = camera.zoom;
    let text_edit = text_input.edit;
    let focus_handle = text_input.focus_handle;
    let input_entity = text_input.entity;
    let mut world = div()
        .absolute()
        .top(gpui_px(0.0))
        .left(gpui_px(0.0))
        .right(gpui_px(0.0))
        .bottom(gpui_px(0.0))
        .overflow_hidden();

    let editing_object = text_edit.as_ref().map(|edit| edit.id);
    let mut constructed = 0u64;
    for object in document
        .objects()
        .iter()
        .filter(|object| should_construct(camera, object, editing_object))
    {
        constructed += 1;
        let content = match object.id {
            ObjectId::LANDING => Some(landing_frame(zoom, object.size).into_any_element()),
            ObjectId::EDITOR => Some(editor_frame(zoom, object.size).into_any_element()),
            ObjectId::FEATURES => Some(features_frame(zoom, object.size).into_any_element()),
            ObjectId::MOBILE => Some(mobile_frame(zoom, object.size).into_any_element()),
            _ => None,
        };
        if let Some(content) = content {
            world = world.child(positioned_frame(camera, object, content, zoom));
        } else {
            let editing_this_object = editing_object == Some(object.id);
            world = world.child(render_object(camera, object, zoom, editing_this_object));
            if editing_this_object {
                if let (Some(edit), Some(focus_handle)) =
                    (text_edit.as_ref(), focus_handle.as_ref())
                {
                    world = world.child(render_text_input(
                        camera,
                        object,
                        edit,
                        focus_handle.clone(),
                        input_entity.clone(),
                        zoom,
                    ));
                }
            }
        }
    }

    if let Some(preview) = preview {
        world = world.child(render_preview(camera, preview, zoom));
    }

    // Hover sits *under* selection so that hovering something already selected
    // does not change its appearance at all — the selection outline is the
    // stronger statement and should win.
    if let Some(hovered) = feedback.hovered.filter(|id| !selection.contains(*id)) {
        if let Some(object) = document.object(hovered) {
            world = world.child(hover_outline(camera, object));
        }
    }
    // Selection chrome is drawn from the *transform box*: one outline and one set
    // of handles for a single object, and one outline and one set of handles
    // around the union for several. Per-object outlines were fine while a
    // multi-selection could not be resized, because the handles only ever
    // appeared for one object; with a union box to grab, eight outlines would be
    // eight sets of chrome disagreeing about what is selected.
    //
    // A multi-selection shows only the union, not the union *and* its members:
    // the union is the thing that will move and resize, so it is the only outline
    // that predicts what the pointer will do.
    match selection_chrome(document, selection) {
        Some(Chrome::Union(bounds)) => {
            world = world.child(selection_outline(camera, bounds));
            for handle in ResizeHandle::ALL {
                world = world.child(resize_handle_element(camera, bounds, handle));
            }
        }
        Some(Chrome::Members(ids)) => {
            for id in ids {
                if let Some(object) = document.object(id) {
                    world = world.child(selection_outline(camera, object.geometry()));
                }
            }
            // The handles still belong to the selection's box, which for a single
            // object is that object's own box.
            if let Some(bounds) = transform_bounds_of_ids(document, selection.ids()) {
                for handle in ResizeHandle::ALL {
                    world = world.child(resize_handle_element(camera, bounds, handle));
                }
            }
        }
        None => {}
    }

    // Guides last, so nothing in the scene can paint over the explanation of a
    // move that is happening right now.
    for guide in feedback.snap_guides {
        world = world.child(render_snap_guide(camera, guide, zoom));
    }

    if let Some(marquee) = marquee {
        let bounds = WorldRect::from_points(marquee.start, marquee.current);
        let origin = camera.world_to_screen(bounds.min);
        let marquee_size = size(
            (bounds.max.x - bounds.min.x) * zoom,
            (bounds.max.y - bounds.min.y) * zoom,
        );
        world = world.child(
            div()
                .absolute()
                .left(gpui_px(origin.x))
                .top(gpui_px(origin.y))
                .w(gpui_px(marquee_size.width))
                .h(gpui_px(marquee_size.height))
                .border_1()
                .border_color(rgb(theme::ACCENT)),
        );
    }

    diagnostics::record("canvas_elements", build_start);
    let considered = document.objects().len() as u64;
    diagnostics::count("objects_considered", considered);
    diagnostics::count("objects_constructed", constructed);
    diagnostics::count("objects_culled", considered - constructed);
    diagnostics::count("geometry_intersecting", visible);
    diagnostics::count("geometry_offscreen", considered - visible);
    world
}

fn render_object(
    camera: Camera,
    object: &DesignObject,
    zoom: f32,
    hide_editing_text: bool,
) -> impl IntoElement {
    let origin = camera.world_to_screen(object.position);
    let mut body = div()
        .absolute()
        .left(gpui_px(origin.x))
        .top(gpui_px(origin.y))
        .w(px!(object.size.width, zoom))
        .h(px!(object.size.height, zoom));
    // Authored `opacity`, applied to the whole object the way CSS applies it:
    // the element and everything it contains, not just its paint.
    if object.opacity < 1.0 {
        body = body.opacity(object.opacity.clamp(0.0, 1.0));
    }
    if object.object_type != ObjectType::Text {
        // Authored `border-radius`. Applied before the ellipse case below so a
        // round shape still wins over a rectangular radius.
        if object.border_radius > 0.0 {
            body = body.rounded(gpui_px(object.border_radius * zoom));
        }
        if let Some(fill) = object.fill {
            body = body.bg(rgb(fill.color.to_rgb()));
        }
        if let Some(stroke) = object.stroke {
            body = body
                .border_1()
                .border_color(rgb(stroke.color.to_rgb()))
                .border_t(gpui_px(stroke.width * zoom))
                .border_b(gpui_px(stroke.width * zoom))
                .border_l(gpui_px(stroke.width * zoom))
                .border_r(gpui_px(stroke.width * zoom));
        }
    }
    match object.object_type {
        ObjectType::Frame => {
            body = body.child(
                div()
                    .absolute()
                    .left(gpui_px(0.0))
                    .top(gpui_px(-LABEL_HEIGHT * zoom))
                    .text_size(px!(12.0, zoom))
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child(object.name.clone()),
            );
        }
        ObjectType::Rectangle => {}
        ObjectType::Ellipse => {
            body = body.rounded_full();
        }
        ObjectType::Text if !hide_editing_text => {
            body = body
                .flex()
                .items_start()
                .text_size(px!(text_size_of(object), zoom))
                .text_color(ink_of(object))
                .child(object.text_content.clone().unwrap_or_default());
        }
        ObjectType::Text => {}
    }

    // Authored text is drawn for any object that has it, not only for objects
    // typed as text. Metadata declares a button as `frame` while its element
    // is an `<a>` with a label; hiding that label would make a correctly
    // loaded project look emptier than its source.
    if let Some(text) = object.text_content.as_ref().filter(|t| !t.is_empty()) {
        if object.object_type != ObjectType::Text {
            body = body
                .flex()
                .items_start()
                .text_size(px!(text_size_of(object), zoom))
                .text_color(ink_of(object))
                .child(text.clone());
        }
    }
    body
}

/// Text size for an object: the authored one when source declared it.
///
/// Provisional: an authored size is applied as the renderer font size, with no
/// line-height model and no scaling against the element's own box.
fn text_size_of(object: &DesignObject) -> f32 {
    object.font_size.unwrap_or(14.0)
}

/// Text colour for an object: authored first, contrast guess second.
///
/// The authored `color` wins because source is authoritative. Only when no
/// colour was authored does the renderer choose one, because an unreadable
/// label is worse than an arbitrary choice.
fn ink_of(object: &DesignObject) -> gpui::Rgba {
    if let Some(color) = object.text_color {
        return rgb(color.to_rgb());
    }
    object
        .fill
        .map(|fill| rgb(contrasting_ink(fill.color)))
        .unwrap_or_else(|| rgb(theme::TEXT))
}

/// Pick black or white text for legibility against a background.
///
/// Provisional: the authored `color` should win once style resolution is
/// plumbed through to the renderer.
fn contrasting_ink(background: Color) -> u32 {
    let luminance = 0.299 * background.red as f32
        + 0.587 * background.green as f32
        + 0.114 * background.blue as f32;
    if luminance > 140.0 {
        0x1a1a1a
    } else {
        0xffffff
    }
}

fn render_preview(camera: Camera, preview: CreationPreview, zoom: f32) -> impl IntoElement {
    let origin = camera.world_to_screen(preview.geometry.position);
    let mut body = div()
        .absolute()
        .left(gpui_px(origin.x))
        .top(gpui_px(origin.y))
        .w(px!(preview.geometry.size.width, zoom))
        .h(px!(preview.geometry.size.height, zoom))
        .bg(rgb(theme::SURFACE_RAISED))
        .border_1()
        .border_color(rgb(theme::ACCENT));
    if preview.object_type == ObjectType::Ellipse {
        body = body.rounded_full();
    }
    body
}

fn positioned_frame(
    camera: Camera,
    object: &DesignObject,
    content: impl IntoElement,
    zoom: f32,
) -> impl IntoElement {
    let origin = camera.world_to_screen(object.position);
    div()
        .absolute()
        .left(gpui_px(origin.x))
        .top(gpui_px(origin.y))
        .child(
            div()
                .absolute()
                .left(gpui_px(0.0))
                .top(gpui_px(-LABEL_HEIGHT * zoom))
                .text_size(px!(12.0, zoom))
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(format!("{:02}  /  {}", object.id.0, object.name)),
        )
        .child(
            div()
                .absolute()
                .left(gpui_px(0.0))
                .top(gpui_px(0.0))
                .child(content),
        )
}

/// What selection chrome to draw, and where.
///
/// Split out from [`artboards`] so the decision can be tested without a window:
/// the element builders need a GPUI context, but *which* box gets outlined and
/// whether the members are outlined individually is the part with a rule in it,
/// and a rule that cannot be tested is a rule that will quietly change.
#[derive(Debug)]
enum Chrome {
    /// One outline and one set of handles around the selection's union box.
    Union(ObjectGeometry),
    /// One outline per member, for a selection of a single object — where the
    /// union box and the object are the same rectangle, so this is not a
    /// different picture, just the one that does not compute a union to draw it.
    Members(Vec<ObjectId>),
}

fn selection_chrome(document: &Document, selection: &Selection) -> Option<Chrome> {
    let bounds = transform_bounds_of_ids(document, selection.ids())?;
    Some(if selection.ids().len() > 1 {
        Chrome::Union(bounds)
    } else {
        Chrome::Members(selection.ids().to_vec())
    })
}

/// The selection outline for a box.
///
/// Takes the transform box rather than a drawn object, so the same function
/// outlines a single object and the union box of a multi-selection.
fn selection_outline(camera: Camera, box_geometry: ObjectGeometry) -> impl IntoElement {
    let origin = camera.world_to_screen(box_geometry.position);
    div()
        .absolute()
        .left(gpui_px(origin.x))
        .top(gpui_px(origin.y))
        .w(gpui_px(box_geometry.size.width * camera.zoom))
        .h(gpui_px(box_geometry.size.height * camera.zoom))
        .border_1()
        .border_color(rgb(theme::ACCENT))
}

/// The outline for an object under the pointer that is not selected.
///
/// Deliberately weaker than the selection outline: thinner and dimmer, so hover
/// answers "is this clickable?" without claiming the user has chosen it. No
/// fill, because a fill would hide the object's own colours at the exact moment
/// the user is comparing them to something else.
fn hover_outline(camera: Camera, object: &DesignObject) -> impl IntoElement {
    let origin = camera.world_to_screen(object.position);
    div()
        .absolute()
        .left(gpui_px(origin.x))
        .top(gpui_px(origin.y))
        .w(gpui_px(object.size.width * camera.zoom))
        .h(gpui_px(object.size.height * camera.zoom))
        .border_1()
        .border_color(rgba((theme::ACCENT & 0x00ff_ffff) | (0x66 << 24)))
}

/// The line that explains a snap.
///
/// One screen pixel wide at any zoom — a guide that grows with the world is a
/// wall, and one that shrinks is invisible exactly when the user is zoomed in
/// trying to see it. Magenta is Figma's choice and this is that convention: a
/// colour no object in the document is likely to be, so it reads as editor
/// chrome rather than as content.
fn render_snap_guide(camera: Camera, guide: snap::Guide, zoom: f32) -> impl IntoElement {
    let thickness = 1.0;
    match guide.axis {
        snap::Axis::Vertical => {
            let x = (guide.at - camera.offset.x) * zoom;
            let top = (guide.start - camera.offset.y) * zoom;
            let height = (guide.end - guide.start) * zoom;
            div()
                .absolute()
                .left(gpui_px(x - thickness / 2.0))
                .top(gpui_px(top))
                .w(gpui_px(thickness))
                .h(gpui_px(height.max(thickness)))
                .bg(rgb(theme::SNAP_GUIDE))
        }
        snap::Axis::Horizontal => {
            let y = (guide.at - camera.offset.y) * zoom;
            let left = (guide.start - camera.offset.x) * zoom;
            let width = (guide.end - guide.start) * zoom;
            div()
                .absolute()
                .left(gpui_px(left))
                .top(gpui_px(y - thickness / 2.0))
                .w(gpui_px(width.max(thickness)))
                .h(gpui_px(thickness))
                .bg(rgb(theme::SNAP_GUIDE))
        }
    }
}

fn resize_handle_element(
    camera: Camera,
    box_geometry: ObjectGeometry,
    handle: ResizeHandle,
) -> impl IntoElement {
    let position = handle.screen_position(camera, box_geometry);
    let half_size = RESIZE_HANDLE_SIZE / 2.0;
    div()
        .absolute()
        .left(gpui_px(position.x - half_size))
        .top(gpui_px(position.y - half_size))
        .size(gpui_px(RESIZE_HANDLE_SIZE))
        .rounded_sm()
        .bg(rgb(theme::ACCENT))
        .border_1()
        .border_color(rgb(theme::WINDOW))
}

fn landing_frame(zoom: f32, frame_size: Size<f32>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .w(px!(frame_size.width, zoom))
        .h(px!(frame_size.height, zoom))
        .p(px!(22.0, zoom))
        .gap(px!(24.0, zoom))
        .bg(rgb(theme::PAPER))
        .text_color(rgb(theme::INK))
        .border_1()
        .border_color(rgb(0xd8d5ce))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px!(8.0, zoom))
                        .child(div().size(px!(15.0, zoom)).rounded_sm().bg(rgb(theme::INK)))
                        .child(
                            div()
                                .text_size(px!(14.0, zoom))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Forma"),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px!(16.0, zoom))
                        .text_size(px!(12.0, zoom))
                        .text_color(rgb(0x686963))
                        .child("Studio")
                        .child("Journal")
                        .child("About"),
                )
                .child(
                    div()
                        .px(px!(13.0, zoom))
                        .py(px!(7.0, zoom))
                        .rounded_md()
                        .bg(rgb(theme::INK))
                        .text_size(px!(12.0, zoom))
                        .text_color(rgb(theme::PAPER))
                        .child("Explore"),
                ),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_between()
                .gap(px!(16.0, zoom))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(px!(12.0, zoom))
                        .w(px!(205.0, zoom))
                        .child(
                            div()
                                .text_size(px!(12.0, zoom))
                                .text_color(rgb(0x6c7068))
                                .child("A PLACE TO BEGIN AGAIN"),
                        )
                        .child(
                            div()
                                .text_color(rgb(theme::INK))
                                .text_size(px!(29.0, zoom))
                                .line_height(px!(32.0, zoom))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child("Make room for\nwhat matters."),
                        )
                        .child(
                            div()
                                .text_size(px!(12.0, zoom))
                                .text_color(rgb(0x676b65))
                                .child("Thoughtful objects for slower days."),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px!(8.0, zoom))
                                .mt(px!(4.0, zoom))
                                .child(
                                    div()
                                        .px(px!(13.0, zoom))
                                        .py(px!(8.0, zoom))
                                        .rounded_md()
                                        .bg(rgb(theme::INK))
                                        .text_size(px!(12.0, zoom))
                                        .text_color(rgb(theme::PAPER))
                                        .child("Discover the collection"),
                                )
                                .child(
                                    div()
                                        .text_size(px!(14.0, zoom))
                                        .text_color(rgb(0x5a5e58))
                                        .child("↗"),
                                ),
                        ),
                )
                .child(
                    div()
                        .relative()
                        .flex()
                        .items_end()
                        .justify_center()
                        .w(px!(156.0, zoom))
                        .h(px!(170.0, zoom))
                        .overflow_hidden()
                        .rounded_md()
                        .bg(rgb(theme::SAGE))
                        .child(
                            div()
                                .absolute()
                                .top(px!(19.0, zoom))
                                .left(px!(18.0, zoom))
                                .size(px!(88.0, zoom))
                                .rounded_full()
                                .bg(rgb(0xb9bda9)),
                        )
                        .child(
                            div()
                                .absolute()
                                .bottom(px!(-14.0, zoom))
                                .right(px!(14.0, zoom))
                                .w(px!(73.0, zoom))
                                .h(px!(120.0, zoom))
                                .rounded_t_full()
                                .bg(rgb(0x565e50)),
                        )
                        .child(
                            div()
                                .relative()
                                .mb(px!(20.0, zoom))
                                .w(px!(52.0, zoom))
                                .h(px!(78.0, zoom))
                                .rounded_md()
                                .bg(rgb(theme::SAND))
                                .border_1()
                                .border_color(rgb(0xeee4d4)),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .border_t_1()
                .border_color(rgb(0xd7d3ca))
                .pt(px!(10.0, zoom))
                .text_size(px!(12.0, zoom))
                .text_color(rgb(0x74766f))
                .child("Designed for the everyday")
                .child("01 — 04"),
        )
}

fn editor_frame(zoom: f32, frame_size: Size<f32>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .w(px!(frame_size.width, zoom))
        .h(px!(frame_size.height, zoom))
        .p(px!(12.0, zoom))
        .gap(px!(12.0, zoom))
        .bg(rgb(0xf0efeb))
        .border_1()
        .border_color(rgb(0xd8d5ce))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .text_size(px!(12.0, zoom))
                .text_color(rgb(0x4a4e4b))
                .child("◈  spool")
                .child("Landing page     100%"),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .gap(px!(8.0, zoom))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .w(px!(28.0, zoom))
                        .items_center()
                        .gap(px!(12.0, zoom))
                        .py(px!(8.0, zoom))
                        .bg(rgb(0xe6e4df))
                        .text_size(px!(14.0, zoom))
                        .text_color(rgb(0x555955))
                        .child("↖")
                        .child("□")
                        .child("○")
                        .child("T"),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .p(px!(13.0, zoom))
                        .gap(px!(12.0, zoom))
                        .bg(rgb(0xe5e3dc))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px!(12.0, zoom))
                                        .text_color(rgb(0x555953))
                                        .child("Forma"),
                                )
                                .child(
                                    div()
                                        .text_size(px!(12.0, zoom))
                                        .text_color(rgb(0x777a73))
                                        .child("Studio    Journal"),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .justify_center()
                                .items_start()
                                .gap(px!(8.0, zoom))
                                .p(px!(12.0, zoom))
                                .bg(rgb(0xd5d4cc))
                                .child(
                                    div()
                                        .text_size(px!(12.0, zoom))
                                        .text_color(rgb(0x666b62))
                                        .child("A PLACE TO BEGIN AGAIN"),
                                )
                                .child(
                                    div()
                                        .text_size(px!(18.0, zoom))
                                        .line_height(px!(20.0, zoom))
                                        .text_color(rgb(0x262923))
                                        .child("Make room for\nwhat matters."),
                                )
                                .child(
                                    div()
                                        .size(px!(70.0, zoom))
                                        .rounded_md()
                                        .bg(rgb(theme::SAGE)),
                                ),
                        ),
                ),
        )
}

/// Move the "Rectangle N" counters past every name the document already uses.
///
/// Only names this allocator could have produced are considered — `Rectangle 12`
/// is one, `Primary CTA` is not — so an author's own naming is never renumbered,
/// only avoided.
fn seed_object_names(runtime: &mut Document, nodes: &[StructuralNode]) {
    for node in nodes {
        let Some(number) = node
            .name
            .rsplit(' ')
            .next()
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        let Some(label) = node.name.rsplit(' ').next().map(|last| {
            node.name[..node.name.len() - last.len()]
                .trim_end()
                .to_owned()
        }) else {
            continue;
        };
        let index = match label.as_str() {
            "Frame" => 0,
            "Rectangle" => 1,
            "Ellipse" => 2,
            "Text" => 3,
            _ => continue,
        };
        runtime.next_names[index] = runtime.next_names[index].max(number + 1);
    }
}

/// The `lamine.yaml` kind for a canvas object type.
///
/// The same strings the projection reads back, so a created object is described
/// in metadata the way it will be understood on reopen. A kind the projection
/// cannot read comes back as an object that exists and draws nothing — persisted
/// but invisible — which is why this is one function rather than a literal at
/// each call site.
fn kind_for(object_type: ObjectType) -> &'static str {
    match object_type {
        ObjectType::Frame => "frame",
        ObjectType::Rectangle => "rectangle",
        ObjectType::Ellipse => "ellipse",
        ObjectType::Text => "text",
    }
}

/// The next free identity number for a project that has just been opened.
///
/// Reads the identities already on disk rather than trusting the counter the
/// runtime starts with. `allocate_node_id` only knows about objects in the
/// runtime, so a reopened project full of `spool-node-…` identities would
/// otherwise be handed one it already uses — and two objects would arrive at the
/// same element on the next load.
///
/// Unparseable identities are ignored rather than guessed at: a node whose id was
/// not minted by the allocator cannot collide with one that was, and refusing to
/// open over it would be worse than skipping it.
fn next_node_id_after(document: &PersistentDocument) -> u64 {
    const PREFIX: &str = "spool-node-";
    document
        .structure
        .nodes
        .iter()
        .filter_map(|node| {
            let rest = node.id.as_str().strip_prefix(PREFIX)?;
            u64::from_str_radix(rest, 16).ok()
        })
        .max()
        .map_or(1, |highest| highest + 1)
}

fn features_frame(zoom: f32, frame_size: Size<f32>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .w(px!(frame_size.width, zoom))
        .h(px!(frame_size.height, zoom))
        .p(px!(20.0, zoom))
        .gap(px!(16.0, zoom))
        .bg(rgb(0x202421))
        .text_color(rgb(0xf0efe8))
        .border_1()
        .border_color(rgb(0x333a34))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .text_size(px!(12.0, zoom))
                .text_color(rgb(0xb2b8ac))
                .child("FORMA  /  OUR APPROACH")
                .child("A quieter kind of good"),
        )
        .child(
            div()
                .flex()
                .items_end()
                .justify_between()
                .flex_1()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px!(8.0, zoom))
                        .w(px!(190.0, zoom))
                        .child(
                            div()
                                .text_size(px!(24.0, zoom))
                                .line_height(px!(27.0, zoom))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child("Made to stay.\nMade with care."),
                        )
                        .child(
                            div()
                                .text_size(px!(12.0, zoom))
                                .text_color(rgb(0xb7bcb1))
                                .child("Considered forms, honest materials, fewer better things."),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_end()
                        .gap(px!(8.0, zoom))
                        .child(
                            div()
                                .flex()
                                .items_end()
                                .justify_center()
                                .w(px!(62.0, zoom))
                                .h(px!(100.0, zoom))
                                .pb(px!(9.0, zoom))
                                .bg(rgb(0x666f5f))
                                .text_size(px!(12.0, zoom))
                                .text_color(rgb(0xe9e8df))
                                .child("01"),
                        )
                        .child(
                            div()
                                .flex()
                                .items_end()
                                .justify_center()
                                .w(px!(62.0, zoom))
                                .h(px!(122.0, zoom))
                                .pb(px!(9.0, zoom))
                                .bg(rgb(0x88877a))
                                .text_size(px!(12.0, zoom))
                                .text_color(rgb(0xf0eee7))
                                .child("02"),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .gap(px!(8.0, zoom))
                .child(feature_pill("01", "Thoughtful form", zoom))
                .child(feature_pill("02", "Lasting materials", zoom))
                .child(feature_pill("03", "Less, but better", zoom)),
        )
}

fn feature_pill(number: &'static str, label: &'static str, zoom: f32) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .gap(px!(8.0, zoom))
        .p(px!(10.0, zoom))
        .border_1()
        .border_color(rgb(0x3a413a))
        .child(
            div()
                .text_size(px!(12.0, zoom))
                .text_color(rgb(theme::ACCENT))
                .child(number),
        )
        .child(
            div()
                .text_size(px!(12.0, zoom))
                .text_color(rgb(0xe3e4dd))
                .child(label),
        )
}

fn mobile_frame(zoom: f32, frame_size: Size<f32>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .w(px!(frame_size.width, zoom))
        .h(px!(frame_size.height, zoom))
        .p(px!(10.0, zoom))
        .gap(px!(12.0, zoom))
        .rounded_lg()
        .bg(rgb(0x171a1d))
        .border_1()
        .border_color(rgb(0x45494a))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px(px!(5.0, zoom))
                .text_size(px!(12.0, zoom))
                .text_color(rgb(0xe7e7e2))
                .child("9:41")
                .child("●  ▮"),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap(px!(12.0, zoom))
                .p(px!(12.0, zoom))
                .bg(rgb(0xe9e6de))
                .text_color(rgb(0x252722))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .text_size(px!(12.0, zoom))
                        .child("Forma")
                        .child("☰"),
                )
                .child(
                    div()
                        .text_size(px!(21.0, zoom))
                        .line_height(px!(23.0, zoom))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child("Good design\nlives well."),
                )
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .items_end()
                        .justify_end()
                        .p(px!(9.0, zoom))
                        .bg(rgb(theme::SAGE))
                        .child(
                            div()
                                .w(px!(54.0, zoom))
                                .h(px!(92.0, zoom))
                                .rounded_t_full()
                                .bg(rgb(0x68715f)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .text_size(px!(12.0, zoom))
                        .text_color(rgb(0x676b64))
                        .child("Objects for living")
                        .child("↗"),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::test_support::RetainedLayers;
    use crate::operations::{OperationTarget, SemanticHistory};

    fn starter_layer_rows() -> Vec<(ObjectId, SharedString)> {
        vec![
            (ObjectId::LANDING, "Landing".into()),
            (ObjectId::EDITOR, "Editor".into()),
            (ObjectId::FEATURES, "Features".into()),
            (ObjectId::MOBILE, "Mobile".into()),
        ]
    }

    #[test]
    fn retained_layer_rows_follow_create_delete_duplicate_and_history() {
        let mut canvas = CanvasView::new();
        let mut layers = RetainedLayers::default();
        let starter = starter_layer_rows();
        assert_eq!(layers.synchronize(&canvas), starter);

        let text = canvas.session.runtime.create_object(
            ObjectType::Text,
            point(10.0, 20.0),
            size(80.0, 30.0),
            Some("Content, not the layer name".into()),
        );
        canvas.commit(DocumentCommand::insert(vec![canvas
            .session
            .runtime
            .placement(text.id)
            .unwrap()]));
        let mut created = starter.clone();
        created.push((text.id, "Text 1".into()));
        assert_eq!(layers.synchronize(&canvas), created);
        assert!(canvas.session.undo().unwrap());
        assert_eq!(layers.synchronize(&canvas), starter);
        assert!(canvas.session.redo().unwrap());
        assert_eq!(layers.synchronize(&canvas), created);

        // Reverse selection order must not reverse document/projected order.
        let duplicates = canvas
            .session
            .runtime
            .duplicate_objects(&[text.id, ObjectId::EDITOR]);
        assert_eq!(duplicates.len(), 2);
        canvas.commit(DocumentCommand::insert(duplicates.clone()));
        let mut duplicated = created.clone();
        duplicated.extend([
            (duplicates[0].object.id, "Frame 1".into()),
            (duplicates[1].object.id, "Text 2".into()),
        ]);
        assert_eq!(layers.synchronize(&canvas), duplicated);
        assert!(canvas.session.undo().unwrap());
        assert_eq!(layers.synchronize(&canvas), created);
        assert!(canvas.session.redo().unwrap());
        assert_eq!(layers.synchronize(&canvas), duplicated);

        // Non-adjacent deletions must restore their original positions and names.
        let removed = canvas
            .session
            .runtime
            .remove_objects(&[text.id, ObjectId::EDITOR]);
        canvas.commit(DocumentCommand::delete(removed));
        let deleted: Vec<_> = duplicated
            .iter()
            .filter(|(id, _)| *id != text.id && *id != ObjectId::EDITOR)
            .cloned()
            .collect();
        assert_eq!(layers.synchronize(&canvas), deleted);
        assert!(canvas.session.undo().unwrap());
        assert_eq!(layers.synchronize(&canvas), duplicated);
        assert!(canvas.session.redo().unwrap());
        assert_eq!(layers.synchronize(&canvas), deleted);
        assert_eq!(layers.document_walks(), 10);
    }

    #[test]
    fn retained_layer_rows_keep_names_and_skip_walks_for_selection_and_transient_changes() {
        let mut canvas = CanvasView::new();
        let text = canvas.session.runtime.create_object(
            ObjectType::Text,
            point(10.0, 20.0),
            size(80.0, 30.0),
            Some("hello".into()),
        );
        let mut layers = RetainedLayers::default();
        let mut expected = starter_layer_rows();
        expected.push((text.id, "Text 1".into()));
        assert_eq!(layers.synchronize(&canvas), expected);

        canvas.selection.replace(vec![text.id, ObjectId::EDITOR]);
        assert_eq!(layers.synchronize(&canvas), expected);
        assert_eq!(layers.selected(), &[text.id, ObjectId::EDITOR]);
        // Selection changes report only the rows whose presentation changed,
        // in row order; unchanged rows are not touched.
        assert_eq!(
            layers.presentation_updates(),
            &[(ObjectId::EDITOR, true), (text.id, true)]
        );
        canvas.selection.replace(vec![ObjectId::LANDING]);
        assert_eq!(layers.synchronize(&canvas), expected);
        assert_eq!(layers.selected(), &[ObjectId::LANDING]);
        assert_eq!(
            layers.presentation_updates(),
            &[
                (ObjectId::LANDING, true),
                (ObjectId::EDITOR, false),
                (text.id, false),
            ]
        );

        let before = canvas.session.runtime.geometry(text.id).unwrap();
        canvas
            .session
            .runtime
            .set_position(text.id, point(100.0, 200.0));
        canvas.session.runtime.set_size(text.id, size(120.0, 40.0));
        assert_eq!(layers.synchronize(&canvas), expected);
        let after = canvas.session.runtime.geometry(text.id).unwrap();
        canvas
            .session
            .history
            .record(SemanticOperation::Runtime(DocumentCommand::geometry(vec![
                GeometryChange {
                    id: text.id,
                    before,
                    after,
                },
            ])));
        assert!(canvas.session.undo().unwrap());
        assert_eq!(layers.synchronize(&canvas), expected);
        assert!(canvas.session.redo().unwrap());
        assert_eq!(layers.synchronize(&canvas), expected);

        canvas.session.runtime.set_style(
            text.id,
            ObjectStyle {
                fill: None,
                stroke: None,
                ..ObjectStyle::default()
            },
        );
        assert_eq!(layers.synchronize(&canvas), expected);
        canvas
            .session
            .runtime
            .set_text_content(text.id, "Changed content must not replace Text 1".into());
        assert_eq!(layers.synchronize(&canvas), expected);
        canvas.commit(DocumentCommand::text(vec![TextChange {
            id: text.id,
            before: "hello".into(),
            after: "Changed content must not replace Text 1".into(),
        }]));
        assert!(canvas.session.undo().unwrap());
        assert_eq!(layers.synchronize(&canvas), expected);
        assert!(canvas.session.redo().unwrap());
        assert_eq!(layers.synchronize(&canvas), expected);

        canvas.camera.offset = point(300.0, 400.0);
        canvas.set_zoom_percent(200);
        assert_eq!(layers.synchronize(&canvas), expected);
        canvas.selection.replace(vec![]);
        assert_eq!(layers.synchronize(&canvas), expected);
        assert!(layers.selected().is_empty());
        assert_eq!(layers.presentation_updates(), &[(ObjectId::LANDING, false)]);
        assert_eq!(layers.document_walks(), 1);
    }

    #[test]
    fn layer_structure_revision_tracks_successful_mutations_and_history_order() {
        let mut document = Document::default();
        let mut history = SemanticHistory::default();
        assert_eq!(document.layer_structure_revision(), 0);
        let object = document.create_object(
            ObjectType::Text,
            point(1.0, 2.0),
            size(80.0, 30.0),
            Some("hello".into()),
        );
        assert_eq!(document.layer_structure_revision(), 1);
        assert!(!document.insert_object(object.clone(), 0));
        assert!(document.remove_objects(&[ObjectId(9999)]).is_empty());
        assert_eq!(document.layer_structure_revision(), 1);
        let original_order: Vec<_> = document.objects().iter().map(|object| object.id).collect();
        let duplicates = document.duplicate_objects(&[ObjectId::EDITOR, object.id]);
        assert_eq!(document.layer_structure_revision(), 3);
        history.record(SemanticOperation::Runtime(DocumentCommand::insert(
            duplicates.clone(),
        )));
        history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert_eq!(document.layer_structure_revision(), 5);
        assert_eq!(
            document
                .objects()
                .iter()
                .map(|object| object.id)
                .collect::<Vec<_>>(),
            original_order
        );
        history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert_eq!(document.layer_structure_revision(), 7);
        let order: Vec<_> = document.objects().iter().map(|object| object.id).collect();
        assert_eq!(
            &order[original_order.len()..],
            &duplicates
                .iter()
                .map(|placement| placement.object.id)
                .collect::<Vec<_>>()
        );
        let removed = document.remove_objects(&[ObjectId::EDITOR, object.id]);
        assert_eq!(document.layer_structure_revision(), 9);
        history.record(SemanticOperation::Runtime(DocumentCommand::delete(removed)));
        history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert_eq!(document.layer_structure_revision(), 11);
        assert_eq!(
            document
                .objects()
                .iter()
                .map(|object| object.id)
                .collect::<Vec<_>>(),
            order
        );
        history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert_eq!(document.layer_structure_revision(), 13);
    }

    #[test]
    fn layer_structure_revision_ignores_geometry_style_text_selection_and_camera() {
        let mut canvas = CanvasView::new();
        let object = canvas.session.runtime.create_object(
            ObjectType::Text,
            point(1.0, 2.0),
            size(80.0, 30.0),
            Some("hello".into()),
        );
        let revision = canvas.layer_structure_revision();
        let before = canvas.session.runtime.geometry(object.id).unwrap();
        canvas
            .session
            .runtime
            .set_position(object.id, point(20.0, 30.0));
        let after = canvas.session.runtime.geometry(object.id).unwrap();
        canvas
            .session
            .history
            .record(SemanticOperation::Runtime(DocumentCommand::geometry(vec![
                GeometryChange {
                    id: object.id,
                    before,
                    after,
                },
            ])));
        canvas.session.undo().unwrap();
        canvas.session.redo().unwrap();
        canvas
            .session
            .runtime
            .set_size(object.id, size(100.0, 50.0));
        canvas.session.runtime.set_style(
            object.id,
            ObjectStyle {
                fill: None,
                stroke: None,
                ..ObjectStyle::default()
            },
        );
        canvas
            .session
            .runtime
            .set_text_content(object.id, "changed".into());
        canvas.commit(DocumentCommand::text(vec![TextChange {
            id: object.id,
            before: "hello".into(),
            after: "changed".into(),
        }]));
        canvas.session.undo().unwrap();
        canvas.session.redo().unwrap();
        canvas.selection.click_flat(Some(object.id), false);
        canvas.set_zoom_percent(200);
        canvas.camera.offset = point(100.0, 200.0);
        assert_eq!(canvas.layer_structure_revision(), revision);
    }

    #[test]
    fn borrowed_object_element_construction_preserves_document_at_all_zoom_limits() {
        let mut document = Document::default();
        for object_type in [
            ObjectType::Frame,
            ObjectType::Rectangle,
            ObjectType::Ellipse,
            ObjectType::Text,
        ] {
            document.create_object(
                object_type,
                point(20.0, 40.0),
                size(100.0, 80.0),
                (object_type == ObjectType::Text).then(|| "Unicode 🧵 text".to_string()),
            );
        }
        let before = document.objects().to_vec();
        for zoom in [MIN_ZOOM, 1.0, MAX_ZOOM] {
            let camera = Camera {
                zoom,
                ..Camera::default()
            };
            for object in document.objects() {
                let _element = render_object(camera, object, zoom, false).into_any_element();
                let _outline = selection_outline(camera, object.geometry()).into_any_element();
                for handle in ResizeHandle::ALL {
                    let _handle =
                        resize_handle_element(camera, object.geometry(), handle).into_any_element();
                }
            }
        }
        assert_eq!(document.objects(), before);
    }

    #[test]
    fn camera_coordinates_round_trip() {
        let mut camera = Camera::default();
        camera.resize(size(960.0, 720.0));
        camera.zoom_at(1.7, point(317.0, 246.0));

        let world = point(183.0, 512.0);
        let screen = camera.world_to_screen(world);
        let round_trip = camera.screen_to_world(screen);
        assert!((round_trip.x - world.x).abs() < 0.001);
        assert!((round_trip.y - world.y).abs() < 0.001);
    }

    #[test]
    fn zoom_keeps_the_world_point_under_the_cursor() {
        let mut camera = Camera::default();
        camera.resize(size(960.0, 720.0));
        let cursor = point(723.0, 151.0);
        let world_anchor = camera.screen_to_world(cursor);

        camera.zoom_at(2.4, cursor);

        let screen_anchor = camera.world_to_screen(world_anchor);
        assert!((screen_anchor.x - cursor.x).abs() < 0.001);
        assert!((screen_anchor.y - cursor.y).abs() < 0.001);
    }

    #[test]
    fn a_zoom_factor_that_cannot_describe_a_zoom_leaves_the_camera_alone() {
        // A trackpad pinch reports its own delta, so the factor reaching
        // `zoom_at` is `1.0 + delta` and a delta of -1 or worse arrives as zero
        // or negative. A malformed gesture can arrive as NaN. Both used to be
        // multiplied straight into the zoom and from there into the offset,
        // which is unrecoverable: one NaN offset makes the canvas disappear
        // with no way back, and a negative factor slams the view to minimum
        // zoom from wherever the user was.
        let cursor = point(723.0, 151.0);
        for factor in [0.0, -1.0, -12.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut camera = Camera::default();
            camera.resize(size(960.0, 720.0));
            let zoom = camera.zoom;
            let offset = camera.offset;

            camera.zoom_at(factor, cursor);

            assert_eq!(camera.zoom, zoom, "factor {factor} must not zoom");
            assert_eq!(camera.offset, offset, "factor {factor} must not pan");
        }
    }

    #[test]
    fn a_valid_zoom_factor_still_goes_through_the_same_choke_point() {
        // The guard above is a refusal, not a clamp, so the ordinary factors
        // have to be unaffected by it.
        let cursor = point(723.0, 151.0);
        let mut camera = Camera::default();
        camera.resize(size(960.0, 720.0));

        camera.zoom_at(2.0, cursor);

        assert_eq!(camera.zoom, 2.0);
    }

    #[test]
    fn a_released_pan_modifier_stops_a_left_drag_from_panning() {
        // The consequence of a stuck space bar: with it held, a left-drag pans
        // the camera instead of moving the object under the pointer. The middle
        // button pans either way, which is why only the left button is at risk.
        let mut canvas = CanvasView::new();
        assert!(canvas.pans_with(MouseButton::Middle), "middle always pans");
        assert!(
            !canvas.pans_with(MouseButton::Left),
            "and a left-drag is not a pan until space says so"
        );

        canvas.set_space_held(true);
        assert!(canvas.pans_with(MouseButton::Left), "space held");

        canvas.set_space_held(false);
        assert!(
            !canvas.pans_with(MouseButton::Left),
            "releasing it is what unsticks the canvas, so the release must be reachable"
        );
    }

    #[test]
    fn the_zoom_keys_do_not_anchor_on_a_selection_that_is_off_screen() {
        // Anchoring is a screen position. Handed the screen coordinates of
        // something far outside the viewport, `zoom_at` faithfully pins *that*
        // world point under that screen point — so one press of `+` would swing
        // the view across the document to an object the user cannot see.
        let mut camera = Camera::default();
        camera.resize(size(960.0, 720.0));
        assert!(camera.viewport_contains(point(480.0, 360.0)));
        assert!(!camera.viewport_contains(point(-4000.0, 360.0)));
        assert!(!camera.viewport_contains(point(480.0, 4000.0)));

        let mut canvas = CanvasView::new();
        canvas.camera = camera;
        // Put the selection far to the left of everything the camera can see.
        canvas
            .session
            .runtime
            .set_position(ObjectId::LANDING, point(-9000.0, 24.0));
        canvas
            .selection
            .click(Some(ObjectId::LANDING), false, &canvas.hierarchy());
        let geometry = canvas.session.runtime.geometry(ObjectId::LANDING).unwrap();
        let world = point(
            geometry.position.x + geometry.size.width / 2.0,
            geometry.position.y + geometry.size.height / 2.0,
        );
        assert!(
            canvas.selection_bounds_screen().is_none(),
            "an off-screen selection is not an anchor"
        );

        let screen_before = canvas.camera.world_to_screen(world);
        canvas.zoom_in();
        let screen_after = canvas.camera.world_to_screen(world);

        assert!(
            (screen_after.x - screen_before.x).abs() > 1.0,
            "the off-screen object was not pinned under the cursor, so it was not the anchor: {screen_before:?} -> {screen_after:?}"
        );
    }

    #[test]
    fn the_zoom_keys_still_anchor_on_a_selection_that_is_on_screen() {
        let mut camera = Camera::default();
        camera.resize(size(960.0, 720.0));
        let mut canvas = CanvasView::new();
        canvas.camera = camera;
        canvas
            .session
            .runtime
            .set_position(ObjectId::LANDING, point(40.0, 40.0));
        canvas
            .selection
            .click(Some(ObjectId::LANDING), false, &canvas.hierarchy());

        let anchor = canvas
            .selection_bounds_screen()
            .expect("an on-screen selection is an anchor");
        assert!(canvas.camera.viewport_contains(anchor));

        canvas.zoom_in();

        // The anchor keeps its world point, which is what "anchored on the
        // selection" means.
        let world = canvas.camera.screen_to_world(anchor);
        let after = canvas.camera.world_to_screen(world);
        assert!((after.x - anchor.x).abs() < 0.001, "{:?}", canvas.camera);
        assert!((after.y - anchor.y).abs() < 0.001, "{:?}", canvas.camera);
    }

    #[test]
    fn a_shifted_wheel_scrolls_sideways_without_touching_the_other_axis() {
        // Drives `handle_wheel` — the body the listener calls — rather than
        // restating its arithmetic. The previous version of this test computed
        // `point(x + y, 0.0)` and asserted on the result, which would have
        // passed with the handler deleted.
        let mut canvas = CanvasView::new();
        canvas.camera.resize(size(800.0, 600.0));
        let centre = point(400.0, 300.0);

        // A diagonal scroll, the case that separates summing from swapping.
        let before = canvas.camera.offset;
        canvas.handle_wheel(
            gpui::ScrollDelta::Pixels(point(gpui_px(5.0), gpui_px(3.0))),
            false,
            true,
            centre,
        );
        let after = canvas.camera.offset;
        assert_eq!(
            after.y, before.y,
            "the vertical travel is suppressed, so nothing moved up or down"
        );
        assert!(
            after.x < before.x,
            "and the travel went sideways instead: {:?} -> {:?}",
            before,
            after
        );

        // Without `⇧`, both axes move, and the vertical one by exactly its own
        // delta — a swap would have thrown that away.
        let before = canvas.camera.offset;
        canvas.handle_wheel(
            gpui::ScrollDelta::Pixels(point(gpui_px(5.0), gpui_px(3.0))),
            false,
            false,
            centre,
        );
        let after = canvas.camera.offset;
        assert!(
            after.y < before.y && after.x < before.x,
            "an unshifted scroll uses both axes: {:?} -> {:?}",
            before,
            after
        );
    }

    #[test]
    fn a_wheel_zoom_reads_the_vertical_component_at_the_documented_gain() {
        // The other half of the wheel, and the reason the line height and the
        // gain are named constants: the conversion is the behaviour, so it is
        // asserted through the handler instead of beside it.
        let mut canvas = CanvasView::new();
        canvas.camera.resize(size(800.0, 600.0));
        let centre = point(400.0, 300.0);
        assert_eq!(canvas.camera.zoom, 1.0, "the camera starts at actual size");

        // A `⌘`+wheel notch: one line of travel, zoomed by the gain.
        canvas.handle_wheel(
            gpui::ScrollDelta::Pixels(point(gpui_px(0.0), gpui_px(24.0))),
            true,
            false,
            centre,
        );
        let notched = canvas.camera.zoom;
        assert!(
            notched > 1.0,
            "a zoom wheel zooms in: {} -> {notched}",
            canvas.camera.zoom
        );
        assert!(
            (notched - (24.0f32 * ZOOM_WHEEL_GAIN).exp()).abs() < 0.0001,
            "and by exactly the documented gain, {notched}"
        );

        // A purely horizontal wheel event carries no vertical travel, so a zoom
        // wheel does nothing. Deliberate rather than accidental: the gain is
        // applied to `y` because that is the axis a notch moves on.
        let before = canvas.camera.zoom;
        canvas.handle_wheel(
            gpui::ScrollDelta::Pixels(point(gpui_px(24.0), gpui_px(0.0))),
            true,
            false,
            centre,
        );
        assert_eq!(
            canvas.camera.zoom, before,
            "a horizontal-only wheel event is not a zoom"
        );

        // And the inverse: the same notch downwards zooms back out.
        canvas.handle_wheel(
            gpui::ScrollDelta::Pixels(point(gpui_px(0.0), gpui_px(-24.0))),
            true,
            false,
            centre,
        );
        assert!(
            canvas.camera.zoom < notched,
            "the opposite notch zooms back out"
        );
    }

    #[test]
    fn a_wheel_is_refused_while_a_gesture_owns_the_pointer() {
        // The gate the handler shares with the pinch, asserted where the wheel
        // now goes through the same named path.
        let mut canvas = CanvasView::new();
        canvas.camera.resize(size(800.0, 600.0));
        let centre = point(400.0, 300.0);
        let id = ObjectId::LANDING;

        canvas.handle_wheel(
            gpui::ScrollDelta::Pixels(point(gpui_px(0.0), gpui_px(60.0))),
            false,
            false,
            centre,
        );
        let panned = canvas.camera.offset;
        canvas.handle_wheel(
            gpui::ScrollDelta::Pixels(point(gpui_px(0.0), gpui_px(60.0))),
            true,
            false,
            centre,
        );
        assert_ne!(canvas.camera.zoom, 1.0, "both work with the pointer free");

        begin_live_move(&mut canvas, &[id]);
        canvas.camera.offset = panned;
        let zoom = canvas.camera.zoom;
        canvas.pan = None;
        canvas.handle_wheel(
            gpui::ScrollDelta::Pixels(point(gpui_px(0.0), gpui_px(60.0))),
            false,
            false,
            centre,
        );
        assert_eq!(
            canvas.camera.offset, panned,
            "a pan wheel is refused mid-drag"
        );
        canvas.handle_wheel(
            gpui::ScrollDelta::Pixels(point(gpui_px(0.0), gpui_px(60.0))),
            true,
            false,
            centre,
        );
        assert_eq!(canvas.camera.zoom, zoom, "and so is a zoom wheel");
        canvas.cancel_interaction();
    }

    #[test]
    fn pan_tracks_pointer_delta_at_current_zoom() {
        let mut camera = Camera::default();
        camera.resize(size(960.0, 720.0));
        camera.zoom_at(2.0, point(480.0, 360.0));

        let world_point = point(350.0, 280.0);
        let start_screen = camera.world_to_screen(world_point);
        let offset_start = camera.offset;
        camera.pan_from(offset_start, point(20.0, 30.0), point(84.0, 6.0));
        let end_screen = camera.world_to_screen(world_point);

        assert!((end_screen.x - start_screen.x - 64.0).abs() < 0.001);
        assert!((end_screen.y - start_screen.y + 24.0).abs() < 0.001);
    }

    #[test]
    fn zoom_is_clamped_and_fit_centers_world_bounds() {
        let mut camera = Camera::default();
        camera.resize(size(960.0, 720.0));

        camera.zoom_at(0.001, point(480.0, 360.0));
        assert_eq!(camera.zoom, MIN_ZOOM);
        camera.zoom_at(100.0, point(480.0, 360.0));
        assert_eq!(camera.zoom, MAX_ZOOM);

        camera.fit();
        let center = camera.world_to_screen(WORLD_CENTER);
        assert!((center.x - 480.0).abs() < 0.001);
        assert!((center.y - 360.0).abs() < 0.001);
        let bottom_right = camera.world_to_screen(point(WORLD_BOUNDS.width, WORLD_BOUNDS.height));
        assert!(bottom_right.x <= 912.0);
        assert!(bottom_right.y <= 672.0);
    }

    #[test]
    fn document_allocates_unique_ids_and_deterministic_names() {
        let mut document = Document::default();
        let first = document.create_object(
            ObjectType::Rectangle,
            point(10.0, 20.0),
            size(120.0, 80.0),
            None,
        );
        let second = document.create_object(
            ObjectType::Rectangle,
            point(30.0, 40.0),
            size(90.0, 60.0),
            None,
        );

        assert_ne!(first.id, second.id);
        assert_eq!(first.name, "Rectangle 1");
        assert_eq!(second.name, "Rectangle 2");
    }

    #[test]
    fn document_creates_each_phase_six_object_type_with_geometry() {
        let mut document = Document::default();
        for (index, object_type) in [
            ObjectType::Frame,
            ObjectType::Rectangle,
            ObjectType::Ellipse,
            ObjectType::Text,
        ]
        .into_iter()
        .enumerate()
        {
            let content = (object_type == ObjectType::Text).then(|| "hello, world".to_string());
            let object = document.create_object(
                object_type,
                point(index as f32 * 10.0, 25.0),
                size(140.0, 90.0),
                content.clone(),
            );
            assert_eq!(object.object_type, object_type);
            assert_eq!(object.position, point(index as f32 * 10.0, 25.0));
            assert_eq!(object.size, size(140.0, 90.0));
            assert_eq!(object.text_content, content);
            assert_eq!(document.object(object.id), Some(&object));
        }
        assert_eq!(document.objects().len(), 8);
    }

    #[test]
    fn created_objects_are_hit_testable_and_newest_object_wins() {
        let mut document = Document::default();
        let first = document.create_object(
            ObjectType::Rectangle,
            point(20.0, 30.0),
            size(100.0, 80.0),
            None,
        );
        let second = document.create_object(
            ObjectType::Ellipse,
            point(20.0, 30.0),
            size(100.0, 80.0),
            None,
        );

        assert_eq!(document.hit_test(point(50.0, 50.0)), Some(second.id));
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn creation_drag_normalizes_reversed_direction_and_never_has_negative_size() {
        let geometry = creation_geometry(point(260.0, 220.0), point(100.0, 100.0));

        assert_eq!(geometry.position, point(100.0, 100.0));
        assert_eq!(geometry.size, size(160.0, 120.0));
    }

    // -- Click creation and the tool that completes it.

    /// Press and release without moving, with `tool` active: the production
    /// pointer-up path, driven the way a pointer drives it.
    fn click_with(tool: Tool, object_type: ObjectType, at: Point<f32>) -> CanvasView {
        let mut canvas = CanvasView::new();
        canvas.set_tool(tool);
        canvas.interaction = Interaction::PotentialCreate(CreateGesture {
            object_type,
            pointer_start_screen: at,
            pointer_start_world: at,
            current_world: at,
            moved: false,
        });
        canvas.finish_interaction(at);
        canvas
    }

    /// The same, with the camera already zoomed, and the world point the press
    /// actually landed on.
    fn click_with_zoomed(
        tool: Tool,
        object_type: ObjectType,
        at: Point<f32>,
        zoom: f32,
    ) -> (CanvasView, Point<f32>) {
        let mut canvas = CanvasView::new();
        canvas.camera.set_zoom_at_center(zoom);
        canvas.set_tool(tool);
        let world = canvas.camera.screen_to_world(at);
        canvas.interaction = Interaction::PotentialCreate(CreateGesture {
            object_type,
            pointer_start_screen: at,
            pointer_start_world: world,
            current_world: world,
            moved: false,
        });
        canvas.finish_interaction(at);
        (canvas, world)
    }

    /// The object the gesture just made, read back off the canvas.
    fn last_object(canvas: &CanvasView) -> DesignObject {
        canvas
            .session
            .runtime
            .objects()
            .last()
            .cloned()
            .expect("an object was created")
    }

    #[test]
    fn a_click_creates_the_tools_default_box() {
        for (tool, object_type) in [
            (Tool::Frame, ObjectType::Frame),
            (Tool::Rectangle, ObjectType::Rectangle),
            (Tool::Ellipse, ObjectType::Ellipse),
        ] {
            let canvas = click_with(tool, object_type, point(240.0, 180.0));
            let created = last_object(&canvas);
            assert_eq!(created.object_type, object_type);
            assert_eq!(
                created.size,
                size(DEFAULT_CREATION_SIZE, DEFAULT_CREATION_SIZE),
                "{object_type:?} gets the default square"
            );
            // Named here so the requirement is stated rather than implied.
            assert_eq!(DEFAULT_CREATION_SIZE, 100.0, "Frame click is 100x100");
        }
    }

    #[test]
    fn a_click_with_the_text_tool_creates_its_existing_box() {
        // Text's click size predates click creation for the other tools; it is
        // preserved rather than folded into the default square.
        let canvas = click_with(Tool::Text, ObjectType::Text, point(240.0, 180.0));
        let created = last_object(&canvas);
        assert_eq!(created.object_type, ObjectType::Text);
        // Literal, not the constants: asserting a constant against itself would
        // pass whatever the constants were changed to, which is the opposite of
        // pinning the box down.
        assert_eq!(
            created.size,
            size(180.0, 48.0),
            "text keeps its existing box"
        );
    }

    #[test]
    fn a_click_places_the_object_by_its_top_left_where_a_drag_would() {
        // One anchor for both. `creation_geometry` puts a drag's top-left at the
        // press, so a click does the same rather than inventing a centre.
        //
        // Zoomed, because at 1x the press point and its own world coordinate are
        // the same number and a click anchored to either would look identical.
        // At 2x they are genuinely different places, and only the press point's
        // world coordinate can be where the click was.
        let press = point(140.0, 110.0);
        let zoomed = click_with_zoomed(Tool::Rectangle, ObjectType::Rectangle, press, 2.0);
        let at = zoomed.1;
        let clicked = last_object(&zoomed.0);

        let mut canvas = CanvasView::new();
        canvas.camera.set_zoom_at_center(2.0);
        canvas.set_tool(Tool::Rectangle);
        canvas.interaction = Interaction::PotentialCreate(CreateGesture {
            object_type: ObjectType::Rectangle,
            pointer_start_screen: at,
            pointer_start_world: at,
            current_world: at,
            moved: false,
        });
        let to = point(360.0, 300.0);
        canvas.update_interaction(to);
        canvas.finish_interaction(to);
        let dragged = last_object(&canvas);

        assert_eq!(clicked.position, at, "click anchors top-left at the press");
        assert_eq!(
            dragged.position, at,
            "and so does a drag, which is the whole claim"
        );
        let to_world = canvas.camera.screen_to_world(to);
        assert_eq!(
            dragged.size,
            size((to_world.x - at.x).abs(), (to_world.y - at.y).abs()),
            "the drag still sets its own bounds"
        );
    }

    #[test]
    fn a_drag_still_creates_explicit_geometry() {
        let at = point(100.0, 100.0);
        let mut canvas = CanvasView::new();
        canvas.set_tool(Tool::Frame);
        canvas.interaction = Interaction::PotentialCreate(CreateGesture {
            object_type: ObjectType::Frame,
            pointer_start_screen: at,
            pointer_start_world: at,
            current_world: at,
            moved: false,
        });
        canvas.update_interaction(point(260.0, 220.0));
        canvas.finish_interaction(point(260.0, 220.0));
        let created = last_object(&canvas);
        assert_eq!(created.size, size(160.0, 120.0), "the drag's own bounds");
    }

    #[test]
    fn a_clicks_default_size_is_the_same_at_every_zoom() {
        // The default is in world units. Zoom changes how big the box looks on
        // screen and must not change the box in the document.
        for zoom in [0.5, 1.0, 2.0] {
            let mut canvas = CanvasView::new();
            canvas.camera.set_zoom_at_center(zoom);
            canvas.set_tool(Tool::Frame);
            // Deliberately off-centre: on the viewport's own centre, the world
            // point a press maps to is the same at every zoom, which would let an
            // anchor bug hide.
            let at = canvas.camera.screen_to_world(point(260.0, 210.0));
            canvas.interaction = Interaction::PotentialCreate(CreateGesture {
                object_type: ObjectType::Frame,
                pointer_start_screen: point(260.0, 210.0),
                pointer_start_world: at,
                current_world: at,
                moved: false,
            });
            canvas.finish_interaction(point(260.0, 210.0));
            let created = last_object(&canvas);
            assert_eq!(
                created.size,
                size(DEFAULT_CREATION_SIZE, DEFAULT_CREATION_SIZE),
                "at {zoom}x the world size is unchanged"
            );
            assert_eq!(created.position, at, "and it lands where the click was");
        }
    }

    #[test]
    fn a_creation_selects_the_new_object_and_returns_to_selection() {
        for (tool, object_type) in [
            (Tool::Frame, ObjectType::Frame),
            (Tool::Text, ObjectType::Text),
        ] {
            let canvas = click_with(tool, object_type, point(240.0, 180.0));
            let created = last_object(&canvas);
            assert_eq!(
                canvas.selection.ids(),
                vec![created.id],
                "{object_type:?}: the new object is the only selection"
            );
            // Text's session is opened by the pointer-up handler that also owns
            // this transition, and it returns to Selection there.
            if object_type != ObjectType::Text {
                assert_eq!(
                    canvas.tool(),
                    Tool::Select,
                    "{object_type:?}: creation finishes back on Selection"
                );
            }
        }
    }

    #[test]
    fn a_creation_deselects_whatever_was_selected_first() {
        let mut canvas = CanvasView::new();
        let previously = canvas.session.runtime.objects()[0].id;
        canvas.selection.replace(vec![previously]);
        assert_eq!(canvas.selection.ids(), vec![previously]);

        canvas.set_tool(Tool::Rectangle);
        let at = point(240.0, 180.0);
        canvas.interaction = Interaction::PotentialCreate(CreateGesture {
            object_type: ObjectType::Rectangle,
            pointer_start_screen: at,
            pointer_start_world: at,
            current_world: at,
            moved: false,
        });
        canvas.finish_interaction(at);

        let created = last_object(&canvas).id;
        assert_eq!(
            canvas.selection.ids(),
            vec![created],
            "the new object replaced the selection, not joined it"
        );
    }

    #[test]
    fn a_drag_creation_also_returns_to_selection() {
        let at = point(100.0, 100.0);
        let mut canvas = CanvasView::new();
        canvas.set_tool(Tool::Ellipse);
        canvas.interaction = Interaction::PotentialCreate(CreateGesture {
            object_type: ObjectType::Ellipse,
            pointer_start_screen: at,
            pointer_start_world: at,
            current_world: at,
            moved: false,
        });
        canvas.update_interaction(point(200.0, 200.0));
        canvas.finish_interaction(point(200.0, 200.0));
        assert_eq!(canvas.tool(), Tool::Select);
        assert_eq!(canvas.selection.ids(), vec![last_object(&canvas).id]);
    }

    #[test]
    fn a_click_creation_is_one_history_entry_that_undo_and_redo_restore() {
        let mut canvas = CanvasView::new();
        let before = canvas.session.runtime.objects().len();
        let entries = canvas.session.history.undo_len();

        canvas.set_tool(Tool::Rectangle);
        let at = point(240.0, 180.0);
        canvas.interaction = Interaction::PotentialCreate(CreateGesture {
            object_type: ObjectType::Rectangle,
            pointer_start_screen: at,
            pointer_start_world: at,
            current_world: at,
            moved: false,
        });
        canvas.finish_interaction(at);
        let created = last_object(&canvas).spool_id.clone();

        assert_eq!(canvas.session.runtime.objects().len(), before + 1);
        assert_eq!(
            canvas.session.history.undo_len(),
            entries + 1,
            "a click is one gesture and therefore one entry"
        );
        // Selecting the new object is runtime state and records nothing.
        assert_eq!(
            canvas.session.history.undo_len(),
            entries + 1,
            "tool switching and selection are not history"
        );

        canvas.session.undo().expect("undo");
        assert_eq!(canvas.session.runtime.objects().len(), before);
        canvas.session.redo().expect("redo");
        assert!(canvas
            .session
            .runtime
            .objects()
            .iter()
            .any(|object| object.spool_id == created));
    }

    #[test]
    fn repeated_clicks_keep_creating() {
        let mut canvas = CanvasView::new();
        canvas.set_tool(Tool::Rectangle);
        let before = canvas.session.runtime.objects().len();
        for index in 0..3 {
            let at = point(100.0 + index as f32 * 50.0, 100.0);
            canvas.interaction = Interaction::PotentialCreate(CreateGesture {
                object_type: ObjectType::Rectangle,
                pointer_start_screen: at,
                pointer_start_world: at,
                current_world: at,
                moved: false,
            });
            canvas.finish_interaction(at);
            // Each creation returns to Selection, so the tool is re-armed each
            // time exactly as a user's would be.
            canvas.set_tool(Tool::Rectangle);
        }
        assert_eq!(canvas.session.runtime.objects().len(), before + 3);
    }

    // -- Deleting from the canvas.

    #[test]
    fn deleting_the_canvas_selection_removes_it_and_one_history_entry_covers_it() {
        let mut canvas = CanvasView::new();
        let victims: Vec<_> = canvas
            .session
            .runtime
            .objects()
            .iter()
            .take(2)
            .map(|object| object.id)
            .collect();
        canvas.selection.replace(victims.clone());
        let entries = canvas.session.history.undo_len();
        let before = canvas.session.runtime.objects().len();

        assert!(canvas.delete_selected_objects());

        assert_eq!(canvas.session.runtime.objects().len(), before - 2);
        assert_eq!(
            canvas.session.history.undo_len(),
            entries + 1,
            "two objects, one gesture, one entry"
        );
        assert!(
            canvas.selection.ids().is_empty(),
            "and nothing deleted is left selected"
        );

        canvas.session.undo().expect("undo");
        assert_eq!(canvas.session.runtime.objects().len(), before);
        for victim in &victims {
            assert!(
                canvas.session.runtime.object(*victim).is_some(),
                "undo restored {victim:?}"
            );
        }
        canvas.session.redo().expect("redo");
        assert_eq!(canvas.session.runtime.objects().len(), before - 2);
    }

    #[test]
    fn the_delete_key_and_backspace_are_one_command_on_the_canvas() {
        // Both spellings reach the same `Command::Delete`, which is the only
        // deletion implementation: Canvas and Layers have no separate paths.
        let modifiers = gpui::Modifiers::default();
        for key in ["delete", "backspace"] {
            assert_eq!(
                crate::commands::resolve(key, modifiers, crate::commands::Scope::Editor),
                Some(crate::commands::Command::Delete),
                "{key} deletes the selection"
            );
        }
        // And not while a text buffer owns the key.
        assert_eq!(
            crate::commands::resolve("backspace", modifiers, crate::commands::Scope::TextEditing),
            None,
            "a caret outranks deletion"
        );
    }

    #[test]
    fn a_creation_then_delete_leaves_nothing_behind() {
        // The completion chain this milestone exists for: create, the new object
        // is selected, delete it, and the document is back where it started.
        let mut canvas = CanvasView::new();
        let before = canvas.session.runtime.objects().len();
        canvas.set_tool(Tool::Rectangle);
        let at = point(240.0, 180.0);
        canvas.interaction = Interaction::PotentialCreate(CreateGesture {
            object_type: ObjectType::Rectangle,
            pointer_start_screen: at,
            pointer_start_world: at,
            current_world: at,
            moved: false,
        });
        canvas.finish_interaction(at);

        let created = last_object(&canvas).id;
        assert_eq!(
            canvas.selection.ids(),
            vec![created],
            "the new object is selected, so Delete needs no further setup"
        );
        assert!(canvas.delete_selected_objects());
        assert_eq!(canvas.session.runtime.objects().len(), before);
        assert!(canvas
            .session
            .runtime
            .objects()
            .iter()
            .all(|object| object.id != created));
        assert!(canvas.selection.ids().is_empty());
    }

    #[test]
    fn sub_threshold_creation_gesture_creates_no_object() {
        let mut canvas = CanvasView::new();
        canvas.tool = Tool::Rectangle;
        canvas.interaction = Interaction::PotentialCreate(CreateGesture {
            object_type: ObjectType::Rectangle,
            pointer_start_screen: point(0.0, 0.0),
            pointer_start_world: point(100.0, 100.0),
            current_world: point(100.0, 100.0),
            moved: false,
        });

        canvas.update_interaction(point(3.0, 0.0));
        canvas.finish_interaction(point(3.0, 0.0));

        assert_eq!(canvas.session.runtime.objects().len(), 4);
        assert!(canvas.session.history.undo_len() == 0);
    }

    #[test]
    fn frame_rectangle_and_ellipse_gestures_create_selected_document_objects() {
        for (tool, object_type) in [
            (Tool::Frame, ObjectType::Frame),
            (Tool::Rectangle, ObjectType::Rectangle),
            (Tool::Ellipse, ObjectType::Ellipse),
        ] {
            let mut canvas = CanvasView::new();
            canvas.tool = tool;
            let start_screen = point(100.0, 100.0);
            let end_screen = point(160.0, 140.0);
            let start_world = canvas.camera.screen_to_world(start_screen);
            canvas.interaction = Interaction::PotentialCreate(CreateGesture {
                object_type,
                pointer_start_screen: start_screen,
                pointer_start_world: start_world,
                current_world: start_world,
                moved: false,
            });

            assert!(canvas.update_interaction(end_screen));
            assert!(canvas.interaction.preview().is_some());
            assert_eq!(canvas.session.runtime.objects().len(), 4);
            canvas.finish_interaction(end_screen);

            let created = canvas.session.runtime.objects().last().unwrap();
            assert_eq!(created.object_type, object_type);
            assert_eq!(created.size, size(60.0, 40.0));
            assert_eq!(canvas.selection.ids(), &[created.id]);
            assert_eq!(canvas.session.history.undo_len(), 1);
        }
    }

    #[test]
    fn escape_cancels_creation_without_history_or_selection_changes() {
        let mut canvas = CanvasView::new();
        canvas.selection.click_flat(Some(ObjectId::LANDING), false);
        canvas.interaction = Interaction::Creating(CreateGesture {
            object_type: ObjectType::Ellipse,
            pointer_start_screen: point(0.0, 0.0),
            pointer_start_world: point(100.0, 100.0),
            current_world: point(180.0, 160.0),
            moved: true,
        });
        let preview = canvas.interaction.preview();
        assert!(preview.is_some());

        assert!(canvas.cancel_interaction());

        assert_eq!(canvas.session.runtime.objects().len(), 4);
        assert!(canvas.session.history.undo_len() == 0);
        assert_eq!(canvas.selection.ids(), &[ObjectId::LANDING]);
        assert!(canvas.interaction.preview().is_none());
    }

    #[test]
    fn text_click_creates_and_selects_a_text_object_immediately() {
        let mut canvas = CanvasView::new();
        canvas.tool = Tool::Text;
        canvas.interaction = Interaction::PotentialCreate(CreateGesture {
            object_type: ObjectType::Text,
            pointer_start_screen: point(100.0, 120.0),
            pointer_start_world: point(100.0, 120.0),
            current_world: point(100.0, 120.0),
            moved: false,
        });

        canvas.finish_interaction(point(100.0, 120.0));

        let text = canvas.session.runtime.objects().last().unwrap();
        assert_eq!(text.object_type, ObjectType::Text);
        assert_eq!(text.text_content.as_deref(), Some("Type something"));
        assert_eq!(text.position, point(100.0, 120.0));
        assert_eq!(canvas.selection.ids(), &[text.id]);
        assert_eq!(canvas.session.history.undo_len(), 1);
    }

    #[test]
    fn created_object_can_be_selected_moved_and_resized() {
        let mut document = Document::default();
        let object = document.create_object(
            ObjectType::Rectangle,
            point(100.0, 100.0),
            size(120.0, 80.0),
            None,
        );
        let mut selection = Selection::default();
        selection.click_flat(Some(object.id), false);
        let snapshot = ObjectSnapshot {
            id: object.id,
            geometry: object.geometry(),
        };

        apply_move(&mut document, &[snapshot], point(25.0, 15.0));
        let moved = document.geometry(object.id).unwrap();
        let resized = resized_geometry(
            moved,
            ResizeHandle::BottomRight,
            point(20.0, 10.0),
            false,
            false,
        );
        document.set_geometry(object.id, resized);

        assert!(selection.contains(object.id));
        assert_eq!(
            document.geometry(object.id).unwrap().position,
            point(125.0, 115.0)
        );
        assert_eq!(
            document.geometry(object.id).unwrap().size,
            size(140.0, 90.0)
        );
    }

    #[test]
    fn document_hit_test_returns_hit_and_miss() {
        let document = Document::default();

        assert_eq!(
            document.hit_test(point(100.0, 100.0)),
            Some(ObjectId::LANDING)
        );
        assert_eq!(document.hit_test(point(440.0, 100.0)), None);
    }

    #[test]
    fn overlapping_objects_resolve_to_the_topmost_document_object() {
        let document = Document {
            objects: vec![
                frame(
                    ObjectId::LANDING,
                    node_id("spool-node-test-back"),
                    "Back",
                    0.0,
                    0.0,
                    100.0,
                    100.0,
                ),
                frame(
                    ObjectId::EDITOR,
                    node_id("spool-node-test-front"),
                    "Front",
                    25.0,
                    25.0,
                    100.0,
                    100.0,
                ),
            ],
            next_id: 5,
            next_node_id: 1,
            next_names: [1; 4],
            layer_structure_revision: 0,
        };

        assert_eq!(document.hit_test(point(50.0, 50.0)), Some(ObjectId::EDITOR));
    }

    #[test]
    fn screen_to_world_hit_test_works_at_non_default_zoom() {
        let mut camera = Camera::default();
        camera.resize(size(960.0, 720.0));
        camera.set_zoom_at_center(0.5);
        let document = Document::default();
        let world_point = point(100.0, 100.0);

        assert_eq!(
            document.hit_test(camera.screen_to_world(camera.world_to_screen(world_point))),
            Some(ObjectId::LANDING)
        );
    }

    #[test]
    fn hit_test_remains_correct_after_camera_pan() {
        let mut camera = Camera::default();
        camera.resize(size(960.0, 720.0));
        let document = Document::default();
        let world_point = point(100.0, 100.0);
        let start_offset = camera.offset;
        camera.pan_from(start_offset, point(40.0, 40.0), point(190.0, 95.0));

        assert_eq!(
            document.hit_test(camera.screen_to_world(camera.world_to_screen(world_point))),
            Some(ObjectId::LANDING)
        );
    }

    #[test]
    fn empty_click_clears_selection() {
        let mut selection = Selection::default();
        selection.click_flat(Some(ObjectId::LANDING), false);
        selection.click_flat(None, false);

        assert!(selection.is_empty());
    }

    #[test]
    fn shift_click_toggles_object_selection() {
        let mut selection = Selection::default();
        selection.click_flat(Some(ObjectId::LANDING), true);
        selection.click_flat(Some(ObjectId::EDITOR), true);
        assert_eq!(selection.ids(), &[ObjectId::LANDING, ObjectId::EDITOR]);

        selection.click_flat(Some(ObjectId::LANDING), true);

        assert_eq!(selection.ids(), &[ObjectId::EDITOR]);
    }

    fn test_geometry(x: f32, y: f32, width: f32, height: f32) -> ObjectGeometry {
        ObjectGeometry {
            position: point(x, y),
            size: size(width, height),
        }
    }

    #[test]
    fn moving_one_object_changes_its_position_by_world_delta() {
        let mut document = Document::default();
        let object = document.object(ObjectId::LANDING).unwrap();
        let snapshot = ObjectSnapshot {
            id: object.id,
            geometry: object.geometry(),
        };
        let start = snapshot.geometry.position;

        apply_move(&mut document, &[snapshot], point(40.0, -12.0));

        assert_eq!(
            document.object(ObjectId::LANDING).unwrap().position,
            point(start.x + 40.0, start.y - 12.0)
        );
    }

    #[test]
    fn moving_multiple_objects_preserves_their_relative_positions() {
        let mut document = Document::default();
        let ids = [ObjectId::LANDING, ObjectId::EDITOR];
        let snapshots: Vec<_> = ids
            .iter()
            .map(|id| {
                let object = document.object(*id).unwrap();
                ObjectSnapshot {
                    id: *id,
                    geometry: object.geometry(),
                }
            })
            .collect();
        let initial_delta = point(
            snapshots[1].geometry.position.x - snapshots[0].geometry.position.x,
            snapshots[1].geometry.position.y - snapshots[0].geometry.position.y,
        );

        apply_move(&mut document, &snapshots, point(40.0, 20.0));

        let landing = document.object(ObjectId::LANDING).unwrap();
        let editor = document.object(ObjectId::EDITOR).unwrap();
        assert_eq!(
            point(
                editor.position.x - landing.position.x,
                editor.position.y - landing.position.y,
            ),
            initial_delta
        );
    }

    #[test]
    fn pointer_movement_becomes_world_movement_at_non_default_zoom() {
        let mut camera = Camera::default();
        camera.resize(size(960.0, 720.0));
        camera.set_zoom_at_center(0.5);
        let pointer_start_screen = point(300.0, 220.0);
        let pointer_start_world = camera.screen_to_world(pointer_start_screen);
        let pointer_end_screen = point(350.0, 250.0);

        assert_eq!(
            movement_delta(camera, pointer_start_world, pointer_end_screen),
            point(100.0, 60.0)
        );
    }

    #[test]
    fn resize_screen_delta_scales_by_camera_zoom() {
        for zoom in [0.25, 0.5, 1.0, 2.0, 4.0] {
            let mut canvas = CanvasView::new();
            canvas.camera.resize(size(960.0, 720.0));
            canvas.camera.set_zoom_at_center(zoom);
            let object = canvas
                .session
                .runtime
                .object(ObjectId::LANDING)
                .expect("landing");
            let start_width = object.size.width;
            // Suspended: this test is about how far the pointer's screen travel
            // becomes world travel, and a snap would edit that distance.
            begin_live_resize(
                &mut canvas,
                ObjectId::LANDING,
                ResizeHandle::Right,
                point(300.0, 220.0),
                true,
            );

            canvas.update_interaction_with(point(350.0, 220.0), false, false);

            assert_eq!(
                canvas
                    .session
                    .runtime
                    .object(ObjectId::LANDING)
                    .unwrap()
                    .size
                    .width,
                start_width + 50.0 / zoom
            );
        }
    }

    #[test]
    fn drag_threshold_is_screen_space() {
        assert!(!drag_threshold_crossed(point(0.0, 0.0), point(3.0, 0.0)));
        assert!(drag_threshold_crossed(point(0.0, 0.0), point(4.0, 0.0)));
    }

    #[test]
    fn right_resize_changes_width_and_keeps_left_edge_fixed() {
        let start = test_geometry(10.0, 20.0, 100.0, 80.0);

        let resized = resized_geometry(start, ResizeHandle::Right, point(25.0, 0.0), false, false);

        assert_eq!(resized.position, start.position);
        assert_eq!(resized.size, size(125.0, 80.0));
    }

    #[test]
    fn left_resize_changes_position_and_width() {
        let start = test_geometry(10.0, 20.0, 100.0, 80.0);

        let resized = resized_geometry(start, ResizeHandle::Left, point(20.0, 0.0), false, false);

        assert_eq!(resized.position, point(30.0, 20.0));
        assert_eq!(resized.size, size(80.0, 80.0));
    }

    #[test]
    fn bottom_resize_changes_height_and_keeps_top_edge_fixed() {
        let start = test_geometry(10.0, 20.0, 100.0, 80.0);

        let resized = resized_geometry(start, ResizeHandle::Bottom, point(0.0, 18.0), false, false);

        assert_eq!(resized.position, start.position);
        assert_eq!(resized.size, size(100.0, 98.0));
    }

    #[test]
    fn top_resize_changes_position_and_height() {
        let start = test_geometry(10.0, 20.0, 100.0, 80.0);

        let resized = resized_geometry(start, ResizeHandle::Top, point(0.0, 20.0), false, false);

        assert_eq!(resized.position, point(10.0, 40.0));
        assert_eq!(resized.size, size(100.0, 60.0));
    }

    #[test]
    fn corner_resize_changes_both_dimensions() {
        let start = test_geometry(10.0, 20.0, 100.0, 80.0);

        let resized = resized_geometry(
            start,
            ResizeHandle::BottomRight,
            point(20.0, 15.0),
            false,
            false,
        );

        assert_eq!(resized.position, start.position);
        assert_eq!(resized.size, size(120.0, 95.0));
    }

    #[test]
    fn resize_enforces_minimum_size_without_flipping() {
        let start = test_geometry(10.0, 20.0, 100.0, 80.0);

        let right = resized_geometry(start, ResizeHandle::Right, point(-200.0, 0.0), false, false);
        let left = resized_geometry(start, ResizeHandle::Left, point(200.0, 0.0), false, false);
        let top = resized_geometry(start, ResizeHandle::Top, point(0.0, 200.0), false, false);

        assert_eq!(right.size.width, MIN_OBJECT_SIZE);
        assert_eq!(left.position.x, 90.0);
        assert_eq!(left.size.width, MIN_OBJECT_SIZE);
        assert_eq!(top.position.y, 80.0);
        assert_eq!(top.size.height, MIN_OBJECT_SIZE);
    }

    #[test]
    fn cancel_restores_original_geometry() {
        let mut document = Document::default();
        let object = document.object(ObjectId::LANDING).unwrap();
        let snapshot = ObjectSnapshot {
            id: object.id,
            geometry: object.geometry(),
        };
        let original = snapshot.geometry;
        let gesture = MoveGesture {
            pointer_start_screen: point(0.0, 0.0),
            pointer_start_world: point(0.0, 0.0),
            objects: vec![snapshot],
            selected_ids: vec![ObjectId::LANDING],
            click_selection: ClickSelection::SelectOnly(ObjectId::LANDING),
            duplicate: false,
            duplicates: Vec::new(),
            placements: Vec::new(),
            suspend_snap: false,
        };
        apply_move(&mut document, &gesture.objects, point(75.0, 30.0));
        let interaction = Interaction::Moving(gesture);

        interaction.restore(&mut document);

        assert_eq!(
            document.object(ObjectId::LANDING).unwrap().geometry(),
            original
        );
    }

    fn record_position(
        history: &mut SemanticHistory,
        document: &mut Document,
        id: ObjectId,
        x: f32,
    ) {
        let before = document.geometry(id).unwrap();
        let after = Geometry {
            position: point(x, before.position.y),
            size: before.size,
        };
        document.set_geometry(id, after);
        history.record(SemanticOperation::Runtime(DocumentCommand::geometry(vec![
            GeometryChange { id, before, after },
        ])));
    }

    /// Start a move gesture the way a pointer-down on a selected object does.
    fn begin_live_move(canvas: &mut CanvasView, ids: &[ObjectId]) {
        begin_live_move_with_snap(canvas, ids, false);
    }

    /// The same gesture, but with the pointer already `suspend_snap`.
    ///
    /// Modifier state is a property of the press, not of the movement, so a
    /// test that wants `⌘`-drag has to start the gesture with it held rather
    /// than flip it mid-flight.
    fn begin_live_move_with_snap(canvas: &mut CanvasView, ids: &[ObjectId], suspend_snap: bool) {
        begin_live_move_with_modifiers(canvas, ids, suspend_snap, false);
    }

    /// A move gesture with both press-time modifiers spelled out.
    ///
    /// `⌘` and `⌥` are properties of the press rather than of the movement, so a
    /// test that wants `⌘`-drag or `⌥`-drag has to start the gesture with the key
    /// held rather than flip it mid-flight.
    fn begin_live_move_with_modifiers(
        canvas: &mut CanvasView,
        ids: &[ObjectId],
        suspend_snap: bool,
        duplicate: bool,
    ) {
        let start = point(0.0, 0.0);
        canvas.interaction = Interaction::PotentialMove(MoveGesture {
            pointer_start_screen: start,
            pointer_start_world: start,
            objects: snapshots(&canvas.session.runtime, ids),
            selected_ids: ids.to_vec(),
            click_selection: ClickSelection::SelectOnly(ids[0]),
            suspend_snap,
            duplicate,
            duplicates: Vec::new(),
            placements: Vec::new(),
        });
    }

    /// Drive a gesture to `end` and commit it, exactly as pointer-up does.
    ///
    /// Returns whether the gesture was considered live, which is false for a
    /// click that never moved.
    fn drag_to(canvas: &mut CanvasView, end: Point<f32>) -> bool {
        let live = canvas.update_interaction(end);
        canvas.finish_interaction(end);
        live
    }

    /// Start a resize gesture the way a pointer-down on a handle does.
    ///
    /// The ids must already be the selection, exactly as the real pointer path
    /// requires before it will pick a handle — and note that "the selection", not
    /// "the one object", is what the gesture is built from: several ids become a
    /// gesture over their union box.
    fn begin_live_resize(
        canvas: &mut CanvasView,
        id: ObjectId,
        handle: ResizeHandle,
        at: Point<f32>,
        suspend_snap: bool,
    ) {
        begin_live_resize_of(canvas, &[id], handle, at, suspend_snap);
    }

    /// A resize gesture over the union box of `ids`.
    fn begin_live_resize_of(
        canvas: &mut CanvasView,
        ids: &[ObjectId],
        handle: ResizeHandle,
        at: Point<f32>,
        suspend_snap: bool,
    ) {
        for (index, id) in ids.iter().enumerate() {
            canvas.selection.click_flat(Some(*id), index > 0);
        }
        let members = snapshots(&canvas.session.runtime, ids);
        let bounds = transform_bounds(&members).expect("selection has bounds");
        canvas.interaction = Interaction::PotentialResize(ResizeGesture {
            pointer_start_screen: at,
            pointer_start_world: canvas.camera.screen_to_world(at),
            members,
            bounds,
            handle,
            suspend_snap,
        });
    }

    fn positions(canvas: &CanvasView, ids: &[ObjectId]) -> Vec<(f32, f32)> {
        ids.iter()
            .map(|id| {
                let g = canvas.session.runtime.geometry(*id).expect("object");
                (g.position.x, g.position.y)
            })
            .collect()
    }

    // ---------------------------------------------------------------------
    // Transform box and multi-selection resize
    // ---------------------------------------------------------------------

    /// Two boxes on round coordinates, so every expected number below is exact
    /// arithmetic and not a second implementation of the rule under test.
    ///
    /// `a` sits at the union box's origin, `b` at its far corner, which is the
    /// case that catches a member being mapped from the wrong reference point.
    /// Snapping is suspended throughout: these tests are about the mapping, and
    /// a snap would move the expected result.
    fn two_box_selection(canvas: &mut CanvasView) -> (ObjectId, ObjectId) {
        let a = canvas.session.runtime.create_object(
            ObjectType::Rectangle,
            point(0.0, 0.0),
            size(100.0, 50.0),
            None,
        );
        let b = canvas.session.runtime.create_object(
            ObjectType::Rectangle,
            point(200.0, 100.0),
            size(100.0, 50.0),
            None,
        );
        (a.id, b.id)
    }

    /// The union of `a` and `b` from `two_box_selection` is `(0, 0, 300, 150)`.
    fn drag_two_box_resize(
        canvas: &mut CanvasView,
        ids: (ObjectId, ObjectId),
        handle: ResizeHandle,
        to: Point<f32>,
        proportional: bool,
        from_center: bool,
    ) {
        begin_live_resize_of(canvas, &[ids.0, ids.1], handle, point(0.0, 0.0), true);
        canvas.update_interaction_with(to, proportional, from_center);
        canvas.finish_interaction(to);
    }

    fn geometry_of(canvas: &CanvasView, id: ObjectId) -> ObjectGeometry {
        canvas.session.runtime.geometry(id).expect("object")
    }

    #[test]
    fn a_single_selection_is_outlined_per_member_and_a_multi_selection_as_one_union() {
        let mut canvas = CanvasView::new();
        let ids = two_box_selection(&mut canvas);
        let document = canvas.session.runtime.clone();

        canvas.selection.click_flat(Some(ids.0), false);
        assert!(matches!(
            selection_chrome(&document, &canvas.selection),
            Some(Chrome::Members(_))
        ));

        canvas.selection.click_flat(Some(ids.1), true);
        match selection_chrome(&document, &canvas.selection) {
            Some(Chrome::Union(bounds)) => {
                assert_eq!(bounds, test_geometry(0.0, 0.0, 300.0, 150.0))
            }
            other => panic!("two objects get one union outline, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_selection_draws_no_chrome() {
        let canvas = CanvasView::new();

        assert!(selection_chrome(&canvas.session.runtime, &canvas.selection).is_none());
    }

    #[test]
    fn the_transform_box_of_one_object_is_that_object() {
        let mut canvas = CanvasView::new();
        canvas.selection.click_flat(Some(ObjectId::LANDING), false);

        assert_eq!(
            canvas.selection_transform_bounds(),
            Some(test_geometry(0.0, 24.0, 430.0, 286.0))
        );
    }

    #[test]
    fn the_transform_box_of_several_objects_is_their_union() {
        let mut canvas = CanvasView::new();
        for (index, id) in [ObjectId::LANDING, ObjectId::EDITOR]
            .into_iter()
            .enumerate()
        {
            canvas.selection.click_flat(Some(id), index > 0);
        }

        // Landing spans x 0..430, Editor x 454..764, both starting at y 24;
        // Editor is the shorter, so the union's bottom is Landing's.
        assert_eq!(
            canvas.selection_transform_bounds(),
            Some(test_geometry(0.0, 24.0, 764.0, 286.0))
        );
    }

    #[test]
    fn the_transform_box_of_three_objects_covers_all_three() {
        let mut canvas = CanvasView::new();
        for (index, id) in [ObjectId::LANDING, ObjectId::EDITOR, ObjectId::FEATURES]
            .into_iter()
            .enumerate()
        {
            canvas.selection.click_flat(Some(id), index > 0);
        }

        // Features reaches down to y 646, which is now the bottom of the union.
        assert_eq!(
            canvas.selection_transform_bounds(),
            Some(test_geometry(0.0, 24.0, 764.0, 622.0))
        );
    }

    #[test]
    fn an_empty_selection_has_no_transform_box() {
        let canvas = CanvasView::new();

        assert_eq!(canvas.selection_transform_bounds(), None);
        assert_eq!(transform_bounds(&[]), None);
    }

    #[test]
    fn a_multi_selection_offers_the_same_eight_handles_as_one_object() {
        let mut canvas = CanvasView::new();
        let (a, b) = two_box_selection(&mut canvas);
        canvas.selection.click_flat(Some(a), false);
        canvas.selection.click_flat(Some(b), true);

        let bounds = canvas
            .selection_transform_bounds()
            .expect("a selection has bounds");
        // Every handle lands on the union box, which is the whole point: the box
        // the user can grab is the box the drag will act on.
        assert_eq!(
            ResizeHandle::TopLeft.screen_position(canvas.camera, bounds),
            canvas.camera.world_to_screen(point(0.0, 0.0))
        );
        assert_eq!(
            ResizeHandle::BottomRight.screen_position(canvas.camera, bounds),
            canvas.camera.world_to_screen(point(300.0, 150.0))
        );
        assert_eq!(
            ResizeHandle::Right.screen_position(canvas.camera, bounds),
            canvas.camera.world_to_screen(point(300.0, 75.0))
        );
    }

    #[test]
    fn resizing_two_objects_from_the_right_edge_scales_only_the_x_axis() {
        let mut canvas = CanvasView::new();
        let ids = two_box_selection(&mut canvas);

        // Right edge +150: the union goes 300 -> 450 wide, so x scales by 1.5.
        // The y axis is untouched, so both members keep their heights and rows.
        drag_two_box_resize(
            &mut canvas,
            ids,
            ResizeHandle::Right,
            point(150.0, 0.0),
            false,
            false,
        );

        assert_eq!(
            geometry_of(&canvas, ids.0),
            test_geometry(0.0, 0.0, 150.0, 50.0)
        );
        assert_eq!(
            geometry_of(&canvas, ids.1),
            test_geometry(300.0, 100.0, 150.0, 50.0)
        );
    }

    #[test]
    fn resizing_two_objects_from_a_corner_scales_both_axes() {
        let mut canvas = CanvasView::new();
        let ids = two_box_selection(&mut canvas);

        // Bottom-right +150, +150: the union goes 300 -> 450 wide (x1.5) and
        // 150 -> 300 tall (x2), so the two axes scale independently.
        drag_two_box_resize(
            &mut canvas,
            ids,
            ResizeHandle::BottomRight,
            point(150.0, 150.0),
            false,
            false,
        );

        assert_eq!(
            geometry_of(&canvas, ids.0),
            test_geometry(0.0, 0.0, 150.0, 100.0)
        );
        // `b` sat 200 across and 100 down inside the old union, so it now sits
        // 300 across and 200 down inside the new one.
        assert_eq!(
            geometry_of(&canvas, ids.1),
            test_geometry(300.0, 200.0, 150.0, 100.0)
        );
    }

    #[test]
    fn resizing_three_objects_keeps_each_one_in_proportion_to_the_union() {
        let mut canvas = CanvasView::new();
        let a = canvas.session.runtime.create_object(
            ObjectType::Rectangle,
            point(0.0, 0.0),
            size(100.0, 100.0),
            None,
        );
        let b = canvas.session.runtime.create_object(
            ObjectType::Rectangle,
            point(100.0, 0.0),
            size(100.0, 100.0),
            None,
        );
        let c = canvas.session.runtime.create_object(
            ObjectType::Rectangle,
            point(50.0, 200.0),
            size(100.0, 100.0),
            None,
        );
        let ids = (a.id, b.id);
        // Union: x 0..200, y 0..300.
        canvas.selection.click_flat(Some(a.id), false);
        canvas.selection.click_flat(Some(b.id), true);
        canvas.selection.click_flat(Some(c.id), true);
        let members = vec![a.id, b.id, c.id];
        let start = snapshots(&canvas.session.runtime, &members);
        let bounds = transform_bounds(&start).unwrap();
        canvas.interaction = Interaction::PotentialResize(ResizeGesture {
            pointer_start_screen: point(0.0, 0.0),
            pointer_start_world: point(0.0, 0.0),
            members: start,
            bounds,
            handle: ResizeHandle::BottomRight,
            suspend_snap: true,
        });
        canvas.update_interaction_with(point(100.0, 300.0), false, false);
        canvas.finish_interaction(point(100.0, 300.0));

        // The union's x scales by 1.5 and its y by 2, so every member keeps its
        // proportion of the selection and `c` stays below the top pair rather
        // than being flattened onto them.
        assert_eq!(
            geometry_of(&canvas, a.id),
            test_geometry(0.0, 0.0, 150.0, 200.0)
        );
        assert_eq!(
            geometry_of(&canvas, b.id),
            test_geometry(150.0, 0.0, 150.0, 200.0)
        );
        assert_eq!(
            geometry_of(&canvas, c.id),
            test_geometry(75.0, 400.0, 150.0, 200.0)
        );
        let _ = ids;
    }

    #[test]
    fn resizing_an_edge_of_a_multi_selection_leaves_the_other_axis_alone() {
        let mut canvas = CanvasView::new();
        let ids = two_box_selection(&mut canvas);

        // Bottom edge +75: the union's height goes 150 -> 225, so heights scale
        // by 1.5 while widths and columns are untouched.
        drag_two_box_resize(
            &mut canvas,
            ids,
            ResizeHandle::Bottom,
            point(0.0, 75.0),
            false,
            false,
        );

        assert_eq!(
            geometry_of(&canvas, ids.0),
            test_geometry(0.0, 0.0, 100.0, 75.0)
        );
        assert_eq!(
            geometry_of(&canvas, ids.1),
            test_geometry(200.0, 150.0, 100.0, 75.0)
        );
    }

    #[test]
    fn a_multi_selection_resize_is_one_history_entry_that_undoes_and_redoes_as_a_unit() {
        let mut canvas = CanvasView::new();
        let ids = two_box_selection(&mut canvas);
        let before = (geometry_of(&canvas, ids.0), geometry_of(&canvas, ids.1));
        assert_eq!(
            canvas.session.history.undo_len(),
            0,
            "creating fixtures directly must not enter history"
        );

        drag_two_box_resize(
            &mut canvas,
            ids,
            ResizeHandle::BottomRight,
            point(150.0, 150.0),
            false,
            false,
        );

        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "one gesture on a multi-selection is one entry, not one per object"
        );
        let after = (geometry_of(&canvas, ids.0), geometry_of(&canvas, ids.1));

        assert!(canvas.session.undo().unwrap());
        assert_eq!(
            (geometry_of(&canvas, ids.0), geometry_of(&canvas, ids.1)),
            before,
            "undo restores every member together"
        );

        assert!(canvas.session.redo().unwrap());
        assert_eq!(
            (geometry_of(&canvas, ids.0), geometry_of(&canvas, ids.1)),
            after,
            "redo reapplies every member together"
        );
    }

    #[test]
    fn a_cancelled_multi_selection_resize_restores_everything_and_records_nothing() {
        let mut canvas = CanvasView::new();
        let ids = two_box_selection(&mut canvas);
        let before = (geometry_of(&canvas, ids.0), geometry_of(&canvas, ids.1));

        begin_live_resize_of(
            &mut canvas,
            &[ids.0, ids.1],
            ResizeHandle::BottomRight,
            point(0.0, 0.0),
            true,
        );
        canvas.update_interaction_with(point(150.0, 150.0), false, false);
        assert_ne!(
            (geometry_of(&canvas, ids.0), geometry_of(&canvas, ids.1)),
            before,
            "the resize really did move things before the cancel"
        );

        assert!(canvas.cancel_interaction());

        assert_eq!(
            (geometry_of(&canvas, ids.0), geometry_of(&canvas, ids.1)),
            before
        );
        assert!(!canvas.session.history.can_undo());
        assert!(!canvas.session.history.can_redo());
    }

    #[test]
    fn a_multi_selection_cannot_be_shrunk_below_the_minimum_or_flipped() {
        let mut canvas = CanvasView::new();
        let ids = two_box_selection(&mut canvas);

        // Drag the right edge far past the left one. The union must stop at the
        // minimum rather than inverting, exactly as a single object does.
        drag_two_box_resize(
            &mut canvas,
            ids,
            ResizeHandle::Right,
            point(-10_000.0, 0.0),
            false,
            false,
        );

        let bounds = transform_bounds(&[
            ObjectSnapshot {
                id: ids.0,
                geometry: geometry_of(&canvas, ids.0),
            },
            ObjectSnapshot {
                id: ids.1,
                geometry: geometry_of(&canvas, ids.1),
            },
        ])
        .unwrap();
        assert!(
            bounds.size.width >= MIN_OBJECT_SIZE,
            "no flip: {:?}",
            bounds
        );
        assert!(
            geometry_of(&canvas, ids.0).size.width > 0.0,
            "no member collapsed to nothing"
        );
        assert!(
            geometry_of(&canvas, ids.1).size.width > 0.0,
            "no member collapsed to nothing"
        );
    }

    #[test]
    fn a_multi_selection_resize_never_grows_without_moving_its_left_edge() {
        let mut canvas = CanvasView::new();
        let ids = two_box_selection(&mut canvas);
        let left_before = geometry_of(&canvas, ids.0).position.x;

        drag_two_box_resize(
            &mut canvas,
            ids,
            ResizeHandle::Right,
            point(60.0, 0.0),
            false,
            false,
        );

        assert_eq!(
            geometry_of(&canvas, ids.0).position.x,
            left_before,
            "the anchored edge stays anchored while the far edge moves"
        );
    }

    #[test]
    fn a_multi_selection_resize_scales_the_same_at_every_zoom() {
        // The gesture converts screen travel to world units through the camera, so
        // a resize must produce the same world geometry whatever the zoom. This is
        // the assertion that would catch a screen-space size sneaking in.
        for zoom in [0.25, 0.5, 1.0, 2.0, 4.0] {
            let mut canvas = CanvasView::new();
            canvas.camera.resize(size(960.0, 720.0));
            canvas.camera.set_zoom_at_center(zoom);
            let ids = two_box_selection(&mut canvas);

            drag_two_box_resize(
                &mut canvas,
                ids,
                ResizeHandle::Right,
                point(300.0, 0.0),
                false,
                false,
            );

            let scale = 1.0 + 1.0 / zoom;
            assert_eq!(
                geometry_of(&canvas, ids.0).size.width,
                100.0 * scale,
                "zoom {zoom}"
            );
            assert_eq!(
                geometry_of(&canvas, ids.1).position.x,
                200.0 * scale,
                "zoom {zoom}"
            );
        }
    }

    #[test]
    fn a_zero_width_selection_is_left_alone_rather_than_dividing_by_zero() {
        // Two objects sharing an x have a union with no width. There is no scale
        // to compute on that axis, so it must pass through unchanged instead of
        // producing an infinity.
        let mut canvas = CanvasView::new();
        let a = canvas.session.runtime.create_object(
            ObjectType::Rectangle,
            point(50.0, 0.0),
            size(0.0, 40.0),
            None,
        );
        let b = canvas.session.runtime.create_object(
            ObjectType::Rectangle,
            point(50.0, 100.0),
            size(0.0, 40.0),
            None,
        );
        let ids = (a.id, b.id);

        drag_two_box_resize(
            &mut canvas,
            ids,
            ResizeHandle::Bottom,
            point(0.0, 40.0),
            false,
            false,
        );

        for id in [ids.0, ids.1] {
            let geometry = geometry_of(&canvas, id);
            assert!(
                geometry.position.x.is_finite() && geometry.size.width.is_finite(),
                "no infinity leaked into {id:?}: {geometry:?}"
            );
        }
        assert_eq!(geometry_of(&canvas, ids.0).position.x, 50.0);
    }

    // ---------------------------------------------------------------------
    // Resize from centre
    // ---------------------------------------------------------------------

    #[test]
    fn alt_resizing_a_corner_grows_the_box_about_its_centre() {
        let start = test_geometry(0.0, 0.0, 100.0, 100.0);
        let centre = point(50.0, 50.0);

        let resized = resized_geometry(
            start,
            ResizeHandle::BottomRight,
            point(20.0, 20.0),
            false,
            true,
        );

        // Both edges moved by the same delta in the same direction, so the box
        // grew by 40 in each axis and the centre did not move.
        assert_eq!(resized, test_geometry(-20.0, -20.0, 140.0, 140.0));
        assert_eq!(
            point(
                resized.position.x + resized.size.width / 2.0,
                resized.position.y + resized.size.height / 2.0
            ),
            centre
        );
    }

    #[test]
    fn alt_resizing_an_edge_mirrors_the_opposite_edge_too() {
        let start = test_geometry(10.0, 10.0, 100.0, 60.0);

        let resized = resized_geometry(start, ResizeHandle::Right, point(30.0, 999.0), false, true);

        // The right edge follows the pointer; the left edge mirrors it so the
        // centre holds. The pointer's travel on the untouched axis is ignored,
        // which is what an edge handle has always done.
        assert_eq!(resized, test_geometry(-20.0, 10.0, 160.0, 60.0));
    }

    #[test]
    fn without_alt_the_opposite_edge_stays_anchored() {
        let start = test_geometry(0.0, 0.0, 100.0, 100.0);

        let resized = resized_geometry(
            start,
            ResizeHandle::BottomRight,
            point(20.0, 20.0),
            false,
            false,
        );

        assert_eq!(resized, test_geometry(0.0, 0.0, 120.0, 120.0));
    }

    #[test]
    fn shift_and_alt_together_resize_proportionally_about_the_centre() {
        let start = test_geometry(0.0, 0.0, 200.0, 100.0);

        // The x axis dominates, so `⇧` drives the scale off the width: asking for
        // 300 wide on a 200-wide box is 1.5x, and the height follows to 150. Then
        // `⌥` mirrors both axes about the centre, which doubles the travel and
        // moves both edges — hence 400 x 200 rather than 300 x 150. The 2:1 ratio
        // is the thing being preserved.
        let resized = resized_geometry(
            start,
            ResizeHandle::BottomRight,
            point(100.0, 0.0),
            true,
            true,
        );

        assert_eq!(resized, test_geometry(-100.0, -50.0, 400.0, 200.0));
    }

    #[test]
    fn alt_resize_still_honours_the_minimum_size() {
        let start = test_geometry(0.0, 0.0, 100.0, 100.0);

        let resized = resized_geometry(
            start,
            ResizeHandle::BottomRight,
            point(-500.0, -500.0),
            false,
            true,
        );

        assert!(
            resized.size.width >= MIN_OBJECT_SIZE && resized.size.height >= MIN_OBJECT_SIZE,
            "shrinking about the centre must not invert the box: {resized:?}"
        );
    }

    #[test]
    fn a_centre_resize_through_the_gesture_is_one_undoable_entry() {
        let mut canvas = CanvasView::new();
        let before = canvas.session.runtime.geometry(ObjectId::LANDING).unwrap();
        begin_live_resize(
            &mut canvas,
            ObjectId::LANDING,
            ResizeHandle::BottomRight,
            point(0.0, 0.0),
            true,
        );

        canvas.update_interaction_with(point(40.0, 0.0), false, true);
        canvas.finish_interaction(point(40.0, 0.0));

        let resized = canvas.session.runtime.geometry(ObjectId::LANDING).unwrap();
        assert_ne!(resized, before);
        assert!(
            resized.position.x < before.position.x,
            "the left edge moved out too, which is what makes it a centre resize"
        );
        assert_eq!(canvas.session.history.undo_len(), 1);
        assert!(canvas.session.undo().unwrap());
        assert_eq!(
            canvas.session.runtime.geometry(ObjectId::LANDING),
            Some(before)
        );
    }

    #[test]
    fn a_cancelled_centre_resize_restores_the_box_and_records_nothing() {
        let mut canvas = CanvasView::new();
        let before = canvas.session.runtime.geometry(ObjectId::LANDING).unwrap();
        begin_live_resize(
            &mut canvas,
            ObjectId::LANDING,
            ResizeHandle::BottomRight,
            point(0.0, 0.0),
            true,
        );
        canvas.update_interaction_with(point(120.0, 90.0), false, true);

        assert!(canvas.cancel_interaction());

        assert_eq!(
            canvas.session.runtime.geometry(ObjectId::LANDING),
            Some(before)
        );
        assert!(!canvas.session.history.can_undo());
    }

    // ---------------------------------------------------------------------
    // Alt-drag duplication
    // ---------------------------------------------------------------------

    #[test]
    fn alt_dragging_one_object_leaves_the_original_where_it_was() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let original = canvas.session.runtime.geometry(id).unwrap();
        let object_count = canvas.session.runtime.objects().len();

        begin_live_move_with_modifiers(&mut canvas, &[id], true, true);
        drag_to(&mut canvas, point(60.0, 25.0));

        assert_eq!(
            canvas.session.runtime.geometry(id),
            Some(original),
            "an alt-drag moves the copy, never the original"
        );
        assert_eq!(
            canvas.session.runtime.objects().len(),
            object_count + 1,
            "exactly one copy was made"
        );
    }

    #[test]
    fn alt_dragging_moves_the_copy_and_selects_it() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let original = canvas.session.runtime.geometry(id).unwrap();

        begin_live_move_with_modifiers(&mut canvas, &[id], true, true);
        drag_to(&mut canvas, point(60.0, 25.0));

        let copy = *canvas.selection.ids().first().expect("a copy is selected");
        assert_ne!(copy, id, "the selection follows the copy");
        assert_eq!(
            canvas.session.runtime.geometry(copy).unwrap().position,
            point(
                original.position.x + DUPLICATE_OFFSET + 60.0,
                original.position.y + DUPLICATE_OFFSET + 25.0
            )
        );
    }

    #[test]
    fn alt_dragging_a_multi_selection_copies_every_member_and_moves_them_as_one() {
        let mut canvas = CanvasView::new();
        let ids = [ObjectId::LANDING, ObjectId::EDITOR];
        let before: Vec<_> = ids
            .iter()
            .map(|id| canvas.session.runtime.geometry(*id).unwrap())
            .collect();
        let gap_before = point(
            before[1].position.x - before[0].position.x,
            before[1].position.y - before[0].position.y,
        );
        let count = canvas.session.runtime.objects().len();

        begin_live_move_with_modifiers(&mut canvas, &ids, true, true);
        drag_to(&mut canvas, point(40.0, 10.0));

        assert_eq!(
            canvas.session.runtime.objects().len(),
            count + 2,
            "one copy per selected object"
        );
        assert_eq!(canvas.selection.ids().len(), 2);
        let copies: Vec<_> = canvas
            .selection
            .ids()
            .iter()
            .map(|id| canvas.session.runtime.geometry(*id).unwrap())
            .collect();
        assert_eq!(
            point(
                copies[1].position.x - copies[0].position.x,
                copies[1].position.y - copies[0].position.y
            ),
            gap_before,
            "the copies keep the arrangement they were duplicated with"
        );
    }

    #[test]
    fn an_alt_drag_is_one_history_entry_that_undoes_and_redoes_the_whole_gesture() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let original = canvas.session.runtime.geometry(id).unwrap();
        let count = canvas.session.runtime.objects().len();

        begin_live_move_with_modifiers(&mut canvas, &[id], true, true);
        drag_to(&mut canvas, point(60.0, 25.0));

        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "the copies and their movement are one entry, not two"
        );

        assert!(canvas.session.undo().unwrap());
        assert_eq!(
            canvas.session.runtime.objects().len(),
            count,
            "undo takes the copies back out"
        );
        assert_eq!(
            canvas.session.runtime.geometry(id),
            Some(original),
            "and leaves the original exactly where it was"
        );

        assert!(canvas.session.redo().unwrap());
        assert_eq!(
            canvas.session.runtime.objects().len(),
            count + 1,
            "redo brings the copies back"
        );
        let copy = *canvas.selection.ids().first().expect("a copy is selected");
        assert_ne!(copy, id);
        assert_eq!(
            canvas.session.runtime.geometry(id),
            Some(original),
            "redo still does not move the original"
        );
    }

    #[test]
    fn cancelling_an_alt_drag_removes_the_copies_and_records_nothing() {
        let mut canvas = CanvasView::new();
        let ids = [ObjectId::LANDING, ObjectId::EDITOR];
        let before: Vec<_> = ids
            .iter()
            .map(|id| canvas.session.runtime.geometry(*id).unwrap())
            .collect();
        let count = canvas.session.runtime.objects().len();

        begin_live_move_with_modifiers(&mut canvas, &ids, true, true);
        canvas.update_interaction(point(70.0, 40.0));
        assert_eq!(
            canvas.session.runtime.objects().len(),
            count + 2,
            "the copies exist mid-gesture, which is what makes the cancel a real test"
        );

        assert!(canvas.cancel_interaction());

        assert_eq!(
            canvas.session.runtime.objects().len(),
            count,
            "escape must not leave half-created copies behind"
        );
        for (id, geometry) in ids.into_iter().zip(before) {
            assert_eq!(canvas.session.runtime.geometry(id), Some(geometry));
        }
        assert!(!canvas.session.history.can_undo());
        assert!(!canvas.session.history.can_redo());
    }

    #[test]
    fn alt_clicking_without_a_drag_duplicates_nothing() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let count = canvas.session.runtime.objects().len();

        // The pointer never travels far enough to be a drag.
        begin_live_move_with_modifiers(&mut canvas, &[id], true, true);
        canvas.finish_interaction(point(1.0, 1.0));

        assert_eq!(canvas.session.runtime.objects().len(), count);
        assert!(!canvas.session.history.can_undo());
    }

    #[test]
    fn duplicated_objects_get_their_own_identity_rather_than_a_renumbered_one() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;

        begin_live_move_with_modifiers(&mut canvas, &[id], true, true);
        drag_to(&mut canvas, point(10.0, 0.0));

        let copy = *canvas.selection.ids().first().unwrap();
        assert_ne!(copy, id, "a distinct object id");
        assert_ne!(
            id_node(&canvas, copy),
            id_node(&canvas, id),
            "and a distinct node id, so it is a new authored thing rather than the same one twice"
        );
    }

    fn id_node(canvas: &CanvasView, id: ObjectId) -> NodeId {
        canvas.session.runtime.object(id).unwrap().spool_id.clone()
    }

    #[test]
    fn a_plain_drag_is_still_a_single_geometry_entry_with_no_compound() {
        let mut canvas = CanvasView::new();
        let count = canvas.session.runtime.objects().len();

        begin_live_move(&mut canvas, &[ObjectId::LANDING]);
        drag_to(&mut canvas, point(40.0, 20.0));

        assert_eq!(canvas.session.history.undo_len(), 1);
        assert_eq!(
            canvas.session.runtime.objects().len(),
            count,
            "a plain drag creates nothing"
        );
        match canvas.session.history.peek_undo() {
            Some(SemanticOperation::Runtime(_)) => {}
            other => panic!("expected one plain runtime command, got {other:?}"),
        }
    }

    #[test]
    fn live_move_commits_exactly_one_entry_and_undo_redo_round_trips() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let before = canvas.session.runtime.geometry(id).unwrap();
        assert_eq!(canvas.session.history.undo_len(), 0);

        begin_live_move(&mut canvas, &[id]);
        drag_to(&mut canvas, point(37.0, 23.0));

        let moved = canvas.session.runtime.geometry(id).unwrap();
        assert_eq!(
            moved.position,
            point(before.position.x + 37.0, before.position.y + 23.0)
        );
        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "one committed gesture is one entry, not one per pointer event"
        );

        assert!(canvas.undo_history());
        assert_eq!(canvas.session.runtime.geometry(id).unwrap(), before);
        assert!(canvas.redo_history());
        assert_eq!(canvas.session.runtime.geometry(id).unwrap(), moved);
    }

    #[test]
    fn a_drag_near_a_neighbour_snap_into_line_and_say_so() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let before = canvas.session.runtime.geometry(id).unwrap();

        begin_live_move(&mut canvas, &[id]);
        // Editor's left edge is at 454; this drag puts Landing's left edge at
        // 456, two world pixels away at 100%.
        canvas.update_interaction(point(456.0, 0.0));

        assert_eq!(
            canvas.session.runtime.geometry(id).unwrap().position.x,
            454.0,
            "held in line with Editor's left edge"
        );
        assert_eq!(
            canvas.session.runtime.geometry(id).unwrap().position.y,
            before.position.y,
            "a purely horizontal drag must not also move vertically"
        );
        assert_eq!(canvas.snap_guides().len(), 1, "the snap explains itself");
        assert_eq!(canvas.snap_guides()[0].at, 454.0);

        canvas.finish_interaction(point(456.0, 0.0));
    }

    #[test]
    fn the_command_modifier_places_an_object_freely_and_draws_no_guide() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        begin_live_move_with_snap(&mut canvas, &[id], true);
        drag_to(&mut canvas, point(456.0, 0.0));

        let moved = canvas.session.runtime.geometry(id).unwrap();
        assert_eq!(
            moved.position.x, 456.0,
            "exactly where the pointer asked, not where the magnet wanted"
        );
        assert!(canvas.snap_guides().is_empty());
    }

    #[test]
    fn shift_constrains_a_move_to_the_axis_the_pointer_chose() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let start = canvas.session.runtime.geometry(id).unwrap().position;

        // Suspended snapping: these tests are about the axis constraint, and a
        // neighbouring edge nearby would answer a different question.
        begin_live_move_with_snap(&mut canvas, &[id], true);
        // Mostly horizontal with `⇧` held: only the x survives.
        canvas.update_interaction_with(point(60.0, 12.0), true, false);
        let horizontal = canvas.session.runtime.geometry(id).unwrap().position;
        assert_eq!(horizontal, point(start.x + 60.0, start.y));

        // Mostly vertical with `⇧` held: only the y survives.
        canvas.update_interaction_with(point(30.0, 60.0), true, false);
        let vertical = canvas.session.runtime.geometry(id).unwrap().position;
        assert_eq!(vertical, point(start.x, start.y + 60.0));
    }

    #[test]
    fn releasing_shift_mid_drag_lets_the_object_move_on_both_axes_again() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let start = canvas.session.runtime.geometry(id).unwrap().position;

        begin_live_move_with_snap(&mut canvas, &[id], true);
        canvas.update_interaction_with(point(40.0, 5.0), true, false);
        canvas.update_interaction_with(point(60.0, 12.0), false, false);
        assert_eq!(
            canvas.session.runtime.geometry(id).unwrap().position,
            point(start.x + 60.0, start.y + 12.0),
            "the constraint is sampled per movement, not latched at press time"
        );
    }

    #[test]
    fn shift_preserves_the_aspect_ratio_of_a_corner_resize() {
        let start = ObjectGeometry {
            position: point(0.0, 0.0),
            size: size(200.0, 100.0),
        };
        // 40 across a 200-wide box is a 1.2 scale, so the height becomes 120
        // rather than the 110 an unconstrained drag would have produced.
        let resized = resized_geometry(
            start,
            ResizeHandle::BottomRight,
            point(40.0, 10.0),
            true,
            false,
        );
        assert!(
            (resized.size.width - 240.0).abs() < 0.01,
            "{:?}",
            resized.size
        );
        assert!(
            (resized.size.height - 120.0).abs() < 0.01,
            "{:?}",
            resized.size
        );

        let free = resized_geometry(
            start,
            ResizeHandle::BottomRight,
            point(40.0, 10.0),
            false,
            false,
        );
        assert_eq!(free.size, size(240.0, 110.0));
    }

    #[test]
    fn shift_scales_a_resize_from_the_dominant_axis() {
        let start = ObjectGeometry {
            position: point(0.0, 0.0),
            size: size(200.0, 100.0),
        };
        // A tall drag on a short box: the height drives the scale.
        let resized = resized_geometry(
            start,
            ResizeHandle::BottomRight,
            point(10.0, 100.0),
            true,
            false,
        );
        assert_eq!(resized.size.height, 200.0, "doubled from 100 to 200");
        assert_eq!(resized.size.width, 400.0, "and the width follows");
    }

    #[test]
    fn snap_guides_disappear_when_the_gesture_ends() {
        let mut canvas = CanvasView::new();
        begin_live_move(&mut canvas, &[ObjectId::LANDING]);
        canvas.update_interaction(point(456.0, 0.0));
        assert_eq!(canvas.snap_guides().len(), 1, "held during the drag");
        canvas.finish_interaction(point(456.0, 0.0));
        assert!(
            canvas.snap_guides().is_empty(),
            "a guide that outlives its gesture is a line drawn on the document"
        );
    }

    /// Look at the world from its origin at 1x, showing `viewport` of it.
    fn camera_showing(viewport: Size<f32>) -> Camera {
        Camera {
            zoom: 1.0,
            offset: point(0.0, 0.0),
            viewport,
            initialized: true,
            pending_fit: None,
        }
    }

    #[test]
    fn a_resized_edge_snaps_and_says_so() {
        // Landing's bottom edge is at 310 and both Features and Mobile start at
        // 366, so dragging the corner handle 53 down puts it three short of
        // them — inside a snap width, and on a line two objects share.
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let before = canvas.session.runtime.geometry(id).unwrap();
        begin_live_resize(
            &mut canvas,
            id,
            ResizeHandle::BottomRight,
            point(0.0, 0.0),
            false,
        );

        canvas.update_interaction_with(point(0.0, 53.0), false, false);

        let after = canvas.session.runtime.geometry(id).unwrap();
        assert_eq!(
            after.size.height,
            before.size.height + 56.0,
            "pulled down onto the line the two frames share"
        );
        assert_eq!(
            after.position, before.position,
            "the left edge did not move"
        );
        assert_eq!(canvas.snap_guides().len(), 1, "{:?}", canvas.snap_guides());
        assert_eq!(canvas.snap_guides()[0].axis, snap::Axis::Horizontal);
        assert_eq!(canvas.snap_guides()[0].at, 366.0);
        // One guide spanning both frames, not one per frame: collinear snaps
        // are a single line.
        assert_eq!(
            canvas.snap_guides()[0].end,
            716.0,
            "{:?}",
            canvas.snap_guides()
        );
    }

    #[test]
    fn resize_guides_disappear_when_the_gesture_ends() {
        let mut canvas = CanvasView::new();
        begin_live_resize(
            &mut canvas,
            ObjectId::LANDING,
            ResizeHandle::BottomRight,
            point(0.0, 0.0),
            false,
        );
        canvas.update_interaction_with(point(0.0, 53.0), false, false);
        assert_eq!(canvas.snap_guides().len(), 1, "held during the resize");

        canvas.finish_interaction(point(0.0, 53.0));

        assert!(
            canvas.snap_guides().is_empty(),
            "a guide that outlives its gesture is a line drawn on the document"
        );
    }

    #[test]
    fn the_command_modifier_places_a_resized_edge_freely_and_draws_no_guide() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let before = canvas.session.runtime.geometry(id).unwrap();
        begin_live_resize(
            &mut canvas,
            id,
            ResizeHandle::BottomRight,
            point(0.0, 0.0),
            true,
        );

        canvas.update_interaction_with(point(0.0, 53.0), false, false);

        assert_eq!(
            canvas.session.runtime.geometry(id).unwrap().size.height,
            before.size.height + 53.0,
            "exactly where the pointer asked, not where the magnet wanted"
        );
        assert!(canvas.snap_guides().is_empty());
    }

    #[test]
    fn a_proportional_resize_asks_for_no_snap() {
        // `⇧` turns the pointer's travel into a scale factor before any edge
        // has a position, so there is no single edge whose correction would
        // survive the scale about to be applied to it. Asking for none is the
        // honest answer; guessing would mean re-deriving the scale here.
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let before = canvas.session.runtime.geometry(id).unwrap();
        begin_live_resize(
            &mut canvas,
            id,
            ResizeHandle::BottomRight,
            point(0.0, 0.0),
            false,
        );

        canvas.update_interaction_with(point(0.0, 53.0), true, false);

        let after = canvas.session.runtime.geometry(id).unwrap();
        assert_ne!(
            after.size.height,
            before.size.height + 56.0,
            "the unsnapped height, not the snapped one"
        );
        assert!(
            canvas.snap_guides().is_empty(),
            "{:?}",
            canvas.snap_guides()
        );
    }

    #[test]
    fn a_resize_never_snaps_to_the_object_being_resized() {
        // Its own edges are the nearest thing to itself, so a candidate set that
        // forgot to exclude it would report a snap on every single movement.
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        begin_live_resize(
            &mut canvas,
            id,
            ResizeHandle::BottomRight,
            point(0.0, 0.0),
            false,
        );

        canvas.update_interaction_with(point(40.0, 13.0), false, false);

        assert_eq!(
            canvas.session.runtime.geometry(id).unwrap().size.width,
            470.0,
            "no correction from its own left edge"
        );
        assert!(
            canvas.snap_guides().is_empty(),
            "{:?}",
            canvas.snap_guides()
        );
    }

    #[test]
    fn an_object_the_camera_cannot_see_does_not_pull_a_drag() {
        // Landing's right edge is at 430 and Editor's left edge at 454, so
        // dragging 21 to the right leaves Landing three short of Editor — well
        // inside a snap width, if Editor is anywhere the user can see it.
        let drag_21 = |canvas: &mut CanvasView| {
            begin_live_move(canvas, &[ObjectId::LANDING]);
            canvas.update_interaction(point(21.0, 0.0));
            canvas.snap_guides().to_vec()
        };

        let mut watched = CanvasView::new();
        watched.camera = camera_showing(size(960.0, 720.0));
        assert!(
            drag_21(&mut watched).iter().any(|guide| guide.at == 454.0),
            "Editor is on screen, so its left edge holds the drag in line"
        );

        let mut unobserved = CanvasView::new();
        // A 300x260 window at the origin sees Landing and nothing else: Editor's
        // left edge at 454 is past 300 of world plus the cull padding.
        unobserved.camera = camera_showing(size(300.0, 260.0));
        assert!(
            drag_21(&mut unobserved).is_empty(),
            "an off-screen object is not what the user meant to line up with"
        );
        assert_eq!(
            unobserved
                .session
                .runtime
                .geometry(ObjectId::LANDING)
                .unwrap()
                .position
                .x,
            21.0,
            "so the drag lands exactly where the pointer asked"
        );
    }

    #[test]
    fn a_constrained_move_is_not_pulled_by_the_axis_the_pointer_left_alone() {
        // Park Landing so its bottom edge sits three above Features' top edge,
        // then drag it right. Unconstrained, both axes snap; with `⇧` the drag
        // is horizontal-only and the vertical magnet must not reach for it.
        let drag_right_21 = |canvas: &mut CanvasView, constrain: bool| {
            canvas
                .session
                .runtime
                .set_position(ObjectId::LANDING, point(0.0, 77.0));
            begin_live_move(canvas, &[ObjectId::LANDING]);
            canvas.update_interaction_with(point(21.0, 0.0), constrain, false);
            canvas
                .session
                .runtime
                .geometry(ObjectId::LANDING)
                .unwrap()
                .position
        };

        let mut free = CanvasView::new();
        assert_eq!(
            drag_right_21(&mut free, false).y,
            80.0,
            "bottom snapped to 366"
        );
        assert_eq!(free.snap_guides().len(), 2, "{:?}", free.snap_guides());

        let mut constrained = CanvasView::new();
        assert_eq!(
            drag_right_21(&mut constrained, true).y,
            77.0,
            "`⇧` promised the box would not move vertically"
        );
        assert_eq!(
            constrained.snap_guides().len(),
            1,
            "and nothing is drawn for an axis that did not move: {:?}",
            constrained.snap_guides()
        );
        assert_eq!(constrained.snap_guides()[0].axis, snap::Axis::Vertical);
    }

    #[test]
    fn a_nudge_is_never_pulled_back_by_an_alignment() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        // Park the object two pixels from Editor's left edge, which is well
        // inside a snap width.
        canvas.session.runtime.set_position(id, point(452.0, 24.0));
        canvas.selection.click_flat(Some(id), false);
        let before = canvas.session.runtime.geometry(id).unwrap();

        assert!(canvas.nudge_selection(1.0, 0.0));

        assert_eq!(
            canvas.session.runtime.geometry(id).unwrap().position.x,
            before.position.x + 1.0,
            "an arrow key states a position; it does not ask where it should be"
        );
    }

    #[test]
    fn hovering_tracks_the_pointer_and_stops_when_a_gesture_starts() {
        let mut canvas = CanvasView::new();
        // Landing occupies world (0, 24) to (430, 310) at 100%.
        assert!(canvas.update_hover(point(10.0, 30.0)));
        assert_eq!(canvas.hovered(), Some(ObjectId::LANDING));
        assert!(
            !canvas.update_hover(point(11.0, 31.0)),
            "same object, no repaint"
        );
        assert!(canvas.update_hover(point(900.0, 700.0)));
        assert_eq!(canvas.hovered(), None, "empty canvas is not hoverable");

        assert!(canvas.update_hover(point(10.0, 30.0)));
        assert_eq!(canvas.hovered(), Some(ObjectId::LANDING));

        begin_live_move(&mut canvas, &[ObjectId::LANDING]);
        assert!(
            canvas.update_hover(point(10.0, 30.0)),
            "dragging replaces hover rather than layering it"
        );
        assert_eq!(canvas.hovered(), None);
    }

    #[test]
    fn only_the_command_modifier_suspends_snapping() {
        // The corpus is unambiguous that it is the command key, and equally that
        // no other modifier does this: `⇧` constrains and `⌥` duplicates. A
        // blanket "any modifier" rule would quietly take snapping away from the
        // gesture `⇧` is already changing.
        let mods = |shift: bool, alt: bool, control: bool, platform: bool| gpui::Modifiers {
            shift,
            alt,
            control,
            platform,
            ..Default::default()
        };
        assert!(!suspends_snap(mods(false, false, false, false)));
        assert!(!suspends_snap(mods(true, false, false, false)));
        assert!(!suspends_snap(mods(false, true, false, false)));
        assert!(suspends_snap(mods(false, false, true, false)));
        assert!(suspends_snap(mods(false, false, false, true)));
        assert!(
            suspends_snap(mods(true, false, true, false)),
            "`⇧⌘` is still the command key, held harder"
        );
    }

    #[test]
    fn an_object_does_not_snap_to_its_own_child() {
        let mut canvas = CanvasView::new();
        // Make Editor a child of Landing in the persistent structure, and put the
        // child 100 to the right of the parent so there is a real line to snap
        // to.
        let landing = node_id("spool-node-landing");
        let editor = node_id("spool-node-editor");
        let node = |id: NodeId, parent: Option<NodeId>, name: &str| {
            crate::source_document::StructuralNode {
                id,
                name: name.to_owned(),
                kind: "frame".to_owned(),
                parent,
                children: Vec::new(),
                source: crate::source_document::SourceBinding {
                    file: "index.html".to_owned(),
                    selector: name.to_owned(),
                },
            }
        };
        canvas.session.document.structure.nodes = vec![
            node(landing.clone(), None, "Landing"),
            node(editor.clone(), Some(landing), "Editor"),
        ];
        let parent = canvas
            .session
            .runtime
            .object(ObjectId::LANDING)
            .unwrap()
            .spool_id
            .clone();
        let child = canvas
            .session
            .runtime
            .object(ObjectId::EDITOR)
            .unwrap()
            .spool_id
            .clone();
        canvas
            .session
            .runtime
            .set_position(ObjectId::EDITOR, point(100.0, 24.0));
        let child_x = canvas
            .session
            .runtime
            .geometry(ObjectId::EDITOR)
            .unwrap()
            .position
            .x;

        // The parent's left edge lands two pixels from the child's — inside the
        // threshold — but the child moves with the parent, so aligning to it
        // would be aligning to the user.
        let targets = canvas.snap_targets(&[ObjectId::LANDING]);
        assert!(
            !targets
                .iter()
                .any(|rect| rect.left() == child_x && rect.x == child_x),
            "a descendant is not a snap target for its ancestor: {targets:?}"
        );
        assert!(
            targets.iter().any(|rect| rect.left() == 106.0),
            "an unrelated sibling still is: {targets:?}"
        );
        let _ = (parent, child);
    }

    #[test]
    fn live_resize_commits_exactly_one_entry_and_undo_redo_round_trips() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let before = canvas.session.runtime.geometry(id).unwrap();

        let start = point(0.0, 0.0);
        canvas.interaction = Interaction::PotentialResize(ResizeGesture {
            pointer_start_screen: start,
            pointer_start_world: start,
            members: snapshots(&canvas.session.runtime, &[id]),
            bounds: snapshots(&canvas.session.runtime, &[id])[0].geometry,
            handle: ResizeHandle::BottomRight,
            // Suspended: this test is about history, and a snap that adjusted
            // the drop point would make the expected geometry a moving target.
            suspend_snap: true,
        });
        drag_to(&mut canvas, point(30.0, 15.0));

        let resized = canvas.session.runtime.geometry(id).unwrap();
        assert_ne!(resized, before, "the live resize changed the object");
        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "one committed resize is one entry"
        );

        assert!(canvas.undo_history());
        assert_eq!(canvas.session.runtime.geometry(id).unwrap(), before);
        assert!(canvas.redo_history());
        assert_eq!(canvas.session.runtime.geometry(id).unwrap(), resized);
    }

    #[test]
    fn live_multi_object_move_is_one_entry_and_undo_restores_all() {
        let mut canvas = CanvasView::new();
        let ids = [ObjectId::LANDING, ObjectId::EDITOR, ObjectId::FEATURES];
        let before = positions(&canvas, &ids);

        begin_live_move(&mut canvas, &ids);
        drag_to(&mut canvas, point(25.0, 12.0));

        let moved = positions(&canvas, &ids);
        for (id, (x, y)) in moved.iter().enumerate() {
            assert_eq!(*x, before[id].0 + 25.0, "object {id} moved horizontally");
            assert_eq!(*y, before[id].1 + 12.0, "object {id} moved vertically");
        }
        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "three objects dragged together is still one semantic edit"
        );

        assert!(canvas.undo_history());
        assert_eq!(
            positions(&canvas, &ids),
            before,
            "undo restored every object"
        );
        assert!(canvas.redo_history());
        assert_eq!(positions(&canvas, &ids), moved);
    }

    #[test]
    fn live_gesture_cancel_restores_geometry_and_records_nothing() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let before = canvas.session.runtime.geometry(id).unwrap();

        begin_live_move(&mut canvas, &[id]);
        // Transient mutation happens exactly as it would during a real drag.
        assert!(canvas.update_interaction(point(80.0, 45.0)));
        assert_ne!(
            canvas.session.runtime.geometry(id).unwrap(),
            before,
            "the preview really moved the object"
        );

        assert!(canvas.cancel_interaction(), "the gesture was active");
        assert_eq!(
            canvas.session.runtime.geometry(id).unwrap(),
            before,
            "cancel restored the exact pre-gesture geometry"
        );
        assert_eq!(
            canvas.session.history.undo_len(),
            0,
            "a cancelled gesture must not become a history entry"
        );
        assert!(!canvas.session.history.can_undo());
        assert!(!canvas.session.history.can_redo());
    }

    #[test]
    fn live_selection_camera_pan_and_zoom_never_enter_history() {
        let mut canvas = CanvasView::new();
        // Commit one real edit so the stack is non-empty and any stray entry
        // from runtime state would be visible.
        begin_live_move(&mut canvas, &[ObjectId::LANDING]);
        drag_to(&mut canvas, point(10.0, 10.0));
        let depth = canvas.session.history.undo_len();
        assert_eq!(depth, 1);

        // Selection changes.
        canvas.selection.click_flat(Some(ObjectId::EDITOR), false);
        canvas.selection.click_flat(Some(ObjectId::FEATURES), true);
        // Camera changes: pan offset and zoom.
        canvas.camera.offset = point(-140.0, 92.0);
        canvas.camera.set_zoom_at_center(2.5);
        canvas.camera.fit();
        canvas.pan = Some(PanGesture {
            button: MouseButton::Left,
            pointer_start: point(0.0, 0.0),
            offset_start: point(0.0, 0.0),
        });

        assert_eq!(
            canvas.session.history.undo_len(),
            depth,
            "selection, camera, pan and zoom are runtime state, not history"
        );
        assert_eq!(canvas.selection.ids().len(), 2);
    }

    #[test]
    fn live_canvas_edit_then_rename_undoes_in_commit_order() {
        use crate::operations::SemanticOperation;
        use crate::source_document::{LamineStructure, RenameNode, SourceBinding, StructuralNode};

        let mut canvas = CanvasView::new();
        canvas.session.document.structure = LamineStructure {
            nodes: vec![StructuralNode {
                id: NodeId::new("spool-order").unwrap(),
                name: "Ordered".into(),
                kind: "frame".into(),
                parent: None,
                children: vec![],
                source: SourceBinding {
                    file: "index.html".into(),
                    selector: "[data-spool-id=\"spool-order\"]".into(),
                },
            }],
        };
        let id = ObjectId::LANDING;
        let resting = canvas.session.runtime.geometry(id).unwrap();

        // Opposite order from the sibling test: canvas edit FIRST.
        begin_live_move(&mut canvas, &[id]);
        drag_to(&mut canvas, point(18.0, 9.0));
        let moved = canvas.session.runtime.geometry(id).unwrap();

        canvas
            .session
            .execute(SemanticOperation::Rename(RenameNode {
                id: NodeId::new("spool-order").unwrap(),
                before: "Ordered".into(),
                after: "Reordered".into(),
            }))
            .unwrap();
        assert_eq!(canvas.session.history.undo_len(), 2);

        // First undo pops the rename (committed last) and must leave the
        // canvas move completely untouched.
        assert!(canvas.undo_history());
        assert_eq!(canvas.session.document.structure.nodes[0].name, "Ordered");
        assert_eq!(canvas.session.runtime.geometry(id).unwrap(), moved);

        // Second undo pops the canvas move.
        assert!(canvas.undo_history());
        assert_eq!(canvas.session.runtime.geometry(id).unwrap(), resting);
        assert_eq!(canvas.session.history.undo_len(), 0);

        // Redo replays in the same order.
        assert!(canvas.redo_history());
        assert_eq!(canvas.session.runtime.geometry(id).unwrap(), moved);
        assert!(canvas.redo_history());
        assert_eq!(canvas.session.document.structure.nodes[0].name, "Reordered");
    }

    #[test]
    fn live_no_op_gesture_records_nothing_and_preserves_redo() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;

        begin_live_move(&mut canvas, &[id]);
        // Pointer goes down and up in the same place: a real click, not a drag.
        assert!(
            !drag_to(&mut canvas, point(0.0, 0.0)),
            "a pointer that never moved is a click, not a drag"
        );
        assert_eq!(
            canvas.session.history.undo_len(),
            0,
            "a gesture that ends where it started is not a committed edit"
        );

        // A real edit, undone, leaves a redo branch.
        begin_live_move(&mut canvas, &[id]);
        drag_to(&mut canvas, point(12.0, 0.0));
        let moved_x = canvas.session.runtime.geometry(id).unwrap().position.x;
        assert!(canvas.undo_history());
        assert!(canvas.session.history.can_redo());

        // A no-op afterwards must not destroy that redo branch.
        begin_live_move(&mut canvas, &[id]);
        drag_to(&mut canvas, point(0.0, 0.0));
        assert!(
            canvas.session.history.can_redo(),
            "a no-op must not clear the redo branch"
        );
        assert!(canvas.redo_history());
        assert_eq!(
            canvas.session.runtime.geometry(id).unwrap().position.x,
            moved_x,
            "redo restored the undone drag"
        );
    }

    /// Architectural invariant, proven through the live editor path.
    ///
    /// This drives a real [`CanvasView`] through the real gesture entry
    /// points (`update_interaction` / `finish_interaction` / `undo`) rather
    /// than calling [`EditSession`] directly, so it fails if the canvas ever
    /// stops routing committed edits through the semantic boundary.
    ///
    /// The claim under test: a canvas move and a metadata rename live in one
    /// stack and undo as one LIFO sequence. If the canvas kept a private
    /// history beside the session, the rename would be invisible here.
    #[test]
    fn live_canvas_and_metadata_share_one_history_stack() {
        use crate::operations::SemanticOperation;
        use crate::source_document::{LamineStructure, RenameNode, SourceBinding, StructuralNode};

        let mut canvas = CanvasView::new();

        // Give the session a real node so a metadata rename has a target.
        canvas.session.document.structure = LamineStructure {
            nodes: vec![StructuralNode {
                id: NodeId::new("spool-shared").unwrap(),
                name: "Shared".into(),
                kind: "frame".into(),
                parent: None,
                children: vec![],
                source: SourceBinding {
                    file: "index.html".into(),
                    selector: "[data-spool-id=\"spool-shared\"]".into(),
                },
            }],
        };
        assert_eq!(canvas.session.history.undo_len(), 0);

        // --- Live canvas gesture: pointer down, move, pointer up. ---
        let origin = point(0.0, 0.0);
        let pointer_end = point(60.0, 40.0);
        let resting = canvas
            .session
            .runtime
            .geometry(ObjectId::LANDING)
            .unwrap()
            .position;
        let expected = point(resting.x + 60.0, resting.y + 40.0);
        canvas.interaction = Interaction::PotentialMove(MoveGesture {
            pointer_start_screen: origin,
            pointer_start_world: origin,
            objects: snapshots(&canvas.session.runtime, &[ObjectId::LANDING]),
            selected_ids: vec![ObjectId::LANDING],
            click_selection: ClickSelection::SelectOnly(ObjectId::LANDING),
            duplicate: false,
            duplicates: Vec::new(),
            placements: Vec::new(),
            suspend_snap: false,
        });
        assert!(canvas.update_interaction(pointer_end));
        canvas.finish_interaction(pointer_end);

        assert_eq!(
            canvas
                .session
                .runtime
                .geometry(ObjectId::LANDING)
                .unwrap()
                .position,
            expected,
            "the live gesture moved the object"
        );
        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "one entry for the move"
        );

        // --- A metadata rename on the same session, through the boundary. ---
        // `execute` is the single mutation entry point: it validates, records,
        // and applies. No separate apply call, or the rename would be applied
        // twice and the second attempt would correctly fail as stale.
        let rename = SemanticOperation::Rename(RenameNode {
            id: NodeId::new("spool-shared").unwrap(),
            before: "Shared".into(),
            after: "Renamed".into(),
        });
        assert!(canvas.session.execute(rename).unwrap());
        assert_eq!(canvas.session.document.structure.nodes[0].name, "Renamed");
        assert_eq!(
            canvas.session.history.undo_len(),
            2,
            "the rename and the move share one depth"
        );

        // --- Undo is strictly LIFO across both kinds. ---
        assert!(canvas.undo_history());
        assert_eq!(
            canvas.session.document.structure.nodes[0].name, "Shared",
            "the rename, committed second, undoes first"
        );
        assert_eq!(
            canvas
                .session
                .runtime
                .geometry(ObjectId::LANDING)
                .unwrap()
                .position,
            expected,
            "the move is untouched until the rename is undone"
        );

        assert!(canvas.undo_history());
        assert_eq!(
            canvas
                .session
                .runtime
                .geometry(ObjectId::LANDING)
                .unwrap()
                .position,
            resting,
            "then the live canvas move undoes"
        );
        assert_eq!(canvas.session.history.undo_len(), 0);

        // --- And redo replays both in the same order. ---
        assert!(canvas.redo_history());
        assert_eq!(
            canvas
                .session
                .runtime
                .geometry(ObjectId::LANDING)
                .unwrap()
                .position,
            expected
        );
        assert!(canvas.redo_history());
        assert_eq!(canvas.session.document.structure.nodes[0].name, "Renamed");
        assert_eq!(canvas.session.history.redo_len(), 0);
    }

    fn snapshots(document: &Document, ids: &[ObjectId]) -> Vec<ObjectSnapshot> {
        ids.iter()
            .map(|id| ObjectSnapshot {
                id: *id,
                geometry: document.geometry(*id).unwrap(),
            })
            .collect()
    }

    #[test]
    fn history_records_a_geometry_command() {
        let mut document = Document::default();
        let mut history = SemanticHistory::default();

        record_position(&mut history, &mut document, ObjectId::LANDING, 100.0);

        assert!(history.can_undo());
        assert!(!history.can_redo());
        assert_eq!(history.undo_len(), 1);
    }

    #[test]
    fn create_command_can_be_undone_and_redone_with_the_same_id() {
        let mut document = Document::default();
        let mut history = SemanticHistory::default();
        let created = document.create_object(
            ObjectType::Rectangle,
            point(40.0, 50.0),
            size(120.0, 80.0),
            None,
        );
        history.record(SemanticOperation::Runtime(DocumentCommand::insert(vec![
            document.placement(created.id).unwrap(),
        ])));
        assert_eq!(document.objects().len(), 5);

        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.objects().len(), 4);
        assert!(document.object(created.id).is_none());
        let after_undo =
            document.create_object(ObjectType::Ellipse, point(0.0, 0.0), size(50.0, 50.0), None);
        assert_ne!(after_undo.id, created.id);

        assert!(history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.object(created.id), Some(&created));
        assert_eq!(document.objects()[4].id, created.id);
        assert_eq!(document.objects().last().unwrap().id, after_undo.id);
    }

    #[test]
    fn create_move_undoes_geometry_before_creation() {
        let mut document = Document::default();
        let mut history = SemanticHistory::default();
        let object = document.create_object(
            ObjectType::Rectangle,
            point(40.0, 50.0),
            size(120.0, 80.0),
            None,
        );
        let initial = object.geometry();
        history.record(SemanticOperation::Runtime(DocumentCommand::insert(vec![
            document.placement(object.id).unwrap(),
        ])));
        let moved = Geometry {
            position: point(90.0, 110.0),
            size: initial.size,
        };
        document.set_geometry(object.id, moved);
        history.record(SemanticOperation::Runtime(DocumentCommand::geometry(vec![
            GeometryChange {
                id: object.id,
                before: initial,
                after: moved,
            },
        ])));

        history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert_eq!(document.geometry(object.id), Some(initial));
        assert_eq!(document.objects().len(), 5);
        history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert!(document.object(object.id).is_none());

        history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert_eq!(document.geometry(object.id), Some(initial));
        history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert_eq!(document.geometry(object.id), Some(moved));
    }

    #[test]
    fn undo_restores_a_move_snapshot() {
        let mut document = Document::default();
        let original = document.geometry(ObjectId::LANDING).unwrap();
        let mut history = SemanticHistory::default();
        record_position(&mut history, &mut document, ObjectId::LANDING, 100.0);

        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());

        assert_eq!(document.geometry(ObjectId::LANDING), Some(original));
    }

    #[test]
    fn redo_reapplies_a_move_snapshot() {
        let mut document = Document::default();
        let mut history = SemanticHistory::default();
        record_position(&mut history, &mut document, ObjectId::LANDING, 100.0);
        history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();

        assert!(history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());

        assert_eq!(
            document.geometry(ObjectId::LANDING).unwrap().position.x,
            100.0
        );
    }

    #[test]
    fn multiple_commands_undo_in_reverse_order() {
        let mut document = Document::default();
        let mut history = SemanticHistory::default();
        record_position(&mut history, &mut document, ObjectId::LANDING, 100.0);
        record_position(&mut history, &mut document, ObjectId::LANDING, 200.0);

        history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert_eq!(
            document.geometry(ObjectId::LANDING).unwrap().position.x,
            100.0
        );
        history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert_eq!(
            document.geometry(ObjectId::LANDING).unwrap().position.x,
            0.0
        );
    }

    #[test]
    fn multiple_commands_redo_in_forward_order() {
        let mut document = Document::default();
        let mut history = SemanticHistory::default();
        record_position(&mut history, &mut document, ObjectId::LANDING, 100.0);
        record_position(&mut history, &mut document, ObjectId::LANDING, 200.0);
        history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();

        history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert_eq!(
            document.geometry(ObjectId::LANDING).unwrap().position.x,
            100.0
        );
        history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert_eq!(
            document.geometry(ObjectId::LANDING).unwrap().position.x,
            200.0
        );
    }

    #[test]
    fn multi_object_move_commits_as_one_command_and_undoes_together() {
        let mut canvas = CanvasView::new();
        let ids = [ObjectId::LANDING, ObjectId::EDITOR];
        let before = snapshots(&canvas.session.runtime, &ids);
        let gesture = MoveGesture {
            pointer_start_screen: point(0.0, 0.0),
            pointer_start_world: point(0.0, 0.0),
            objects: before.clone(),
            selected_ids: ids.to_vec(),
            click_selection: ClickSelection::SelectOnly(ObjectId::LANDING),
            duplicate: false,
            duplicates: Vec::new(),
            placements: Vec::new(),
            suspend_snap: false,
        };
        canvas.interaction = Interaction::Moving(gesture);
        canvas.finish_interaction(point(50.0, 25.0));

        assert_eq!(canvas.session.history.undo_len(), 1);
        match &match canvas.session.history.peek_undo() {
            Some(SemanticOperation::Runtime(command)) => &command.operation,
            _ => panic!("expected a recorded canvas command"),
        } {
            CommandOperation::Geometry(changes) => assert_eq!(changes.len(), 2),
            _ => panic!("expected geometry command"),
        }
        assert_eq!(
            canvas.session.runtime.geometry(ids[0]).unwrap().position,
            point(50.0, 49.0)
        );
        assert_eq!(
            canvas.session.runtime.geometry(ids[1]).unwrap().position,
            point(504.0, 49.0)
        );

        canvas.session.undo().unwrap();
        for snapshot in before {
            assert_eq!(
                canvas.session.runtime.geometry(snapshot.id),
                Some(snapshot.geometry)
            );
        }
    }

    #[test]
    fn redo_restores_every_object_in_a_multi_object_move() {
        let mut canvas = CanvasView::new();
        let ids = [ObjectId::LANDING, ObjectId::EDITOR];
        let before = snapshots(&canvas.session.runtime, &ids);
        canvas.interaction = Interaction::Moving(MoveGesture {
            pointer_start_screen: point(0.0, 0.0),
            pointer_start_world: point(0.0, 0.0),
            objects: before,
            selected_ids: ids.to_vec(),
            click_selection: ClickSelection::SelectOnly(ObjectId::LANDING),
            duplicate: false,
            duplicates: Vec::new(),
            placements: Vec::new(),
            suspend_snap: false,
        });
        canvas.finish_interaction(point(50.0, 25.0));
        let after: Vec<_> = ids
            .iter()
            .map(|id| canvas.session.runtime.geometry(*id).unwrap())
            .collect();
        canvas.session.undo().unwrap();

        assert!(canvas.session.redo().unwrap());

        for (id, geometry) in ids.into_iter().zip(after) {
            assert_eq!(canvas.session.runtime.geometry(id), Some(geometry));
        }
    }

    #[test]
    fn new_command_invalidates_redo_branch() {
        let mut document = Document::default();
        let mut history = SemanticHistory::default();
        record_position(&mut history, &mut document, ObjectId::LANDING, 100.0);
        history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap();
        assert!(history.can_redo());

        record_position(&mut history, &mut document, ObjectId::LANDING, 250.0);

        assert!(!history.can_redo());
        assert_eq!(history.redo_len(), 0);
        assert_eq!(
            document.geometry(ObjectId::LANDING).unwrap().position.x,
            250.0
        );
    }

    #[test]
    fn no_op_geometry_change_is_not_recorded() {
        let mut history = SemanticHistory::default();
        let geometry = test_geometry(10.0, 20.0, 100.0, 80.0);

        history.record(SemanticOperation::Runtime(DocumentCommand::geometry(vec![
            GeometryChange {
                id: ObjectId::LANDING,
                before: geometry,
                after: geometry,
            },
        ])));

        assert!(!history.can_undo());
    }

    #[test]
    fn resize_interaction_can_be_undone_and_redone() {
        let mut canvas = CanvasView::new();
        let snapshot = snapshots(&canvas.session.runtime, &[ObjectId::LANDING])[0];
        canvas.interaction = Interaction::Resizing(ResizeGesture {
            pointer_start_screen: point(0.0, 0.0),
            pointer_start_world: point(0.0, 0.0),
            members: vec![snapshot],
            bounds: snapshot.geometry,
            handle: ResizeHandle::Right,
            suspend_snap: true,
        });
        canvas.finish_interaction(point(24.0, 0.0));
        let resized = canvas.session.runtime.geometry(ObjectId::LANDING).unwrap();

        assert_eq!(canvas.session.history.undo_len(), 1);
        assert!(canvas.session.undo().unwrap());
        assert_eq!(
            canvas.session.runtime.geometry(ObjectId::LANDING),
            Some(snapshot.geometry)
        );
        assert!(canvas.session.redo().unwrap());
        assert_eq!(
            canvas.session.runtime.geometry(ObjectId::LANDING),
            Some(resized)
        );
    }

    #[test]
    fn escape_cancellation_restores_geometry_without_history() {
        let mut document = Document::default();
        let snapshot = snapshots(&document, &[ObjectId::LANDING])[0];
        let gesture = MoveGesture {
            pointer_start_screen: point(0.0, 0.0),
            pointer_start_world: point(0.0, 0.0),
            objects: vec![snapshot],
            selected_ids: vec![ObjectId::LANDING],
            click_selection: ClickSelection::SelectOnly(ObjectId::LANDING),
            duplicate: false,
            duplicates: Vec::new(),
            placements: Vec::new(),
            suspend_snap: false,
        };
        apply_move(&mut document, &gesture.objects, point(80.0, 30.0));
        let interaction = Interaction::Moving(gesture);
        let history = SemanticHistory::default();

        interaction.restore(&mut document);

        assert_eq!(
            document.geometry(ObjectId::LANDING),
            Some(snapshot.geometry)
        );
        assert!(!history.can_undo());
        assert!(!history.can_redo());
    }

    #[test]
    fn click_or_return_to_start_does_not_create_history() {
        let mut canvas = CanvasView::new();
        let snapshot = snapshots(&canvas.session.runtime, &[ObjectId::LANDING])[0];
        canvas.interaction = Interaction::PotentialMove(MoveGesture {
            pointer_start_screen: point(10.0, 10.0),
            pointer_start_world: point(0.0, 0.0),
            objects: vec![snapshot],
            selected_ids: vec![ObjectId::LANDING],
            click_selection: ClickSelection::SelectOnly(ObjectId::LANDING),
            duplicate: false,
            duplicates: Vec::new(),
            placements: Vec::new(),
            suspend_snap: false,
        });
        canvas.finish_interaction(point(10.0, 10.0));
        assert!(!canvas.session.history.can_undo());

        canvas.interaction = Interaction::Moving(MoveGesture {
            pointer_start_screen: point(0.0, 0.0),
            pointer_start_world: point(0.0, 0.0),
            objects: vec![snapshot],
            selected_ids: vec![ObjectId::LANDING],
            click_selection: ClickSelection::SelectOnly(ObjectId::LANDING),
            duplicate: false,
            duplicates: Vec::new(),
            placements: Vec::new(),
            suspend_snap: false,
        });
        canvas.finish_interaction(point(0.0, 0.0));
        assert!(!canvas.session.history.can_undo());
    }

    #[test]
    fn selection_and_camera_changes_do_not_enter_history() {
        let mut canvas = CanvasView::new();
        canvas.selection.click_flat(Some(ObjectId::LANDING), false);
        canvas.selection.click_flat(Some(ObjectId::EDITOR), true);
        canvas.camera.zoom_at(2.0, point(100.0, 100.0));
        canvas
            .camera
            .pan_from(point(0.0, 0.0), point(0.0, 0.0), point(40.0, 20.0));
        canvas.camera.fit();

        assert!(!canvas.session.history.can_undo());
        assert!(!canvas.session.history.can_redo());
    }

    #[test]
    fn removing_and_restoring_object_preserves_its_original_index() {
        let mut document = Document::default();
        let first = document.create_object(
            ObjectType::Rectangle,
            point(10.0, 10.0),
            size(40.0, 30.0),
            None,
        );
        let second = document.create_object(
            ObjectType::Ellipse,
            point(60.0, 10.0),
            size(40.0, 30.0),
            None,
        );
        let expected = document.objects().to_vec();
        let deleted = document.remove_objects(&[first.id]);

        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].index, 4);
        assert_eq!(document.objects().last().unwrap().id, second.id);

        document.insert_objects(&deleted);

        assert_eq!(document.objects(), expected);
    }

    #[test]
    fn removing_multiple_objects_and_restoring_them_keeps_original_order() {
        let mut document = Document::default();
        let objects: Vec<_> = [ObjectType::Rectangle, ObjectType::Ellipse, ObjectType::Text]
            .into_iter()
            .enumerate()
            .map(|(index, object_type)| {
                document.create_object(
                    object_type,
                    point(index as f32 * 30.0, 20.0),
                    size(40.0, 30.0),
                    (object_type == ObjectType::Text).then(|| "remember me".to_string()),
                )
            })
            .collect();
        let expected = document.objects().to_vec();
        let deleted = document.remove_objects(&[objects[0].id, objects[2].id]);

        assert_eq!(
            deleted.iter().map(|item| item.index).collect::<Vec<_>>(),
            [4, 6]
        );
        document.insert_objects(&deleted);

        assert_eq!(document.objects(), expected);
    }

    #[test]
    fn duplicate_preserves_object_data_and_allocates_new_ids_and_names() {
        let mut document = Document::default();
        let original = document.create_object(
            ObjectType::Text,
            point(31.0, 47.0),
            size(180.0, 48.0),
            Some("Type something".to_string()),
        );

        let duplicates = document.duplicate_objects(&[original.id]);
        let duplicate = &duplicates[0].object;

        assert_ne!(duplicate.id, original.id);
        assert_ne!(duplicate.spool_id, original.spool_id);
        assert_eq!(duplicate.name, "Text 2");
        assert_eq!(duplicate.object_type, original.object_type);
        assert_eq!(duplicate.size, original.size);
        assert_eq!(duplicate.text_content, original.text_content);
        assert_eq!(duplicate.position, point(47.0, 63.0));
        assert_eq!(document.object(duplicate.id), Some(duplicate));
    }

    #[test]
    fn spool_node_identity_survives_history_and_is_unique_for_duplicates() {
        let mut document = Document::default();
        let original = document.create_object(
            ObjectType::Rectangle,
            point(10.0, 20.0),
            size(80.0, 50.0),
            None,
        );
        let original_node_id = original.spool_id.clone();
        let mut history = SemanticHistory::default();
        history.record(SemanticOperation::Runtime(DocumentCommand::insert(vec![
            document.placement(original.id).unwrap(),
        ])));

        let duplicate = document.duplicate_objects(&[original.id]).remove(0);
        assert_ne!(duplicate.object.spool_id, original_node_id);
        history.record(SemanticOperation::Runtime(DocumentCommand::insert(vec![
            duplicate.clone(),
        ])));
        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert!(history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());

        assert_eq!(
            document.object(original.id).unwrap().spool_id,
            original_node_id
        );
        assert_eq!(
            document.object(duplicate.object.id).unwrap().spool_id,
            duplicate.object.spool_id
        );
        assert_ne!(
            document.object(original.id).unwrap().spool_id,
            document.object(duplicate.object.id).unwrap().spool_id
        );
    }

    #[test]
    fn duplicate_preserves_each_supported_object_type() {
        for object_type in [
            ObjectType::Frame,
            ObjectType::Rectangle,
            ObjectType::Ellipse,
            ObjectType::Text,
        ] {
            let mut document = Document::default();
            let original = document.create_object(
                object_type,
                point(20.0, 30.0),
                size(100.0, 80.0),
                (object_type == ObjectType::Text).then(|| "Type something".to_string()),
            );

            let duplicates = document.duplicate_objects(&[original.id]);
            let duplicate = &duplicates[0].object;

            assert_eq!(duplicate.object_type, object_type);
            assert_ne!(duplicate.id, original.id);
        }
    }

    #[test]
    fn duplicate_preserves_relative_order_and_is_topmost_for_hit_testing() {
        let mut document = Document::default();
        let lower = document.create_object(
            ObjectType::Rectangle,
            point(20.0, 20.0),
            size(100.0, 80.0),
            None,
        );
        let upper = document.create_object(
            ObjectType::Ellipse,
            point(20.0, 20.0),
            size(100.0, 80.0),
            None,
        );
        let duplicates = document.duplicate_objects(&[upper.id, lower.id]);
        let duplicate_ids: Vec<_> = duplicates
            .iter()
            .map(|placement| placement.object.id)
            .collect();

        assert_eq!(document.objects().len(), 8);
        assert_eq!(document.objects()[4].id, lower.id);
        assert_eq!(document.objects()[5].id, upper.id);
        assert_eq!(document.objects()[6].id, duplicate_ids[0]);
        assert_eq!(document.hit_test(point(40.0, 40.0)), Some(duplicate_ids[1]));
        assert_eq!(
            document.objects()[4..]
                .iter()
                .map(|object| object.id)
                .collect::<Vec<_>>(),
            [lower.id, upper.id, duplicate_ids[0], duplicate_ids[1]]
        );
    }

    #[test]
    fn duplicate_multiple_objects_preserves_document_order_and_offsets_each_by_sixteen() {
        let mut document = Document::default();
        let first = document.create_object(
            ObjectType::Rectangle,
            point(12.0, 14.0),
            size(40.0, 50.0),
            None,
        );
        let second = document.create_object(
            ObjectType::Ellipse,
            point(90.0, 100.0),
            size(60.0, 70.0),
            None,
        );
        let duplicates = document.duplicate_objects(&[second.id, first.id]);

        assert_eq!(duplicates[0].object.object_type, ObjectType::Rectangle);
        assert_eq!(duplicates[0].object.position, point(28.0, 30.0));
        assert_eq!(duplicates[1].object.object_type, ObjectType::Ellipse);
        assert_eq!(duplicates[1].object.position, point(106.0, 116.0));
    }

    #[test]
    fn deletion_is_one_history_command_and_undo_restores_all_original_data() {
        let mut document = Document::default();
        let first = document.create_object(
            ObjectType::Text,
            point(15.0, 25.0),
            size(180.0, 48.0),
            Some("Type something".to_string()),
        );
        let second = document.create_object(
            ObjectType::Ellipse,
            point(210.0, 125.0),
            size(64.0, 72.0),
            None,
        );
        let expected = document.objects().to_vec();
        let mut history = SemanticHistory::default();
        let deleted = document.remove_objects(&[first.id, second.id]);
        history.record(SemanticOperation::Runtime(DocumentCommand::delete(deleted)));

        assert_eq!(history.undo_len(), 1);
        assert_eq!(document.objects().len(), 4);
        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.objects(), expected);
        assert_eq!(
            document.object(first.id).unwrap().text_content.as_deref(),
            Some("Type something")
        );
    }

    #[test]
    fn delete_undo_redo_restores_then_removes_same_object() {
        let mut document = Document::default();
        let created = document.create_object(
            ObjectType::Rectangle,
            point(20.0, 30.0),
            size(100.0, 50.0),
            None,
        );
        let mut history = SemanticHistory::default();
        history.record(SemanticOperation::Runtime(DocumentCommand::insert(vec![
            document.placement(created.id).unwrap(),
        ])));
        history.record(SemanticOperation::Runtime(DocumentCommand::delete(
            document.remove_objects(&[created.id]),
        )));

        assert!(document.object(created.id).is_none());
        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.object(created.id), Some(&created));
        assert!(history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert!(document.object(created.id).is_none());
    }

    #[test]
    fn duplicate_insert_undo_redo_keeps_duplicate_ids() {
        let mut document = Document::default();
        let original = document.create_object(
            ObjectType::Rectangle,
            point(30.0, 40.0),
            size(80.0, 60.0),
            None,
        );
        let mut history = SemanticHistory::default();
        let duplicate_objects = document.duplicate_objects(&[original.id]);
        let duplicate_id = duplicate_objects[0].object.id;
        history.record(SemanticOperation::Runtime(DocumentCommand::insert(
            duplicate_objects,
        )));
        assert_eq!(history.undo_len(), 1);

        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert!(document.object(original.id).is_some());
        assert!(document.object(duplicate_id).is_none());
        assert!(history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.objects().last().unwrap().id, duplicate_id);
    }

    #[test]
    fn deletion_followed_by_creation_invalidates_redo_branch() {
        let mut document = Document::default();
        let target = document.create_object(
            ObjectType::Rectangle,
            point(15.0, 15.0),
            size(40.0, 40.0),
            None,
        );
        let mut history = SemanticHistory::default();
        history.record(SemanticOperation::Runtime(DocumentCommand::delete(
            document.remove_objects(&[target.id]),
        )));
        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert!(history.can_redo());

        let created = document.create_object(
            ObjectType::Ellipse,
            point(80.0, 80.0),
            size(50.0, 50.0),
            None,
        );
        history.record(SemanticOperation::Runtime(DocumentCommand::insert(vec![
            document.placement(created.id).unwrap(),
        ])));

        assert!(!history.can_redo());
        assert!(document.object(target.id).is_some());
        assert!(document.object(created.id).is_some());
    }

    #[test]
    fn duplicate_then_move_undoes_movement_before_removing_duplicates() {
        let mut document = Document::default();
        let original = document.create_object(
            ObjectType::Rectangle,
            point(20.0, 30.0),
            size(80.0, 60.0),
            None,
        );
        let mut history = SemanticHistory::default();
        let duplicates = document.duplicate_objects(&[original.id]);
        let duplicate = duplicates[0].object.clone();
        history.record(SemanticOperation::Runtime(DocumentCommand::insert(
            duplicates,
        )));
        let before_move = duplicate.geometry();
        let after_move = Geometry {
            position: point(before_move.position.x + 22.0, before_move.position.y + 9.0),
            size: before_move.size,
        };
        document.set_geometry(duplicate.id, after_move);
        history.record(SemanticOperation::Runtime(DocumentCommand::geometry(vec![
            GeometryChange {
                id: duplicate.id,
                before: before_move,
                after: after_move,
            },
        ])));

        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.geometry(duplicate.id), Some(before_move));
        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert!(document.object(duplicate.id).is_none());
        assert!(document.object(original.id).is_some());
    }

    #[test]
    fn selection_delete_clears_ids_and_selection_reconciliation_drops_missing_objects() {
        let mut canvas = CanvasView::new();
        let created = canvas.session.runtime.create_object(
            ObjectType::Rectangle,
            point(10.0, 10.0),
            size(60.0, 40.0),
            None,
        );
        canvas.selection.click_flat(Some(created.id), false);
        assert!(canvas.delete_selected_objects());
        assert!(canvas.selection.is_empty());
        assert!(canvas.session.runtime.object(created.id).is_none());
        assert_eq!(canvas.session.history.undo_len(), 1);

        canvas.session.undo().unwrap();
        canvas.retain_existing_selection();
        assert!(canvas.selection.is_empty());
        canvas.session.redo().unwrap();
        canvas.retain_existing_selection();
        assert!(canvas.selection.is_empty());
    }

    #[test]
    fn duplication_selects_only_the_duplicates_and_is_one_command() {
        let mut canvas = CanvasView::new();
        let first = canvas.session.runtime.create_object(
            ObjectType::Rectangle,
            point(10.0, 10.0),
            size(60.0, 40.0),
            None,
        );
        let second = canvas.session.runtime.create_object(
            ObjectType::Ellipse,
            point(100.0, 100.0),
            size(60.0, 40.0),
            None,
        );
        canvas.selection.replace(vec![second.id, first.id]);

        assert!(canvas.duplicate_selected_objects());

        let selected = canvas.selection.ids();
        assert_eq!(selected.len(), 2);
        assert!(!selected.contains(&first.id));
        assert!(!selected.contains(&second.id));
        assert_eq!(canvas.session.history.undo_len(), 1);
        assert_eq!(
            canvas
                .session
                .runtime
                .object(selected[0])
                .unwrap()
                .object_type,
            ObjectType::Rectangle
        );
        assert_eq!(
            canvas
                .session
                .runtime
                .object(selected[1])
                .unwrap()
                .object_type,
            ObjectType::Ellipse
        );
        assert!(selected
            .iter()
            .all(|id| canvas.session.runtime.object(*id).is_some()));
    }

    #[test]
    fn deleting_multiple_selected_objects_is_one_command_and_undo_restores_them() {
        let mut canvas = CanvasView::new();
        let ids = [ObjectId::LANDING, ObjectId::EDITOR];
        let before = canvas.session.runtime.objects().to_vec();
        canvas.selection.replace(ids.to_vec());

        assert!(canvas.delete_selected_objects());

        assert_eq!(canvas.session.history.undo_len(), 1);
        assert!(canvas.selection.is_empty());
        assert!(canvas.session.runtime.object(ids[0]).is_none());
        assert!(canvas.session.runtime.object(ids[1]).is_none());
        assert!(canvas.session.undo().unwrap());
        assert_eq!(canvas.session.runtime.objects(), before);
    }

    #[test]
    fn duplicate_without_selection_and_delete_without_selection_are_no_ops() {
        let mut canvas = CanvasView::new();

        assert!(!canvas.delete_selected_objects());
        assert!(!canvas.duplicate_selected_objects());
        assert_eq!(canvas.session.runtime.objects().len(), 4);
        assert!(!canvas.session.history.can_undo());
    }

    // ── the commands the keyboard resolves to ────────────────────────────

    #[test]
    fn select_all_takes_the_roots_and_leaves_a_descendant_out_of_the_selection() {
        let mut canvas = CanvasView::new();
        // Make Editor a child of Landing, so the starter scene has one real
        // hierarchy in it rather than four unrelated objects. The node ids come
        // from the runtime objects rather than being written out, because that
        // is where they really came from.
        let landing = canvas
            .session
            .runtime
            .object(ObjectId::LANDING)
            .unwrap()
            .spool_id
            .clone();
        let editor = canvas
            .session
            .runtime
            .object(ObjectId::EDITOR)
            .unwrap()
            .spool_id
            .clone();
        let node = |id: NodeId, parent: Option<NodeId>, name: &str| {
            crate::source_document::StructuralNode {
                id,
                name: name.to_owned(),
                kind: "frame".to_owned(),
                parent,
                children: Vec::new(),
                source: crate::source_document::SourceBinding {
                    file: "index.html".to_owned(),
                    selector: name.to_owned(),
                },
            }
        };
        canvas.session.document.structure.nodes = vec![
            node(landing.clone(), None, "Landing"),
            node(editor.clone(), Some(landing), "Editor"),
        ];
        canvas.selection.replace(vec![ObjectId::LANDING]);

        canvas.select_roots();

        // An ancestor and its descendant are both selected at once only if
        // select-all ignores the hierarchy, and then every operation that reads
        // the selection has to guess which of the two was meant.
        assert!(canvas.selection.contains(ObjectId::LANDING));
        assert!(
            !canvas.selection.contains(ObjectId::EDITOR),
            "a nested object is reached through its root"
        );
        assert_eq!(canvas.selection.ids().len(), 3);
        // Selection is runtime state, so it is not a document edit.
        assert_eq!(canvas.session.history.undo_len(), 0);
    }

    #[test]
    fn select_all_covers_a_minted_object_because_nothing_claims_it() {
        let mut canvas = CanvasView::new();
        let minted = canvas
            .session
            .runtime
            .create_object(
                ObjectType::Rectangle,
                point(10.0, 10.0),
                size(40.0, 40.0),
                None,
            )
            .id;

        canvas.select_roots();

        // A locally minted object has no structural node at all. Reading the
        // hierarchy alone would skip it, and `⌘A` would then fail to select a
        // rectangle the user just drew.
        assert!(canvas.selection.contains(minted));
        assert_eq!(canvas.selection.ids().len(), 5);
    }

    #[test]
    fn select_all_records_no_history_and_select_all_is_undoable_nothing() {
        let mut canvas = CanvasView::new();
        canvas.select_roots();
        assert!(!canvas.session.history.can_undo());
        assert!(!canvas.session.history.can_redo());
    }

    #[test]
    fn the_zoom_step_keys_move_by_a_ratio_and_clamp_at_both_ends() {
        let mut canvas = CanvasView::new();
        canvas.camera.set_zoom_at_center(1.0);

        canvas.zoom_in();
        let after_in = canvas.zoom_percent();
        assert!(
            (119..=121).contains(&after_in),
            "one step in is a ratio, not a fixed number of points: {after_in}"
        );

        canvas.zoom_out();
        assert_eq!(canvas.zoom_percent(), 100);

        // Clamping is the camera's job, not the key's: holding `-` at the
        // minimum has to stay at the minimum rather than walk into nonsense.
        for _ in 0..40 {
            canvas.zoom_out();
        }
        let minimum = canvas.zoom_percent();
        canvas.zoom_out();
        assert_eq!(canvas.zoom_percent(), minimum, "the floor holds");

        for _ in 0..80 {
            canvas.zoom_in();
        }
        let maximum = canvas.zoom_percent();
        canvas.zoom_in();
        assert_eq!(canvas.zoom_percent(), maximum, "the ceiling holds");
    }

    #[test]
    fn a_zoom_step_is_camera_state_and_not_a_document_edit() {
        let mut canvas = CanvasView::new();
        canvas.camera.set_zoom_at_center(1.0);

        assert!(canvas.zoom_in());
        assert!(canvas.zoom_out());

        // Undoing a zoom must not undo the last thing the user drew. The camera
        // is not in history, in every product in the corpus.
        assert_eq!(canvas.session.history.undo_len(), 0);
        assert!(!canvas.session.history.can_undo());
    }

    #[test]
    fn escape_gives_up_an_in_flight_marquee_and_pan_as_well_as_a_gesture() {
        // A marquee and a pan have changed nothing yet, so Escape dropping them
        // is the whole of what it can do — and it still has to do it, or Escape
        // during a selection drag does nothing at all.
        let mut canvas = CanvasView::new();
        canvas.marquee = Some(MarqueeGesture {
            start: point(10.0, 10.0),
            current: point(80.0, 80.0),
            additive: false,
            initial_selection: Vec::new(),
        });
        canvas.pan = Some(PanGesture {
            button: MouseButton::Middle,
            pointer_start: point(0.0, 0.0),
            offset_start: point(0.0, 0.0),
        });
        assert!(canvas.is_manipulating());

        assert!(canvas.cancel_gesture());

        assert!(!canvas.is_manipulating());
        assert_eq!(canvas.session.history.undo_len(), 0);
        assert!(
            canvas.selection.is_empty(),
            "an abandoned marquee selects nothing"
        );
    }

    #[test]
    fn an_idle_canvas_has_no_gesture_for_escape_to_cancel() {
        let mut canvas = CanvasView::new();
        assert!(!canvas.is_manipulating());
        assert!(!canvas.cancel_gesture());
    }

    #[test]
    fn a_caret_key_with_no_text_buffer_leaves_the_verb_to_the_editor() {
        // `⌫` outside a text buffer is the editor-wide delete, so the canvas's
        // own handler must not also act on it. It used to ring the system bell
        // here, which meant deleting an object rang an error bell immediately
        // before the object disappeared — GPUI dispatches a key binding before
        // the shell's own key listener, so both ran.
        let mut canvas = CanvasView::new();
        assert!(!canvas.is_text_editing());
        assert!(
            !canvas.delete_a_character(false),
            "with no buffer there is no character to delete"
        );
        assert_eq!(canvas.session.runtime.objects().len(), 4);
    }

    #[test]
    fn a_caret_key_with_a_text_buffer_deletes_the_character_and_records_nothing() {
        let mut canvas = CanvasView::new();
        let object = create_test_text(&mut canvas, "Hello");
        canvas.selection.replace(vec![object.id]);
        canvas.text_edit = Some(TextEditState {
            id: object.id,
            original_text: "Hello".to_owned(),
            editing_text: "Hello".to_owned(),
            selected_range: 5..5,
            selection_reversed: false,
            marked_range: None,
            pointer_anchor: None,
        });

        assert!(canvas.delete_a_character(false));

        assert_eq!(canvas.text_edit.as_ref().unwrap().editing_text, "Hell");
        assert!(
            canvas.delete_a_character(true),
            "`⌦` at the end has nothing to delete forward, but the buffer is open, so the verb is claimed"
        );
        assert_eq!(canvas.text_edit.as_ref().unwrap().editing_text, "Hell");
        // The buffer is not the document yet: one text session is one undo step,
        // and it is recorded when the session closes.
        assert_eq!(canvas.session.history.undo_len(), 0);
    }

    #[test]
    fn object_types_receive_their_expected_default_styles() {
        let rectangle = default_style(ObjectType::Rectangle);
        let ellipse = default_style(ObjectType::Ellipse);
        let frame_style = default_style(ObjectType::Frame);
        let text = default_style(ObjectType::Text);

        assert_eq!(
            rectangle.fill.unwrap().color.to_rgb(),
            theme::SURFACE_RAISED
        );
        assert_eq!(rectangle.stroke.unwrap().color.to_rgb(), theme::BORDER);
        assert_eq!(rectangle.stroke.unwrap().width, 1.0);
        assert_eq!(ellipse, rectangle);
        assert_eq!(frame_style.fill.unwrap().color.to_rgb(), theme::PAPER);
        assert_eq!(frame_style.stroke.unwrap().color.to_rgb(), theme::BORDER);
        assert_eq!(frame_style.stroke.unwrap().width, 1.0);
        assert_eq!(
            text,
            ObjectStyle {
                fill: None,
                stroke: None,
                ..ObjectStyle::default()
            }
        );
    }

    fn apply_style_to_document(
        document: &mut Document,
        history: &mut SemanticHistory,
        ids: &[ObjectId],
        edit: StyleEdit,
    ) {
        let changes: Vec<_> = ids
            .iter()
            .filter_map(|id| {
                let before = document.appearance(*id)?;
                let after = edited_style(before, edit);
                (before != after).then_some(StyleChange {
                    id: *id,
                    before,
                    after,
                })
            })
            .collect();
        for change in &changes {
            document.set_appearance(change.id, change.after);
        }
        history.record(SemanticOperation::Runtime(DocumentCommand::style(changes)));
    }

    #[test]
    fn fill_and_stroke_properties_can_be_changed_or_disabled() {
        let mut document = Document::default();
        let object = document.create_object(
            ObjectType::Rectangle,
            point(10.0, 10.0),
            size(80.0, 60.0),
            None,
        );
        let green = Color::from_rgb(theme::SAGE);
        let red = Color::from_rgb(0xc45d5d);
        let mut history = SemanticHistory::default();

        apply_style_to_document(
            &mut document,
            &mut history,
            &[object.id],
            StyleEdit::Fill(Some(green)),
        );
        assert_eq!(
            document.object(object.id).unwrap().fill,
            Some(Fill { color: green })
        );
        apply_style_to_document(
            &mut document,
            &mut history,
            &[object.id],
            StyleEdit::Fill(None),
        );
        assert_eq!(document.object(object.id).unwrap().fill, None);
        apply_style_to_document(
            &mut document,
            &mut history,
            &[object.id],
            StyleEdit::Stroke(Some(red)),
        );
        assert_eq!(
            document.object(object.id).unwrap().stroke.unwrap().color,
            red
        );
        apply_style_to_document(
            &mut document,
            &mut history,
            &[object.id],
            StyleEdit::StrokeWidth(4.0),
        );
        assert_eq!(
            document.object(object.id).unwrap().stroke.unwrap().width,
            4.0
        );
        apply_style_to_document(
            &mut document,
            &mut history,
            &[object.id],
            StyleEdit::Stroke(None),
        );
        assert_eq!(document.object(object.id).unwrap().stroke, None);
    }

    #[test]
    fn style_command_undo_redo_and_no_op_filtering_work() {
        let mut document = Document::default();
        let object = document.create_object(
            ObjectType::Rectangle,
            point(10.0, 10.0),
            size(80.0, 60.0),
            None,
        );
        let default = document.style(object.id).unwrap();
        let mut history = SemanticHistory::default();
        let blue = Color::from_rgb(0x6689c7);

        apply_style_to_document(
            &mut document,
            &mut history,
            &[object.id],
            StyleEdit::Fill(Some(blue)),
        );
        let changed = document.style(object.id).unwrap();
        assert_eq!(history.undo_len(), 1);
        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.style(object.id), Some(default));
        assert!(history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.style(object.id), Some(changed));

        let commands = history.undo_len();
        apply_style_to_document(
            &mut document,
            &mut history,
            &[object.id],
            StyleEdit::Fill(Some(blue)),
        );
        assert_eq!(history.undo_len(), commands);
    }

    #[test]
    fn style_changes_are_one_command_for_multi_selection_and_preserve_selection() {
        let mut canvas = CanvasView::new();
        let rectangle = canvas.session.runtime.create_object(
            ObjectType::Rectangle,
            point(10.0, 10.0),
            size(80.0, 60.0),
            None,
        );
        let text = canvas.session.runtime.create_object(
            ObjectType::Text,
            point(110.0, 10.0),
            size(80.0, 60.0),
            Some("Type something".to_string()),
        );
        canvas.selection.replace(vec![rectangle.id, text.id]);
        let selected = canvas.selection.ids().to_vec();
        let fill = Color::from_rgb(0x6689c7);

        assert!(canvas.apply_selected_style(StyleEdit::Fill(Some(fill))));

        assert_eq!(canvas.session.history.undo_len(), 1);
        assert_eq!(canvas.selection.ids(), selected);
        assert!(selected.iter().all(|id| {
            canvas.session.runtime.object(*id).unwrap().fill == Some(Fill { color: fill })
        }));
        assert!(matches!(match canvas.session.history.peek_undo() {
                Some(SemanticOperation::Runtime(command)) => &command.operation,
                _ => panic!("expected a recorded canvas command"),
            }, CommandOperation::Style(ref changes) if changes.len() == 2));

        let stroke = Color::from_rgb(0xc45d5d);
        assert!(canvas.apply_selected_style(StyleEdit::Stroke(Some(stroke))));
        assert!(selected.iter().all(|id| {
            canvas
                .session
                .runtime
                .object(*id)
                .unwrap()
                .stroke
                .map(|value| value.color)
                == Some(stroke)
        }));
        assert_eq!(canvas.session.history.undo_len(), 2);
    }

    #[test]
    fn style_change_then_move_undoes_in_command_order() {
        let mut document = Document::default();
        let object = document.create_object(
            ObjectType::Rectangle,
            point(20.0, 30.0),
            size(80.0, 60.0),
            None,
        );
        let mut history = SemanticHistory::default();
        let default_style = document.style(object.id).unwrap();
        apply_style_to_document(
            &mut document,
            &mut history,
            &[object.id],
            StyleEdit::Fill(Some(Color::from_rgb(0x6689c7))),
        );
        let styled = document.style(object.id).unwrap();
        record_position(&mut history, &mut document, object.id, 140.0);

        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.object(object.id).unwrap().position.x, 20.0);
        assert_eq!(document.style(object.id), Some(styled));
        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.style(object.id), Some(default_style));
    }

    #[test]
    fn style_is_preserved_by_duplicate_delete_restore_and_insertion_history() {
        let mut document = Document::default();
        let original = document.create_object(
            ObjectType::Text,
            point(20.0, 30.0),
            size(180.0, 48.0),
            Some("styled text".to_string()),
        );
        let mut history = SemanticHistory::default();
        let fill = Color::from_rgb(0x9576b8);
        apply_style_to_document(
            &mut document,
            &mut history,
            &[original.id],
            StyleEdit::Fill(Some(fill)),
        );
        let duplicate = document.duplicate_objects(&[original.id]);
        let duplicate_object = duplicate[0].object.clone();
        assert_eq!(duplicate_object.fill, Some(Fill { color: fill }));
        assert_eq!(
            duplicate_object.text_content.as_deref(),
            Some("styled text")
        );
        history.record(SemanticOperation::Runtime(DocumentCommand::insert(
            duplicate,
        )));

        history.record(SemanticOperation::Runtime(DocumentCommand::delete(
            document.remove_objects(&[original.id]),
        )));
        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(
            document.object(original.id).unwrap().fill,
            Some(Fill { color: fill })
        );
        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert!(document.object(duplicate_object.id).is_none());
        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(
            document.object(original.id).unwrap().fill,
            default_style(ObjectType::Text).fill
        );
    }

    fn create_test_text(canvas: &mut CanvasView, text: &str) -> DesignObject {
        canvas.session.runtime.create_object(
            ObjectType::Text,
            point(45.0, 60.0),
            size(180.0, 48.0),
            Some(text.to_owned()),
        )
    }

    #[test]
    fn text_objects_have_default_content_and_document_text_api_mutates_only_text() {
        let mut document = Document::default();
        let object = document.create_object(
            ObjectType::Text,
            point(0.0, 0.0),
            size(180.0, 48.0),
            Some("Type something".to_owned()),
        );
        let geometry = object.geometry();
        let style = document.style(object.id).unwrap();

        assert_eq!(document.text_content(object.id), Some("Type something"));
        assert!(document.set_text_content(object.id, "Hello world".to_owned()));
        assert_eq!(document.text_content(object.id), Some("Hello world"));
        assert_eq!(document.geometry(object.id), Some(geometry));
        assert_eq!(document.style(object.id), Some(style));
        assert!(!document.set_text_content(ObjectId::LANDING, "not text".to_owned()));
    }

    #[test]
    fn text_change_command_undoes_and_redoes_exact_content() {
        let mut document = Document::default();
        let object = document.create_object(
            ObjectType::Text,
            point(0.0, 0.0),
            size(180.0, 48.0),
            Some("before".to_owned()),
        );
        let mut history = SemanticHistory::default();
        document.set_text_content(object.id, "after 🧵".to_owned());
        history.record(SemanticOperation::Runtime(DocumentCommand::text(vec![
            TextChange {
                id: object.id,
                before: "before".to_owned(),
                after: "after 🧵".to_owned(),
            },
        ])));

        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.text_content(object.id), Some("before"));
        assert!(history
            .redo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert_eq!(document.text_content(object.id), Some("after 🧵"));
    }

    #[test]
    fn unchanged_text_change_is_not_recorded_and_new_edit_clears_redo() {
        let mut document = Document::default();
        let object = document.create_object(
            ObjectType::Text,
            point(0.0, 0.0),
            size(180.0, 48.0),
            Some("same".to_owned()),
        );
        let mut history = SemanticHistory::default();
        history.record(SemanticOperation::Runtime(DocumentCommand::text(vec![
            TextChange {
                id: object.id,
                before: "same".to_owned(),
                after: "same".to_owned(),
            },
        ])));
        assert!(!history.can_undo());

        document.set_text_content(object.id, "edited".to_owned());
        history.record(SemanticOperation::Runtime(DocumentCommand::text(vec![
            TextChange {
                id: object.id,
                before: "same".to_owned(),
                after: "edited".to_owned(),
            },
        ])));
        assert!(history
            .undo(&mut OperationTarget::Runtime(&mut document))
            .unwrap());
        assert!(history.can_redo());
        document.set_text_content(object.id, "new edit".to_owned());
        history.record(SemanticOperation::Runtime(DocumentCommand::text(vec![
            TextChange {
                id: object.id,
                before: "same".to_owned(),
                after: "new edit".to_owned(),
            },
        ])));
        assert!(!history.can_redo());
        assert_eq!(document.text_content(object.id), Some("new edit"));
    }

    #[test]
    fn text_content_survives_duplicate_and_delete_restore() {
        let mut document = Document::default();
        let object = document.create_object(
            ObjectType::Text,
            point(0.0, 0.0),
            size(180.0, 48.0),
            Some("keep 🧵".to_owned()),
        );
        let duplicate = document.duplicate_objects(&[object.id]).remove(0);
        assert_eq!(document.text_content(duplicate.object.id), Some("keep 🧵"));
        let removed = document.remove_objects(&[object.id]);
        document.insert_objects(&removed);
        assert_eq!(document.text_content(object.id), Some("keep 🧵"));
    }

    #[test]
    fn multiple_character_edits_commit_as_one_history_command_and_leave_selection() {
        let mut canvas = CanvasView::new();
        let object = create_test_text(&mut canvas, "Type something");
        canvas.selection.click_flat(Some(object.id), false);
        canvas.text_edit = Some(TextEditState {
            id: object.id,
            original_text: "Type something".to_owned(),
            editing_text: "Hello world".to_owned(),
            selected_range: 11..11,
            selection_reversed: false,
            marked_range: None,
            pointer_anchor: None,
        });

        assert!(canvas.commit_text_edit());
        assert_eq!(
            canvas.session.runtime.text_content(object.id),
            Some("Hello world")
        );
        assert_eq!(canvas.session.history.undo_len(), 1);
        assert_eq!(canvas.selection.ids(), &[object.id]);
        assert!(matches!(
            match canvas.session.history.peek_undo() {
                Some(SemanticOperation::Runtime(command)) => &command.operation,
                _ => panic!("expected a recorded canvas command"),
            },
            CommandOperation::Text(_)
        ));
    }

    #[test]
    fn escape_discards_text_buffer_without_mutation_or_history() {
        let mut canvas = CanvasView::new();
        let object = create_test_text(&mut canvas, "Original");
        canvas.text_edit = Some(TextEditState {
            id: object.id,
            original_text: "Original".to_owned(),
            editing_text: "Cancelled edit".to_owned(),
            selected_range: 14..14,
            selection_reversed: false,
            marked_range: None,
            pointer_anchor: None,
        });

        assert!(canvas.discard_text_edit());
        assert!(!canvas.is_text_editing());
        assert_eq!(
            canvas.session.runtime.text_content(object.id),
            Some("Original")
        );
        assert!(!canvas.session.history.can_undo());
    }

    #[test]
    fn text_edits_do_not_change_geometry_or_style() {
        let mut canvas = CanvasView::new();
        let object = create_test_text(&mut canvas, "before");
        let geometry = object.geometry();
        let style = canvas.session.runtime.style(object.id).unwrap();
        canvas.selection.click_flat(Some(object.id), false);
        canvas.text_edit = Some(TextEditState {
            id: object.id,
            original_text: "before".to_owned(),
            editing_text: "after".to_owned(),
            selected_range: 5..5,
            selection_reversed: false,
            marked_range: None,
            pointer_anchor: None,
        });
        canvas.commit_text_edit();

        assert_eq!(canvas.session.runtime.geometry(object.id), Some(geometry));
        assert_eq!(canvas.session.runtime.style(object.id), Some(style));
    }

    #[test]
    fn text_hit_testing_places_caret_before_and_after_text() {
        let positions = [
            (0, 0.0),
            (1, 6.0),
            (2, 12.0),
            (3, 18.0),
            (4, 24.0),
            (5, 30.0),
        ];
        assert_eq!(
            nearest_boundary_from_positions("Hello", &positions, -20.0),
            0
        );
        assert_eq!(
            nearest_boundary_from_positions("Hello", &positions, 80.0),
            5
        );
    }

    #[test]
    fn text_hit_testing_uses_nearest_shaped_character_boundary() {
        let text = "Hello world";
        let positions = [
            (0, 0.0),
            (1, 7.0),
            (2, 12.0),
            (3, 18.0),
            (4, 24.0),
            (5, 29.0),
            (6, 33.0),
            (7, 40.0),
            (8, 46.0),
            (9, 52.0),
            (10, 58.0),
            (11, 64.0),
        ];
        assert_eq!(nearest_boundary_from_positions(text, &positions, 9.0), 1);
        assert_eq!(nearest_boundary_from_positions(text, &positions, 32.0), 6);
    }

    #[test]
    fn text_hit_testing_handles_empty_text_and_unicode_boundaries() {
        assert_eq!(nearest_boundary_from_positions("", &[(0, 0.0)], 10.0), 0);

        let text = "Café 👋";
        let positions = text
            .char_indices()
            .map(|(offset, _)| (offset, offset as f32))
            .chain(std::iter::once((text.len(), text.len() as f32)))
            .collect::<Vec<_>>();
        let nearest = nearest_boundary_from_positions(text, &positions, 5.8);
        assert!(text.is_char_boundary(nearest));
        assert_eq!(nearest, 6);
    }

    #[test]
    fn text_pointer_coordinates_respect_camera_zoom() {
        let object_position = point(120.0, 80.0);
        let local_world = point(32.0, 18.0);
        for zoom in [0.5, 1.0, 2.0] {
            let camera = Camera {
                offset: point(15.0, 10.0),
                zoom,
                viewport: size(800.0, 600.0),
                initialized: true,
                pending_fit: None,
            };
            let world = point(
                object_position.x + local_world.x,
                object_position.y + local_world.y,
            );
            let screen = camera.world_to_screen(world);
            assert_eq!(
                screen_to_object_local(camera, screen, object_position),
                point(local_world.x * zoom, local_world.y * zoom)
            );
        }
    }

    #[test]
    fn forward_and_reverse_pointer_drags_create_normalized_selection() {
        assert_eq!(
            selection_from_anchor_and_caret("Hello world", 2, 8),
            (2..8, false)
        );
        assert_eq!(
            selection_from_anchor_and_caret("Hello world", 8, 2),
            (2..8, true)
        );
    }

    #[test]
    fn shift_click_keeps_the_existing_selection_anchor() {
        let range = 3..9;
        let anchor = selection_anchor(&range, false);
        assert_eq!(
            selection_from_anchor_and_caret("Hello world", anchor, 10),
            (3..10, false)
        );

        let reverse_range = 3..9;
        let reverse_anchor = selection_anchor(&reverse_range, true);
        assert_eq!(
            selection_from_anchor_and_caret("Hello world", reverse_anchor, 1),
            (1..9, true)
        );
    }

    #[test]
    fn pointer_selection_does_not_change_document_or_history() {
        let mut canvas = CanvasView::new();
        let object = create_test_text(&mut canvas, "Hello world");
        let geometry = object.geometry();
        let style = canvas.session.runtime.style(object.id).unwrap();
        let selection = selection_from_anchor_and_caret("Hello world", 1, 7);
        canvas.text_edit = Some(TextEditState {
            id: object.id,
            original_text: "Hello world".to_owned(),
            editing_text: "Hello world".to_owned(),
            selected_range: selection.0,
            selection_reversed: selection.1,
            marked_range: None,
            pointer_anchor: Some(1),
        });

        assert_eq!(
            canvas.session.runtime.text_content(object.id),
            Some("Hello world")
        );
        assert_eq!(canvas.session.runtime.geometry(object.id), Some(geometry));
        assert_eq!(canvas.session.runtime.style(object.id), Some(style));
        assert!(!canvas.session.history.can_undo());
    }

    #[test]
    fn text_edit_after_pointer_selection_commits_once_and_keeps_object_selected() {
        let mut canvas = CanvasView::new();
        let object = create_test_text(&mut canvas, "Hello world");
        canvas.selection.click_flat(Some(object.id), false);
        let (selected_range, selection_reversed) =
            selection_from_anchor_and_caret("Hello world", 0, 5);
        canvas.text_edit = Some(TextEditState {
            id: object.id,
            original_text: "Hello world".to_owned(),
            editing_text: "Hello".to_owned(),
            selected_range: 5..5,
            selection_reversed: false,
            marked_range: None,
            pointer_anchor: None,
        });
        let edit = canvas.text_edit.as_mut().unwrap();
        edit.selected_range = selected_range;
        edit.selection_reversed = selection_reversed;

        assert!(canvas.commit_text_edit());
        assert_eq!(
            canvas.session.runtime.text_content(object.id),
            Some("Hello")
        );
        assert_eq!(canvas.selection.ids(), &[object.id]);
        assert_eq!(canvas.session.history.undo_len(), 1);
    }

    #[test]
    fn escape_after_pointer_selection_discards_text_without_history() {
        let mut canvas = CanvasView::new();
        let object = create_test_text(&mut canvas, "Original");
        let (selected_range, selection_reversed) = selection_from_anchor_and_caret("Changed", 1, 4);
        canvas.text_edit = Some(TextEditState {
            id: object.id,
            original_text: "Original".to_owned(),
            editing_text: "Changed".to_owned(),
            selected_range,
            selection_reversed,
            marked_range: None,
            pointer_anchor: None,
        });

        assert!(canvas.discard_text_edit());
        assert_eq!(
            canvas.session.runtime.text_content(object.id),
            Some("Original")
        );
        assert!(!canvas.session.history.can_undo());
    }

    #[test]
    fn utf16_ranges_map_to_valid_utf8_boundaries_for_ime_and_selection() {
        let text = "a🧵é";
        assert_eq!(utf16_range_to_utf8(text, 1..3), 1..5);
        assert_eq!(utf8_range_to_utf16(text, 1..5), 1..3);
        assert_eq!(utf16_range_to_utf8(text, 3..4), 5..7);
    }

    // Phase 14: conservative viewport-aware culling.

    fn culling_camera(zoom: f32, offset: Point<f32>) -> Camera {
        Camera {
            offset,
            zoom,
            viewport: size(400.0, 400.0),
            initialized: true,
            pending_fit: None,
        }
    }

    fn culling_object(id: u64, position: Point<f32>, object_size: Size<f32>) -> DesignObject {
        DesignObject {
            id: ObjectId(id),
            spool_id: node_id(format!("cull-probe-{id}")),
            name: "Cull probe".to_owned(),
            position,
            size: object_size,
            object_type: ObjectType::Rectangle,
            text_content: None,
            text_color: None,
            font_size: None,
            fill: default_style(ObjectType::Rectangle).fill,
            stroke: None,
            border_radius: default_style(ObjectType::Rectangle).border_radius,
            opacity: 1.0,
        }
    }

    #[test]
    fn culling_predicate_covers_inside_partial_edge_and_outside_cases() {
        let camera = culling_camera(1.0, point(0.0, 0.0));

        // Fully inside.
        assert!(camera.affects_viewport(point(100.0, 100.0), size(50.0, 50.0)));
        // Partially intersecting the right edge.
        assert!(camera.affects_viewport(point(370.0, 100.0), size(50.0, 50.0)));
        // Touching the viewport edge exactly: inclusive model.
        assert!(camera.affects_viewport(point(400.0, 100.0), size(50.0, 50.0)));
        assert!(camera.affects_viewport(point(-50.0, 100.0), size(50.0, 50.0)));
        // Fully outside the viewport but within the 64-unit padding.
        assert!(camera.affects_viewport(point(460.0, 100.0), size(50.0, 50.0)));
        assert!(camera.affects_viewport(point(100.0, 460.0), size(50.0, 50.0)));
        // Exactly at the padding boundary stays inclusive.
        assert!(camera.affects_viewport(point(464.0, 100.0), size(50.0, 50.0)));
        // Just beyond the padding.
        assert!(!camera.affects_viewport(point(465.0, 100.0), size(50.0, 50.0)));
        assert!(!camera.affects_viewport(point(100.0, 465.0), size(50.0, 50.0)));
        // Fully outside, far beyond the padding.
        assert!(!camera.affects_viewport(point(600.0, 600.0), size(50.0, 50.0)));
    }

    #[test]
    fn culling_predicate_handles_negative_world_coordinates() {
        let camera = culling_camera(1.0, point(-500.0, -500.0));
        // Maps to screen (10,10)-(60,60): inside.
        assert!(camera.affects_viewport(point(-490.0, -490.0), size(50.0, 50.0)));
        // Maps to screen (-60,-60): outside, but within padding of the edge.
        assert!(camera.affects_viewport(point(-560.0, -560.0), size(50.0, 50.0)));
        // Far outside the padded viewport in negative world space.
        assert!(!camera.affects_viewport(point(-1100.0, -490.0), size(50.0, 50.0)));
        // An object entirely in negative coordinates visible to a camera there.
        let deep = culling_camera(1.0, point(-5_000.0, -5_000.0));
        assert!(deep.affects_viewport(point(-4_990.0, -4_990.0), size(50.0, 50.0)));
        assert!(!deep.affects_viewport(point(-6_000.0, -4_990.0), size(50.0, 50.0)));
    }

    #[test]
    fn culling_predicate_keeps_large_spanning_objects() {
        let camera = culling_camera(1.0, point(0.0, 0.0));
        assert!(camera.affects_viewport(point(-10_000.0, -10_000.0), size(20_000.0, 20_000.0)));
        // Origin far outside the viewport, box still spans it in x.
        assert!(camera.affects_viewport(point(-1_000_000.0, 100.0), size(1_500_000.0, 50.0)));
        let zoomed_out = culling_camera(0.25, point(0.0, 0.0));
        assert!(zoomed_out.affects_viewport(point(-40_000.0, -40_000.0), size(80_000.0, 80_000.0)));
    }

    #[test]
    fn culling_predicate_tracks_zoom_and_pan() {
        let base = culling_camera(1.0, point(0.0, 0.0));
        let zoomed_out = culling_camera(0.5, point(0.0, 0.0));
        let zoomed_in = culling_camera(4.0, point(0.0, 0.0));
        let far = point(600.0, 100.0);
        let near = point(200.0, 100.0);

        // Same world object: visible when zoomed out, culled at 1x and 4x.
        assert!(zoomed_out.affects_viewport(far, size(50.0, 50.0)));
        assert!(!base.affects_viewport(far, size(50.0, 50.0)));
        assert!(!zoomed_in.affects_viewport(far, size(50.0, 50.0)));

        // Same world object: visible at 1x and 0.5x, culled when zoomed to 4x.
        assert!(base.affects_viewport(near, size(50.0, 50.0)));
        assert!(zoomed_out.affects_viewport(near, size(50.0, 50.0)));
        assert!(!zoomed_in.affects_viewport(near, size(50.0, 50.0)));

        // Panning brings a culled object into the padded viewport...
        let panned = culling_camera(1.0, point(300.0, 0.0));
        assert!(panned.affects_viewport(far, size(50.0, 50.0)));
        // ...and panning away culls it again.
        let away = culling_camera(1.0, point(-1_000.0, 0.0));
        assert!(base.affects_viewport(point(100.0, 100.0), size(50.0, 50.0)));
        assert!(!away.affects_viewport(point(100.0, 100.0), size(50.0, 50.0)));
    }

    #[test]
    fn culling_predicate_constructs_when_inputs_cannot_be_evaluated() {
        // Empty viewport before the first prepaint resize: construct, never cull.
        let uninitialized = Camera::default();
        assert_eq!(uninitialized.viewport, size(0.0, 0.0));
        assert!(uninitialized.affects_viewport(point(5_000.0, 5_000.0), size(50.0, 50.0)));

        let camera = culling_camera(1.0, point(0.0, 0.0));
        // Non-positive zoom.
        assert!(Camera {
            zoom: 0.0,
            ..camera
        }
        .affects_viewport(point(5_000.0, 5_000.0), size(50.0, 50.0)));
        // Non-finite camera offset.
        assert!(Camera {
            offset: point(f32::NAN, 0.0),
            ..camera
        }
        .affects_viewport(point(5_000.0, 5_000.0), size(50.0, 50.0)));
        // Non-finite geometry.
        assert!(camera.affects_viewport(point(f32::NAN, 100.0), size(50.0, 50.0)));
        assert!(camera.affects_viewport(point(5_000.0, 5_000.0), size(f32::INFINITY, 50.0),));
        // Negative size.
        assert!(camera.affects_viewport(point(5_000.0, 5_000.0), size(-50.0, -50.0),));
    }

    #[test]
    fn culling_includes_every_object_the_diagnostic_model_sees() {
        for zoom in [0.25, 0.5, 1.0, 2.0, 4.0] {
            for offset in [point(0.0, 0.0), point(-1_000.0, 500.0)] {
                let camera = culling_camera(zoom, offset);
                for position in [-300.0_f32, -150.0, 0.0, 150.0, 350.0, 399.0, 450.0, 700.0] {
                    for object_size in [size(10.0, 10.0), size(80.0, 80.0), size(1_000.0, 60.0)] {
                        if diagnostics::intersects(
                            point(position, position),
                            object_size,
                            offset,
                            camera.viewport,
                            zoom,
                        ) {
                            assert!(
                                camera.affects_viewport(point(position, position), object_size),
                                "culling must retain every object the Phase 13 diagnostic \
                                 visibility model sees: position {position} size {object_size:?} \
                                 zoom {zoom} offset {offset:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn culling_exempts_only_the_active_text_edit_object() {
        let camera = culling_camera(1.0, point(0.0, 0.0));
        let inside = culling_object(5, point(100.0, 100.0), size(50.0, 50.0));
        let outside = culling_object(6, point(5_000.0, 5_000.0), size(50.0, 50.0));

        // Geometry gate without editing.
        assert!(should_construct(camera, &inside, None));
        assert!(!should_construct(camera, &outside, None));
        // The actively edited object is constructed even far outside the viewport.
        assert!(should_construct(camera, &outside, Some(outside.id)));
        // A different object being edited does not exempt the offscreen one.
        assert!(!should_construct(camera, &outside, Some(inside.id)));
        // An inside object is constructed whether or not it is being edited.
        assert!(should_construct(camera, &inside, Some(inside.id)));

        // Selection must not gate base-element construction: selection outlines
        // and resize handles are constructed separately in `artboards`, so an
        // offscreen selected object stays culled without losing its chrome.
        let mut selection = Selection::default();
        selection.click_flat(Some(outside.id), false);
        assert_eq!(selection.ids(), &[outside.id]);
        assert!(!should_construct(camera, &outside, None));
    }

    // ---------------------------------------------------------------------
    // Editing a source-backed project
    //
    // These cover the product loop the milestone is about: a loaded project is
    // an ordinary editable document, and what the editor changes is written
    // back to the authored source it came from.
    // ---------------------------------------------------------------------

    /// Copy a fixture into a temp directory so a save cannot touch the repo.
    fn project_scratch(fixture: &str) -> std::path::PathBuf {
        let from = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture);
        let to = std::env::temp_dir().join(format!(
            "spool-canvas-{fixture}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&to);
        copy_tree(&from, &to).expect("copy fixture");
        to
    }

    fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let target = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_tree(&entry.path(), &target)?;
            } else {
                std::fs::copy(entry.path(), &target)?;
            }
        }
        Ok(())
    }

    fn project_view(root: &std::path::Path) -> CanvasView {
        let loaded = crate::project_open::open_project(root).expect("project opens");
        let mut view = CanvasView::new();
        view.load_project(loaded);
        view
    }

    fn object_with_node(view: &CanvasView, node: &str) -> DesignObject {
        view.document_objects()
            .iter()
            .find(|object| object.spool_id.as_str() == node)
            .unwrap_or_else(|| panic!("{node} is projected"))
            .clone()
    }

    #[test]
    fn a_created_object_never_reuses_a_loaded_identity() {
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let loaded_ids: Vec<String> = view
            .document_objects()
            .iter()
            .map(|object| object.spool_id.as_str().to_owned())
            .collect();
        assert_eq!(
            loaded_ids.len(),
            3,
            "the fixture has three projected objects"
        );

        // The counter behind `allocate_node_id` restarts at 1 when a project is
        // loaded, so a freshly created object's candidate identity collides with
        // an authored one unless allocation checks what is live.
        for index in 0..5 {
            let created = view.session.runtime.create_object(
                ObjectType::Rectangle,
                point(index as f32 * 10.0, 0.0),
                size(10.0, 10.0),
                None,
            );
            assert!(
                !loaded_ids.contains(&created.spool_id.as_str().to_owned()),
                "a new object must not take the identity of {:?}",
                created.spool_id.as_str()
            );
        }

        // And identities stay unique across the whole document, not just against
        // the loaded set.
        let all: Vec<&str> = view
            .document_objects()
            .iter()
            .map(|object| object.spool_id.as_str())
            .collect();
        let mut unique = all.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), all.len(), "every live identity is unique");
    }

    #[test]
    fn a_new_identity_is_never_one_that_is_already_live() {
        // The counter that feeds `allocate_node_id` is set from scratch when a
        // projected document is loaded, so its next value can name an identity
        // that is already in the scene. Allocation has to notice and move on;
        // without that check the editor would end up with two objects claiming
        // one identity, and every later lookup by identity would be ambiguous.
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let first = view.session.runtime.create_object(
            ObjectType::Rectangle,
            point(0.0, 0.0),
            size(10.0, 10.0),
            None,
        );

        // Rewind the counter to the value it would hand out next time.
        view.session.runtime.next_node_id = 1;
        let second = view.session.runtime.create_object(
            ObjectType::Rectangle,
            point(10.0, 0.0),
            size(10.0, 10.0),
            None,
        );
        assert_ne!(
            first.spool_id, second.spool_id,
            "a live identity was handed out twice"
        );
        assert_eq!(
            first.spool_id,
            node_id(format!("spool-node-{:016x}", 1)),
            "the first object took the identity the counter offered"
        );
    }

    #[test]
    fn deleting_an_object_does_not_free_its_identity_for_reuse() {
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = object_with_node(&view, "spool-cta-primary");

        let removed = view.session.runtime.remove_objects(&[cta.id]);
        view.commit(DocumentCommand::delete(removed));
        assert!(
            view.document_objects()
                .iter()
                .all(|object| object.spool_id != cta.spool_id),
            "the deleted object is gone from the runtime"
        );
        assert!(
            view.session
                .document
                .structure
                .nodes
                .iter()
                .any(|node| node.id == cta.spool_id),
            "and its identity is still live in the persistent document"
        );

        // A new object minted after the deletion must still not land on it: the
        // identity belongs to the document, not to the runtime object that used
        // to hold it.
        let created = view.session.runtime.create_object(
            ObjectType::Rectangle,
            point(0.0, 0.0),
            size(10.0, 10.0),
            None,
        );
        assert_ne!(created.spool_id, cta.spool_id);
    }

    #[test]
    fn duplicating_a_projected_object_gives_it_new_identities() {
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = object_with_node(&view, "spool-cta-primary");
        let originals: Vec<String> = view
            .document_objects()
            .iter()
            .map(|object| object.spool_id.as_str().to_owned())
            .collect();

        let duplicates = view.session.runtime.duplicate_objects(&[cta.id]);
        view.commit(DocumentCommand::insert(duplicates.clone()));
        assert_eq!(duplicates.len(), 1);

        let copy = &duplicates[0].object;
        assert_ne!(copy.id, cta.id, "a new runtime key");
        assert_ne!(copy.spool_id, cta.spool_id, "a new persistent identity");
        assert!(
            !originals.contains(&copy.spool_id.as_str().to_owned()),
            "the duplicate does not take an existing identity"
        );
        assert_eq!(
            copy.text_content, cta.text_content,
            "and it carries the same authored text"
        );

        // The original is untouched and both remain in the scene.
        let after: Vec<String> = view
            .document_objects()
            .iter()
            .map(|object| object.spool_id.as_str().to_owned())
            .collect();
        assert!(after.contains(&cta.spool_id.as_str().to_owned()));
        assert!(after.contains(&copy.spool_id.as_str().to_owned()));
    }

    #[test]
    fn a_moved_object_survives_undo_redo_and_then_the_save_loop() {
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = object_with_node(&view, "spool-cta-primary");
        let resting = cta.position;

        // A real gesture: select, drag, release. The move goes through the
        // canvas commit path, so it is one history entry.
        view.selection.replace(vec![cta.id]);
        let depth_before = view.session.history.undo_len();
        begin_live_move(&mut view, &[cta.id]);
        drag_to(&mut view, point(120.0, 40.0));
        assert_eq!(
            view.session.history.undo_len() - depth_before,
            1,
            "one drag is one history entry"
        );
        let committed = object_with_node(&view, "spool-cta-primary").position;
        assert_ne!(committed, resting, "the object actually moved");

        assert!(view.undo_history());
        assert_eq!(
            object_with_node(&view, "spool-cta-primary").position,
            resting,
            "undo restored the authored position"
        );
        assert!(view.redo_history());
        assert_eq!(
            object_with_node(&view, "spool-cta-primary").position,
            committed,
            "redo re-applied the move"
        );

        // Save, then reopen from disk through the ordinary loader.
        let outcome = view.save_project().expect("save succeeds");
        assert!(
            outcome.unsupported.is_empty(),
            "nothing about this edit was dropped: {:?}",
            outcome.unsupported
        );
        let reopened = project_view(&root);
        let after = object_with_node(&reopened, "spool-cta-primary");
        assert_eq!(
            after.position, committed,
            "the saved position came back from disk"
        );
        assert_eq!(
            after.size, cta.size,
            "an untouched size is not rewritten as a new one"
        );
        assert_eq!(
            reopened.persistent_document().structure.nodes.len(),
            3,
            "identity and hierarchy survive the loop"
        );
        assert_eq!(
            after.text_content, cta.text_content,
            "authored text survives the loop"
        );
    }

    #[test]
    fn moving_a_frame_and_its_children_survives_the_round_trip() {
        // The regression this covers: a child's absolute position is measured
        // from the element that contains it, so saving a moved frame and its
        // children has to write the children relative to where the frame ended
        // up. Writing world coordinates as `left` put every child twice as far
        // from the origin as it should be once the parent itself moved.
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let ids: Vec<ObjectId> = view.document_objects().iter().map(|o| o.id).collect();
        let resting: Vec<(f32, f32)> = view
            .document_objects()
            .iter()
            .map(|o| (o.position.x, o.position.y))
            .collect();

        view.selection.replace(ids.clone());
        begin_live_move(&mut view, &ids);
        drag_to(&mut view, point(120.0, 90.0));
        let committed: Vec<(f32, f32)> = view
            .document_objects()
            .iter()
            .map(|o| (o.position.x, o.position.y))
            .collect();
        assert_ne!(committed, resting);

        let outcome = view.save_project().expect("save succeeds");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        let reopened = project_view(&root);
        let after: Vec<(f32, f32)> = reopened
            .document_objects()
            .iter()
            .map(|o| (o.position.x, o.position.y))
            .collect();
        for ((want_x, want_y), (got_x, got_y)) in committed.iter().zip(&after) {
            assert!(
                (want_x - got_x).abs() < 0.01 && (want_y - got_y).abs() < 0.01,
                "every object came back where it was left: want ({want_x}, {want_y}), got ({got_x}, {got_y})"
            );
        }
    }

    #[test]
    fn an_inspector_field_change_is_one_operation_and_undoes_exactly() {
        // The Inspector is a second way to ask the same question a canvas drag
        // asks, not a second store: same command, same history, same undo.
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = object_with_node(&view, "spool-cta-primary");
        let before = view.object_geometry(cta.id).expect("geometry");

        assert!(view.set_object_geometry(
            cta.id,
            Geometry {
                position: point(before.position.x + 24.0, before.position.y),
                size: size(before.size.width, before.size.height + 8.0),
            },
        ));
        assert_eq!(view.session.history.undo_len(), 1, "one edit, one entry");

        assert!(view.undo_history());
        assert_eq!(
            view.object_geometry(cta.id),
            Some(before),
            "undo restores the exact geometry the field replaced"
        );
        assert!(view.redo_history());
        assert_eq!(
            view.object_geometry(cta.id).unwrap().position.x,
            before.position.x + 24.0
        );

        // An edit that changes nothing is not an edit, and in particular does
        // not destroy the redo that undo just made available.
        assert!(view.undo_history());
        assert_eq!(view.session.history.redo_len(), 1);
        let undone = view.object_geometry(cta.id).unwrap();
        assert!(!view.set_object_geometry(cta.id, undone));
        assert_eq!(view.session.history.redo_len(), 1);
        assert!(view.redo_history(), "the redo survived the no-op");
    }

    #[test]
    fn an_inspector_field_drag_records_one_operation_not_one_per_movement() {
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = object_with_node(&view, "spool-cta-primary");
        let before = view.object_geometry(cta.id).expect("geometry");

        // What the shell does across a drag: open, move, move again, commit.
        let scrub = view.begin_geometry_scrub(cta.id).expect("scrub opens");
        assert_eq!(
            view.session.history.undo_len(),
            0,
            "an open scrub has recorded nothing"
        );
        for x in [4.0, 9.0, 15.0] {
            view.scrub_geometry(
                &scrub,
                Geometry {
                    position: point(before.position.x + x, before.position.y),
                    size: before.size,
                },
            );
        }
        assert_eq!(
            view.session.history.undo_len(),
            0,
            "still nothing: the drag is not over"
        );
        assert!(view.commit_geometry_scrub(scrub));
        assert_eq!(view.session.history.undo_len(), 1);

        assert!(view.undo_history());
        assert_eq!(view.object_geometry(cta.id), Some(before));
    }

    #[test]
    fn an_inspector_drag_that_ends_where_it_started_records_nothing() {
        // The same rule a canvas drag already follows: a gesture that returns to
        // its origin is not a change, so it must not clear redo either.
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = object_with_node(&view, "spool-cta-primary");
        let before = view.object_geometry(cta.id).expect("geometry");

        let scrub = view.begin_geometry_scrub(cta.id).expect("scrub opens");
        for x in [30.0, 12.0, 0.0] {
            view.scrub_geometry(
                &scrub,
                Geometry {
                    position: point(before.position.x + x, before.position.y),
                    size: before.size,
                },
            );
        }
        assert!(!view.commit_geometry_scrub(scrub));
        assert_eq!(view.object_geometry(cta.id), Some(before));
        assert_eq!(view.session.history.undo_len(), 0);
    }

    #[test]
    fn a_cancelled_inspector_drag_restores_the_geometry_and_records_nothing() {
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = object_with_node(&view, "spool-cta-primary");
        let before = view.object_geometry(cta.id).expect("geometry");

        let scrub = view.begin_geometry_scrub(cta.id).expect("scrub opens");
        view.scrub_geometry(
            &scrub,
            Geometry {
                position: point(before.position.x - 40.0, before.position.y - 12.0),
                size: before.size,
            },
        );
        view.cancel_geometry_scrub(scrub);

        assert_eq!(
            view.object_geometry(cta.id),
            Some(before),
            "a cancelled drag is not a move"
        );
        assert_eq!(view.session.history.undo_len(), 0);
    }

    #[test]
    fn the_canvas_and_the_inspector_report_the_same_geometry() {
        // Direct manipulation and the Inspector are two views of one fact: after
        // a canvas drag, the value the Inspector reads is the dragged value.
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = object_with_node(&view, "spool-cta-primary");
        let before = view.object_geometry(cta.id).expect("geometry");

        view.selection.replace(vec![cta.id]);
        begin_live_move(&mut view, &[cta.id]);
        drag_to(&mut view, point(50.0, 30.0));

        let dragged = view.object_geometry(cta.id).expect("geometry");
        let object = view
            .document_objects()
            .iter()
            .find(|object| object.id == cta.id)
            .expect("still there");
        assert_eq!(
            (object.position.x, object.position.y),
            (dragged.position.x, dragged.position.y),
            "the object the Inspector draws its fields from holds the dragged value"
        );
        assert_ne!(
            dragged.position, before.position,
            "and the drag really moved something"
        );
    }

    #[test]
    fn renaming_an_object_is_one_operation_that_survives_save() {
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = object_with_node(&view, "spool-cta-primary");

        assert!(
            view.rename_object(cta.id, "Primary action".to_owned()),
            "a source-backed object renames"
        );
        assert_eq!(
            view.document_objects()
                .iter()
                .find(|object| object.id == cta.id)
                .map(|object| object.name.clone()),
            Some("Primary action".to_owned()),
            "the runtime mirrors the document, which is the authority"
        );
        assert_eq!(view.session.history.undo_len(), 1);

        let outcome = view.save_project().expect("save succeeds");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);
        let yaml = std::fs::read_to_string(root.join("lamine.yaml")).expect("read metadata");
        assert!(
            yaml.contains("Primary action"),
            "the name reaches the metadata file: {yaml}"
        );

        let reopened = project_view(&root);
        assert_eq!(
            object_with_node(&reopened, "spool-cta-primary").name,
            "Primary action"
        );

        assert!(view.undo_history());
        assert_eq!(
            view.document_objects()
                .iter()
                .find(|object| object.id == cta.id)
                .map(|object| object.name.clone()),
            Some("Primary CTA".to_owned()),
            "undo restores the authored name"
        );
    }

    #[test]
    fn an_object_with_no_node_cannot_be_renamed() {
        // Reported, not invented. An object placed on the canvas without
        // `commit_created` has no metadata entry to rename, and writing one would
        // be a structural edit the user did not ask for.
        //
        // Not the created-object case: an object created through the creation
        // gesture *does* have a node now, and is renamed like any other.
        let mut view = CanvasView::new();
        let created = view.session.runtime.create_object(
            ObjectType::Rectangle,
            point(10.0, 10.0),
            size(20.0, 20.0),
            None,
        );
        assert!(!view.rename_object(created.id, "New".to_owned()));
        assert_eq!(view.session.history.undo_len(), 0);
    }

    #[test]
    fn one_session_edits_text_style_geometry_and_a_name_and_all_of_it_survives() {
        // The milestone in one test: one project, one history stack, four kinds
        // of semantic operation, one save, one reopen. Every dimension is
        // checked against what came back off disk rather than against what the
        // editor was showing, because "the canvas looked right" is not the claim
        // being made.
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let headline = object_with_node(&view, "spool-text-headline");
        let cta = object_with_node(&view, "spool-cta-primary");
        let html_before = std::fs::read_to_string(root.join("index.html")).expect("read html");
        let css_before = std::fs::read_to_string(root.join("styles.css")).expect("read css");
        let start_depth = view.session.history.undo_len();

        // 1. Text.
        let edited = "Design in source, structure in Spool — saved";
        assert!(view.set_object_text(headline.id, edited.to_owned()));
        assert_eq!(
            view.session.history.undo_len() - start_depth,
            1,
            "a text edit is one operation"
        );

        // 2. Appearance, through the same edits the Inspector sends. The fill is
        // owned by `.cta`, so it lands in the stylesheet; the opacity is owned
        // by nobody, so it becomes a local declaration.
        view.selection.replace(vec![cta.id]);
        let depth_before_style = view.session.history.undo_len();
        for edit in [
            StyleEdit::Fill(Some(Color::from_rgb(0xc4_5d_5d))),
            StyleEdit::TextColor(Some(Color::from_rgb(0x16_16_1d))),
            StyleEdit::FontSize(24.0),
            StyleEdit::BorderRadius(16.0),
            StyleEdit::Opacity(0.5),
        ] {
            assert!(
                view.apply_selected_style(edit),
                "{edit:?} changed something"
            );
        }
        assert_eq!(
            view.session.history.undo_len() - depth_before_style,
            5,
            "five style edits are five operations, not one batch and not five per property"
        );

        // 3. Geometry, through the Inspector's own operation.
        let moved_to = match view.object_geometry(cta.id) {
            Some(geometry) => Geometry {
                position: point(geometry.position.x + 40.0, geometry.position.y + 24.0),
                size: size(geometry.size.width + 20.0, geometry.size.height + 8.0),
            },
            None => panic!("the CTA is projected"),
        };
        assert!(view.set_object_geometry(cta.id, moved_to));

        // 4. A name, which is persistent metadata rather than runtime state.
        assert!(view.rename_object(headline.id, "Page headline".to_owned()));
        let history_count = view.session.history.undo_len() - start_depth;
        assert_eq!(
            history_count, 8,
            "one entry per user action, across four kinds of operation"
        );

        // Undo the last two actions: the resize-then-move geometry edit and the
        // rename. The text and the style must be untouched, which is what proves
        // the stack is one stack and not four.
        assert!(view.undo_history(), "undo the rename");
        assert_eq!(
            view.document_objects()
                .iter()
                .find(|object| object.id == headline.id)
                .map(|object| object.name.clone()),
            Some("Headline".to_owned())
        );
        assert_eq!(
            view.object_geometry(cta.id),
            Some(moved_to),
            "geometry is a separate step"
        );
        assert!(view.undo_history(), "undo the geometry edit");
        assert!(view.redo_history(), "redo it");
        assert_eq!(view.object_geometry(cta.id), Some(moved_to));
        assert!(view.redo_history(), "redo the rename as well");
        assert_eq!(
            view.document_objects()
                .iter()
                .find(|object| object.id == headline.id)
                .map(|object| object.name.clone()),
            Some("Page headline".to_owned()),
            "two operations of different kinds share one stack in order"
        );

        let outcome = view.save_project().expect("save succeeds");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        // The authored files changed in exactly the places the edits name.
        let html_after = std::fs::read_to_string(root.join("index.html")).expect("read html");
        let css_after = std::fs::read_to_string(root.join("styles.css")).expect("read css");
        assert!(html_after.contains(edited), "the headline text was written");
        assert!(
            html_after.contains("opacity: 0.5"),
            "the unowned property became local"
        );
        assert!(
            html_after.contains(&format!(
                "width: {}px",
                crate::project_save::css_length(moved_to.size.width)
            )),
            "the measured width was written: {html_after}"
        );
        assert!(
            css_after.contains("#c45d5d"),
            "the fill was rewritten where `.cta` owns it"
        );
        assert_ne!(html_before, html_after);
        assert_ne!(css_before, css_after);
        // The stylesheet gained a value and lost nothing: every rule the author
        // wrote is still there, and no rule was added.
        assert_eq!(
            css_after.matches(".cta").count(),
            css_before.matches(".cta").count(),
            "no rule was added or removed"
        );
        assert!(
            html_after.contains("href=\"#start\""),
            "unrelated attributes survived"
        );

        // Reopen from disk and check every dimension against the disk.
        let reopened = project_view(&root);
        let headline_after = object_with_node(&reopened, "spool-text-headline");
        let cta_after = object_with_node(&reopened, "spool-cta-primary");
        assert_eq!(headline_after.text_content.as_deref(), Some(edited));
        assert_eq!(headline_after.name, "Page headline");
        assert_eq!(
            cta_after.fill.map(|fill| fill.color.to_rgb()),
            Some(0xc4_5d_5d)
        );
        assert_eq!(cta_after.font_size, Some(24.0));
        assert_eq!(cta_after.border_radius, 16.0);
        assert_eq!(cta_after.opacity, 0.5);
        assert_eq!(cta_after.text_color, Some(Color::from_rgb(0x16_16_1d)));
        assert!(
            (cta_after.size.width - moved_to.size.width).abs() < 0.01
                && (cta_after.size.height - moved_to.size.height).abs() < 0.01,
            "the size survived: {:?}",
            cta_after.size
        );
        assert!(
            (cta_after.position.x - moved_to.position.x).abs() < 0.01
                && (cta_after.position.y - moved_to.position.y).abs() < 0.01,
            "and so did the move: {:?}",
            cta_after.position
        );
    }

    #[test]
    fn a_second_save_measures_from_the_first_save_not_from_the_project_opening() {
        // The regression this covers: save expresses a move as a *change* from a
        // baseline, and the write layer adds that change to whatever offset the
        // bytes already carry. So the baseline has to be what the source
        // currently says, not what it said when the project was opened.
        //
        // With the opening state as the baseline, moving an object twice and
        // saving twice wrote an offset of 40, then wrote 70 again on top of the
        // 40 already on disk — so the file described 110 while the editor showed
        // 70. The editor and its own source drifted apart by more with every
        // save, and a reopen put the object somewhere the user had never put it.
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = id_of(&view, "spool-cta-primary");
        let authored = view.session.runtime.geometry(cta).unwrap().position;

        // 40 down, saved.
        view.selection.replace(vec![cta]);
        begin_live_move_with_snap(&mut view, &[cta], true);
        drag_to(&mut view, point(0.0, 40.0));
        view.save_project().expect("first save succeeds");

        // 30 more, saved. The second save's change is 30, not 70: it is measured
        // from what the first save left on disk.
        begin_live_move_with_snap(&mut view, &[cta], true);
        drag_to(&mut view, point(0.0, 30.0));
        let live = view.session.runtime.geometry(cta).unwrap().position;
        assert!(
            (live.y - authored.y - 70.0).abs() < 0.01,
            "two drags of 40 and 30 leave the editor at 70, not 30"
        );
        view.save_project().expect("second save succeeds");

        let reopened = project_view(&root);
        let back = reopened.session.runtime.geometry(cta).unwrap().position;
        assert!(
            (back.y - live.y).abs() < 0.01,
            "the reopened offset is {} but the editor had {}: the second save \
             measured from the project opening instead of from the first save",
            back.y - authored.y,
            live.y - authored.y
        );
    }

    #[test]
    fn an_undo_after_save_is_carried_by_the_next_save() {
        // Save writes the disk and undo changes only the editor, so immediately
        // after an undo the two genuinely disagree. The next save is what
        // resolves that, and it is the only thing that can: the file's bytes are
        // the editor's own past output.
        //
        // This used to assert the opposite — that the second save wrote nothing —
        // on the reasoning that an undo had put the object back where the project
        // was opened, so there was nothing to write. That was agreement about the
        // wrong thing. The bytes still said `translate(0px, 40px)`, so a reopen
        // resurrected the exact move the user had just undone, and the undo held
        // only until something else was edited.
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = id_of(&view, "spool-cta-primary");
        let authored = view.session.runtime.geometry(cta).unwrap().position;

        view.selection.replace(vec![cta]);
        begin_live_move_with_snap(&mut view, &[cta], true);
        drag_to(&mut view, point(0.0, 40.0));
        view.save_project().expect("save succeeds");

        assert!(view.undo_history());
        assert!(
            (view.session.runtime.geometry(cta).unwrap().position.y - authored.y).abs() < 0.01,
            "undo put the object back where it was authored"
        );

        view.save_project().expect("second save succeeds");

        let reopened = project_view(&root);
        let back = reopened.session.runtime.geometry(cta).unwrap().position;
        assert!(
            (back.y - authored.y).abs() < 0.01,
            "the reopened offset is {} but the editor was back at the authored 0: \
             an undo that the next save drops is not an undo",
            back.y - authored.y
        );
    }

    #[test]
    fn saving_an_untouched_project_writes_nothing() {
        let root = project_scratch("landing");
        let before: Vec<(std::path::PathBuf, Vec<u8>)> = std::fs::read_dir(&root)
            .expect("read project")
            .map(|entry| entry.expect("entry"))
            .map(|entry| {
                let bytes = std::fs::read(entry.path()).expect("read file");
                (entry.path(), bytes)
            })
            .collect();

        let mut view = project_view(&root);
        let outcome = view.save_project().expect("save succeeds");

        assert!(
            outcome.written.is_empty(),
            "no file should be rewritten for a session that changed nothing: {:?}",
            outcome.written
        );
        for (path, bytes) in before {
            assert_eq!(
                std::fs::read(&path).expect("read file"),
                bytes,
                "{} is byte-for-byte unchanged",
                path.display()
            );
        }
    }

    // -- Created-object persistence: the milestone this closes.
    //
    // Every test here drives the *production* path — `commit_created`, then
    // `save_project`, then a fresh `open_project` from disk — because the gap
    // this milestone closed was never in a type. It was that a created object
    // reached the canvas and not the document, and no amount of unit testing the
    // two halves separately would have shown it.

    /// Create one object exactly as the creation gesture does.
    ///
    /// The same three steps `commit_creation` performs, on the same private
    /// methods, because the point of these tests is the production path rather
    /// than a stand-in for it.
    fn create(view: &mut CanvasView, object_type: ObjectType) -> DesignObject {
        let text = (object_type == ObjectType::Text).then(|| "Type something".to_string());
        let object = view.session.runtime.create_object(
            object_type,
            point(240.0, 180.0),
            size(120.0, 80.0),
            text,
        );
        let placement = view
            .session
            .runtime
            .placement(object.id)
            .expect("a new object is placed");
        view.selection
            .click(Some(object.id), false, &view.hierarchy());
        view.commit_created(vec![placement]);
        object
    }

    /// Replace a text object's content, as the text editor commits it.
    fn set_text(view: &mut CanvasView, id: ObjectId, text: &str) {
        let geometry = view.session.runtime.geometry(id).expect("has geometry");
        let position = geometry.position;
        let size = geometry.size;
        view.commit(DocumentCommand::text(vec![TextChange {
            id,
            before: view
                .session
                .runtime
                .object(id)
                .and_then(|object| object.text_content.clone())
                .unwrap_or_default(),
            after: text.to_owned(),
        }]));
        let _ = (position, size);
    }

    /// Reopen the project from disk, as a fresh session.
    fn reopen(root: &std::path::Path) -> crate::project_open::LoadedProject {
        crate::project_open::open_project(root).expect("the project reopens")
    }

    fn node_of<'a>(loaded: &'a crate::project_open::LoadedProject, id: &str) -> &'a StructuralNode {
        loaded
            .document
            .structure
            .nodes
            .iter()
            .find(|node| node.id.as_str() == id)
            .unwrap_or_else(|| panic!("{id} is in the document"))
    }

    fn ids(loaded: &crate::project_open::LoadedProject) -> Vec<String> {
        loaded
            .document
            .structure
            .nodes
            .iter()
            .map(|node| node.id.as_str().to_owned())
            .collect()
    }

    #[test]
    fn a_created_rectangle_survives_save_and_reopen() {
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let created = create(&mut view, ObjectType::Rectangle);
        let node_id = created.spool_id.clone();

        let outcome = view.save_project().expect("save succeeds");
        assert!(
            outcome.unsupported.is_empty(),
            "a created object is supportable now: {:?}",
            outcome.unsupported
        );

        // The metadata knows about it...
        let reloaded = reopen(&root);
        let node = node_of(&reloaded, node_id.as_str());
        assert_eq!(node.kind, "rectangle");
        assert_eq!(node.name, created.name);
        assert_eq!(node.source.file, "pages/index.html");
        // ...the authored source has an element carrying its identity...
        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        assert!(
            html.contains(&format!("data-spool-id=\"{}\"", node_id.as_str())),
            "the element was authored: {html}"
        );
        // ...and the reopened project draws it as the same kind of object.
        assert_eq!(reloaded.runtime.objects().len(), 4);
        let round_tripped = reloaded
            .runtime
            .objects()
            .iter()
            .find(|object| object.spool_id == node_id)
            .expect("the created object is projected");
        assert_eq!(round_tripped.object_type, ObjectType::Rectangle);
        assert_eq!(round_tripped.name, created.name);
    }

    #[test]
    fn a_created_ellipse_survives_save_and_reopen_as_an_ellipse() {
        // The kind has to be one the projection reads back. A created ellipse
        // authored as anything else would reopen as an object that exists in the
        // document and draws as nothing.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let created = create(&mut view, ObjectType::Ellipse);
        view.save_project().expect("save succeeds");

        let reloaded = reopen(&root);
        assert_eq!(
            node_of(&reloaded, created.spool_id.as_str()).kind,
            "ellipse"
        );
        let round_tripped = reloaded
            .runtime
            .objects()
            .iter()
            .find(|object| object.spool_id == created.spool_id)
            .expect("projected");
        assert_eq!(round_tripped.object_type, ObjectType::Ellipse);
    }

    #[test]
    fn a_created_text_object_keeps_its_text_across_a_reopen() {
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let created = create(&mut view, ObjectType::Text);
        let text = created
            .text_content
            .clone()
            .expect("text objects start with text");
        view.save_project().expect("save succeeds");

        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        assert!(html.contains(&text), "the text is in the source: {html}");

        let reloaded = reopen(&root);
        let round_tripped = reloaded
            .runtime
            .objects()
            .iter()
            .find(|object| object.spool_id == created.spool_id)
            .expect("projected");
        assert_eq!(round_tripped.object_type, ObjectType::Text);
        assert_eq!(round_tripped.text_content.as_deref(), Some(text.as_str()));
    }

    #[test]
    fn editing_a_created_text_object_then_saving_persists_the_edit() {
        // Text the user typed after creation has to survive too, which means the
        // created element must own a single run of text for the next edit to
        // find a range in.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let created = create(&mut view, ObjectType::Text);

        let edited = "Spool writes this back";
        set_text(&mut view, created.id, edited);
        view.save_project().expect("save succeeds");

        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        assert!(html.contains(edited), "the edit is in the source: {html}");
        let reloaded = reopen(&root);
        assert_eq!(
            reloaded
                .runtime
                .objects()
                .iter()
                .find(|object| object.spool_id == created.spool_id)
                .and_then(|object| object.text_content.clone())
                .as_deref(),
            Some(edited)
        );
    }

    #[test]
    fn a_created_object_is_a_child_of_the_selected_frame() {
        // Hierarchy is the question the canvas already answers for a click, so a
        // creation asks it the same way rather than inventing a parent.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let frame = object_with_node(&view, "spool-frame-root");
        view.selection.replace(vec![frame.id]);

        let created = create(&mut view, ObjectType::Rectangle);
        view.save_project().expect("save succeeds");

        let reloaded = reopen(&root);
        let node = node_of(&reloaded, created.spool_id.as_str());
        assert_eq!(
            node.parent.as_ref().map(|id| id.as_str()),
            Some("spool-frame-root"),
            "created inside the selected frame"
        );
        let frame_node = node_of(&reloaded, "spool-frame-root");
        assert!(
            frame_node
                .children
                .iter()
                .any(|child| child.as_str() == created.spool_id.as_str()),
            "and the frame lists it as a child"
        );
        // The element really is inside the frame's element, not merely claimed
        // by it: the structure says one thing and the source has to agree.
        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        let frame_at = html
            .find("data-spool-id=\"spool-frame-root\"")
            .expect("frame");
        let created_at = html
            .find(&format!("data-spool-id=\"{}\"", created.spool_id.as_str()))
            .expect("created element");
        assert!(frame_at < created_at, "authored inside the frame: {html}");
    }

    #[test]
    fn two_created_objects_keep_creation_order_in_the_authored_source() {
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let first = create(&mut view, ObjectType::Rectangle);
        let second = create(&mut view, ObjectType::Ellipse);
        view.save_project().expect("save succeeds");

        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        let first_at = html.find(first.spool_id.as_str()).expect("first element");
        let second_at = html.find(second.spool_id.as_str()).expect("second element");
        assert!(
            first_at < second_at,
            "creation order is source order: {html}"
        );

        let reloaded = reopen(&root);
        let order: Vec<String> = ids(&reloaded);
        assert_eq!(order.len(), 5);
    }

    #[test]
    fn creating_twice_saves_twice_and_does_not_rewrite_the_first_object() {
        // The second save is measured from the first. If the created object were
        // authored again on the second save, its element would be duplicated.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let created = create(&mut view, ObjectType::Rectangle);
        view.save_project().expect("first save");
        let after_first = std::fs::read_to_string(root.join("pages/index.html")).expect("html");

        view.save_project().expect("second save");
        let after_second = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        assert_eq!(
            after_first, after_second,
            "a second save with nothing changed writes nothing"
        );
        assert_eq!(
            after_second
                .matches(&format!("data-spool-id=\"{}\"", created.spool_id.as_str()))
                .count(),
            1,
            "one element, authored once"
        );
    }

    #[test]
    fn create_save_edit_save_keeps_both_the_creation_and_the_edit() {
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let created = create(&mut view, ObjectType::Rectangle);
        view.save_project().expect("first save");

        // Reopen, so the second save is measured from what is on disk.
        let mut view = project_view(&root);
        let reopened = object_with_node(&view, created.spool_id.as_str());
        view.selection.replace(vec![reopened.id]);
        view.nudge_selection(60.0, 40.0);
        view.save_project().expect("second save");

        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        // The created element was authored out of the flow, so moving it writes
        // coordinates from its containing block — which is what the existing
        // geometry policy does for an absolute element. The frame sits at the
        // origin, so these are the world coordinates the editor left it at.
        assert!(
            html.contains("left: 300px") && html.contains("top: 220px"),
            "the move survived alongside the creation: {html}"
        );
        assert_eq!(
            html.matches(created.spool_id.as_str()).count(),
            1,
            "and the element was not authored twice"
        );
        assert_eq!(reopen(&root).runtime.objects().len(), 4);
    }

    #[test]
    fn a_created_object_keeps_its_identity_through_save_and_reopen() {
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let created = create(&mut view, ObjectType::Rectangle);
        view.save_project().expect("save succeeds");

        let reloaded = reopen(&root);
        assert_eq!(
            node_of(&reloaded, created.spool_id.as_str()).id,
            created.spool_id,
            "the same logical node, read back from lamine.yaml and the source"
        );
        // And the identity is what the authored element carries, so the two
        // halves cannot drift apart.
        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        assert!(html.contains(created.spool_id.as_str()));
    }

    #[test]
    fn a_duplicated_object_persists_with_its_own_identity() {
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let original = object_with_node(&view, "spool-cta-primary");
        let original_node = original.spool_id.clone();
        view.selection.replace(vec![original.id]);

        assert!(view.duplicate_selected_objects());
        let duplicate = *view
            .selection
            .ids()
            .first()
            .expect("the duplicate is selected");
        let duplicate_node = view
            .session
            .runtime
            .object(duplicate)
            .expect("duplicated")
            .spool_id
            .clone();
        assert_ne!(
            duplicate_node, original_node,
            "a duplicate is a different node, not a second handle on one"
        );

        view.save_project().expect("save succeeds");
        let reloaded = reopen(&root);
        assert_eq!(
            reloaded.runtime.objects().len(),
            4,
            "original and duplicate"
        );
        // Both exist, both bound, and they kept their own identities.
        for node in [&original_node, &duplicate_node] {
            assert!(
                reloaded
                    .document
                    .structure
                    .nodes
                    .iter()
                    .any(|candidate| &candidate.id == node),
                "{:?} survived",
                node
            );
        }
        // And they are distinguishable: the duplicate was offset, so its geometry
        // is different in the source, not the same box twice.
        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        assert_eq!(html.matches("data-spool-id=").count(), 4);
    }

    #[test]
    fn undo_removes_a_created_object_and_redo_brings_it_back() {
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let before = view.document_objects().len();
        let created = create(&mut view, ObjectType::Rectangle);
        assert_eq!(
            view.document_objects().len(),
            before + 1,
            "created on the canvas and in the document"
        );
        assert!(
            view.session
                .document
                .structure
                .nodes
                .iter()
                .any(|node| node.id == created.spool_id),
            "the document knows it too"
        );

        view.session.undo().expect("undo replays");
        assert_eq!(view.document_objects().len(), before, "undo removed it");
        assert!(
            !view
                .session
                .document
                .structure
                .nodes
                .iter()
                .any(|node| node.id == created.spool_id),
            "and the document forgot it as well, which is the half that used to be missing"
        );

        view.session.redo().expect("redo replays");
        assert_eq!(
            view.document_objects().len(),
            before + 1,
            "redo restored it"
        );
        assert!(
            view.session
                .document
                .structure
                .nodes
                .iter()
                .any(|node| node.id == created.spool_id),
            "in the document again"
        );
    }

    #[test]
    fn one_creation_is_one_history_entry() {
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let before = view.session.history.undo_len();
        create(&mut view, ObjectType::Rectangle);
        assert_eq!(
            view.session.history.undo_len(),
            before + 1,
            "the document half and the runtime half travel together as one entry"
        );
    }

    #[test]
    fn creating_saving_and_undoing_then_saving_leaves_the_project_without_it() {
        // History is semantic, and the save is a function of the document. An
        // undone creation that is then saved must not leave the element behind.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let created = create(&mut view, ObjectType::Rectangle);
        view.save_project().expect("save succeeds");
        assert!(std::fs::read_to_string(root.join("pages/index.html"))
            .expect("html")
            .contains(created.spool_id.as_str()));

        view.session.undo().expect("undo replays");
        view.save_project().expect("save succeeds");
        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        assert!(
            !html.contains(created.spool_id.as_str()),
            "the undone object was taken back out of the source: {html}"
        );
        assert_eq!(reopen(&root).runtime.objects().len(), 3);
    }

    #[test]
    fn deleting_a_container_takes_its_contents_with_it_in_one_entry() {
        // The cascade, at the level where it is decided: one gesture, one history
        // entry, and the whole subtree gone from the runtime *and* the structure.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let before_runtime = view.document_objects().len();
        let before_nodes = view.persistent_document().structure.nodes.len();
        assert!(
            before_runtime >= 3 && before_nodes >= 3,
            "the fixture nests objects"
        );

        let container = object_with_node(&view, "spool-frame-root");
        let children: Vec<String> = view
            .persistent_document()
            .structure
            .nodes
            .iter()
            .filter(|node| node.parent.as_ref() == Some(&container.spool_id))
            .map(|node| node.id.as_str().to_owned())
            .collect();
        assert_eq!(children.len(), 2, "the landing fixture nests two objects");
        let entries = view.session.history.undo_len();

        view.selection.replace(vec![container.id]);
        assert!(view.delete_selected_objects());

        // Everything went: the container, and what was inside it.
        assert!(!view
            .document_objects()
            .iter()
            .any(|object| children.contains(&object.spool_id.as_str().to_owned())));
        assert!(view.persistent_document().structure.nodes.is_empty());
        assert_eq!(
            view.session.history.undo_len(),
            entries + 1,
            "a cascade is still one gesture and therefore one entry"
        );

        // And no node was left naming a parent that is gone, which is what used
        // to make the project unsaveable.
        let ids: Vec<&str> = view
            .persistent_document()
            .structure
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect();
        for node in &view.persistent_document().structure.nodes {
            if let Some(parent) = &node.parent {
                assert!(
                    ids.contains(&parent.as_str()),
                    "{} is named by a node but is not there",
                    parent.as_str()
                );
            }
            for child in &node.children {
                assert!(
                    ids.contains(&child.as_str()),
                    "{} is listed by a node but is not there",
                    child.as_str()
                );
            }
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn undoing_a_container_deletion_brings_the_whole_subtree_back() {
        // The half of the history contract that a cascade could plausibly break:
        // undo replays a compound in reverse, so the container has to be restored
        // before the children that link into it, or they come back stranded.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let before_runtime: Vec<String> = view
            .document_objects()
            .iter()
            .map(|object| object.spool_id.as_str().to_owned())
            .collect();
        let before_nodes = view.persistent_document().structure.nodes.clone();

        let container = object_with_node(&view, "spool-frame-root");
        view.selection.replace(vec![container.id]);
        assert!(view.delete_selected_objects());

        view.session.undo().expect("undo");

        let after_runtime: Vec<String> = view
            .document_objects()
            .iter()
            .map(|object| object.spool_id.as_str().to_owned())
            .collect();
        assert_eq!(after_runtime, before_runtime, "every object comes back");
        assert_eq!(
            view.persistent_document().structure.nodes.len(),
            before_nodes.len(),
            "and every structural node"
        );
        for node in &before_nodes {
            let restored = view
                .persistent_document()
                .structure
                .nodes
                .iter()
                .find(|candidate| candidate.id == node.id)
                .unwrap_or_else(|| panic!("{} was not restored", node.id.as_str()));
            assert_eq!(
                restored.parent,
                node.parent,
                "{} came back with a different parent",
                node.id.as_str()
            );
        }

        // Redo takes it all away again.
        view.session.redo().expect("redo");
        assert!(view.persistent_document().structure.nodes.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_deleted_container_does_not_come_back_from_the_source() {
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let container = object_with_node(&view, "spool-frame-root");
        view.selection.replace(vec![container.id]);
        assert!(view.delete_selected_objects());
        view.save_project()
            .expect("a container deletion leaves a saveable project");

        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        for gone in [
            "spool-frame-root",
            "spool-text-headline",
            "spool-cta-primary",
        ] {
            assert!(
                !html.contains(gone),
                "{gone} is still in the source: {html}"
            );
        }
        let metadata = std::fs::read_to_string(root.join("lamine.yaml")).expect("metadata");
        assert!(
            !metadata.contains("spool-frame-root"),
            "the deleted container is still in lamine.yaml: {metadata}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_deleted_object_stays_deleted_after_a_reopen() {
        // The inverse lifecycle. A delete has to leave the metadata too, or the
        // object comes back with nothing to draw it.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let victim = object_with_node(&view, "spool-cta-primary");
        let victim_node = victim.spool_id.clone();
        view.selection.replace(vec![victim.id]);

        assert!(view.delete_selected_objects());
        view.save_project().expect("save succeeds");

        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        assert!(
            !html.contains(victim_node.as_str()),
            "its element is gone from the source: {html}"
        );
        let metadata = std::fs::read_to_string(root.join("lamine.yaml")).expect("metadata");
        assert!(
            !metadata.contains(victim_node.as_str()),
            "and its node is gone from lamine.yaml: {metadata}"
        );
        let reloaded = reopen(&root);
        assert_eq!(reloaded.runtime.objects().len(), 2);
        assert!(!ids(&reloaded).iter().any(|id| id == victim_node.as_str()));
    }

    #[test]
    fn deleting_a_created_object_removes_the_element_it_authored() {
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let created = create(&mut view, ObjectType::Rectangle);
        let id = view
            .session
            .runtime
            .object(created.id)
            .expect("still there")
            .id;
        view.selection.replace(vec![id]);
        view.save_project().expect("save the creation first");

        assert!(view.delete_selected_objects());
        view.save_project().expect("save succeeds");
        assert!(!std::fs::read_to_string(root.join("pages/index.html"))
            .expect("html")
            .contains(created.spool_id.as_str()));
        assert_eq!(reopen(&root).runtime.objects().len(), 3);
    }

    #[test]
    fn a_created_object_does_not_disturb_the_rest_of_the_authored_source() {
        // The smallest reasonable diff: one element added inside the frame, and
        // nothing else in the file moved.
        let root = project_scratch("landing.spool");
        let before = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        let mut view = project_view(&root);
        create(&mut view, ObjectType::Rectangle);
        view.save_project().expect("save succeeds");
        let after = std::fs::read_to_string(root.join("pages/index.html")).expect("html");

        // Remove the one line that was added; the rest has to be the original.
        let added: Vec<&str> = after
            .lines()
            .filter(|line| !before.contains(*line))
            .collect();
        assert_eq!(added.len(), 1, "exactly one line added: {added:?}");
        let original_lines: Vec<&str> = before.lines().collect();
        for line in &original_lines {
            assert!(after.contains(line), "an authored line survived: {line}");
        }
        assert_eq!(
            after
                .lines()
                .filter(|line| line.contains("data-spool-id"))
                .count(),
            4,
            "three authored identities plus the created one"
        );
    }

    #[test]
    fn a_created_object_stays_inside_the_project() {
        // Confinement is the loader's rule and creation does not get an exemption:
        // the binding names a file under the root, and nothing is written outside.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        create(&mut view, ObjectType::Rectangle);
        let outcome = view.save_project().expect("save succeeds");
        for path in &outcome.written {
            assert!(
                path.starts_with(&root),
                "{} escaped the project root",
                path.display()
            );
        }
        assert!(outcome.written.iter().all(|path| path.starts_with(&root)));
    }

    #[test]
    fn a_created_object_in_a_reopened_project_does_not_reuse_an_identity() {
        // The allocator has to read the identities already on disk, or the first
        // creation after a reopen collides with one the project is using and two
        // objects arrive at the same element.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        create(&mut view, ObjectType::Rectangle);
        view.save_project().expect("save succeeds");

        let mut reopened = project_view(&root);
        let created = create(&mut reopened, ObjectType::Rectangle);
        reopened.save_project().expect("save succeeds");

        let reloaded = reopen(&root);
        let mut seen = std::collections::BTreeSet::new();
        for id in ids(&reloaded) {
            assert!(seen.insert(id.clone()), "{id} appears twice");
        }
        assert!(seen.contains(created.spool_id.as_str()));
        assert_eq!(reloaded.runtime.objects().len(), 5);
    }

    #[test]
    fn create_duplicate_move_style_save_reopen_keeps_the_whole_chain() {
        // The combination the milestone asks for, end to end.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        let frame = object_with_node(&view, "spool-frame-root");
        view.selection.replace(vec![frame.id]);

        let created = create(&mut view, ObjectType::Rectangle);
        let created_id = view.session.runtime.object(created.id).expect("created").id;
        assert!(view.duplicate_selected_objects(), "duplicate the rectangle");
        let duplicate_id = *view.selection.ids().first().expect("the duplicate");

        view.selection.replace(vec![created_id]);
        view.nudge_selection(30.0, 20.0);
        view.apply_selected_style(StyleEdit::Opacity(0.5));
        view.save_project().expect("save succeeds");

        let reloaded = reopen(&root);
        assert_eq!(
            reloaded.runtime.objects().len(),
            5,
            "3 + created + duplicate"
        );
        let all = ids(&reloaded);
        let duplicate_node = view
            .session
            .runtime
            .object(duplicate_id)
            .map(|object| object.spool_id.clone())
            .unwrap_or_else(|| {
                view.selection
                    .ids()
                    .first()
                    .and_then(|id| view.session.runtime.object(*id))
                    .map(|object| object.spool_id.clone())
                    .expect("the duplicate")
            });
        assert!(
            all.iter().any(|id| id == duplicate_node.as_str()),
            "the duplicate's own identity is in the document: {all:?}"
        );
        assert_eq!(all.len(), 5);
        assert_eq!(
            all.iter().collect::<std::collections::BTreeSet<_>>().len(),
            5,
            "five distinct identities"
        );
        // The duplicate kept its own geometry.
        let nodes = &reloaded.document.structure.nodes;
        assert!(nodes.len() >= 5);
        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        assert_eq!(html.matches("data-spool-id=").count(), 5);
        assert!(
            html.contains("opacity: 0.5"),
            "the style edit persisted: {html}"
        );
    }

    #[test]
    fn a_created_element_is_indented_like_the_authored_children() {
        // The authored file has to stay readable as source. An element written at
        // column zero next to two-space children looks like a machine produced it,
        // and it is the only cue that the insertion reused the document's layout
        // rather than inventing one.
        let root = project_scratch("landing.spool");
        let mut view = project_view(&root);
        create(&mut view, ObjectType::Rectangle);
        view.save_project().expect("save succeeds");

        let html = std::fs::read_to_string(root.join("pages/index.html")).expect("html");
        let created = html
            .lines()
            .find(|line| line.contains("spool-node-"))
            .expect("the created element is on its own line");
        assert!(
            created.starts_with("      "),
            "indented like its siblings, not at column zero: {created:?}"
        );
        // And its parent is still closed on a line of its own.
        assert!(
            html.contains("\n    </main>"),
            "the parent's close tag kept its line: {html}"
        );
    }

    #[test]
    fn an_object_with_no_document_node_is_reported_rather_than_invented() {
        // The escape hatch stays honest. A created object is given a node by
        // `commit_created`, so this is the shape that must NOT happen: an object
        // put on the canvas with no node behind it. It has no binding and no
        // parent, so there is nowhere to author it — and the save says so instead
        // of inventing an element for it.
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let created = view.session.runtime.create_object(
            ObjectType::Rectangle,
            point(400.0, 400.0),
            size(60.0, 60.0),
            None,
        );
        view.commit(DocumentCommand::insert(vec![view
            .session
            .runtime
            .placement(created.id)
            .expect("new object is placed")]));

        let outcome = view.save_project().expect("save succeeds");
        let reported: Vec<&str> = outcome
            .unsupported
            .iter()
            .map(|edit| edit.node.as_str())
            .collect();
        assert_eq!(
            reported,
            vec![created.spool_id.as_str()],
            "the object with no node is named, not quietly skipped"
        );
        assert!(
            outcome.unsupported[0]
                .reason
                .contains("persistent document"),
            "and the reason says why: {}",
            outcome.unsupported[0].reason
        );
        assert!(
            !std::fs::read_to_string(root.join("index.html"))
                .expect("html readable")
                .contains("spool-node"),
            "nothing was invented in the source to stand in for it"
        );
    }

    #[test]
    fn a_refused_node_keeps_its_baseline_and_does_not_hold_back_a_node_that_saved() {
        // A refusal must be scoped to the node that earned it.
        //
        // The baseline refresh used to *replace* the whole map with the nodes the
        // save accepted, so a refused node lost its baseline entirely. On the next
        // save that node reported "no opened state to compare against" — a
        // different and far less useful reason than the one that applies — and its
        // edit was never attempted again. The nodes that did save were unaffected,
        // so the bookkeeping bug looked like a per-node problem.
        //
        // Two buttons sharing one `.cta` rule gives exactly that shape: a fill edit
        // on either is refused because the rule governs two elements, while a move
        // on either lands as an inline `transform` the rule has no opinion about.
        let root = std::env::temp_dir().join(format!(
            "spool-refusal-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create project");
        std::fs::write(
            root.join("index.html"),
            "<!doctype html>\n<html>\n<head><link rel=\"stylesheet\" href=\"styles.css\" /></head>\n<body>\n  <a class=\"cta\" data-spool-id=\"spool-cta-primary\">One</a>\n  <a class=\"cta\" data-spool-id=\"spool-cta-secondary\">Two</a>\n</body>\n</html>\n",
        )
        .expect("write html");
        std::fs::write(root.join("styles.css"), ".cta { background: #3b5bfd; }\n")
            .expect("write css");
        let node = |id: &str| {
            format!(
                "  - id: \"{id}\"\n    name: \"{id}\"\n    kind: \"frame\"\n    parent: null\n    file: \"index.html\"\n    selector: \"[data-spool-id=\\\"{id}\\\"]\"\n    children: []\n"
            )
        };
        std::fs::write(
            root.join("lamine.yaml"),
            format!(
                "version: 1\nnodes:\n{}{}",
                node("spool-cta-primary"),
                node("spool-cta-secondary")
            ),
        )
        .expect("write metadata");

        let mut view = project_view(&root);
        let refused = id_of(&view, "spool-cta-primary");
        let saved = id_of(&view, "spool-cta-secondary");

        // The node that lands: a move needs no stylesheet declaration to rewrite.
        let resting = view.session.runtime.geometry(saved).unwrap();
        view.selection.replace(vec![saved]);
        begin_live_move_with_snap(&mut view, &[saved], true);
        drag_to(&mut view, point(0.0, 30.0));
        let moved = view.session.runtime.geometry(saved).unwrap().position;
        assert_ne!(moved, resting.position, "the move really happened");

        // The node that is refused: the `.cta` fill governs both buttons.
        assert_eq!(
            object_with_node(&view, "spool-cta-primary")
                .fill
                .map(|fill| fill.color),
            Some(Color::from_rgb(0x3b_5b_fd)),
            "the fill comes from the rule the two buttons share"
        );
        view.selection.replace(vec![refused]);
        assert!(
            view.apply_selected_style(StyleEdit::Fill(Some(Color::from_rgb(0xff0000)))),
            "the style edit is a real operation"
        );

        let outcome = view.save_project().expect("first save succeeds");
        let reported: Vec<(&str, &str)> = outcome
            .unsupported
            .iter()
            .map(|edit| (edit.node.as_str(), edit.kind))
            .collect();
        assert!(
            !outcome
                .unsupported
                .iter()
                .any(|edit| edit.node.as_str() == "spool-cta-secondary"),
            "the node that could be written was written: {reported:?}"
        );
        assert_eq!(
            reported,
            vec![("spool-cta-primary", "style")],
            "and the shared rule leaves exactly the one node reported"
        );

        let css_after_first = std::fs::read_to_string(root.join("styles.css")).unwrap();
        assert_eq!(
            css_after_first, ".cta { background: #3b5bfd; }\n",
            "the shared rule is untouched"
        );
        let html_after_first = std::fs::read_to_string(root.join("index.html")).unwrap();
        assert!(
            html_after_first.contains("translate(0px"),
            "the accepted edit is in the source: {html_after_first}"
        );
        assert!(
            !html_after_first.contains("background"),
            "and the refused fill was not smuggled in as an inline override"
        );

        // The second save is the one that matters. The accepted node's baseline has
        // advanced, so nothing is written for it again — no double-apply. The
        // refused node still holds a baseline, so the refusal is reported as the
        // conflict it is rather than as a missing baseline.
        let outcome = view.save_project().expect("second save succeeds");
        assert_eq!(
            std::fs::read_to_string(root.join("index.html")).unwrap(),
            html_after_first,
            "the accepted node's baseline advanced, so its edit is not written twice"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("styles.css")).unwrap(),
            css_after_first,
            "and the refused rule is still untouched"
        );
        let again = outcome
            .unsupported
            .iter()
            .find(|edit| edit.node.as_str() == "spool-cta-primary")
            .expect("the refused node is reported again");
        assert_eq!(again.kind, "style", "with the same kind of refusal");
        assert!(
            !again.reason.contains("no opened state"),
            "and reported as the real conflict rather than a missing baseline: {}",
            again.reason
        );
    }

    #[test]
    fn a_style_change_rewrites_the_declaration_that_already_owns_it() {
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = object_with_node(&view, "spool-cta-primary");
        assert_eq!(
            cta.fill.map(|fill| fill.color),
            Some(Color::from_rgb(0x3b5bfd)),
            "the CTA's colour comes from the authored stylesheet"
        );

        // The inspector writes a new fill through the ordinary style path.
        let red = Color::from_rgb(0xff0000);
        view.selection.replace(vec![cta.id]);
        assert!(
            view.apply_selected_style(StyleEdit::Fill(Some(red))),
            "the inspector path applies and commits the fill"
        );

        let outcome = view.save_project().expect("save succeeds");
        assert!(
            outcome.unsupported.is_empty(),
            "the fill had an authored owner: {:?}",
            outcome.unsupported
        );
        let css = std::fs::read_to_string(root.join("styles.css")).expect("css readable");
        assert!(
            css.contains("background: #ff0000"),
            "the owning declaration was rewritten in place: {css}"
        );
        assert_eq!(
            css.matches(":root").count(),
            1,
            "no rule was added to make room for the edit"
        );
        assert_eq!(
            css.matches("background").count(),
            1,
            "the edit replaced a value rather than adding a declaration"
        );

        // And it comes back as what was authored.
        let reopened = project_view(&root);
        assert_eq!(
            object_with_node(&reopened, "spool-cta-primary")
                .fill
                .map(|fill| fill.color),
            Some(red)
        );
    }

    #[test]
    fn what_the_renderer_would_draw_comes_from_source_not_from_editor_defaults() {
        // The strongest statement about appearance that does not need a screen:
        // these are the exact values the render path hands to GPUI for a loaded
        // project. If the editor defaults were still winning, the CTA would draw
        // in the theme's blue-on-paper instead of its authored white-on-accent.
        let root = project_scratch("landing");
        let view = project_view(&root);

        let cta = object_with_node(&view, "spool-cta-primary");
        assert_eq!(
            ink_of(&cta),
            rgb(0xffffff),
            "the CTA's label is authored white, not a contrast guess"
        );
        assert_eq!(
            cta.fill.map(|fill| fill.color),
            Some(Color::from_rgb(0x3b5bfd))
        );
        assert_eq!(
            text_size_of(&cta),
            16.0,
            "no authored font size, so the renderer's own default applies"
        );

        let headline = object_with_node(&view, "spool-text-headline");
        assert_eq!(
            ink_of(&headline),
            rgb(0x16161d),
            "the headline is the ink colour body authored"
        );
        assert_eq!(
            text_size_of(&headline),
            16.0,
            "and uses the inherited font size"
        );

        // A node the source gave no paint still falls back, and says so.
        let frame = object_with_node(&view, "spool-frame-root");
        assert!(frame.fill.is_some(), "the editor default still paints it");
        assert_eq!(ink_of(&frame), rgb(0x16161d), "from the inherited colour");
    }

    #[test]
    fn a_loaded_project_renders_its_authored_text_style_and_geometry() {
        let root = project_scratch("landing");
        let view = project_view(&root);

        let headline = object_with_node(&view, "spool-text-headline");
        assert_eq!(
            headline.text_content.as_deref(),
            Some("Design in source, structure in Spool"),
            "authored text reaches the runtime"
        );
        assert_eq!(
            headline.text_color,
            // `body { color: var(--ink) }` inherited by the headline.
            Some(Color::from_rgb(0x16161d)),
            "an inherited colour is still an authored colour"
        );

        let cta = object_with_node(&view, "spool-cta-primary");
        assert_eq!(cta.text_content.as_deref(), Some("Start designing"));
        assert_eq!(cta.text_color, Some(Color::from_rgb(0xffffff)));
        assert_eq!(
            cta.fill.map(|fill| fill.color),
            Some(Color::from_rgb(0x3b5bfd))
        );
        // Padding from `.cta` is part of the box, not a decoration.
        assert!(
            cta.size.height >= 40.0,
            "the CTA's box accounts for its padding, got {}",
            cta.size.height
        );

        // Children flow inside their parent's content box.
        let frame = object_with_node(&view, "spool-frame-root");
        assert_eq!(frame.size.width, crate::visual::DEFAULT_CONTENT_WIDTH);
        assert!(
            headline.position.y >= frame.position.y,
            "the headline is laid out below the frame's origin"
        );
        assert!(
            cta.position.y >= headline.position.y,
            "and the CTA follows the headline in document order"
        );
    }

    // ── selection and hierarchy parity ────────────────────────────────────

    /// A canvas whose starter scene has real hierarchy in it: Editor is a child
    /// of Landing, and Editor sits *inside* Landing so one click point is inside
    /// both boxes. Everything below is stated against that, because a flat
    /// document cannot tell a hierarchy-aware selection from an unaware one.
    fn nested_canvas() -> CanvasView {
        let mut canvas = CanvasView::new();
        let landing = canvas
            .session
            .runtime
            .object(ObjectId::LANDING)
            .unwrap()
            .spool_id
            .clone();
        let editor = canvas
            .session
            .runtime
            .object(ObjectId::EDITOR)
            .unwrap()
            .spool_id
            .clone();
        let node = |id: NodeId, parent: Option<NodeId>, name: &str| {
            crate::source_document::StructuralNode {
                id,
                name: name.to_owned(),
                kind: "frame".to_owned(),
                parent,
                children: Vec::new(),
                source: crate::source_document::SourceBinding {
                    file: "index.html".to_owned(),
                    selector: name.to_owned(),
                },
            }
        };
        canvas.session.document.structure.nodes = vec![
            node(landing.clone(), None, "Landing"),
            node(editor, Some(landing), "Editor"),
        ];
        // Editor (40,60)-(350,312) now sits inside Landing (0,24)-(430,310).
        canvas
            .session
            .runtime
            .set_position(ObjectId::EDITOR, point(40.0, 60.0));
        canvas
    }

    /// A point inside Editor, and therefore also inside Landing.
    fn inside_editor() -> Point<f32> {
        point(100.0, 100.0)
    }

    fn selection_mods(shift: bool, alt: bool, control: bool, platform: bool) -> gpui::Modifiers {
        gpui::Modifiers {
            shift,
            alt,
            control,
            platform,
            ..gpui::Modifiers::none()
        }
    }

    #[test]
    fn a_click_resolves_through_the_hierarchy_to_the_object_a_user_can_see() {
        let canvas = nested_canvas();
        let hit = canvas.session.runtime.hit_test(inside_editor());
        assert_eq!(
            hit,
            Some(ObjectId::EDITOR),
            "the raw hit is the child, which is what is painted on top"
        );
        // The canvas resolves it before anything acts on it.
        let hierarchy = canvas.hierarchy();
        assert_eq!(
            hierarchy.selection_target(hit.unwrap(), false),
            ObjectId::LANDING,
            "and what gets selected is the frame the user can see a boundary around"
        );
    }

    #[test]
    fn command_click_reaches_past_the_container_to_the_child_under_the_pointer() {
        let canvas = nested_canvas();
        let hit = canvas.session.runtime.hit_test(inside_editor()).unwrap();
        let hierarchy = canvas.hierarchy();
        assert_eq!(
            hierarchy.selection_target(hit, deep_select(selection_mods(false, false, true, false))),
            ObjectId::EDITOR,
            "the documented escape hatch in all four products"
        );
    }

    #[test]
    fn the_deep_select_modifier_is_named_separately_from_the_snap_suppressor() {
        // They are the same key today and are not the same intent: one picks
        // *what* to select, the other switches snapping off for a drag. A test
        // that only asserted their current agreement would pass after either one
        // was wrongly deleted.
        let command = selection_mods(false, false, true, false);
        assert!(deep_select(command));
        assert!(suspends_snap(command));
        // `⇧` alone is neither.
        let shifted = selection_mods(true, false, false, false);
        assert!(!deep_select(shifted));
        assert!(!suspends_snap(shifted));
    }

    #[test]
    fn a_selection_never_holds_an_object_and_its_ancestor_at_once() {
        let canvas = nested_canvas();
        let hierarchy = canvas.hierarchy();
        // Every way a two-element selection can arrive.
        for ids in [
            vec![ObjectId::EDITOR, ObjectId::LANDING],
            vec![ObjectId::LANDING, ObjectId::EDITOR],
            vec![ObjectId::EDITOR, ObjectId::EDITOR, ObjectId::LANDING],
        ] {
            let normalized = hierarchy.normalize(&ids);
            assert!(
                !normalized.contains(&ObjectId::EDITOR) || !normalized.contains(&ObjectId::LANDING),
                "the container wins over its own contents: {normalized:?}"
            );
            assert_eq!(normalized, vec![ObjectId::LANDING]);
        }
    }

    #[test]
    fn a_marquee_that_catches_a_frame_and_its_contents_selects_the_frame_only() {
        // Driven through the production `finish_marquee`, not through
        // `Hierarchy::normalize`, so the mutation this rules out has to survive
        // the real gesture path to get away with it.
        let mut canvas = nested_canvas();
        let everything = canvas.session.runtime.objects_in(WorldRect::from_points(
            point(-10.0, -10.0),
            point(900.0, 900.0),
        ));
        assert!(
            everything.contains(&ObjectId::EDITOR),
            "the raw sweep sees both"
        );

        canvas.marquee = Some(MarqueeGesture {
            start: point(-10.0, -10.0),
            current: point(900.0, 900.0),
            additive: false,
            initial_selection: Vec::new(),
        });
        canvas.finish_marquee(point(900.0, 900.0));

        assert_eq!(
            canvas.selection.ids(),
            &[ObjectId::LANDING, ObjectId::FEATURES, ObjectId::MOBILE],
            "a sweep covering a whole frame selects the frame, not the frame and its child"
        );
    }

    #[test]
    fn an_additive_marquee_keeps_what_was_already_selected_and_still_collapses_nesting() {
        let mut canvas = nested_canvas();
        canvas.selection.click_flat(Some(ObjectId::FEATURES), false);
        canvas.marquee = Some(MarqueeGesture {
            start: point(-10.0, -10.0),
            current: point(900.0, 900.0),
            additive: true,
            initial_selection: canvas.selection.ids().to_vec(),
        });
        canvas.finish_marquee(point(900.0, 900.0));
        assert_eq!(
            canvas.selection.ids(),
            &[ObjectId::LANDING, ObjectId::FEATURES, ObjectId::MOBILE],
            "the prior selection survives and the nested pair still collapses"
        );
    }

    #[test]
    fn selection_order_is_document_order_whatever_route_produced_it() {
        let canvas = nested_canvas();
        let hierarchy = canvas.hierarchy();
        let by_click = vec![ObjectId::MOBILE, ObjectId::LANDING, ObjectId::FEATURES];
        let by_marquee = vec![ObjectId::FEATURES, ObjectId::MOBILE, ObjectId::LANDING];
        assert_eq!(
            hierarchy.normalize(&by_click),
            hierarchy.normalize(&by_marquee),
            "two routes to one set are one selection"
        );
        assert_eq!(
            hierarchy.normalize(&by_click),
            vec![ObjectId::LANDING, ObjectId::FEATURES, ObjectId::MOBILE]
        );
    }

    #[test]
    fn an_additive_selection_never_ends_up_holding_a_duplicate() {
        let canvas = nested_canvas();
        let mut selection = canvas.selection.clone();
        let hierarchy = canvas.hierarchy();
        selection.click_flat(Some(ObjectId::FEATURES), true);
        selection.click_flat(Some(ObjectId::FEATURES), false);
        assert_eq!(selection.ids(), &[ObjectId::FEATURES]);
        // And the rebuild is idempotent, so a second pass cannot double anything.
        let once = hierarchy.normalize(selection.ids());
        assert_eq!(hierarchy.normalize(&once), once);
    }

    #[test]
    fn toggling_a_selected_object_off_is_not_normalized_away() {
        // The rebuild has to be able to *remove* an id, not only filter down to
        // what is in the document. A toggle that rebuilt first would leave the
        // id in and silently turn `⇧`-click-off into a no-op.
        //
        // Driven through the real `Selection::click` with a real hierarchy, so
        // the ordering of "remove" against "normalize" is what is under test
        // rather than a helper that always runs it the right way round.
        let canvas = nested_canvas();
        let hierarchy = canvas.hierarchy();
        let mut selection = canvas.selection.clone();
        selection.click(Some(ObjectId::FEATURES), false, &hierarchy);
        selection.click(Some(ObjectId::MOBILE), true, &hierarchy);
        assert_eq!(selection.ids(), &[ObjectId::FEATURES, ObjectId::MOBILE]);
        selection.click(Some(ObjectId::FEATURES), true, &hierarchy);
        assert_eq!(
            selection.ids(),
            &[ObjectId::MOBILE],
            "the toggled-off id is gone rather than normalized back in"
        );
    }

    #[test]
    fn toggling_an_ancestor_out_of_a_nested_pair_leaves_the_descendant_alone() {
        // The ordering matters most when the selection is *not* already clean.
        // Rebuilding before removing would collapse the pair to its ancestor
        // first, and the toggle would then remove the only member and leave
        // nothing — so the child the user could still see selected would vanish
        // from the selection instead of becoming it.
        let canvas = nested_canvas();
        let hierarchy = canvas.hierarchy();
        let mut selection = Selection::flat(vec![ObjectId::LANDING, ObjectId::EDITOR]);
        assert_eq!(
            selection.ids(),
            &[ObjectId::LANDING, ObjectId::EDITOR],
            "seeded deliberately unclean, which no production path produces"
        );

        selection.click(Some(ObjectId::LANDING), true, &hierarchy);
        assert_eq!(
            selection.ids(),
            &[ObjectId::EDITOR],
            "the child is what remains once the container is toggled off"
        );
    }

    #[test]
    fn tab_walks_siblings_rather_than_document_order() {
        let mut canvas = nested_canvas();
        let hierarchy = canvas.hierarchy();
        // Editor's only sibling is itself; its parent's are the roots. If `Tab`
        // walked document order instead, the next row after Editor would be
        // Features — a different object on a different branch.
        assert_eq!(hierarchy.siblings(ObjectId::EDITOR), vec![ObjectId::EDITOR]);
        canvas.selection.click_flat(Some(ObjectId::EDITOR), false);
        // A lone sibling is nowhere to step to, so the key falls through rather
        // than jumping the user out of the branch they are standing in.
        assert!(!canvas.traverse_selection(Traversal::Sibling(true)));
        assert_eq!(canvas.selection.ids(), &[ObjectId::EDITOR]);
        // Out to the frame, and then `Tab` moves among *its* siblings.
        assert!(canvas.traverse_selection(Traversal::Ascend));
        assert_eq!(canvas.selection.ids(), &[ObjectId::LANDING]);
        assert!(canvas.traverse_selection(Traversal::Sibling(true)));
        assert_eq!(canvas.selection.ids(), &[ObjectId::FEATURES]);
    }

    #[test]
    fn traversal_descends_and_ascends_and_reports_when_there_is_nowhere_to_go() {
        let mut canvas = nested_canvas();
        // Nothing selected: the first step picks the first root.
        assert!(canvas.traverse_selection(Traversal::Sibling(true)));
        assert_eq!(canvas.selection.ids(), &[ObjectId::LANDING]);
        // Descend into the frame.
        assert!(canvas.traverse_selection(Traversal::Descend));
        assert_eq!(canvas.selection.ids(), &[ObjectId::EDITOR]);
        // A leaf has nothing to descend into.
        assert!(!canvas.traverse_selection(Traversal::Descend));
        assert_eq!(
            canvas.selection.ids(),
            &[ObjectId::EDITOR],
            "a step with nowhere to go leaves the selection alone"
        );
        // Back out to the frame.
        assert!(canvas.traverse_selection(Traversal::Ascend));
        assert_eq!(canvas.selection.ids(), &[ObjectId::LANDING]);
        // A root is the top of the ladder.
        assert!(!canvas.traverse_selection(Traversal::Ascend));
    }

    #[test]
    fn tab_cycles_through_the_roots_and_shift_tab_walks_back_through_them() {
        let mut canvas = nested_canvas();
        let order = [ObjectId::LANDING, ObjectId::FEATURES, ObjectId::MOBILE];
        assert!(canvas.traverse_selection(Traversal::Sibling(true)));
        assert_eq!(canvas.selection.ids(), &order[..1]);
        assert!(canvas.traverse_selection(Traversal::Sibling(true)));
        assert_eq!(canvas.selection.ids(), &order[1..2]);
        assert!(canvas.traverse_selection(Traversal::Sibling(true)));
        assert_eq!(canvas.selection.ids(), &order[2..3]);
        // Wraps, which is the only reliable way back to the start.
        assert!(canvas.traverse_selection(Traversal::Sibling(true)));
        assert_eq!(canvas.selection.ids(), &order[..1]);
        assert!(canvas.traverse_selection(Traversal::Sibling(false)));
        assert_eq!(canvas.selection.ids(), &order[2..3]);
    }

    #[test]
    fn the_layers_panel_and_the_canvas_agree_on_one_selection_either_way() {
        let mut canvas = nested_canvas();
        let mut panel = RetainedLayers::default();
        panel.synchronize(&canvas);
        // Canvas → panel: a selection made on the canvas is the panel's, and the
        // panel reports it rather than owning it.
        canvas.selection.click_flat(Some(ObjectId::EDITOR), false);
        panel.synchronize(&canvas);
        assert_eq!(panel.selected(), &[ObjectId::EDITOR]);
        // Panel → canvas: the panel's only route is `select_object`, so a
        // selection made there lands in the same model.
        canvas.selection.click_flat(None, false);
        assert!(canvas.selection.is_empty());
        panel.synchronize(&canvas);
        let clicked = panel.selected().first().copied();
        assert_eq!(clicked, None, "the panel saw the clear");
    }

    #[test]
    fn a_row_that_is_inside_a_selected_ancestor_is_not_itself_a_member() {
        let mut canvas = nested_canvas();
        let mut panel = RetainedLayers::default();
        panel.synchronize(&canvas);
        canvas.selection.click_flat(Some(ObjectId::LANDING), false);
        panel.synchronize(&canvas);
        // The frame is the member. Its child is drawn differently, not selected,
        // because the canvas now forbids the two from being in one selection.
        assert!(panel.selected().contains(&ObjectId::LANDING));
        assert!(
            !panel.selected().contains(&ObjectId::EDITOR),
            "a child inside a selected frame is not part of the selection"
        );
    }

    #[test]
    fn the_traversal_keys_are_reachable_and_unclaimed_by_anything_else() {
        // Asserted through the command table rather than through the layers
        // panel's private grammar: what matters here is that the editor can
        // reach every rung of the ladder, and that none of them was taken by a
        // key that already meant something else.
        use crate::commands::{resolve, Command, Scope};
        let bare = selection_mods(false, false, false, false);
        let shifted = selection_mods(true, false, false, false);
        let command = selection_mods(false, false, false, true);
        let ascend_also = selection_mods(false, false, true, false);
        assert_eq!(
            resolve("tab", bare, Scope::Editor),
            Some(Command::Traverse(Traversal::Sibling(true)))
        );
        assert_eq!(
            resolve("tab", shifted, Scope::Editor),
            Some(Command::Traverse(Traversal::Sibling(false)))
        );
        assert_eq!(
            resolve("enter", shifted, Scope::Editor),
            Some(Command::Traverse(Traversal::Ascend))
        );
        assert_eq!(
            resolve("down", command, Scope::Editor),
            Some(Command::Traverse(Traversal::Descend))
        );
        assert_eq!(
            resolve("up", ascend_also, Scope::Editor),
            Some(Command::Traverse(Traversal::Ascend)),
            "`⌘↑` is select-parent in every product, so it is the natural ascend"
        );
        // Bare `Enter` is descend, and `F2` is the one spelling of rename. The
        // layers panel gives `Enter` back rather than claiming it, so the two
        // surfaces no longer disagree about what the key means.
        assert_eq!(
            resolve("enter", bare, Scope::Editor),
            Some(Command::Traverse(Traversal::Descend)),
            "bare Enter descends, as it does in every product in the corpus"
        );
        assert_eq!(
            resolve("f2", bare, Scope::Editor),
            Some(Command::Rename),
            "rename keeps one unambiguous spelling"
        );
    }

    #[test]
    fn select_all_still_reaches_every_root_after_the_normalization_change() {
        let mut canvas = nested_canvas();
        canvas.select_roots();
        assert_eq!(
            canvas.selection.ids(),
            &[ObjectId::LANDING, ObjectId::FEATURES, ObjectId::MOBILE],
            "three roots: Landing keeps its place even though it has a child"
        );
    }

    // ---------------------------------------------------------------------
    // The interaction workflow, end to end
    //
    // Every test above proves one system against itself: a move is measured
    // against a move, a resize against a resize, a layer row against a layer
    // row. That is how each of them was able to be correct on its own while the
    // product loop a user actually walks stayed broken.
    //
    // These tests walk the loop instead. Each one starts from a selection and
    // runs the real production path — the same `Interaction` variants the
    // pointer drives, the same `commit`, the same history — and asserts the
    // claim at the seam rather than inside a subsystem. The letter names the
    // step in the product loop so a failure says which part of the walk broke.
    // ---------------------------------------------------------------------

    /// The same `two_box_selection` the transform tests use, selected the way a
    /// user selects it: click the first, add the second.
    fn select_two_boxes(canvas: &mut CanvasView) -> (ObjectId, ObjectId) {
        let ids = two_box_selection(canvas);
        canvas.selection.click_flat(Some(ids.0), false);
        canvas.selection.click_flat(Some(ids.1), true);
        ids
    }

    fn geometries(canvas: &CanvasView, ids: &[ObjectId]) -> Vec<ObjectGeometry> {
        ids.iter().map(|id| geometry_of(canvas, *id)).collect()
    }

    /// A: select, move, undo, redo.
    ///
    /// The floor the whole loop stands on. If this is wrong, every other
    /// workflow test is measuring noise.
    #[test]
    fn workflow_a_select_move_undo_redo() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        canvas.selection.click_flat(Some(id), false);
        let before = geometry_of(&canvas, id);

        begin_live_move(&mut canvas, &[id]);
        drag_to(&mut canvas, point(40.0, 25.0));

        let moved = geometry_of(&canvas, id);
        assert_eq!(moved.position, point(40.0, 49.0), "moved by the drag");
        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "the whole move is one step, whatever the pointer did on the way"
        );

        assert!(canvas.undo_history());
        assert_eq!(geometry_of(&canvas, id), before);
        assert!(canvas.redo_history());
        assert_eq!(geometry_of(&canvas, id), moved);
    }

    /// B: multi-select, move, undo.
    ///
    /// The claim under test is not "both moved" — it is "both moved *together*".
    /// A per-object move implementation passes the first assertion and destroys
    /// the arrangement the user built, and only undo reveals it.
    #[test]
    fn workflow_b_multi_select_move_undo_keeps_the_arrangement() {
        let mut canvas = CanvasView::new();
        let ids = select_two_boxes(&mut canvas);
        let before = geometries(&canvas, &[ids.0, ids.1]);
        let gap_before = (
            before[1].position.x - before[0].position.x,
            before[1].position.y - before[0].position.y,
        );

        // Snapping suspended: this is about the mapping, and a magnet would move
        // the expected result rather than the behaviour under test.
        begin_live_move_with_snap(&mut canvas, &[ids.0, ids.1], true);
        drag_to(&mut canvas, point(30.0, 20.0));

        let after = geometries(&canvas, &[ids.0, ids.1]);
        assert_eq!(
            (after[0].position.x, after[0].position.y),
            (30.0, 20.0),
            "the first member moved by the whole delta"
        );
        assert_eq!(
            (after[1].position.x, after[1].position.y),
            (230.0, 120.0),
            "and so did the second, by the same delta"
        );
        assert_eq!(
            (
                after[1].position.x - after[0].position.x,
                after[1].position.y - after[0].position.y
            ),
            gap_before,
            "a selection moves as a unit, so the gap between its members is invariant"
        );
        assert_eq!(
            after[0].size, before[0].size,
            "a move never resizes anything"
        );

        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "two objects, one gesture, one entry"
        );
        assert!(canvas.undo_history());
        assert_eq!(
            geometries(&canvas, &[ids.0, ids.1]),
            before,
            "undo restores the whole arrangement, not one object"
        );
    }

    /// C: multi-select, resize, undo.
    ///
    /// The cross-feature claim here is that a resize over a union box is
    /// *proportional*: the members keep their arrangement inside the new box
    /// rather than being pinned to their old coordinates.
    #[test]
    fn workflow_c_multi_select_resize_undo_scales_the_arrangement() {
        let mut canvas = CanvasView::new();
        let ids = select_two_boxes(&mut canvas);
        let before = geometries(&canvas, &[ids.0, ids.1]);
        // `two_box_selection` is built so this is arithmetic, not a second
        // implementation of the rule: union is (0,0) 300x150.
        assert_eq!(
            transform_bounds(&snapshots(&canvas.session.runtime, &[ids.0, ids.1])),
            Some(ObjectGeometry {
                position: point(0.0, 0.0),
                size: size(300.0, 150.0)
            }),
            "the two boxes have the union box the resize is measured against"
        );

        // A bottom-right handle *adds* to the far edges, so this halves the
        // union box: 300x150 becomes 150x75, a scale of exactly 0.5.
        drag_two_box_resize(
            &mut canvas,
            ids,
            ResizeHandle::BottomRight,
            point(-150.0, -75.0),
            false,
            false,
        );

        let after = geometries(&canvas, &[ids.0, ids.1]);
        assert_eq!(
            after[0].size,
            size(50.0, 25.0),
            "a member is scaled by the union box's scale factor, not left alone"
        );
        assert_eq!(
            after[1].position,
            point(100.0, 50.0),
            "and its offset inside the box is scaled with it"
        );
        assert_eq!(
            (
                after[1].position.x - after[0].position.x,
                after[1].position.y - after[0].position.y
            ),
            (100.0, 50.0),
            "the arrangement survives as a scaled copy of itself: the gap between \
             the members is the old gap times the same 0.5 the sizes went through"
        );

        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "a multi-selection resize is one step"
        );
        assert!(canvas.undo_history());
        assert_eq!(geometries(&canvas, &[ids.0, ids.1]), before);
        assert!(canvas.redo_history());
        assert_eq!(geometries(&canvas, &[ids.0, ids.1]), after);
    }

    /// D: move, snap, commit, undo.
    ///
    /// Two systems meet here. The snap engine corrects the delta during the drag;
    /// the history records the *corrected* result on release. If the recorded
    /// `before` were the pre-snap proposal, undo would leave the object on the
    /// guide rather than where it started.
    #[test]
    fn workflow_d_snap_then_commit_then_undo_returns_to_the_authored_place() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let before = geometry_of(&canvas, id);

        begin_live_move(&mut canvas, &[id]);
        // Editor's left edge sits at 454; this puts Landing's at 456, which is
        // inside the 8px threshold at 100% and so must click into line.
        canvas.update_interaction(point(456.0, 0.0));

        assert_eq!(
            geometry_of(&canvas, id).position.x,
            454.0,
            "the magnet moved the object onto the line"
        );
        assert_eq!(
            canvas.snap_guides().len(),
            1,
            "and left the guide that explains why"
        );

        canvas.finish_interaction(point(456.0, 0.0));

        assert!(
            canvas.snap_guides().is_empty(),
            "a committed gesture does not leave its guides on the canvas"
        );
        assert_eq!(
            geometry_of(&canvas, id).position.x,
            454.0,
            "what history records is what the user saw, not what they aimed at"
        );

        assert_eq!(canvas.session.history.undo_len(), 1);
        assert!(canvas.undo_history());
        assert_eq!(
            geometry_of(&canvas, id),
            before,
            "undo returns to the authored position, not to the unsnapped proposal"
        );
    }

    /// E: move, Escape, nothing changed.
    ///
    /// Escape is the one key that crosses every subsystem at once: the shell's
    /// ladder, the canvas's gesture state, the operation layer's history. It has
    /// to leave the document and the history *both* untouched, and it has to do
    /// so for a gesture that had already moved things on screen.
    #[test]
    fn workflow_e_escape_mid_drag_changes_neither_document_nor_history() {
        let mut canvas = CanvasView::new();
        let ids = select_two_boxes(&mut canvas);
        let before = geometries(&canvas, &[ids.0, ids.1]);

        begin_live_move_with_snap(&mut canvas, &[ids.0, ids.1], true);
        canvas.update_interaction(point(90.0, 70.0));
        assert_ne!(
            geometries(&canvas, &[ids.0, ids.1]),
            before,
            "the drag really did move things before the cancel"
        );
        assert!(
            canvas.snap_guides().is_empty(),
            "snapping was suspended, so there is nothing to explain"
        );

        assert!(
            canvas.cancel_interaction(),
            "a live gesture is something Escape can cancel"
        );

        assert_eq!(
            geometries(&canvas, &[ids.0, ids.1]),
            before,
            "the document is back where it started"
        );
        assert!(
            !canvas.session.history.can_undo(),
            "and nothing was recorded"
        );
        assert!(
            !canvas.session.history.can_redo(),
            "nor was the redo branch disturbed"
        );
        assert!(
            canvas.snap_guides().is_empty(),
            "cancelling drops the guides too"
        );
    }

    /// F: select in Layers, move on the canvas, Layers agrees afterwards.
    ///
    /// The panel holds a projection of the canvas selection rather than a second
    /// copy of it, so "Layers reflects the result" has to be checked in both
    /// directions: the row the panel selected is the object the canvas moved,
    /// and the panel still reports that row as selected afterwards. A panel that
    /// drifted here would show the user editing one object while moving another.
    #[test]
    fn workflow_f_a_layers_selection_is_the_object_the_canvas_moves() {
        let mut canvas = CanvasView::new();
        let mut layers = RetainedLayers::default();
        let ids = two_box_selection(&mut canvas);
        assert!(
            !layers.synchronize(&canvas).is_empty(),
            "the panel has rows to select from"
        );

        // What a row click does: the panel asks the canvas to select.
        canvas.selection.click_flat(Some(ids.1), false);
        layers.synchronize(&canvas);
        assert_eq!(
            layers.selected(),
            &[ids.1],
            "the panel reports the row it just selected"
        );

        let before = geometry_of(&canvas, ids.1);
        let untouched = geometry_of(&canvas, ids.0);
        begin_live_move_with_snap(&mut canvas, &[ids.1], true);
        drag_to(&mut canvas, point(25.0, 15.0));

        assert_ne!(
            geometry_of(&canvas, ids.1),
            before,
            "the object the panel selected is the object that moved"
        );
        assert_eq!(geometry_of(&canvas, ids.0), untouched, "and only that one");

        layers.synchronize(&canvas);
        assert_eq!(
            layers.selected(),
            &[ids.1],
            "the selection survives the move, so the panel still agrees"
        );
        assert!(
            layers
                .presentation_updates()
                .iter()
                .all(|(id, selected)| *id != ids.1 || *selected),
            "a move does not present the selected row as newly selected"
        );

        // And undo, driven from the keyboard, is seen by the panel too.
        assert!(canvas.undo_history());
        assert_eq!(geometry_of(&canvas, ids.1), before);
        layers.synchronize(&canvas);
        assert_eq!(layers.selected(), &[ids.1]);
    }

    /// G: duplicate, move, undo.
    ///
    /// Duplicating hands the selection over to the copies, so the move that
    /// follows acts on objects that did not exist when the history entry for the
    /// duplication was recorded. Two entries, and undo must take them in the
    /// reverse order that leaves a valid document at every step.
    #[test]
    fn workflow_g_duplicate_then_move_undoes_the_move_first() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        canvas.selection.click_flat(Some(id), false);
        let original = geometry_of(&canvas, id);

        assert!(canvas.duplicate_selected_objects());
        let duplicates: Vec<ObjectId> = canvas.selection.ids().to_vec();
        assert_eq!(duplicates.len(), 1, "one copy, and it is selected");
        assert!(
            !duplicates.contains(&id),
            "the copy has its own identity, so undo can name it"
        );
        let copied = geometry_of(&canvas, duplicates[0]);

        begin_live_move_with_snap(&mut canvas, &duplicates, true);
        drag_to(&mut canvas, point(60.0, 0.0));
        let moved_copy = geometry_of(&canvas, duplicates[0]);
        assert_ne!(moved_copy, copied);

        assert_eq!(
            canvas.session.history.undo_len(),
            2,
            "the duplication and the move are separate steps"
        );

        assert!(canvas.undo_history());
        assert_eq!(
            geometry_of(&canvas, duplicates[0]),
            copied,
            "undo takes back the move"
        );
        assert_eq!(
            geometry_of(&canvas, id),
            original,
            "and leaves the object that was duplicated where it was"
        );

        assert!(canvas.undo_history());
        assert!(
            canvas.session.runtime.object(duplicates[0]).is_none(),
            "the second undo removes the copy rather than leaving an orphan"
        );
        assert_eq!(geometry_of(&canvas, id), original);
    }

    /// H: rename, undo, redo.
    ///
    /// Rename is the one edit that goes through the document's structural
    /// operation rather than the runtime's geometry commands, so it is the only
    /// one where undo has to restore something the runtime never held.
    #[test]
    fn workflow_h_rename_undo_redo() {
        // A rename goes through the document's structural operation, so it needs
        // a document that has structure to rename. `CanvasView::new()` is an
        // empty `lamine` with runtime objects over it, which is exactly the state
        // a rename must refuse — so this walks a canvas that was loaded.
        let mut canvas = nested_canvas();
        let id = ObjectId::LANDING;
        let node = canvas.session.runtime.object(id).unwrap().spool_id.clone();
        let original = canvas
            .session
            .document
            .structure
            .nodes
            .iter()
            .find(|n| n.id == node)
            .unwrap()
            .name
            .clone();

        assert!(
            canvas.rename_object(id, "Hero".into()),
            "the rename is accepted"
        );
        assert_eq!(
            canvas
                .session
                .document
                .structure
                .nodes
                .iter()
                .find(|n| n.id == node)
                .unwrap()
                .name,
            "Hero"
        );
        assert_eq!(
            canvas.session.runtime.object(id).unwrap().name,
            "Hero",
            "and the runtime mirrors the document's name"
        );

        assert!(canvas.undo_history());
        assert_eq!(
            canvas
                .session
                .document
                .structure
                .nodes
                .iter()
                .find(|n| n.id == node)
                .unwrap()
                .name,
            original,
            "undo restored the authored name"
        );
        assert_eq!(
            canvas.session.runtime.object(id).unwrap().name,
            original,
            "in the runtime too, or the panel would show a stale name"
        );

        assert!(canvas.redo_history());
        assert_eq!(
            canvas
                .session
                .document
                .structure
                .nodes
                .iter()
                .find(|n| n.id == node)
                .unwrap()
                .name,
            "Hero"
        );

        // Renaming to the name it already has is not an edit, so it must not
        // become a step the user has to undo twice.
        let depth = canvas.session.history.undo_len();
        assert!(!canvas.rename_object(id, "Hero".into()));
        assert_eq!(
            canvas.session.history.undo_len(),
            depth,
            "a no-op rename records nothing"
        );
    }

    /// I: zoom, snap, move, undo.
    ///
    /// Navigation and editing share one canvas and one undo stack, so the claim
    /// is that the camera is not a document edit: zooming must not consume an
    /// undo step, must survive the undo of the move that followed it, and must
    /// not change *how far* a snap reaches — the threshold is a distance on
    /// screen, so the same gesture has to snap at 50% and at 200%.
    #[test]
    fn workflow_i_zoom_then_snap_then_move_undo_leaves_the_camera_alone() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        let resting = geometry_of(&canvas, id);
        let zoom_resting = canvas.camera.zoom;

        // Two zooms, in opposite directions. The anchor is the selection's own
        // centre, which is what the wheel and the `+`/`-` keys both use.
        canvas.selection.click_flat(Some(id), false);
        canvas.zoom_in();
        let zoomed_in = canvas.camera.zoom;
        assert_ne!(zoomed_in, zoom_resting, "the camera really moved");
        assert_eq!(
            canvas.session.history.undo_len(),
            0,
            "the camera is not a document edit, so it costs no undo step"
        );

        canvas.zoom_out();
        assert_eq!(canvas.camera.zoom, zoom_resting, "and the view came back");
        assert_eq!(canvas.session.history.undo_len(), 0);

        // The snap threshold is a distance *on screen*, so the world-space window it
        // covers has to grow as the view pulls back. Landing's left edge aimed at
        // world 474 sits 20 world px from Editor's left edge at 454: at 100% that
        // is 20 screen px and outside an 8px threshold, and at 25% it is 5 screen
        // px and inside one. Same gesture, same target, different zoom, different
        // answer — which is the only way to tell a screen-space threshold from a
        // world-space one. `update_interaction` speaks screen coordinates, so
        // the aim point has to be projected rather than reused.
        canvas.camera.set_zoom_at_center(1.0);
        begin_live_move(&mut canvas, &[id]);
        canvas.update_interaction(canvas.camera.world_to_screen(point(474.0, 0.0)));
        assert_eq!(
            geometry_of(&canvas, id).position.x,
            474.0,
            "at 100% a 20px error is well outside an 8px threshold, so nothing snaps"
        );
        canvas.cancel_interaction();

        canvas.camera.set_zoom_at_center(0.25);
        begin_live_move(&mut canvas, &[id]);
        canvas.update_interaction(canvas.camera.world_to_screen(point(474.0, 0.0)));
        assert_eq!(
            geometry_of(&canvas, id).position.x,
            454.0,
            "at 25% the same 20px error is 5 screen px, inside the same 8px threshold"
        );
        assert!(
            canvas.snap_guides().iter().any(|guide| guide.at == 454.0),
            "and a guide on Editor's edge is there to explain it, whatever else \
             the same drag happened to line up"
        );
        canvas.cancel_interaction();
        assert_eq!(geometry_of(&canvas, id), resting);

        // And the committed move undoes without disturbing the zoom.
        let zoom_before_move = canvas.camera.zoom;
        let aim = canvas.camera.world_to_screen(point(456.0, 0.0));
        begin_live_move(&mut canvas, &[id]);
        drag_to(&mut canvas, aim);
        let committed = geometry_of(&canvas, id);
        assert_ne!(committed, resting);
        assert_eq!(canvas.session.history.undo_len(), 1);

        assert!(canvas.undo_history());
        assert_eq!(geometry_of(&canvas, id), resting);
        assert_eq!(
            canvas.camera.zoom, zoom_before_move,
            "undo restores the document, not the view"
        );
    }

    /// J: save, reopen, and the edited document is what comes back.
    ///
    /// The loop's last step, and the one that crosses the boundary the whole
    /// editor is built around: what the gesture layer produced has to survive a
    /// round trip through authored HTML/CSS, and come back as the same editable
    /// objects rather than as pixels that merely look right.
    #[test]
    fn workflow_j_save_reopen_reproduces_the_edited_document() {
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let ids: Vec<ObjectId> = view.document_objects().iter().map(|o| o.id).collect();
        let resting: Vec<ObjectGeometry> = ids
            .iter()
            .map(|id| view.session.runtime.geometry(*id).unwrap())
            .collect();
        let cta = object_with_node(&view, "spool-cta-primary");

        // Move one object, then resize another, so the round trip has to carry
        // both a position and a size back out of authored CSS.
        let mover = ids[0];
        view.selection.replace(vec![mover]);
        begin_live_move_with_snap(&mut view, &[mover], true);
        drag_to(&mut view, point(70.0, 40.0));
        assert_ne!(
            view.session.runtime.geometry(mover).unwrap().position,
            resting[0].position,
            "the move really happened before the save"
        );

        let cta_id = view
            .document_objects()
            .iter()
            .find(|o| o.spool_id.as_str() == "spool-cta-primary")
            .unwrap()
            .id;
        view.selection.replace(vec![cta_id]);
        begin_live_resize(
            &mut view,
            cta_id,
            ResizeHandle::BottomRight,
            point(0.0, 0.0),
            true,
        );
        view.update_interaction_with(point(24.0, 12.0), false, false);
        view.finish_interaction(point(24.0, 12.0));

        let moved_to = view.session.runtime.geometry(mover).unwrap().position;
        let resized_to = view.session.runtime.geometry(cta_id).unwrap();
        assert_ne!(resized_to.size, cta.size);

        let outcome = view.save_project().expect("save succeeds");
        assert!(
            outcome.unsupported.is_empty(),
            "a move and a resize of authored objects are both writable: {:?}",
            outcome.unsupported
        );

        let reopened = project_view(&root);
        assert_eq!(
            reopened.session.runtime.geometry(mover).unwrap().position,
            moved_to,
            "the move came back from the authored source"
        );
        assert_eq!(
            reopened.session.runtime.geometry(cta_id).unwrap().size,
            resized_to.size,
            "and so did the resize"
        );
        assert_eq!(
            reopened
                .session
                .runtime
                .object(cta_id)
                .unwrap()
                .text_content,
            cta.text_content,
            "authored content the editor never touched is byte-for-byte intact"
        );

        // The reopened document is still editable, which is the part a rendered
        // screenshot could not tell us.
        let mut reopened = reopened;
        reopened.selection.replace(vec![mover]);
        let before = reopened.session.runtime.geometry(mover).unwrap();
        begin_live_move_with_snap(&mut reopened, &[mover], true);
        drag_to(&mut reopened, point(10.0, 0.0));
        assert_ne!(
            reopened.session.runtime.geometry(mover).unwrap(),
            before,
            "the reopened document accepts another gesture"
        );
        assert!(reopened.undo_history());
        assert_eq!(
            reopened.session.runtime.geometry(mover).unwrap(),
            before,
            "and that gesture is undoable like any other"
        );
    }

    // ---------------------------------------------------------------------
    // The seams: a keyboard command arriving while the pointer is busy
    //
    // The pointer and the keyboard are two readers of one editor, and until now
    // every test above drove one or the other and never both. These three walk
    // the steps where they collide, because that is where the systems that are
    // each individually correct stop agreeing with each other.
    // ---------------------------------------------------------------------

    /// An `⌥`-drag that is cancelled has to take its copies with it, and the
    /// selection has to notice.
    ///
    /// The gesture handed the selection over to the copies when the drag went
    /// live, and the cancel removes those copies from the document. Nothing
    /// reconciles the two, so without this the panel is left holding ids that no
    /// longer name anything: invisible, but still counted as "a selection" by
    /// the Escape ladder, so the next Escape offers to clear a selection the
    /// user cannot see.
    #[test]
    fn workflow_k_cancelling_a_duplicate_drag_leaves_no_dead_selection() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        canvas.selection.click_flat(Some(id), false);
        let original = geometry_of(&canvas, id);

        begin_live_move_with_modifiers(&mut canvas, &[id], true, true);
        canvas.update_interaction(point(60.0, 40.0));
        let copies: Vec<ObjectId> = canvas.selection.ids().to_vec();
        assert_eq!(copies.len(), 1, "the drag handed the selection to the copy");
        assert_ne!(copies[0], id);

        assert!(canvas.cancel_interaction());

        assert!(
            canvas.session.runtime.object(copies[0]).is_none(),
            "the cancel took the copy out of the document"
        );
        assert_eq!(geometry_of(&canvas, id), original, "and left the original");
        assert!(
            canvas.selection.is_empty(),
            "the selection must not go on naming objects the cancel deleted"
        );
        assert!(
            !canvas.session.history.can_undo(),
            "and a cancelled gesture is still not an edit"
        );

        // The panel agrees, rather than reporting a selection that is not there.
        let mut layers = RetainedLayers::default();
        layers.synchronize(&canvas);
        assert!(layers.selected().is_empty());
    }

    /// Undo arriving mid-drag has to take back the drag.
    ///
    /// The competing readings are both wrong in different ways: committing the
    /// drag and *then* undoing makes one keystroke do two visible things, and
    /// discarding the drag without recording it throws away work the user can
    /// neither see nor undo. Undoing the drag is the only answer in which the
    /// keystroke means one thing — the last thing the user did.
    #[test]
    fn workflow_l_undo_mid_drag_reverts_the_drag_rather_than_dropping_it() {
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;

        // One committed edit, so there is something older for undo to reach if
        // it looks in the wrong place.
        begin_live_move(&mut canvas, &[id]);
        drag_to(&mut canvas, point(10.0, 0.0));
        let first = geometry_of(&canvas, id);
        assert_eq!(canvas.session.history.undo_len(), 1);

        // A second drag, still in flight when the undo key arrives.
        begin_live_move(&mut canvas, &[id]);
        canvas.update_interaction(point(80.0, 0.0));
        assert_ne!(geometry_of(&canvas, id), first);

        assert!(canvas.undo_history());
        assert_eq!(
            geometry_of(&canvas, id),
            first,
            "undo must revert the drag in flight, not discard it and revert the \
             earlier move instead"
        );
        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "the drag became an entry and was then undone, so the stack is the \
             same depth it was before the key"
        );
        assert!(
            !canvas.interaction.is_active(),
            "and the gesture is no longer holding the pointer"
        );
    }

    /// Moving one child of a frame, on its own, has to round-trip through source.
    ///
    /// A child's box is authored relative to the element that contains it, so a
    /// child moved by itself is the case where writing world coordinates as
    /// `left` would quietly displace it by the frame's own offset. The frame's
    /// bytes are the other half of the claim: nothing the editor did to one
    /// child may rewrite its siblings or its parent.
    #[test]
    fn workflow_m_moving_one_child_leaves_the_rest_of_the_authored_source_alone() {
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let html_before = std::fs::read_to_string(root.join("index.html")).unwrap();
        let css_before = std::fs::read_to_string(root.join("styles.css")).unwrap();

        let frame_before = view
            .session
            .runtime
            .geometry(id_of(&view, "spool-frame-root"))
            .unwrap();
        let sibling_before = view
            .session
            .runtime
            .geometry(id_of(&view, "spool-cta-primary"))
            .unwrap();
        let child = id_of(&view, "spool-text-headline");
        let child_before = view.session.runtime.geometry(child).unwrap();

        // Only the child is selected: a frame and its child can never both be in
        // a selection, so this is the only way a user can move a child alone.
        view.selection.replace(vec![child]);
        begin_live_move_with_snap(&mut view, &[child], true);
        drag_to(&mut view, point(30.0, 18.0));
        let moved = view.session.runtime.geometry(child).unwrap();
        assert_ne!(moved, child_before, "the child really moved");

        let outcome = view.save_project().expect("save succeeds");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);

        // The frame's own box is authored in flow, so what must not change is
        // that the save did not rewrite the siblings to compensate.
        assert_eq!(
            std::fs::read_to_string(root.join("styles.css")).unwrap(),
            css_before,
            "the stylesheet had no reason to be rewritten"
        );

        let reopened = project_view(&root);
        assert_eq!(
            reopened.session.runtime.geometry(child).unwrap(),
            moved,
            "the child's own edit survived the round trip"
        );
        assert_eq!(
            reopened
                .session
                .runtime
                .geometry(id_of(&reopened, "spool-cta-primary"))
                .unwrap(),
            sibling_before,
            "and its untouched sibling is exactly where it was"
        );
        let frame_after = reopened
            .session
            .runtime
            .geometry(id_of(&reopened, "spool-frame-root"))
            .unwrap();
        assert_eq!(
            frame_after.size, frame_before.size,
            "the containing frame did not resize because a child moved"
        );

        // The edit landed in the child's own rule, so the bytes outside that one
        // declaration are still the ones that were loaded.
        let html_after = std::fs::read_to_string(root.join("index.html")).unwrap();
        assert_ne!(
            html_after, html_before,
            "the child's authored position was written back"
        );
        assert!(
            html_after.contains("spool-cta-primary"),
            "the sibling's markup is still there"
        );
        assert!(html_after.contains("spool-text-headline"));
    }

    fn id_of(view: &CanvasView, node: &str) -> ObjectId {
        view.document_objects()
            .iter()
            .find(|object| object.spool_id.as_str() == node)
            .unwrap_or_else(|| panic!("{node} is projected"))
            .id
    }

    /// The camera belongs to navigation, so it must not move under a gesture.
    ///
    /// Two halves. The first is the behaviour: with a drag in flight the camera
    /// is the pointer's to use, not the wheel's. The second is the arithmetic
    /// that makes the rule necessary, pinned so the guard is not "simplified"
    /// away by someone who decides to compensate inside the gesture instead.
    #[test]
    fn workflow_n_a_gesture_in_flight_owns_the_camera() {
        let mut canvas = CanvasView::new();
        assert!(
            !canvas.pointer_gesture_active(),
            "an idle canvas gives the wheel the camera"
        );

        // Every kind of pointer gesture takes the camera away.
        let id = ObjectId::LANDING;
        canvas.selection.click_flat(Some(id), false);
        begin_live_move(&mut canvas, &[id]);
        assert!(canvas.pointer_gesture_active(), "a move owns it");
        canvas.cancel_interaction();

        begin_live_resize(
            &mut canvas,
            id,
            ResizeHandle::BottomRight,
            point(0.0, 0.0),
            true,
        );
        assert!(canvas.pointer_gesture_active(), "a resize owns it");
        canvas.cancel_interaction();

        canvas.marquee = Some(MarqueeGesture {
            start: point(0.0, 0.0),
            current: point(10.0, 10.0),
            additive: false,
            initial_selection: Vec::new(),
        });
        assert!(canvas.pointer_gesture_active(), "a marquee owns it");
        canvas.marquee = None;

        canvas.pan = Some(PanGesture {
            button: MouseButton::Middle,
            pointer_start: point(0.0, 0.0),
            offset_start: point(0.0, 0.0),
        });
        assert!(canvas.pointer_gesture_active(), "a pan owns it");
        canvas.pan = None;

        assert!(!canvas.pointer_gesture_active());

        // The Escape ladder asks a narrower question on purpose: a drag inside a
        // text buffer should leave the text session, not be reported as a
        // gesture that nothing owns.
        canvas.text_edit = Some(TextEditState {
            id,
            original_text: "hello".into(),
            editing_text: "hello".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            pointer_anchor: Some(0),
        });
        assert!(
            canvas.pointer_gesture_active(),
            "a text-buffer drag still owns the camera"
        );
        assert!(
            !canvas.is_manipulating(),
            "but it is not a gesture as far as the Escape ladder is concerned"
        );
    }

    #[test]
    fn the_keyboard_camera_commands_are_frozen_by_a_live_gesture_too() {
        // The wheel and the pinch are gated at their handlers; every other way
        // the camera can move has to answer the same question, or `+` mid-drag
        // rewrites the distance already dragged exactly as a pinch does.
        let mut canvas = CanvasView::new();
        canvas.camera.resize(size(800.0, 600.0));
        let id = ObjectId::LANDING;
        canvas.selection.click_flat(Some(id), false);

        // With nothing in flight every one of them works.
        assert!(canvas.zoom_in(), "zoom in works when the pointer is free");
        assert!(canvas.zoom_out(), "zoom out works when the pointer is free");
        assert!(
            canvas.set_zoom_percent(100),
            "an explicit percentage works when the pointer is free"
        );
        assert!(
            canvas.fit_canvas(),
            "zoom to fit works when the pointer is free"
        );
        assert!(
            canvas.zoom_to_selection(),
            "zoom to selection works when the pointer is free"
        );
        assert!(
            canvas.zoom_to_actual_size(),
            "zoom to actual size works when the pointer is free"
        );
        assert!(
            canvas.pan_by(point(30.0, 0.0)),
            "and so does a keyboard pan"
        );

        // Now hold a drag and try all of them again.
        begin_live_move(&mut canvas, &[id]);
        canvas.update_interaction(point(40.0, 0.0));
        let zoom = canvas.camera.zoom;
        let offset = canvas.camera.offset;
        let live = geometry_of(&canvas, id);

        assert!(!canvas.zoom_in(), "zoom in is refused");
        assert!(!canvas.zoom_out(), "zoom out is refused");
        assert!(
            !canvas.set_zoom_percent(200),
            "an explicit percentage is refused"
        );
        assert!(!canvas.fit_canvas(), "zoom to fit is refused");
        assert!(!canvas.zoom_to_selection(), "zoom to selection is refused");
        assert!(
            !canvas.zoom_to_actual_size(),
            "zoom to actual size is refused"
        );
        assert!(
            !canvas.pan_by(point(30.0, 0.0)),
            "a keyboard pan is refused"
        );

        assert_eq!(
            (canvas.camera.zoom, canvas.camera.offset),
            (zoom, offset),
            "so the coordinate frame the gesture measures against never moved"
        );
        assert_eq!(
            geometry_of(&canvas, id),
            live,
            "and the object is exactly where the drag had put it"
        );

        // The gesture is still the gesture: finishing it lands one entry, not two.
        canvas.finish_interaction(point(40.0, 0.0));
        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "the refused camera commands left the gesture intact"
        );
    }

    #[test]
    fn a_pan_that_starts_on_top_of_a_live_drag_puts_the_drag_back() {
        // `begin_pan` used to assign `Interaction::None` outright. The drag had
        // already written its geometry to the document, so that left an applied
        // change with no history entry behind it and no snapshots left to restore
        // from — an edit the user could neither see in the undo stack nor get
        // back. The pan now goes through the same abandonment every other
        // interrupting command uses.
        //
        // `begin_pan` needs a mouse event, so what is asserted is the policy it
        // now uses: a live gesture is handed back through the central path.
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        canvas.selection.click_flat(Some(id), false);
        let resting = geometry_of(&canvas, id);

        begin_live_move_with_snap(&mut canvas, &[id], true);
        canvas.update_interaction(point(70.0, 40.0));
        assert_ne!(
            geometry_of(&canvas, id),
            resting,
            "the drag really did move things before the pan began"
        );
        assert!(canvas.interaction.is_active());

        // What `begin_pan` does, in the order it does it. Driven through the same
        // method the mouse path calls, because a test that asserted the policy
        // separately from the caller would keep passing if the caller stopped
        // using it.
        assert!(
            canvas.begin_pan_from(MouseButton::Middle, point(400.0, 300.0)),
            "the middle button starts a pan"
        );
        assert!(canvas.pan.is_some(), "and the pan is live");

        assert_eq!(
            geometry_of(&canvas, id),
            resting,
            "the document is back where the drag started"
        );
        assert!(
            !canvas.session.history.can_undo(),
            "and nothing was recorded for an abandoned drag"
        );
        assert!(!canvas.interaction.is_active());
    }

    #[test]
    fn a_style_edit_that_interrupts_a_duplicate_drag_leaves_no_dead_selection() {
        // The same hole `abandon_interaction` was introduced to close, in the one
        // interrupting command that was still open-coded: `apply_selected_style`
        // restored the gesture — which removes the `⌥`-drag's copies — but never
        // reconciled the selection those copies had been handed. The Inspector's
        // preset chips reach this path, so applying one mid-drag left the panel
        // selecting objects the document no longer had.
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        canvas.selection.click_flat(Some(id), false);
        let original = geometry_of(&canvas, id);

        begin_live_move_with_modifiers(&mut canvas, &[id], true, true);
        canvas.update_interaction(point(60.0, 40.0));
        let copies: Vec<ObjectId> = canvas.selection.ids().to_vec();
        assert_eq!(copies.len(), 1, "the drag handed the selection to the copy");

        // Whether this changes anything depends on what is left selected once the
        // copies are gone, and that is not what this test is about.
        let _ = canvas.apply_selected_style(StyleEdit::Opacity(0.5));

        for copy in &copies {
            assert!(
                canvas.session.runtime.object(*copy).is_none(),
                "the interrupt took the copy out of the document"
            );
        }
        assert_eq!(
            geometry_of(&canvas, id),
            original,
            "and put the original back"
        );
        assert!(
            !canvas
                .selection
                .ids()
                .iter()
                .any(|held| copies.contains(held)),
            "so the selection cannot still be naming it: {:?}",
            canvas.selection.ids()
        );
        for held in canvas.selection.ids() {
            assert!(
                canvas.session.runtime.object(*held).is_some(),
                "every selected id still names a live object: {held:?}"
            );
        }
        assert!(
            !canvas.session.history.can_undo(),
            "and an abandoned drag is still not an edit"
        );
    }

    #[test]
    fn a_style_edit_interrupting_a_plain_drag_restores_it_and_then_applies() {
        // The same command, on a drag that has not duplicated anything, so the
        // selection survives the abandon and the edit has something to act on.
        // Both halves matter: the drag must be given up cleanly, and the command
        // the user actually asked for must still happen.
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        canvas.selection.click_flat(Some(id), false);
        let resting = geometry_of(&canvas, id);

        begin_live_move_with_snap(&mut canvas, &[id], true);
        canvas.update_interaction(point(70.0, 40.0));
        assert_ne!(geometry_of(&canvas, id), resting);

        assert!(
            canvas.apply_selected_style(StyleEdit::Opacity(0.5)),
            "the style edit applies to what is still selected"
        );
        assert_eq!(
            geometry_of(&canvas, id),
            resting,
            "and the interrupted drag was put back rather than left applied"
        );
        assert_eq!(
            canvas.session.history.undo_len(),
            1,
            "one entry: the style edit. The abandoned drag recorded nothing."
        );
        assert!(canvas.undo_history());
        assert_eq!(
            canvas
                .session
                .runtime
                .appearance(id)
                .map(|a| a.style.opacity),
            Some(1.0),
            "and undo reverts the style edit, not a drag the user never completed"
        );
    }

    #[test]
    fn opening_a_project_frames_the_projects_own_objects() {
        // `load_project` called `Camera::fit`, which frames a fixed placeholder
        // box. That is right for the empty starter scene and wrong for every
        // other project: a small one opened off to one side and a large one
        // overflowed the window, because the camera was framing a box the
        // document had no relationship to.
        //
        // The claim is that the viewport ends up containing the objects the
        // project actually has — checked against their real bounds rather than
        // against the placeholder constants.
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        // The camera cannot frame anything until it knows how big the window is,
        // and `resize` applies the fit the load asked for.
        assert!(view.camera.resize(size(800.0, 600.0)));

        let bounds = WorldRect::around(view.session.runtime.objects()).expect("objects");
        let width = bounds.width();
        let height = bounds.height();
        assert!(
            width > 0.0 && height > 0.0,
            "the fixture is a real document, not an empty one"
        );

        let viewport = view.camera.viewport;
        let visible = |world: gpui::Point<f32>| view.camera.world_to_screen(world);
        for corner in [
            bounds.min,
            point(bounds.max.x, bounds.min.y),
            point(bounds.min.x, bounds.max.y),
            bounds.max,
        ] {
            let screen = visible(corner);
            assert!(
                screen.x >= -1.0 && screen.x <= viewport.width + 1.0,
                "the project's own corner is inside the viewport: {screen:?}"
            );
            assert!(
                screen.y >= -1.0 && screen.y <= viewport.height + 1.0,
                "and inside it vertically too: {screen:?}"
            );
        }

        // And the fit is a real fit rather than the placeholder box: a camera
        // framing `WORLD_BOUNDS` would put a project this size somewhere else
        // entirely, so pinning the zoom to the bounds-derived value is what makes
        // the test fail if the old call comes back.
        let margin = 48.0_f32;
        let expected = ((viewport.width - margin * 2.0) / width)
            .min((viewport.height - margin * 2.0) / height)
            .clamp(MIN_ZOOM, MAX_ZOOM);
        assert!(
            (view.camera.zoom - expected).abs() < 0.001,
            "the zoom is the one the project's bounds imply: {} vs {expected}",
            view.camera.zoom
        );
    }

    #[test]
    fn workflow_o_a_zoom_mid_drag_rewrites_the_distance_dragged() {
        // Why `pointer_gesture_active` gates the wheel and the pinch, stated as
        // arithmetic so it cannot be forgotten.
        //
        // A gesture stores where the pointer was in *world* units when the
        // button went down, and re-projects the pointer through the *current*
        // camera on every move. Change the camera between those two and the
        // delta is no longer the distance the pointer travelled.
        let mut canvas = CanvasView::new();
        let id = ObjectId::LANDING;
        canvas.camera.resize(size(800.0, 600.0));
        let resting = geometry_of(&canvas, id);

        // `begin_live_move` presses at world (0,0) and screen (0,0), which at the
        // default camera are the same point.
        begin_live_move(&mut canvas, &[id]);
        let target = canvas.camera.world_to_screen(point(150.0, 100.0));
        canvas.update_interaction(target);
        let dragged = geometry_of(&canvas, id);
        assert_eq!(
            dragged.position.x - resting.position.x,
            150.0,
            "screen 150 is world 150 at 100%, and the press was world 0"
        );

        // Now the pinch that must not reach the camera. The pointer has not
        // moved on screen, so the object must not move in the world.
        //
        // Pinching to 200% about screen (100,100) pins world (100,100) there,
        // which moves the origin out to screen (100,100)/2 = world 50. So the
        // pointer's screen 150 now reads as world 125, and a drag that had gone
        // 150 now reads as 125 — the object slides back toward where the drag
        // began, with no error and no way for the user to tell why.
        canvas
            .camera
            .zoom_at(2.0, canvas.camera.world_to_screen(point(100.0, 100.0)));
        canvas.update_interaction(target);
        let after_zoom = geometry_of(&canvas, id);
        assert_eq!(
            after_zoom.position.x - resting.position.x,
            125.0,
            "the same pointer position now reads as a shorter drag"
        );

        // Which is why the camera is frozen rather than the gesture compensated:
        // compensating would make the committed result depend on the zoom the
        // user happened to pinch at, which is not a quantity they chose on
        // purpose.
        canvas.cancel_interaction();
    }

    /// Undo has to survive a save, or it is not an undo.
    ///
    /// Save answers "what does the document say now?" by comparing the live
    /// objects against what the source currently says. That baseline has to
    /// advance when the source is written: pinned to the state the project was
    /// *opened* in, it says "nothing changed" for any document the user has
    /// already saved once, and writes nothing — leaving the file describing an
    /// edit that has since been undone.
    #[test]
    fn workflow_p_undo_then_save_puts_the_authored_bytes_back() {
        let root = project_scratch("landing");
        let mut view = project_view(&root);
        let cta = id_of(&view, "spool-cta-primary");
        let authored = view.session.runtime.geometry(cta).unwrap();
        let html_authored = std::fs::read_to_string(root.join("index.html")).unwrap();
        let css_authored = std::fs::read_to_string(root.join("styles.css")).unwrap();

        // Move it and save: the edit reaches the authored source.
        view.selection.replace(vec![cta]);
        begin_live_move_with_snap(&mut view, &[cta], true);
        drag_to(&mut view, point(0.0, 40.0));
        view.save_project().expect("first save succeeds");
        assert_ne!(
            std::fs::read_to_string(root.join("index.html")).unwrap(),
            html_authored,
            "the move was written into the source"
        );

        // Undo, and the editor is back to the authored geometry.
        assert!(view.undo_history());
        assert_eq!(
            view.session.runtime.geometry(cta).unwrap(),
            authored,
            "undo restored the object in the editor"
        );

        // Saving again has to take the source with it.
        let outcome = view.save_project().expect("second save succeeds");
        assert!(outcome.unsupported.is_empty(), "{:?}", outcome.unsupported);
        // Not a byte comparison with the authored file: the editor cannot know
        // the author "meant" no offset, only that the object is where it started.
        // The honest form of that is a zero offset written against what is on
        // disk, so what has to hold is that a reopen agrees with the editor.
        let reopened = project_view(&root);
        assert_eq!(
            reopened.session.runtime.geometry(cta).unwrap(),
            authored,
            "the undo reached the authored source, not just the editor"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("styles.css")).unwrap(),
            css_authored,
            "and the stylesheet is untouched, since the move was an inline offset"
        );

        // And the save is now a fixed point: a third save has nothing left to
        // say, so the editor and its source have stopped disagreeing.
        let settled = std::fs::read_to_string(root.join("index.html")).unwrap();
        let outcome = view.save_project().expect("third save succeeds");
        assert!(
            outcome.written.is_empty(),
            "saving an unchanged document writes nothing: {:?}",
            outcome.written
        );
        assert_eq!(
            std::fs::read_to_string(root.join("index.html")).unwrap(),
            settled,
            "so the bytes stop moving"
        );
    }
}
