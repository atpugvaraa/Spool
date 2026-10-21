//! The effects section of the Design panel, ported from figcraft's egui frontend.
//!
//! # What this module owns
//!
//! Two things, in one file because they are one thing:
//!
//! 1. **The model.** [`EffectsModel`] reads the engine's [`Document`] and turns
//!    the selected layers' [`Effect`]s into what the panel shows, and
//!    [`effect_command`] turns a change into the engine's own command.
//! 2. **The panel.** [`EffectsPanel`] draws that model and asks the engine for
//!    the changes the user makes.
//!
//! # The reference
//!
//! figcraft's right panel — `crates/ui-egui/src/right.rs`, the "Effects"
//! section — is the specification. Its list, its order, its per-effect fields,
//! its labels, its defaults and its empty state are the *engine's* own: the
//! effect types come from [`figcraft_doc::Effect::TYPES`] and the defaults
//! from [`figcraft_doc::Effect::of_type`], so the panel and the engine cannot
//! disagree about what an effect is.
//!
//! # Where this differs from figcraft, and why
//!
//! figcraft reads the whole effect list as one value: if the selection
//! disagrees about *any* effect, it shows the empty state and nothing else.
//! Spool's Inspector shows three states for every property — a value, "Mixed"
//! and "Not set" — so the model here folds the selection *per property*. One
//! differing radius must not hide the blur radius, the spread and the colour,
//! which all still have values the user agreed on.
//!
//! # Commands, not mutations
//!
//! Every change leaves through the engine's command surface: `effect.add`,
//! `effect.remove`, `effect.set` and `effect.move`. Nothing here writes to a
//! [`Document`] directly, so an edit made from this panel, from the CLI and
//! from an agent are the same edit and land in the same history.

use std::collections::BTreeSet;

use figcraft_doc::color::Color as EngineColor;
use figcraft_doc::{Document, Effect, EffectKind, NodeId};
use gpui::{
    div, prelude::*, px, rgb, rgba, AnyElement, Context, Div, Entity, FocusHandle, IntoElement,
    Render, SharedString, Window,
};
use serde_json::Value as JsonValue;

use crate::engine::canvas::EngineCanvas;
use crate::theme;

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

// ═══════════════════════════════════════════════════════════════════════════
// The effect types
// ═══════════════════════════════════════════════════════════════════════════

/// One kind of effect.
///
/// The seven the engine offers, in the order the engine's Design panel lists
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectType {
    DropShadow,
    InnerShadow,
    LayerBlur,
    BackgroundBlur,
    Noise,
    Texture,
    Glass,
}

impl EffectType {
    /// Every effect type, in the order the engine lists them.
    ///
    /// The order is the engine's: an effect list is read from the bottom up,
    /// and the panel draws them in that order.
    pub const ALL: [EffectType; 7] = [
        EffectType::DropShadow,
        EffectType::InnerShadow,
        EffectType::LayerBlur,
        EffectType::BackgroundBlur,
        EffectType::Noise,
        EffectType::Texture,
        EffectType::Glass,
    ];

    /// The type's label, as the engine's table spells it.
    pub fn label(self) -> &'static str {
        match self {
            EffectType::DropShadow => "Drop shadow",
            EffectType::InnerShadow => "Inner shadow",
            EffectType::LayerBlur => "Layer blur",
            EffectType::BackgroundBlur => "Background blur",
            EffectType::Noise => "Noise",
            EffectType::Texture => "Texture",
            EffectType::Glass => "Glass",
        }
    }

    /// The type's name, as the engine's command parameter spells it.
    pub fn type_name(self) -> &'static str {
        match self {
            EffectType::DropShadow => "dropShadow",
            EffectType::InnerShadow => "innerShadow",
            EffectType::LayerBlur => "layerBlur",
            EffectType::BackgroundBlur => "backgroundBlur",
            EffectType::Noise => "noise",
            EffectType::Texture => "texture",
            EffectType::Glass => "glass",
        }
    }

    /// The type of an effect, as the engine holds it.
    pub fn of(effect: &Effect) -> Self {
        match effect.kind {
            EffectKind::DropShadow(_) => EffectType::DropShadow,
            EffectKind::InnerShadow(_) => EffectType::InnerShadow,
            EffectKind::LayerBlur(_) => EffectType::LayerBlur,
            EffectKind::BackgroundBlur(_) => EffectType::BackgroundBlur,
            EffectKind::Noise(_) => EffectType::Noise,
            EffectKind::Texture(_) => EffectType::Texture,
            EffectKind::Glass(_) => EffectType::Glass,
        }
    }

    /// A new effect of this type, with the engine's own defaults.
    ///
    /// The engine's defaults, not a copy of them: `Effect::of_type` is what
    /// `effect.add` itself uses, so a shadow added here is the same shadow the
    /// command would add.
    pub fn default_effect(self) -> Option<Effect> {
        Effect::of_type(self.type_name())
    }

    /// The type the engine names `name`, if it is one of the seven.
    pub fn of_type_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.type_name() == name)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The three states of a property
// ═══════════════════════════════════════════════════════════════════════════

/// One property of an effect, across a selection.
///
/// Three states, never two: a value every selected layer agrees on, no value
/// at all, and a disagreement. The Inspector's own properties have the same
/// three, and the same words, so "Mixed" means one thing in the panel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum EffectValue<T> {
    /// Every selected layer agrees on this value.
    Set(T),
    /// No selected layer has this property set.
    #[default]
    Unset,
    /// The selected layers disagree.
    Mixed,
}

impl<T: PartialEq> EffectValue<T> {
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

    /// Whether no selected layer has this property set.
    pub fn is_unset(&self) -> bool {
        matches!(self, Self::Unset)
    }
}

impl<T: Copy> EffectValue<T> {
    /// The value, when the selection agrees on one.
    pub fn set(&self) -> Option<T> {
        match self {
            Self::Set(value) => Some(*value),
            _ => None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The properties
// ═══════════════════════════════════════════════════════════════════════════

/// One property of an effect.
///
/// The engine's own properties, named the engine's way. A shadow's blur radius
/// and a blur's radius are different properties of different effects, so they
/// are different variants rather than one "radius" that means two things.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectProperty {
    /// A shadow's horizontal offset.
    ShadowOffsetX,
    /// A shadow's vertical offset.
    ShadowOffsetY,
    /// A shadow's blur radius.
    ShadowRadius,
    /// A shadow's spread.
    ShadowSpread,
    /// Whether a drop shadow shows behind transparent areas.
    ShadowShowBehind,
    /// A blur's kind: uniform or progressive.
    BlurKind,
    /// A blur's radius.
    BlurRadius,
    /// A progressive blur's radius at its start.
    BlurStartRadius,
    /// Noise's kind: mono, duo or multi.
    NoiseKind,
    /// The size of a noise grain.
    NoiseSize,
    /// How many grains are drawn.
    NoiseDensity,
    /// How opaque the grains are.
    NoiseOpacity,
    /// The size of a texture's grain.
    TextureSize,
    /// How far a texture scatters pixels.
    TextureRadius,
    /// Whether a texture stays inside the layer's pixels.
    TextureClipToShape,
    /// Where the light comes from, in degrees.
    GlassAngle,
    /// How bright the glass's edge highlights are.
    GlassIntensity,
    /// How strongly the glass bends the backdrop.
    GlassRefraction,
    /// How far the glass's curved edge reaches in.
    GlassDepth,
    /// How much the glass splits the backdrop's colours.
    GlassDispersion,
    /// How blurred the backdrop is through the glass.
    GlassFrost,
    /// How widely the light spreads over the glass.
    GlassSplay,
    /// The effect's colour.
    Color,
    /// A duo noise's second colour.
    SecondaryColor,
}

impl EffectProperty {
    /// Every property an effect can have.
    pub const ALL: [EffectProperty; 24] = [
        EffectProperty::ShadowOffsetX,
        EffectProperty::ShadowOffsetY,
        EffectProperty::ShadowRadius,
        EffectProperty::ShadowSpread,
        EffectProperty::ShadowShowBehind,
        EffectProperty::BlurKind,
        EffectProperty::BlurRadius,
        EffectProperty::BlurStartRadius,
        EffectProperty::NoiseKind,
        EffectProperty::NoiseSize,
        EffectProperty::NoiseDensity,
        EffectProperty::NoiseOpacity,
        EffectProperty::TextureSize,
        EffectProperty::TextureRadius,
        EffectProperty::TextureClipToShape,
        EffectProperty::GlassAngle,
        EffectProperty::GlassIntensity,
        EffectProperty::GlassRefraction,
        EffectProperty::GlassDepth,
        EffectProperty::GlassDispersion,
        EffectProperty::GlassFrost,
        EffectProperty::GlassSplay,
        EffectProperty::Color,
        EffectProperty::SecondaryColor,
    ];

    /// The property's label, as the panel shows it.
    ///
    /// Empty for a property that has no label of its own: a dropdown names
    /// itself, and a colour is a swatch, a hex and an alpha.
    pub fn label(self) -> &'static str {
        match self {
            EffectProperty::ShadowOffsetX => "X",
            EffectProperty::ShadowOffsetY => "Y",
            EffectProperty::ShadowRadius => "Blur",
            EffectProperty::ShadowSpread => "Spread",
            EffectProperty::ShadowShowBehind => "Show behind transparent areas",
            EffectProperty::BlurKind => "",
            EffectProperty::BlurRadius => "Blur",
            EffectProperty::BlurStartRadius => "Start",
            EffectProperty::NoiseKind => "",
            EffectProperty::NoiseSize => "Size",
            EffectProperty::NoiseDensity => "Density",
            EffectProperty::NoiseOpacity => "Opacity",
            EffectProperty::TextureSize => "Size",
            EffectProperty::TextureRadius => "Radius",
            EffectProperty::TextureClipToShape => "Clip to shape",
            EffectProperty::GlassAngle => "Light",
            EffectProperty::GlassIntensity => "",
            EffectProperty::GlassRefraction => "Refraction",
            EffectProperty::GlassDepth => "Depth",
            EffectProperty::GlassDispersion => "Dispersion",
            EffectProperty::GlassFrost => "Frost",
            EffectProperty::GlassSplay => "Splay",
            EffectProperty::Color => "",
            EffectProperty::SecondaryColor => "",
        }
    }

    /// The property's unit, as the panel shows it: "°" or nothing.
    pub fn suffix(self) -> &'static str {
        match self {
            EffectProperty::GlassAngle => "°",
            _ => "",
        }
    }

