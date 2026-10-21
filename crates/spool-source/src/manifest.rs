//! `lamine.yaml` — the Owner B side of the property-ownership rule.
//!
//! This file holds every persisted property that has no honest HTML/CSS/SVG
//! spelling, plus the identity and hierarchy that markup cannot supply. It does
//! **not** hold anything the engine can read back out of authored source. An
//! Owner A property appearing here is a defect, not a convenience.
//!
//! See the crate docs for the identity split, and `docs/02-document-and-source-model.md`
//! for the normative rule.
//!
//! # Compatibility
//!
//! `version: 1` is the pre-engine format: a hand-parsed line-oriented file whose
//! nodes carry string ids and no Owner B fields. It is still readable, and
//! [`Manifest::from_yaml`] upgrades it by allocating model ids in document
//! order. `version: 2` is this format.
//!
//! Upgrading is lossy in one direction only: a v1 file read and written back
//! gains model ids and may gain nothing else. Nothing a v1 file could express is
//! dropped.

use figcraft_color::ColorProfile;
use figcraft_doc::{
    proto::Interaction, Annotation, AnnotationCategory, AutoLayout, BoolOp, Brush, ChildLayout,
    CollectionId, Comment, Constraints, Corners, DevStatus, ExportSetting, Guide, ImagePreset,
    LayoutGrid, MaskType, Mode, ModeId, NodeId, StrokeProps, Style, StyleRefs, Variable,
    VariableId, WidthProfile,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The `lamine.yaml` schema version this module writes.
pub const MANIFEST_VERSION: u32 = 2;

/// Everything `lamine.yaml` can fail to be.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("lamine.yaml is not valid YAML: {0}")]
    Yaml(String),
    #[error(
        "unsupported lamine.yaml version {found}; this build understands 1 and {MANIFEST_VERSION}"
    )]
    Version { found: u32 },
    #[error("node id {0} appears more than once")]
    DuplicateId(u64),
    #[error("node id {id} has no source key")]
    MissingSourceKey { id: u64 },
    #[error("node id {id} names parent {parent}, which does not exist")]
    MissingParent { id: u64, parent: u64 },
    #[error("node id {id} lists child {child}, which does not exist")]
    MissingChild { id: u64, child: u64 },
    #[error("node id {id} is its own parent")]
    SelfParent { id: u64 },
    #[error("source key {0} is used by more than one node")]
    DuplicateSourceKey(String),
    #[error("node {id} binds to file {file}, which escapes the project root")]
    BindingEscapesRoot { id: u64, file: String },
    #[error("node {id} is a {kind}, and the engine's model cannot give one children")]
    NotAContainer { id: u64, kind: String },
    #[error("node {id} has a binding that is not project-root-relative")]
    BindingNotRelative { id: u64 },
    #[error("node {id} has no source binding at all")]
    Unbound { id: u64 },
}

/// Where a node's authored implementation lives.
///
/// Both fields are project-root-relative with `/` separators. A binding that is
/// absolute, or that climbs out with `..`, is rejected: a `.spool` project must
/// be self-contained, and a binding that escapes it means the document cannot be
/// rebuilt on another machine.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceBinding {
    /// Project-root-relative path, e.g. `pages/index.html`.
    pub file: String,
    /// Selector that resolves to exactly one element in that file.
    pub selector: String,
}

impl SourceBinding {
    /// Rejects a binding that is absolute or climbs above the project root.
    pub fn validate(&self, id: u64) -> Result<(), ManifestError> {
        if self.file.is_empty() {
            return Err(ManifestError::BindingEscapesRoot {
                id,
                file: self.file.clone(),
            });
        }
        if self.file.starts_with('/') || self.file.contains('\\') {
            return Err(ManifestError::BindingNotRelative { id });
        }
        for part in self.file.split('/') {
            if part == ".." {
                return Err(ManifestError::BindingEscapesRoot {
                    id,
                    file: self.file.clone(),
                });
            }
        }
        Ok(())
    }
}

