//! Dev Mode: the Inspect tab, the annotation callouts it draws on the canvas, and the
//! handoff status badge a frame carries.
//!
//! # What this module is
//!
//! Dev Mode is a **mode**, not a panel. figcraft's Dev Mode is the Inspect tab
//! (`RightTab::Inspect`), and while it is open three things change:
//!
//! 1. the **canvas** measures distances on hover without Alt held
//!    (`figcraft_engine::ToolState::redlines`, which the engine's own
//!    `distance::hover_lines` computes), and draws the annotations of the page
//!    as **callouts** beside their layers;
//! 2. a frame with a handoff status carries a **status badge** on the canvas;
//! 3. the **panel** shows the layer's handoff status, its annotations, its dev
//!    resources, its inspected properties and its assets — and, with nothing
//!    selected, the list of what is ready for dev.
//!
//! So this file has three halves, and they are one thing:
//!
//! - **The model** — [`DevModeModel`] reads the engine's [`Document`] and turns
//!   the page, the selection and the focus view into what the panel shows.
//!   Every value it shows comes from the engine's own inspection
//!   (`figcraft_engine::devinspect`), the engine's own annotations
//!   (`figcraft_engine::annotations`) and the engine's own dev resources
//!   (`figcraft_engine::dev_resources`). Nothing here re-derives a measurement.
//! - **The commands** — [`DevAction`] turns a change into the engine's own
//!   command id and parameters, so a change made from the panel is the same
//!   change the CLI, the keymap or an agent would make.
//! - **The drawing** — [`paint_dev_mode_overlay`] draws the callouts over the
//!   canvas, and [`paint_dev_status_badge`] draws the badge. Both take what
//!   they need and return; the canvas's overlay pass calls them.
//!
//! # The reference
//!
//! figcraft's `crates/ui-egui/src/dev_mode.rs` is the specification: the
//! callouts on the canvas (`draw`), the Inspect tab's sections (`status`,
//! `annotations_section`, `dev_resources_section`, `ready_list`,
//! `categories_section`) and the focus view's bar (`focus_bar`).
//! `crates/ui-egui/src/right.rs::inspect` is the panel that mounts them, and
//! `crates/ui-egui/src/canvas.rs::status_badge` is the badge.
//!
//! # Where this differs from figcraft, and why
//!
//! - **The list is folded per property.** figcraft shows the *first* selected
//!   layer's properties and folds nothing. Spool folds per property across the
//!   selection, so two layers that disagree about their width say "Mixed" on
//!   the width's row rather than one of them being silently right.
//! - **A value that is not there says so.** A pinned property the layer does
//!   not have is "Not set", the Inspector's word for a property nobody set,
//!   where figcraft draws an em dash.
//! - **The focus view's bar is a panel section, not a canvas banner.** The bar
//!   is a row of buttons over the canvas, and Spool's canvas overlay pass
//!   paints: it has no hit testing, so a button drawn there could not be
//!   pressed. The bar's commands are in the panel instead.
//! - **No code, no playground, no connect-to-code.** figcraft's Inspect tab
//!   ends with the layer's code, the component playground and the link to a
//!   code component. Those are a code panel, not a Dev Mode panel, and Spool
//!   has no code surface yet.
//!
//! # The source boundary
//!
//! Dev Mode is a *view* of the document: it reads what the engine holds and
//! writes through the engine's commands. The engine's annotations, dev
//! resources and handoff statuses live on the node and in the document, and
//! Spool's save path does not write them back to `lamine.yaml` — the same
//! position the Prototype panel states for its interactions. Nothing here
//! invents a source format and nothing here edits a value the save path
//! cannot carry.
//!
//! # The mode, and the toggle
//!
//! figcraft toggles Dev Mode with `view.devMode` (Shift+D), which switches the
//! right-hand tab between Inspect and Design. The engine's command is the same
//! one Spool already has; what Spool needs is for the Inspector to answer it.
//! [`dev_mode_tab`] is that answer, in one line, and the Inspector's wiring is
//! reported rather than applied.

use std::cell::RefCell;
use std::rc::Rc;

use figcraft_doc::{Annotation, AnnotationCategory, DevStatus, Document, NodeId};
use figcraft_engine::devinspect::{self, ColorFormat};
use figcraft_engine::ViewState;
use gpui::{
    div, prelude::*, px, rgb, rgba, AnyElement, App, Bounds, Context, Corners, Div, Edges, Hsla,
    InteractiveElement, PaintQuad, PathBuilder, Pixels, Point as GPoint, ShapedLine, SharedString,
    Stateful, TextAlign, TextRun, Window,
};
use serde_json::{json, Value};

use crate::engine::canvas::{Palette, Viewport};
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

/// The word for a layer the document no longer holds.
pub const MISSING_LAYER: &str = "Missing layer";

// ═══════════════════════════════════════════════════════════════════════════
// The three states
// ═══════════════════════════════════════════════════════════════════════════

/// One property's state across a selection.
///
/// Three states, never two: a value every selected layer agrees on, agreement
/// that nobody set it, and the absence of agreement. The Inspector's own
/// properties have the same three, and the same words, so "Mixed" means one
/// thing in the panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum DevValue<T> {
    /// Every selected layer has this value, and they agree on it.
    Set(T),
    /// No selected layer has this value.
    #[default]
    Unset,
    /// The selected layers disagree.
    Mixed,
}

impl<T: PartialEq> DevValue<T> {
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
// Which annotations the canvas shows
// ═══════════════════════════════════════════════════════════════════════════

/// Which annotations the canvas shows.
///
/// figcraft's filter, with its three answers: everything, the ones with no
/// category, or one category by id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CategoryFilter {
    /// Every annotation.
    #[default]
    All,
    /// The annotations with no category.
    Uncategorized,
    /// One category, by id.
    Only(String),
}

impl CategoryFilter {
    /// From `all`, `none` or a category id or name.
    pub fn parse(doc: &Document, s: &str) -> Option<Self> {
        match s.trim() {
            k if k.eq_ignore_ascii_case("all") => Some(Self::All),
            k if k.eq_ignore_ascii_case("none") => Some(Self::Uncategorized),
            k => figcraft_engine::annotations::find_category(
                figcraft_engine::annotations::categories(doc),
                k,
            )
            .map(|c| Self::Only(c.id.clone())),
        }
    }

    /// The filter's own name, as the control channel spells it.
    pub fn id(&self) -> &str {
        match self {
            Self::All => "all",
            Self::Uncategorized => "none",
            Self::Only(id) => id,
        }
    }

    /// Whether the filter shows this annotation.
    pub fn shows(&self, doc: &Document, a: &Annotation) -> bool {
        let category = figcraft_engine::annotations::category_of(doc, a);
        match self {
            Self::All => true,
            Self::Uncategorized => category.is_none(),
            Self::Only(id) => category.is_some_and(|c| c.id == *id),
        }
    }

    /// The filter's label, as the dropdown shows it.
    pub fn label(&self, doc: &Document) -> String {
        match self {
            Self::All => "All categories".to_owned(),
            Self::Uncategorized => "No category".to_owned(),
            Self::Only(id) => figcraft_engine::annotations::categories(doc)
                .iter()
                .find(|c| c.id == *id)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "All categories".to_owned()),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The model
// ═══════════════════════════════════════════════════════════════════════════

/// The handoff status of the selection, as the panel shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DevStatusState {
    /// The status, folded across the selection.
    pub status: DevValue<DevStatus>,
    /// Whether the layers changed since they were marked ready for dev.
    ///
    /// figcraft's "Changed": a layer marked ready for dev that has been edited
    /// since says so, and says it instead of its status.
    pub changed: bool,
    /// Whether every selected layer can carry a status at all.
    ///
    /// A status belongs to a frame at the top of a page or section, or to a
    /// section. A rectangle cannot have one, so the panel says nothing about
    /// status rather than offering a dropdown that would be refused.
    pub can_have_status: bool,
}

impl DevStatusState {
    /// The label the status dropdown shows.
    ///
    /// figcraft's rule: a layer that changed since it was marked says
    /// "Changed", a layer with a status says the status, and layers that
    /// disagree say "Mixed".
    pub fn label(&self) -> String {
        match (&self.status, self.changed) {
            (DevValue::Set(DevStatus::ReadyForDev), true) => "Changed".to_owned(),
            (DevValue::Set(status), _) => status.label().to_owned(),
            (DevValue::Mixed, _) => MIXED.to_owned(),
            (DevValue::Unset, _) => DevStatus::None.label().to_owned(),
        }
    }
}

/// One annotation of the selected layer, as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DevAnnotation {
    /// The annotation's index in the layer's list.
    pub index: usize,
    /// The note.
    pub label: String,
    /// The category it is in, when it has one.
    pub category: Option<DevCategory>,
    /// The pinned properties, with their current values.
    pub pinned: Vec<DevPinned>,
}

/// One pinned property of an annotation, with its current value.
#[derive(Clone, Debug, PartialEq)]
pub struct DevPinned {
    /// The engine's own id for the property ("width", "fontSize"…).
    pub id: &'static str,
    /// The engine's own label for it ("Width", "Font size"…).
    pub label: &'static str,
    /// Its value, or "Not set" when the layer does not have it.
    pub value: DevValue<String>,
    /// The colour it is, when it is a colour.
    pub color: Option<String>,
    /// The style or variable it comes from.
    pub token: Option<String>,
}

/// One annotation category, as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DevCategory {
    /// The category's stable id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its colour, as CSS hex.
    pub color: String,
}

/// One dev resource of the selected layer, as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DevResourceRow {
    /// The link's name.
    pub name: String,
    /// The link itself.
    pub url: String,
    /// Its index in the layer's own list, when it is the layer's own.
    ///
    /// `None` when it is inherited: a link of a main component, which the
    /// panel shows read-only.
    pub index: Option<usize>,
    /// The layer it comes from, when it is inherited.
    pub from: Option<NodeId>,
}

impl DevResourceRow {
    /// Whether the layer owns this link, and so whether it can be edited.
    pub fn is_own(&self) -> bool {
        self.index.is_some()
    }
}

/// One inspected property, as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DevRow {
    /// The engine's own label for it ("Width", "Font size"…).
    pub label: String,
    /// Its value, folded across the selection.
    pub value: DevValue<String>,
    /// The colour it is, when it is a colour.
    pub color: Option<String>,
    /// The style or variable it comes from.
    pub token: Option<String>,
}

/// One group of inspected properties.
#[derive(Clone, Debug, PartialEq)]
pub struct DevSection {
    /// The engine's own title for the group ("Layout", "Typography"…).
    pub title: String,
    /// Its rows.
    pub rows: Vec<DevRow>,
}

/// One asset of the selection, as the panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DevAsset {
    /// The layer the asset is.
    pub id: NodeId,
    /// Its name.
    pub name: String,
    /// How it exports, as the engine's own words ("Export", "Icon", "Image").
    pub kind: &'static str,
    /// How it exports, in detail ("PNG 2x", "SVG 1x"…).
    pub exports: Vec<String>,
}

/// One frame or section that is ready for dev, as the list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DevReadyItem {
    /// The layer.
    pub id: NodeId,
    /// Its name.
    pub name: String,
    /// The page it is on.
    pub page: NodeId,
    /// The page's name, as the list shows it beside the layer's.
    pub page_name: String,
    /// Its handoff status.
    pub status: DevStatus,
}

/// The focus view: the layer the canvas shows on its own.
#[derive(Clone, Debug, PartialEq)]
pub struct DevFocus {
    /// The focused layer.
    pub id: NodeId,
    /// Its name.
    pub name: String,
    /// Its handoff status.
    pub status: DevStatus,
    /// Whether it changed since it was marked ready for dev.
    pub changed: bool,
    /// Where it is in the ready-for-dev list, one-based.
    ///
    /// `None` when it is not in the list: the list holds the frames marked
    /// ready for dev, and the focus view can be on any frame at the top of a
    /// page.
    pub at: Option<usize>,
    /// How many ready-for-dev frames there are.
    pub count: usize,
}