    /// What kind of value the property holds.
    pub fn kind(self) -> PropertyKind {
        match self {
            EffectProperty::ShadowShowBehind | EffectProperty::TextureClipToShape => {
                PropertyKind::Flag
            }
            EffectProperty::BlurKind | EffectProperty::NoiseKind => PropertyKind::Choice,
            EffectProperty::Color | EffectProperty::SecondaryColor => PropertyKind::Color,
            EffectProperty::NoiseDensity
            | EffectProperty::NoiseOpacity
            | EffectProperty::GlassIntensity
            | EffectProperty::GlassRefraction
            | EffectProperty::GlassDispersion
            | EffectProperty::GlassSplay => PropertyKind::Proportion,
            _ => PropertyKind::Length,
        }
    }

    /// The property's name in a control id, for a test to look it up.
    pub fn slug(self) -> &'static str {
        match self {
            EffectProperty::ShadowOffsetX => "shadow-offset-x",
            EffectProperty::ShadowOffsetY => "shadow-offset-y",
            EffectProperty::ShadowRadius => "shadow-radius",
            EffectProperty::ShadowSpread => "shadow-spread",
            EffectProperty::ShadowShowBehind => "shadow-show-behind",
            EffectProperty::BlurKind => "blur-kind",
            EffectProperty::BlurRadius => "blur-radius",
            EffectProperty::BlurStartRadius => "blur-start-radius",
            EffectProperty::NoiseKind => "noise-kind",
            EffectProperty::NoiseSize => "noise-size",
            EffectProperty::NoiseDensity => "noise-density",
            EffectProperty::NoiseOpacity => "noise-opacity",
            EffectProperty::TextureSize => "texture-size",
            EffectProperty::TextureRadius => "texture-radius",
            EffectProperty::TextureClipToShape => "texture-clip-to-shape",
            EffectProperty::GlassAngle => "glass-angle",
            EffectProperty::GlassIntensity => "glass-intensity",
            EffectProperty::GlassRefraction => "glass-refraction",
            EffectProperty::GlassDepth => "glass-depth",
            EffectProperty::GlassDispersion => "glass-dispersion",
            EffectProperty::GlassFrost => "glass-frost",
            EffectProperty::GlassSplay => "glass-splay",
            EffectProperty::Color => "color",
            EffectProperty::SecondaryColor => "secondary-color",
        }
    }

    /// Read the property from one effect.
    ///
    /// `None` when the effect has no such property: a blur has no spread, and
    /// a property nobody set is a state, not a missing answer.
    pub fn read(self, effect: &Effect) -> Option<PropertyValue> {
        match (&effect.kind, self) {
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::ShadowOffsetX,
            ) => Some(PropertyValue::Length(shadow.offset.0)),
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::ShadowOffsetY,
            ) => Some(PropertyValue::Length(shadow.offset.1)),
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::ShadowRadius,
            ) => Some(PropertyValue::Length(shadow.radius)),
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::ShadowSpread,
            ) => Some(PropertyValue::Length(shadow.spread)),
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::ShadowShowBehind,
            ) => Some(PropertyValue::Flag(shadow.show_behind)),
            (
                EffectKind::LayerBlur(blur) | EffectKind::BackgroundBlur(blur),
                EffectProperty::BlurKind,
            ) => Some(PropertyValue::Choice(blur.blur_type.label())),
            (
                EffectKind::LayerBlur(blur) | EffectKind::BackgroundBlur(blur),
                EffectProperty::BlurRadius,
            ) => Some(PropertyValue::Length(blur.radius)),
            (
                EffectKind::LayerBlur(blur) | EffectKind::BackgroundBlur(blur),
                EffectProperty::BlurStartRadius,
            ) => Some(PropertyValue::Length(blur.start_radius)),
            (EffectKind::Noise(noise), EffectProperty::NoiseKind) => {
                Some(PropertyValue::Choice(noise.mode.label()))
            }
            (EffectKind::Noise(noise), EffectProperty::NoiseSize) => {
                Some(PropertyValue::Length(noise.size))
            }
            (EffectKind::Noise(noise), EffectProperty::NoiseDensity) => {
                Some(PropertyValue::Proportion(noise.density))
            }
            (EffectKind::Noise(noise), EffectProperty::NoiseOpacity) => {
                Some(PropertyValue::Proportion(f64::from(noise.opacity)))
            }
            (EffectKind::Texture(texture), EffectProperty::TextureSize) => {
                Some(PropertyValue::Length(texture.size))
            }
            (EffectKind::Texture(texture), EffectProperty::TextureRadius) => {
                Some(PropertyValue::Length(texture.radius))
            }
            (EffectKind::Texture(texture), EffectProperty::TextureClipToShape) => {
                Some(PropertyValue::Flag(texture.clip_to_shape))
            }
            (EffectKind::Glass(glass), EffectProperty::GlassAngle) => {
                Some(PropertyValue::Length(glass.light_angle))
            }
            (EffectKind::Glass(glass), EffectProperty::GlassIntensity) => {
                Some(PropertyValue::Proportion(glass.light_intensity))
            }
            (EffectKind::Glass(glass), EffectProperty::GlassRefraction) => {
                Some(PropertyValue::Proportion(glass.refraction))
            }
            (EffectKind::Glass(glass), EffectProperty::GlassDepth) => {
                Some(PropertyValue::Length(glass.depth))
            }
            (EffectKind::Glass(glass), EffectProperty::GlassDispersion) => {
                Some(PropertyValue::Proportion(glass.dispersion))
            }
            (EffectKind::Glass(glass), EffectProperty::GlassFrost) => {
                Some(PropertyValue::Length(glass.frost))
            }
            (EffectKind::Glass(glass), EffectProperty::GlassSplay) => {
                Some(PropertyValue::Proportion(glass.splay))
            }
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::Color,
            ) => Some(PropertyValue::Color(shadow.color)),
            (EffectKind::Noise(noise), EffectProperty::Color) => {
                Some(PropertyValue::Color(noise.color))
            }
            (EffectKind::Noise(noise), EffectProperty::SecondaryColor) => {
                Some(PropertyValue::Color(noise.secondary))
            }
            _ => None,
        }
    }

    /// Write the property into one effect.
    ///
    /// `false` when the effect has no such property, so a write that would go
    /// nowhere is refused rather than silently dropped.
    ///
    /// The clamps are the engine's own ranges: a radius cannot be negative, a
    /// grain is at least a hundredth of a unit across, a proportion is between
    /// zero and one.
    pub fn write(self, effect: &mut Effect, value: PropertyValue) -> bool {
        match (&mut effect.kind, self, value) {
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::ShadowOffsetX,
                PropertyValue::Length(v),
            ) => {
                shadow.offset.0 = v;
                true
            }
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::ShadowOffsetY,
                PropertyValue::Length(v),
            ) => {
                shadow.offset.1 = v;
                true
            }
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::ShadowRadius,
                PropertyValue::Length(v),
            ) => {
                shadow.radius = v.max(0.0);
                true
            }
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::ShadowSpread,
                PropertyValue::Length(v),
            ) => {
                shadow.spread = v;
                true
            }
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::ShadowShowBehind,
                PropertyValue::Flag(v),
            ) => {
                shadow.show_behind = v;
                true
            }
            (
                EffectKind::LayerBlur(blur) | EffectKind::BackgroundBlur(blur),
                EffectProperty::BlurKind,
                PropertyValue::Choice(v),
            ) => {
                blur.blur_type = blur_kind(v);
                true
            }
            (
                EffectKind::LayerBlur(blur) | EffectKind::BackgroundBlur(blur),
                EffectProperty::BlurRadius,
                PropertyValue::Length(v),
            ) => {
                blur.radius = v.clamp(0.0, figcraft_doc::MAX_BLUR_RADIUS);
                true
            }
            (
                EffectKind::LayerBlur(blur) | EffectKind::BackgroundBlur(blur),
                EffectProperty::BlurStartRadius,
                PropertyValue::Length(v),
            ) => {
                blur.start_radius = v.clamp(0.0, figcraft_doc::MAX_BLUR_RADIUS);
                true
            }
            (EffectKind::Noise(noise), EffectProperty::NoiseKind, PropertyValue::Choice(v)) => {
                noise.mode = noise_kind(v);
                true
            }
            (EffectKind::Noise(noise), EffectProperty::NoiseSize, PropertyValue::Length(v)) => {
                noise.size = v.clamp(0.01, 1000.0);
                noise.size_y = None;
                true
            }
            (
                EffectKind::Noise(noise),
                EffectProperty::NoiseDensity,
                PropertyValue::Proportion(v),
            ) => {
                noise.density = v.clamp(0.0, 1.0);
                true
            }
            (
                EffectKind::Noise(noise),
                EffectProperty::NoiseOpacity,
                PropertyValue::Proportion(v),
            ) => {
                noise.opacity = v.clamp(0.0, 1.0) as f32;
                true
            }
            (
                EffectKind::Texture(texture),
                EffectProperty::TextureSize,
                PropertyValue::Length(v),
            ) => {
                texture.size = v.clamp(0.01, 1000.0);
                texture.size_y = None;
                true
            }
            (
                EffectKind::Texture(texture),
                EffectProperty::TextureRadius,
                PropertyValue::Length(v),
            ) => {
                texture.radius = v.max(0.0);
                true
            }
            (
                EffectKind::Texture(texture),
                EffectProperty::TextureClipToShape,
                PropertyValue::Flag(v),
            ) => {
                texture.clip_to_shape = v;
                true
            }
            (EffectKind::Glass(glass), EffectProperty::GlassAngle, PropertyValue::Length(v)) => {
                glass.light_angle = v.clamp(-180.0, 180.0);
                true
            }
            (
                EffectKind::Glass(glass),
                EffectProperty::GlassIntensity,
                PropertyValue::Proportion(v),
            ) => {
                glass.light_intensity = v.clamp(0.0, 1.0);
                true
            }
            (
                EffectKind::Glass(glass),
                EffectProperty::GlassRefraction,
                PropertyValue::Proportion(v),
            ) => {
                glass.refraction = v.clamp(0.0, 1.0);
                true
            }
            (EffectKind::Glass(glass), EffectProperty::GlassDepth, PropertyValue::Length(v)) => {
                glass.depth = v.clamp(1.0, 1000.0);
                true
            }
            (
                EffectKind::Glass(glass),
                EffectProperty::GlassDispersion,
                PropertyValue::Proportion(v),
            ) => {
                glass.dispersion = v.clamp(0.0, 1.0);
                true
            }
            (EffectKind::Glass(glass), EffectProperty::GlassFrost, PropertyValue::Length(v)) => {
                glass.frost = v.clamp(0.0, 1000.0);
                true
            }
            (
                EffectKind::Glass(glass),
                EffectProperty::GlassSplay,
                PropertyValue::Proportion(v),
            ) => {
                glass.splay = v.clamp(0.0, 1.0);
                true
            }
            (
                EffectKind::DropShadow(shadow) | EffectKind::InnerShadow(shadow),
                EffectProperty::Color,
                PropertyValue::Color(v),
            ) => {
                shadow.color = v;
                true
            }
            (EffectKind::Noise(noise), EffectProperty::Color, PropertyValue::Color(v)) => {
                noise.color = v;
                true
            }
            (EffectKind::Noise(noise), EffectProperty::SecondaryColor, PropertyValue::Color(v)) => {
                noise.secondary = v;
                true
            }
            _ => false,
        }
    }

    /// The value as the panel shows it.
    ///
    /// The three states are shown honestly: the value, "Mixed" for a
    /// disagreement and "Not set" for a property nobody has.
    pub fn display(self, value: &EffectValue<PropertyValue>) -> String {
        match value {
            EffectValue::Set(value) => self.display_value(value),
            EffectValue::Unset => NOT_SET.to_owned(),
            EffectValue::Mixed => MIXED.to_owned(),
        }
    }

    /// One value, as the panel shows it.
    fn display_value(self, value: &PropertyValue) -> String {
        match value {
            PropertyValue::Length(v) => format!("{}{}", fmt_num(*v), self.suffix()),
            PropertyValue::Proportion(v) => format!("{}%", fmt_num(v * 100.0)),
            PropertyValue::Flag(v) => {
                if *v {
                    "On".to_owned()
                } else {
                    "Off".to_owned()
                }
            }
            PropertyValue::Choice(v) => (*v).to_owned(),
            PropertyValue::Color(v) => v.to_hex(),
        }
    }

    /// The text a field opens with: the number the user reads, and nothing
    /// else.
    ///
    /// The unit is decoration the field adds when it is not being edited: a
    /// buffer that carried `%` would not parse, so the arrows would stop
    /// working the moment a value was typed. A value nobody agrees on opens
    /// empty, because "Mixed" is not a number.
    pub fn seed(self, value: &EffectValue<PropertyValue>) -> String {
        match value {
            EffectValue::Set(PropertyValue::Length(v)) => fmt_num(*v),
            EffectValue::Set(PropertyValue::Proportion(v)) => fmt_num(v * 100.0),
            _ => String::new(),
        }
    }

    /// The value a typed number means, in the property's own units.
    ///
    /// `None` when the text is not a number, so a field that cannot be read
    /// commits nothing rather than committing zero.
    pub fn typed(self, text: &str) -> Option<PropertyValue> {
        let value: f64 = text.trim().parse().ok()?;
        match self.kind() {
            PropertyKind::Proportion => Some(PropertyValue::Proportion(value / 100.0)),
            _ => Some(PropertyValue::Length(value)),
        }
    }
}

