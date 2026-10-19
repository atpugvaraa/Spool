//! First-pass CSS interpretation.
//!
//! # Scope, deliberately
//!
//! This is not a CSS engine. It resolves a small, explicitly enumerated set of
//! declarations that the current project fixtures actually use, and it records
//! where each resolved value came from.
//!
//! Supported selectors: a single compound selector of tag name, `.class`, or
//! `#id`. No combinators, no pseudo-classes, no media queries, no `@import`.
//! Supported properties are listed in [`SUPPORTED_PROPERTIES`]. Anything else
//! is skipped rather than approximated.
//!
//! # Why provenance is carried
//!
//! Every resolved value keeps the file and byte range of the declaration that
//! produced it. That is what lets a later edit write back to the exact authored
//! span instead of regenerating a stylesheet. It is also why this module never
//! writes anything: resolution and mutation are separate concerns, and keeping
//! them apart is what stops `lamine.yaml` from becoming a second stylesheet.
//!
//! # Cascade
//!
//! Two declarations that both match an element compete on specificity first
//! (id, then class, then type — see [`Selector::specificity`]) and author
//! order second. That is the CSS cascade for the selector subset this parser
//! accepts. Origin (inline style beats stylesheet), `!important`, and inherited
//! values are *not* part of it: the inline style is applied by the caller in
//! `visual`, and inheritance belongs to the document tree rather than to a
//! stylesheet. `docs/` records that boundary.
//!
//! # One sheet at a time, and the rank that outlives it
//!
//! [`Stylesheet::resolve`] answers for a single sheet, and a project has more
//! than one. Merging several sheets is not "take the last sheet that has a
//! value": link order decides only between declarations of *equal* specificity,
//! so the ranking has to survive into the caller that merges. A merge done on
//! [`Stylesheet::resolve`] alone has already lost the fact that made an earlier
//! `#hero` win, and hands the element whichever sheet happens to be linked last.
//! [`Stylesheet::resolve_ranked`] is the same answer with the rank attached, for
//! that caller; `visual` is where the two are put back together.
//!
//! # Flex longhands
//!
//! `flex-direction`, `flex-wrap`, `gap`, `row-gap`, `column-gap`,
//! `justify-content`, and `align-items` are understood because `docs/02` records
//! authored `display: flex` as the on-disk representation of auto-layout, which
//! makes them authored properties rather than Spool-only facts. They are parsed,
//! cascaded, and reported with their provenance like any other declaration.
//! Nothing here lays anything out: what they mean for a frame is the engine's
//! business, and the divergences between Figma's auto-layout and CSS flexbox are
//! to be characterised where they are found rather than smoothed over here.

use std::collections::BTreeMap;

use crate::canvas::Color;

/// The properties this module understands. Anything else is ignored rather
/// than guessed at, so an unsupported declaration can never masquerade as a
/// supported one.
pub const SUPPORTED_PROPERTIES: &[&str] = &[
    "align-items",
    "background",
    "background-color",
    "border",
    "border-radius",
    "bottom",
    "box-sizing",
    "color",
    "column-gap",
    "display",
    "flex-direction",
    "flex-wrap",
    "font-size",
    "font-weight",
    "gap",
    "height",
    "justify-content",
    "left",
    "margin",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "opacity",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "position",
    "right",
    "row-gap",
    "text-align",
    "top",
    "transform",
    "width",
];

/// One declaration, with the authored span it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    pub property: String,
    pub value: String,
    /// Byte range of the value text, inside the authored stylesheet.
    pub value_range: (usize, usize),
    /// Author order, used as the tie-breaker between equal specificity.
    pub order: usize,
}

impl Declaration {
    /// This declaration's rank, for comparing against one from another rule.
    ///
    /// `specificity` is supplied by the owning rule, which is where the
    /// selector — and therefore the specificity — actually lives. Taken as a
    /// pair rather than a number so the two ranking criteria stay separable.
    fn rank_for(&self, specificity: u32) -> (u32, usize) {
        (specificity, self.order)
    }
}

/// The canonical longhands a supported declaration competes for under.
///
/// CSS expands a shorthand into longhands *before* the cascade runs, which is
/// why a later `background: blue` overrides an earlier `background-color: red`.
/// Keying both under one group is what makes author order mean the same thing
/// here as in a browser, and it keeps the winning declaration's authored span
/// pointing at the shorthand the author really wrote, so a later edit rewrites
/// the text that produced the value.
///
/// `gap` is the one shorthand that expands into *two* longhands, because it
/// carries both axes and a `row-gap` or `column-gap` written after it overrides
/// exactly one of them. Its row comes first, so a caller asking for `gap` is
/// answered with the declaration that won the row axis — the same
/// "whichever spelling won" answer [`Stylesheet::declaration_value_range`]
/// already gives for `background` and `background-color`. A writer that needs
/// both axes rewrites them one longhand at a time.
///
/// Every other supported property is its own group.
fn property_groups(property: &str) -> Vec<String> {
    match property {
        "background" | "background-color" => vec!["background-color".to_owned()],
        "gap" => vec!["row-gap".to_owned(), "column-gap".to_owned()],
        other => vec![other.to_owned()],
    }
}

/// A parsed stylesheet: its rules in author order and its `:root` custom
/// properties.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stylesheet {
    rules: Vec<Rule>,
    custom_properties: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
struct Rule {
    selector: Selector,
    declarations: Vec<Declaration>,
}

/// A single compound selector: `main`, `.cta`, `#hero`, `a.cta`, or `.a.b`.
///
/// A compound selector is one element matched by every simple part, with no
/// combinator. More than one class is allowed because an element can carry
/// several, and `.a.b` is how an author narrows a rule to that combination.
#[derive(Clone, Debug, PartialEq)]
struct Selector {
    tag: Option<String>,
    classes: Vec<String>,
    id: Option<String>,
}

