//! Authored CSS, read as a cascade that reports where each value was written.
//!
//! # Scope, deliberately
//!
//! This is not a CSS engine. It resolves a small, explicitly enumerated set of
//! declarations — the ones in [`SUPPORTED_PROPERTIES`] — for one element at a
//! time, and reports the authored byte range of every declaration it resolves.
//!
//! Supported selectors are a single compound selector of tag name, `.class`,
//! `#id`, or a combination of those on one element: `a`, `.cta`, `#hero`,
//! `a.cta`, `.a.b`. No combinators, no pseudo-classes, no selector lists, no
//! media queries, no `@import`. Anything outside that is skipped rather than
//! approximated, which is what keeps a typo in a stylesheet from becoming a
//! value the author never asked for.
//!
//! # Why the ranges are the point
//!
//! Every resolved declaration carries the byte range of its authored *value*
//! text. That is what lets a later write splice an edit into the exact span the
//! author typed instead of regenerating a stylesheet — and it is why the range
//! is computed from byte offsets during the scan rather than reconstructed from
//! the value afterwards. A range that does not slice back to the value it
//! reported would let a save rewrite the wrong bytes in a person's file.
//!
//! [`Stylesheet::resolve`] substitutes `var()` references into the value, so a
//! resolved value is not always byte-identical to the span it came from. The
//! span always is: it points at what is written in the file. Consumers must
//! therefore treat the two as separate facts — `value` is what the element is
//! styled with, `range` is what a writer may overwrite.
//!
//! # Cascade
//!
//! Two declarations that both match an element compete on specificity first
//! (id, then class, then tag — see [`Selector::specificity`]) and author order
//! second. That is the CSS cascade for the selector subset this parser accepts.
//! Origin (an inline `style` attribute), `!important`, and inherited values are
//! *not* part of it: an inline style belongs to the element reader, and
//! inheritance belongs to the document tree rather than to a stylesheet.
//!
//! `!important` is reported rather than ordered — see
//! [`Stylesheet::important_properties`]. It is reported *before* the cascade
//! picks a winner, because the interesting case is a declaration that loses:
//! once [`Stylesheet::resolve`] has chosen, a discarded `!important` cannot be
//! seen at all, and the caller is left believing nothing was ambiguous.
//!
//! Shorthands are expanded into the longhands they compete with *before* the
//! cascade runs, exactly as CSS does, which is why a later `background` beats
//! an earlier `background-color`. Keeping both under one group also keeps the
//! winning declaration's span pointing at the shorthand the author really
//! wrote.
//!
//! # Reading order
//!
//! Each [`Stylesheet`] owns the bytes of exactly one source file and reports
//! ranges into that file. A document that links several stylesheets resolves
//! them in document order and asks each one in turn; nothing here reaches
//! across from one stylesheet into another, because an offset into a different
//! file is not a range.
//!
//! Asking each sheet in turn is not the same as letting the last sheet win.
//! Link order decides only between declarations of *equal* specificity, so the
//! ranking has to outlive [`Stylesheet::resolve`] for a caller merging several
//! sheets — see [`Stylesheet::resolve_ranked`]. An earlier `#hero` and a later
//! `.theme` are not tied, and a merge that keeps only the values has already
//! lost the fact that made the first one win.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// The properties this module understands.
///
/// Anything else is ignored rather than guessed at, so an unsupported
/// declaration can never masquerade as a supported one.
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

/// How many passes `var()` substitution is allowed.
///
/// Bounded rather than recursive: a custom property whose value refers back to
/// itself — or to a name that never resolves — must terminate instead of
/// looping. Four passes is enough for the authored shapes (a variable inside a
/// variable) while still refusing to chase an unbounded chain.
pub const VARIABLE_SUBSTITUTION_PASSES: usize = 4;

/// One declaration, with the authored span it came from.
///
/// `value` and `range` are two views of the same authored bytes: slicing the
/// original stylesheet at `range` yields exactly `value`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    /// Lower-cased property name, as authored.
    pub property: String,
    /// The authored value text, trimmed of surrounding whitespace and comments.
    pub value: String,
    /// Byte range of `value` inside the stylesheet it was read from.
    pub range: Range<usize>,
    /// Author order, used as the tie-breaker between equal specificity.
    pub order: usize,
    /// Whether the author wrote `!important` on this declaration.
    ///
    /// Recorded from the authored text rather than re-read from `value` later,
    /// because `value` is also where a substituted `var()` result can differ
    /// from what was written. The cascade does not order it — see
    /// [`Stylesheet::important_properties`] — so this is a fact reported to the
    /// caller, not a rank used to pick a winner.
    pub important: bool,
}

impl Declaration {
    /// This declaration's rank, for comparing against one from another rule.
    ///
    /// `specificity` is supplied by the owning rule, which is where the
    /// selector — and therefore the specificity — actually lives. Taken as a
    /// pair rather than a single number so the two ranking criteria stay
    /// separable, and so a declaration cannot disagree with its own rule about
    /// why it won.
    fn rank_for(&self, specificity: u32) -> (u32, usize) {
        (specificity, self.order)
    }
}

/// The canonical longhands a supported declaration competes under.
///
/// CSS expands a shorthand into longhands before the cascade runs, which is why
/// a later `background: blue` overrides an earlier `background-color: red`.
/// Keying both under one group is what makes author order mean the same thing
/// here as in a browser, and it keeps the winning declaration's authored span
/// pointing at the shorthand the author really wrote.
///
/// `gap` is the one shorthand that expands into *two* longhands, because it
/// carries both axes and a `row-gap` or `column-gap` written after it overrides
/// exactly one of them. Its row comes first, so a caller asking for `gap` is
/// answered with the declaration that won the row axis — the same "whichever
/// spelling won" answer `background` gets. A writer that needs both axes
/// rewrites them one longhand at a time.
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