/// What kind of value a property holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyKind {
    /// A length, in document units.
    Length,
    /// A proportion from 0 to 1, shown as a percentage.
    Proportion,
    /// A boolean.
    Flag,
    /// One of a fixed set of names.
    Choice,
    /// A colour, with its alpha.
    Color,
}

/// One property's value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PropertyValue {
    /// A length, in document units.
    Length(f64),
    /// A proportion from 0 to 1.
    Proportion(f64),
    /// A boolean.
    Flag(bool),
    /// One of a fixed set of names: "Uniform", "Progressive", "Mono", "Duo",
    /// "Multi".
    Choice(&'static str),
    /// A colour, with its alpha.
    Color(EngineColor),
}

impl PropertyValue {
    /// The number this value holds, when it holds one.
    pub fn number(&self) -> Option<f64> {
        match self {
            PropertyValue::Length(v) | PropertyValue::Proportion(v) => Some(*v),
            _ => None,
        }
    }

    /// The flag this value holds, when it holds one.
    pub fn flag(&self) -> Option<bool> {
        match self {
            PropertyValue::Flag(v) => Some(*v),
            _ => None,
        }
    }

    /// The choice this value holds, when it holds one.
    pub fn choice(&self) -> Option<&'static str> {
        match self {
            PropertyValue::Choice(v) => Some(v),
            _ => None,
        }
    }

    /// The colour this value holds, when it holds one.
    pub fn color(&self) -> Option<EngineColor> {
        match self {
            PropertyValue::Color(v) => Some(*v),
            _ => None,
        }
    }
}

/// The blur kind a label names.
fn blur_kind(label: &str) -> figcraft_doc::BlurType {
    figcraft_doc::BlurType::ALL
        .into_iter()
        .find(|kind| kind.label() == label)
        .unwrap_or_default()
}

/// The noise kind a label names.
fn noise_kind(label: &str) -> figcraft_doc::NoiseMode {
    figcraft_doc::NoiseMode::ALL
        .into_iter()
        .find(|kind| kind.label() == label)
        .unwrap_or_default()
}

// ═══════════════════════════════════════════════════════════════════════════
// Reading the model
// ═══════════════════════════════════════════════════════════════════════════

/// What kind of effect a row holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectRowKind {
    /// Every selected layer has the same kind of effect here.
    Same(EffectType),
    /// The selected layers disagree about the kind.
    ///
    /// They may not even agree that there *is* an effect here: a layer with
    /// two effects and a layer with one have no agreed first effect.
    Mixed,
}

/// One effect's properties, each with its own reading across the selection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EffectFields {
    readings: Vec<(EffectProperty, EffectValue<PropertyValue>)>,
}

impl EffectFields {
    /// The reading of one property.
    ///
    /// "Not set" for a property the effect does not have: a blur has no
    /// spread, and a property nobody set is a state, not a missing answer.
    pub fn get(&self, property: EffectProperty) -> EffectValue<PropertyValue> {
        self.readings
            .iter()
            .find(|(read, _)| *read == property)
            .map(|(_, value)| *value)
            .unwrap_or(EffectValue::Unset)
    }

    /// Every property, with its reading.
    pub fn readings(&self) -> &[(EffectProperty, EffectValue<PropertyValue>)] {
        &self.readings
    }
}

/// One effect, as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectRow {
    /// Where the effect sits in the engine's effect list.
    ///
    /// The engine's own index: `effect.set` and `effect.remove` name the
    /// effect by it, and the list is read from the bottom up.
    pub index: usize,
    /// What kind of effect it is.
    pub kind: EffectRowKind,
    /// Whether the effect is on.
    ///
    /// An effect that is present but off is a real state, and not the same as
    /// an effect that is not there at all.
    pub visible: EffectValue<bool>,
    /// The effect's own properties, each with its own reading.
    pub fields: EffectFields,
    /// The effect a change would be made on: the first selected layer's
    /// effect here.
    ///
    /// The engine's `effect.set` replaces the whole effect, so a change is
    /// made on the effect the panel is showing.
    pub base: Option<Effect>,
}

/// The effects of a selection, as the panel shows them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EffectsModel {
    /// The rows, in the order the panel draws them: the last effect first.
    ///
    /// The engine's effect list is read from the bottom up, so the first
    /// effect in the document is the last row on screen.
    pub rows: Vec<EffectRow>,
}

impl EffectsModel {
    /// The effects of the selected layers, as the panel shows them.
    ///
    /// A layer the document does not hold is skipped rather than shown as an
    /// empty one, and a selection with no effects at all has no rows.
    pub fn read(doc: &Document, selection: &[NodeId]) -> Self {
        let effects: Vec<&Vec<Effect>> = selection
            .iter()
            .filter_map(|id| doc.node(*id))
            .map(|node| &node.effects)
            .collect();
        let count = effects.iter().map(|list| list.len()).max().unwrap_or(0);
        let mut rows = Vec::new();
        // The list is drawn from the bottom up, so the last effect is the
        // first row.
        for index in (0..count).rev() {
            let at: Vec<Option<&Effect>> = effects.iter().map(|list| list.get(index)).collect();
            let kind = row_kind(&at);
            let visible =
                EffectValue::across(at.iter().map(|effect| effect.map(|effect| effect.visible)));
            let mut fields = EffectFields::default();
            for property in EffectProperty::ALL {
                let reading = EffectValue::across(
                    at.iter()
                        .map(|effect| effect.and_then(|effect| property.read(effect))),
                );
                fields.readings.push((property, reading));
            }
            rows.push(EffectRow {
                index,
                kind,
                visible,
                fields,
                base: at
                    .iter()
                    .find_map(|effect| effect.as_ref().map(|effect| (*effect).clone())),
            });
        }
        Self { rows }
    }

    /// Whether the selection has no effects at all.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The row for one index, if the panel is showing it.
    pub fn row(&self, index: usize) -> Option<&EffectRow> {
        self.rows.iter().find(|row| row.index == index)
    }
}