impl Selector {
    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() || text.contains(char::is_whitespace) {
            // Whitespace means a combinator, which is out of scope.
            return None;
        }
        let mut selector = Self {
            tag: None,
            classes: Vec::new(),
            id: None,
        };
        let mut rest = text;
        while !rest.is_empty() {
            let first = rest.as_bytes()[0];
            let is_class = first == b'.';
            let is_id = first == b'#';
            let body = if is_class || is_id { &rest[1..] } else { rest };
            let end = body
                .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(body.len());
            // A bare `.` or `#` names nothing, so it is not a selector.
            if end == 0 {
                return None;
            }
            let name = body[..end].to_owned();
            if is_class {
                selector.classes.push(name);
            } else if is_id {
                // `#a#b` matches nothing; refuse it rather than pick one.
                if selector.id.is_some() {
                    return None;
                }
                selector.id = Some(name);
            } else {
                // A type selector may only lead, and only appear once:
                // `a.b` is valid CSS, `a.b c` is a combinator (already refused),
                // and `b.a` is not a selector this parser can honour.
                if selector.tag.is_some() || !selector.classes.is_empty() || selector.id.is_some() {
                    return None;
                }
                selector.tag = Some(name.to_ascii_lowercase());
            }
            rest = &body[end..];
        }
        (selector.tag.is_some() || !selector.classes.is_empty() || selector.id.is_some())
            .then_some(selector)
    }

    fn matches(&self, tag: &str, classes: &[String], id: Option<&str>) -> bool {
        if let Some(expected) = &self.tag {
            if expected != tag {
                return false;
            }
        }
        // Every class the selector names must be present: `.a.b` needs both.
        if !self
            .classes
            .iter()
            .all(|expected| classes.iter().any(|class| class == expected))
        {
            return false;
        }
        if let Some(expected) = &self.id {
            if id != Some(expected.as_str()) {
                return false;
            }
        }
        true
    }

    /// How specific this selector is, as a single comparable number.
    ///
    /// CSS ranks selectors as a tuple of three counts — ids, then classes and
    /// attributes, then types — compared lexicographically. Collapsing that
    /// into one number is only equivalent while each count stays below its
    /// radix, which the 100/10000 weights guarantee for the selectors this
    /// parser accepts (one id, and a bounded number of classes).
    ///
    /// It is computed from the selector rather than stored, so a selector and
    /// the rank it wins by cannot disagree.
    fn specificity(&self) -> u32 {
        let ids = u32::from(self.id.is_some()) * 10_000;
        let classes = self.classes.len() as u32 * 100;
        let tags = u32::from(self.tag.is_some());
        ids + classes + tags
    }
}

impl Stylesheet {
    /// Parse a stylesheet. Never fails: an unparseable rule is skipped, which
    /// matches how the source binder treats malformed markup.
    pub fn parse(source: &str) -> Self {
        let mut stylesheet = Self::default();
        let bytes = source.as_bytes();
        let mut order = 0usize;

        for (rule_range, prelude) in rules(source) {
            let selector_text = prelude.trim();
            // `:root` only contributes custom properties.
            if selector_text == ":root" {
                for (property, value, range) in declarations(source, rule_range) {
                    if let Some(name) = property.strip_prefix("--") {
                        stylesheet
                            .custom_properties
                            .insert(name.to_owned(), value.clone());
                        let _ = range;
                    }
                }
                continue;
            }
            let Some(selector) = Selector::parse(selector_text) else {
                continue;
            };
            let mut parsed = Vec::new();
            for (property, value, range) in declarations(source, rule_range) {
                if !SUPPORTED_PROPERTIES.contains(&property.as_str()) {
                    continue;
                }
                parsed.push(Declaration {
                    property,
                    value,
                    value_range: range,
                    order,
                });
                order += 1;
            }
            if !parsed.is_empty() {
                stylesheet.rules.push(Rule {
                    selector,
                    declarations: parsed,
                });
            }
        }
        let _ = bytes;
        stylesheet
    }

    /// Resolve the declarations that apply to one element, in cascade order.
    ///
    /// One entry per computed longhand, keyed by that longhand's canonical name
    /// — so a `background` shorthand and a `background-color` declaration land on
    /// the same key and only one of them survives. Returns the resolved value
    /// with the provenance of the winning declaration.
    ///
    /// The rank that made each winner is not reported, because this answers for
    /// one sheet and a merge of several cannot rank anything from it — see
    /// [`Stylesheet::resolve_ranked`].
    ///
    /// The renderer merges through [`Stylesheet::resolve_ranked`], so outside the
    /// tests this has no caller: it answers for a *single* sheet, and `visual`
    /// is the only caller that ever needed one answer covering several. Kept as
    /// the unranked entry point rather than deleted, because dropping the rank
    /// is only safe once one declaration has won — the property a single-sheet
    /// caller is relying on.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn resolve(
        &self,
        tag: &str,
        classes: &[String],
        id: Option<&str>,
    ) -> BTreeMap<String, ResolvedValue> {
        self.resolve_ranked(tag, classes, id)
            .into_iter()
            .map(|(property, ranked)| (property, ranked.into_resolved()))
            .collect()
    }

    /// [`Stylesheet::resolve`], keeping each winner's specificity and author
    /// order.
    ///
    /// Author order is only comparable *within* one stylesheet, so a caller that
    /// merges several has nothing to compare unless the specificity travels with
    /// the value. Dropping it here is only correct once exactly one declaration
    /// has won; converting earlier discards the rank while it still decides the
    /// answer.
    pub fn resolve_ranked(
        &self,
        tag: &str,
        classes: &[String],
        id: Option<&str>,
    ) -> BTreeMap<String, RankedValue> {
        self.cascaded(tag, classes, id)
            .into_iter()
            .map(|(property, winner)| {
                let declaration = winner.declaration;
                (
                    property,
                    RankedValue {
                        value: self.substitute_variables(&declaration.value),
                        order: declaration.order,
                        specificity: winner.specificity,
                    },
                )
            })
            .collect()
    }

    /// Every declaration that wins for one computed longhand on one element,
    /// with the rank that made it win.
    ///
    /// Single source of truth for the cascade: [`Stylesheet::resolve`] and
    /// [`Stylesheet::resolve_ranked`] read values out of it, and
    /// [`Stylesheet::declaration_value_range`] reads spans out of it, so the
    /// value a node is drawn with and the authored text an edit would rewrite
    /// cannot disagree.
    fn cascaded<'a>(
        &'a self,
        tag: &str,
        classes: &[String],
        id: Option<&str>,
    ) -> BTreeMap<String, Winner<'a>> {
        // The incumbent's own specificity is carried alongside it. Reusing the
        // candidate rule's specificity for the incumbent would make the
        // comparison depend on the order the rules happen to be visited in.
        let mut winners: BTreeMap<String, Winner<'a>> = BTreeMap::new();
        for rule in &self.rules {
            if !rule.selector.matches(tag, classes, id) {
                continue;
            }
            let specificity = rule.selector.specificity();
            for declaration in &rule.declarations {
                for group in property_groups(&declaration.property) {
                    match winners.get(&group) {
                        Some(current) => {
                            if declaration.rank_for(specificity)
                                > current.declaration.rank_for(current.specificity)
                            {
                                winners.insert(
                                    group,
                                    Winner {
                                        declaration,
                                        specificity,
                                    },
                                );
                            }
                        }
                        None => {
                            winners.insert(
                                group,
                                Winner {
                                    declaration,
                                    specificity,
                                },
                            );
                        }
                    }
                }
            }
        }
        winners
    }

    /// The declaration that currently wins for one property on one element.
    ///
    /// `property` may be spelled either way — asking for `background` and asking
    /// for `background-color` both answer with whichever spelling the cascade
    /// actually chose, and asking for `gap` answers with whichever declaration
    /// won the row axis. That is deliberate: a caller that rewrites a declaration
    /// has to be able to ask for the property it means and get the span that
    /// produced the value being replaced.
    fn winner<'a>(
        &'a self,
        tag: &str,
        classes: &[String],
        id: Option<&str>,
        property: &str,
    ) -> Option<&'a Declaration> {
        let cascaded = self.cascaded(tag, classes, id);
        property_groups(property)
            .into_iter()
            .find_map(|group| cascaded.get(&group).map(|winner| winner.declaration))
    }

    /// The authored span of the declaration that currently wins for `property`
    /// on an element.
    ///
    /// This is what makes CSS ownership checkable: a caller asking "does this
    /// element own `background`?" gets `None` when nothing declared it, and a
    /// byte range into this stylesheet when something did — the span of
    /// whichever spelling won, so it agrees with [`Stylesheet::resolve`].
    pub fn declaration_value_range(
        &self,
        tag: &str,
        classes: &[String],
        id: Option<&str>,
        property: &str,
    ) -> Option<(usize, usize)> {
        self.winner(tag, classes, id, property)
            .map(|declaration| declaration.value_range)
    }

    /// Expand `var(--name)` against the `:root` custom properties.
    ///
    /// One level of substitution only. A cycle or an unknown name leaves the
    /// reference in place rather than resolving it to something invented.
    fn substitute_variables(&self, value: &str) -> String {
        let mut current = value.trim().to_owned();
        for _ in 0..4 {
            let Some(start) = current.find("var(") else {
                return current;
            };
            let Some(end) = current[start..].find(')') else {
                return current;
            };
            let reference = &current[start + 4..start + end];
            let name = reference.trim().strip_prefix("--").unwrap_or("").trim();
            let Some(replacement) = self.custom_properties.get(name) else {
                return current;
            };
            current = format!(
                "{}{}{}",
                &current[..start],
                replacement,
                &current[start + end + 1..]
            );
        }
        current
    }
}