/// What the Inspect tab shows about the document and the selection.
#[derive(Clone, Debug, PartialEq)]
pub struct DevModeModel {
    /// The layers the model was read for.
    pub ids: Vec<NodeId>,
    /// The page the model was read for.
    pub page: NodeId,
    /// Whether anything is selected at all.
    ///
    /// The empty state is a different panel, not a panel with no rows: with
    /// nothing selected figcraft shows what is ready for dev and the file's
    /// annotation categories, and no layer's own properties.
    pub has_selection: bool,
    /// The selected layer's name, as the panel's header.
    pub name: String,
    /// The handoff status of the selection.
    pub status: DevStatusState,
    /// The selected layer's annotations.
    pub annotations: Vec<DevAnnotation>,
    /// The selected layer's dev resources.
    pub dev_resources: Vec<DevResourceRow>,
    /// The inspected properties of the selection.
    pub sections: Vec<DevSection>,
    /// The selection's assets.
    pub assets: Vec<DevAsset>,
    /// The frames and sections that are ready for dev.
    pub ready: Vec<DevReadyItem>,
    /// The file's annotation categories.
    pub categories: Vec<DevCategory>,
    /// The focus view, when it is on.
    pub focus: Option<DevFocus>,
    /// The layers on the page that carry annotations.
    ///
    /// The canvas overlay's list: figcraft's `annotations::on_page`.
    pub annotated: Vec<NodeId>,
}

impl Default for DevModeModel {
    /// The model of nothing: no page, no selection, no properties.
    ///
    /// The page's id is the engine's own zero, which is the id the engine
    /// gives a document that has no page yet — a state the editor is never
    /// in, but one a panel can be built in before the engine is attached.
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            page: NodeId(0),
            has_selection: false,
            name: String::new(),
            status: DevStatusState::default(),
            annotations: Vec::new(),
            dev_resources: Vec::new(),
            sections: Vec::new(),
            assets: Vec::new(),
            ready: Vec::new(),
            categories: Vec::new(),
            focus: None,
            annotated: Vec::new(),
        }
    }
}

