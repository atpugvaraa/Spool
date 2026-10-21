//! The vector edit surface: the points of the vector being edited — where they
//! are, how big their box is, the radius of their corners, how their two
//! handles follow each other, and what the stroke does where it ends and turns
//! at them — and the Variable width tool's width point.
//!
//! # What this surface is
//!
//! figcraft's `crates/ui-egui/src/vector_panel.rs` is the Design panel *while a
//! vector is being edited*. It takes the layer's own "Position and size"
//! section's place and shows the **points** instead, because in vector edit
//! mode the points are what is being edited: the layer's box is derived from
//! them, and a box derived from a path is not the fact the user is changing.
//!
//! | figcraft's row | here |
//! |---|---|
//! | the **Point** / **Points** header | [`VectorModel::title`] |
//! | **Position** — X, Y | the Position row |
//! | **Dimensions** — W, H | the Dimensions row |
//! | **Corner radius** | the Corner radius row |
//! | **Mirroring** | the Mirroring row |
//! | **End point** / **Join** | the End point and Join rows |
//! | **Width point** (the Variable width tool) | the Width point rows |
//!
//! The labels, their order, the empty states and the "Mixed" fold are
//! figcraft's, word for word.
//!
//! # The write path, and why it is not offered
//!
//! The engine can do all of it. `vector.edit.position`, `vector.edit.resize`,
//! `vector.edit.cornerRadius`, `vector.edit.mirroring`,
//! `vector.edit.strokeCap`, `vector.edit.strokeJoin` and
//! `vector.edit.widthPoint` are the engine's own commands, and [`VectorAction`]
//! is this port's spelling of them — the exact ids, the exact parameters and
//! the exact value spellings, so that a change made from here, from the CLI
//! and from an agent would be the same change and would land in the same
//! history.
//!
//! **Spool's source cannot carry any of it yet.** Spool is source-backed: HTML
//! and CSS are the authored source and `lamine.yaml` holds the structure. A
//! vector path has no authored spelling in either:
//!
//! - `spool-source`'s reader does not compile a path at all — there is no
//!   HTML or CSS it can turn back into a network;
//! - the save path refuses a vector node outright. `source_edits::plan_drawn`
//!   asks `kind_for` for the node's kind and a vector has none, so the node is
//!   left out of the project and reported as "a vector has no authored
//!   spelling this editor can write yet" — which is what the shell shows as
//!   "not saved: engine-6".
//!
//! So the facts on this surface — where a point is, how round its corner is,
//! how its handles follow each other, what the stroke does at it — have no
//! owner in Spool's document model, and editing them would be editing a value
//! the save path cannot write back, which is the one thing this architecture
//! forbids. The panel therefore **shows** them and says that they cannot be
//! changed here. The engine's commands are still here, and pinned by tests, so
//! that the day the source format grows a spelling for a path the surface
//! becomes editable by changing one verdict.
//!
//! The width point is the interesting one: a *width profile* is an Owner B
//! fact — `lamine.yaml` carries it, and `docs/02-document-and-source-model.md`
//! says so — but the layer it belongs to is a vector, and the save path
//! refuses the whole layer, so the profile goes with it.
//!
//! # Commands, not mutations
//!
//! Every change leaves through the engine's command surface, as a command id
//! and its parameters. Nothing here writes to a [`Document`] directly, so an
//! edit made from this panel, from the CLI and from an agent are the same edit
//! and land in the same history.

// The Inspector mounts this surface in a later pass (docs/06-ui-parity.md §4:
// "Do not edit `app/src/inspector.rs`" — a slice reports the snippet it needs
// and the Inspector is wired afterwards, in one pass). Nothing in this module
// is reachable from `main` until then, so every item in it reads as dead. The
// allow is scoped to this module and comes off when the Inspector wires it in.
#![allow(dead_code)]

use figcraft_doc::{Document, HandleMirroring, NodeId, NodeKind, StrokeCap, StrokeJoin};
use figcraft_engine::vectoredit::{self, VTool, VectorEdit};
use figcraft_geom::Point;
use gpui::{div, prelude::*, px, rgb, AnyElement, Div, SharedString};
use serde_json::{json, Map, Value};

use crate::theme;

// ═══════════════════════════════════════════════════════════════════════════
// The words
// ═══════════════════════════════════════════════════════════════════════════

/// The word for a property the selected points disagree about.
///
/// figcraft's word, and the Inspector's: a value and a nothing would leave a
/// panel free to invent a third thing at every row that faced a disagreement.
pub const MIXED: &str = "Mixed";

/// The section's title when one point is selected.
pub const TITLE_POINT: &str = "Point";

/// The section's title when more than one point is selected.
pub const TITLE_POINTS: &str = "Points";

/// The section's title under the Variable width tool.
pub const TITLE_WIDTH_POINT: &str = "Width point";

/// What the panel says when the points section has no points selected.
pub const SELECT_POINTS: &str = "Select points to edit their position, radius and mirroring";

/// What the panel says when the Variable width tool has no width point.
pub const DRAG_THE_STROKE: &str = "Drag the stroke to add a width point";

/// The cap and join a point inherits from the layer.
pub const SAME_AS_LAYER: &str = "Same as layer";

/// The label of a point's handle mirroring: none.
pub const MIRRORING_NONE: &str = "No mirroring";

/// The label of a point's handle mirroring: the other handle keeps its angle.
pub const MIRRORING_ANGLE: &str = "Mirror angle";

/// The label of a point's handle mirroring: the other handle mirrors angle and
/// length.
pub const MIRRORING_ANGLE_AND_LENGTH: &str = "Mirror angle and length";