/// A resolved declaration and the authored span it came from.
///
/// The span is a `(start, end)` byte range into the stylesheet the value was
/// read from, which is what a later source-preserving write targets.
///
/// Only [`Stylesheet::resolve`] produces this, and only a single-sheet caller
/// wants it; a merge wants [`RankedValue`].
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct ResolvedValue {
    pub value: String,
    pub order: usize,
}

/// The declaration that currently wins one longhand, with the specificity of the
/// rule it came from.
///
/// Split out from [`Declaration`] because the specificity belongs to the
/// selector: keeping the pair adjacent is what stops a declaration from being
/// ranked by a rule other than its own.
struct Winner<'a> {
    declaration: &'a Declaration,
    specificity: u32,
}

/// A resolved declaration that carries the rank that made it win.
///
/// [`ResolvedValue`] is this without the rank. A caller reading a single
/// stylesheet does not need it; a caller merging stylesheets cannot rank a
/// merge without it, because the rank is exactly what the merge is decided on.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedValue {
    /// The winning value, with `var()` references substituted.
    pub value: String,
    /// Author order of the winning declaration, within its own stylesheet.
    ///
    /// Not comparable across stylesheets on its own: it counts declarations
    /// from this file only.
    pub order: usize,
    /// Specificity of the rule the winning declaration was authored under.
    ///
    /// The criterion a cross-sheet merge has to compare first.
    pub specificity: u32,
}

impl RankedValue {
    /// The same declaration without the rank, for a caller that has finished
    /// merging.
    ///
    /// Dropping the rank is only correct once one declaration has won: there is
    /// then nothing left to rank against, so the fact that decided it is spent.
    #[cfg_attr(not(test), allow(dead_code))]
    fn into_resolved(self) -> ResolvedValue {
        ResolvedValue {
            value: self.value,
            order: self.order,
        }
    }
}

/// Iterate `(body_range, prelude)` for every top-level rule in `source`.
fn rules(source: &str) -> Vec<((usize, usize), &str)> {
    let mut found = Vec::new();
    let bytes = source.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        let Some(open) = source[index..].find('{') else {
            break;
        };
        let open = index + open;
        // Skip at-rules entirely: their bodies are not a flat rule list.
        let line_start = source[..open].rfind('\n').map(|i| i + 1).unwrap_or(0);
        if source[line_start..open].trim_start().starts_with('@') {
            let Some(close) = source[open..].find('}') else {
                break;
            };
            index = open + close + 1;
            continue;
        }
        let Some(close_offset) = source[open..].find('}') else {
            break;
        };
        let close = open + close_offset;
        let prelude = &source[line_start..open];
        found.push(((open + 1, close), prelude));
        index = close + 1;
    }
    found
}

/// Iterate `(property, value, value_range)` inside a rule body.
fn declarations(source: &str, body: (usize, usize)) -> Vec<(String, String, (usize, usize))> {
    let mut found = Vec::new();
    let (body_start, body_end) = body;
    let body = &source[body_start..body_end];
    let base = body_start;
    let mut rest = body;
    while let Some(colon) = rest.find(':') {
        let property = rest[..colon].trim().to_ascii_lowercase();
        rest = &rest[colon + 1..];
        let Some(end) = rest.find(';') else {
            // A final declaration without a trailing semicolon is still valid.
            let value_start = base + (body.len() - rest.len());
            let raw = rest.trim();
            let lead = rest.len() - rest.trim_start().len();
            let start = value_start + lead;
            let end = start + raw.len();
            if !property.is_empty() && !raw.is_empty() {
                found.push((property, raw.to_owned(), (start, end)));
            }
            break;
        };
        let value_start = base + (body.len() - rest.len());
        let raw = rest[..end].trim();
        let lead = rest[..end].len() - rest[..end].trim_start().len();
        let start = value_start + lead;
        let range = (start, start + raw.len());
        rest = &rest[end + 1..];
        if !property.is_empty() && !raw.is_empty() {
            found.push((property, raw.to_owned(), range));
        }
    }
    found
}

/// Parse a CSS colour into the renderer's colour type.
///
/// Only the forms the fixtures use are accepted: `#rgb`, `#rrggbb`, and a few
/// keywords. Anything unrecognised returns `None` so an unsupported colour can
/// never be silently turned into black.
pub fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "white" => return Some(Color::from_rgb(0xffffff)),
        "black" => return Some(Color::from_rgb(0x000000)),
        "transparent" => return None,
        _ => {}
    }
    let hex = value.strip_prefix('#')?;
    let expanded: String = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 => hex.to_owned(),
        _ => return None,
    };
    if !expanded.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(&expanded, 16).ok().map(Color::from_rgb)
}

/// Parse a CSS length into pixels.
///
/// Accepts a bare number as pixels and a `px` suffix. Percentages, `em`, `rem`,
/// and `vh` are out of scope and return `None`.
pub fn parse_length(value: &str) -> Option<f32> {
    let value = value.trim();
    let numeric = value.strip_suffix("px").unwrap_or(value);
    numeric.trim().parse::<f32>().ok()
}