impl DevModeModel {
    /// The Inspect tab's model: the document, the page and the selection.
    ///
    /// A layer the document does not hold is skipped rather than shown as an
    /// empty one. The properties are folded across the selection, per
    /// property, so a disagreement says so on that property's row.
    ///
    /// `focused` is the layer the focus view is on, which lives in the
    /// session rather than in the document.
    pub fn read(
        doc: &Document,
        page: NodeId,
        selection: &[NodeId],
        focused: Option<NodeId>,
        color_format: ColorFormat,
    ) -> Self {
        let nodes: Vec<&figcraft_doc::Node> =
            selection.iter().filter_map(|id| doc.node(*id)).collect();
        let first_id = selection
            .first()
            .copied()
            .filter(|id| doc.node(*id).is_some());
        let first = first_id.and_then(|id| doc.node(id));

        // ── The handoff status ──
        let status = DevValue::across(nodes.iter().map(|node| Some(node.dev_status)));
        let changed = nodes.iter().all(|node| node.dev_changed);
        let can_have_status =
            !selection.is_empty() && selection.iter().all(|id| devinspect::has_status(doc, *id));

        // ── The inspected properties, folded per property ──
        let per_layer: Vec<Vec<devinspect::Section>> = selection
            .iter()
            .filter_map(|id| doc.node(*id))
            .map(|node| devinspect::properties_with(doc, node.id, color_format))
            .collect();
        let sections = fold_sections(&per_layer);

        // ── The selected layer's annotations ──
        let annotations = first_id
            .map(|id| {
                doc.node(id)
                    .map(|node| {
                        node.annotations
                            .iter()
                            .enumerate()
                            .map(|(index, a)| DevAnnotation {
                                index,
                                label: a.label.clone(),
                                category: figcraft_engine::annotations::category_of(doc, a)
                                    .map(category_row),
                                pinned: pinned_rows(doc, id, a),
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();

        // ── The selected layer's dev resources ──
        let dev_resources = first_id
            .map(|id| {
                figcraft_engine::dev_resources::list(doc, id)
                    .into_iter()
                    .map(|listed| DevResourceRow {
                        name: listed.resource.name.clone(),
                        url: listed.resource.url.clone(),
                        index: listed.index,
                        from: listed.from,
                    })
                    .collect()
            })
            .unwrap_or_default();

        // ── The selection's assets ──
        let assets = devinspect::assets(doc, selection)
            .into_iter()
            .map(|asset| DevAsset {
                id: asset.id,
                name: asset.name,
                kind: match asset.kind {
                    devinspect::AssetKind::Export => "Export",
                    devinspect::AssetKind::Icon => "Icon",
                    devinspect::AssetKind::Image => "Image",
                },
                exports: asset.exports,
            })
            .collect();

        // ── What is ready for dev ──
        let ready: Vec<DevReadyItem> = devinspect::dev_items(doc, None, None)
            .into_iter()
            .map(|item| DevReadyItem {
                id: item.id,
                name: item.name,
                page: item.page,
                page_name: doc
                    .node(item.page)
                    .map(|p| p.name.clone())
                    .unwrap_or_default(),
                status: item.status,
            })
            .collect();

        // ── The file's annotation categories ──
        let categories = figcraft_engine::annotations::categories(doc)
            .iter()
            .map(category_row)
            .collect();

        // ── The focus view ──
        let focus = focused.and_then(|id| {
            let node = doc.node(id)?;
            let at = ready.iter().position(|item| item.id == id).map(|k| k + 1);
            Some(DevFocus {
                id,
                name: node.name.clone(),
                status: node.dev_status,
                changed: node.dev_changed,
                at,
                count: ready
                    .iter()
                    .filter(|item| item.status == DevStatus::ReadyForDev)
                    .count(),
            })
        });

        Self {
            ids: selection.to_vec(),
            page,
            has_selection: !selection.is_empty(),
            name: first.map(|n| n.name.clone()).unwrap_or_default(),
            status: DevStatusState {
                status,
                changed,
                can_have_status,
            },
            annotations,
            dev_resources,
            sections,
            assets,
            ready,
            categories,
            focus,
            annotated: figcraft_engine::annotations::on_page(doc, page),
        }
    }

    /// The ready-for-dev list, filtered by status.
    ///
    /// `None` means any status, which is what the list shows when the filter
    /// is on "Any status".
    pub fn ready_with(&self, status: Option<DevStatus>) -> Vec<&DevReadyItem> {
        self.ready
            .iter()
            .filter(|item| status.is_none_or(|want| want == item.status))
            .collect()
    }

    /// The layer the panel is about: the first selected one.
    pub fn first(&self) -> Option<NodeId> {
        self.ids.first().copied()
    }

    /// The filter's label, from the categories the model read.
    pub fn category_filter_label(&self, filter: &CategoryFilter) -> String {
        match filter {
            CategoryFilter::All => "All categories".to_owned(),
            CategoryFilter::Uncategorized => "No category".to_owned(),
            CategoryFilter::Only(id) => self
                .categories
                .iter()
                .find(|c| &c.id == id)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "All categories".to_owned()),
        }
    }
}

/// One annotation category, as the panel shows it.
fn category_row(category: &AnnotationCategory) -> DevCategory {
    DevCategory {
        id: category.id.clone(),
        name: category.name.clone(),
        color: category.color.to_css_hex(),
    }
}

/// The pinned properties of an annotation, with their current values.
///
/// The engine's own list (`annotations::pinned`), so the panel never spells a
/// property id or a value itself. A property the layer does not have reads as
/// "Not set" rather than as nothing at all.
fn pinned_rows(doc: &Document, id: NodeId, a: &Annotation) -> Vec<DevPinned> {
    figcraft_engine::annotations::pinned(doc, id, a)
        .into_iter()
        .map(|(pinnable, prop)| DevPinned {
            id: pinnable.id,
            label: pinnable.label,
            value: match prop.as_ref() {
                Some(prop) => DevValue::Set(prop.value.clone()),
                None => DevValue::Unset,
            },
            color: prop.as_ref().and_then(|p| p.color.clone()),
            token: prop.as_ref().and_then(|p| p.token.clone()),
        })
        .collect()
}

/// The inspected properties of several layers, folded per property.
///
/// The engine groups the properties of one layer; a selection has several
/// layers, so the groups are the union of theirs and every row is folded on
/// its own. A row one layer has and another does not is a disagreement, and
/// says so.
fn fold_sections(layers: &[Vec<devinspect::Section>]) -> Vec<DevSection> {
    let mut titles: Vec<&str> = Vec::new();
    for layer in layers {
        for section in layer {
            if !titles.contains(&section.title) {
                titles.push(section.title);
            }
        }
    }
    titles
        .into_iter()
        .map(|title| {
            let mut labels: Vec<&str> = Vec::new();
            for layer in layers {
                for section in layer.iter().filter(|s| s.title == title) {
                    for row in &section.rows {
                        if !labels.contains(&row.label.as_str()) {
                            labels.push(row.label.as_str());
                        }
                    }
                }
            }
            let rows = labels
                .into_iter()
                .map(|label| {
                    let value = DevValue::across(layers.iter().map(|layer| {
                        layer
                            .iter()
                            .find(|s| s.title == title)
                            .and_then(|s| s.rows.iter().find(|r| r.label == label))
                            .map(|r| r.value.clone())
                    }));
                    // The colour and the style or variable come from the first
                    // layer that has them: a mixed row has no single colour.
                    let first = layers.iter().find_map(|layer| {
                        layer
                            .iter()
                            .find(|s| s.title == title)
                            .and_then(|s| s.rows.iter().find(|r| r.label == label))
                    });
                    DevRow {
                        label: label.to_owned(),
                        value,
                        color: first.and_then(|r| r.color.clone()),
                        token: first.and_then(|r| r.token.clone()),
                    }
                })
                .collect();
            DevSection {
                title: title.to_owned(),
                rows,
            }
        })
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// The commands
// ═══════════════════════════════════════════════════════════════════════════

/// A change the Inspect tab can ask for.
///
/// One variant per thing the tab can change to the document, so a change the
/// engine has no command for cannot be written down as though it had one.
/// A change to the panel's own state is a [`DevPanelChange`], not one of
/// these: it never reaches the engine.
#[derive(Clone, Debug, PartialEq)]
pub enum DevAction {
    /// Set the handoff status of a set of layers.
    SetStatus {
        /// The layers it is set on.
        ids: Vec<NodeId>,
        /// The status it is set to.
        status: DevStatus,
    },
    /// Add an annotation to a layer.
    AddAnnotation {
        /// The layer it is added to.
        id: NodeId,
        /// The note.
        label: String,
    },
    /// Change an annotation.
    EditAnnotation {
        /// The layer it is on.
        id: NodeId,
        /// Its index.
        index: usize,
        /// Its new note, when the note changes.
        label: Option<String>,
        /// Its pinned properties, when they change.
        properties: Option<Vec<String>>,
        /// Its category, when it changes: `None` for "no category".
        category: Option<Option<String>>,
    },
    /// Remove an annotation.
    RemoveAnnotation {
        /// The layer it is on.
        id: NodeId,
        /// Its index.
        index: usize,
    },
    /// Add an annotation category.
    AddCategory {
        /// The category's name.
        name: String,
    },
    /// Rename or recolour an annotation category.
    EditCategory {
        /// The category's id.
        id: String,
        /// Its new name, when it is renamed.
        name: Option<String>,
        /// Its new colour, as CSS hex, when it is recoloured.
        color: Option<String>,
    },
    /// Remove an annotation category.
    RemoveCategory {
        /// The category's id.
        id: String,
    },
    /// Add a dev resource to a layer.
    AddDevResource {
        /// The layer it is added to.
        id: NodeId,
        /// The link.
        url: String,
    },
    /// Change a dev resource.
    EditDevResource {
        /// The layer it is on.
        id: NodeId,
        /// Its index in the layer's own list.
        index: usize,
        /// Its new link, when the link changes.
        url: Option<String>,
        /// Its new name, when the name changes.
        name: Option<String>,
    },
    /// Remove a dev resource.
    RemoveDevResource {
        /// The layer it is on.
        id: NodeId,
        /// Its index in the layer's own list.
        index: usize,
    },
    /// Show one layer on the canvas on its own.
    Focus {
        /// The layer.
        id: NodeId,
    },
    /// Step to the next or the previous ready-for-dev frame.
    FocusStep {
        /// "next" or "previous".
        direction: &'static str,
    },
    /// Leave the focus view.
    Unfocus,
    /// Compare what changed since the layer was marked ready for dev.
    Compare {
        /// The layer.
        id: NodeId,
    },
    /// Go back to ready for dev, keeping the reason.
    ResolveChange {
        /// The layers.
        ids: Vec<NodeId>,
        /// Why it changed.
        reason: Option<String>,
    },
    /// Select a layer from the ready-for-dev list and zoom to it.
    GoTo {
        /// The layer.
        id: NodeId,
    },
}

impl DevAction {
    /// The engine's command id and parameters for this change.
    ///
    /// The engine's own spellings, so a change made from the panel is the
    /// same change the CLI, the keymap and an agent would make.
    pub fn command(&self) -> (String, Value) {
        match self {
            Self::SetStatus { ids, status } => (
                "node.set".to_owned(),
                json!({ "ids": ids, "props": { "devStatus": status.id() } }),
            ),
            Self::AddAnnotation { id, label } => (
                "annotation.add".to_owned(),
                json!({ "id": id, "label": label }),
            ),
            Self::EditAnnotation {
                id,
                index,
                label,
                properties,
                category,
            } => {
                let mut params = json!({ "id": id, "index": index });
                if let Some(label) = label {
                    params["label"] = json!(label);
                }
                if let Some(properties) = properties {
                    params["properties"] = json!(properties);
                }
                if let Some(category) = category {
                    params["category"] = match category {
                        Some(id) => json!(id),
                        None => Value::Null,
                    };
                }
                ("annotation.edit".to_owned(), params)
            }
            Self::RemoveAnnotation { id, index } => (
                "annotation.remove".to_owned(),
                json!({ "id": id, "index": index }),
            ),
            Self::AddCategory { name } => {
                ("annotation.addCategory".to_owned(), json!({ "name": name }))
            }
            Self::EditCategory { id, name, color } => {
                let mut params = json!({ "id": id });
                if let Some(name) = name {
                    params["name"] = json!(name);
                }
                if let Some(color) = color {
                    params["color"] = json!(color);
                }
                ("annotation.editCategory".to_owned(), params)
            }
            Self::RemoveCategory { id } => {
                ("annotation.removeCategory".to_owned(), json!({ "id": id }))
            }
            Self::AddDevResource { id, url } => (
                "devResource.add".to_owned(),
                json!({ "id": id, "url": url }),
            ),
            Self::EditDevResource {
                id,
                index,
                url,
                name,
            } => {
                let mut params = json!({ "id": id, "index": index });
                if let Some(url) = url {
                    params["url"] = json!(url);
                }
                if let Some(name) = name {
                    params["name"] = json!(name);
                }
                ("devResource.edit".to_owned(), params)
            }
            Self::RemoveDevResource { id, index } => (
                "devResource.remove".to_owned(),
                json!({ "id": id, "index": index }),
            ),
            Self::Focus { id } => ("dev.focus".to_owned(), json!({ "id": id })),
            Self::FocusStep { direction } => {
                ("dev.focus".to_owned(), json!({ "direction": direction }))
            }
            Self::Unfocus => ("dev.unfocus".to_owned(), json!({})),
            Self::Compare { id } => ("dev.compare".to_owned(), json!({ "id": id, "show": true })),
            Self::ResolveChange { ids, reason } => {
                let mut params = json!({ "ids": ids });
                if let Some(reason) = reason {
                    params["reason"] = json!(reason);
                }
                ("dev.resolveChange".to_owned(), params)
            }
            Self::GoTo { id } => (
                "command.batch".to_owned(),
                json!({
                    "label": "Go to layer",
                    "commands": [
                        { "command": "select.set", "params": { "ids": [id] } },
                        { "command": "view.zoomSelection", "params": {} },
                    ],
                }),
            ),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel's own state
// ═══════════════════════════════════════════════════════════════════════════

/// A change to the panel's own state.
///
/// Never in the document: the filter of the ready list and the filter of the
/// callouts are the editor's own, and the engine has no command for them.
#[derive(Clone, Debug, PartialEq)]
pub enum DevPanelChange {
    /// Show only the frames with this status in the ready list.
    ReadyFilter(Option<DevStatus>),
    /// Show only the annotations in this category on the canvas.
    CategoryFilter(CategoryFilter),
}

/// Which menu of the panel is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevMenu {
    /// The ready-for-dev status filter.
    ReadyFilter,
    /// The annotation category filter.
    CategoryFilter,
    /// The handoff status of the selection.
    Status,
    /// One annotation's category.
    AnnotationCategory(usize),
    /// The properties one annotation pins.
    PinProperties(usize),
    /// One category's colour.
    CategoryColor(String),
}

/// Which field is being typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevField {
    /// An annotation's note.
    Note {
        /// The layer the annotation is on.
        id: NodeId,
        /// The annotation's index.
        index: usize,
    },
    /// A new annotation's note.
    NewNote {
        /// The layer it is added to.
        id: NodeId,
    },
    /// A new dev resource's link.
    NewResource {
        /// The layer it is added to.
        id: NodeId,
    },
}

/// The text being typed into one field.
#[derive(Clone, Debug, PartialEq)]
pub struct DevDraft {
    /// Which field.
    pub field: DevField,
    /// What has been typed.
    pub text: String,
    /// Whether the text is selected, so the first key replaces it.
    pub select_all: bool,
}

impl DevField {
    /// Whether the field is the note of this annotation.
    pub fn is_note(&self, id: NodeId, index: usize) -> bool {
        matches!(self, DevField::Note { id: field, index: at } if *field == id && *at == index)
    }

    /// Whether the field is a new note on this layer.
    pub fn is_new_note(&self, id: NodeId) -> bool {
        matches!(self, DevField::NewNote { id: field } if *field == id)
    }

    /// Whether the field is a new dev resource on this layer.
    pub fn is_new_resource(&self, id: NodeId) -> bool {
        matches!(self, DevField::NewResource { id: field } if *field == id)
    }
}

/// The panel's own state.
///
/// Presentation only: never in the document.
#[derive(Clone, Debug, PartialEq)]
pub struct DevPanelState {
    /// Which ready-for-dev status the list shows.
    pub filter: Option<DevStatus>,
    /// Which annotations the canvas shows.
    pub categories: CategoryFilter,
    /// Which menu is open, if any.
    pub open: Option<DevMenu>,
    /// The colour format the inspected properties are shown in.
    pub color_format: ColorFormat,
    /// The text being typed, if any.
    pub draft: Option<DevDraft>,
}

impl Default for DevPanelState {
    fn default() -> Self {
        Self {
            filter: Some(DevStatus::ReadyForDev),
            categories: CategoryFilter::All,
            open: None,
            color_format: ColorFormat::Hex,
            draft: None,
        }
    }
}

impl DevPanelState {
    /// The state a panel starts in.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the panel is part-way through something.
    pub fn is_busy(&self) -> bool {
        self.draft.is_some()
    }

    /// Give up whatever the panel was part-way through.
    ///
    /// Returns whether anything was given up, so a caller knows whether the
    /// key was consumed.
    pub fn cancel(&mut self) -> bool {
        self.open = None;
        self.draft.take().is_some()
    }

    /// Apply a change to the panel's own state.
    pub fn apply(&mut self, change: DevPanelChange) {
        match change {
            DevPanelChange::ReadyFilter(status) => self.filter = status,
            DevPanelChange::CategoryFilter(filter) => self.categories = filter,
        }
    }

    /// Handle one keystroke.
    ///
    /// Returns the change the key asks for, if it asks for one. Escape is not
    /// claimed: it belongs to the editor's ladder.
    pub fn handle_key(&mut self, key: &str) -> Option<DevAction> {
        let draft = self.draft.as_mut()?;
        match key {
            "enter" | "return" => {
                let draft = self.draft.take()?;
                self.commit_draft(draft)
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
            // Arrows are not characters. The panel has no nudge to make, so
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

    /// Commit a typed value: what it means, if it means anything.
    fn commit_draft(&self, draft: DevDraft) -> Option<DevAction> {
        let text = draft.text.trim().to_owned();
        match draft.field {
            DevField::Note { id, index } => {
                if text.is_empty() {
                    return None;
                }
                Some(DevAction::EditAnnotation {
                    id,
                    index,
                    label: Some(text),
                    properties: None,
                    category: None,
                })
            }
            DevField::NewNote { id } => {
                if text.is_empty() {
                    return None;
                }
                Some(DevAction::AddAnnotation { id, label: text })
            }
            DevField::NewResource { id } => {
                if text.is_empty() {
                    return None;
                }
                Some(DevAction::AddDevResource { id, url: text })
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel
// ═══════════════════════════════════════════════════════════════════════════

/// What a surface that hosts the Inspect panel must be able to do.
///
/// The panel draws and asks; the surface owns the engine and runs the changes.
/// The panel never touches the document itself, so the engine's command
/// surface stays the one operation boundary.
pub trait DevHost: Sized + 'static {
    /// Run a change through the engine.
    fn run_dev_action(&mut self, action: DevAction, cx: &mut Context<Self>);

    /// The filter of the annotations the canvas shows has changed.
    ///
    /// The panel owns the filter and the canvas draws the callouts it
    /// selects, so the two have to agree; the surface is what carries the
    /// change from one to the other.
    fn set_dev_category_filter(&mut self, filter: CategoryFilter, cx: &mut Context<Self>) {
        let _ = (filter, cx);
    }
}

/// The Inspect tab, as an element.
///
/// Everything it needs is passed in: the model to show, the panel's own state,
/// and the surface that runs the changes. It returns an element, so the
/// Inspector can put it in a tab of its own and a test can put it in a window
/// on its own.
pub fn dev_mode_panel<T: DevHost>(
    model: &DevModeModel,
    state: &Rc<RefCell<DevPanelState>>,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut column = div()
        .id("devmode-panel")
        .debug_selector(|| "devmode-panel".to_owned())
        .flex()
        .flex_col()
        .gap_3()
        .px(px(14.0))
        .py(px(12.0));

    // Nothing selected: what is ready for dev and the file's categories, and
    // nothing about a layer.
    if !model.has_selection {
        column = column.child(panel_header("Inspect"));
        column = column.child(note("Select a layer to see its properties and code"));
        column = column.child(note("Hover other layers to measure the distance"));
        column = column.child(separator());
        column = column.child(ready_list(model, state, cx));
        column = column.child(separator());
        column = column.child(categories_section(model, state, cx));
        return column.into_any_element();
    }

    column = column.child(panel_header(&model.name));

    // The focus view: the layer the canvas shows on its own.
    if let Some(focus) = &model.focus {
        column = column.child(separator());
        column = column.child(focus_section(focus, cx));
    }

    // The handoff status, for the layers that can carry one.
    if model.status.can_have_status {
        column = column.child(separator());
        column = column.child(status_section(model, state, cx));
    }

    column = column.child(separator());
    column = column.child(annotations_section(model, state, cx));

    column = column.child(separator());
    column = column.child(dev_resources_section(model, state, cx));

    // The inspected properties: the readouts, in the engine's own groups.
    for section in &model.sections {
        column = column.child(separator());
        column = column.child(property_section(section));
    }

    column = column.child(separator());
    column = column.child(assets_section(model));

    column.into_any_element()
}

/// The panel's header: a layer's name, or "Inspect" when there is none.
fn panel_header(title: &str) -> Div {
    div()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(theme::TEXT))
        .child(title.to_owned())
}

/// The focus view's section: the focused layer, and the way out of it.
///
/// figcraft draws this as a bar over the canvas. Spool's canvas overlay pass
/// paints and cannot be pressed, so the bar's commands are here instead.
fn focus_section<T: DevHost>(focus: &DevFocus, cx: &mut Context<T>) -> impl IntoElement {
    let mut section = section_header("Focus view", Vec::new(), false, cx);
    section = section.child(
        div()
            .text_xs()
            .text_color(rgb(theme::TEXT_SECONDARY))
            .child(focus.name.clone()),
    );
    let mut actions: Vec<(&'static str, DevAction)> = Vec::new();
    if focus.status == DevStatus::ReadyForDev && focus.changed {
        actions.push(("Compare changes", DevAction::Compare { id: focus.id }));
    }
    if focus.count > 0 {
        actions.push((
            "Previous ready for dev",
            DevAction::FocusStep {
                direction: "previous",
            },
        ));
        actions.push((
            "Next ready for dev",
            DevAction::FocusStep { direction: "next" },
        ));
    }
    actions.push(("Exit focus view", DevAction::Unfocus));
    for (label, action) in actions {
        section = section.child(action_button(
            label,
            &format!("devmode-focus-{label}"),
            action,
            cx,
        ));
    }
    if let Some(at) = focus.at {
        section = section.child(note(&format!("{at} of {} ready for dev", focus.count)));
    }
    section
}

/// The handoff status of the selection.
fn status_section<T: DevHost>(
    model: &DevModeModel,
    state: &Rc<RefCell<DevPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let open = state.borrow().open == Some(DevMenu::Status);
    let label = model.status.label();
    let mut section = section_header("Status", Vec::new(), false, cx);
    section = section.child(sublabel("Status"));
    let shared = Rc::clone(state);
    let control = dropdown(&label, "devmode-status".to_owned(), open).on_click(cx.listener(
        move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = if state.open == Some(DevMenu::Status) {
                    None
                } else {
                    Some(DevMenu::Status)
                };
                state.draft = None;
            }
            cx.notify();
        },
    ));
    section = section.child(control);
    if open {
        let ids = model.ids.clone();
        let mut menu = menu_box();
        for status in DevStatus::ALL {
            menu = menu.child(menu_item(
                status.label(),
                &format!("devmode-status-option-{}", status.id()),
                DevAction::SetStatus {
                    ids: ids.clone(),
                    status,
                },
                cx,
            ));
        }
        section = section.child(menu);
    }
    // A frame marked ready for dev that has been edited since says so, and
    // offers the way back.
    if model.status.changed {
        section = section.child(note("Changed since it was marked ready for dev"));
        let ids = model.ids.clone();
        section = section.child(action_button(
            "Resolve change",
            "devmode-resolve",
            DevAction::ResolveChange { ids, reason: None },
            cx,
        ));
    }
    section
}

/// The selected layer's annotations: each note with its pinned properties.
fn annotations_section<T: DevHost>(
    model: &DevModeModel,
    state: &Rc<RefCell<DevPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let Some(id) = model.first() else {
        return section_header("Annotations", Vec::new(), true, cx);
    };
    let mut section = section_header("Annotations", Vec::new(), model.annotations.is_empty(), cx);
    for annotation in &model.annotations {
        let index = annotation.index;
        section = section.child(annotation_row(model, annotation, state, cx));
        // The category it is in.
        let open = state.borrow().open == Some(DevMenu::AnnotationCategory(index));
        let label = annotation
            .category
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "No category".to_owned());
        let shared = Rc::clone(state);
        let control = dropdown(&label, format!("devmode-annotation-category-{index}"), open)
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.open = if state.open == Some(DevMenu::AnnotationCategory(index)) {
                        None
                    } else {
                        Some(DevMenu::AnnotationCategory(index))
                    };
                    state.draft = None;
                }
                cx.notify();
            }));
        section = section.child(control);
        if open {
            let mut menu = menu_box();
            menu = menu.child(menu_item(
                "No category",
                &format!("devmode-annotation-category-{index}-none"),
                DevAction::EditAnnotation {
                    id,
                    index,
                    label: None,
                    properties: None,
                    category: Some(None),
                },
                cx,
            ));
            for category in &model.categories {
                menu = menu.child(menu_item(
                    &category.name,
                    &format!("devmode-annotation-category-{index}-{}", category.id),
                    DevAction::EditAnnotation {
                        id,
                        index,
                        label: None,
                        properties: None,
                        category: Some(Some(category.id.clone())),
                    },
                    cx,
                ));
            }
            section = section.child(menu);
        }
        // The pinned properties, with their values.
        for pinned in &annotation.pinned {
            section = section.child(pinned_row(pinned));
        }
        // The menu of properties to pin.
        let open = state.borrow().open == Some(DevMenu::PinProperties(index));
        let shared = Rc::clone(state);
        let control = dropdown(
            "Pin a property",
            format!("devmode-annotation-pin-{index}"),
            open,
        )
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = if state.open == Some(DevMenu::PinProperties(index)) {
                    None
                } else {
                    Some(DevMenu::PinProperties(index))
                };
                state.draft = None;
            }
            cx.notify();
        }));
        section = section.child(control);
        if open {
            let mut menu = menu_box();
            for pinnable in figcraft_engine::annotations::PINNABLE {
                let pinned = annotation.pinned.iter().any(|p| p.id == pinnable.id);
                let mut properties: Vec<String> =
                    annotation.pinned.iter().map(|p| p.id.to_owned()).collect();
                if pinned {
                    properties.retain(|p| p != pinnable.id);
                } else {
                    properties.push(pinnable.id.to_owned());
                }
                menu = menu.child(menu_item(
                    pinnable.label,
                    &format!("devmode-annotation-pin-{index}-{}", pinnable.id),
                    DevAction::EditAnnotation {
                        id,
                        index,
                        label: None,
                        properties: Some(properties),
                        category: None,
                    },
                    cx,
                ));
            }
            section = section.child(menu);
        }
    }
    // A new annotation starts as a note; properties are pinned to it after.
    let editing = state
        .borrow()
        .draft
        .as_ref()
        .is_some_and(|draft| draft.field.is_new_note(id));
    let shown = match state.borrow().draft.as_ref() {
        Some(draft) if draft.field.is_new_note(id) => format!("{}|", draft.text),
        _ => "Add an annotation for developers".to_owned(),
    };
    let shared = Rc::clone(state);
    let field = div()
        .id(SharedString::from("devmode-annotation-new"))
        .debug_selector(|| "devmode-annotation-new".to_owned())
        .text_xs()
        .text_color(rgb(if editing {
            theme::TEXT
        } else {
            theme::TEXT_MUTED
        }))
        .border_1()
        .border_color(rgb(if editing {
            theme::ACCENT
        } else {
            theme::BORDER_SOFT
        }))
        .rounded_sm()
        .px(px(6.0))
        .py(px(4.0))
        .cursor_pointer()
        .child(shown)
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = None;
                state.draft = Some(DevDraft {
                    field: DevField::NewNote { id },
                    text: String::new(),
                    select_all: false,
                });
            }
            cx.notify();
        }));
    section = section.child(field);
    section
}