/// One managed node's Owner B state.
///
/// The fields here are deliberately *only* Owner B. Geometry, size, opacity,
/// fills, strokes, text and everything else that CSS can express is read from
/// authored source at compile time and is not recorded here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeRecord {
    // --- identity and hierarchy (Owner B) ---
    /// The engine's identity. Opaque; never derived from anything.
    pub id: u64,
    /// The authored identity, matching `data-spool-id` in markup.
    pub key: String,
    /// Human-facing layer name. Unique within the document.
    pub name: String,
    /// Semantic kind, matching the engine's node kind.
    pub kind: String,
    /// Parent node id. `None` for a page root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    /// Ordered children. Index is paint order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<u64>,
    /// Where this node's authored implementation lives.
    pub binding: SourceBinding,

    // --- resize behaviour: Figma's constraint model is not CSS ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraints: Option<Constraints>,
    /// Aspect-ratio lock while resizing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub constrain_proportions: bool,

    // --- auto-layout semantics CSS cannot state ---
    /// Container auto-layout, when this node is a layout container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<AutoLayout>,
    /// Sizing when this node is a child of a layout container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_layout: Option<ChildLayout>,
    /// Whether this node's own box follows its children.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bounds_follow_children: bool,

    // --- paint and stroke semantics CSS cannot state ---
    /// Corner radii are CSS (`border-radius`); smoothing is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corners: Option<Corners>,
    /// Stroke geometry. Weight, sides, joins and dashes are CSS; the rest is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<StrokeProps>,
    /// Variable-width stroke profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_profile: Option<WidthProfile>,
    /// Scatter brush.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brush: Option<Brush>,
    /// An effect list entry that is present but switched off has no CSS spelling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_effects: Vec<u32>,
    /// An entry index in the fill list that is present but switched off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_fills: Vec<u32>,
    /// Blend mode, when it is `PassThrough` (an isolation flag, not a blend).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_isolation: Option<String>,

    // --- masks ---
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_mask: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_type: Option<MaskType>,

    // --- grids, guides, slices ---
    /// Design-time layout grid overlays. Not CSS Grid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grids: Vec<LayoutGrid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guides: Vec<Guide>,

    // --- export ---
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<ExportSetting>,

    // --- prototype ---
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interactions: Vec<Interaction>,
    /// Flow name when this node starts one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<String>,

    // --- components and variables ---
    /// Shared style links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub styles: Option<StyleRefs>,
    /// Variable bindings, keyed by engine field path such as `fills/0/color`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bound: BTreeMap<String, VariableId>,
    /// Explicitly selected mode per variable collection.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub var_modes: BTreeMap<CollectionId, ModeId>,
    /// The component an instance mirrors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<u64>,
    /// Property groups an instance overrides.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub overrides: std::collections::BTreeSet<String>,
    /// Which component property drives which node field.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prop_refs: BTreeMap<String, String>,

    // --- boolean geometry ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bool_op: Option<BoolOp>,

    // --- dev mode ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev_status: Option<DevStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_link: Option<figcraft_doc::CodeLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dev_resources: Vec<figcraft_doc::DevResource>,

    // --- editor facts with no CSS spelling ---
    /// Editor lock.
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    /// Collapsed contents, for a section.
    #[serde(default, skip_serializing_if = "is_false")]
    pub contents_hidden: bool,
    /// Fixed scroll position: stays put while the parent scrolls.
    #[serde(default, skip_serializing_if = "is_false")]
    pub scroll_position_fixed: bool,
    /// Free-form plugin and agent data.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugin_data: BTreeMap<String, String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One record's place in a metadata file.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordSpan {
    /// The byte offset the record starts at.
    start: usize,
    /// The byte offset just after the record's last line.
    end: usize,
    /// The indentation the record's `- ` is written at.
    indent: String,
}

/// The `nodes:` sequence of a metadata file.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NodesSequence {
    /// Each record's span, in the order the file lists them.
    records: Vec<RecordSpan>,
    /// The byte offset just after the last record.
    end: usize,
    /// The indentation the records are written at.
    indent: String,
}