/// Parse a unitless CSS number, such as an `opacity` or a unitless `line-height`.
///
/// Distinct from [`parse_length`], which accepts the same syntax plus `px`:
/// accepting `px` here would let `opacity: 0.5px` through, which is not a
/// number CSS would accept.
pub fn parse_number(value: &str) -> Option<f32> {
    let value = value.trim();
    // A percentage is a different unit of opacity, and this subset does not
    // resolve it. Refusing it is better than reading `50%` as 50.
    if value.ends_with('%') {
        return None;
    }
    value.parse::<f32>().ok()
}

/// Parse `transform: translate(<x>, <y>)` into a pixel offset.
///
/// Only the `translate` function, and only a two-argument pixel one, is
/// understood. Every other transform — rotate, scale, a matrix, a percentage —
/// returns `None` rather than being approximated, because guessing at a
/// transform would move an element somewhere the author never asked for.
///
/// This is what lets a moved element keep its place in the flow instead of
/// being lifted out of it.
pub fn parse_translate(value: &str) -> Option<(f32, f32)> {
    let value = value.trim();
    let inner = value.strip_prefix("translate(")?.strip_suffix(')')?;
    let mut parts = inner.split(',').map(str::trim);
    let x = parse_length(parts.next()?)?;
    let y = parse_length(parts.next()?)?;
    // A third component would be a translateZ this subset does not model.
    if parts.next().is_some() {
        return None;
    }
    Some((x, y))
}

/// A parsed `border` shorthand: a width for every side, and a colour if one
/// was authored.
///
/// Only the uniform form is understood. `border: 1px solid red`, `border: 1px
/// red`, and `border: red` all parse; a per-side shorthand such as
/// `border-left`, a `style` keyword that is not `solid`, and a width in a unit
/// this subset cannot read all return `None` rather than being approximated.
///
/// The width is geometry and the colour is paint, and they are returned
/// separately on purpose: `border: 1px solid` occupies a box with no authored
/// colour, so treating the two as one either invents a stroke or silently
/// drops the border from the layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderStyle {
    pub width: f32,
    pub color: Option<Color>,
}

/// Parse the `border` shorthand.
pub fn parse_border(value: &str) -> Option<BorderStyle> {
    let mut width = 1.0f32;
    let mut color = None;
    let mut components = 0usize;
    for part in value.split_whitespace() {
        if part.eq_ignore_ascii_case("none") {
            // `border: none` is an explicit statement that there is no border.
            return (components == 0).then_some(BorderStyle {
                width: 0.0,
                color: None,
            });
        }
        if part.eq_ignore_ascii_case("solid") {
            // The line style carries no geometry this subset can draw, but it
            // is a real keyword in the shorthand rather than a parse error.
            components += 1;
            continue;
        }
        if let Some(parsed) = parse_length(part) {
            width = parsed;
        } else if let Some(parsed) = parse_color(part) {
            color = Some(parsed);
        } else {
            return None;
        }
        components += 1;
    }
    (components > 0).then_some(BorderStyle { width, color })
}

/// Parse a one-to-four component box value such as `12px 20px` or `0`.
pub fn parse_box(value: &str) -> Option<[f32; 4]> {
    let parts: Vec<f32> = value
        .split_whitespace()
        .map(parse_length)
        .collect::<Option<Vec<_>>>()?;
    match parts.len() {
        1 => Some([parts[0]; 4]),
        2 => Some([parts[0], parts[1], parts[0], parts[1]]),
        4 => Some([parts[0], parts[1], parts[2], parts[3]]),
        _ => None,
    }
}

/// Normalise an authored keyword: CSS keywords are ASCII case-insensitive, and
/// the cascade hands back the value exactly as it was written.
// Landed with the parsing half; the auto-layout mapping is not built yet.
#[allow(dead_code)]
fn keyword(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

/// The main axis a flex container lays its children out along.
///
/// Each variant is one CSS keyword, so a value that reads back is the value
/// that was authored. Nothing here claims how a frame uses the axis; that is
/// the engine's auto-layout, and it is not CSS flexbox.
// Landed with the parsing half; the auto-layout mapping is not built yet.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

/// Parse a `flex-direction` keyword.
///
/// `normal` is refused rather than folded into `row`: it is not one of the four
/// keywords this subset supports, and choosing an axis the author did not name
/// is the failure mode this module exists to avoid.
// Landed with the parsing half; the auto-layout mapping is not built yet.
#[allow(dead_code)]
pub fn parse_flex_direction(value: &str) -> Option<FlexDirection> {
    match keyword(value).as_str() {
        "row" => Some(FlexDirection::Row),
        "row-reverse" => Some(FlexDirection::RowReverse),
        "column" => Some(FlexDirection::Column),
        "column-reverse" => Some(FlexDirection::ColumnReverse),
        _ => None,
    }
}

/// Whether a flex container keeps its children on one line or breaks them onto
/// new ones.
// Landed with the parsing half; the auto-layout mapping is not built yet.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexWrap {
    NoWrap,
    Wrap,
    WrapReverse,
}

/// Parse a `flex-wrap` keyword.
// Landed with the parsing half; the auto-layout mapping is not built yet.
#[allow(dead_code)]
pub fn parse_flex_wrap(value: &str) -> Option<FlexWrap> {
    match keyword(value).as_str() {
        "nowrap" => Some(FlexWrap::NoWrap),
        "wrap" => Some(FlexWrap::Wrap),
        "wrap-reverse" => Some(FlexWrap::WrapReverse),
        _ => None,
    }
}

/// How a flex container distributes free space along its main axis.
///
/// `start` and `end` are the CSS `flex-start` and `flex-end` under the spelling
/// this subset supports. The `flex-` prefixed keywords are deliberately not
/// accepted as aliases: they mean the same thing here, and carrying two
/// spellings for one value would make a write-back able to change the author's
/// words.
// Landed with the parsing half; the auto-layout mapping is not built yet.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JustifyContent {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// Parse a `justify-content` keyword.
// Landed with the parsing half; the auto-layout mapping is not built yet.
#[allow(dead_code)]
pub fn parse_justify_content(value: &str) -> Option<JustifyContent> {
    match keyword(value).as_str() {
        "start" => Some(JustifyContent::Start),
        "end" => Some(JustifyContent::End),
        "center" => Some(JustifyContent::Center),
        "space-between" => Some(JustifyContent::SpaceBetween),
        "space-around" => Some(JustifyContent::SpaceAround),
        "space-evenly" => Some(JustifyContent::SpaceEvenly),
        _ => None,
    }
}

/// How a flex container aligns its children on the cross axis.
// Landed with the parsing half; the auto-layout mapping is not built yet.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignItems {
    Start,
    End,
    Center,
    Baseline,
    Stretch,
}