/// One annotation: its number, its note, and the way to remove it.
fn annotation_row<T: DevHost>(
    model: &DevModeModel,
    annotation: &DevAnnotation,
    state: &Rc<RefCell<DevPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let index = annotation.index;
    let id = model.first().unwrap_or(NodeId(0));
    let editing = state
        .borrow()
        .draft
        .as_ref()
        .is_some_and(|draft| draft.field.is_note(id, index));
    let shown = match state.borrow().draft.as_ref() {
        Some(draft) if draft.field.is_note(id, index) => format!("{}|", draft.text),
        _ => {
            if annotation.label.trim().is_empty() {
                "Add a note for developers".to_owned()
            } else {
                annotation.label.clone()
            }
        }
    };
    let shared = Rc::clone(state);
    let text = annotation.label.clone();
    let field = div()
        .id(SharedString::from(format!("devmode-annotation-{index}")))
        .debug_selector(move || format!("devmode-annotation-{index}"))
        .flex_1()
        .text_xs()
        .text_color(rgb(if editing {
            theme::TEXT
        } else {
            theme::TEXT_SECONDARY
        }))
        .border_1()
        .border_color(rgb(if editing {
            theme::ACCENT
        } else {
            theme::BORDER_SOFT
        }))
        .rounded_sm()
        .px(px(6.0))
        .py(px(4.0))
        .cursor_pointer()
        .child(shown)
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = None;
                state.draft = Some(DevDraft {
                    field: DevField::Note { id, index },
                    text: text.clone(),
                    select_all: true,
                });
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
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(format!("Note {}", index + 1)),
        )
        .child(field)
        .child(action_button(
            "−",
            &format!("devmode-annotation-remove-{index}"),
            DevAction::RemoveAnnotation { id, index },
            cx,
        ))
}

/// One pinned property, with its value.
fn pinned_row(pinned: &DevPinned) -> Div {
    let value = match &pinned.value {
        DevValue::Set(value) => value.clone(),
        DevValue::Unset => NOT_SET.to_owned(),
        DevValue::Mixed => MIXED.to_owned(),
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .px(px(6.0))
        .py(px(2.0))
        .child(
            div()
                .w(px(96.0))
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(pinned.label.to_owned()),
        )
        .child(
            div()
                .flex_1()
                .text_xs()
                .text_color(rgb(theme::TEXT))
                .child(value),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(pinned.token.clone().unwrap_or_default()),
        )
}

/// The selected layer's dev resources.
fn dev_resources_section<T: DevHost>(
    model: &DevModeModel,
    state: &Rc<RefCell<DevPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let Some(id) = model.first() else {
        return section_header("Dev resources", Vec::new(), true, cx);
    };
    let mut section = section_header(
        "Dev resources",
        Vec::new(),
        model.dev_resources.is_empty(),
        cx,
    );
    for resource in &model.dev_resources {
        let mut row = div()
            .flex()
            .items_center()
            .gap_2()
            .px(px(6.0))
            .py(px(4.0))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme::ACCENT))
                            .child(resource.name.clone()),
                    )
                    .child(div().text_xs().text_color(rgb(theme::TEXT_MUTED)).child(
                        match resource.from {
                            Some(from) => format!("From {from}"),
                            None => resource.url.clone(),
                        },
                    )),
            );
        if let Some(index) = resource.index {
            row = row.child(action_button(
                "−",
                &format!("devmode-resource-remove-{index}"),
                DevAction::RemoveDevResource { id, index },
                cx,
            ));
        } else {
            row = row.child(note("From a component"));
        }
        section = section.child(row);
    }
    if model.dev_resources.is_empty() {
        section = section.child(note("Link docs, code or tickets for developers."));
    }
    let editing = state
        .borrow()
        .draft
        .as_ref()
        .is_some_and(|draft| draft.field.is_new_resource(id));
    let shown = match state.borrow().draft.as_ref() {
        Some(draft) if draft.field.is_new_resource(id) => format!("{}|", draft.text),
        _ => "Paste a link (https://…)".to_owned(),
    };
    let shared = Rc::clone(state);
    let field = div()
        .id(SharedString::from("devmode-resource-new"))
        .debug_selector(|| "devmode-resource-new".to_owned())
        .text_xs()
        .text_color(rgb(if editing {
            theme::TEXT
        } else {
            theme::TEXT_MUTED
        }))
        .border_1()
        .border_color(rgb(if editing {
            theme::ACCENT
        } else {
            theme::BORDER_SOFT
        }))
        .rounded_sm()
        .px(px(6.0))
        .py(px(4.0))
        .cursor_pointer()
        .child(shown)
        .on_click(cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = None;
                state.draft = Some(DevDraft {
                    field: DevField::NewResource { id },
                    text: String::new(),
                    select_all: false,
                });
            }
            cx.notify();
        }));
    section = section.child(field);
    section
}

/// One group of inspected properties.
fn property_section(section: &DevSection) -> Div {
    let mut block = div().flex().flex_col().gap_1().child(
        div()
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(theme::TEXT))
            .child(section.title.clone()),
    );
    for row in &section.rows {
        block = block.child(property_row(row));
    }
    block
}

/// One inspected property: its label, its value and the style or variable it
/// comes from.
fn property_row(row: &DevRow) -> Div {
    let value = match &row.value {
        DevValue::Set(value) => value.clone(),
        DevValue::Unset => NOT_SET.to_owned(),
        DevValue::Mixed => MIXED.to_owned(),
    };
    let mut line = div()
        .flex()
        .items_center()
        .gap_2()
        .px(px(6.0))
        .py(px(2.0))
        .child(
            div()
                .w(px(96.0))
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(row.label.clone()),
        );
    if let Some(color) = row
        .color
        .as_deref()
        .and_then(figcraft_doc::color::Color::from_hex)
    {
        line = line.child(swatch(color));
    }
    line = line.child(
        div()
            .flex_1()
            .text_xs()
            .text_color(rgb(theme::TEXT))
            .child(value),
    );
    if let Some(token) = &row.token {
        line = line.child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(token.clone()),
        );
    }
    line
}

/// The selection's assets.
fn assets_section(model: &DevModeModel) -> Div {
    let mut section = div().flex().flex_col().gap_1().child(
        div()
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(theme::TEXT))
            .child("Assets"),
    );
    if model.assets.is_empty() {
        section = section.child(note(
            "No assets: no export settings, icons or images. Add export settings in the Design tab.",
        ));
        return section;
    }
    for asset in &model.assets {
        section = section.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px(px(6.0))
                .py(px(2.0))
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(theme::TEXT))
                        .child(asset.name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_SECONDARY))
                        .child(asset.exports.join(", ")),
                ),
        );
    }
    section
}