/// The note under the rows: why nothing here can be changed, and why.
///
/// The honest half of the surface. The rows above are what the engine holds;
/// this says where they come from and what the source cannot carry.
pub const SOURCE_NOTE: &str = "\
Spool's source has no spelling for a vector path, so nothing on this panel can be \
saved. The points, their radius, their mirroring and the stroke at them live in the \
editor, and the save path reports a vector it cannot author as not saved. A vector \
needs an authored spelling before these rows can change it.";

/// Why a point's own facts cannot be saved.
pub const POINT_FACTS_NOTE: &str = "\
A vector has no authored spelling in Spool's source, so a point's place on it, its \
radius, its mirroring and the stroke at it cannot be saved.";

/// Why the width point cannot be saved.
pub const WIDTH_FACTS_NOTE: &str = "\
A width profile is a fact lamine.yaml can carry, but a vector has no authored \
element, so the save path refuses the whole layer and the profile with it.";

// ═══════════════════════════════════════════════════════════════════════════
// The model
// ═══════════════════════════════════════════════════════════════════════════

/// One fact this surface shows.
///
/// Named so that the panel can say, per fact, whether the save path can carry
/// it — which is the question that decides whether a row can be changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorFact {
    /// Where the selected points are.
    Position,
    /// How big the selected points' box is.
    Size,
    /// How round a selected point's corner is.
    CornerRadius,
    /// How a selected point's two handles follow each other.
    Mirroring,
    /// The cap where the stroke ends at the selected points.
    Cap,
    /// The join where the stroke turns at the selected points.
    Join,
    /// Where the width point sits along the path.
    WidthPosition,
    /// How wide the stroke is at the width point.
    Width,
    /// How wide one side of the path is at the width point.
    WidthSide,
}

/// What the save path can do with a fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writable {
    /// The fact can be written back to Spool's source.
    Yes,
    /// It cannot, and this says why.
    No(&'static str),
}

impl Writable {
    /// Whether the fact can be changed from this panel.
    pub fn is_writable(self) -> bool {
        matches!(self, Self::Yes)
    }

    /// Why the fact cannot be changed, when it cannot.
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Yes => None,
            Self::No(reason) => Some(reason),
        }
    }
}

/// What Spool's source can do with the layer being edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Persistence {
    /// The source can carry the layer, so its facts can be written back.
    Authored,
    /// It cannot: the layer has no authored spelling, so the save path refuses
    /// the whole node.
    Unauthored,
}

/// The selected points' box, in the vector's own axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VectorBox {
    /// How wide it is.
    pub width: f64,
    /// How high it is.
    pub height: f64,
}

/// The Variable width tool's selected width point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WidthPointModel {
    /// The width point's index in the stroke's profile.
    pub index: usize,
    /// Where it is along the path: 0 at the start, 1 at the end.
    pub t: f64,
    /// How wide the stroke is there, as a multiple of the stroke weight.
    pub scale: f64,
    /// The stroke's weight — the unit the scale is counted in.
    pub weight: f64,
    /// The left side's scale.
    pub left: f64,
    /// The right side's scale.
    pub right: f64,
}

/// What the vector surface shows.
///
/// Every field is the engine's own: the points are the network's, the
/// position and the box are the engine's own geometry, the radius, the
/// mirroring, the cap and the join are the vertices' own. Nothing here is
/// invented, and nothing here is authored.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorModel {
    /// The layer being edited.
    pub id: NodeId,
    /// The tool in edit mode.
    pub tool: VTool,
    /// The selected points, as the network's own indexes.
    pub points: Vec<usize>,
    /// The selected points' top-left, in the parent's coordinates.
    pub position: Option<Point>,
    /// The selected points' box, when there is one worth showing.
    ///
    /// `None` when a single point is selected: one point has no box, so
    /// there is no width or height to show.
    pub size: Option<VectorBox>,
    /// The corner radius every selected point agrees on.
    ///
    /// `None` when they disagree — the panel has one radius row, and a number
    /// there would be a radius the user never chose.
    pub radius: Option<f64>,
    /// The handle mirroring every selected point agrees on.
    pub mirroring: Option<HandleMirroring>,
    /// The cap every selected point agrees on.
    ///
    /// The outer `None` is a disagreement ("Mixed"). The inner `None` is the
    /// layer's own cap: the point has none of its own.
    pub cap: Option<Option<StrokeCap>>,
    /// The join every selected point agrees on.
    ///
    /// The outer `None` is a disagreement ("Mixed"). The inner `None` is the
    /// layer's own join.
    pub join: Option<Option<StrokeJoin>>,
    /// The width point, when the Variable width tool has one selected.
    pub width_point: Option<WidthPointModel>,
    /// What Spool's source can do with the layer being edited.
    pub persistence: Persistence,
}

impl VectorModel {
    /// Read the vector being edited out of the engine's document.
    ///
    /// A layer that is not a vector has no network and so no points: the
    /// panel then has nothing to say, which is what it says.
    pub fn read(doc: &Document, ve: &VectorEdit) -> Self {
        let net = vectoredit::network(doc, ve.id);
        let points = net
            .map(|net| vectoredit::selected_points(net, ve))
            .unwrap_or_default();
        let vertex = |v: &usize| net.and_then(|net| net.vertices.get(*v).copied());

        // figcraft folds the selection per property, so one disagreement hides
        // that row and not the whole block.
        let radius = same(points.iter().map(|v| vertex(v).map(|x| x.radius))).flatten();
        let mirroring = same(points.iter().map(|v| {
            net.map(|net| net.mirroring(*v))
                .unwrap_or(HandleMirroring::None)
        }));
        let cap = same(points.iter().map(|v| vertex(v).map(|x| x.cap))).flatten();
        let join = same(points.iter().map(|v| vertex(v).map(|x| x.join))).flatten();

        Self {
            id: ve.id,
            tool: ve.tool,
            points,
            position: vectoredit::selection_position(doc, ve),
            size: vectoredit::transform::point_box(doc, ve)
                .filter(|b| b.points.len() > 1)
                .map(|b| VectorBox {
                    width: b.width,
                    height: b.height,
                }),
            radius,
            mirroring,
            cap,
            join,
            width_point: width_point_of(doc, ve),
            persistence: match doc.node(ve.id).map(|node| &node.kind) {
                Some(NodeKind::Vector { .. }) => Persistence::Unauthored,
                _ => Persistence::Authored,
            },
        }
    }

