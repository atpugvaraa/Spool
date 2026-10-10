//! Persistent source-backed document primitives. This module deliberately
//! models Spool structure and provenance; authored content stays in source.

use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        if value.is_empty()
            || !value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(ModelError::InvalidId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceBinding {
    pub file: String,
    pub selector: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructuralNode {
    pub id: NodeId,
    pub name: String,
    pub kind: String,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub source: SourceBinding,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LamineStructure {
    pub nodes: Vec<StructuralNode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelError {
    InvalidId(String),
    DuplicateId(String),
    DuplicateName(String),
    MissingNode(String),
    InvalidYaml(String),
    InvalidOperation(String),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidId(value) => write!(f, "{value:?} is not a valid node ID"),
            Self::DuplicateId(value) => write!(f, "duplicate node ID {value:?}"),
            Self::DuplicateName(value) => write!(f, "duplicate node name {value:?}"),
            Self::MissingNode(value) => write!(f, "reference to unknown node {value:?}"),
            Self::InvalidYaml(detail) => write!(f, "{detail}"),
            Self::InvalidOperation(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for ModelError {}

impl LamineStructure {
    /// `root` and every node beneath it, deepest first.
    ///
    /// Post-order because that is the only order in which a cascade can be
    /// applied without ever orphaning anyone: a child is always removed before
    /// its parent, so no node is ever left naming a parent that has gone.
    ///
    /// Uses the recorded `children` rather than scanning `parent`, because that
    /// is the edge the structure owns; a `parent` left dangling by a removal
    /// would otherwise widen the subtree without bound.
    pub fn subtree_post_order(&self, root: &NodeId) -> Vec<StructuralNode> {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        self.walk_post_order(root, &mut seen, &mut found);
        found
    }

    fn walk_post_order(
        &self,
        id: &NodeId,
        seen: &mut HashSet<NodeId>,
        found: &mut Vec<StructuralNode>,
    ) {
        // A cycle in a malformed structure would otherwise recurse forever.
        if !seen.insert(id.clone()) {
            return;
        }
        let Some(node) = self.nodes.iter().find(|node| node.id == *id) else {
            return;
        };
        // Children first, and the node itself only once they are all recorded:
        // that is what makes the result safe to remove in order.
        for child in &node.children {
            self.walk_post_order(child, seen, found);
        }
        found.push(node.clone());
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for node in &self.nodes {
            if !ids.insert(node.id.clone()) {
                return Err(ModelError::DuplicateId(node.id.0.clone()));
            }
            if node.name.trim().is_empty() || !names.insert(node.name.clone()) {
                return Err(ModelError::DuplicateName(node.name.clone()));
            }
            if node.source.file.trim().is_empty() || node.source.selector.trim().is_empty() {
                return Err(ModelError::InvalidOperation(format!(
                    "node {} has an empty source binding",
                    node.id.0
                )));
            }
        }
        for node in &self.nodes {
            if let Some(parent) = &node.parent {
                if !ids.contains(parent) {
                    return Err(ModelError::MissingNode(parent.0.clone()));
                }
            }
            for child in &node.children {
                if !ids.contains(child) {
                    return Err(ModelError::MissingNode(child.0.clone()));
                }
            }
        }
        Ok(())
    }

    /// Read and write the project's deliberately small, versioned YAML
    /// contract. Quoted scalars use JSON escaping, which is valid YAML.
    pub fn from_yaml(input: &str) -> Result<Self, ModelError> {
        let mut lines = input.lines().filter(|line| !line.trim().is_empty());
        if lines.next().map(str::trim) != Some("version: 1")
            || lines.next().map(str::trim) != Some("nodes:")
        {
            return Err(ModelError::InvalidYaml(
                "expected version: 1 and nodes:".into(),
            ));
        }
        let lines: Vec<_> = lines.collect();
        let mut nodes = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let id = field(lines[i], "  - id: ")?;
            i += 1;
            let name = field_at(&lines, &mut i, "    name: ")?;
            let kind = field_at(&lines, &mut i, "    kind: ")?;
            let parent_value = field_at(&lines, &mut i, "    parent: ")?;
            let file = field_at(&lines, &mut i, "    file: ")?;
            let selector = field_at(&lines, &mut i, "    selector: ")?;
            let children = field_at(&lines, &mut i, "    children: ")?;
            let id = NodeId::new(scalar(id)?)?;
            let parent = if parent_value.trim() == "null" {
                None
            } else {
                Some(NodeId::new(scalar(parent_value)?)?)
            };
            nodes.push(StructuralNode {
                id,
                name: scalar(name)?,
                kind: scalar(kind)?,
                parent,
                children: parse_children(children)?,
                source: SourceBinding {
                    file: scalar(file)?,
                    selector: scalar(selector)?,
                },
            });
        }
        let structure = Self { nodes };
        structure.validate()?;
        Ok(structure)
    }

    pub fn to_yaml(&self) -> Result<String, ModelError> {
        self.validate()?;
        let mut out = String::from("version: 1\nnodes:\n");
        for node in &self.nodes {
            out.push_str(&format!("  - id: {}\n", quote(&node.id.0)));
            out.push_str(&format!("    name: {}\n", quote(&node.name)));
            out.push_str(&format!("    kind: {}\n", quote(&node.kind)));
            out.push_str(&format!(
                "    parent: {}\n",
                node.parent
                    .as_ref()
                    .map(|id| quote(&id.0))
                    .unwrap_or_else(|| "null".into())
            ));
            out.push_str(&format!("    file: {}\n", quote(&node.source.file)));
            out.push_str(&format!("    selector: {}\n", quote(&node.source.selector)));
            let children = node
                .children
                .iter()
                .map(|id| quote(&id.0))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("    children: [{children}]\n"));
        }
        Ok(out)
    }
}

fn field<'a>(line: &'a str, prefix: &str) -> Result<&'a str, ModelError> {
    line.strip_prefix(prefix)
        .ok_or_else(|| ModelError::InvalidYaml(format!("expected {prefix}")))
}

fn field_at<'a>(lines: &[&'a str], i: &mut usize, prefix: &str) -> Result<&'a str, ModelError> {
    let line = lines
        .get(*i)
        .ok_or_else(|| ModelError::InvalidYaml(format!("missing {prefix}")))?;
    *i += 1;
    field(line, prefix)
}