/// The frames and sections that are ready for dev.
fn ready_list<T: DevHost>(
    model: &DevModeModel,
    state: &Rc<RefCell<DevPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let open = state.borrow().open == Some(DevMenu::ReadyFilter);
    let filter = state.borrow().filter;
    let label = match filter {
        Some(status) => status.label().to_owned(),
        None => "Any status".to_owned(),
    };
    let mut section = section_header("Ready for dev", Vec::new(), model.ready.is_empty(), cx);
    let shared = Rc::clone(state);
    let control = dropdown(&label, "devmode-ready-filter".to_owned(), open).on_click(cx.listener(
        move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = if state.open == Some(DevMenu::ReadyFilter) {
                    None
                } else {
                    Some(DevMenu::ReadyFilter)
                };
                state.draft = None;
            }
            cx.notify();
        },
    ));
    section = section.child(control);
    if open {
        let mut menu = menu_box();
        for status in [
            Some(DevStatus::ReadyForDev),
            Some(DevStatus::Completed),
            None,
        ] {
            let name = match status {
                Some(status) => status.label().to_owned(),
                None => "Any status".to_owned(),
            };
            menu = menu.child(menu_item_state(
                &name,
                &format!("devmode-ready-filter-{name}"),
                Rc::clone(state),
                DevPanelChange::ReadyFilter(status),
                cx,
            ));
        }
        section = section.child(menu);
    }
    let items = model.ready_with(filter);
    if items.is_empty() {
        section = section.child(note("Nothing here yet. Set a frame's status to list it."));
        return section;
    }
    for item in items {
        let id = item.id;
        let name = item.name.clone();
        let page_name = item.page_name.clone();
        let status = item.status;
        section = section.child(
            div()
                .id(SharedString::from(format!("devmode-ready-item-{id}")))
                .debug_selector(move || format!("devmode-ready-item-{id}"))
                .flex()
                .items_center()
                .gap_2()
                .px(px(6.0))
                .py(px(4.0))
                .cursor_pointer()
                .child(div().size(px(8.0)).rounded_full().bg(rgb(
                    if status == DevStatus::ReadyForDev {
                        theme::SAGE
                    } else {
                        theme::TEXT_MUTED
                    },
                )))
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(theme::TEXT))
                        .child(name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::TEXT_MUTED))
                        .child(page_name),
                )
                .on_click(cx.listener(move |host, _event, _window, cx| {
                    host.run_dev_action(DevAction::GoTo { id }, cx);
                })),
        );
        // The way to the layer on its own.
        section = section.child(action_button(
            "Focus view",
            &format!("devmode-ready-focus-{}", item.id),
            DevAction::Focus { id },
            cx,
        ));
    }
    section
}

/// The file's annotation categories, and the filter of the callouts.
fn categories_section<T: DevHost>(
    model: &DevModeModel,
    state: &Rc<RefCell<DevPanelState>>,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let open = state.borrow().open == Some(DevMenu::CategoryFilter);
    let filter = state.borrow().categories.clone();
    let label = model.category_filter_label(&filter);
    let mut section = section_header("Annotations", Vec::new(), false, cx);
    let shared = Rc::clone(state);
    let control = dropdown(&label, "devmode-category-filter".to_owned(), open).on_click(
        cx.listener(move |_host, _event, _window, cx| {
            {
                let mut state = shared.borrow_mut();
                state.open = if state.open == Some(DevMenu::CategoryFilter) {
                    None
                } else {
                    Some(DevMenu::CategoryFilter)
                };
                state.draft = None;
            }
            cx.notify();
        }),
    );
    section = section.child(control);
    if open {
        let mut menu = menu_box();
        menu = menu.child(menu_item_state(
            "All categories",
            "devmode-category-filter-all",
            Rc::clone(state),
            DevPanelChange::CategoryFilter(CategoryFilter::All),
            cx,
        ));
        menu = menu.child(menu_item_state(
            "No category",
            "devmode-category-filter-none",
            Rc::clone(state),
            DevPanelChange::CategoryFilter(CategoryFilter::Uncategorized),
            cx,
        ));
        for category in &model.categories {
            menu = menu.child(menu_item_state(
                &category.name,
                &format!("devmode-category-filter-{}", category.id),
                Rc::clone(state),
                DevPanelChange::CategoryFilter(CategoryFilter::Only(category.id.clone())),
                cx,
            ));
        }
        section = section.child(menu);
    }
    for category in &model.categories {
        let open = state.borrow().open == Some(DevMenu::CategoryColor(category.id.clone()));
        let shared = Rc::clone(state);
        let id = category.id.clone();
        let for_label = id.clone();
        let swatch = div()
            .id(SharedString::from(format!("devmode-category-color-{id}")))
            .debug_selector(move || format!("devmode-category-color-{for_label}"))
            .size(px(16.0))
            .rounded_sm()
            .bg(rgba(rgba_of_hex(&category.color)))
            .cursor_pointer()
            .on_click(cx.listener(move |_host, _event, _window, cx| {
                {
                    let mut state = shared.borrow_mut();
                    state.open = if state.open == Some(DevMenu::CategoryColor(id.clone())) {
                        None
                    } else {
                        Some(DevMenu::CategoryColor(id.clone()))
                    };
                    state.draft = None;
                }
                cx.notify();
            }));
        section = section.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px(px(6.0))
                .py(px(4.0))
                .child(swatch)
                .child(
                    div()
                        .flex_1()
                        .text_xs()
                        .text_color(rgb(theme::TEXT))
                        .child(category.name.clone()),
                )
                .child(action_button(
                    "−",
                    &format!("devmode-category-remove-{}", category.id),
                    DevAction::RemoveCategory {
                        id: category.id.clone(),
                    },
                    cx,
                )),
        );
        if open {
            let mut menu = menu_box();
            for color in AnnotationCategory::COLORS {
                let hex = color.to_css_hex();
                menu = menu.child(menu_item(
                    &hex,
                    &format!("devmode-category-color-{}", category.id),
                    DevAction::EditCategory {
                        id: category.id.clone(),
                        name: None,
                        color: Some(hex.clone()),
                    },
                    cx,
                ));
            }
            section = section.child(menu);
        }
    }
    section = section.child(action_button(
        "Add a category",
        "devmode-category-add",
        DevAction::AddCategory {
            name: next_category_name(model),
        },
        cx,
    ));
    section
}

/// The name a new category gets: the first "Category N" that is free.
fn next_category_name(model: &DevModeModel) -> String {
    (1..=model.categories.len() + 1)
        .map(|k| format!("Category {k}"))
        .find(|name| !model.categories.iter().any(|c| &c.name == name))
        .unwrap_or_default()
}

// ═══════════════════════════════════════════════════════════════════════════
// The panel's chrome
// ═══════════════════════════════════════════════════════════════════════════

/// A section's header, with the actions it carries.
fn section_header<T: DevHost>(
    title: &str,
    actions: Vec<(&'static str, DevAction)>,
    empty: bool,
    cx: &mut Context<T>,
) -> Div {
    let mut header = div().flex().items_center().gap_2().child(
        div()
            .flex_1()
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(theme::TEXT))
            .child(title.to_owned()),
    );
    for (label, action) in actions {
        header = header.child(action_button(
            label,
            &format!("devmode-{title}-{label}"),
            action,
            cx,
        ));
    }
    if empty {
        header = header.child(
            div()
                .text_xs()
                .text_color(rgb(theme::TEXT_MUTED))
                .child("None"),
        );
    }
    header
}

/// A button that asks the host to run a change.
fn action_button<T: DevHost>(
    label: &str,
    selector: &str,
    action: DevAction,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let selector = selector.to_owned();
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .rounded_sm()
        .px(px(6.0))
        .py(px(3.0))
        .cursor_pointer()
        .child(label.to_owned())
        .on_click(cx.listener(move |host, _event, _window, cx| {
            host.run_dev_action(action.clone(), cx);
        }))
}

/// A control that opens a menu.
fn dropdown(label: &str, selector: String, open: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .text_xs()
        .text_color(rgb(theme::TEXT))
        .border_1()
        .border_color(rgb(if open { theme::ACCENT } else { theme::BORDER }))
        .rounded_sm()
        .px(px(6.0))
        .py(px(4.0))
        .cursor_pointer()
        .child(label.to_owned())
        .child("▾")
}

/// A menu's box.
fn menu_box() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .rounded_sm()
        .border_1()
        .border_color(rgb(theme::BORDER))
        .bg(rgb(theme::SURFACE_RAISED))
        .px(px(6.0))
        .py(px(4.0))
}

/// A menu's row that runs a change.
fn menu_item<T: DevHost>(
    label: &str,
    selector: &str,
    action: DevAction,
    cx: &mut Context<T>,
) -> impl IntoElement {
    action_button(label, selector, action, cx)
}

/// A menu's row that changes the panel's own state.
fn menu_item_state<T: DevHost>(
    label: &str,
    selector: &str,
    state: Rc<RefCell<DevPanelState>>,
    change: DevPanelChange,
    cx: &mut Context<T>,
) -> impl IntoElement {
    let selector = selector.to_owned();
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .text_xs()
        .text_color(rgb(theme::TEXT_SECONDARY))
        .rounded_sm()
        .px(px(6.0))
        .py(px(3.0))
        .cursor_pointer()
        .child(label.to_owned())
        .on_click(cx.listener(move |host, _event, _window, cx| {
            {
                let mut state = state.borrow_mut();
                state.apply(change.clone());
                state.open = None;
                state.draft = None;
            }
            // The canvas draws the callouts the filter selects, so it has to
            // be told.
            if let DevPanelChange::CategoryFilter(filter) = &change {
                host.set_dev_category_filter(filter.clone(), cx);
            }
            cx.notify();
        }))
}

/// A label above a group of rows.
fn sublabel(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A note under a section.
fn note(text: &str) -> Div {
    div()
        .text_xs()
        .text_color(rgb(theme::TEXT_MUTED))
        .child(text.to_owned())
}

/// A rule between two parts of the panel.
fn separator() -> Div {
    div().h(px(1.0)).bg(rgb(theme::BORDER_SOFT))
}

/// A colour swatch.
fn swatch(color: figcraft_doc::color::Color) -> Div {
    div().size(px(12.0)).rounded_sm().bg(rgba(rgba_of(color)))
}

/// An engine colour as GPUI's packed RGBA.
fn rgba_of(color: figcraft_doc::color::Color) -> u32 {
    let [r, g, b, a] = color.to_rgba8();
    (u32::from(r) << 24) | (u32::from(g) << 16) | (u32::from(b) << 8) | u32::from(a)
}

/// A CSS hex colour as GPUI's packed RGBA.
fn rgba_of_hex(hex: &str) -> u32 {
    figcraft_doc::color::Color::from_hex(hex)
        .map(rgba_of)
        .unwrap_or(0x22262b)
}

// ═══════════════════════════════════════════════════════════════════════════
// The overlay: the annotation callouts on the canvas
// ═══════════════════════════════════════════════════════════════════════════

/// The width of an annotation callout on the canvas.
pub const CALLOUT_W: f32 = 220.0;
/// The gap between a layer and its callouts.
pub const CALLOUT_GAP: f32 = 24.0;
/// The height of a callout's category line.
const CATEGORY_H: f32 = 18.0;
/// The height of a callout's pinned property row.
const PROPERTY_H: f32 = 18.0;
/// The padding inside a callout, below its last line.
const CALLOUT_PAD: f32 = 16.0;
/// The gap between a note and the pinned properties under it.
const NOTE_GAP: f32 = 6.0;
/// The gap between two callouts.
const CALLOUT_STACK: f32 = 6.0;
/// The size of the note's text.
const NOTE_PT: f32 = 11.0;
/// The size of the category's and the properties' text.
const SMALL_PT: f32 = 10.0;
/// The line height of the note's text.
const NOTE_LINE: f32 = NOTE_PT * 1.3;
/// The most lines a note wraps into.
const NOTE_ROWS: usize = 6;
/// The width of the card's left bar.
const BAR_W: f32 = 3.0;

/// One pinned property, as a callout shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DevCalloutProp {
    /// The property's label.
    pub label: String,
    /// Its value, or "Not set" when the layer does not have it.
    pub value: String,
    /// The colour it is, when it is a colour.
    pub color: Option<Hsla>,
}

/// One annotation callout, laid out on the canvas.
#[derive(Clone, Debug)]
pub struct DevCallout {
    /// The layer the annotation is on.
    pub layer: NodeId,
    /// The annotation's index on that layer.
    pub index: usize,
    /// The card, in window pixels.
    pub rect: Bounds<Pixels>,
    /// The layer's top-right corner: where the callout's line starts.
    pub anchor: GPoint<f32>,
    /// The category's colour, or the accent when it has no category.
    pub color: Hsla,
    /// The category's name.
    pub category: Option<String>,
    /// The note, wrapped.
    pub note: Vec<ShapedLine>,
    /// The pinned properties.
    pub pinned: Vec<DevCalloutProp>,
    /// Whether the layer is selected.
    pub selected: bool,
}

