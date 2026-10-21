//! The engine-backed mirror of the Inspector's data model.
//!
//! The Inspector is a *surface*: it shows what is selected and asks for
//! changes to it. Where those values come from decides what an edit means, so
//! that "where" lives here rather than in the panel's widgets: this module
//! reads a layer's editable properties out of the engine's
//! [`Document`](figcraft_doc::Document) and turns a change into an engine
//! command.
//!
//! # Identity
//!
//! The engine's [`NodeId`](figcraft_doc::NodeId) *is* the manifest's node id —
//! the `id:` in `lamine.yaml` — as a `u64`. The manifest's `key` (the
//! `data-spool-id` in the markup) names the same node, and the source binding
//! (file and selector) names where it is authored. So an engine node id is the
//! document's identity, not a runtime handle.
//!
//! A layer the engine made itself — a rectangle drawn on the canvas — has a
//! fresh id with no manifest node behind it. It has no source binding, and
//! therefore nowhere to be authored. That is reported rather than invented:
//! the panel shows the layer, and the save path says the edit has nowhere to
//! go.
//!
//! # Commands, not mutations
//!
//! Every write here is a command id and its parameters. Nothing in this module
//! changes a [`Document`](figcraft_doc::Document) directly: the engine's
//! command surface is the one operation boundary, so an edit made from the
//! panel, from a shortcut, from the CLI and from an agent are the same edit and
//! land in the same history.
//!
//! # Spaces
//!
//! X and Y are measured in the space of the frame the layer is in — the same
//! space the engine's own design panel uses
//! ([`Document::frame_relative`](figcraft_doc::Document::frame_relative)), and
//! the same space `node.set`'s `x` and `y` are written in. A layer directly on
//! a page is measured from the page.

use figcraft_doc::color::Color as EngineColor;
use figcraft_doc::color::Paint as EnginePaint;
use figcraft_doc::{Document, Node, NodeId};
use gpui::{point, size, Point, Size};
use serde_json::Value;

/// The engine's identity for a layer.
///
/// The manifest's node id, as the engine holds it: a `u64` that is the same
/// number in the manifest, in the engine's document and in the panel.
pub use figcraft_doc::NodeId as EngineObjectId;

/// A colour, in the eight bits per channel the panel draws.
///
/// The engine's colour carries an alpha channel and floats per channel; the
/// panel draws an opaque swatch and names a colour in hex, so the model's
/// colour is the eight-bit form the panel already used. The conversion is
/// here, in one place, rather than spread across the widgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Color {
    /// A colour from the panel's own `0xRRGGBB` form.
    pub const fn from_rgb(value: u32) -> Self {
        Self {
            red: ((value >> 16) & 0xff) as u8,
            green: ((value >> 8) & 0xff) as u8,
            blue: (value & 0xff) as u8,
        }
    }

    /// The panel's own `0xRRGGBB` form.
    pub const fn to_rgb(self) -> u32 {
        ((self.red as u32) << 16) | ((self.green as u32) << 8) | self.blue as u32
    }

    /// The engine's colour, as eight bits per channel.
    pub fn from_engine(color: EngineColor) -> Self {
        let channel = |value: f32| {
            if value.is_finite() {
                (value.clamp(0.0, 1.0) * 255.0).round() as u32
            } else {
                0
            }
        };
        Self {
            red: channel(color.r) as u8,
            green: channel(color.g) as u8,
            blue: channel(color.b) as u8,
        }
    }

    /// The engine's colour, opaque.
    pub fn to_engine(self) -> EngineColor {
        EngineColor::new(
            f32::from(self.red) / 255.0,
            f32::from(self.green) / 255.0,
            f32::from(self.blue) / 255.0,
            1.0,
        )
    }

    /// The colour as the engine spells it in a command: `#RRGGBB`.
    pub fn to_hex(self) -> String {
        format!("#{:06X}", self.to_rgb())
    }
}

/// A stroke: the colour it is painted with and how wide it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    pub color: Color,
    pub width: f32,
}

/// A box: where a layer is and how big it is.
///
/// The position is in the frame the layer is in, which is the space the
/// engine's `node.set` writes `x` and `y` in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub position: Point<f32>,
    pub size: Size<f32>,
}