/// What kind of effect the selected layers have at one index.
///
/// Every layer must have an effect there, and they must all be the same kind:
/// a layer with no effect at all has no kind, and a layer with a different
/// kind has a different one.
fn row_kind(at: &[Option<&Effect>]) -> EffectRowKind {
    let kinds: Vec<Option<EffectType>> =
        at.iter().map(|effect| effect.map(EffectType::of)).collect();
    match kinds.iter().copied().collect::<Option<Vec<_>>>() {
        Some(kinds) if !kinds.is_empty() && kinds.windows(2).all(|pair| pair[0] == pair[1]) => {
            EffectRowKind::Same(kinds[0])
        }
        _ => EffectRowKind::Mixed,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Writing
// ═══════════════════════════════════════════════════════════════════════════

/// A change to the selection's effects.
#[derive(Clone, Debug, PartialEq)]
pub enum EffectEdit {
    /// Add an effect of this kind, with the engine's own defaults.
    Add(EffectType),
    /// Remove the effect at this index.
    Remove {
        /// The effect's index in the engine's list.
        index: usize,
    },
    /// Replace the effect at this index with this one.
    Set {
        /// The effect's index in the engine's list.
        index: usize,
        /// The effect, as the engine holds it.
        effect: Effect,
    },
    /// Move the effect at one index to another.
    Move {
        /// The index the effect is at now.
        from: usize,
        /// The index it goes to.
        to: usize,
    },
}

/// The command that makes one change to the selection's effects.
///
/// The engine's own commands, with the engine's own parameters:
/// `effect.add` takes a `type`, `effect.remove` and `effect.set` take an
/// `index`, `effect.set` takes the whole `effect`, and `effect.move` takes a
/// `from` and a `to`. The ids are the engine's, so the change lands in the
/// engine's history like any other.
pub fn effect_command(edit: &EffectEdit, ids: &[NodeId]) -> (String, JsonValue) {
    let mut params = serde_json::Map::new();
    params.insert(
        "ids".to_owned(),
        JsonValue::Array(ids.iter().map(|id| JsonValue::from(id.0)).collect()),
    );
    match edit {
        EffectEdit::Add(kind) => {
            params.insert(
                "type".to_owned(),
                JsonValue::String(kind.type_name().to_owned()),
            );
            ("effect.add".to_owned(), JsonValue::Object(params))
        }
        EffectEdit::Remove { index } => {
            params.insert("index".to_owned(), JsonValue::from(*index));
            ("effect.remove".to_owned(), JsonValue::Object(params))
        }
        EffectEdit::Set { index, effect } => {
            params.insert("index".to_owned(), JsonValue::from(*index));
            params.insert(
                "effect".to_owned(),
                serde_json::to_value(effect).unwrap_or(JsonValue::Null),
            );
            ("effect.set".to_owned(), JsonValue::Object(params))
        }
        EffectEdit::Move { from, to } => {
            params.insert("from".to_owned(), JsonValue::from(*from));
            params.insert("to".to_owned(), JsonValue::from(*to));
            ("effect.move".to_owned(), JsonValue::Object(params))
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// The effects panel: the engine's effects, as rows the user can change.
///
/// Its own entity, so it owns its own state — which settings are open, which
/// menu is open, what is being typed — and its own keyboard. The document is
/// the engine's: this panel reads it and asks the engine for changes, and
/// never writes to it.
pub struct EffectsPanel {
    /// The engine canvas: the document and the selection this panel reads.
    canvas: Entity<EngineCanvas>,
    /// The panel's own state.
    state: EffectsState,
    /// The keyboard focus, so a field being typed into can take the keyboard.
    focus: FocusHandle,
    /// The observation of the canvas, kept so it does not detach.
    ///
    /// An engine edit repaints the canvas, and the canvas repaints this
    /// panel: the panel shows the engine's document, and a panel that did not
    /// repaint would show an effect the engine has already moved past.
    _observation: gpui::Subscription,
}

/// The panel's own state. Presentation only: never in the document.
#[derive(Clone, Debug, Default)]
struct EffectsState {
    /// Which effect rows have their settings open.
    open: BTreeSet<usize>,
    /// Which menu is open.
    menu: Option<Menu>,
    /// The field being typed into, if one is.
    edit: Option<FieldEdit>,
}

/// Which menu is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    /// The type of the effect at this index.
    Type(usize),
    /// The kind of the blur at this index.
    BlurKind(usize),
    /// The kind of the noise at this index.
    NoiseKind(usize),
}

/// Which field of an effect is being edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EffectField {
    /// One of the effect's properties.
    Property(EffectProperty),
    /// The hex text of a colour property.
    ColorHex(EffectProperty),
    /// The alpha of a colour property.
    ColorAlpha(EffectProperty),
}

/// A value being typed into a field.
///
/// A buffer, not a second copy of the property: the value becomes real only
/// when Enter hands it to the engine as one semantic operation. The text
/// arrives selected, so the first keystroke replaces it rather than appending
/// to it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FieldEdit {
    /// Which field: the effect's index and the field within it.
    target: (usize, EffectField),
    /// The text as typed.
    buffer: String,
    /// Whether the text is selected, so the next keystroke replaces it.
    select_all: bool,
}

/// What a keystroke did to an open field.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EditOutcome {
    /// The buffer changed; the field is still open.
    Continue,
    /// The field closed and this text should be committed, if it parses.
    Commit(String),
    /// The field closed and nothing should reach the document.
    Abandon,
}

impl FieldEdit {
    /// Apply one keystroke to the open field.
    ///
    /// The whole grammar in one place, because it is a grammar: Enter commits,
    /// Escape abandons without touching the document, the first printable key
    /// replaces the selected text, and a modifier combination is an editor
    /// shortcut rather than a character.
    fn apply_key(&mut self, key: &str, typed: Option<&str>) -> EditOutcome {
        match key {
            "enter" | "return" => {
                let buffer = std::mem::take(&mut self.buffer);
                self.select_all = false;
                EditOutcome::Commit(buffer)
            }
            "escape" => {
                self.buffer.clear();
                self.select_all = false;
                EditOutcome::Abandon
            }
            "backspace" => {
                // With the text selected, Backspace deletes the selection.
                if self.select_all {
                    self.buffer.clear();
                    self.select_all = false;
                } else {
                    self.buffer.pop();
                }
                EditOutcome::Continue
            }
            _ => {
                let Some(typed) = typed else {
                    return EditOutcome::Continue;
                };
                if self.select_all {
                    self.buffer.clear();
                    self.select_all = false;
                }
                self.buffer.push_str(typed);
                EditOutcome::Continue
            }
        }
    }
}

impl EffectsPanel {
    /// The panel, over the engine it reads and writes.
    ///
    /// The canvas is the engine's document and the engine's selection, so the
    /// panel and the canvas cannot be about different layers.
    pub fn new(canvas: Entity<EngineCanvas>, cx: &mut Context<Self>) -> Self {
        let observation = cx.observe(&canvas, |_, _, cx| cx.notify());
        Self {
            canvas,
            state: EffectsState::default(),
            focus: cx.focus_handle(),
            _observation: observation,
        }
    }

    /// The engine canvas the panel reads and writes through.
    pub fn canvas(&self) -> &Entity<EngineCanvas> {
        &self.canvas
    }

    /// The effects section, as an element.
    ///
    /// The body of the Inspector's Effects section: the way to add an effect,
    /// and the effects themselves.
    pub fn section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model(cx);
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .px(px(14.0))
            .py(px(12.0))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if this.handle_key(&event.keystroke, cx) {
                    cx.stop_propagation();
                }
            }));
        body = body.child(self.add_button(cx));
        for row in &model.rows {
            body = body.child(self.row(row, cx));
        }
        body.into_any_element()
    }

    /// The model: the selected layers' effects, as the panel shows them.
    fn model(&self, cx: &mut Context<Self>) -> EffectsModel {
        self.canvas.read(cx).with_session(|session| {
            let Ok(doc) = session.doc() else {
                return EffectsModel::default();
            };
            EffectsModel::read(doc, &session.selection())
        })
    }

    /// The engine's selection.
    fn selection(&self, cx: &mut Context<Self>) -> Vec<NodeId> {
        self.canvas
            .read(cx)
            .with_session(|session| session.selection())
    }

    /// Run one engine command. The only way this panel changes the document.
    fn run(&mut self, id: &str, params: JsonValue, cx: &mut Context<Self>) -> bool {
        self.canvas
            .update(cx, |canvas, cx| canvas.run_engine_command(id, params, cx))
    }

    /// Make one change to the selection's effects.
    fn commit(&mut self, edit: &EffectEdit, cx: &mut Context<Self>) -> bool {
        let ids = self.selection(cx);
        if ids.is_empty() {
            return false;
        }
        let (id, params) = effect_command(edit, &ids);
        self.run(&id, params, cx)
    }

    /// The effect a change at `index` is made on: the first selected layer's
    /// effect there.
    fn base_effect(&self, index: usize, cx: &mut Context<Self>) -> Option<Effect> {
        self.canvas.read(cx).with_session(|session| {
            let doc = session.doc().ok()?;
            for id in session.selection() {
                let Some(node) = doc.node(id) else {
                    continue;
                };
                if let Some(effect) = node.effects.get(index) {
                    return Some(effect.clone());
                }
            }
            None
        })
    }

    // ── Changing the effects ─────────────────────────────────────────────────

    /// Add an effect, with the engine's own defaults.
    fn add_effect(&mut self, cx: &mut Context<Self>) {
        // figcraft's "Add effect" button adds a drop shadow: the first type in
        // the list, with the defaults the engine gives it.
        self.commit(&EffectEdit::Add(EffectType::DropShadow), cx);
    }

    /// Remove the effect at `index`.
    fn remove_effect(&mut self, index: usize, cx: &mut Context<Self>) {
        self.commit(&EffectEdit::Remove { index }, cx);
    }

    /// Change the type of the effect at `index`.
    ///
    /// The new effect is the engine's default for that type, so choosing a
    /// type gives the effect that type's defaults — which is what figcraft
    /// does, and what makes "Inner shadow" mean an inner shadow.
    fn set_effect_type(&mut self, index: usize, kind: EffectType, cx: &mut Context<Self>) {
        let Some(effect) = kind.default_effect() else {
            return;
        };
        self.commit(&EffectEdit::Set { index, effect }, cx);
    }

    /// Turn the effect at `index` on or off.
    fn set_visible(&mut self, index: usize, visible: bool, cx: &mut Context<Self>) {
        let Some(mut effect) = self.base_effect(index, cx) else {
            return;
        };
        effect.visible = visible;
        self.commit(&EffectEdit::Set { index, effect }, cx);
    }

    /// Change one property of the effect at `index`.
    fn set_property(
        &mut self,
        index: usize,
        property: EffectProperty,
        value: PropertyValue,
        cx: &mut Context<Self>,
    ) {
        let Some(mut effect) = self.base_effect(index, cx) else {
            return;
        };
        if !property.write(&mut effect, value) {
            return;
        }
        self.commit(&EffectEdit::Set { index, effect }, cx);
    }

    // ── The keyboard ─────────────────────────────────────────────────────────

    /// One keystroke, when a field is open.
    ///
    /// `true` when the panel took the key, so nothing else answers it too: a
    /// number being typed is not an editor shortcut.
    fn handle_key(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        if self.state.edit.is_none() {
            return false;
        }
        let key = keystroke.key.clone();
        let typed = if keystroke.modifiers.control || keystroke.modifiers.platform {
            None
        } else {
            keystroke.key_char.clone()
        };
        let target = self.state.edit.as_ref().map(|edit| edit.target);
        let outcome = {
            let Some(edit) = self.state.edit.as_mut() else {
                return false;
            };
            edit.apply_key(&key, typed.as_deref())
        };
        match outcome {
            EditOutcome::Continue => {
                cx.notify();
            }
            EditOutcome::Commit(text) => {
                self.state.edit = None;
                if let Some(target) = target {
                    self.commit_field(target, &text, cx);
                }
            }
            EditOutcome::Abandon => {
                self.state.edit = None;
                cx.notify();
            }
        }
        true
    }

    /// Commit a typed value to the field it was typed in.
    fn commit_field(&mut self, target: (usize, EffectField), text: &str, cx: &mut Context<Self>) {
        let (index, field) = target;
        match field {
            EffectField::Property(property) => {
                let Some(value) = property.typed(text) else {
                    return;
                };
                self.set_property(index, property, value, cx);
            }
            EffectField::ColorHex(property) => {
                let Some(color) = EngineColor::from_hex_or_name(text.trim()) else {
                    return;
                };
                let Some(current) = self.property_color(index, property, cx) else {
                    return;
                };
                self.set_property(
                    index,
                    property,
                    PropertyValue::Color(color.with_alpha(current.a)),
                    cx,
                );
            }
            EffectField::ColorAlpha(property) => {
                let Ok(alpha) = text.trim().parse::<f32>() else {
                    return;
                };
                let Some(current) = self.property_color(index, property, cx) else {
                    return;
                };
                self.set_property(
                    index,
                    property,
                    PropertyValue::Color(current.with_alpha((alpha / 100.0).clamp(0.0, 1.0))),
                    cx,
                );
            }
        }
    }

    /// The colour a colour property holds now.
    fn property_color(
        &self,
        index: usize,
        property: EffectProperty,
        cx: &mut Context<Self>,
    ) -> Option<EngineColor> {
        let effect = self.base_effect(index, cx)?;
        property.read(&effect)?.color()
    }

    // ── Drawing ──────────────────────────────────────────────────────────────

    /// The way to add an effect.
    fn add_button(&mut self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(SharedString::from("effects-add"))
            .debug_selector(|| "effects-add".to_owned())
            .flex()
            .items_center()
            .gap_2()
            .px(px(8.0))
            .py(px(7.0))
            .rounded_md()
            .bg(rgb(theme::SURFACE_RAISED))
            .border_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("＋")
            .child("Add effect")
            .on_click(cx.listener(|this, _, _, cx| {
                this.add_effect(cx);
            }))
            .into_any_element()
    }

    /// One effect: its header, and its settings when they are open.
    fn row(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> AnyElement {
        let mut column = div().flex().flex_col().gap_1();
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(self.open_button(row, cx))
            .child(self.type_dropdown(row, cx))
            .child(self.visible_button(row, cx))
            .child(self.remove_button(row, cx));
        column = column.child(header);
        if self.state.open.contains(&row.index) {
            column = column.child(self.settings(row, cx));
        }
        column.into_any_element()
    }

    /// The button that opens and closes an effect's settings.
    fn open_button(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> AnyElement {
        let index = row.index;
        let open = self.state.open.contains(&row.index);
        let selector = format!("effect-{index}-open");
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .justify_center()
            .w(px(20.0))
            .h(px(20.0))
            .rounded_sm()
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child(if open { "−" } else { "＋" })
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.state.open.contains(&index) {
                    this.state.open.remove(&index);
                } else {
                    this.state.open.insert(index);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// The dropdown that names the effect's type.
    fn type_dropdown(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> AnyElement {
        let index = row.index;
        let label = match row.kind {
            EffectRowKind::Same(kind) => kind.label().to_owned(),
            EffectRowKind::Mixed => MIXED.to_owned(),
        };
        let open = self.state.menu == Some(Menu::Type(index));
        let label_selector = format!("effect-label-{label}");
        let trigger = div()
            .id(SharedString::from(format!("effect-{index}-type")))
            .debug_selector(|| format!("effect-{index}-type"))
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
            .border_color(rgb(theme::BORDER_SOFT))
            .text_xs()
            .child(
                div()
                    .id(SharedString::from(label_selector.clone()))
                    .debug_selector(move || label_selector.clone())
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .child(label),
            )
            .child(div().text_color(rgb(theme::TEXT_MUTED)).child(if open {
                "▴"
            } else {
                "▾"
            }));
        let trigger = trigger.on_click(cx.listener(move |this, _, _, cx| {
            this.state.menu = if this.state.menu == Some(Menu::Type(index)) {
                None
            } else {
                Some(Menu::Type(index))
            };
            cx.notify();
        }));
        let mut menu = div().flex().flex_col().child(trigger);
        if open {
            let mut list = div()
                .flex()
                .flex_col()
                .gap_1()
                .p(px(6.0))
                .rounded_md()
                .bg(rgb(theme::WINDOW))
                .border_1()
                .border_color(rgb(theme::BORDER));
            // The engine's own table of types: the order and the labels are
            // the engine's, not a copy of them.
            for (label, type_name) in figcraft_doc::Effect::TYPES {
                let Some(kind) = EffectType::of_type_name(type_name) else {
                    continue;
                };
                let selector = format!("effect-{index}-type-{type_name}");
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
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.menu = None;
                        this.set_effect_type(index, kind, cx);
                    }));
                list = list.child(item);
            }
            menu = menu.child(list);
        }
        menu.into_any_element()
    }

    /// The button that turns the effect on and off.
    fn visible_button(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> AnyElement {
        let index = row.index;
        let visible = match row.visible {
            EffectValue::Set(true) => "On",
            EffectValue::Set(false) => "Off",
            _ => MIXED,
        };
        let on = matches!(row.visible, EffectValue::Set(true));
        div()
            .id(SharedString::from(format!("effect-{index}-visible")))
            .debug_selector(|| format!("effect-{index}-visible"))
            .flex()
            .items_center()
            .justify_center()
            .px(px(6.0))
            .py(px(6.0))
            .rounded_md()
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child(visible)
            .on_click(cx.listener(move |this, _, _, cx| {
                // A mixed effect is turned on: the button says what the
                // effect will be, and "on" is the one answer a button can
                // give that is not "mixed".
                this.set_visible(index, !on, cx);
            }))
            .into_any_element()
    }

    /// The button that removes the effect.
    fn remove_button(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> AnyElement {
        let index = row.index;
        div()
            .id(SharedString::from(format!("effect-{index}-remove")))
            .debug_selector(|| format!("effect-{index}-remove"))
            .flex()
            .items_center()
            .justify_center()
            .w(px(20.0))
            .h(px(20.0))
            .rounded_sm()
            .text_xs()
            .text_color(rgb(theme::TEXT_MUTED))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child("×")
            .on_click(cx.listener(move |this, _, _, cx| {
                this.remove_effect(index, cx);
            }))
            .into_any_element()
    }

    /// The effect's settings, for the effect's own kind.
    fn settings(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> AnyElement {
        let mut body = div().flex().flex_col().gap_2().pl(px(20.0));
        match row.kind {
            EffectRowKind::Same(EffectType::DropShadow)
            | EffectRowKind::Same(EffectType::InnerShadow) => {
                body = self.shadow_settings(row, cx);
            }
            EffectRowKind::Same(EffectType::LayerBlur)
            | EffectRowKind::Same(EffectType::BackgroundBlur) => {
                body = self.blur_settings(row, cx);
            }
            EffectRowKind::Same(EffectType::Noise) => {
                body = self.noise_settings(row, cx);
            }
            EffectRowKind::Same(EffectType::Texture) => {
                body = self.texture_settings(row, cx);
            }
            EffectRowKind::Same(EffectType::Glass) => {
                body = self.glass_settings(row, cx);
            }
            EffectRowKind::Mixed => {
                body = body.child(muted("The selected layers have different effects here."));
            }
        }
        body.into_any_element()
    }

    /// A shadow's settings: offset, blur, spread, colour, and — for a drop
    /// shadow — whether it shows behind transparent areas.
    fn shadow_settings(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> Div {
        let inner = matches!(row.kind, EffectRowKind::Same(EffectType::InnerShadow));
        let mut body = div().flex().flex_col().gap_2();
        body = body.child(field_pair(
            self.number_field(row, EffectProperty::ShadowOffsetX, cx),
            self.number_field(row, EffectProperty::ShadowOffsetY, cx),
        ));
        body = body.child(field_pair(
            self.number_field(row, EffectProperty::ShadowRadius, cx),
            self.number_field(row, EffectProperty::ShadowSpread, cx),
        ));
        body = body.child(self.color_field(row, EffectProperty::Color, cx));
        if !inner {
            body = body.child(self.checkbox(row, EffectProperty::ShadowShowBehind, cx));
        }
        body
    }

    /// A blur's settings: its kind, and its radius — or the ramp's two ends,
    /// when it is progressive.
    fn blur_settings(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> Div {
        let mut body = div().flex().flex_col().gap_2();
        body = body.child(self.blur_kind_dropdown(row, cx));
        let progressive = matches!(
            row.fields.get(EffectProperty::BlurKind).set(),
            Some(PropertyValue::Choice(label)) if label == "Progressive"
        );
        if progressive {
            body = body.child(field_pair(
                self.number_field(row, EffectProperty::BlurStartRadius, cx),
                self.number_field(row, EffectProperty::BlurRadius, cx),
            ));
        } else {
            body = body.child(self.number_field(row, EffectProperty::BlurRadius, cx));
        }
        body
    }

    /// Noise's settings: its kind, its grain, and its colour or its opacity.
    fn noise_settings(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> Div {
        let mut body = div().flex().flex_col().gap_2();
        body = body.child(self.noise_kind_dropdown(row, cx));
        body = body.child(field_pair(
            self.number_field(row, EffectProperty::NoiseSize, cx),
            self.number_field(row, EffectProperty::NoiseDensity, cx),
        ));
        let mode = row
            .fields
            .get(EffectProperty::NoiseKind)
            .set()
            .and_then(|value| value.choice())
            .unwrap_or("Mono");
        match mode {
            "Duo" => {
                body = body.child(self.color_field(row, EffectProperty::Color, cx));
                body = body.child(self.color_field(row, EffectProperty::SecondaryColor, cx));
            }
            "Multi" => {
                body = body.child(self.number_field(row, EffectProperty::NoiseOpacity, cx));
            }
            _ => {
                body = body.child(self.color_field(row, EffectProperty::Color, cx));
            }
        }
        body
    }

    /// A texture's settings: its grain, how far it scatters, and whether it
    /// stays inside the layer.
    fn texture_settings(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> Div {
        let mut body = div().flex().flex_col().gap_2();
        body = body.child(field_pair(
            self.number_field(row, EffectProperty::TextureSize, cx),
            self.number_field(row, EffectProperty::TextureRadius, cx),
        ));
        body = body.child(self.checkbox(row, EffectProperty::TextureClipToShape, cx));
        body
    }

    /// Glass's settings: where the light comes from, how it bends and splits
    /// the backdrop, and how frosted it is.
    fn glass_settings(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> Div {
        let mut body = div().flex().flex_col().gap_2();
        body = body.child(field_pair(
            self.number_field(row, EffectProperty::GlassAngle, cx),
            self.number_field(row, EffectProperty::GlassIntensity, cx),
        ));
        body = body.child(field_pair(
            self.number_field(row, EffectProperty::GlassRefraction, cx),
            self.number_field(row, EffectProperty::GlassDepth, cx),
        ));
        body = body.child(field_pair(
            self.number_field(row, EffectProperty::GlassDispersion, cx),
            self.number_field(row, EffectProperty::GlassFrost, cx),
        ));
        body = body.child(self.number_field(row, EffectProperty::GlassSplay, cx));
        body
    }

    /// One number field.
    fn number_field(
        &mut self,
        row: &EffectRow,
        property: EffectProperty,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let index = row.index;
        let value = row.fields.get(property);
        let editing = self
            .state
            .edit
            .as_ref()
            .filter(|edit| edit.target == (index, EffectField::Property(property)))
            .map(|edit| SharedString::from(edit.buffer.clone()));
        let shown = editing.unwrap_or_else(|| SharedString::from(property.display(&value)));
        let label = property.label();
        let selector = format!("effect-{index}-{}", property.slug());
        let seed = property.seed(&value);
        let field = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .flex_1()
            .items_center()
            .justify_between()
            .gap_2()
            .px(px(8.0))
            .py(px(7.0))
            .rounded_md()
            .bg(rgb(theme::WINDOW))
            .border_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .text_xs();
        let field = if label.is_empty() {
            field
                .justify_end()
                .child(div().text_color(rgb(theme::TEXT_SECONDARY)).child(shown))
        } else {
            field
                .child(div().text_color(rgb(theme::TEXT_MUTED)).child(label))
                .child(div().text_color(rgb(theme::TEXT_SECONDARY)).child(shown))
        };
        field
            .cursor_text()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.begin_edit(
                    index,
                    EffectField::Property(property),
                    seed.clone(),
                    window,
                    cx,
                );
            }))
            .into_any_element()
    }

    /// A checkbox: a mark and the property's label.
    fn checkbox(
        &mut self,
        row: &EffectRow,
        property: EffectProperty,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let index = row.index;
        let value = row.fields.get(property);
        let on = matches!(value.set(), Some(PropertyValue::Flag(true)));
        let mixed = value.is_mixed();
        let selector = format!("effect-{index}-{}", property.slug());
        let mark = div()
            .size(px(12.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(if on || mixed {
                theme::ACCENT
            } else {
                theme::BORDER
            }))
            .bg(rgb(if on { theme::ACCENT } else { theme::WINDOW }));
        let mut row_element = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .gap_2()
            .px(px(4.0))
            .py(px(4.0))
            .rounded_sm()
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
            .cursor_pointer()
            .child(mark)
            .child(property.label());
        if mixed {
            row_element = row_element.child(div().text_color(rgb(theme::TEXT_MUTED)).child(MIXED));
        }
        row_element
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_property(index, property, PropertyValue::Flag(!on), cx);
            }))
            .into_any_element()
    }

    /// A colour: a swatch, its hex, and its alpha.
    fn color_field(
        &mut self,
        row: &EffectRow,
        property: EffectProperty,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let index = row.index;
        let value = row.fields.get(property);
        let color = value.set().and_then(|value| value.color());
        let swatch = match color {
            Some(color) => {
                let [r, g, b, _] = color.to_rgba8();
                div()
                    .size(px(12.0))
                    .rounded_sm()
                    .bg(rgb((u32::from(r) << 16)
                        | (u32::from(g) << 8)
                        | u32::from(b)))
            }
            None => div().size(px(12.0)).rounded_sm().bg(rgba(0x00000000)),
        };
        let hex = color.map(|color| color.to_hex()).unwrap_or_default();
        let alpha = color.map(|color| f64::from(color.a) * 100.0).unwrap_or(0.0);
        let hex_selector = format!("effect-{index}-{}-hex", property.slug());
        let alpha_selector = format!("effect-{index}-{}-alpha", property.slug());
        let alpha_seed = fmt_num(alpha);
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(swatch)
            .child(
                div()
                    .id(SharedString::from(hex_selector.clone()))
                    .debug_selector(move || hex_selector.clone())
                    .flex_1()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px(px(8.0))
                    .py(px(7.0))
                    .rounded_md()
                    .bg(rgb(theme::WINDOW))
                    .border_1()
                    .border_color(rgb(theme::BORDER_SOFT))
                    .text_xs()
                    .cursor_text()
                    .child(div().text_color(rgb(theme::TEXT_MUTED)).child("Hex"))
                    .child(
                        div()
                            .text_color(rgb(theme::TEXT_SECONDARY))
                            .child(self.field_text(index, EffectField::ColorHex(property), &hex)),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let seed = hex.clone();
                        this.begin_edit(index, EffectField::ColorHex(property), seed, window, cx);
                    })),
            )
            .child(
                div()
                    .id(SharedString::from(alpha_selector.clone()))
                    .debug_selector(move || alpha_selector.clone())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px(px(8.0))
                    .py(px(7.0))
                    .rounded_md()
                    .bg(rgb(theme::WINDOW))
                    .border_1()
                    .border_color(rgb(theme::BORDER_SOFT))
                    .text_xs()
                    .cursor_text()
                    .child(div().text_color(rgb(theme::TEXT_MUTED)).child("Alpha"))
                    .child(
                        div()
                            .text_color(rgb(theme::TEXT_SECONDARY))
                            .child(self.field_text(
                                index,
                                EffectField::ColorAlpha(property),
                                &format!("{}%", alpha_seed),
                            )),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let seed = alpha_seed.clone();
                        this.begin_edit(index, EffectField::ColorAlpha(property), seed, window, cx);
                    })),
            )
            .into_any_element()
    }

    /// The text a field shows, or the text being typed into it.
    fn field_text(&self, index: usize, field: EffectField, fallback: &str) -> String {
        match self
            .state
            .edit
            .as_ref()
            .filter(|edit| edit.target == (index, field))
        {
            Some(edit) => edit.buffer.clone(),
            None => fallback.to_owned(),
        }
    }

    /// The dropdown that names a blur's kind.
    fn blur_kind_dropdown(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> AnyElement {
        let index = row.index;
        let label = row
            .fields
            .get(EffectProperty::BlurKind)
            .set()
            .and_then(|value| value.choice())
            .unwrap_or("Uniform")
            .to_owned();
        let choices: Vec<&'static str> = figcraft_doc::BlurType::ALL
            .iter()
            .map(|kind| kind.label())
            .collect();
        self.choice_dropdown(
            index,
            Menu::BlurKind(index),
            EffectProperty::BlurKind,
            label,
            choices,
            cx,
        )
    }

    /// The dropdown that names noise's kind.
    fn noise_kind_dropdown(&mut self, row: &EffectRow, cx: &mut Context<Self>) -> AnyElement {
        let index = row.index;
        let label = row
            .fields
            .get(EffectProperty::NoiseKind)
            .set()
            .and_then(|value| value.choice())
            .unwrap_or("Mono")
            .to_owned();
        let choices: Vec<&'static str> = figcraft_doc::NoiseMode::ALL
            .iter()
            .map(|kind| kind.label())
            .collect();
        self.choice_dropdown(
            index,
            Menu::NoiseKind(index),
            EffectProperty::NoiseKind,
            label,
            choices,
            cx,
        )
    }

    /// A dropdown of a property's choices.
    fn choice_dropdown(
        &mut self,
        index: usize,
        menu: Menu,
        property: EffectProperty,
        label: String,
        choices: Vec<&'static str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.state.menu == Some(menu);
        let selector = format!("effect-{index}-{}", property.slug());
        let trigger = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px(px(8.0))
            .py(px(7.0))
            .rounded_md()
            .bg(rgb(theme::WINDOW))
            .border_1()
            .border_color(rgb(theme::BORDER_SOFT))
            .text_xs()
            .child(div().text_color(rgb(theme::TEXT_SECONDARY)).child(label))
            .child(div().text_color(rgb(theme::TEXT_MUTED)).child(if open {
                "▴"
            } else {
                "▾"
            }));
        let trigger = trigger.on_click(cx.listener(move |this, _, _, cx| {
            this.state.menu = if this.state.menu == Some(menu) {
                None
            } else {
                Some(menu)
            };
            cx.notify();
        }));
        let mut column = div().flex().flex_col().child(trigger);
        if open {
            let mut list = div()
                .flex()
                .flex_col()
                .gap_1()
                .p(px(6.0))
                .rounded_md()
                .bg(rgb(theme::WINDOW))
                .border_1()
                .border_color(rgb(theme::BORDER));
            for (position, choice) in choices.into_iter().enumerate() {
                let item_selector = format!("effect-{index}-{}-{position}", property.slug());
                let item = div()
                    .id(SharedString::from(item_selector.clone()))
                    .debug_selector(move || item_selector.clone())
                    .px(px(6.0))
                    .py(px(5.0))
                    .rounded_sm()
                    .text_xs()
                    .text_color(rgb(theme::TEXT_SECONDARY))
                    .hover(|style| style.bg(rgb(theme::SURFACE_HOVER)))
                    .cursor_pointer()
                    .child(choice)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.menu = None;
                        this.set_property(index, property, PropertyValue::Choice(choice), cx);
                    }));
                list = list.child(item);
            }
            column = column.child(list);
        }
        column.into_any_element()
    }

    /// Open a field for typing.
    fn begin_edit(
        &mut self,
        index: usize,
        field: EffectField,
        seed: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A field that was open keeps what it was: the value becomes real
        // only when Enter hands it to the engine, so opening another field
        // abandons the one that was open rather than committing a value the
        // user did not finish.
        self.state.edit = Some(FieldEdit {
            target: (index, field),
            buffer: seed,
            select_all: true,
        });
        // The keyboard goes to the panel, so the key the user types reaches
        // the field and not the editor's shortcuts.
        self.focus.focus(window, cx);
        cx.notify();
    }
}