/// The callouts the page's annotations make, laid out where they belong.
///
/// figcraft's `dev_mode::draw`, in the same order and with the same rules: the
/// annotated layers in page order, each layer's callouts stacked beside it,
/// and a callout that would overlap one already drawn moved down below it.
#[allow(clippy::too_many_arguments)]
pub fn dev_callouts(
    window: &mut Window,
    doc: &Document,
    page: NodeId,
    selection: &[NodeId],
    filter: &CategoryFilter,
    view: &ViewState,
    viewport: Viewport,
    clip: Bounds<Pixels>,
    palette: &Palette,
) -> Vec<DevCallout> {
    let mut placed: Vec<Bounds<Pixels>> = Vec::new();
    let mut out: Vec<DevCallout> = Vec::new();
    for id in figcraft_engine::annotations::on_page(doc, page) {
        let Some(node) = doc.node(id).filter(|n| n.visible) else {
            continue;
        };
        let Some(bounds) = doc.world_bounds(id) else {
            continue;
        };
        let a = crate::engine::canvas::page_to_screen(view, viewport, bounds.x0, bounds.y0);
        let b = crate::engine::canvas::page_to_screen(view, viewport, bounds.x1, bounds.y1);
        let right = a.x.max(b.x);
        let top = a.y.min(b.y);
        let x = right + CALLOUT_GAP;
        // Off the canvas: nothing to draw beside.
        if x > f32::from(clip.right())
            || x + CALLOUT_W < f32::from(clip.left())
            || top > f32::from(clip.bottom())
        {
            continue;
        }
        let selected = selection.contains(&id);
        let anchor = GPoint { x: right, y: top };
        let mut y = top;
        for (index, annotation) in node.annotations.iter().enumerate() {
            if y > f32::from(clip.bottom()) {
                break;
            }
            if !filter.shows(doc, annotation) {
                continue;
            }
            let category = figcraft_engine::annotations::category_of(doc, annotation);
            let color = category
                .map(|c| engine_color(c.color))
                .unwrap_or_else(|| palette.accent);
            let pinned = callout_properties(doc, id, annotation);
            let note = wrap_note(window, &annotation.label, palette.ink);
            let height = CATEGORY_H * f32::from(u8::from(category.is_some()))
                + note.len() as f32 * NOTE_LINE
                + if note.is_empty() || pinned.is_empty() {
                    0.0
                } else {
                    NOTE_GAP
                }
                + pinned.len() as f32 * PROPERTY_H
                + CALLOUT_PAD;
            let mut rect = Bounds::new(
                gpui::point(px(x), px(y)),
                gpui::size(px(CALLOUT_W), px(height)),
            );
            // A callout that would overlap one already drawn moves below it.
            loop {
                let below = placed
                    .iter()
                    .filter(|p| intersects(p.dilate(px(3.0)), rect.dilate(px(3.0))))
                    .map(|p| p.bottom())
                    .max();
                let Some(below) = below else { break };
                let moved = Bounds::new(
                    gpui::point(rect.origin.x, below + px(CALLOUT_STACK)),
                    rect.size,
                );
                if moved.origin.y <= rect.origin.y {
                    break;
                }
                rect = moved;
            }
            out.push(DevCallout {
                layer: id,
                index,
                rect,
                anchor,
                color,
                category: category.map(|c| c.name.clone()),
                note,
                pinned,
                selected,
            });
            placed.push(rect);
            y = f32::from(rect.bottom()) + CALLOUT_STACK;
        }
    }
    out
}

/// Whether two rectangles overlap.
fn intersects(a: Bounds<Pixels>, b: Bounds<Pixels>) -> bool {
    a.origin.x < b.right()
        && b.origin.x < a.right()
        && a.origin.y < b.bottom()
        && b.origin.y < a.bottom()
}

/// The pinned properties of an annotation, as a callout shows them.
fn callout_properties(doc: &Document, id: NodeId, annotation: &Annotation) -> Vec<DevCalloutProp> {
    figcraft_engine::annotations::pinned(doc, id, annotation)
        .into_iter()
        .map(|(pinnable, prop)| {
            // The style or variable reads better than its value.
            let value = prop.as_ref().map_or_else(
                || NOT_SET.to_owned(),
                |p| p.token.clone().unwrap_or_else(|| p.value.clone()),
            );
            DevCalloutProp {
                label: pinnable.label.to_owned(),
                value,
                color: prop
                    .as_ref()
                    .and_then(|p| p.color.as_deref())
                    .and_then(figcraft_doc::color::Color::from_hex)
                    .map(engine_color),
            }
        })
        .collect()
}

/// The note's lines: the text wrapped to the callout's width.
fn wrap_note(window: &mut Window, text: &str, ink: Hsla) -> Vec<ShapedLine> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<ShapedLine> = Vec::new();
    let mut current = String::new();
    for word in trimmed.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_owned()
        } else {
            format!("{current} {word}")
        };
        let shaped = shape(window, &candidate, NOTE_PT, ink);
        if f32::from(shaped.width()) > CALLOUT_W - 22.0 && !current.is_empty() {
            lines.push(shape(window, &current, NOTE_PT, ink));
            if lines.len() >= NOTE_ROWS {
                return lines;
            }
            current = word.to_owned();
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() && lines.len() < NOTE_ROWS {
        lines.push(shape(window, &current, NOTE_PT, ink));
    }
    lines
}

/// A line of text, shaped so it can be measured and painted.
fn shape(window: &mut Window, text: &str, size: f32, color: Hsla) -> ShapedLine {
    let run = TextRun {
        len: text.len(),
        font: window.text_style().font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(SharedString::from(text.to_owned()), px(size), &[run], None)
}

/// Draw the callouts of the page's annotations over the canvas.
///
/// The canvas's hook: it is called while Dev Mode is on, and it paints the
/// annotations as cards beside their layers. Returns how many callouts were
/// drawn, so a caller can tell a page without annotations from one that was
/// not drawn.
#[allow(clippy::too_many_arguments)]
pub fn paint_dev_mode_overlay(
    window: &mut Window,
    app: &mut App,
    view: &ViewState,
    viewport: Viewport,
    doc: &Document,
    page: NodeId,
    selection: &[NodeId],
    palette: &Palette,
    filter: &CategoryFilter,
) -> usize {
    let callouts = dev_callouts(
        window,
        doc,
        page,
        selection,
        filter,
        view,
        viewport,
        viewport.bounds(),
        palette,
    );
    paint_dev_callouts(window, app, &callouts, palette);
    callouts.len()
}

/// Draw the callouts themselves.
fn paint_dev_callouts(
    window: &mut Window,
    app: &mut App,
    callouts: &[DevCallout],
    palette: &Palette,
) {
    for callout in callouts {
        let rect = callout.rect;
        // The card.
        fill_rounded(window, rect, 6.0, palette.panel);
        stroke_rounded(
            window,
            rect,
            6.0,
            1.0,
            if callout.selected {
                palette.accent
            } else {
                Hsla::from(rgb(theme::BORDER))
            },
        );
        // The category's bar down the card's left edge.
        fill_rounded(
            window,
            Bounds::new(
                gpui::point(rect.origin.x, rect.origin.y + px(6.0)),
                gpui::size(px(BAR_W), rect.size.height - px(12.0)),
            ),
            2.0,
            callout.color,
        );
        let x = rect.origin.x + px(12.0);
        let mut y = rect.origin.y + px(8.0);
        // The category, as a pill in its colour.
        if let Some(name) = &callout.category {
            let line = shape(window, name, SMALL_PT, gpui::white());
            let width = f32::from(line.width()).min(CALLOUT_W - 30.0);
            let pill = Bounds::new(
                gpui::point(x, y - px(1.0)),
                gpui::size(px(width + 10.0), px(14.0)),
            );
            fill_rounded(window, pill, 4.0, callout.color);
            paint_line(
                window,
                app,
                &line,
                gpui::point(pill.origin.x + px(5.0), pill.origin.y),
                px(SMALL_PT * 1.2),
            );
            y += px(CATEGORY_H);
        }
        // The note.
        for line in &callout.note {
            paint_line(window, app, line, gpui::point(x, y), px(NOTE_LINE));
            y += px(NOTE_LINE);
        }
        if !callout.note.is_empty() && !callout.pinned.is_empty() {
            y += px(NOTE_GAP);
        }
        // The pinned properties, with their values.
        for property in &callout.pinned {
            let mid = y + px(9.0);
            let label = shape(window, &property.label, SMALL_PT, palette.ink_soft);
            paint_line(
                window,
                app,
                &label,
                gpui::point(x, mid - px(SMALL_PT * 0.6)),
                px(SMALL_PT * 1.2),
            );
            let mut vx = x + px(84.0);
            if let Some(color) = property.color {
                let swatch = Bounds::new(
                    gpui::point(vx + px(5.0), mid - px(5.0)),
                    gpui::size(px(10.0), px(10.0)),
                );
                fill_rounded(window, swatch, 2.0, color);
                vx += px(15.0);
            }
            let value = shape(window, &property.value, SMALL_PT, palette.ink);
            paint_line(
                window,
                app,
                &value,
                gpui::point(vx, mid - px(SMALL_PT * 0.6)),
                px(SMALL_PT * 1.2),
            );
            y += px(PROPERTY_H);
        }
        // The line from the layer's corner to the card.
        line_segment(
            window,
            callout.anchor,
            GPoint {
                x: f32::from(rect.origin.x),
                y: f32::from(rect.origin.y) + 14.0,
            },
            callout.color,
        );
    }
}

/// Paint a shaped line of text at `at`.
fn paint_line(
    window: &mut Window,
    app: &mut App,
    line: &ShapedLine,
    at: gpui::Point<Pixels>,
    line_height: Pixels,
) {
    let _ = line.paint(at, line_height, TextAlign::Left, None, window, app);
}

/// A straight line between two window points.
fn line_segment(window: &mut Window, a: GPoint<f32>, b: GPoint<f32>, color: Hsla) {
    let mut path = PathBuilder::stroke(px(1.0));
    path.move_to(gpui::point(px(a.x), px(a.y)));
    path.line_to(gpui::point(px(b.x), px(b.y)));
    if let Ok(built) = path.build() {
        window.paint_path(built, color);
    }
}

/// Fill a rectangle with rounded corners.
fn fill_rounded(window: &mut Window, bounds: Bounds<Pixels>, radius: f32, color: Hsla) {
    window.paint_quad(PaintQuad {
        bounds,
        corner_radii: Corners::all(px(radius)),
        background: color.into(),
        border_widths: Default::default(),
        border_color: Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.0,
            a: 0.0,
        },
        border_style: Default::default(),
    });
}

/// Stroke a rectangle with rounded corners.
fn stroke_rounded(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    radius: f32,
    width: f32,
    color: Hsla,
) {
    let w = px(width);
    window.paint_quad(PaintQuad {
        bounds,
        corner_radii: Corners::all(px(radius)),
        background: Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.0,
            a: 0.0,
        }
        .into(),
        border_widths: Edges {
            top: w,
            bottom: w,
            left: w,
            right: w,
        },
        border_color: color,
        border_style: Default::default(),
    });
}

/// An engine colour as a GPUI colour.
fn engine_color(color: figcraft_doc::color::Color) -> Hsla {
    let [r, g, b, a] = color.to_rgba8();
    Hsla::from(rgba(
        (u32::from(r) << 24) | (u32::from(g) << 16) | (u32::from(b) << 8) | u32::from(a),
    ))
}

// ═══════════════════════════════════════════════════════════════════════════
// The status badge on the canvas
// ═══════════════════════════════════════════════════════════════════════════

/// The colour a handoff status is marked with.
fn status_color(status: DevStatus, changed: bool) -> Hsla {
    let hex = match (status, changed) {
        (DevStatus::ReadyForDev, true) => 0xd97a06,
        (DevStatus::ReadyForDev, false) => 0x14ae5c,
        (DevStatus::Completed, _) => 0x6e6e73,
        (DevStatus::None, _) => 0x6e6e73,
    };
    Hsla::from(rgb(hex))
}

/// The label a handoff status is marked with.
///
/// figcraft's own words: a frame marked ready for dev that has changed since
/// says "Changed" rather than its status.
pub fn status_label(node: &figcraft_doc::Node) -> Option<&'static str> {
    match node.dev_status {
        DevStatus::None => None,
        DevStatus::ReadyForDev if node.dev_changed => Some("Changed"),
        DevStatus::ReadyForDev => Some("Ready for dev"),
        DevStatus::Completed => Some("Completed"),
    }
}

/// Draw a frame's or section's handoff status as a small pill at `at`.
///
/// Returns whether a badge was drawn: a layer with no status has none.
pub fn paint_dev_status_badge(
    window: &mut Window,
    app: &mut App,
    node: &figcraft_doc::Node,
    at: GPoint<f32>,
) -> bool {
    let Some(label) = status_label(node) else {
        return false;
    };
    let line = shape(window, label, SMALL_PT, gpui::white());
    let width = f32::from(line.width());
    let rect = Bounds::new(
        gpui::point(px(at.x), px(at.y - SMALL_PT * 0.7)),
        gpui::size(px(width + 10.0), px(14.0)),
    );
    fill_rounded(
        window,
        rect,
        4.0,
        status_color(node.dev_status, node.dev_changed),
    );
    paint_line(
        window,
        app,
        &line,
        gpui::point(rect.origin.x + px(5.0), rect.origin.y + px(1.0)),
        px(SMALL_PT * 1.2),
    );
    true
}