/// One layer, with the properties the Inspector shows and edits.
#[derive(Clone, Debug, PartialEq)]
pub struct DesignObject {
    /// The engine's identity for this layer.
    pub id: EngineObjectId,
    pub name: String,
    /// The engine's own label for the kind: "Frame", "Rectangle", "Text"…
    pub kind: String,
    pub position: Point<f32>,
    pub size: Size<f32>,
    /// The text a text layer holds, if it is one.
    pub text_content: Option<String>,
    /// The colour the text is painted with, if it is painted with one.
    pub text_color: Option<Color>,
    /// The font size, in the points the text is set at.
    pub font_size: Option<f32>,
    /// The fill's colour, when the layer has a solid fill.
    pub fill: Option<Color>,
    /// The stroke, when the layer has one.
    pub stroke: Option<Stroke>,
    /// The corner radius, when every corner agrees on one.
    ///
    /// `None` when the corners disagree: the panel has one radius field, and a
    /// number there would be a radius the user never chose.
    pub border_radius: Option<f32>,
    /// The layer's opacity, from 0 to 1.
    pub opacity: f32,
}

/// A change to a layer's paint or its typography.
///
/// One variant per thing the panel can change, so a change that has no
/// command cannot be written down as though it had one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StyleEdit {
    /// The fill's colour, or no fill at all.
    Fill(Option<Color>),
    /// The stroke's colour, or no stroke at all.
    Stroke(Option<Color>),
    /// How wide the stroke is.
    StrokeWidth(f32),
    /// The colour the text is painted with, or no explicit colour.
    TextColor(Option<Color>),
    /// The font size.
    FontSize(f32),
    /// The corner radius.
    BorderRadius(f32),
    /// The opacity, from 0 to 1.
    Opacity(f32),
}

/// A property of a layer's box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxProperty {
    X,
    Y,
    Width,
    Height,
}

impl BoxProperty {
    /// The `node.set` key this property is written with.
    pub fn key(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Y => "y",
            Self::Width => "width",
            Self::Height => "height",
        }
    }
}

// ── Reading ──────────────────────────────────────────────────────────────────

/// The layers the engine has selected, with the properties the panel shows.
///
/// A layer the document does not hold — a selection that outlived its layer —
/// is skipped rather than shown as an empty one.
pub fn selected_objects(doc: &Document, selection: &[NodeId]) -> Vec<DesignObject> {
    selection.iter().filter_map(|id| object(doc, *id)).collect()
}

/// One layer, with the properties the panel shows.
pub fn object(doc: &Document, id: NodeId) -> Option<DesignObject> {
    let node = doc.node(id)?;
    let geometry = geometry_of(doc, node);
    Some(DesignObject {
        id: node.id,
        name: node.name.clone(),
        kind: node.kind.label().to_owned(),
        position: geometry.position,
        size: geometry.size,
        text_content: node.kind.text().map(|text| text.characters.clone()),
        text_color: text_color_of(node),
        font_size: node.kind.text().map(|text| text.style.font_size as f32),
        fill: solid_color(&node.fills),
        stroke: stroke_of(node),
        border_radius: node.corners.uniform().map(|radius| radius as f32),
        opacity: node.opacity,
    })
}

/// The box of one layer: where it is and how big it is.
pub fn geometry(doc: &Document, id: NodeId) -> Option<Geometry> {
    let node = doc.node(id)?;
    Some(geometry_of(doc, node))
}

/// The box of one layer, in the frame the layer is in.
fn geometry_of(doc: &Document, node: &Node) -> Geometry {
    // The engine's own design-panel view reads the position this way
    // (`figcraft_engine`'s `describe`), so the panel and the engine agree on
    // what a layer's X and Y are.
    let coefficients = doc.frame_relative(node.id).as_coeffs();
    Geometry {
        position: point(coefficients[4] as f32, coefficients[5] as f32),
        size: size(node.width as f32, node.height as f32),
    }
}

/// The colour a text layer's text is painted with.
///
/// A text run may carry its own fills; when it does not, the text is painted
/// with the layer's own fill.
fn text_color_of(node: &Node) -> Option<Color> {
    let text = node.kind.text()?;
    text.style
        .fills
        .as_ref()
        .and_then(|paints| solid_color(paints))
        .or_else(|| solid_color(&node.fills))
}