    /// The section's title, in figcraft's words.
    ///
    /// "Point" for one point, "Points" for several, and "Width point" under
    /// the Variable width tool.
    pub fn title(&self) -> &'static str {
        match self.tool {
            VTool::Width => TITLE_WIDTH_POINT,
            _ if self.points.len() > 1 => TITLE_POINTS,
            _ => TITLE_POINT,
        }
    }

    /// What the save path can do with one fact.
    ///
    /// The verdict is per fact, and it is what decides whether a row can be
    /// changed. Today every fact on this surface is refused, because the layer
    /// being edited is a vector and the save path cannot author one.
    pub fn writable(&self, fact: VectorFact) -> Writable {
        match self.persistence {
            Persistence::Authored => Writable::Yes,
            Persistence::Unauthored => Writable::No(match fact {
                VectorFact::WidthPosition | VectorFact::Width | VectorFact::WidthSide => {
                    WIDTH_FACTS_NOTE
                }
                _ => POINT_FACTS_NOTE,
            }),
        }
    }
}

/// The Variable width tool's selected width point, read out of the stroke.
fn width_point_of(doc: &Document, ve: &VectorEdit) -> Option<WidthPointModel> {
    let index = ve.width_point?;
    let stroke = doc.node(ve.id)?.stroke.clone();
    let point = *stroke.profile()?.points.get(index)?;
    let (left, right) = point.sides();
    Some(WidthPointModel {
        index,
        t: point.t,
        scale: point.scale,
        weight: stroke.weight,
        left,
        right,
    })
}

/// The value every item shares, if they all share one.
///
/// figcraft's rule, word for word: the first item is the answer only if every
/// other item agrees with it. `None` is therefore a disagreement, and a
/// property nobody set is the same word as a property the selection disagrees
/// about — which is why the panel's own rows fold per property and say
/// "Mixed" rather than showing a number nobody chose.
fn same<T: PartialEq>(mut it: impl Iterator<Item = T>) -> Option<T> {
    let first = it.next()?;
    it.all(|x| x == first).then_some(first)
}

// ═══════════════════════════════════════════════════════════════════════════
// The change
// ═══════════════════════════════════════════════════════════════════════════

/// A change the vector surface can ask for.
///
/// One variant per thing the surface shows, so a change the engine has no
/// command for cannot be written down as though it had one. The engine's
/// spellings, so a change made from here is the change the CLI and an agent
/// would make.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorAction {
    /// Move the selected points so their top-left is at this place.
    Position {
        /// The new X, in the parent's coordinates.
        x: Option<f64>,
        /// The new Y, in the parent's coordinates.
        y: Option<f64>,
    },
    /// Resize the selected points' box.
    Resize {
        /// The new width, in the vector's own axes.
        width: Option<f64>,
        /// The new height, in the vector's own axes.
        height: Option<f64>,
    },
    /// Round the selected points' corners.
    CornerRadius(f64),
    /// How the selected points' handles follow each other.
    Mirroring(HandleMirroring),
    /// The cap where the stroke ends at the selected points.
    ///
    /// `None` is the layer's own cap.
    Cap(Option<StrokeCap>),
    /// The join where the stroke turns at the selected points.
    ///
    /// `None` is the layer's own join.
    Join(Option<StrokeJoin>),
    /// The Variable width tool's width point.
    WidthPoint {
        /// The width point's index in the stroke's profile.
        index: usize,
        /// Where it is along the path: 0 at the start, 1 at the end.
        t: Option<f64>,
        /// How wide it is, in the stroke's own units.
        width: Option<f64>,
        /// Which side of the path the width is for, when it is one side only.
        side: Option<&'static str>,
    },
}