/// The `nodes:` sequence of a metadata file, and where each record in it is.
///
/// Line-based, because that is what the file is: a block sequence of
/// records, one `- id:` each, with the keys of a record indented under it.
/// A file that is not shaped like that is refused rather than guessed at.
///
/// The gap between two records — a blank line, a comment — belongs to
/// neither of them, so a record that is rewritten does not take the
/// author's comment about the next one with it.
fn nodes_sequence(text: &str) -> Option<NodesSequence> {
    let mut records: Vec<RecordSpan> = Vec::new();
    let mut item_indent: Option<String> = None;
    let mut offset = 0usize;
    let mut in_sequence = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let content = trimmed.trim_start();
        let indent = trimmed[..trimmed.len() - content.len()].to_owned();
        let body = content.trim_end();
        if !in_sequence {
            if body == "nodes:" || body.starts_with("nodes: #") {
                in_sequence = true;
            }
            offset += line.len();
            continue;
        }
        // Inside the sequence: a blank line or a comment is the gap between
        // two records.
        if body.is_empty() || body.starts_with('#') {
            offset += line.len();
            continue;
        }
        let is_item = body == "-" || body.starts_with("- ");
        match &item_indent {
            None => {
                // The first record sets the indentation the sequence is
                // written at.
                if !is_item {
                    break;
                }
                item_indent = Some(indent.clone());
                records.push(RecordSpan {
                    start: offset,
                    end: offset + line.len(),
                    indent: indent.clone(),
                });
            }
            Some(current) => {
                if is_item && indent.len() == current.len() {
                    records.push(RecordSpan {
                        start: offset,
                        end: offset + line.len(),
                        indent: indent.clone(),
                    });
                } else if indent.len() > current.len() {
                    // A key of the record above, or a sequence inside it.
                    if let Some(record) = records.last_mut() {
                        record.end = offset + line.len();
                    }
                } else {
                    // Anything else ends the sequence.
                    break;
                }
            }
        }
        offset += line.len();
    }
    if !in_sequence {
        return None;
    }
    let indent = item_indent.unwrap_or_else(|| "  ".to_owned());
    let end = records.last().map(|record| record.end).unwrap_or(offset);
    Some(NodesSequence {
        records,
        end,
        indent,
    })
}

/// A record as the file writes it.
///
/// The YAML the schema's own writer produces, at the file's own
/// indentation: a record is a mapping, so its keys sit two spaces in from
/// the `- ` that introduces it.
fn encode_record(record: &NodeRecord, indent: &str) -> Result<String, ManifestError> {
    let yaml =
        noyalib_serde_yaml::to_string(record).map_err(|e| ManifestError::Yaml(e.to_string()))?;
    let key_indent = format!("{indent}  ");
    let mut out = String::new();
    for (index, line) in yaml.lines().enumerate() {
        if index == 0 {
            out.push_str(indent);
            out.push_str("- ");
        } else {
            out.push_str(&key_indent);
        }
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

/// Whether the engine's model lets this kind hold children.
///
/// Mirrors `figcraft_doc::NodeKind::is_container`. Keep the two in step: a kind
/// listed here but not there would let a manifest validate and then lose a subtree
/// at compile time.
pub fn is_container_kind(kind: &str) -> bool {
    matches!(
        kind,
        "page"
            | "Page"
            | "frame"
            | "Frame"
            | "group"
            | "Group"
            | "section"
            | "Section"
            | "booleanOp"
            | "BooleanOp"
            | "component"
            | "Component"
            | "componentSet"
            | "ComponentSet"
            | "instance"
            | "Instance"
    )
}

impl NodeRecord {
    /// The engine identity for this node.
    pub fn node_id(&self) -> NodeId {
        NodeId(self.id)
    }
}

/// The whole `lamine.yaml` document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Schema version. 1 is the pre-engine string-id format.
    pub version: u32,
    /// Every managed node, in any order. Hierarchy comes from `parent`/`children`.
    #[serde(default)]
    pub nodes: Vec<NodeRecord>,

    // --- document-level Owner B ---
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub styles: Vec<Style>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<Variable>,
    /// Variable collections, including their modes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<figcraft_doc::Collection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<Mode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<Comment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_profile: Option<ColorProfile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub document_colors: Vec<figcraft_doc::color::Color>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotation_categories: Vec<AnnotationCategory>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_presets: Vec<ImagePreset>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            nodes: Vec::new(),
            styles: Vec::new(),
            variables: Vec::new(),
            collections: Vec::new(),
            modes: Vec::new(),
            comments: Vec::new(),
            color_profile: None,
            document_colors: Vec::new(),
            annotation_categories: Vec::new(),
            image_presets: Vec::new(),
        }
    }
}