/// The first visible solid paint's colour.
fn solid_color(paints: &[EnginePaint]) -> Option<Color> {
    paints
        .iter()
        .filter(|paint| paint.visible)
        .find_map(|paint| paint.solid_color())
        .map(Color::from_engine)
}

/// The stroke a layer has, if it has one.
fn stroke_of(node: &Node) -> Option<Stroke> {
    let color = solid_color(&node.strokes)?;
    Some(Stroke {
        color,
        width: stroke_width(&node.stroke) as f32,
    })
}

/// How wide the stroke is.
///
/// A stroke with per-side weights has no one width: the widest side is the
/// one the panel can honestly show.
fn stroke_width(stroke: &figcraft_doc::StrokeProps) -> f64 {
    match stroke.sides {
        Some(sides) => sides.iter().copied().fold(0.0, f64::max),
        None => stroke.weight,
    }
}

// ── Writing ──────────────────────────────────────────────────────────────────

/// The command that writes one property of a layer's box.
///
/// `node.set` with the one key the property is, so the other axis is left
/// exactly as it was.
pub fn box_command(property: BoxProperty, value: f32, id: EngineObjectId) -> (String, Value) {
    (
        "node.set".to_owned(),
        params(vec![
            ("ids", id_list(&[id])),
            ("props", props(vec![(property.key(), number(value))])),
        ]),
    )
}

/// The command that writes a style change across a selection.
///
/// Paint is a property of whatever is selected, so this takes the whole
/// selection rather than one layer: "make these all this colour" is one
/// operation.
pub fn style_command(edit: &StyleEdit, ids: &[NodeId]) -> (String, Value) {
    let (key, value) = match edit {
        StyleEdit::Fill(Some(color)) => ("fill", Value::String(color.to_hex())),
        StyleEdit::Fill(None) => ("fills", Value::Array(Vec::new())),
        StyleEdit::Stroke(Some(color)) => ("stroke", Value::String(color.to_hex())),
        StyleEdit::Stroke(None) => ("strokes", Value::Array(Vec::new())),
        StyleEdit::StrokeWidth(width) => ("strokeWeight", number(*width)),
        StyleEdit::TextColor(Some(color)) => (
            "textFills",
            Value::Array(vec![Value::String(color.to_hex())]),
        ),
        StyleEdit::TextColor(None) => ("textFills", Value::Null),
        StyleEdit::FontSize(size) => ("fontSize", number(*size)),
        StyleEdit::BorderRadius(radius) => ("cornerRadius", number(*radius)),
        StyleEdit::Opacity(opacity) => ("opacity", number(*opacity)),
    };
    (
        "node.set".to_owned(),
        params(vec![
            ("ids", id_list(ids)),
            ("props", props(vec![(key, value)])),
        ]),
    )
}

/// The command that renames a layer.
pub fn rename_command(id: EngineObjectId, name: &str) -> (String, Value) {
    (
        "object.rename".to_owned(),
        params(vec![
            ("ids", id_list(&[id])),
            ("name", Value::String(name.to_owned())),
        ]),
    )
}

/// The name of the engine's gesture for a property of the box.
///
/// The engine's own tools name their gestures the same way, so the undo stack
/// reads the same whichever surface made the change.
pub fn gesture_label(property: BoxProperty) -> &'static str {
    match property {
        BoxProperty::X | BoxProperty::Y => "Move",
        BoxProperty::Width | BoxProperty::Height => "Resize",
    }
}

// ── The parameter shapes the commands take ───────────────────────────────────

/// A JSON object, built one entry at a time.
fn params(entries: Vec<(&str, Value)>) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in entries {
        map.insert(key.to_owned(), value);
    }
    Value::Object(map)
}

/// The `props` object `node.set` takes.
fn props(entries: Vec<(&str, Value)>) -> Value {
    params(entries)
}

/// The `ids` array a command takes.
fn id_list(ids: &[NodeId]) -> Value {
    Value::Array(ids.iter().map(|id| Value::from(id.0)).collect())
}