impl Render for EffectsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.section(cx)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Shared chrome
// ═══════════════════════════════════════════════════════════════════════════

/// Two fields side by side.
fn field_pair(left: impl IntoElement, right: impl IntoElement) -> Div {
    div().flex().gap_2().child(left).child(right)
}

/// A run of text that says why a row is not showing anything.
fn muted(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A number, as the panel shows it.
///
/// The engine's own rule: two decimals at most, and a whole number without
/// them, so `12` and `12.0` are one number in one spelling.
fn fmt_num(value: f64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;

    use figcraft_doc::NodeKind;
    use figcraft_engine::Session;
    use gpui::{point, size, VisualTestContext};

    // ── The engine's own table ───────────────────────────────────────────────

    #[test]
    fn the_effect_types_are_the_engines_own_table() {
        // The order and the labels are the engine's. A copy of them here
        // would be a second place to forget to update.
        let engine: Vec<(&str, &str)> = figcraft_doc::Effect::TYPES.to_vec();
        let mine: Vec<(&str, &str)> = EffectType::ALL
            .iter()
            .map(|kind| (kind.label(), kind.type_name()))
            .collect();
        assert_eq!(
            mine, engine,
            "the panel's effect types are the engine's table, in the engine's order"
        );
    }

    #[test]
    fn an_effect_type_names_itself_the_way_the_engine_does() {
        assert_eq!(EffectType::DropShadow.type_name(), "dropShadow");
        assert_eq!(
            EffectType::of_type_name("backgroundBlur"),
            Some(EffectType::BackgroundBlur)
        );
        assert_eq!(EffectType::of_type_name("nonsense"), None);
    }

    // ── Reading the engine's document ───────────────────────────────────────

    /// A document with one rectangle, selected, with the given effects.
    fn document_with(effects: Vec<Effect>) -> (Document, NodeId) {
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(page, NodeKind::Rectangle, "Card", 0.0, 0.0, 100.0, 100.0)
            .expect("a rectangle");
        doc.get_mut(id).expect("the layer").effects = effects;
        (doc, id)
    }

    /// A drop shadow with a radius of `radius`.
    fn shadow_with_radius(radius: f64) -> Effect {
        let mut effect = Effect::drop_shadow();
        if let EffectKind::DropShadow(shadow) = &mut effect.kind {
            shadow.radius = radius;
            shadow.spread = 2.0;
            shadow.offset = (3.0, 4.0);
        }
        effect
    }

    #[test]
    fn an_effect_reads_back_the_engines_values() {
        // The type, its own fields, and whether it is on.
        let (doc, id) = document_with(vec![shadow_with_radius(12.0)]);
        let model = EffectsModel::read(&doc, &[id]);
        assert_eq!(model.rows.len(), 1, "one effect, one row");

        let row = &model.rows[0];
        assert_eq!(row.index, 0);
        assert_eq!(row.kind, EffectRowKind::Same(EffectType::DropShadow));
        assert_eq!(
            row.visible,
            EffectValue::Set(true),
            "the engine's effect is visible"
        );
        assert_eq!(
            row.fields.get(EffectProperty::ShadowRadius),
            EffectValue::Set(PropertyValue::Length(12.0))
        );
        assert_eq!(
            row.fields.get(EffectProperty::ShadowSpread),
            EffectValue::Set(PropertyValue::Length(2.0))
        );
        assert_eq!(
            row.fields.get(EffectProperty::ShadowOffsetX),
            EffectValue::Set(PropertyValue::Length(3.0))
        );
        assert_eq!(
            row.fields.get(EffectProperty::ShadowOffsetY),
            EffectValue::Set(PropertyValue::Length(4.0))
        );
        assert_eq!(
            row.fields.get(EffectProperty::ShadowShowBehind),
            EffectValue::Set(PropertyValue::Flag(false))
        );
        // A blur has no radius in the shadow's sense: a property nobody set is
        // a state.
        assert_eq!(
            row.fields.get(EffectProperty::BlurRadius),
            EffectValue::Unset
        );
    }

    #[test]
    fn an_effect_that_is_off_is_not_the_same_as_an_effect_that_is_not_there() {
        // `visible: false` is a real state, and the engine's own word for it.
        let mut effect = shadow_with_radius(4.0);
        effect.visible = false;
        let (doc, id) = document_with(vec![effect]);
        let model = EffectsModel::read(&doc, &[id]);
        assert_eq!(model.rows.len(), 1);
        assert_eq!(model.rows[0].visible, EffectValue::Set(false));

        // No effect at all: no row.
        let (doc, id) = document_with(Vec::new());
        let model = EffectsModel::read(&doc, &[id]);
        assert!(model.is_empty(), "a layer with no effects has no rows");
    }

    #[test]
    fn the_effect_list_is_read_from_the_bottom_up() {
        // The engine's list is drawn last-first, so the first effect in the
        // document is the last row on screen.
        let (doc, id) = document_with(vec![
            Effect::drop_shadow(),
            Effect::layer_blur(),
            Effect::noise(),
        ]);
        let model = EffectsModel::read(&doc, &[id]);
        let indexes: Vec<usize> = model.rows.iter().map(|row| row.index).collect();
        assert_eq!(indexes, vec![2, 1, 0], "the last effect is the first row");
        assert_eq!(model.rows[0].kind, EffectRowKind::Same(EffectType::Noise));
    }

    #[test]
    fn a_mixed_selection_reads_mixed_for_the_property_that_differs() {
        // Two layers, the same effect, one radius apart.
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let first = doc
            .create(page, NodeKind::Rectangle, "One", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let second = doc
            .create(page, NodeKind::Rectangle, "Two", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        doc.get_mut(first).expect("the layer").effects = vec![shadow_with_radius(4.0)];
        doc.get_mut(second).expect("the layer").effects = vec![shadow_with_radius(9.0)];

        let model = EffectsModel::read(&doc, &[first, second]);
        let row = &model.rows[0];
        assert_eq!(row.kind, EffectRowKind::Same(EffectType::DropShadow));
        assert_eq!(
            row.fields.get(EffectProperty::ShadowRadius),
            EffectValue::Mixed,
            "the radius is the property the layers disagree about"
        );
        // The properties they agree on still have their values.
        assert_eq!(
            row.fields.get(EffectProperty::ShadowSpread),
            EffectValue::Set(PropertyValue::Length(2.0)),
            "the spread is the same on both layers, so it is not mixed"
        );
        assert_eq!(
            row.fields.get(EffectProperty::ShadowOffsetX),
            EffectValue::Set(PropertyValue::Length(3.0)),
            "the offset is the same on both layers, so it is not mixed"
        );
        assert_eq!(row.visible, EffectValue::Set(true));
        // And the words the panel shows for them.
        assert_eq!(
            EffectProperty::ShadowRadius.display(&row.fields.get(EffectProperty::ShadowRadius)),
            "Mixed"
        );
        assert_eq!(
            EffectProperty::ShadowSpread.display(&row.fields.get(EffectProperty::ShadowSpread)),
            "2"
        );
    }

    #[test]
    fn a_layer_without_the_effect_reads_not_set_rather_than_a_value() {
        // One layer with a shadow, one with none: the shadow's properties are
        // not set on the second layer.
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let first = doc
            .create(page, NodeKind::Rectangle, "One", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        let second = doc
            .create(page, NodeKind::Rectangle, "Two", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        doc.get_mut(first).expect("the layer").effects = vec![shadow_with_radius(4.0)];

        let model = EffectsModel::read(&doc, &[first, second]);
        let row = &model.rows[0];
        assert_eq!(row.kind, EffectRowKind::Mixed);
        assert_eq!(
            row.fields.get(EffectProperty::ShadowRadius),
            EffectValue::Mixed,
            "one layer has a radius and the other has no effect at all"
        );
        assert_eq!(
            row.visible,
            EffectValue::Mixed,
            "one layer's effect is on and the other has none"
        );
    }

    // ── Writing ──────────────────────────────────────────────────────────────

    #[test]
    fn a_command_for_a_change_carries_the_engines_own_spelling() {
        // The engine's command, the engine's parameters, and the engine's
        // own spelling of the effect's fields.
        //
        // The spelling is the engine's own: `show_behind` and not
        // `showBehind`, `offset` as a pair, `color` as four channels. The
        // panel serializes the engine's `Effect` rather than writing the
        // fields out by hand, so the two cannot drift apart.
        let mut effect = shadow_with_radius(12.0);
        effect.visible = false;
        let (id, params) = effect_command(&EffectEdit::Set { index: 1, effect }, &[NodeId(7)]);

        assert_eq!(id, "effect.set");
        assert_eq!(params["ids"], serde_json::json!([7]));
        assert_eq!(params["index"], serde_json::json!(1));
        let written = &params["effect"];
        assert_eq!(written["type"], serde_json::json!("dropShadow"));
        assert_eq!(written["radius"], serde_json::json!(12.0));
        assert_eq!(written["spread"], serde_json::json!(2.0));
        assert_eq!(written["offset"], serde_json::json!([3.0, 4.0]));
        assert_eq!(written["show_behind"], serde_json::json!(false));
        assert_eq!(written["blend"], serde_json::json!("normal"));
        assert_eq!(written["visible"], serde_json::json!(false));
        assert_eq!(written["color"]["r"], serde_json::json!(0.0));
        assert_eq!(written["color"]["a"], serde_json::json!(0.25));
    }

    #[test]
    fn the_effect_json_is_the_engines_own() {
        // The same value, byte for byte, as the engine's own copy of a
        // layer's properties: `spool-cli`'s `edit.copyProperties` prints the
        // engine's serialization of an effect, and this is it.
        let effect = Effect::drop_shadow();
        let written = serde_json::to_value(&effect).expect("the effect as JSON");
        let object = written.as_object().expect("the effect is an object");
        let mut names: Vec<&str> = object.keys().map(|key| key.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "blend",
                "color",
                "offset",
                "radius",
                "show_behind",
                "spread",
                "type",
                "visible",
            ],
            "the effect's fields are named the engine's way"
        );
        assert_eq!(object["type"], serde_json::json!("dropShadow"));
        assert_eq!(object["blend"], serde_json::json!("normal"));
        assert_eq!(object["offset"], serde_json::json!([0.0, 4.0]));
        assert_eq!(object["radius"], serde_json::json!(4.0));
        assert_eq!(
            object["color"],
            serde_json::json!({"r": 0.0, "g": 0.0, "b": 0.0, "a": 0.25})
        );
    }

    #[test]
    fn the_commands_are_the_engines_own() {
        let (id, params) = effect_command(&EffectEdit::Add(EffectType::DropShadow), &[NodeId(3)]);
        assert_eq!(id, "effect.add");
        assert_eq!(params["ids"], serde_json::json!([3]));
        assert_eq!(params["type"], serde_json::json!("dropShadow"));

        let (id, params) = effect_command(&EffectEdit::Remove { index: 2 }, &[NodeId(3)]);
        assert_eq!(id, "effect.remove");
        assert_eq!(params["index"], serde_json::json!(2));

        let (id, params) = effect_command(&EffectEdit::Move { from: 0, to: 1 }, &[NodeId(3)]);
        assert_eq!(id, "effect.move");
        assert_eq!(params["from"], serde_json::json!(0));
        assert_eq!(params["to"], serde_json::json!(1));
    }

    #[test]
    fn the_engines_own_effect_list_takes_the_same_json() {
        // The `effects` prop of `node.set` and the `effect` of `effect.set`
        // are the same value: the engine's own `Effect`, serialized by the
        // engine. A panel that spelled an effect differently would be
        // rejected here.
        let written = serde_json::to_value(shadow_with_radius(12.0)).expect("the effect as JSON");
        let mut doc = Document::new("test");
        let page = doc.add_page("Page 1");
        let id = doc
            .create(page, NodeKind::Rectangle, "Card", 0.0, 0.0, 10.0, 10.0)
            .expect("a rectangle");
        doc.get_mut(id).expect("the layer").effects =
            serde_json::from_value(serde_json::json!([written]))
                .expect("the engine's own Effect takes the engine's own JSON");

        let model = EffectsModel::read(&doc, &[id]);
        assert_eq!(model.rows.len(), 1);
        assert_eq!(
            model.rows[0].fields.get(EffectProperty::ShadowRadius),
            EffectValue::Set(PropertyValue::Length(12.0)),
            "the engine's own JSON round-trips through the model"
        );
    }

    #[test]
    fn a_command_reaches_the_engines_document() {
        // The write path, end to end: the panel's command, the engine's
        // command, the engine's document.
        let (doc, id) = document_with(Vec::new());
        let mut session = Session::with_document(doc);
        session
            .execute("select.set", &serde_json::json!({ "ids": [id.0] }))
            .expect("the layer is selected");

        let (command, params) = effect_command(&EffectEdit::Add(EffectType::DropShadow), &[id]);
        session
            .execute(&command, &params)
            .expect("the effect is added");

        let doc = session.doc().expect("a document");
        let node = doc.node(id).expect("the layer");
        assert_eq!(node.effects.len(), 1, "the engine has the effect");
        assert_eq!(
            EffectType::of(&node.effects[0]),
            EffectType::DropShadow,
            "and it is the kind the panel asked for"
        );
    }

    #[test]
    fn a_property_the_effect_does_not_have_is_not_written() {
        // A blur has no spread: a write that would go nowhere is refused.
        let mut effect = Effect::layer_blur();
        assert!(!EffectProperty::ShadowSpread.write(&mut effect, PropertyValue::Length(1.0)));
        assert!(EffectProperty::BlurRadius.write(&mut effect, PropertyValue::Length(8.0)));
        assert_eq!(
            EffectProperty::BlurRadius.read(&effect),
            Some(PropertyValue::Length(8.0))
        );
    }

    // ── In a window ──────────────────────────────────────────────────────────

    /// A session over a document with one rectangle on it, selected.
    fn session_with(effects: Vec<Effect>) -> (Rc<RefCell<Session>>, u64) {
        let (doc, id) = document_with(effects);
        let session = Rc::new(RefCell::new(Session::with_document(doc)));
        session
            .borrow_mut()
            .execute("select.set", &serde_json::json!({ "ids": [id.0] }))
            .expect("the layer is selected");
        (session, id.0)
    }

    /// A root view that shows the effects panel on its own.
    ///
    /// The panel is normally part of the Inspector; a test that wants to see
    /// what it draws needs somewhere to draw it.
    struct Panel(Entity<EffectsPanel>);

    impl Render for Panel {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            self.0.clone()
        }
    }

    /// Open a window with the effects panel in it, over the given session.
    fn open_panel(
        cx: &mut gpui::TestAppContext,
        session: Rc<RefCell<Session>>,
    ) -> (Entity<Panel>, &mut VisualTestContext) {
        let (root, window) = cx.add_window_view(move |_, cx| {
            let canvas = cx.new(|cx| EngineCanvas::from_handle(Rc::clone(&session), cx));
            Panel(cx.new(|cx| EffectsPanel::new(canvas, cx)))
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

    /// The selector of the element that carries an effect's type label.
    fn label_selector(label: &str) -> &'static str {
        Box::leak(format!("effect-label-{label}").into_boxed_str())
    }

    #[gpui::test]
    fn the_panel_draws_the_effect_it_reads(cx: &mut gpui::TestAppContext) {
        // The render test: the panel, on its own, in a real window, showing
        // the effect the engine's document holds.
        let (session, _id) = session_with(vec![shadow_with_radius(12.0)]);
        let (panel, window) = open_panel(cx, session);
        draw(window, &panel);

        // "Drop shadow" is on screen: the row that carries the effect's type
        // was laid out.
        let bounds = window
            .debug_bounds(label_selector("Drop shadow"))
            .expect("the panel draws the effect's type");
        assert!(
            bounds.size.height > px(0.0),
            "the effect's type is on screen: {bounds:?}"
        );
        assert!(
            bounds.size.width > px(0.0),
            "the effect's type is on screen: {bounds:?}"
        );
    }

    #[gpui::test]
    fn the_panel_shows_the_empty_state_when_there_are_no_effects(cx: &mut gpui::TestAppContext) {
        // figcraft's empty state: nothing but the way to add an effect. Not
        // the placeholder's "No effects yet.".
        let (session, _id) = session_with(Vec::new());
        let (panel, window) = open_panel(cx, session);
        draw(window, &panel);

        assert!(
            window.debug_bounds("effects-add").is_some(),
            "the panel offers the way to add an effect"
        );
        assert!(
            window.debug_bounds(label_selector("Drop shadow")).is_none(),
            "and shows no effect at all"
        );
    }

    #[gpui::test]
    fn adding_an_effect_shows_its_fields(cx: &mut gpui::TestAppContext) {
        // The empty state and the full one, in the same window: the panel
        // reads the engine's document, so an effect added to it shows up.
        let (session, id) = session_with(Vec::new());
        let (panel, window) = open_panel(cx, Rc::clone(&session));
        draw(window, &panel);
        assert!(
            window.debug_bounds("effects-add").is_some(),
            "the panel starts empty"
        );
        assert!(
            window.debug_bounds(label_selector("Drop shadow")).is_none(),
            "and shows no effect"
        );

        // Add one, the way the panel's own button does.
        let (command, params) =
            effect_command(&EffectEdit::Add(EffectType::DropShadow), &[NodeId(id)]);
        session
            .borrow_mut()
            .execute(&command, &params)
            .expect("the effect is added");
        draw(window, &panel);

        assert!(
            window.debug_bounds(label_selector("Drop shadow")).is_some(),
            "the panel shows the effect that was added"
        );
        // And the effect's own row, with its type and its visibility.
        assert!(
            window.debug_bounds("effect-0-type").is_some(),
            "the panel shows the effect's row"
        );
        assert!(
            window.debug_bounds("effect-0-visible").is_some(),
            "the panel shows whether the effect is on"
        );
    }

    #[gpui::test]
    fn opening_an_effects_settings_shows_its_fields(cx: &mut gpui::TestAppContext) {
        // The settings are figcraft's: closed until the row is opened, and
        // then the effect's own fields.
        let (session, _id) = session_with(vec![shadow_with_radius(12.0)]);
        let (panel, window) = open_panel(cx, session);
        draw(window, &panel);
        assert!(
            window.debug_bounds("effect-0-shadow-radius").is_none(),
            "the settings start closed"
        );

        let open = window
            .debug_bounds("effect-0-open")
            .expect("the panel draws the row's open button");
        window.simulate_click(open.center(), gpui::Modifiers::none());
        draw(window, &panel);

        assert!(
            window.debug_bounds("effect-0-shadow-radius").is_some(),
            "the opened effect shows its blur radius"
        );
        assert!(
            window.debug_bounds("effect-0-shadow-spread").is_some(),
            "and its spread"
        );
        assert!(
            window.debug_bounds("effect-0-shadow-offset-x").is_some(),
            "and its offset"
        );
    }
}