fn scalar(value: &str) -> Result<String, ModelError> {
    serde_json::from_str(value)
        .map_err(|_| ModelError::InvalidYaml(format!("expected quoted scalar, got {value}")))
}

fn parse_children(value: &str) -> Result<Vec<NodeId>, ModelError> {
    let body = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .ok_or_else(|| ModelError::InvalidYaml("children must be a flow list".into()))?
        .trim();
    if body.is_empty() {
        return Ok(Vec::new());
    }
    body.split(", ")
        .map(|entry| scalar(entry).and_then(NodeId::new))
        .collect()
}

fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HtmlSource {
    pub file: String,
    pub contents: String,
}

impl HtmlSource {
    /// Verify an authored node has the explicit identity marker expected by
    /// its metadata binding. General CSS selector evaluation is intentionally
    /// deferred; the first source contract uses data-spool-id selectors.
    pub fn binding_is_present(&self, node: &StructuralNode) -> bool {
        self.binding_occurrences(node) > 0
    }

    /// Count the authored identity markers a node's binding resolves to.
    /// Callers distinguish zero (no match) from more than one (ambiguous);
    /// an ambiguous binding must be reported rather than resolved by guessing.
    pub fn binding_occurrences(&self, node: &StructuralNode) -> usize {
        if self.file != node.source.file {
            return 0;
        }
        let expected_selector = format!("[data-spool-id=\"{}\"]", node.id.as_str());
        if node.source.selector != expected_selector {
            return 0;
        }
        let marker = format!("data-spool-id=\"{}\"", node.id.as_str());
        let single_quoted = format!("data-spool-id='{}'", node.id.as_str());
        self.contents.matches(&marker).count() + self.contents.matches(&single_quoted).count()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersistentDocument {
    pub structure: LamineStructure,
    pub sources: HashMap<String, String>,
}

/// State owned by the editor session. This is intentionally separate from
/// PersistentDocument and has no persistence encoder.
///
/// **This is not the live editor state.** The running editor keeps its
/// selection and camera on the `Canvas` entity as `canvas::Selection` (holding
/// `Vec<ObjectId>` runtime keys) and `canvas::Camera`. This type is a
/// model-level statement of the boundary in `docs/02`: it exists so the model
/// tests can assert that selection, camera, and tool never enter persistent
/// state, and it deliberately has no production caller.
///
/// Note the difference in element type is intentional rather than a mismatch:
/// a live view selects runtime keys because that is what it can hit-test,
/// while anything that must survive a reload refers to persistent `NodeId`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditorRuntimeState {
    pub selection: Vec<NodeId>,
    pub camera_offset: (i32, i32),
    pub active_tool: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameNode {
    pub id: NodeId,
    pub before: String,
    pub after: String,
}

impl RenameNode {
    pub fn apply(&self, document: &mut PersistentDocument) -> Result<(), ModelError> {
        if self.before == self.after {
            return Ok(());
        }
        if document
            .structure
            .nodes
            .iter()
            .any(|n| n.id != self.id && n.name == self.after)
        {
            return Err(ModelError::DuplicateName(self.after.clone()));
        }
        let node = document
            .structure
            .nodes
            .iter_mut()
            .find(|n| n.id == self.id)
            .ok_or_else(|| ModelError::MissingNode(self.id.0.clone()))?;
        if node.name != self.before {
            return Err(ModelError::InvalidOperation(
                "rename source is stale".into(),
            ));
        }
        node.name.clone_from(&self.after);
        Ok(())
    }

    pub fn inverse(&self) -> Self {
        Self {
            id: self.id.clone(),
            before: self.after.clone(),
            after: self.before.clone(),
        }
    }
}

// NOTE: this module deliberately has no history type of its own.
//
// An earlier `SemanticHistory` lived here, holding `Vec<RenameNode>` stacks.
// It was a second implementation of the same concept as
// `crate::operations::SemanticHistory`, and having two types with that name
// made it unclear which stack an undo belonged to. It had no production
// caller. History now lives in exactly one place: `crate::operations`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::{apply, OperationTarget, SemanticHistory, SemanticOperation};

    fn fixture() -> LamineStructure {
        let root = NodeId::new("spool-root-001").unwrap();
        let child = NodeId::new("spool-child-002").unwrap();
        LamineStructure {
            nodes: vec![
                StructuralNode {
                    id: root.clone(),
                    name: "Landing".into(),
                    kind: "frame".into(),
                    parent: None,
                    children: vec![child.clone()],
                    source: SourceBinding {
                        file: "index.html".into(),
                        selector: "[data-spool-id=\"spool-root-001\"]".into(),
                    },
                },
                StructuralNode {
                    id: child,
                    name: "Title".into(),
                    kind: "text".into(),
                    parent: Some(root),
                    children: vec![],
                    source: SourceBinding {
                        file: "index.html".into(),
                        selector: "[data-spool-id=\"spool-child-002\"]".into(),
                    },
                },
            ],
        }
    }

    /// A root with a child, and that child with a child of its own.
    fn deep_fixture() -> LamineStructure {
        let root = NodeId::new("spool-root-001").unwrap();
        let middle = NodeId::new("spool-middle-002").unwrap();
        let leaf = NodeId::new("spool-leaf-003").unwrap();
        let bind = |id: &NodeId| SourceBinding {
            file: "index.html".into(),
            selector: format!("[data-spool-id=\"{}\"]", id.as_str()),
        };
        LamineStructure {
            nodes: vec![
                StructuralNode {
                    id: root.clone(),
                    name: "Root".into(),
                    kind: "frame".into(),
                    parent: None,
                    children: vec![middle.clone()],
                    source: bind(&root),
                },
                StructuralNode {
                    id: middle.clone(),
                    name: "Middle".into(),
                    kind: "frame".into(),
                    parent: Some(root),
                    children: vec![leaf.clone()],
                    source: bind(&middle),
                },
                StructuralNode {
                    id: leaf.clone(),
                    name: "Leaf".into(),
                    kind: "text".into(),
                    parent: Some(middle),
                    children: vec![],
                    source: bind(&leaf),
                },
            ],
        }
    }

    #[test]
    fn a_subtree_lists_children_before_their_parent() {
        // The order is the whole point: a cascade is applied in this order so that
        // no node is ever removed while another still names it as parent. A
        // pre-order walk looks right and leaves exactly the dangling reference this
        // is meant to prevent.
        let structure = deep_fixture();
        let root = NodeId::new("spool-root-001").unwrap();
        let order: Vec<String> = structure
            .subtree_post_order(&root)
            .into_iter()
            .map(|node| node.id.as_str().to_owned())
            .collect();
        assert_eq!(
            order,
            vec!["spool-leaf-003", "spool-middle-002", "spool-root-001"],
            "deepest first, and the root last"
        );
    }

    #[test]
    fn a_subtree_starts_from_any_node_not_only_a_root() {
        let structure = deep_fixture();
        let middle = NodeId::new("spool-middle-002").unwrap();
        let order: Vec<String> = structure
            .subtree_post_order(&middle)
            .into_iter()
            .map(|node| node.id.as_str().to_owned())
            .collect();
        assert_eq!(order, vec!["spool-leaf-003", "spool-middle-002"]);
    }

    #[test]
    fn a_subtree_of_a_cyclic_structure_terminates() {
        // A cycle cannot survive validation, but this walk is reachable before
        // that and must not hang the editor on a malformed structure.
        let mut structure = deep_fixture();
        let leaf = NodeId::new("spool-leaf-003").unwrap();
        let root = NodeId::new("spool-root-001").unwrap();
        structure
            .nodes
            .iter_mut()
            .find(|node| node.id == leaf)
            .expect("the leaf is there")
            .children
            .push(root.clone());

        let order = structure.subtree_post_order(&root);
        assert_eq!(
            order.len(),
            3,
            "each node is visited once despite the cycle back to the root"
        );
    }

    #[test]
    fn lamine_round_trip_preserves_ids_hierarchy_and_source_bindings() {
        let structure = fixture();
        let encoded = structure.to_yaml().unwrap();
        assert_eq!(LamineStructure::from_yaml(&encoded).unwrap(), structure);
    }

    #[test]
    fn metadata_rejects_duplicate_ids_names_and_dangling_children() {
        let mut structure = fixture();
        structure.nodes[1].id = structure.nodes[0].id.clone();
        assert!(matches!(
            structure.validate(),
            Err(ModelError::DuplicateId(_))
        ));
        let mut structure = fixture();
        structure.nodes[1].name = structure.nodes[0].name.clone();
        assert!(matches!(
            structure.validate(),
            Err(ModelError::DuplicateName(_))
        ));
        let mut structure = fixture();
        structure.nodes[0].children = vec![NodeId::new("missing").unwrap()];
        assert!(matches!(
            structure.validate(),
            Err(ModelError::MissingNode(_))
        ));
    }

    #[test]
    fn html_binding_resolves_by_stable_spool_identity() {
        let structure = fixture();
        let source = HtmlSource {
            file: "index.html".into(),
            contents: "<h1 data-spool-id=\"spool-child-002\">Canonical content</h1>".into(),
        };
        assert!(source.binding_is_present(&structure.nodes[1]));
        assert!(!source.binding_is_present(&structure.nodes[0]));
    }

    #[test]
    fn semantic_rename_is_reversible_and_stale_or_duplicate_edits_fail_cleanly() {
        let mut document = PersistentDocument {
            structure: fixture(),
            sources: HashMap::new(),
        };
        let operation = RenameNode {
            id: NodeId::new("spool-child-002").unwrap(),
            before: "Title".into(),
            after: "Hero title".into(),
        };
        operation.apply(&mut document).unwrap();
        operation.inverse().apply(&mut document).unwrap();
        assert_eq!(document.structure.nodes[1].name, "Title");
        let duplicate = RenameNode {
            id: operation.id.clone(),
            before: "Title".into(),
            after: "Landing".into(),
        };
        let before = document.clone();
        assert!(duplicate.apply(&mut document).is_err());
        assert_eq!(document, before);
    }

    #[test]
    fn semantic_history_commits_undoes_redoes_and_clears_redo_on_new_edit() {
        let mut document = PersistentDocument {
            structure: fixture(),
            sources: HashMap::from([("styles.css".into(), ".title { color: red; }\n".into())]),
        };
        // The one and only history implementation, replayed against metadata
        // alone via the metadata-only target.
        let mut history = SemanticHistory::default();
        let rename = |before: &str, after: &str| RenameNode {
            id: NodeId::new("spool-child-002").unwrap(),
            before: before.into(),
            after: after.into(),
        };
        let commit =
            |history: &mut SemanticHistory, document: &mut PersistentDocument, op: RenameNode| {
                assert!(history.record(SemanticOperation::Rename(op.clone())));
                apply(
                    &SemanticOperation::Rename(op),
                    &mut OperationTarget::Document(document),
                    crate::canvas::ReplayDirection::Redo,
                )
                .expect("the rename applies")
            };

        commit(&mut history, &mut document, rename("Title", "Hero"));
        assert_eq!(document.structure.nodes[1].name, "Hero");
        assert_eq!(document.sources["styles.css"], ".title { color: red; }\n");

        assert!(history
            .undo(&mut OperationTarget::Document(&mut document))
            .unwrap());
        assert_eq!(document.structure.nodes[1].name, "Title");
        assert!(history.can_redo());
        assert!(history
            .redo(&mut OperationTarget::Document(&mut document))
            .unwrap());
        assert_eq!(document.structure.nodes[1].name, "Hero");
        assert!(history
            .undo(&mut OperationTarget::Document(&mut document))
            .unwrap());

        commit(&mut history, &mut document, rename("Title", "Heading"));
        assert!(!history.can_redo(), "a new commit clears the redo branch");
        assert_eq!(document.structure.nodes[1].name, "Heading");
    }

    #[test]
    fn editor_runtime_state_is_not_part_of_persistent_document() {
        let runtime = EditorRuntimeState {
            selection: vec![NodeId::new("spool-root-001").unwrap()],
            camera_offset: (18, 24),
            active_tool: "select".into(),
        };
        let document = PersistentDocument {
            structure: fixture(),
            sources: HashMap::new(),
        };
        let saved_state = document.clone();
        assert_eq!(runtime.selection.len(), 1);
        assert_eq!(runtime.camera_offset, (18, 24));
        assert_eq!(runtime.active_tool, "select");
        assert_eq!(document, saved_state);
    }
}