/// A number, as a command parameter.
fn number(value: f32) -> Value {
    serde_json::Number::from_f64(f64::from(value))
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use figcraft_doc::{NodeKind, TextData, TextStyle};

    fn document_with_rectangle() -> (Document, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(
                page,
                NodeKind::Rectangle,
                "Rectangle",
                10.0,
                20.0,
                30.0,
                40.0,
            )
            .expect("a rectangle");
        (doc, id)
    }

    #[test]
    fn a_layer_is_read_out_of_the_engine_document() {
        let (doc, id) = document_with_rectangle();
        let object = object(&doc, id).expect("the layer");
        assert_eq!(object.name, "Rectangle");
        assert_eq!(object.kind, "Rectangle");
        assert_eq!(object.position.x, 10.0);
        assert_eq!(object.position.y, 20.0);
        assert_eq!(object.size.width, 30.0);
        assert_eq!(object.size.height, 40.0);
        assert_eq!(object.opacity, 1.0);
        assert_eq!(object.border_radius, Some(0.0));
    }

    #[test]
    fn the_engine_node_id_is_the_identity_the_panel_uses() {
        let (doc, id) = document_with_rectangle();
        let object = object(&doc, id).expect("the layer");
        assert_eq!(object.id, id);
        assert_eq!(
            object.id.0, id.0,
            "the id is the manifest's node id, as a number"
        );
    }

    #[test]
    fn a_selection_reads_only_the_layers_the_document_holds() {
        let (doc, id) = document_with_rectangle();
        let selected = selected_objects(&doc, &[id, NodeId(9999)]);
        assert_eq!(
            selected.len(),
            1,
            "a layer that is gone is not shown as empty"
        );
    }

    #[test]
    fn a_fill_is_read_as_the_colour_it_is_painted_with() {
        let (mut doc, id) = document_with_rectangle();
        let color = Color::from_rgb(0x336699);
        doc.get_mut(id).expect("the layer").fills = vec![EnginePaint::solid(color.to_engine())];
        let object = object(&doc, id).expect("the layer");
        assert_eq!(object.fill, Some(color));
    }

    #[test]
    fn a_layer_with_no_fill_has_none() {
        let (mut doc, id) = document_with_rectangle();
        doc.get_mut(id).expect("the layer").fills = Vec::new();
        let object = object(&doc, id).expect("the layer");
        assert_eq!(object.fill, None);
    }

    #[test]
    fn a_stroke_is_read_with_its_width() {
        let (mut doc, id) = document_with_rectangle();
        let color = Color::from_rgb(0x112233);
        let node = doc.get_mut(id).expect("the layer");
        node.strokes = vec![EnginePaint::solid(color.to_engine())];
        node.stroke.weight = 3.0;
        let object = object(&doc, id).expect("the layer");
        assert_eq!(object.stroke, Some(Stroke { color, width: 3.0 }));
    }

    #[test]
    fn a_layer_with_no_stroke_has_no_width() {
        let (mut doc, id) = document_with_rectangle();
        let node = doc.get_mut(id).expect("the layer");
        node.strokes = Vec::new();
        node.stroke.weight = 3.0;
        let object = object(&doc, id).expect("the layer");
        assert_eq!(
            object.stroke, None,
            "a width with no stroke is not a width of zero"
        );
    }

    #[test]
    fn corners_that_disagree_have_no_one_radius() {
        let (mut doc, id) = document_with_rectangle();
        doc.get_mut(id).expect("the layer").corners.radii = [0.0, 4.0, 0.0, 4.0];
        let object = object(&doc, id).expect("the layer");
        assert_eq!(
            object.border_radius, None,
            "a number here would be a radius nobody chose"
        );
    }

    #[test]
    fn a_text_layer_carries_its_text_and_its_size() {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let style = TextStyle {
            font_size: 24.0,
            ..Default::default()
        };
        let text = TextData {
            characters: "Headline".to_owned(),
            style,
            ..Default::default()
        };
        let id = doc
            .create(page, NodeKind::Text(text), "Text", 0.0, 0.0, 100.0, 30.0)
            .expect("a text layer");
        let object = object(&doc, id).expect("the layer");
        assert_eq!(object.text_content.as_deref(), Some("Headline"));
        assert_eq!(object.font_size, Some(24.0));
        assert_eq!(object.kind, "Text");
    }

    #[test]
    fn a_box_is_written_with_the_key_the_property_names() {
        let (id, value) = (NodeId(7), 12.5_f32);
        let (command, params) = box_command(BoxProperty::X, value, id);
        assert_eq!(command, "node.set");
        assert_eq!(params["ids"], serde_json::json!([7]));
        assert_eq!(params["props"]["x"], serde_json::json!(12.5));
    }

    #[test]
    fn a_style_edit_names_the_engine_key_it_writes() {
        let color = Color::from_rgb(0xff0000);
        let (command, params) = style_command(&StyleEdit::Fill(Some(color)), &[NodeId(3)]);
        assert_eq!(command, "node.set");
        assert_eq!(params["props"]["fill"], serde_json::json!("#FF0000"));

        let (_, params) = style_command(&StyleEdit::Fill(None), &[NodeId(3)]);
        assert_eq!(params["props"]["fills"], serde_json::json!([]));

        let (_, params) = style_command(&StyleEdit::Stroke(Some(color)), &[NodeId(3)]);
        assert_eq!(params["props"]["stroke"], serde_json::json!("#FF0000"));

        let (_, params) = style_command(&StyleEdit::Stroke(None), &[NodeId(3)]);
        assert_eq!(params["props"]["strokes"], serde_json::json!([]));

        let (_, params) = style_command(&StyleEdit::TextColor(Some(color)), &[NodeId(3)]);
        assert_eq!(params["props"]["textFills"], serde_json::json!(["#FF0000"]));

        let (_, params) = style_command(&StyleEdit::TextColor(None), &[NodeId(3)]);
        assert_eq!(params["props"]["textFills"], serde_json::Value::Null);

        let (_, params) = style_command(&StyleEdit::StrokeWidth(2.0), &[NodeId(3)]);
        assert_eq!(params["props"]["strokeWeight"], serde_json::json!(2.0));

        let (_, params) = style_command(&StyleEdit::FontSize(16.0), &[NodeId(3)]);
        assert_eq!(params["props"]["fontSize"], serde_json::json!(16.0));

        let (_, params) = style_command(&StyleEdit::BorderRadius(4.0), &[NodeId(3)]);
        assert_eq!(params["props"]["cornerRadius"], serde_json::json!(4.0));

        let (_, params) = style_command(&StyleEdit::Opacity(0.5), &[NodeId(3)]);
        assert_eq!(params["props"]["opacity"], serde_json::json!(0.5));
    }

    #[test]
    fn a_style_edit_applies_to_the_whole_selection() {
        let (_, params) = style_command(&StyleEdit::Opacity(1.0), &[NodeId(1), NodeId(2)]);
        assert_eq!(params["ids"], serde_json::json!([1, 2]));
    }

    #[test]
    fn a_rename_is_the_engines_own_command() {
        let (command, params) = rename_command(NodeId(4), "Headline");
        assert_eq!(command, "object.rename");
        assert_eq!(params["ids"], serde_json::json!([4]));
        assert_eq!(params["name"], serde_json::json!("Headline"));
    }

    #[test]
    fn a_colour_survives_the_round_trip_through_the_engines_own_form() {
        for value in [0x000000_u32, 0xFFFFFF, 0x336699, 0x7F7F7F] {
            let color = Color::from_rgb(value);
            let back = Color::from_engine(color.to_engine());
            assert_eq!(back, color, "{value:06X} did not survive the round trip");
        }
    }

    // ── The commands, against the engine itself ─────────────────────────────
    //
    // The shapes above say what the model *builds*. These say the engine
    // *accepts* them: a command id and a parameter shape the engine refuses
    // would be a panel whose every control logs a failure.

    use figcraft_engine::Session;

    /// A session over a document with one rectangle on it.
    fn session_with_rectangle() -> (Session, NodeId) {
        let (doc, id) = document_with_rectangle();
        (Session::with_document(doc), id)
    }

    /// The x a layer sits at, in the frame it is in.
    fn x_of(session: &Session, id: NodeId) -> f64 {
        let doc = session.doc().expect("a document");
        doc.node(id).expect("the layer");
        doc.frame_relative(id).as_coeffs()[4]
    }

    #[test]
    fn the_engine_accepts_the_box_command() {
        let (mut session, id) = session_with_rectangle();
        let (command, params) = box_command(BoxProperty::X, 42.0, id);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        assert_eq!(x_of(&session, id), 42.0);
    }

    #[test]
    fn the_engine_accepts_the_size_command() {
        let (mut session, id) = session_with_rectangle();
        let (command, params) = box_command(BoxProperty::Width, 120.0, id);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let doc = session.doc().expect("a document");
        assert_eq!(doc.node(id).expect("the layer").width, 120.0);
    }

    #[test]
    fn the_engine_accepts_the_fill_command() {
        let (mut session, id) = session_with_rectangle();
        let color = Color::from_rgb(0x336699);
        let (command, params) = style_command(&StyleEdit::Fill(Some(color)), &[id]);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(node.fills.len(), 1);
        assert_eq!(
            node.fills[0].solid_color().map(Color::from_engine),
            Some(color)
        );
    }

    #[test]
    fn the_engine_accepts_the_command_that_removes_a_fill() {
        let (mut session, id) = session_with_rectangle();
        let (command, params) = style_command(&StyleEdit::Fill(None), &[id]);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert!(node.fills.is_empty(), "no fill is no fill");
    }

    #[test]
    fn the_engine_accepts_the_stroke_commands() {
        let (mut session, id) = session_with_rectangle();
        let color = Color::from_rgb(0x112233);
        let (command, params) = style_command(&StyleEdit::Stroke(Some(color)), &[id]);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let (command, params) = style_command(&StyleEdit::StrokeWidth(4.0), &[id]);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(
            node.strokes[0].solid_color().map(Color::from_engine),
            Some(color)
        );
        assert_eq!(node.stroke.weight, 4.0);
    }

    #[test]
    fn the_engine_accepts_the_radius_and_opacity_commands() {
        let (mut session, id) = session_with_rectangle();
        let (command, params) = style_command(&StyleEdit::BorderRadius(8.0), &[id]);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let (command, params) = style_command(&StyleEdit::Opacity(0.5), &[id]);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(node.corners.uniform(), Some(8.0));
        assert_eq!(node.opacity, 0.5);
    }

    #[test]
    fn the_engine_accepts_the_text_commands() {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let text = TextData {
            characters: "Headline".to_owned(),
            ..Default::default()
        };
        let id = doc
            .create(page, NodeKind::Text(text), "Text", 0.0, 0.0, 100.0, 30.0)
            .expect("a text layer");
        let mut session = Session::with_document(doc);

        let (command, params) = style_command(&StyleEdit::FontSize(24.0), &[id]);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let color = Color::from_rgb(0x00FF00);
        let (command, params) = style_command(&StyleEdit::TextColor(Some(color)), &[id]);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");

        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        let text = node.kind.text().expect("a text layer");
        assert_eq!(text.style.font_size, 24.0);
        assert_eq!(
            text.style
                .fills
                .as_ref()
                .and_then(|paints| solid_color(paints)),
            Some(color)
        );
    }

    #[test]
    fn the_engine_accepts_the_rename_command() {
        let (mut session, id) = session_with_rectangle();
        let (command, params) = rename_command(id, "Headline");
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        let doc = session.doc().expect("a document");
        assert_eq!(doc.node(id).expect("the layer").name, "Headline");
    }

    #[test]
    fn a_drag_is_one_undo_step() {
        let (mut session, id) = session_with_rectangle();
        session.begin_interaction("Move").expect("a gesture opens");
        for x in [20.0_f32, 30.0, 40.0] {
            let (command, params) = box_command(BoxProperty::X, x, id);
            session
                .execute(&command, &params)
                .expect("the engine accepts the command");
        }
        session.commit_interaction().expect("the gesture closes");
        assert_eq!(
            session.undo_depth().0,
            1,
            "a drag is one step in the history, not one per pointer movement"
        );
        assert_eq!(
            x_of(&session, id),
            40.0,
            "and the layer is where the drag left it"
        );
        session.undo().expect("undo");
        assert_eq!(
            x_of(&session, id),
            10.0,
            "and undo puts the layer back where it was"
        );
    }

    #[test]
    fn an_abandoned_drag_leaves_the_document_alone() {
        let (mut session, id) = session_with_rectangle();
        session.begin_interaction("Move").expect("a gesture opens");
        let (command, params) = box_command(BoxProperty::X, 99.0, id);
        session
            .execute(&command, &params)
            .expect("the engine accepts the command");
        session
            .cancel_interaction()
            .expect("the gesture is abandoned");
        assert_eq!(x_of(&session, id), 10.0, "the layer is where it was");
        assert_eq!(
            session.undo_depth().0,
            0,
            "an abandoned drag records nothing"
        );
    }
}