impl Manifest {
    /// Serialise to `lamine.yaml`.
    ///
    /// Validates first, so a manifest that cannot be compiled never reaches disk.
    pub fn to_yaml(&self) -> Result<String, ManifestError> {
        self.validate()?;
        let header = "# Spool project metadata.\n\
             #\n\
             # Identity, hierarchy and source bindings, plus the design-tool facts that\n\
             # HTML/CSS cannot express: auto-layout sizing, components, variables, styles,\n\
             # masks, grids, export settings and prototype data.\n\
             #\n\
             # This file must not duplicate anything authored in CSS. If a value can be read\n\
             # from a stylesheet, it does not belong here. See docs/02-document-and-source-model.md.\n";
        let body =
            noyalib_serde_yaml::to_string(self).map_err(|e| ManifestError::Yaml(e.to_string()))?;
        // `version` is emitted by serde as the first field; the header carries no
        // value so it cannot drift from MANIFEST_VERSION or appear twice.
        Ok(format!("{header}{body}"))
    }

    /// Parse `lamine.yaml`, accepting both version 1 and version 2.
    pub fn from_yaml(source: &str) -> Result<Self, ManifestError> {
        let raw: serde_json::Value =
            noyalib_serde_yaml::from_str(source).map_err(|e| ManifestError::Yaml(e.to_string()))?;
        let version = raw
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1) as u32;
        match version {
            1 => Self::from_v1(&raw),
            MANIFEST_VERSION => {
                serde_json::from_value(raw).map_err(|e| ManifestError::Yaml(e.to_string()))
            }
            found => Err(ManifestError::Version { found }),
        }
    }

    /// Merge a changed manifest into the file it was read from.
    ///
    /// The file is **edited**, not rewritten: a record that did not change
    /// keeps the author's own bytes — its comments, its quoting, its
    /// indentation — and only the records that did change are written out
    /// again. A file that is not shaped like a sequence of records is refused
    /// rather than guessed at, so a caller can write the whole thing again
    /// instead of corrupting it.
    ///
    /// The result is checked before it is returned: it has to parse back into
    /// the manifest that was asked for, or the merge is refused. A merge that
    /// quietly produced a different file would be worse than no merge.
    pub fn merge_into(text: &str, merged: &Self) -> Result<String, ManifestError> {
        let authored = Self::from_yaml(text)?;
        let Some(sequence) = nodes_sequence(text) else {
            return Err(ManifestError::Yaml(
                "the metadata file has no `nodes:` sequence to merge into".into(),
            ));
        };
        // The file's records and the manifest's are the same records in the
        // same order: the manifest was parsed out of the file.
        if sequence.records.len() != authored.nodes.len() {
            return Err(ManifestError::Yaml(
                "the metadata file's records and the manifest's do not line up".into(),
            ));
        }

        let mut out = String::new();
        let mut cursor = 0usize;
        for (span, record) in sequence.records.iter().zip(&authored.nodes) {
            let replacement = match merged.node_by_key(&record.key) {
                // The record is gone: take it out with its own lines.
                None => Some(String::new()),
                // The record did not change: the author's bytes are the
                // best ones there are.
                Some(new) if new == record => None,
                Some(new) => Some(encode_record(new, &span.indent)?),
            };
            let Some(replacement) = replacement else {
                continue;
            };
            out.push_str(&text[cursor..span.start]);
            out.push_str(&replacement);
            cursor = span.end;
        }

        // The records that are new go at the end of the sequence, where a
        // record added to the project belongs.
        let added: Vec<&NodeRecord> = merged
            .nodes
            .iter()
            .filter(|record| authored.node_by_key(&record.key).is_none())
            .collect();
        if !added.is_empty() {
            out.push_str(&text[cursor..sequence.end]);
            cursor = sequence.end;
            for record in added {
                out.push_str(&encode_record(record, &sequence.indent)?);
            }
        }
        out.push_str(&text[cursor..]);

        // The merged file has to say what it was meant to say.
        let back = Self::from_yaml(&out)?;
        if &back != merged {
            return Err(ManifestError::Yaml(
                "the merged metadata does not say what it was meant to say".into(),
            ));
        }
        Ok(out)
    }

    /// Upgrade a version 1 manifest by allocating model ids in document order.
    ///
    /// Version 1 nodes carried only a string id and the six required fields. The
    /// string becomes the `key`; a numeric `id` is allocated from 1 in file order.
    /// That allocation is arbitrary and stable only for a given file, which is
    /// sufficient because it is written back immediately.
    fn from_v1(raw: &serde_json::Value) -> Result<Self, ManifestError> {
        #[derive(Deserialize)]
        struct V1Node {
            id: String,
            name: String,
            kind: String,
            #[serde(default)]
            parent: Option<String>,
            #[serde(default)]
            children: Vec<String>,
            #[serde(default)]
            file: Option<String>,
            #[serde(default)]
            selector: Option<String>,
        }
        #[derive(Deserialize)]
        struct V1 {
            #[serde(default)]
            nodes: Vec<V1Node>,
        }
        let v1: V1 =
            serde_json::from_value(raw.clone()).map_err(|e| ManifestError::Yaml(e.to_string()))?;

        let mut index: BTreeMap<String, u64> = BTreeMap::new();
        for (i, n) in v1.nodes.iter().enumerate() {
            index.insert(n.id.clone(), i as u64 + 1);
        }
        let mut manifest = Manifest {
            version: MANIFEST_VERSION,
            ..Default::default()
        };
        for n in v1.nodes {
            let file = n.file.clone().unwrap_or_else(|| "index.html".to_string());
            let key = n.id.clone();
            manifest.nodes.push(NodeRecord {
                id: index[&key],
                key,
                name: n.name,
                kind: n.kind,
                parent: n.parent.as_ref().and_then(|p| index.get(p)).copied(),
                children: n
                    .children
                    .iter()
                    .filter_map(|c| index.get(c).copied())
                    .collect(),
                binding: SourceBinding {
                    file,
                    selector: n.selector.unwrap_or_default(),
                },
                ..NodeRecord::default()
            });
        }
        Ok(manifest)
    }

    /// Look up a node by its engine identity.
    pub fn node(&self, id: NodeId) -> Option<&NodeRecord> {
        self.nodes.iter().find(|n| n.id == id.0)
    }

    /// Look up a node by its authored source key.
    pub fn node_by_key(&self, key: &str) -> Option<&NodeRecord> {
        self.nodes.iter().find(|n| n.key == key)
    }

    /// Structural checks that must hold before the manifest can be compiled.
    ///
    /// Returns the first problem. This is deliberately exhaustive but
    /// fail-fast: a manifest is either compilable or it is not, and listing every
    /// downstream symptom of one broken id is noise.
    pub fn validate(&self) -> Result<(), ManifestError> {
        let mut seen_ids: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        let mut seen_keys: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();

        for node in &self.nodes {
            if !seen_ids.insert(node.id) {
                return Err(ManifestError::DuplicateId(node.id));
            }
            if node.key.is_empty() {
                return Err(ManifestError::MissingSourceKey { id: node.id });
            }
            if !seen_keys.insert(node.key.as_str()) {
                return Err(ManifestError::DuplicateSourceKey(node.key.clone()));
            }
            node.binding.validate(node.id)?;
            if node.parent == Some(node.id) {
                return Err(ManifestError::SelfParent { id: node.id });
            }
            // The engine's model gives children only to these kinds. A `Text` node
            // with children describes a tree that cannot exist, and the compiler
            // would drop the children with nothing but a per-node diagnostic --
            // which is a managed layer silently vanishing. Refuse it here instead,
            // before anything is written.
            if !node.children.is_empty() && !is_container_kind(&node.kind) {
                return Err(ManifestError::NotAContainer {
                    id: node.id,
                    kind: node.kind.clone(),
                });
            }
        }

        // Second pass: referential integrity, now that every id is known.
        for node in &self.nodes {
            if let Some(p) = node.parent {
                if !seen_ids.contains(&p) {
                    return Err(ManifestError::MissingParent {
                        id: node.id,
                        parent: p,
                    });
                }
            }
            for c in &node.children {
                if !seen_ids.contains(c) {
                    return Err(ManifestError::MissingChild {
                        id: node.id,
                        child: *c,
                    });
                }
            }
        }

        // A node whose parent does not list it is recoverable, not fatal: the
        // compiler reports it with the context needed to fix the ordering. Only
        // a dangling parent or child reference is a structural error, because
        // those mean the hierarchy cannot be walked at all.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u64, key: &str, parent: Option<u64>, children: Vec<u64>) -> NodeRecord {
        NodeRecord {
            id,
            key: key.to_string(),
            name: key.to_string(),
            kind: "Frame".to_string(),
            parent,
            children,
            binding: SourceBinding {
                file: "index.html".to_string(),
                selector: format!("[data-spool-id=\"{key}\"]"),
            },
            ..NodeRecord::default()
        }
    }

    fn page_with_child() -> Manifest {
        Manifest {
            nodes: vec![
                record(1, "root", None, vec![2]),
                record(2, "headline", Some(1), vec![]),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn empty_manifest_is_valid() {
        assert_eq!(Manifest::default().validate(), Ok(()));
    }

    #[test]
    fn round_trips_through_yaml() {
        let m = page_with_child();
        let yaml = m.to_yaml().expect("serialise");
        let back = Manifest::from_yaml(&yaml).expect("parse");
        assert_eq!(back, m);
    }

    #[test]
    fn yaml_carries_the_version_exactly_once() {
        let yaml = page_with_child().to_yaml().expect("serialise");
        assert_eq!(
            yaml.lines().filter(|l| l.starts_with("version:")).count(),
            1,
            "version emitted twice:\n{yaml}"
        );
    }

    #[test]
    fn yaml_does_not_duplicate_engine_identity_under_a_second_name() {
        let yaml = page_with_child().to_yaml().expect("serialise");
        // The model id and the source key are distinct fields, both present.
        let m = Manifest::from_yaml(&yaml).expect("parse");
        assert_eq!(m.node_by_key("headline").expect("key lookup").id, 2);
        assert_eq!(m.node(NodeId(2)).expect("id lookup").key, "headline");
    }

    #[test]
    fn rejects_duplicate_id() {
        let mut m = page_with_child();
        m.nodes.push(record(2, "other", Some(1), vec![]));
        assert_eq!(m.validate(), Err(ManifestError::DuplicateId(2)));
    }

    #[test]
    fn rejects_duplicate_source_key() {
        let mut m = page_with_child();
        m.nodes[1].key = "root".to_string();
        assert_eq!(
            m.validate(),
            Err(ManifestError::DuplicateSourceKey("root".to_string()))
        );
    }

    #[test]
    fn rejects_self_parent() {
        let mut m = page_with_child();
        m.nodes[1].parent = Some(2);
        assert_eq!(m.validate(), Err(ManifestError::SelfParent { id: 2 }));
    }

    #[test]
    fn rejects_dangling_parent() {
        let mut m = page_with_child();
        m.nodes[1].parent = Some(99);
        assert_eq!(
            m.validate(),
            Err(ManifestError::MissingParent { id: 2, parent: 99 })
        );
    }

    #[test]
    fn rejects_dangling_child() {
        let mut m = page_with_child();
        m.nodes[0].children.push(99);
        assert_eq!(
            m.validate(),
            Err(ManifestError::MissingChild { id: 1, child: 99 })
        );
    }

    #[test]
    fn rejects_binding_that_escapes_the_project_root() {
        let mut m = page_with_child();
        m.nodes[0].binding.file = "../elsewhere/index.html".to_string();
        assert_eq!(
            m.validate(),
            Err(ManifestError::BindingEscapesRoot {
                id: 1,
                file: "../elsewhere/index.html".to_string()
            })
        );
    }

    #[test]
    fn rejects_absolute_binding() {
        let mut m = page_with_child();
        m.nodes[0].binding.file = "/etc/passwd".to_string();
        assert_eq!(
            m.validate(),
            Err(ManifestError::BindingNotRelative { id: 1 })
        );
    }

    #[test]
    fn rejects_a_child_under_a_kind_the_engine_cannot_give_children_to() {
        // `Text` is the only kind that carries characters, and the engine's model
        // gives it no children -- so this tree cannot exist. The compiler would
        // drop the child with only a per-node diagnostic, which is a managed layer
        // silently vanishing.
        let mut m = page_with_child();
        m.nodes[0].kind = "Text".to_string();
        assert_eq!(
            m.validate(),
            Err(ManifestError::NotAContainer {
                id: 1,
                kind: "Text".to_string()
            })
        );
    }

    #[test]
    fn a_leaf_kind_with_no_children_is_fine() {
        let mut m = page_with_child();
        m.nodes[0].kind = "Text".to_string();
        m.nodes[0].children.clear();
        m.nodes[1].parent = None;
        assert_eq!(m.validate(), Ok(()));
    }

    #[test]
    fn rejects_unknown_version() {
        let yaml = "version: 99\nnodes: []\n";
        assert_eq!(
            Manifest::from_yaml(yaml),
            Err(ManifestError::Version { found: 99 })
        );
    }

    #[test]
    fn upgrades_version_1_by_allocating_ids_in_document_order() {
        // Version 1 is the line-oriented format app/src/source_document.rs reads:
        // exact key order, quoted scalars, and flow lists for `children`.
        let v1 = "\
version: 1
nodes:
  - id: spool-frame-root
    name: Root
    kind: Frame
    parent: ''
    children: [spool-text-headline]
    file: index.html
    selector: '[data-spool-id=\"spool-frame-root\"]'
  - id: spool-text-headline
    name: Headline
    kind: Text
    parent: spool-frame-root
    children: []
    file: index.html
    selector: '[data-spool-id=\"spool-text-headline\"]'
";
        let m = Manifest::from_yaml(v1).expect("parse v1");
        assert_eq!(m.version, MANIFEST_VERSION);
        assert_eq!(m.nodes.len(), 2);

        let root = m.node_by_key("spool-frame-root").expect("root");
        let head = m.node_by_key("spool-text-headline").expect("headline");
        assert_eq!(root.id, 1);
        assert_eq!(head.id, 2);
        assert_eq!(head.parent, Some(1));
        assert_eq!(root.children, vec![2]);
        assert_eq!(m.validate(), Ok(()), "an upgraded v1 must be valid");
    }

    #[test]
    fn upgraded_v1_keeps_its_source_keys_and_bindings() {
        let v1 = "\
version: 1
nodes:
  - id: keep-me
    name: Kept
    kind: Frame
    parent: ''
    children: []
    file: pages/home.html
    selector: '[data-spool-id=\"keep-me\"]'
";
        let m = Manifest::from_yaml(v1).expect("parse");
        let n = m.node_by_key("keep-me").expect("node");
        assert_eq!(n.binding.file, "pages/home.html");
        assert_eq!(n.binding.selector, "[data-spool-id=\"keep-me\"]");
        assert_eq!(n.name, "Kept");
    }

    #[test]
    fn owner_b_fields_survive_the_round_trip() {
        let mut m = page_with_child();
        m.nodes[1].locked = true;
        m.nodes[1].is_mask = true;
        m.nodes[1].mask_type = Some(MaskType::Luminance);
        m.nodes[0].dev_status = Some(DevStatus::ReadyForDev);
        m.nodes[0]
            .bound
            .insert("fills/0/color".to_string(), "VariableId:1".to_string());
        m.color_profile = Some(ColorProfile::DisplayP3);

        let back = Manifest::from_yaml(&m.to_yaml().expect("serialise")).expect("parse");
        assert_eq!(back, m);
        assert!(back.nodes[1].locked);
        assert_eq!(back.nodes[1].mask_type, Some(MaskType::Luminance));
    }

    /// A metadata file, written the way a person writes one.
    const AUTHORED: &str = r#"version: 2
# A comment the author wrote about the project.
nodes:
  - id: 1
    key: root
    name: Root
    kind: frame
    children:
      - 2
    binding:
      file: index.html
      selector: "[data-spool-id=\"root\"]"

  # A comment about the second record.
  - id: 2
    key: child
    name: Child
    kind: frame
    parent: 1
    binding:
      file: index.html
      selector: "[data-spool-id=\"child\"]"

color_profile: srgb
"#;

    #[test]
    fn a_merge_leaves_the_authors_bytes_alone() {
        // The file is edited, not rewritten: the comment the author wrote
        // stays, and only the record that changed is written out again.
        let mut merged = Manifest::from_yaml(AUTHORED).expect("the file parses");
        merged.nodes[0].children.push(3);
        merged.nodes.push(NodeRecord {
            id: 3,
            key: "new".to_owned(),
            name: "New".to_owned(),
            kind: "rectangle".to_owned(),
            parent: Some(1),
            children: Vec::new(),
            binding: SourceBinding {
                file: "index.html".to_owned(),
                selector: "[data-spool-id=\"new\"]".to_owned(),
            },
            ..NodeRecord::default()
        });

        let out = Manifest::merge_into(AUTHORED, &merged).expect("the file merges");
        assert!(
            out.contains("# A comment the author wrote about the project."),
            "the header comment survives: {out}"
        );
        assert!(
            out.contains("# A comment about the second record."),
            "and so does the one between two records: {out}"
        );
        assert!(
            out.contains("  - id: 2\n    key: child\n"),
            "and the record that did not change keeps its own bytes: {out}"
        );
        assert!(
            out.contains("key: new"),
            "and the new record is there: {out}"
        );
        assert!(
            out.contains("color_profile: srgb"),
            "and so does the document-level fact: {out}"
        );
        assert_eq!(
            Manifest::from_yaml(&out).expect("the merged file parses"),
            merged,
            "the merged file says what it was meant to say"
        );
    }

    #[test]
    fn a_merge_of_an_unchanged_manifest_is_the_same_file() {
        let manifest = Manifest::from_yaml(AUTHORED).expect("the file parses");
        let out = Manifest::merge_into(AUTHORED, &manifest).expect("the file merges");
        assert_eq!(out, AUTHORED, "nothing changed, so nothing moved");
    }

    #[test]
    fn a_removed_record_is_taken_out_with_its_own_lines() {
        let mut merged = Manifest::from_yaml(AUTHORED).expect("the file parses");
        merged.nodes.remove(1);
        merged.nodes[0].children.clear();

        let out = Manifest::merge_into(AUTHORED, &merged).expect("the file merges");
        assert!(!out.contains("key: child"), "the record is gone: {out}");
        assert!(
            out.contains("# A comment the author wrote about the project."),
            "and the author's own text is still there: {out}"
        );
        assert_eq!(
            Manifest::from_yaml(&out).expect("the merged file parses"),
            merged
        );
    }

    #[test]
    fn a_file_that_is_not_a_sequence_of_records_is_refused() {
        // Rather than guessed at: a file this shape cannot be merged into
        // has to be written out again, not edited.
        let text = "version: 2\nnodes: []\n";
        assert!(Manifest::merge_into(text, &Manifest::default()).is_err());
    }
}