// ═══════════════════════════════════════════════════════════════════════════
// The mode
// ═══════════════════════════════════════════════════════════════════════════

/// The Inspector's Design tab.
pub const TAB_DESIGN: usize = 0;
/// The Inspector's Dev Mode tab.
///
/// figcraft's right-hand tab order is Design, Prototype, Dev Mode; Spool's is
/// Design, Prototype, Dev Mode, AI.
pub const TAB_DEV_MODE: usize = 2;

/// The tab `view.devMode` switches to.
///
/// figcraft's rule: Dev Mode on goes to the Inspect tab and off goes back to
/// Design, from wherever it was.
pub fn dev_mode_tab(current: usize) -> usize {
    if current == TAB_DEV_MODE {
        TAB_DESIGN
    } else {
        TAB_DEV_MODE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use figcraft_doc::NodeKind;
    use figcraft_engine::Session;
    use gpui::{point, size, IntoElement, Render, TestAppContext, Window};
    use std::cell::Cell;

    // ── The fixture ──────────────────────────────────────────────────────────

    /// A document with a frame, a button in it and a second frame.
    ///
    /// The engine's own commands make the document, so the tests are looking
    /// at a document an editor would really hold rather than one assembled by
    /// hand.
    struct Fixture {
        session: Session,
        page: NodeId,
        frame: NodeId,
        button: NodeId,
    }

    fn fixture() -> Fixture {
        let mut doc = Document::new("Project");
        let page = doc.pages[0];
        let frame = doc
            .create(page, NodeKind::Frame, "Frame 1", 0.0, 0.0, 320.0, 200.0)
            .expect("a frame");
        let button = doc
            .create(frame, NodeKind::Rectangle, "Button", 10.0, 10.0, 80.0, 30.0)
            .expect("a rectangle");
        Fixture {
            session: Session::with_document(doc),
            page,
            frame,
            button,
        }
    }

    impl Fixture {
        fn doc(&self) -> Document {
            self.session.doc().expect("a document").clone()
        }
    }

    /// The id a command made, as the engine's own node id.
    fn created_id(result: &Value) -> NodeId {
        NodeId(result["id"].as_u64().expect("the command made a node"))
    }

    // ── Reading the engine's model ──────────────────────────────────────────

    #[test]
    fn the_model_reads_the_engines_inspect_data() {
        let mut fixture = fixture();
        fixture
            .session
            .execute(
                "node.set",
                &json!({ "ids": [fixture.button], "props": { "width": 80.0, "height": 30.0 } }),
            )
            .expect("the button gets a size");
        fixture
            .session
            .execute(
                "annotation.add",
                &json!({ "id": fixture.button, "label": "Use the accent colour" }),
            )
            .expect("the button gets a note");
        fixture
            .session
            .execute(
                "annotation.edit",
                &json!({ "id": fixture.button, "index": 0, "properties": ["width", "fill"] }),
            )
            .expect("the note pins two properties");
        fixture
            .session
            .execute(
                "devResource.add",
                &json!({ "id": fixture.button, "url": "https://example.com/button" }),
            )
            .expect("the button gets a dev resource");
        let doc = fixture.doc();

        let model = DevModeModel::read(
            &doc,
            fixture.page,
            &[fixture.button],
            None,
            ColorFormat::Hex,
        );
        assert!(model.has_selection, "a layer is selected");
        assert_eq!(
            model.name, "Button",
            "the panel's header is the layer's name"
        );

        // The inspected properties come from the engine's own inspection.
        let layout = model
            .sections
            .iter()
            .find(|section| section.title == "Layout")
            .expect("the layout group");
        let width = layout
            .rows
            .iter()
            .find(|row| row.label == "Width")
            .expect("the width's row");
        assert_eq!(
            width.value,
            DevValue::Set("80".to_owned()),
            "the width is the engine's own, in the engine's own words"
        );

        // The annotation, with the properties it pins.
        assert_eq!(model.annotations.len(), 1, "the layer has one note");
        assert_eq!(
            model.annotations[0].label, "Use the accent colour",
            "the note is the engine's own"
        );
        assert_eq!(
            model.annotations[0].pinned.len(),
            2,
            "and it pins two properties"
        );
        assert_eq!(model.annotations[0].pinned[0].id, "width");
        assert_eq!(
            model.annotations[0].pinned[0].value,
            DevValue::Set("80".to_owned()),
            "the pinned width is the layer's own"
        );

        // The dev resource.
        assert_eq!(model.dev_resources.len(), 1, "the layer has one link");
        assert_eq!(model.dev_resources[0].url, "https://example.com/button");
        assert!(
            model.dev_resources[0].is_own(),
            "and it is the layer's own, so it can be edited"
        );

        // The layer the annotations are on, for the canvas overlay.
        assert_eq!(
            model.annotated,
            vec![fixture.button],
            "the overlay draws the annotated layers of the page"
        );
    }

    #[test]
    fn a_property_the_selection_disagrees_about_is_mixed() {
        let mut fixture = fixture();
        fixture
            .session
            .execute(
                "node.set",
                &json!({ "ids": [fixture.button], "props": { "width": 80.0 } }),
            )
            .expect("the button gets a width");
        let other = created_id(
            &fixture
                .session
                .execute(
                    "node.create",
                    &json!({
                        "type": "rectangle",
                        "parent": fixture.frame,
                        "x": 10.0,
                        "y": 50.0,
                        "width": 120.0,
                        "height": 30.0,
                        "name": "Other",
                    }),
                )
                .expect("a rectangle"),
        );
        let doc = fixture.doc();

        let model = DevModeModel::read(
            &doc,
            fixture.page,
            &[fixture.button, other],
            None,
            ColorFormat::Hex,
        );
        let layout = model
            .sections
            .iter()
            .find(|section| section.title == "Layout")
            .expect("the layout group");
        let width = layout
            .rows
            .iter()
            .find(|row| row.label == "Width")
            .expect("the width's row");
        assert_eq!(
            width.value,
            DevValue::Mixed,
            "the two layers disagree about their width"
        );
        assert_eq!(width.value.word(), MIXED);
    }

    #[test]
    fn the_status_is_folded_across_the_selection() {
        let mut fixture = fixture();
        fixture
            .session
            .execute(
                "node.set",
                &json!({ "ids": [fixture.frame], "props": { "devStatus": "readyForDev" } }),
            )
            .expect("the frame is marked ready for dev");
        let doc = fixture.doc();

        let model =
            DevModeModel::read(&doc, fixture.page, &[fixture.frame], None, ColorFormat::Hex);
        assert!(
            model.status.can_have_status,
            "a frame at the top of a page can carry a status"
        );
        assert_eq!(
            model.status.status,
            DevValue::Set(DevStatus::ReadyForDev),
            "and it has one"
        );
        assert_eq!(model.status.label(), "Ready for dev");

        // A layer that cannot carry a status says nothing about one.
        let model = DevModeModel::read(
            &doc,
            fixture.page,
            &[fixture.button],
            None,
            ColorFormat::Hex,
        );
        assert!(
            !model.status.can_have_status,
            "a rectangle cannot carry a status"
        );
    }

    #[test]
    fn the_ready_list_and_the_categories_come_from_the_engine() {
        let mut fixture = fixture();
        fixture
            .session
            .execute(
                "node.set",
                &json!({ "ids": [fixture.frame], "props": { "devStatus": "readyForDev" } }),
            )
            .expect("the frame is marked ready for dev");
        let doc = fixture.doc();

        let model = DevModeModel::read(&doc, fixture.page, &[], None, ColorFormat::Hex);
        assert!(!model.has_selection, "nothing is selected");
        let ready = model.ready_with(Some(DevStatus::ReadyForDev));
        assert_eq!(ready.len(), 1, "one frame is ready for dev");
        assert_eq!(ready[0].id, fixture.frame);
        assert_eq!(ready[0].name, "Frame 1");
        assert!(
            !model.categories.is_empty(),
            "the file has the engine's own default categories"
        );
    }

    #[test]
    fn the_empty_state_has_the_ready_list_and_the_categories() {
        let fixture = fixture();
        let doc = fixture.doc();
        let model = DevModeModel::read(&doc, fixture.page, &[], None, ColorFormat::Hex);
        assert!(!model.has_selection);
        assert!(
            model.sections.is_empty(),
            "with nothing selected there is no layer's properties"
        );
        assert!(model.annotations.is_empty(), "and no layer's notes");
        // The empty state still lists what is ready for dev.
        assert!(model.ready.is_empty(), "nothing is ready for dev yet");
    }

    #[test]
    fn the_focus_view_is_read_from_the_session() {
        let mut fixture = fixture();
        fixture
            .session
            .execute(
                "node.set",
                &json!({ "ids": [fixture.frame], "props": { "devStatus": "readyForDev" } }),
            )
            .expect("the frame is marked ready for dev");
        let doc = fixture.doc();

        let model = DevModeModel::read(
            &doc,
            fixture.page,
            &[],
            Some(fixture.frame),
            ColorFormat::Hex,
        );
        let focus = model.focus.expect("the focus view is on");
        assert_eq!(focus.id, fixture.frame);
        assert_eq!(focus.name, "Frame 1");
        assert_eq!(focus.at, Some(1), "it is the first ready frame");
        assert_eq!(focus.count, 1, "and the only one");
    }

    // ── The commands ─────────────────────────────────────────────────────────

    #[test]
    fn a_change_carries_the_engines_exact_command_id_and_params() {
        let (command, params) = DevAction::SetStatus {
            ids: vec![NodeId(7)],
            status: DevStatus::ReadyForDev,
        }
        .command();
        assert_eq!(command, "node.set");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["props"]["devStatus"], json!("readyForDev"));

        let (command, params) = DevAction::AddAnnotation {
            id: NodeId(7),
            label: "Use the accent colour".to_owned(),
        }
        .command();
        assert_eq!(command, "annotation.add");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["label"], json!("Use the accent colour"));

        let (command, params) = DevAction::EditAnnotation {
            id: NodeId(7),
            index: 1,
            label: None,
            properties: Some(vec!["width".to_owned()]),
            category: Some(None),
        }
        .command();
        assert_eq!(command, "annotation.edit");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["index"], json!(1));
        assert_eq!(params["properties"], json!(["width"]));
        assert_eq!(params["category"], Value::Null);
        assert!(
            params.get("label").is_none(),
            "a change that does not touch the note does not send one"
        );

        let (command, params) = DevAction::RemoveAnnotation {
            id: NodeId(7),
            index: 0,
        }
        .command();
        assert_eq!(command, "annotation.remove");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["index"], json!(0));

        let (command, params) = DevAction::AddCategory {
            name: "Category 5".to_owned(),
        }
        .command();
        assert_eq!(command, "annotation.addCategory");
        assert_eq!(params["name"], json!("Category 5"));

        let (command, params) = DevAction::EditCategory {
            id: "development".to_owned(),
            name: Some("Build".to_owned()),
            color: Some("#FF0000".to_owned()),
        }
        .command();
        assert_eq!(command, "annotation.editCategory");
        assert_eq!(params["id"], json!("development"));
        assert_eq!(params["name"], json!("Build"));
        assert_eq!(params["color"], json!("#FF0000"));

        let (command, params) = DevAction::RemoveCategory {
            id: "development".to_owned(),
        }
        .command();
        assert_eq!(command, "annotation.removeCategory");
        assert_eq!(params["id"], json!("development"));

        let (command, params) = DevAction::AddDevResource {
            id: NodeId(7),
            url: "https://example.com/button".to_owned(),
        }
        .command();
        assert_eq!(command, "devResource.add");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["url"], json!("https://example.com/button"));

        let (command, params) = DevAction::EditDevResource {
            id: NodeId(7),
            index: 0,
            url: None,
            name: Some("Docs".to_owned()),
        }
        .command();
        assert_eq!(command, "devResource.edit");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["index"], json!(0));
        assert_eq!(params["name"], json!("Docs"));

        let (command, params) = DevAction::RemoveDevResource {
            id: NodeId(7),
            index: 0,
        }
        .command();
        assert_eq!(command, "devResource.remove");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["index"], json!(0));
    }

    #[test]
    fn the_focus_and_compare_commands_are_the_engines_own() {
        let (command, params) = DevAction::Focus { id: NodeId(7) }.command();
        assert_eq!(command, "dev.focus");
        assert_eq!(params["id"], json!(7));

        let (command, params) = DevAction::FocusStep { direction: "next" }.command();
        assert_eq!(command, "dev.focus");
        assert_eq!(params["direction"], json!("next"));

        let (command, params) = DevAction::Unfocus.command();
        assert_eq!(command, "dev.unfocus");
        assert_eq!(params, json!({}));

        let (command, params) = DevAction::Compare { id: NodeId(7) }.command();
        assert_eq!(command, "dev.compare");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["show"], json!(true));

        let (command, params) = DevAction::ResolveChange {
            ids: vec![NodeId(7)],
            reason: Some("Because the API changed".to_owned()),
        }
        .command();
        assert_eq!(command, "dev.resolveChange");
        assert_eq!(params["ids"], json!([7]));
        assert_eq!(params["reason"], json!("Because the API changed"));
    }

    #[test]
    fn going_to_a_layer_selects_it_and_zooms_to_it_in_one_step() {
        let (command, params) = DevAction::GoTo { id: NodeId(7) }.command();
        assert_eq!(command, "command.batch");
        let steps = params["commands"].as_array().expect("the steps");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["command"], json!("select.set"));
        assert_eq!(steps[0]["params"]["ids"], json!([7]));
        assert_eq!(steps[1]["command"], json!("view.zoomSelection"));
    }

    // ── The panel's own state ────────────────────────────────────────────────

    #[test]
    fn a_typed_note_becomes_the_engines_own_edit() {
        let mut state = DevPanelState::new();
        assert!(!state.is_busy());
        state.draft = Some(DevDraft {
            field: DevField::Note {
                id: NodeId(7),
                index: 0,
            },
            text: String::new(),
            select_all: true,
        });
        assert!(state.is_busy());
        for key in ["h", "i"] {
            assert!(
                state.handle_key(key).is_none(),
                "typing does not change the document yet"
            );
        }
        let action = state.handle_key("enter").expect("Enter commits the note");
        let (command, params) = action.command();
        assert_eq!(command, "annotation.edit");
        assert_eq!(params["id"], json!(7));
        assert_eq!(params["index"], json!(0));
        assert_eq!(params["label"], json!("hi"));
    }

    #[test]
    fn an_empty_note_is_not_committed() {
        let mut state = DevPanelState::new();
        state.draft = Some(DevDraft {
            field: DevField::NewNote { id: NodeId(7) },
            text: "   ".to_owned(),
            select_all: false,
        });
        assert!(
            state.handle_key("enter").is_none(),
            "a note with nothing in it is not a note"
        );
    }

    #[test]
    fn the_callout_filter_is_the_panels_own_state_and_never_reaches_the_engine() {
        let mut state = DevPanelState::new();
        assert_eq!(state.categories, CategoryFilter::All);
        state.apply(DevPanelChange::CategoryFilter(CategoryFilter::Only(
            "development".to_owned(),
        )));
        assert_eq!(
            state.categories,
            CategoryFilter::Only("development".to_owned()),
            "the panel's filter is its own state"
        );
        assert_eq!(state.categories.id(), "development");
        state.apply(DevPanelChange::ReadyFilter(Some(DevStatus::Completed)));
        assert_eq!(
            state.filter,
            Some(DevStatus::Completed),
            "and so is the ready list's"
        );
    }

    #[test]
    fn escape_gives_up_the_draft() {
        let mut state = DevPanelState::new();
        state.draft = Some(DevDraft {
            field: DevField::NewNote { id: NodeId(7) },
            text: "half a note".to_owned(),
            select_all: false,
        });
        assert!(state.cancel(), "there was something to give up");
        assert!(!state.is_busy());
        assert!(!state.cancel(), "and nothing left to give up");
    }

    // ── The mode ─────────────────────────────────────────────────────────────

    #[test]
    fn the_toggle_goes_to_dev_mode_and_back_to_design() {
        assert_eq!(dev_mode_tab(TAB_DESIGN), TAB_DEV_MODE);
        assert_eq!(dev_mode_tab(TAB_DEV_MODE), TAB_DESIGN);
        assert_eq!(
            dev_mode_tab(1),
            TAB_DEV_MODE,
            "from any other tab it goes to Dev Mode"
        );
    }

    // ── The panel, in a real window ─────────────────────────────────────────

    /// A surface that holds the panel and remembers what it asked for.
    struct TestHost {
        model: DevModeModel,
        state: Rc<RefCell<DevPanelState>>,
        actions: Vec<DevAction>,
        filters: Vec<CategoryFilter>,
    }

    impl TestHost {
        fn new(model: DevModeModel) -> Self {
            Self {
                model,
                state: Rc::new(RefCell::new(DevPanelState::new())),
                actions: Vec::new(),
                filters: Vec::new(),
            }
        }
    }

    impl DevHost for TestHost {
        fn run_dev_action(&mut self, action: DevAction, _cx: &mut Context<Self>) {
            self.actions.push(action);
        }

        fn set_dev_category_filter(&mut self, filter: CategoryFilter, _cx: &mut Context<Self>) {
            self.filters.push(filter);
        }
    }

    impl Render for TestHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            dev_mode_panel(&self.model, &self.state, cx)
        }
    }

    /// What the overlay test saw.
    #[derive(Default)]
    struct OverlaySeen {
        /// How many callouts were drawn.
        drawn: Cell<usize>,
        /// The callout's height, in logical pixels.
        height: Cell<f32>,
        /// The callout's width, in logical pixels.
        width: Cell<f32>,
        /// How many lines the note wrapped into.
        notes: Cell<usize>,
    }

    /// A surface that holds a canvas the overlay is drawn on.
    struct OverlayHost {
        doc: Document,
        page: NodeId,
        button: NodeId,
        seen: Rc<OverlaySeen>,
    }

    impl Render for OverlayHost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let doc = self.doc.clone();
            let page = self.page;
            let button = self.button;
            let seen = self.seen.clone();
            div().w(px(800.0)).h(px(600.0)).child(
                gpui::canvas(
                    |_bounds, _window, _app| (),
                    move |bounds, _state, window, app| {
                        let viewport = Viewport::new(bounds);
                        let view = ViewState::default();
                        let filter = CategoryFilter::All;
                        let callouts = dev_callouts(
                            window,
                            &doc,
                            page,
                            &[button],
                            &filter,
                            &view,
                            viewport,
                            viewport.bounds(),
                            &Palette::new(),
                        );
                        if let Some(first) = callouts.first() {
                            seen.height.set(f32::from(first.rect.size.height));
                            seen.width.set(f32::from(first.rect.size.width));
                            seen.notes.set(first.note.len());
                        }
                        seen.drawn.set(paint_dev_mode_overlay(
                            window,
                            app,
                            &view,
                            viewport,
                            &doc,
                            page,
                            &[button],
                            &Palette::new(),
                            &filter,
                        ));
                    },
                )
                .absolute()
                .size_full(),
            )
        }
    }

    #[gpui::test]
    fn the_dev_mode_panel_lays_out_in_a_real_window(cx: &mut TestAppContext) {
        let mut fixture = fixture();
        fixture
            .session
            .execute(
                "annotation.add",
                &json!({ "id": fixture.button, "label": "Use the accent colour" }),
            )
            .expect("the button gets a note");
        let doc = fixture.doc();
        let model = DevModeModel::read(
            &doc,
            fixture.page,
            &[fixture.button],
            None,
            ColorFormat::Hex,
        );

        let (host, window) = cx.add_window_view(|_window, _cx| TestHost::new(model));
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(720.0)),
            |_, _| host.clone().into_any_element(),
        );
        window.run_until_parked();

        let panel = window
            .debug_bounds("devmode-panel")
            .expect("the panel is laid out");
        assert!(
            panel.size.height > px(0.0),
            "the panel has no height, so nothing can be drawn in it: {panel:?}"
        );
        assert!(
            panel.size.width > px(0.0),
            "the panel has no width: {panel:?}"
        );

        // The layer's note is one of the rows the panel draws.
        let row = window
            .debug_bounds("devmode-annotation-0")
            .expect("the note is drawn as a row");
        assert!(
            row.size.height > px(0.0),
            "the row is drawn with no height: {row:?}"
        );
        assert!(
            row.origin.y >= panel.origin.y && row.bottom() <= panel.bottom(),
            "the row is drawn inside the panel: row {row:?} panel {panel:?}"
        );
    }

    #[gpui::test]
    fn the_dev_mode_panel_shows_the_empty_state_when_nothing_is_selected(cx: &mut TestAppContext) {
        let fixture = fixture();
        let doc = fixture.doc();
        let model = DevModeModel::read(&doc, fixture.page, &[], None, ColorFormat::Hex);

        let (host, window) = cx.add_window_view(|_window, _cx| TestHost::new(model));
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(720.0)),
            |_, _| host.clone().into_any_element(),
        );
        window.run_until_parked();

        let panel = window
            .debug_bounds("devmode-panel")
            .expect("the panel is laid out");
        assert!(panel.size.height > px(0.0), "the panel has no height");
        // The empty state's own controls are drawn.
        assert!(
            window.debug_bounds("devmode-ready-filter").is_some(),
            "the ready-for-dev filter is drawn"
        );
        assert!(
            window.debug_bounds("devmode-category-filter").is_some(),
            "the annotation category filter is drawn"
        );
        // And no layer's own properties.
        assert!(
            window.debug_bounds("devmode-annotation-0").is_none(),
            "the empty state has no layer's notes"
        );
    }

    #[gpui::test]
    fn the_callouts_are_laid_out_and_drawn_in_a_real_window(cx: &mut TestAppContext) {
        let mut fixture = fixture();
        fixture
            .session
            .execute(
                "annotation.add",
                &json!({ "id": fixture.button, "label": "Use the accent colour" }),
            )
            .expect("the button gets a note");
        let doc = fixture.doc();

        // The overlay is drawn over a canvas, so it is drawn in one: a canvas
        // element, which is where GPUI's paint API may be used.
        let seen = Rc::new(OverlaySeen::default());
        let (host, window) = cx.add_window_view(|_window, _cx| OverlayHost {
            doc: doc.clone(),
            page: fixture.page,
            button: fixture.button,
            seen: seen.clone(),
        });
        window.draw(
            point(px(0.0), px(0.0)),
            size(px(800.0), px(600.0)),
            |_, _| host.clone().into_any_element(),
        );
        window.run_until_parked();

        assert_eq!(
            seen.drawn.get(),
            1,
            "the button's note makes one callout, and it is drawn"
        );
        assert!(
            seen.height.get() > 0.0,
            "the callout has no height: {}",
            seen.height.get()
        );
        assert!(
            seen.width.get() > 0.0,
            "the callout has no width: {}",
            seen.width.get()
        );
        assert!(
            seen.notes.get() > 0,
            "the note is in the callout: {}",
            seen.notes.get()
        );
    }

    #[test]
    fn the_filter_decides_which_annotations_the_canvas_shows() {
        let mut fixture = fixture();
        fixture
            .session
            .execute(
                "annotation.add",
                &json!({ "id": fixture.button, "label": "A note" }),
            )
            .expect("the button gets a note");
        fixture
            .session
            .execute(
                "annotation.edit",
                &json!({ "id": fixture.button, "index": 0, "category": "development" }),
            )
            .expect("the note gets a category");
        let doc = fixture.doc();
        let annotation = doc
            .node(fixture.button)
            .expect("the button")
            .annotations
            .first()
            .expect("the note");

        assert!(
            CategoryFilter::All.shows(&doc, annotation),
            "everything shows everything"
        );
        assert!(
            !CategoryFilter::Uncategorized.shows(&doc, annotation),
            "the uncategorised filter hides a categorised note"
        );
        assert!(
            CategoryFilter::Only("development".to_owned()).shows(&doc, annotation),
            "the category's own filter shows it"
        );
        assert_eq!(
            CategoryFilter::parse(&doc, "development"),
            Some(CategoryFilter::Only("development".to_owned())),
            "the filter parses from a category name"
        );
        assert_eq!(
            CategoryFilter::parse(&doc, "all"),
            Some(CategoryFilter::All),
            "and from 'all'"
        );
    }

    #[test]
    fn a_frame_with_a_status_has_a_badge() {
        let mut fixture = fixture();
        fixture
            .session
            .execute(
                "node.set",
                &json!({ "ids": [fixture.frame], "props": { "devStatus": "readyForDev" } }),
            )
            .expect("the frame is marked ready for dev");
        let doc = fixture.doc();
        let node = doc.node(fixture.frame).expect("the frame");
        assert_eq!(status_label(node), Some("Ready for dev"));
        assert_eq!(
            status_label(doc.node(fixture.button).expect("the button")),
            None,
            "a layer with no status has no badge"
        );
    }
}