impl VectorAction {
    /// The engine's command id and parameters for this change.
    ///
    /// The engine's own ids and spellings, read from the engine's command
    /// table rather than guessed:
    ///
    /// | change | command | parameters |
    /// |---|---|---|
    /// | position | `vector.edit.position` | `{x}` or `{y}` |
    /// | size | `vector.edit.resize` | `{width}` or `{height}` |
    /// | corner radius | `vector.edit.cornerRadius` | `{radius}` |
    /// | mirroring | `vector.edit.mirroring` | `{mode}` |
    /// | cap | `vector.edit.strokeCap` | `{cap}` |
    /// | join | `vector.edit.strokeJoin` | `{join}` |
    /// | width point | `vector.edit.widthPoint` | `{index, t}` / `{index, width}` / `{index, side, width}` |
    ///
    /// Only the key that changed is sent, so the engine keeps the other half
    /// of the value where it is — which is what figcraft's panel does too.
    pub fn command(&self) -> (String, Value) {
        let mut params = Map::new();
        let id = match self {
            Self::Position { x, y } => {
                if let Some(x) = x {
                    params.insert("x".to_owned(), json!(x));
                }
                if let Some(y) = y {
                    params.insert("y".to_owned(), json!(y));
                }
                "vector.edit.position"
            }
            Self::Resize { width, height } => {
                if let Some(width) = width {
                    params.insert("width".to_owned(), json!(width.max(0.0)));
                }
                if let Some(height) = height {
                    params.insert("height".to_owned(), json!(height.max(0.0)));
                }
                "vector.edit.resize"
            }
            Self::CornerRadius(radius) => {
                params.insert("radius".to_owned(), json!(radius.max(0.0)));
                "vector.edit.cornerRadius"
            }
            Self::Mirroring(mode) => {
                params.insert("mode".to_owned(), json!(mode.id()));
                "vector.edit.mirroring"
            }
            Self::Cap(cap) => {
                // `null` is the layer's own cap; a name is the cap itself.
                params.insert(
                    "cap".to_owned(),
                    match cap {
                        Some(cap) => json!(cap),
                        None => Value::Null,
                    },
                );
                "vector.edit.strokeCap"
            }
            Self::Join(join) => {
                params.insert(
                    "join".to_owned(),
                    match join {
                        Some(join) => json!(join),
                        None => Value::Null,
                    },
                );
                "vector.edit.strokeJoin"
            }
            Self::WidthPoint {
                index,
                t,
                width,
                side,
            } => {
                params.insert("index".to_owned(), json!(index));
                if let Some(t) = t {
                    params.insert("t".to_owned(), json!(t));
                }
                if let Some(width) = width {
                    params.insert("width".to_owned(), json!(width.max(0.0)));
                }
                if let Some(side) = side {
                    params.insert("side".to_owned(), json!(side));
                }
                "vector.edit.widthPoint"
            }
        };
        (id.to_owned(), Value::Object(params))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// The vector surface, as the body of a section.
///
/// Everything it needs is in the model, so it can be put in a window on its
/// own — which is how it is tested, and how it will be mounted: the Inspector
/// gives it the section's header and this is what goes under it.
pub fn section(model: &VectorModel) -> AnyElement {
    let mut body = div().flex().flex_col().gap_2().px(px(14.0)).py(px(12.0));
    body = body.child(match model.tool {
        VTool::Width => width_point_body(model),
        _ => points_body(model),
    });
    body = body.child(note(SOURCE_NOTE, "vector-source-note"));
    body.into_any_element()
}

/// The points: what figcraft shows when the Move tool (or any tool but the
/// Variable width one) is in effect.
fn points_body(model: &VectorModel) -> AnyElement {
    if model.points.is_empty() {
        return note(SELECT_POINTS, "vector-empty");
    }
    let mut body = div().flex().flex_col().gap_2();

    // Position: where the selected points are.
    body = body.child(sublabel("Position", "vector-sublabel-position"));
    let x = model
        .position
        .map(|p| fmt(p.x))
        .unwrap_or_else(|| MIXED.to_owned());
    let y = model
        .position
        .map(|p| fmt(p.y))
        .unwrap_or_else(|| MIXED.to_owned());
    body = body.child(field_pair(
        field("X", &x, "vector-x", dimmed(model, VectorFact::Position)),
        field("Y", &y, "vector-y", dimmed(model, VectorFact::Position)),
    ));

    // Dimensions: the selected points' box, when there are enough points for
    // one to have a size.
    if let Some(size) = model.size {
        body = body.child(sublabel("Dimensions", "vector-sublabel-dimensions"));
        body = body.child(field_pair(
            field(
                "W",
                &fmt(size.width),
                "vector-w",
                dimmed(model, VectorFact::Size),
            ),
            field(
                "H",
                &fmt(size.height),
                "vector-h",
                dimmed(model, VectorFact::Size),
            ),
        ));
    }

    // Corner radius: how round the selected points' corners are.
    body = body.child(sublabel("Corner radius", "vector-sublabel-radius"));
    let radius = radius_word(model.radius);
    body = body.child(field(
        "Corner radius",
        &radius,
        "vector-radius",
        dimmed(model, VectorFact::CornerRadius),
    ));

    // Mirroring: how the selected points' two handles follow each other.
    body = body.child(sublabel("Mirroring", "vector-sublabel-mirroring"));
    let mirroring = model.mirroring.map(mirroring_label).unwrap_or(MIXED);
    body = body.child(field(
        "Mirroring",
        mirroring,
        "vector-mirroring",
        dimmed(model, VectorFact::Mirroring),
    ));

    // The stroke where it ends and turns at the selected points.
    body = body.child(sublabel("End point", "vector-sublabel-end-point"));
    let cap = cap_label(model.cap);
    let join = join_label(model.join);
    body = body.child(field_pair(
        field(
            "End point",
            &cap,
            "vector-cap",
            dimmed(model, VectorFact::Cap),
        ),
        field(
            "Join",
            &join,
            "vector-join",
            dimmed(model, VectorFact::Join),
        ),
    ));

    body.into_any_element()
}

/// The width point: what figcraft shows under the Variable width tool, in
/// place of the points.
fn width_point_body(model: &VectorModel) -> AnyElement {
    let Some(point) = model.width_point else {
        return note(DRAG_THE_STROKE, "vector-no-width-point");
    };
    let mut body = div().flex().flex_col().gap_2();

    // Where it is along the path, and how wide the stroke is there.
    body = body.child(sublabel("Position", "vector-sublabel-width-position"));
    let position = format!("{}%", fmt(point.t * 100.0));
    let width = fmt(point.scale * point.weight);
    body = body.child(field_pair(
        field(
            "Position",
            &position,
            "vector-width-position",
            dimmed(model, VectorFact::WidthPosition),
        ),
        field(
            "Width",
            &width,
            "vector-width",
            dimmed(model, VectorFact::Width),
        ),
    ));

    // Each side's reach from the centre line.
    body = body.child(sublabel("Left side", "vector-sublabel-width-sides"));
    let left = fmt(point.left * point.weight / 2.0);
    let right = fmt(point.right * point.weight / 2.0);
    body = body.child(field_pair(
        field(
            "Left side width",
            &left,
            "vector-width-left",
            dimmed(model, VectorFact::WidthSide),
        ),
        field(
            "Right side width",
            &right,
            "vector-width-right",
            dimmed(model, VectorFact::WidthSide),
        ),
    ));

    body.into_any_element()
}

/// Whether a row is dimmed: the fact cannot be written back.
fn dimmed(model: &VectorModel, fact: VectorFact) -> bool {
    !model.writable(fact).is_writable()
}

/// The word a corner radius row shows: the radius, or "Mixed".
fn radius_word(radius: Option<f64>) -> String {
    radius.map(fmt).unwrap_or_else(|| MIXED.to_owned())
}

/// The label of a handle mirroring mode.
fn mirroring_label(mode: HandleMirroring) -> &'static str {
    match mode {
        HandleMirroring::None => MIRRORING_NONE,
        HandleMirroring::Angle => MIRRORING_ANGLE,
        HandleMirroring::AngleAndLength => MIRRORING_ANGLE_AND_LENGTH,
    }
}

/// The label of a point's cap: the layer's own, a cap, or a disagreement.
fn cap_label(cap: Option<Option<StrokeCap>>) -> String {
    match cap {
        None => MIXED.to_owned(),
        Some(None) => SAME_AS_LAYER.to_owned(),
        Some(Some(cap)) => cap.label().to_owned(),
    }
}

/// The label of a point's join: the layer's own, a join, or a disagreement.
fn join_label(join: Option<Option<StrokeJoin>>) -> String {
    match join {
        None => MIXED.to_owned(),
        Some(None) => SAME_AS_LAYER.to_owned(),
        Some(Some(join)) => match join {
            StrokeJoin::Miter => "Miter",
            StrokeJoin::Bevel => "Bevel",
            StrokeJoin::Round => "Round",
        }
        .to_owned(),
    }
}

/// A number, as figcraft's fields write it: two decimals, and no trailing
/// `.0`.
///
/// figcraft's `fmt_num`, word for word: a value that is not finite has no
/// text, and a whole number is written without a decimal point.
fn fmt(value: f64) -> String {
    if !value.is_finite() {
        return String::new();
    }
    let rounded = (value * 100.0).round() / 100.0;
    if (rounded - rounded.round()).abs() < 1e-9 {
        format!("{}", rounded.round() as i64)
    } else {
        format!("{rounded}")
    }
}

/// A row: a label on the left, the value on the right.
///
/// The shape every labelled field shares, so this surface cannot drift into
/// a second look for the same kind of value. `dimmed` is how a field that
/// cannot be used says so — the value stays readable, which matters, because
/// a value nobody can change is still a value worth reading.
fn field(label: &str, value: &str, selector: &str, dimmed: bool) -> AnyElement {
    let selector = selector.to_owned();
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
    let field = if dimmed { field.opacity(0.55) } else { field };
    field
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .into_any_element()
}

/// Two fields side by side, as the two-column rows are.
fn field_pair(left: AnyElement, right: AnyElement) -> Div {
    div().flex().gap_2().child(left).child(right)
}

/// A sub-label above a row, in figcraft's own words.
fn sublabel(text: &str, selector: &str) -> AnyElement {
    let selector = selector.to_owned();
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
        .into_any_element()
}

/// A note under the rows.
fn note(text: &str, selector: &str) -> AnyElement {
    let selector = selector.to_owned();
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    use figcraft_doc::{NodeKind, Segment, VectorNetwork, Vertex, WidthProfile};
    use figcraft_engine::Session;
    use gpui::{point, size, Entity, VisualTestContext};

    // ── The engine's own model ───────────────────────────────────────────────

    /// A document with one vector on it: a triangle's worth of network.
    fn document_with_a_vector() -> (Document, NodeId) {
        let mut doc = Document::new("test");
        // The document's own page: the session opens on it, and a layer on
        // another page is not on the one being edited.
        let page = doc.first_page().expect("a page");
        let network = VectorNetwork {
            vertices: vec![
                Vertex::new(0.0, 0.0),
                Vertex::new(100.0, 0.0),
                Vertex::new(100.0, 100.0),
            ],
            segments: vec![Segment::line(0, 1), Segment::line(1, 2)],
            regions: vec![],
        };
        let id = doc
            .create(
                page,
                NodeKind::Vector { network },
                "Path",
                0.0,
                0.0,
                100.0,
                100.0,
            )
            .expect("a vector");
        (doc, id)
    }

    /// The edit state of a vector with `points` selected.
    fn editing(id: NodeId, points: &[usize]) -> VectorEdit {
        let mut ve = VectorEdit::new(id);
        ve.vertices = points.to_vec();
        ve
    }

    // ── The model ───────────────────────────────────────────────────────────

    #[test]
    fn the_model_reads_the_points_of_the_vector_being_edited() {
        let (doc, id) = document_with_a_vector();
        let ve = editing(id, &[0, 1]);
        let model = VectorModel::read(&doc, &ve);

        assert_eq!(model.id, id);
        assert_eq!(model.points, vec![0, 1]);
        assert_eq!(
            model.position.map(|p| (p.x, p.y)),
            Some((0.0, 0.0)),
            "the selection's top-left, in the parent's coordinates"
        );
        let size = model.size.expect("two points have a box");
        assert_eq!((size.width, size.height), (100.0, 0.0));
    }

    #[test]
    fn one_point_has_no_box_to_show() {
        let (doc, id) = document_with_a_vector();
        let ve = editing(id, &[1]);
        let model = VectorModel::read(&doc, &ve);

        assert!(model.size.is_none(), "one point has no width or height");
        assert_eq!(model.position.map(|p| (p.x, p.y)), Some((100.0, 0.0)));
    }

    #[test]
    fn the_selection_folds_per_property() {
        let (mut doc, id) = document_with_a_vector();
        doc.get_mut(id).expect("the vector").kind = NodeKind::Vector {
            network: VectorNetwork {
                vertices: vec![
                    Vertex {
                        radius: 4.0,
                        ..Vertex::new(0.0, 0.0)
                    },
                    Vertex {
                        radius: 9.0,
                        ..Vertex::new(100.0, 0.0)
                    },
                ],
                segments: vec![Segment::line(0, 1)],
                regions: vec![],
            },
        };
        let ve = editing(id, &[0, 1]);
        let model = VectorModel::read(&doc, &ve);

        assert_eq!(
            model.radius, None,
            "the two points disagree about the radius"
        );
        assert_eq!(
            model.mirroring,
            Some(HandleMirroring::None),
            "but they agree about the mirroring"
        );

        let one = VectorModel::read(&doc, &editing(id, &[0]));
        assert_eq!(one.radius, Some(4.0), "one point has one radius");
    }

    #[test]
    fn a_points_own_cap_and_join_are_read_as_the_layers_own_when_it_has_none() {
        let (mut doc, id) = document_with_a_vector();
        doc.get_mut(id).expect("the vector").kind = NodeKind::Vector {
            network: VectorNetwork {
                vertices: vec![
                    Vertex {
                        cap: Some(StrokeCap::Round),
                        join: Some(StrokeJoin::Bevel),
                        ..Vertex::new(0.0, 0.0)
                    },
                    Vertex {
                        cap: Some(StrokeCap::Round),
                        join: Some(StrokeJoin::Bevel),
                        ..Vertex::new(100.0, 0.0)
                    },
                ],
                segments: vec![Segment::line(0, 1)],
                regions: vec![],
            },
        };
        let model = VectorModel::read(&doc, &editing(id, &[0, 1]));

        assert_eq!(model.cap, Some(Some(StrokeCap::Round)));
        assert_eq!(model.join, Some(Some(StrokeJoin::Bevel)));

        let (plain, _) = document_with_a_vector();
        let plain_model = VectorModel::read(&plain, &editing(id, &[0, 1]));
        assert_eq!(
            plain_model.cap,
            Some(None),
            "a point with no cap of its own takes the layer's"
        );
        assert_eq!(plain_model.join, Some(None));
    }

    #[test]
    fn the_points_that_disagree_are_mixed_and_not_a_number() {
        let (mut doc, id) = document_with_a_vector();
        doc.get_mut(id).expect("the vector").kind = NodeKind::Vector {
            network: VectorNetwork {
                vertices: vec![
                    Vertex {
                        radius: 4.0,
                        ..Vertex::new(0.0, 0.0)
                    },
                    Vertex {
                        radius: 9.0,
                        ..Vertex::new(100.0, 0.0)
                    },
                ],
                segments: vec![Segment::line(0, 1)],
                regions: vec![],
            },
        };
        let model = VectorModel::read(&doc, &editing(id, &[0, 1]));
        assert_eq!(model.radius, None, "no agreed radius");
        assert_eq!(radius_word(model.radius), MIXED);
        assert_eq!(
            cap_label(model.cap),
            SAME_AS_LAYER,
            "and the caps still agree: they are the layer's own"
        );
    }

    #[test]
    fn the_variable_width_tool_reads_its_selected_width_point() {
        let (mut doc, id) = document_with_a_vector();
        {
            let node = doc.get_mut(id).expect("the vector");
            node.stroke.weight = 4.0;
            node.stroke.width_profile = Some(WidthProfile::new([(0.25, 2.0), (0.75, 1.0)]));
        }
        let mut ve = editing(id, &[]);
        ve.tool = VTool::Width;
        ve.width_point = Some(1);
        let model = VectorModel::read(&doc, &ve);

        let point = model.width_point.expect("a width point");
        assert_eq!(point.index, 1);
        assert_eq!(point.t, 0.75);
        assert_eq!(point.scale, 1.0);
        assert_eq!(point.weight, 4.0);
        assert_eq!((point.left, point.right), (1.0, 1.0), "a symmetric point");
    }

    #[test]
    fn the_variable_width_tool_with_no_width_point_has_none_to_show() {
        let (doc, id) = document_with_a_vector();
        let mut ve = editing(id, &[]);
        ve.tool = VTool::Width;
        let model = VectorModel::read(&doc, &ve);

        assert!(model.width_point.is_none());
    }

    #[test]
    fn the_title_is_the_points_or_the_width_point() {
        let (doc, id) = document_with_a_vector();
        assert_eq!(
            VectorModel::read(&doc, &editing(id, &[0])).title(),
            TITLE_POINT
        );
        assert_eq!(
            VectorModel::read(&doc, &editing(id, &[0, 1])).title(),
            TITLE_POINTS
        );
        let mut ve = editing(id, &[]);
        ve.tool = VTool::Width;
        assert_eq!(VectorModel::read(&doc, &ve).title(), TITLE_WIDTH_POINT);
    }

    // ── The ownership verdict ───────────────────────────────────────────────

    #[test]
    fn no_fact_on_this_surface_can_be_written_back_to_the_source() {
        let (doc, id) = document_with_a_vector();
        let model = VectorModel::read(&doc, &editing(id, &[0, 1]));

        assert_eq!(
            model.persistence,
            Persistence::Unauthored,
            "a vector has no authored spelling, so the save path refuses the node"
        );
        for fact in [
            VectorFact::Position,
            VectorFact::Size,
            VectorFact::CornerRadius,
            VectorFact::Mirroring,
            VectorFact::Cap,
            VectorFact::Join,
            VectorFact::WidthPosition,
            VectorFact::Width,
            VectorFact::WidthSide,
        ] {
            let writable = model.writable(fact);
            assert!(
                !writable.is_writable(),
                "{fact:?} must not be offered as editable"
            );
            assert!(
                writable.reason().is_some_and(|reason| !reason.is_empty()),
                "{fact:?} must say why it cannot be changed"
            );
        }
    }

    // ── The change ───────────────────────────────────────────────────────────

    #[test]
    fn a_change_carries_the_engines_command_id_and_params() {
        let (command, params) = VectorAction::Position {
            x: Some(12.0),
            y: None,
        }
        .command();
        assert_eq!(command, "vector.edit.position");
        assert_eq!(params["x"], json!(12.0));
        assert!(
            params.get("y").is_none(),
            "only the key that changed is sent: {params}"
        );

        let (command, params) = VectorAction::Resize {
            width: Some(200.0),
            height: None,
        }
        .command();
        assert_eq!(command, "vector.edit.resize");
        assert_eq!(params["width"], json!(200.0));

        let (command, params) = VectorAction::CornerRadius(6.0).command();
        assert_eq!(command, "vector.edit.cornerRadius");
        assert_eq!(params["radius"], json!(6.0));

        let (command, params) = VectorAction::Mirroring(HandleMirroring::AngleAndLength).command();
        assert_eq!(command, "vector.edit.mirroring");
        assert_eq!(params["mode"], json!("angleAndLength"));

        let (command, params) = VectorAction::Cap(Some(StrokeCap::LineArrow)).command();
        assert_eq!(command, "vector.edit.strokeCap");
        assert_eq!(params["cap"], json!("lineArrow"));

        let (command, params) = VectorAction::Cap(None).command();
        assert_eq!(command, "vector.edit.strokeCap");
        assert!(
            params["cap"].is_null(),
            "the layer's own cap is null: {params}"
        );

        let (command, params) = VectorAction::Join(Some(StrokeJoin::Bevel)).command();
        assert_eq!(command, "vector.edit.strokeJoin");
        assert_eq!(params["join"], json!("bevel"));

        let (command, params) = VectorAction::Join(None).command();
        assert_eq!(command, "vector.edit.strokeJoin");
        assert!(params["join"].is_null(), "the layer's own join is null");

        let (command, params) = VectorAction::WidthPoint {
            index: 2,
            t: Some(0.25),
            width: None,
            side: None,
        }
        .command();
        assert_eq!(command, "vector.edit.widthPoint");
        assert_eq!(params["index"], json!(2));
        assert_eq!(params["t"], json!(0.25));

        let (command, params) = VectorAction::WidthPoint {
            index: 2,
            t: None,
            width: Some(8.0),
            side: Some("left"),
        }
        .command();
        assert_eq!(command, "vector.edit.widthPoint");
        assert_eq!(params["width"], json!(8.0));
        assert_eq!(params["side"], json!("left"));
    }

    // ── The engine accepts the change ───────────────────────────────────────

    /// A session editing the vector in `document_with_a_vector`.
    fn session_editing_a_vector(points: &[usize]) -> (Session, NodeId) {
        let (doc, id) = document_with_a_vector();
        let mut session = Session::with_document(doc);
        session
            .execute("vector.edit.begin", &json!({ "id": id.0 }))
            .expect("the vector edit begins");
        session
            .execute("vector.edit.select", &json!({ "vertices": points }))
            .expect("the points are selected");
        (session, id)
    }

    #[test]
    fn the_engine_moves_the_points_the_panel_asks_to_move() {
        let (mut session, id) = session_editing_a_vector(&[0, 1]);
        let (command, params) = VectorAction::Position {
            x: Some(10.0),
            y: Some(20.0),
        }
        .command();
        session.execute(&command, &params).expect("the points move");

        // The engine re-fits the layer's box around the network after every
        // edit, so the points keep their place inside the layer and the layer
        // moves instead: what the panel shows is the selection's place in the
        // parent's coordinates.
        let ve = vectoredit::editing(&session).expect("still editing");
        let model = VectorModel::read(session.doc().expect("a document"), &ve);
        assert_eq!(
            model.position.map(|p| (p.x, p.y)),
            Some((10.0, 20.0)),
            "the selection is where the panel asked for it"
        );
        assert_eq!(model.id, id);
    }

    #[test]
    fn the_engine_resizes_the_points_the_panel_asks_to_resize() {
        let (mut session, id) = session_editing_a_vector(&[0, 1]);
        let (command, params) = VectorAction::Resize {
            width: Some(200.0),
            height: None,
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the points resize");

        let doc = session.doc().expect("a document");
        let network = vectoredit::network(doc, id).expect("a network");
        assert_eq!(network.vertices[1].x - network.vertices[0].x, 200.0);
    }

    #[test]
    fn the_engine_rounds_the_corners_the_panel_asks_to_round() {
        let (mut session, id) = session_editing_a_vector(&[0, 1]);
        let (command, params) = VectorAction::CornerRadius(7.0).command();
        session
            .execute(&command, &params)
            .expect("the corners round");

        let doc = session.doc().expect("a document");
        let network = vectoredit::network(doc, id).expect("a network");
        assert_eq!(network.vertices[0].radius, 7.0);
        assert_eq!(network.vertices[1].radius, 7.0);
    }

    #[test]
    fn the_engine_mirrors_the_handles_the_panel_asks_to_mirror() {
        let (mut session, id) = session_editing_a_vector(&[0, 1]);
        let (command, params) = VectorAction::Mirroring(HandleMirroring::Angle).command();
        session
            .execute(&command, &params)
            .expect("the handles mirror");

        let doc = session.doc().expect("a document");
        let network = vectoredit::network(doc, id).expect("a network");
        assert_eq!(network.vertices[0].mirror, Some(HandleMirroring::Angle));
    }

    #[test]
    fn the_engine_caps_and_joins_the_stroke_where_the_panel_asks() {
        let (mut session, id) = session_editing_a_vector(&[0, 1]);
        let (command, params) = VectorAction::Cap(Some(StrokeCap::Round)).command();
        session.execute(&command, &params).expect("the cap is set");
        let (command, params) = VectorAction::Join(Some(StrokeJoin::Bevel)).command();
        session.execute(&command, &params).expect("the join is set");

        let doc = session.doc().expect("a document");
        let network = vectoredit::network(doc, id).expect("a network");
        assert_eq!(network.vertices[0].cap, Some(StrokeCap::Round));
        assert_eq!(network.vertices[0].join, Some(StrokeJoin::Bevel));
    }

    #[test]
    fn the_engine_moves_the_width_point_the_panel_asks_to_move() {
        let (mut doc, id) = document_with_a_vector();
        {
            let node = doc.get_mut(id).expect("the vector");
            node.stroke.weight = 4.0;
            node.stroke.width_profile = Some(WidthProfile::new([(0.0, 1.0), (1.0, 1.0)]));
        }
        let mut session = Session::with_document(doc);
        session
            .execute("vector.edit.begin", &json!({ "id": id.0 }))
            .expect("the vector edit begins");
        session
            .execute("vector.edit.tool", &json!({ "tool": "width" }))
            .expect("the width tool is picked");

        let (command, params) = VectorAction::WidthPoint {
            index: 0,
            t: Some(0.5),
            width: None,
            side: None,
        }
        .command();
        session
            .execute(&command, &params)
            .expect("the width point moves");

        let doc = session.doc().expect("a document");
        let profile = doc
            .node(id)
            .expect("the vector")
            .stroke
            .profile()
            .expect("a width profile");
        assert_eq!(profile.points[0].t, 0.5);
    }

    // ── The panel, in a window ───────────────────────────────────────────────

    /// A host for the surface in a window: it draws it and nothing else.
    struct Host {
        model: VectorModel,
    }

    impl Render for Host {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut Context<Self>,
        ) -> impl IntoElement {
            section(&self.model)
        }
    }

    /// Open a window with the surface in it, over the given model.
    fn open_section(
        cx: &mut gpui::TestAppContext,
        model: VectorModel,
    ) -> (Entity<Host>, &mut VisualTestContext) {
        cx.add_window_view(|_, _| Host { model })
    }

    /// Draw the surface in the window and settle.
    fn draw(root: &Entity<Host>, window: &mut VisualTestContext) {
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(320.0), px(900.0)),
            |_, _| root.clone().into_any_element(),
        );
        window.run_until_parked();
    }

    /// The bounds of an element the surface draws.
    fn bounds(
        window: &mut VisualTestContext,
        selector: &'static str,
    ) -> gpui::Bounds<gpui::Pixels> {
        window
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("the surface draws {selector}"))
    }

    #[gpui::test]
    fn the_surface_draws_the_points_of_the_vector(cx: &mut gpui::TestAppContext) {
        let (doc, id) = document_with_a_vector();
        let model = VectorModel::read(&doc, &editing(id, &[0, 1]));
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        for selector in [
            "vector-x",
            "vector-y",
            "vector-w",
            "vector-h",
            "vector-radius",
            "vector-mirroring",
            "vector-cap",
            "vector-join",
        ] {
            let bounds = bounds(window, selector);
            assert!(
                bounds.size.height > px(0.0),
                "{selector} is on screen: {bounds:?}"
            );
        }
        assert!(
            window.debug_bounds("vector-source-note").is_some(),
            "and the note says why the rows cannot be changed"
        );
    }

    #[gpui::test]
    fn the_surface_draws_the_width_point_under_the_width_tool(cx: &mut gpui::TestAppContext) {
        let (mut doc, id) = document_with_a_vector();
        {
            let node = doc.get_mut(id).expect("the vector");
            node.stroke.weight = 4.0;
            node.stroke.width_profile = Some(WidthProfile::new([(0.25, 2.0)]));
        }
        let mut ve = editing(id, &[]);
        ve.tool = VTool::Width;
        ve.width_point = Some(0);
        let model = VectorModel::read(&doc, &ve);
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        for selector in [
            "vector-width-position",
            "vector-width",
            "vector-width-left",
            "vector-width-right",
        ] {
            let bounds = bounds(window, selector);
            assert!(
                bounds.size.height > px(0.0),
                "{selector} is on screen: {bounds:?}"
            );
        }
        assert!(
            window.debug_bounds("vector-x").is_none(),
            "and the points are not shown under the width tool"
        );
    }

    #[gpui::test]
    fn the_surface_says_so_when_there_are_no_points_to_show(cx: &mut gpui::TestAppContext) {
        let (doc, id) = document_with_a_vector();
        let model = VectorModel::read(&doc, &editing(id, &[]));
        let (root, window) = open_section(cx, model);
        draw(&root, window);

        let empty = bounds(window, "vector-empty");
        assert!(
            empty.size.height > px(0.0),
            "the empty state is on screen: {empty:?}"
        );
        assert!(
            window.debug_bounds("vector-x").is_none(),
            "and there is no position row to say nothing about"
        );
    }
}