/// A single compound selector: `a`, `.cta`, `#hero`, `a.cta`, or `.a.b`.
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
    /// Parse one compound selector, or `None` for anything outside the subset.
    ///
    /// A selector containing whitespace names a combinator, which this cascade
    /// does not implement; it is refused rather than treated as if the later
    /// part were the whole selector, because applying `.card .cta` to every
    /// `.cta` would style elements the author never pointed at.
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
            // A bare `.` or `#` — and any pseudo-class or attribute selector —
            // names nothing this parser can match.
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
                // A type selector may appear once, in any position: `a.b`,
                // `.b a` and `a` are all one element. Two of them are not.
                if selector.tag.is_some() {
                    return None;
                }
                // Tag names are ASCII case-insensitive in HTML; author the
                // stylesheet in any case and it means the same element.
                selector.tag = Some(name.to_ascii_lowercase());
            }
            rest = &body[end..];
        }
        (selector.tag.is_some() || !selector.classes.is_empty() || selector.id.is_some())
            .then_some(selector)
    }

    fn matches(&self, tag: &str, classes: &[String], id: Option<&str>) -> bool {
        if let Some(expected) = &self.tag {
            if !expected.eq_ignore_ascii_case(tag) {
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
    /// Parse a stylesheet.
    ///
    /// Never fails and never panics: a rule that cannot be understood is
    /// skipped, which matches how the source binder treats malformed markup. An
    /// unterminated comment, quote, or block ends the scan instead of being
    /// completed by guesswork.
    pub fn parse(source: &str) -> Self {
        let mut stylesheet = Self::default();
        let mut order = 0usize;

        for raw in scan_rules(source) {
            let text = strip_comments(&source[raw.prelude]).trim().to_owned();
            // `:root` only contributes custom properties; it styles nothing.
            if text.eq_ignore_ascii_case(":root") {
                for (property, range) in scan_declarations(source, raw.body) {
                    if let Some(name) = property.strip_prefix("--") {
                        stylesheet
                            .custom_properties
                            .insert(name.to_owned(), source[range].to_owned());
                    }
                }
                continue;
            }
            let Some(selector) = Selector::parse(&text) else {
                continue;
            };
            let mut parsed = Vec::new();
            for (property, range) in scan_declarations(source, raw.body) {
                if !SUPPORTED_PROPERTIES.contains(&property.as_str()) {
                    continue;
                }
                parsed.push(Declaration {
                    value: source[range.clone()].to_owned(),
                    property,
                    important: carries_important(&source[range.clone()]),
                    range,
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
        stylesheet
    }

    /// Resolve the declarations that apply to one element.
    ///
    /// One entry per computed longhand, keyed by that longhand's canonical name
    /// — so a `background` shorthand and a `background-color` declaration land
    /// on the same key and only one of them survives, and `gap` lands on
    /// `row-gap` and `column-gap`. Each entry carries the resolved value
    /// (with `var()` substituted) and the authored byte range of the declaration
    /// that won.
    ///
    /// The rank that made the winner is not reported, because every caller of
    /// this wants one element's answer and not a merge. A caller combining
    /// several stylesheets wants the rank too, and loses the cascade if it asks
    /// this — see [`Stylesheet::resolve_ranked`].
    pub fn resolve(&self, tag: &str, classes: &[String], id: Option<&str>) -> Resolved {
        self.resolve_ranked(tag, classes, id)
            .into_iter()
            .map(|(property, ranked)| (property, ranked.into_resolved()))
            .collect()
    }

    /// [`Stylesheet::resolve`], keeping each winner's specificity and author
    /// order.
    ///
    /// Merging several stylesheets is not "take the last sheet that has a
    /// value": the cascade ranks every matching declaration against every other
    /// one, and only declarations of equal rank are decided by which sheet the
    /// document links them from. So a merge done on [`Stylesheet::resolve`]
    /// alone cannot rank what it is merging — the rank is gone — and silently
    /// hands an element the value of whichever sheet happens to be linked last.
    /// That is exactly the authored value, so nothing about the result looks
    /// wrong.
    pub fn resolve_ranked(&self, tag: &str, classes: &[String], id: Option<&str>) -> Ranked {
        self.cascaded(tag, classes, id)
            .into_iter()
            .map(|(property, winner)| {
                let declaration = winner.declaration;
                (
                    property,
                    RankedValue {
                        value: self.substitute_variables(&declaration.value),
                        range: declaration.range.clone(),
                        order: declaration.order,
                        specificity: winner.specificity,
                    },
                )
            })
            .collect()
    }

    /// Every computed longhand for which a *matching* declaration is written
    /// `!important`.
    ///
    /// Asked before anything is resolved, because `!important` is a precedence
    /// rule this cascade does not implement: it cannot say whether such a
    /// declaration won, and reading it as though it had — or dropping it
    /// because another rule outranked it — hands the element a value the author
    /// may not have meant. Both readings are wrong, so the fact is reported
    /// instead of resolved.
    ///
    /// A `!important` declaration on a rule that does not match this element is
    /// not in here. It competes with nothing, so there is nothing ambiguous
    /// about it, and a stylesheet is free to use the flag for selectors this
    /// element does not carry.
    pub fn important_properties(
        &self,
        tag: &str,
        classes: &[String],
        id: Option<&str>,
    ) -> ImportantProperties {
        let mut found = BTreeSet::new();
        for rule in &self.rules {
            if !rule.selector.matches(tag, classes, id) {
                continue;
            }
            for declaration in &rule.declarations {
                if declaration.important {
                    found.extend(property_groups(&declaration.property));
                }
            }
        }
        ImportantProperties { properties: found }
    }

    /// The declaration that currently wins for one property on one element.
    ///
    /// `property` may be spelled either way — asking for `background` and asking
    /// for `background-color` both answer with whichever spelling the cascade
    /// actually chose, and asking for `gap` answers with whichever declaration
    /// won the row axis. That is deliberate: a caller that rewrites a
    /// declaration has to be able to ask for the property it means and get back
    /// the declaration that produced the value being replaced.
    pub fn winner(
        &self,
        tag: &str,
        classes: &[String],
        id: Option<&str>,
        property: &str,
    ) -> Option<&Declaration> {
        let cascaded = self.cascaded(tag, classes, id);
        property_groups(property)
            .into_iter()
            .find_map(|group| cascaded.get(&group).map(|winner| winner.declaration))
    }

    /// The authored byte range of the declaration that currently wins for
    /// `property` on an element.
    ///
    /// This is the provenance a source-preserving write targets, and it is what
    /// makes CSS ownership checkable: a caller asking "does this element own
    /// `background`?" gets `None` when nothing declared it and a real span into
    /// this stylesheet when something did — the span of whichever spelling won,
    /// so it always agrees with [`Stylesheet::resolve`].
    pub fn declaration_value_range(
        &self,
        tag: &str,
        classes: &[String],
        id: Option<&str>,
        property: &str,
    ) -> Option<Range<usize>> {
        self.winner(tag, classes, id, property)
            .map(|declaration| declaration.range.clone())
    }

    /// The `:root` custom property named `name`, without its `--` prefix.
    ///
    /// Values are reported exactly as authored, including any `var()` reference
    /// inside them; substitution happens against the value that uses them.
    pub fn custom_property(&self, name: &str) -> Option<&str> {
        self.custom_properties.get(name).map(String::as_str)
    }

    /// Every supported declaration in the stylesheet, in author order.
    ///
    /// Selectors are not exposed: what a stylesheet declares is only meaningful
    /// against an element, and [`Stylesheet::resolve`] is how it is asked.
    pub fn declarations(&self) -> impl Iterator<Item = &Declaration> {
        self.rules
            .iter()
            .flat_map(|rule| rule.declarations.iter())
    }

    /// Every declaration that wins for one computed longhand on one element,
    /// with the rank that made it win.
    ///
    /// Single source of truth for the cascade: [`Stylesheet::resolve`] reads
    /// values out of it, [`Stylesheet::resolve_ranked`] reads ranks out of it,
    /// and [`Stylesheet::winner`] reads declarations out of it, so the value a
    /// node is drawn with and the authored text an edit would rewrite cannot
    /// disagree.
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

    /// Expand `var(--name)` against the `:root` custom properties.
    ///
    /// Bounded to [`VARIABLE_SUBSTITUTION_PASSES`] passes and applied only to
    /// names that actually resolve. An unknown name, a malformed reference, or
    /// a cycle is left in place rather than resolved to something invented:
    /// substituting a default would fabricate a value the author never wrote,
    /// and the declaration stays visibly unusable instead of silently wrong.
    fn substitute_variables(&self, value: &str) -> String {
        let mut current = value.trim().to_owned();
        for _ in 0..VARIABLE_SUBSTITUTION_PASSES {
            match self.substitute_once(&current) {
                // No reference resolved, so another pass would repeat this.
                None => break,
                Some(next) => current = next,
            }
        }
        current
    }

    /// One pass of variable substitution, or `None` if nothing changed.
    fn substitute_once(&self, value: &str) -> Option<String> {
        let mut out = String::with_capacity(value.len());
        let mut rest = value;
        let mut changed = false;
        while let Some(start) = rest.find("var(") {
            out.push_str(&rest[..start]);
            let after = &rest[start + "var(".len()..];
            let Some(end) = after.find(')') else {
                // Not a complete reference: keep the remainder verbatim.
                out.push_str(&rest[start..]);
                return changed.then_some(out);
            };
            let reference = after[..end].trim();
            match reference.strip_prefix("--").map(str::trim) {
                Some(name) => match self.custom_properties.get(name) {
                    Some(replacement) => {
                        out.push_str(replacement);
                        changed = true;
                    }
                    None => out.push_str(&rest[start..start + "var(".len() + end + 1]),
                },
                None => out.push_str(&rest[start..start + "var(".len() + end + 1]),
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        changed.then_some(out)
    }
}

/// The declaration that currently wins one longhand, with the specificity of
/// the rule it came from.
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
/// [`ResolvedValue`] is this without the rank. Callers reading a single
/// stylesheet do not need it; a caller merging stylesheets cannot rank a merge
/// without it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedValue {
    /// The winning value, with `var()` references substituted.
    ///
    /// The same fact [`ResolvedValue::value`] reports, and subject to the same
    /// caveat: it is not necessarily byte-identical to [`RankedValue::range`].
    pub value: String,
    /// Byte range of the authored value this one came from, in the stylesheet
    /// it was read from.
    pub range: Range<usize>,
    /// Author order of the winning declaration, within its own stylesheet.
    pub order: usize,
    /// Specificity of the rule the winning declaration was authored under.
    ///
    /// Compared by a merging caller. Author order alone is not comparable across
    /// stylesheets, so a merge that ranks on it treats link order as a rank of
    /// its own and lets an unrelated selector beat a more specific one.
    pub specificity: u32,
}

/// Every declaration that wins for one element, with its rank.
///
/// Keyed by computed longhand, and looked up by either spelling for the same
/// reason [`Resolved`] is: `background` is found under `background-color` and
/// `gap` under `row-gap`/`column-gap`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ranked {
    entries: BTreeMap<String, RankedValue>,
}

impl Ranked {
    /// The winning declaration for one property, spelled either way.
    pub fn get(&self, property: &str) -> Option<&RankedValue> {
        property_groups(property)
            .iter()
            .find_map(|group| self.entries.get(group))
    }

    /// The canonical longhand names that resolved, in order.
    pub fn properties(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Every resolved longhand and its declaration.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &RankedValue)> {
        self.entries
            .iter()
            .map(|(property, ranked)| (property.as_str(), ranked))
    }

    /// How many longhands resolved.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing matched this element.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl FromIterator<(String, RankedValue)> for Ranked {
    fn from_iter<T: IntoIterator<Item = (String, RankedValue)>>(iter: T) -> Self {
        Self {
            entries: iter.into_iter().collect(),
        }
    }
}

impl<'a> IntoIterator for &'a Ranked {
    type Item = (&'a str, &'a RankedValue);
    type IntoIter = Box<dyn Iterator<Item = (&'a str, &'a RankedValue)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl IntoIterator for Ranked {
    type Item = (String, RankedValue);
    type IntoIter = Box<dyn Iterator<Item = (String, RankedValue)> + 'static>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.entries.into_iter())
    }
}

impl RankedValue {
    /// The same declaration without the rank, for a caller that has finished
    /// merging.
    ///
    /// Dropping the rank here is only correct once one declaration has won:
    /// there is then nothing left to rank against, so the fact that decided it
    /// is spent. Converting earlier would discard it while it still decides
    /// the answer.
    fn into_resolved(self) -> ResolvedValue {
        ResolvedValue {
            value: self.value,
            range: self.range,
            order: self.order,
        }
    }
}

/// The computed longhands a matching `!important` declaration names.
///
/// A report, not a result: see [`Stylesheet::important_properties`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportantProperties {
    properties: BTreeSet<String>,
}

impl ImportantProperties {
    /// Whether a matching `!important` declaration competes for `property`,
    /// spelled either way.
    ///
    /// Spelling-agnostic for the same reason the cascade is: a
    /// `background: … !important` is an `!important` declaration of
    /// `background-color` however the caller asks for it.
    pub fn contains(&self, property: &str) -> bool {
        property_groups(property)
            .iter()
            .any(|group| self.properties.contains(group))
    }

    /// The canonical longhand names, in order.
    pub fn properties(&self) -> impl Iterator<Item = &str> {
        self.properties.iter().map(String::as_str)
    }

    /// Whether no matching declaration was written `!important`.
    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }
}

/// Whether the authored value text carries `!important`.
///
/// Read from the authored text as written, case-insensitively as CSS spells it
/// `!IMPORTANT` too. `!important` is only ever a flag here — this cascade does
/// not order it (see [`Stylesheet::important_properties`]), so nothing depends
/// on where in the value it appears.
fn carries_important(value: &str) -> bool {
    value.to_ascii_lowercase().contains("!important")
}

/// A resolved declaration: what the element is styled with, and where in the
/// source that value was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedValue {
    /// The winning value, with `var()` references substituted.
    ///
    /// This is the text a consumer parses. It is not necessarily byte-identical
    /// to [`ResolvedValue::range`] — see the module docs on why the two are
    /// reported separately.
    pub value: String,
    /// Byte range of the authored value this one came from, in the stylesheet
    /// it was read from.
    pub range: Range<usize>,
    /// Author order of the winning declaration.
    pub order: usize,
}

/// Everything one element's matching declarations resolve to.
///
/// Keyed by computed longhand, so `background` is found under `background-color`
/// and `gap` under `row-gap`/`column-gap`. [`Resolved::get`] accepts either
/// spelling for the same reason.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    entries: BTreeMap<String, ResolvedValue>,
}

impl Resolved {
    /// The winning declaration for one property, spelled either way.
    pub fn get(&self, property: &str) -> Option<&ResolvedValue> {
        property_groups(property)
            .iter()
            .find_map(|group| self.entries.get(group))
    }

    /// The winning value for one property, spelled either way.
    pub fn value(&self, property: &str) -> Option<&str> {
        self.get(property).map(|resolved| resolved.value.as_str())
    }

    /// The authored byte range that produced one property's value.
    pub fn range(&self, property: &str) -> Option<Range<usize>> {
        self.get(property).map(|resolved| resolved.range.clone())
    }

    /// Whether any declaration won for one property, spelled either way.
    pub fn contains(&self, property: &str) -> bool {
        self.get(property).is_some()
    }

    /// The canonical longhand names that resolved, in order.
    pub fn properties(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Every resolved longhand and its value.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ResolvedValue)> {
        self.entries
            .iter()
            .map(|(property, resolved)| (property.as_str(), resolved))
    }

    /// How many longhands resolved.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing matched this element.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl FromIterator<(String, ResolvedValue)> for Resolved {
    fn from_iter<T: IntoIterator<Item = (String, ResolvedValue)>>(iter: T) -> Self {
        Self {
            entries: iter.into_iter().collect(),
        }
    }
}

impl<'a> IntoIterator for &'a Resolved {
    type Item = (&'a str, &'a ResolvedValue);
    type IntoIter = Box<dyn Iterator<Item = (&'a str, &'a ResolvedValue)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

/// One top-level rule, before it is checked for a usable selector.
struct RawRule {
    /// Byte range of the selector text between the previous block and `{`.
    prelude: Range<usize>,
    /// Byte range of the declarations between `{` and the matching `}`.
    body: Range<usize>,
}

/// Scan `source` for top-level rules, in author order.
///
/// At-rules are skipped whole rather than read as a flat rule list: a
/// statement at-rule ends at its semicolon and a block at-rule ends at its
/// matching brace, and neither contributes declarations here.
fn scan_rules(source: &str) -> Vec<RawRule> {
    let mut found = Vec::new();
    let mut index = 0usize;
    while index < source.len() {
        let Some(open) = find_outside(source, index, b'{') else {
            break;
        };
        let prelude = trim_span(source, index..open);
        if strip_comments(&source[prelude.clone()]).starts_with('@') {
            // `@import url(...);` never opens a block, so its semicolon ends it.
            if let Some(end) = find_outside(source, prelude.start, b';') {
                if end < open {
                    index = end + 1;
                    continue;
                }
            }
            let Some(close) = block_end(source, open) else {
                break;
            };
            index = close + 1;
            continue;
        }
        let Some(close) = block_end(source, open) else {
            // Unterminated: the rest of the file is not a rule.
            break;
        };
        found.push(RawRule {
            prelude,
            body: open + 1..close,
        });
        index = close + 1;
    }
    found
}

/// The `(property, value range)` of every declaration in one rule body.
///
/// The range is into `source`, not into the body, so it is already the offset a
/// writer needs.
fn scan_declarations(source: &str, body: Range<usize>) -> Vec<(String, Range<usize>)> {
    let mut found = Vec::new();
    let mut index = body.start;
    while index < body.end {
        let Some(colon) = find_outside(source, index, b':') else {
            break;
        };
        if colon >= body.end {
            // The next colon belongs to another rule; this body has no
            // declaration left.
            break;
        }
        if let Some(semicolon) = find_outside(source, index, b';') {
            if semicolon < colon {
                // No colon in this segment, so it is not a declaration.
                index = semicolon + 1;
                continue;
            }
        }
        let property = strip_comments(&source[index..colon])
            .trim()
            .to_ascii_lowercase();
        let end = find_outside(source, colon + 1, b';').map_or(body.end, |at| at.min(body.end));
        if !property.is_empty() {
            // A final declaration without a trailing semicolon is still valid,
            // and an empty value is not a declaration at all.
            if let Some(range) = trim_declaration_span(source, colon + 1..end) {
                found.push((property, range));
            }
        }
        index = end + 1;
    }
    found
}

/// The index of the next `target` byte at or after `from`, ignoring anything
/// inside a comment or a quoted string.
///
/// Returns `None` at end of input, and also for an unterminated comment or
/// string — in which case the rest of the input is not interpretable, and
/// reporting a delimiter inside it would invent structure that is not there.
fn find_outside(source: &str, from: usize, target: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut index = from;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'/' && source[index..].starts_with("/*") {
            let end = source[index + 2..].find("*/")?;
            index += 2 + end + 2;
            continue;
        }
        if byte == b'"' || byte == b'\'' {
            let quote = byte;
            index += 1;
            let mut closed = None;
            while index < bytes.len() {
                let inner = bytes[index];
                if inner == b'\\' {
                    index += 2;
                    continue;
                }
                index += 1;
                if inner == quote {
                    closed = Some(index);
                    break;
                }
            }
            index = closed?;
            continue;
        }
        if byte == target {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// The index of the `}` matching the `{` at `open`, counting nesting.
fn block_end(source: &str, open: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut index = open + 1;
    loop {
        let next = find_outside(source, index, b'{')
            .into_iter()
            .chain(find_outside(source, index, b'}'))
            .min()?;
        if source.as_bytes()[next] == b'{' {
            depth += 1;
        } else {
            depth -= 1;
            if depth == 0 {
                return Some(next);
            }
        }
        index = next + 1;
    }
}

/// The span between `start` and `end` with leading and trailing whitespace
/// removed, or an empty span if there is nothing left.
fn trim_span(source: &str, span: Range<usize>) -> Range<usize> {
    let bytes = source.as_bytes();
    let mut start = span.start;
    let mut end = span.end;
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    start..end
}

/// A declaration's value span: whitespace and comments removed from both ends.
///
/// Only whole comments at the edges are dropped, and never from the middle: the
/// span has to stay contiguous, because it is an offset range into a file that
/// a writer will splice into.
fn trim_declaration_span(source: &str, span: Range<usize>) -> Option<Range<usize>> {
    let bytes = source.as_bytes();
    let mut start = span.start;
    let mut end = span.end;
    loop {
        while start < end && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
        if source[start..end].starts_with("/*") {
            let at = source[start + 2..end].find("*/")?;
            start += 2 + at + 2;
            continue;
        }
        break;
    }
    loop {
        while end > start && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end >= start + 2 && source[..end].ends_with("*/") {
            match source[..end].rfind("/*") {
                Some(at) if at >= start => {
                    end = at;
                    continue;
                }
                _ => break,
            }
        }
        break;
    }
    (start < end).then_some(start..end)
}

/// `text` with every `/* ... */` comment removed.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start + 2..].find("*/") else {
            // Unterminated: keep what came before it and drop the rest.
            return out;
        };
        rest = &rest[start + 2 + end + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(classes: &[&str]) -> Vec<String> {
        classes.iter().map(|c| (*c).to_owned()).collect()
    }

    fn resolved_for(source: &str, tag: &str, classes: &[&str], id: Option<&str>) -> Resolved {
        Stylesheet::parse(source).resolve(tag, &owned(classes), id)
    }

    /// The authored bytes the cascade says won `property`, sliced back out of
    /// the stylesheet it was parsed from.
    fn winning_text<'a>(source: &'a str, sheet: &Stylesheet, selector: (&str, &[&str], Option<&str>), property: &str) -> &'a str {
        let (tag, classes, id) = selector;
        let range = sheet
            .declaration_value_range(tag, &owned(classes), id, property)
            .unwrap_or_else(|| panic!("{property} has a winning declaration"));
        assert!(
            range.end <= source.len(),
            "{property} reported a span outside the stylesheet"
        );
        &source[range]
    }

    #[test]
    fn every_supported_property_resolves_with_the_span_it_was_written_at() {
        // The supported set is the whole contract with the compiler, so each
        // property is checked end to end: it parses, it wins, and the range it
        // reports slices back to exactly the text that was authored.
        for (property, value) in [
            ("background", "#3b5bfd"),
            ("background-color", "#3b5bfd"),
            ("border", "1px solid #16161d"),
            ("border-radius", "8px"),
            ("bottom", "0"),
            ("box-sizing", "border-box"),
            ("color", "#ffffff"),
            ("display", "flex"),
            ("font-size", "14px"),
            ("font-weight", "600"),
            ("height", "40px"),
            ("left", "0"),
            ("margin", "0 auto"),
            ("margin-bottom", "4px"),
            ("margin-left", "4px"),
            ("margin-right", "4px"),
            ("margin-top", "4px"),
            ("max-height", "120px"),
            ("max-width", "640px"),
            ("min-height", "0"),
            ("min-width", "0"),
            ("opacity", "0.5"),
            ("padding", "4px 8px"),
            ("padding-bottom", "4px"),
            ("padding-left", "8px"),
            ("padding-right", "8px"),
            ("padding-top", "4px"),
            ("position", "absolute"),
            ("right", "0"),
            ("text-align", "center"),
            ("top", "0"),
            ("transform", "translate(12px, -4px)"),
            ("width", "320px"),
            ("align-items", "center"),
            ("column-gap", "8px"),
            ("flex-direction", "column"),
            ("flex-wrap", "wrap"),
            ("gap", "10px"),
            ("justify-content", "space-between"),
            ("row-gap", "10px"),
        ] {
            assert!(
                SUPPORTED_PROPERTIES.contains(&property),
                "{property} is exercised but not declared supported"
            );
            let source = format!(".x {{ {property}: {value}; }}");
            let sheet = Stylesheet::parse(&source);
            let resolved = sheet.resolve("div", &owned(&["x"]), None);
            // `background` and `gap` resolve under their longhand names.
            let key = match property {
                "background" => "background-color",
                "gap" => "row-gap",
                other => other,
            };
            let entry = resolved
                .get(key)
                .unwrap_or_else(|| panic!("{property} resolved as {key}"));
            assert_eq!(entry.value, value, "{property} resolved the wrong value");
            assert_eq!(
                &source[entry.range.clone()],
                value,
                "{property} reported a span that is not its authored value"
            );
            assert_eq!(
                resolved.value(property),
                Some(value),
                "{property} is findable under the spelling that was authored"
            );
        }
    }

    #[test]
    fn the_supported_set_is_exactly_what_is_documented() {
        assert_eq!(SUPPORTED_PROPERTIES.len(), 40);
        for property in ["flex", "align-content", "text-decoration", "z-index"] {
            assert!(
                !SUPPORTED_PROPERTIES.contains(&property),
                "{property} has no place to live yet and must not be read"
            );
        }
        let mut sorted = SUPPORTED_PROPERTIES.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, SUPPORTED_PROPERTIES.to_vec(), "kept sorted");
    }

    #[test]
    fn an_unsupported_property_is_skipped_rather_than_approximated() {
        let source = ".cta { text-decoration: none; color: #ffffff; }";
        let resolved = resolved_for(source, "a", &["cta"], None);
        assert!(!resolved.contains("text-decoration"));
        assert_eq!(resolved.value("color"), Some("#ffffff"));
    }

    #[test]
    fn malformed_input_is_skipped_instead_of_panicking() {
        // Nothing here is a supported rule, and none of it may take the process
        // down: the compiler reads whatever files a project happens to have.
        for source in [
            "",
            "   ",
            "{",
            "}",
            "a {",
            "a { color: red;",
            "a { color }",
            "a { : red; }",
            "a { color: ; }",
            "a { color: red",
            "a { /* unterminated",
            "a { color: \"unterminated; }",
            "a { color: red; /* trailing",
            "@media (min-width: 800px) {",
            ".a { color: red } .b { color: blue } trailing",
            "\u{1F600} { color: red; }",
            "\u{1F600}: { color: red; }",
            "a { color: red; }\u{1F600}",
            "a, b { color: red; }",
            ".a.b.c.d.e.f.g.h.i.j.k.l { color: red; }",
            "{ color: red; }",
        ] {
            let sheet = Stylesheet::parse(source);
            let resolved = sheet.resolve("a", &owned(&["b", "c"]), Some("hero"));
            // Whatever it decided, every range it reports must be real.
            for (_, entry) in resolved.iter() {
                assert!(
                    entry.range.end <= source.len(),
                    "a range escaped the stylesheet for {source:?}"
                );
            }
        }
        // And the last rule on one line is still found, which the naive
        // "line up to the brace" prelude would have dropped.
        let sheet = Stylesheet::parse("a { color: red; } .b { color: blue; }");
        assert_eq!(
            sheet.resolve("div", &owned(&["b"]), None).value("color"),
            Some("blue")
        );
    }

    #[test]
    fn a_rule_spanning_several_lines_reports_offsets_into_the_whole_file() {
        let source = "\
:root { --accent: #3b5bfd; }

.cta {
  background: var(--accent);
  color: #ffffff;
}
";
        let sheet = Stylesheet::parse(source);
        let range = sheet
            .declaration_value_range("a", &owned(&["cta"]), None, "background-color")
            .expect("background wins");
        let value_start = source.find("var(--accent)").expect("value is present");
        assert_eq!(
            range,
            value_start..value_start + "var(--accent)".len(),
            "the span is the authored value's own offset, not one relative to the block"
        );
        assert_eq!(&source[range.clone()], "var(--accent)");
        let color = sheet
            .declaration_value_range("a", &owned(&["cta"]), None, "color")
            .expect("color wins");
        assert_eq!(&source[color.clone()], "#ffffff");
        assert!(color.start > range.end, "the later declaration starts after it");
    }

    #[test]
    fn no_cut_point_of_a_multibyte_stylesheet_can_panic() {
        // Every offset in this module is computed by the scanner and then used
        // to slice the source, so one offset landing inside a multi-byte
        // character would panic on the author's file rather than on a test
        // input. Every prefix, suffix and doubled form of a stylesheet that
        // carries non-ASCII text is walked here, and every span it reports is
        // checked to be in bounds.
        let base = "\
/* \u{1F600} */
:root { --ink: #16161d; }
.a\u{00E9}.b { color: var(--ink); margin: 0 auto; gap: 4px; }
";
        for cut in 0..=base.len() {
            if !base.is_char_boundary(cut) {
                continue;
            }
            let mut doubled = base.to_owned();
            doubled.push_str(&base[..cut]);
            for candidate in [base[..cut].to_owned(), base[cut..].to_owned(), doubled] {
                let sheet = Stylesheet::parse(&candidate);
                // The invariant that makes a write safe: a declaration's value
                // is exactly the bytes its span covers.
                for declaration in sheet.declarations() {
                    assert!(
                        declaration.range.end <= candidate.len()
                            && candidate.is_char_boundary(declaration.range.start)
                            && candidate.is_char_boundary(declaration.range.end),
                        "a span escaped the stylesheet it came from: {:?} in {candidate:?}",
                        declaration.range
                    );
                    assert_eq!(
                        &candidate[declaration.range.clone()],
                        declaration.value,
                        "the span and the value must be the same authored bytes"
                    );
                }
                for classes in [
                    owned(&["a\u{00E9}", "b"]),
                    owned(&["a"]),
                    Vec::new(),
                ] {
                    let resolved = sheet.resolve("div", &classes, Some("\u{1F600}"));
                    for (_, entry) in resolved.iter() {
                        assert!(
                            entry.range.end <= candidate.len()
                                && candidate.is_char_boundary(entry.range.start)
                                && candidate.is_char_boundary(entry.range.end),
                            "a resolved span escaped the stylesheet: {:?} in {candidate:?}",
                            entry.range
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_winning_declaration_reports_its_exact_byte_span() {
        // The save layer rewrites exactly these bytes, so the span is asserted
        // as a literal offset rather than merely sliced: an off-by-one here is
        // a corrupted file, not a cosmetic difference.
        let source = "a { color: red; }";
        assert_eq!(source.find("red"), Some(11));
        let sheet = Stylesheet::parse(source);
        let resolved = sheet.resolve("a", &[], None);
        assert_eq!(resolved.range("color"), Some(11..14));
        assert_eq!(resolved.value("color"), Some("red"));

        // Indented and multi-line, with the value on its own line.
        let spaced = "\
.cta {
    padding: 4px 8px;
}
";
        let sheet = Stylesheet::parse(spaced);
        let range = sheet
            .declaration_value_range("a", &owned(&["cta"]), None, "padding")
            .expect("padding wins");
        let start = spaced.find("4px 8px").expect("value is present");
        assert_eq!(range, start..start + "4px 8px".len());
        assert_eq!(&spaced[range], "4px 8px");
    }

    #[test]
    fn the_winning_span_is_the_winning_spelling_and_not_a_fixed_first_choice() {
        let source = "a { background-color: #ff0000; background: #0000ff; }";
        let sheet = Stylesheet::parse(source);
        for property in ["background", "background-color"] {
            assert_eq!(
                winning_text(source, &sheet, ("a", &[], None), property),
                "#0000ff",
                "asking for {property} must find the declaration that won"
            );
        }
        // And the reverse order: the longhand written last wins, with its own
        // span.
        let reversed = "a { background: #0000ff; background-color: #ff0000; }";
        let sheet = Stylesheet::parse(reversed);
        assert_eq!(
            winning_text(reversed, &sheet, ("a", &[], None), "background"),
            "#ff0000"
        );
    }

    #[test]
    fn a_later_stylesheet_reports_spans_into_its_own_source() {
        // Stylesheets are resolved in document order by the caller, so each one
        // owns only its own bytes: the later sheet must not answer with an
        // offset that happens to be right in the earlier file.
        let base = ".row { gap: 10px; justify-content: center; }";
        let later = "\n.row { row-gap: 4px; justify-content: space-between; }\n";
        let base_sheet = Stylesheet::parse(base);
        let later_sheet = Stylesheet::parse(later);
        for (property, expected) in [
            ("gap", "4px"),
            ("row-gap", "4px"),
            ("justify-content", "space-between"),
        ] {
            assert_eq!(
                winning_text(later, &later_sheet, ("div", &["row"], None), property),
                expected,
                "the later stylesheet owns {property}"
            );
        }
        assert_eq!(
            winning_text(base, &base_sheet, ("div", &["row"], None), "gap"),
            "10px",
            "and the earlier stylesheet still reports its own shorthand"
        );
        assert!(
            later_sheet
                .declaration_value_range("div", &owned(&["row"]), None, "column-gap")
                .is_none(),
            "the later stylesheet declares no column gap, so it owns no span for one"
        );
    }

    #[test]
    fn a_gap_shorthand_fills_both_axes_and_a_row_gap_overrides_only_the_row() {
        // CSS expands `gap` into `row-gap` and `column-gap` before the cascade,
        // so a later `row-gap` overrides the row the shorthand set and leaves
        // the column alone. Resolving `gap` to a single key would take the
        // column with it.
        let source = "a { gap: 10px; row-gap: 4px; }";
        let sheet = Stylesheet::parse(source);
        let resolved = sheet.resolve("a", &[], None);
        assert_eq!(resolved.value("row-gap"), Some("4px"));
        assert_eq!(resolved.value("column-gap"), Some("10px"));
        assert!(
            !resolved.properties().any(|property| property == "gap"),
            "the shorthand resolves under its longhand names, not the spelling used"
        );
        // Each axis reports its own authored span.
        assert_eq!(&source[resolved.range("row-gap").expect("row")], "4px");
        assert_eq!(
            &source[resolved.range("column-gap").expect("column")],
            "10px"
        );

        // Author order decides when specificity does not.
        let reversed = "a { row-gap: 4px; gap: 10px; }";
        let reversed = Stylesheet::parse(reversed).resolve("a", &[], None);
        assert_eq!(reversed.value("row-gap"), Some("10px"));
        assert_eq!(reversed.value("column-gap"), Some("10px"));

        // A unitless gap is the same gap, through the cascade as well.
        let unitless = Stylesheet::parse("a { gap: 10; }").resolve("a", &[], None);
        assert_eq!(unitless.value("row-gap"), Some("10"));
    }

    #[test]
    fn the_gap_group_answers_for_every_spelling_of_an_axis() {
        let source = ".row { gap: 12px; row-gap: 3px; column-gap: 8px; }";
        let sheet = Stylesheet::parse(source);
        for (property, expected) in [("gap", "3px"), ("row-gap", "3px"), ("column-gap", "8px")] {
            assert_eq!(
                winning_text(source, &sheet, ("div", &["row"], None), property),
                expected,
                "{property} must point at the declaration that produced it"
            );
        }
    }

    #[test]
    fn specificity_outranks_author_order() {
        let source = ".cta { color: #ff0000; }\na { color: #0000ff; }\n";
        let with_class = resolved_for(source, "a", &["cta"], None);
        assert_eq!(
            with_class.value("color"),
            Some("#ff0000"),
            "the earlier class rule still wins: specificity is checked first"
        );
        assert_eq!(
            resolved_for(source, "a", &[], None).value("color"),
            Some("#0000ff"),
            "and an element with no class still gets the tag rule's value"
        );
        // Equal specificity falls back to author order.
        let tie = "a { color: #ff0000; }\na { color: #0000ff; }\n";
        assert_eq!(resolved_for(tie, "a", &[], None).value("color"), Some("#0000ff"));
    }

    #[test]
    fn an_id_rule_outranks_a_class_rule_whatever_the_order() {
        let source = "#hero { color: #00ff00; }\n.cta { color: #ff0000; }\n";
        assert_eq!(
            resolved_for(source, "a", &["cta"], Some("hero")).value("color"),
            Some("#00ff00"),
            "one id outranks any number of classes"
        );
        assert_eq!(
            resolved_for(source, "a", &["cta"], Some("other")).value("color"),
            Some("#ff0000"),
            "a different id does not match at all"
        );
        // Three classes still lose to one id.
        let classes = ".a.b.c { color: #ff0000; }\n#hero { color: #00ff00; }\n";
        assert_eq!(
            resolved_for(classes, "div", &["a", "b", "c"], Some("hero")).value("color"),
            Some("#00ff00")
        );
    }

    #[test]
    fn a_compound_selector_matches_an_element_carrying_every_part() {
        for selector in ["a.cta", "#hero.cta", "a.cta#hero"] {
            let source = format!("{selector} {{ color: #ff0000; }}");
            let (tag, classes, id) = ("a", ["cta"], Some("hero"));
            assert_eq!(
                resolved_for(&source, tag, &classes, id).value("color"),
                Some("#ff0000"),
                "{selector} should match an element carrying every part"
            );
        }
        // Each part is still required.
        let source = "a.cta { color: #ff0000; }";
        assert!(resolved_for(source, "button", &["cta"], None).is_empty());
        assert!(resolved_for(source, "a", &[], None).is_empty());
        // A tag selector is matched case-insensitively.
        assert_eq!(
            resolved_for("DIV { color: #ff0000; }", "div", &[], None).value("color"),
            Some("#ff0000")
        );
    }

    #[test]
    fn a_multi_class_selector_needs_all_of_its_classes() {
        let source = ".a.b { color: #ff0000; }";
        assert_eq!(
            resolved_for(source, "div", &["a", "b"], None).value("color"),
            Some("#ff0000")
        );
        assert!(
            resolved_for(source, "div", &["a"], None).is_empty(),
            "one of the two is not enough, and the rule must not half-apply"
        );
    }

    #[test]
    fn a_combinator_selector_is_out_of_scope_and_skipped() {
        // `.card .cta` needs a descendant combinator, which this cascade does
        // not implement. It must not be treated as if it were `.cta`.
        for source in [".card .cta { color: #ff0000; }", ".card>.cta { color: #ff0000; }"] {
            assert!(
                resolved_for(source, "a", &["cta"], None).is_empty(),
                "{source} must not silently apply to the child"
            );
        }
    }

    #[test]
    fn a_pseudo_class_selector_matches_nothing() {
        for source in [
            "a:hover { color: #ff0000; }",
            "::before { color: #ff0000; }",
            "a[data-x] { color: #ff0000; }",
            "* { color: #ff0000; }",
        ] {
            assert!(
                resolved_for(source, "a", &[], None).is_empty(),
                "{source} is outside the supported subset and must not be guessed at"
            );
        }
    }

    #[test]
    fn the_rank_that_decided_a_winner_survives_resolution() {
        // A caller merging several stylesheets cannot rank the merge from the
        // values alone, so `resolve_ranked` has to report why each winner won.
        // Without the specificity, `#hero` and `.cta` are just two values and
        // the sheet linked last wins by default.
        let source = "#hero { color: #00ff00; }\n.cta { color: #ff0000; }\na { color: #0000ff; }\n";
        let sheet = Stylesheet::parse(source);

        let ranked = sheet.resolve_ranked("div", &owned(&["cta"]), Some("hero"));
        let color = ranked
            .get("color")
            .unwrap_or_else(|| panic!("color resolves for every rule here"));
        assert_eq!(color.value, "#00ff00", "one id outranks a class");
        assert_eq!(
            color.specificity, 10_000,
            "and reports the specificity that decided it"
        );
        assert_eq!(color.order, 0, "author order is the sheet's own");
        assert_eq!(&source[color.range.clone()], "#00ff00");

        // Same element, different selectors: three different ranks, so a merge
        // has something to compare.
        for (classes, id, expected, specificity, order) in [
            (&["cta"][..], Some("hero"), "#00ff00", 10_000u32, 0usize),
            (&["cta"][..], None, "#ff0000", 100, 1),
            (&[][..], None, "#0000ff", 1, 2),
        ] {
            let classes = owned(classes);
            let at = source.find(expected).expect("authored");
            assert_eq!(
                sheet.resolve_ranked("a", &classes, id).get("color"),
                Some(&RankedValue {
                    value: expected.to_owned(),
                    range: at..at + expected.len(),
                    order,
                    specificity,
                }),
                "classes {classes:?} id {id:?} resolves to {expected}"
            );
        }

        // `resolve` is unchanged: same winner, same value, same span, no rank
        // in the way.
        let resolved = sheet.resolve("div", &owned(&["cta"]), Some("hero"));
        assert_eq!(resolved.value("color"), Some("#00ff00"));
        assert_eq!(
            resolved.range("color"),
            Some(15..22),
            "and the span is still the winner's own"
        );
        assert_eq!(&source[resolved.range("color").expect("color")], "#00ff00");
        assert_eq!(resolved.len(), 1);
    }

    #[test]
    fn an_important_declaration_is_reported_even_though_it_lost_the_cascade() {
        // `#hero` outranks `.theme`, so the cascade as implemented resolves
        // blue and drops the red `!important` on the floor. That is the one
        // case the flag has to survive for, so it is reported from the rules
        // rather than read back off the winner — a reader that inspects only
        // the winner never sees the declaration it cannot order.
        let source = ".theme { color: #ff0000 !important; }\n#hero { color: #0000ff; }\n";
        let sheet = Stylesheet::parse(source);
        assert_eq!(
            sheet
                .resolve("div", &owned(&["theme"]), Some("hero"))
                .value("color"),
            Some("#0000ff"),
            "the cascade as implemented prefers the id, flag or no flag"
        );

        let important = sheet.important_properties("div", &owned(&["theme"]), Some("hero"));
        assert!(
            important.contains("color"),
            "a losing !important is still a matching one"
        );

        // The flag lives on the authored text, not on the winner, so it is
        // reported the same way whichever rule ends up winning.
        assert!(
            sheet
                .important_properties("div", &owned(&["theme"]), Some("hero"))
                .contains("color")
                && sheet
                    .important_properties("div", &owned(&["theme"]), None)
                    .contains("color"),
            "and it is reported whether the flagged rule wins or loses"
        );
        assert!(
            sheet
                .important_properties("div", &owned(&["other"]), Some("hero"))
                .is_empty(),
            "a flag on a selector this element does not match competes with nothing"
        );
        assert!(
            Stylesheet::parse(".theme { color: #ff0000; }")
                .important_properties("div", &owned(&["theme"]), None)
                .is_empty(),
            "and a stylesheet without the flag reports nothing"
        );
    }

    #[test]
    fn an_important_shorthand_is_reported_under_the_longhand_it_competes_with() {
        // `background` and `background-color` are one group, so the flag has to
        // be findable under either spelling — otherwise an element styled with
        // an important shorthand is silently resolved past it.
        let sheet = Stylesheet::parse(".theme { background: #ff0000 !important; }");
        let important = sheet.important_properties("div", &owned(&["theme"]), None);
        assert!(important.contains("background"));
        assert!(important.contains("background-color"));
        assert_eq!(
            important.properties().collect::<Vec<_>>(),
            vec!["background-color"]
        );

        // Same for the one shorthand that expands into two longhands.
        let gap = Stylesheet::parse(".theme { gap: 10px !important; }");
        let gap = gap.important_properties("div", &owned(&["theme"]), None);
        assert!(gap.contains("gap"), "the shorthand the author wrote");
        assert!(gap.contains("row-gap"), "and the axis it competes under");
        assert!(gap.contains("column-gap"));

        // Case is not a way to hide it either.
        let shouted = Stylesheet::parse(".theme { color: #ff0000 !IMPORTANT; }");
        assert!(
            shouted
                .important_properties("div", &owned(&["theme"]), None)
                .contains("color"),
            "CSS spells the flag case-insensitively and so must this"
        );
    }

    #[test]
    fn a_declaration_reports_the_flag_its_author_wrote() {
        let sheet =
            Stylesheet::parse(".cta { color: #ff0000 !important; }\n#hero { color: #00ff00; }\n");
        // Without the id, the flagged rule is the one that wins.
        let flagged = sheet
            .winner("div", &["cta".to_owned()], None, "color")
            .expect("a declaration wins");
        assert!(
            flagged.important,
            "the winner carries the flag it was authored with"
        );
        let unflagged = sheet
            .winner("div", &[], Some("hero"), "color")
            .expect("a declaration wins");
        assert!(
            !unflagged.important,
            "and the flag is not smeared onto rules that were written without it"
        );
        // And with the id, the winner is the rule written without one.
        assert!(
            !sheet
                .winner("div", &["cta".to_owned()], Some("hero"), "color")
                .expect("a declaration wins")
                .important,
            "a rule that outranks the flagged one is not itself flagged"
        );
    }

    #[test]
    fn an_at_rule_is_skipped_instead_of_being_read_as_a_flat_rule_list() {
        let source = "\
@import url(\"base.css\");
@media (min-width: 800px) { .cta { color: #ff0000; } }
.cta { color: #0000ff; }
";
        let resolved = resolved_for(source, "a", &["cta"], None);
        assert_eq!(
            resolved.value("color"),
            Some("#0000ff"),
            "the media query body is not a top-level rule"
        );
    }

    #[test]
    fn a_declaration_on_another_element_does_not_apply() {
        let source = "body { color: #16161d; }";
        assert!(
            resolved_for(source, "h1", &[], None).is_empty(),
            "`body` styles the body, not the headline inside it"
        );
    }

    #[test]
    fn root_contributes_custom_properties_and_nothing_else() {
        let source = "\
:root {
  --accent: #3b5bfd;
  color: #ff0000;
}
.cta { background: var(--accent); }
";
        let sheet = Stylesheet::parse(source);
        assert_eq!(sheet.custom_property("accent"), Some("#3b5bfd"));
        assert_eq!(sheet.custom_property("--accent"), None);
        assert_eq!(sheet.custom_property("nope"), None);
        let resolved = sheet.resolve("a", &owned(&["cta"]), None);
        assert_eq!(resolved.value("background-color"), Some("#3b5bfd"));
        assert!(
            !resolved.contains("color"),
            "`:root` styles the root element, not everything"
        );
    }

    #[test]
    fn var_references_substitute_into_resolved_values() {
        let source = "\
:root { --accent: #3b5bfd; --pad: 4px; }
.cta { background: var(--accent); padding: var(--pad) 8px; }
";
        let sheet = Stylesheet::parse(source);
        let resolved = sheet.resolve("a", &owned(&["cta"]), None);
        assert_eq!(resolved.value("background-color"), Some("#3b5bfd"));
        assert_eq!(resolved.value("padding"), Some("4px 8px"));
        // The span still points at what was written, not at the substituted
        // value: that is the difference between a value and its provenance.
        assert_eq!(
            &source[resolved.range("background-color").expect("background")],
            "var(--accent)"
        );
        assert_eq!(
            &source[resolved.range("padding").expect("padding")],
            "var(--pad) 8px"
        );
    }

    #[test]
    fn several_references_in_one_value_all_substitute() {
        let source = ":root { --a: 1px; --b: 2px; }\n.x { margin: var(--a) var(--b) var(--a); }";
        let resolved = Stylesheet::parse(source).resolve("div", &owned(&["x"]), None);
        assert_eq!(resolved.value("margin"), Some("1px 2px 1px"));
    }

    #[test]
    fn var_substitution_is_bounded_and_leaves_what_it_cannot_resolve() {
        // A cycle must terminate rather than recurse.
        let cyclic = ":root { --a: var(--b); --b: var(--a); }\n.x { color: var(--a); }";
        let resolved = Stylesheet::parse(cyclic).resolve("div", &owned(&["x"]), None);
        assert!(
            resolved.value("color").is_some_and(|value| value.starts_with("var(")),
            "an unresolvable chain is left visibly wrong, not invented"
        );

        // An unknown name, and a reference with no closing paren.
        for source in [
            ".x { color: var(--nope); }",
            ".x { color: var(--nope; }",
            ".x { color: var(accent); }",
            ".x { color: var(); }",
        ] {
            let value = Stylesheet::parse(source)
                .resolve("div", &owned(&["x"]), None)
                .value("color")
                .map(str::to_owned)
                .unwrap_or_default();
            assert!(
                value.contains("var("),
                "{source} left as {value:?} instead of inventing a value"
            );
        }
    }

    #[test]
    fn var_substitution_reaches_through_a_referring_variable_only_boundedly() {
        // One level resolves; the chain is deliberately not followed to the end,
        // and in no case is it followed forever.
        let source = ":root { --one: var(--two); --two: #ff0000; }\n.x { color: var(--one); }";
        let resolved = Stylesheet::parse(source).resolve("div", &owned(&["x"]), None);
        assert_eq!(resolved.value("color"), Some("#ff0000"));
    }

    #[test]
    fn the_final_declaration_without_a_semicolon_is_still_read() {
        let source = ".cta { color: #ffffff }";
        let sheet = Stylesheet::parse(source);
        assert_eq!(sheet.resolve("a", &owned(&["cta"]), None).value("color"), Some("#ffffff"));
        assert_eq!(
            winning_text(source, &sheet, ("a", &["cta"], None), "color"),
            "#ffffff"
        );
    }

    #[test]
    fn comments_never_enter_a_value_or_a_span() {
        let source = "\
/* the primary action */
.cta {
  /* brand */
  color: /* ink */ #16161d /* on white */;
  /* padding: 99px; */
}
";
        let sheet = Stylesheet::parse(source);
        let resolved = sheet.resolve("a", &owned(&["cta"]), None);
        assert_eq!(
            resolved.value("color"),
            Some("#16161d"),
            "edge comments are outside the value, and a commented-out declaration stays commented out"
        );
        assert_eq!(&source[resolved.range("color").expect("color")], "#16161d");
        assert_eq!(resolved.properties().collect::<Vec<_>>(), vec!["color"]);
    }

    #[test]
    fn an_empty_or_valueless_declaration_resolves_to_nothing() {
        let source = "a { color: ; width: 10px; }";
        let sheet = Stylesheet::parse(source);
        let resolved = sheet.resolve("a", &[], None);
        assert!(!resolved.contains("color"));
        assert_eq!(resolved.value("width"), Some("10px"));
        assert_eq!(resolved.len(), 1);
    }

    #[test]
    fn a_property_nothing_declared_has_no_winner_and_no_span() {
        let source = ".cta { color: #ffffff; }";
        let sheet = Stylesheet::parse(source);
        assert!(sheet
            .winner("a", &owned(&["cta"]), None, "align-content")
            .is_none());
        assert!(sheet
            .declaration_value_range("a", &owned(&["cta"]), None, "align-content")
            .is_none());
        assert!(sheet.resolve("a", &[], None).is_empty());
        // Nothing was authored, so nothing was written.
        assert_eq!(sheet.declarations().count(), 1);
    }

    #[test]
    fn a_winner_reports_the_declaration_not_just_the_span() {
        let source = ".cta { flex-direction: column-reverse; }";
        let sheet = Stylesheet::parse(source);
        let winner = sheet
            .winner("a", &owned(&["cta"]), None, "flex-direction")
            .expect("the declaration that won");
        assert_eq!(winner.property, "flex-direction");
        assert_eq!(winner.value, "column-reverse");
        assert_eq!(&source[winner.range.clone()], winner.value);
        assert_eq!(winner.order, 0);
    }

    #[test]
    fn the_flex_longhands_cascade_like_every_other_property() {
        let source = "\
a { flex-direction: column; gap: 10px; }
.cta { flex-direction: row; }
";
        let sheet = Stylesheet::parse(source);
        let resolved = sheet.resolve("a", &owned(&["cta"]), None);
        assert_eq!(
            resolved.value("flex-direction"),
            Some("row"),
            "the later class rule wins on specificity"
        );
        assert_eq!(
            resolved.value("column-gap"),
            Some("10px"),
            "a longhand only the tag rule mentions is kept from it"
        );
        assert_eq!(
            resolved.value("justify-content"),
            None,
            "an undeclared property resolves to nothing rather than an invented default"
        );
        assert_eq!(
            sheet.resolve("a", &[], None).value("flex-direction"),
            Some("column"),
            "and an element without the class still gets the tag rule's value"
        );
    }

    #[test]
    fn unreadable_values_carry_through_unchanged() {
        // Resolution carries the authored text through; interpreting it is the
        // consumer's job. Reading `diagonal` as an axis would place children
        // somewhere the author never asked for.
        let source = ".cta { flex-direction: diagonal; gap: 2em; align-items: flex-start; }";
        let resolved = Stylesheet::parse(source).resolve("a", &owned(&["cta"]), None);
        assert_eq!(resolved.value("flex-direction"), Some("diagonal"));
        assert_eq!(resolved.value("row-gap"), Some("2em"));
        assert_eq!(resolved.value("align-items"), Some("flex-start"));
    }

    #[test]
    fn author_order_alone_decides_between_equal_specificity_rules() {
        // The competing declarations are in different rules with the same
        // selector, so this is pure author order — and the span has to follow.
        let source = "\
.cta { padding: 1px; }
.cta { padding: 2px; }
";
        let sheet = Stylesheet::parse(source);
        assert_eq!(
            sheet.resolve("a", &owned(&["cta"]), None).value("padding"),
            Some("2px")
        );
        assert_eq!(
            winning_text(source, &sheet, ("a", &["cta"], None), "padding"),
            "2px"
        );
    }
}