/// Parse an `align-items` keyword.
///
/// As with [`JustifyContent`], the `flex-` prefixed spellings are refused
/// rather than aliased, and `normal` is refused rather than read as `stretch`.
// Landed with the parsing half; the auto-layout mapping is not built yet.
#[allow(dead_code)]
pub fn parse_align_items(value: &str) -> Option<AlignItems> {
    match keyword(value).as_str() {
        "start" => Some(AlignItems::Start),
        "end" => Some(AlignItems::End),
        "center" => Some(AlignItems::Center),
        "baseline" => Some(AlignItems::Baseline),
        "stretch" => Some(AlignItems::Stretch),
        _ => None,
    }
}

/// Parse a `gap`, `row-gap`, or `column-gap` value into pixels.
///
/// All three take one length, so they share a parser: `gap: 10px` and `gap: 10`
/// are the same ten pixels, exactly as [`parse_length`] reads a bare number as
/// pixels.
///
/// The two-value `gap: 10px 20px` form is refused rather than half-read — it
/// carries a row and a column, and taking the first would silently drop the
/// second. A negative gap is invalid CSS, so it is refused too rather than
/// turned into an overlap the author never asked for.
// Landed with the parsing half; the auto-layout mapping is not built yet.
#[allow(dead_code)]
pub fn parse_gap(value: &str) -> Option<f32> {
    let parsed = parse_length(value)?;
    (parsed >= 0.0).then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved_map(source: &str, tag: &str, classes: &[&str]) -> BTreeMap<String, String> {
        resolved_for(source, tag, classes, None)
    }

    fn resolved_for(
        source: &str,
        tag: &str,
        classes: &[&str],
        id: Option<&str>,
    ) -> BTreeMap<String, String> {
        let sheet = Stylesheet::parse(source);
        sheet
            .resolve(
                tag,
                &classes.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
                id,
            )
            .into_iter()
            .map(|(property, resolved)| (property, resolved.value))
            .collect()
    }

    #[test]
    fn a_class_rule_wins_over_a_tag_rule_for_the_same_property() {
        let source = "\
a { background-color: #ffffff; padding: 4px; }
.cta { background-color: #3b5bfd; }
";
        let resolved = resolved_map(source, "a", &["cta"]);
        assert_eq!(
            resolved.get("background-color").map(String::as_str),
            Some("#3b5bfd"),
            "the later class rule overrides the tag rule"
        );
        assert_eq!(
            resolved.get("padding").map(String::as_str),
            Some("4px"),
            "a property the class rule does not mention is kept from the tag rule"
        );
    }

    #[test]
    fn specificity_outranks_author_order() {
        // The defect this exists for: author order was the *only* ranking, so a
        // later bare-tag rule beat an earlier class rule. In CSS a class always
        // beats a type selector no matter which came last.
        let source = ".cta { color: #ff0000; }\na { color: #0000ff; }\n";
        assert_eq!(
            resolved_map(source, "a", &["cta"])
                .get("color")
                .map(String::as_str),
            Some("#ff0000"),
            "the earlier class rule still wins: specificity is checked first"
        );
        assert_eq!(
            resolved_map(source, "a", &[])
                .get("color")
                .map(String::as_str),
            Some("#0000ff"),
            "and an element with no class still gets the tag rule's value"
        );
    }

    #[test]
    fn an_id_rule_outranks_a_class_rule_whatever_the_order() {
        let source = "\
#hero { color: #00ff00; }
.cta { color: #ff0000; }
";
        assert_eq!(
            resolved_for(source, "a", &["cta"], Some("hero"))
                .get("color")
                .map(String::as_str),
            Some("#00ff00"),
            "one id outranks any number of classes"
        );
        assert_eq!(
            resolved_for(source, "a", &["cta"], Some("other"))
                .get("color")
                .map(String::as_str),
            Some("#ff0000"),
            "a different id does not match at all"
        );
    }

    #[test]
    fn a_compound_selector_matches_an_element_that_carries_every_part() {
        // `a.cta` was documented as supported but parsed to nothing, because the
        // parser only accepted `.`/`#` at the front and then refused the tag.
        let source = "a.cta { color: #ff0000; }";
        assert_eq!(
            resolved_map(source, "a", &["cta"])
                .get("color")
                .map(String::as_str),
            Some("#ff0000"),
            "the tag and the class together match"
        );
        assert!(
            !resolved_map(source, "button", &["cta"]).contains_key("color"),
            "the tag still has to match"
        );
        assert!(
            !resolved_map(source, "a", &[]).contains_key("color"),
            "and so does the class"
        );
    }

    #[test]
    fn a_multi_class_selector_needs_all_of_its_classes() {
        let source = ".a.b { color: #ff0000; }";
        assert_eq!(
            resolved_map(source, "div", &["a", "b"])
                .get("color")
                .map(String::as_str),
            Some("#ff0000")
        );
        assert!(
            !resolved_map(source, "div", &["a"]).contains_key("color"),
            "one of the two is not enough, and the rule must not half-apply"
        );
    }

    #[test]
    fn the_background_shorthand_and_its_longhand_are_one_property() {
        // CSS expands a shorthand into longhands before the cascade, so a later
        // `background` beats an earlier `background-color` and vice versa. Both
        // spellings resolving to separate keys meant whichever the consumer
        // preferred won, whatever the author wrote last.
        let shorthand_last = resolved_map(
            "a { background-color: #ff0000; background: #0000ff; }",
            "a",
            &[],
        );
        assert_eq!(
            shorthand_last.get("background-color").map(String::as_str),
            Some("#0000ff"),
            "the later shorthand wins"
        );
        assert!(
            !shorthand_last.contains_key("background"),
            "and it resolves under its longhand name, not the spelling used"
        );

        let longhand_last = resolved_map(
            "a { background: #0000ff; background-color: #ff0000; }",
            "a",
            &[],
        );
        assert_eq!(
            longhand_last.get("background-color").map(String::as_str),
            Some("#ff0000"),
            "a later longhand beats an earlier shorthand too"
        );
    }

    #[test]
    fn the_winning_spans_belong_to_the_declaration_that_actually_won() {
        // The save layer rewrites the authored span this reports, so it has to be
        // the span of the winning spelling — not of a fixed first choice.
        let source = "a { background-color: #ff0000; background: #0000ff; }";
        let sheet = Stylesheet::parse(source);
        for property in ["background", "background-color"] {
            let range = sheet
                .declaration_value_range("a", &[], None, property)
                .expect("the shorthand owns the longhand either way it is spelled");
            assert_eq!(
                &source[range.0..range.1],
                "#0000ff",
                "asking for `{property}` must still find the declaration that won"
            );
        }
    }

    #[test]
    fn a_border_shorthand_reports_geometry_and_paint_separately() {
        // `border: 1px solid` takes up space with no authored colour. Returning
        // the two as one value would either invent a stroke or drop the border
        // from the layout; reporting them separately lets each consumer take the
        // half it understands.
        assert_eq!(
            parse_border("1px solid #ff0000"),
            Some(BorderStyle {
                width: 1.0,
                color: parse_color("#ff0000"),
            })
        );
        assert_eq!(
            parse_border("2px #ff0000"),
            Some(BorderStyle {
                width: 2.0,
                color: parse_color("#ff0000"),
            }),
            "the `solid` keyword is optional"
        );
        assert_eq!(
            parse_border("#ff0000"),
            Some(BorderStyle {
                width: 1.0,
                color: parse_color("#ff0000"),
            }),
            "and so is the width, which defaults to the CSS initial value"
        );
        assert_eq!(
            parse_border("1px solid"),
            Some(BorderStyle {
                width: 1.0,
                color: None
            }),
            "a border with no colour occupies space and paints nothing"
        );
        assert_eq!(
            parse_border("none"),
            Some(BorderStyle {
                width: 0.0,
                color: None
            }),
            "`border: none` is an explicit zero, not a parse failure"
        );
        for unsupported in [
            "1px dashed #ff0000",
            "2em solid #ff0000",
            "1px solid rgb(1,2,3,4,5)",
        ] {
            assert!(
                parse_border(unsupported).is_none(),
                "`{unsupported}` is outside the supported subset and must not be guessed at"
            );
        }
    }

    #[test]
    fn a_specificity_that_does_not_come_from_the_current_rule_cannot_win() {
        // The comparison has to rank the incumbent by *its own* rule's
        // specificity. Comparing both against the candidate's is order-dependent:
        // an incumbent from a weak rule that happens to be visited while a strong
        // rule is in hand looks strong enough to keep the slot.
        let source = "\
a { padding: 1px; }
.cta { padding: 2px; }
";
        let sheet = Stylesheet::parse(source);
        assert_eq!(
            sheet.resolve("a", &["cta".to_owned()], None)["padding"].value,
            "2px",
            "the class rule wins"
        );
        // Reversing the visit order must not change the answer.
        let reversed = Stylesheet::parse(
            "\
.cta { padding: 2px; }
a { padding: 1px; }
",
        );
        assert_eq!(
            reversed.resolve("a", &["cta".to_owned()], None)["padding"].value,
            "2px",
            "and it still wins when the tag rule is authored last"
        );
    }

    #[test]
    fn a_declaration_on_an_ancestor_selector_does_not_apply_to_a_child() {
        // No inheritance and no descendant matching: this module answers "which
        // declarations match this element", not "what does this element end up
        // looking like". Property inheritance for text is handled one layer up,
        // in `visual`, where the tree is available.
        let source = "body { color: #16161d; }";
        let resolved = resolved_map(source, "h1", &[]);
        assert!(
            resolved.is_empty(),
            "`body` styles the body, not the headline inside it"
        );
    }

    #[test]
    fn an_unsupported_property_is_skipped_rather_than_approximated() {
        // `text-decoration` is real CSS. This milestone does not implement it,
        // and silently approximating it would produce a visual nobody
        // authored. It must simply not resolve.
        let source = ".cta { text-decoration: none; color: #ffffff; }";
        let resolved = resolved_map(source, "a", &["cta"]);
        assert!(!resolved.contains_key("text-decoration"));
        assert_eq!(resolved.get("color").map(String::as_str), Some("#ffffff"));
    }

    #[test]
    fn a_supported_property_in_an_unreadable_form_is_not_approximated() {
        // `transform` is a supported property, but only the two-argument pixel
        // `translate` is understood. A rotation is carried through resolution
        // as authored text — it is the consumer that must decline it, because
        // reading `rotate(4deg)` as a translate would move an element
        // somewhere the author never asked for.
        let source = ".cta { transform: rotate(4deg); }";
        let resolved = resolved_map(source, "a", &["cta"]);
        assert_eq!(
            resolved.get("transform").map(String::as_str),
            Some("rotate(4deg)"),
            "resolution keeps the authored value; it does not drop or rewrite it"
        );
        assert_eq!(parse_translate("rotate(4deg)"), None);
    }

    #[test]
    fn a_two_argument_pixel_translate_is_read_as_an_offset() {
        assert_eq!(parse_translate("translate(12px, -4px)"), Some((12.0, -4.0)));
        assert_eq!(parse_translate("  translate( 8 , 2 )  "), Some((8.0, 2.0)));
    }

    #[test]
    fn a_translate_the_subset_cannot_read_is_refused() {
        // A third component is a translateZ, a percentage needs a containing
        // block, and a named function is not an offset. None of them is `None`
        // of a single-argument translate; all are "not understood".
        for value in [
            "translate(4px, 8px, 9px)",
            "translate(50%, 50%)",
            "translateX(12px)",
            "scale(2)",
            "translate(4px)",
            "none",
        ] {
            assert_eq!(parse_translate(value), None, "{value} is not an offset");
        }
    }

    #[test]
    fn a_combinator_selector_is_out_of_scope_and_skipped() {
        // `.card .cta` needs a descendant combinator, which this cascade does
        // not implement. It must not be treated as if it were `.cta`.
        let source = ".card .cta { color: #ff0000; }";
        let sheet = Stylesheet::parse(source);
        let matched = sheet.resolve("a", &["cta".to_string()], None);
        assert!(
            matched.is_empty(),
            "a descendant combinator must not silently apply to the child"
        );
    }

    #[test]
    fn an_at_rule_is_skipped_instead_of_being_read_as_a_flat_rule_list() {
        let source = "\
@media (min-width: 800px) { .cta { color: #ff0000; } }
.cta { color: #0000ff; }
";
        let resolved = resolved_map(source, "a", &["cta"]);
        assert_eq!(
            resolved.get("color").map(String::as_str),
            Some("#0000ff"),
            "the media query body is not a top-level rule"
        );
    }

    #[test]
    fn custom_properties_substitute_into_resolved_values() {
        let source = "\
:root { --accent: #3b5bfd; }
.cta { background: var(--accent); }
";
        let resolved = resolved_map(source, "a", &["cta"]);
        assert_eq!(
            resolved.get("background-color").map(String::as_str),
            Some("#3b5bfd"),
            "the fixture's variable resolves to its authored value"
        );
    }

    #[test]
    fn an_unknown_variable_is_left_unresolved_rather_than_invented() {
        // Substituting a default would fabricate a colour the author never
        // wrote. Leaving the reference in place means the declaration is
        // visibly unusable instead of silently wrong.
        let source = ".cta { background: var(--nope); }";
        let resolved = resolved_map(source, "a", &["cta"]);
        assert_eq!(
            resolved.get("background-color").map(String::as_str),
            Some("var(--nope)")
        );
        assert!(
            parse_color("var(--nope)").is_none(),
            "and the unresolved reference cannot be read as a colour"
        );
    }

    #[test]
    fn every_resolved_declaration_points_at_its_authored_value() {
        // Provenance is what lets a later edit rewrite one declaration instead of
        // regenerating the file. A range that does not slice back to the value
        // would make source-preserving saves rewrite the wrong bytes.
        let source = "\
:root { --ink: #16161d; }

.cta {
  background: #3b5bfd;
  color: #ffffff;
}
";
        let sheet = Stylesheet::parse(source);
        let range = |property: &str| -> (usize, usize) {
            sheet
                .rules
                .iter()
                .flat_map(|rule| rule.declarations.iter())
                .find(|declaration| declaration.property == property)
                .map(|declaration| declaration.value_range)
                .unwrap_or_else(|| panic!("{property} resolved"))
        };
        assert_eq!(
            &source[range("background").0..range("background").1],
            "#3b5bfd"
        );
        assert_eq!(&source[range("color").0..range("color").1], "#ffffff");
    }

    #[test]
    fn the_final_declaration_without_a_semicolon_is_still_read() {
        let resolved = resolved_map(".cta { color: #ffffff }", "a", &["cta"]);
        assert_eq!(resolved.get("color").map(String::as_str), Some("#ffffff"));
    }

    #[test]
    fn colour_parsing_stays_inside_the_supported_forms() {
        assert_eq!(
            parse_color("#3b5bfd").map(|c| (c.red, c.green, c.blue)),
            Some((0x3b, 0x5b, 0xfd))
        );
        assert_eq!(
            parse_color("#fff").map(|c| (c.red, c.green, c.blue)),
            Some((255, 255, 255)),
            "three-digit hex expands"
        );
        assert_eq!(
            parse_color("WHITE").map(|c| (c.red, c.green, c.blue)),
            Some((255, 255, 255)),
            "keywords are case-insensitive"
        );
        for unsupported in [
            "rgb(1,2,3)",
            "tomato",
            "transparent",
            "#12345",
            "hsl(0 0% 0%)",
        ] {
            assert!(
                parse_color(unsupported).is_none(),
                "{unsupported} is outside the supported subset and must not be guessed at"
            );
        }
    }

    #[test]
    fn length_parsing_accepts_pixels_and_rejects_relative_units() {
        assert_eq!(parse_length("12px"), Some(12.0));
        assert_eq!(parse_length(" 0 "), Some(0.0));
        for unsupported in ["50%", "2em", "3rem", "10vh", "auto", ""] {
            assert!(
                parse_length(unsupported).is_none(),
                "{unsupported} needs a real layout engine"
            );
        }
    }

    #[test]
    fn box_parsing_handles_one_two_and_four_components() {
        assert_eq!(parse_box("4px"), Some([4.0; 4]));
        assert_eq!(parse_box("12px 20px"), Some([12.0, 20.0, 12.0, 20.0]));
        assert_eq!(parse_box("1px 2px 3px 4px"), Some([1.0, 2.0, 3.0, 4.0]));
        assert!(parse_box("1px 2px 3px").is_none());
        assert!(parse_box("auto").is_none());
    }

    /// The authored bytes the cascade says won `property`, sliced back out of
    /// the stylesheet it was parsed from.
    fn winning_text<'a>(
        source: &'a str,
        sheet: &Stylesheet,
        tag: &str,
        classes: &[&str],
        property: &str,
    ) -> &'a str {
        let range = sheet
            .declaration_value_range(
                tag,
                &classes.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
                None,
                property,
            )
            .unwrap_or_else(|| panic!("{property} has a winning declaration"));
        &source[range.0..range.1]
    }

    #[test]
    fn every_flex_direction_keyword_is_read() {
        for (value, expected) in [
            ("row", FlexDirection::Row),
            ("row-reverse", FlexDirection::RowReverse),
            ("column", FlexDirection::Column),
            ("column-reverse", FlexDirection::ColumnReverse),
            ("  COLUMN-REVERSE  ", FlexDirection::ColumnReverse),
        ] {
            assert_eq!(parse_flex_direction(value), Some(expected), "{value}");
        }
        for unsupported in ["normal", "diagonal", "reverse", "row reverse", ""] {
            assert_eq!(
                parse_flex_direction(unsupported),
                None,
                "{unsupported} is not one of the four supported axes"
            );
        }
    }

    #[test]
    fn every_flex_wrap_keyword_is_read() {
        for (value, expected) in [
            ("nowrap", FlexWrap::NoWrap),
            ("wrap", FlexWrap::Wrap),
            ("wrap-reverse", FlexWrap::WrapReverse),
            (" WRAP ", FlexWrap::Wrap),
        ] {
            assert_eq!(parse_flex_wrap(value), Some(expected), "{value}");
        }
        for unsupported in ["no-wrap", "reverse", "wrap around", ""] {
            assert_eq!(
                parse_flex_wrap(unsupported),
                None,
                "{unsupported} is not one of the three supported wrap keywords"
            );
        }
    }

    #[test]
    fn every_justify_content_keyword_is_read() {
        for (value, expected) in [
            ("start", JustifyContent::Start),
            ("end", JustifyContent::End),
            ("center", JustifyContent::Center),
            ("space-between", JustifyContent::SpaceBetween),
            ("space-around", JustifyContent::SpaceAround),
            ("space-evenly", JustifyContent::SpaceEvenly),
            ("  SPACE-BETWEEN ", JustifyContent::SpaceBetween),
        ] {
            assert_eq!(parse_justify_content(value), Some(expected), "{value}");
        }
        for unsupported in [
            // The `flex-` prefixed spellings are not aliased onto `start`/`end`:
            // one value gets one spelling, so a write-back cannot reword it.
            "flex-start",
            "flex-end",
            "stretch",
            "space",
            "left",
            "right",
            "",
        ] {
            assert_eq!(
                parse_justify_content(unsupported),
                None,
                "{unsupported} is outside the supported subset"
            );
        }
    }

    #[test]
    fn every_align_items_keyword_is_read() {
        for (value, expected) in [
            ("start", AlignItems::Start),
            ("end", AlignItems::End),
            ("center", AlignItems::Center),
            ("baseline", AlignItems::Baseline),
            ("stretch", AlignItems::Stretch),
            ("  BASELINE ", AlignItems::Baseline),
        ] {
            assert_eq!(parse_align_items(value), Some(expected), "{value}");
        }
        for unsupported in [
            "flex-start",
            "flex-end",
            "normal",
            "first baseline",
            "self",
            "",
        ] {
            assert_eq!(
                parse_align_items(unsupported),
                None,
                "{unsupported} is outside the supported subset"
            );
        }
    }

    #[test]
    fn a_gap_is_one_length_in_either_spelling() {
        // `gap: 10` is ten pixels, the same as `gap: 10px`, for the same reason
        // `parse_length` reads a bare number as pixels everywhere else.
        assert_eq!(parse_gap("10px"), Some(10.0));
        assert_eq!(parse_gap("10"), Some(10.0));
        assert_eq!(parse_gap("  0  "), Some(0.0));
        for unsupported in [
            "2em",
            "5%",
            "auto",
            "-4px",
            "10px 20px",
            "calc(2 * 4px)",
            "",
        ] {
            assert_eq!(
                parse_gap(unsupported),
                None,
                "{unsupported} is not a gap this subset can read"
            );
        }
    }

    #[test]
    fn a_gap_shorthand_fills_both_axes_and_a_row_gap_overrides_only_the_row() {
        // CSS expands `gap` into `row-gap` and `column-gap` before the cascade,
        // so a later `row-gap` overrides the row the shorthand set and leaves
        // the column alone. Resolving `gap` to one key would make the override
        // silently take the column with it.
        let source = "\
a { gap: 10px; row-gap: 4px; }
";
        let resolved = resolved_map(source, "a", &[]);
        assert_eq!(resolved.get("row-gap").map(String::as_str), Some("4px"));
        assert_eq!(
            resolved.get("column-gap").map(String::as_str),
            Some("10px"),
            "the shorthand's column survives the row override"
        );
        assert!(
            !resolved.contains_key("gap"),
            "the shorthand resolves under its longhand names, not the spelling used"
        );

        // Author order decides when specificity does not: written last, the
        // shorthand takes the row axis back.
        let reversed = resolved_map("a { row-gap: 4px; gap: 10px; }", "a", &[]);
        assert_eq!(reversed.get("row-gap").map(String::as_str), Some("10px"));
        assert_eq!(reversed.get("column-gap").map(String::as_str), Some("10px"));

        // A unitless gap is the same gap, through the cascade as well.
        assert_eq!(
            resolved_map("a { gap: 10; }", "a", &[])
                .get("row-gap")
                .map(String::as_str),
            Some("10")
        );
    }

    #[test]
    fn the_flex_longhands_cascade_like_every_other_property() {
        // Nothing here is special-cased: the same specificity-then-order ranking
        // decides these declarations that decides `color`, and a property only
        // the weaker rule mentions is still kept.
        let source = "\
a { flex-direction: column; gap: 10px; }
.cta { flex-direction: row; }
";
        let resolved = resolved_map(source, "a", &["cta"]);
        assert_eq!(
            resolved.get("flex-direction").map(String::as_str),
            Some("row"),
            "the later class rule wins on specificity"
        );
        assert_eq!(
            resolved.get("column-gap").map(String::as_str),
            Some("10px"),
            "a longhand only the tag rule mentions is kept from it"
        );
        assert_eq!(
            resolved.get("justify-content"),
            None,
            "an undeclared property resolves to nothing rather than an invented default"
        );
        assert!(
            resolved_map(source, "a", &[]).get("flex-direction") == Some(&"column".to_owned()),
            "and an element without the class still gets the tag rule's value"
        );
    }

    #[test]
    fn an_unreadable_flex_value_falls_back_instead_of_being_guessed_at() {
        // Resolution carries the authored text through unchanged, and the
        // consumer parser declines it. That is the same contract `transform`
        // already has: read what was written, refuse what cannot be understood,
        // and let the CSS initial value stand rather than inventing one.
        let source = "\
.cta {
  flex-direction: diagonal;
  flex-wrap: wrap-around;
  justify-content: stretch;
  align-items: flex-start;
  gap: 2em;
}
";
        let resolved = resolved_map(source, "a", &["cta"]);
        assert_eq!(
            resolved.get("flex-direction").map(String::as_str),
            Some("diagonal")
        );
        assert_eq!(parse_flex_direction("diagonal"), None);
        assert_eq!(parse_flex_wrap("wrap-around"), None);
        assert_eq!(parse_justify_content("stretch"), None);
        assert_eq!(parse_align_items("flex-start"), None);
        assert_eq!(parse_gap("2em"), None);

        let sheet = Stylesheet::parse(source);
        assert!(
            sheet
                .declaration_value_range("a", &["cta".to_owned()], None, "align-content")
                .is_none(),
            "a property nothing declared has no span to report"
        );
    }

    #[test]
    fn every_flex_longhand_reports_the_span_of_the_declaration_that_won_it() {
        // The save layer rewrites exactly the bytes this reports, so each
        // property has to answer with its own authored value — including `gap`,
        // which answers with the row it won, and `column-gap`, which answers
        // with the longhand rather than the shorthand that shares its axis.
        let source = "\
.row {
  flex-direction: column-reverse;
  flex-wrap: wrap-reverse;
  gap: 12px;
  row-gap: 3px;
  column-gap: 8px;
  justify-content: space-between;
  align-items: baseline;
}
";
        let sheet = Stylesheet::parse(source);
        for (property, expected) in [
            ("flex-direction", "column-reverse"),
            ("flex-wrap", "wrap-reverse"),
            ("gap", "3px"),
            ("row-gap", "3px"),
            ("column-gap", "8px"),
            ("justify-content", "space-between"),
            ("align-items", "baseline"),
        ] {
            assert_eq!(
                winning_text(source, &sheet, "div", &["row"], property),
                expected,
                "{property} must point at the declaration that produced it"
            );
        }
        assert!(
            !resolved_map(source, "div", &["row"]).contains_key("flex"),
            "`flex` itself is still unsupported: it has no place to live"
        );
    }

    #[test]
    fn a_flex_winner_in_a_later_stylesheet_owns_the_span_it_was_authored_in() {
        // Stylesheets are resolved in document order by the caller, so each one
        // reports its own bytes. Asking the later sheet for the row axis has to
        // find its own declaration and not reach back into the earlier file's
        // `gap`, and the earlier file still answers for the values it authored.
        let base = ".row { gap: 10px; justify-content: center; }";
        let later = ".row { row-gap: 4px; justify-content: space-between; }";
        let base_sheet = Stylesheet::parse(base);
        let later_sheet = Stylesheet::parse(later);
        for (property, expected) in [
            ("gap", "4px"),
            ("row-gap", "4px"),
            ("justify-content", "space-between"),
        ] {
            assert_eq!(
                winning_text(later, &later_sheet, "div", &["row"], property),
                expected,
                "the later stylesheet owns {property}"
            );
        }
        assert_eq!(
            winning_text(base, &base_sheet, "div", &["row"], "gap"),
            "10px",
            "and the earlier stylesheet still reports its own shorthand"
        );
        assert_eq!(
            winning_text(base, &base_sheet, "div", &["row"], "column-gap"),
            "10px",
            "which is also the column the later stylesheet never mentions"
        );
        assert!(
            later_sheet
                .declaration_value_range("div", &["row".to_owned()], None, "column-gap")
                .is_none(),
            "the later stylesheet declares no column gap, so it owns no span for one"
        );
    }
}
