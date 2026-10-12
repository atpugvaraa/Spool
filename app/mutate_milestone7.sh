#!/usr/bin/env bash
# Mutation check for the Agent 7 guarantees.
#
# Each mutation breaks one specific behaviour the milestone claims, runs the
# tests that are supposed to catch it, and requires the suite to FAIL. A
# mutation that survives means the corresponding test is not really testing the
# claim, which is worse than having no test at all.
set -uo pipefail

cd "$(dirname "$0")" || exit 1

# name | file | python replacement | test filter
# Every mutation is expected to be caught, so a surviving one is a failure.
# `set -e` is deliberately not used: a failing `cargo test` is the expected
# outcome here, not an error in the script.
failures=0

run_mutation() {
  local name="$1" file="$2" script="$3" filter="$4"
  local backup result
  backup="$(mktemp)"
  cp "$file" "$backup"

  python3 - "$file" <<PYEOF
import sys
path = sys.argv[1]
source = open(path).read()
$script
open(path, "w").write(source)
PYEOF

  if ! cmp -s "$backup" "$file"; then
    if cargo test --offline "$filter" >/tmp/spool-mutation.log 2>&1; then
      echo "SURVIVED  $name — the suite still passed with this broken"
      failures=$((failures + 1))
      result=1
    else
      echo "caught    $name"
      result=0
    fi
  else
    echo "NO-OP     $name — the mutation did not change the source"
    failures=$((failures + 1))
    result=1
  fi

  cp "$backup" "$file"
  rm -f "$backup"
  return $result
}

failures=0

# 1. The NodeId allocator no longer skips identities that are already live.
run_mutation "allocate_node_id stops checking live identities" \
  src/canvas.rs \
  'old = """            if !self
                .objects
                .iter()
                .any(|object| object.spool_id == candidate)
            {
                return candidate;
            }"""
new = """            return candidate;"""
assert old in source, "allocator pattern not found"
source = source.replace(old, new, 1)' \
  "a_new_identity_is_never_one_that_is_already_live"

# 2. Save ignores that an object's position changed.
run_mutation "save ignores a moved object" \
  src/canvas.rs \
  'old = """                let moved = before.position != geometry.position;"""
new = """                let moved = false;"""
assert old in source, "geometry diff pattern not found"
source = source.replace(old, new, 1)' \
  "a_moved_object_survives_undo_redo_and_then_the_save_loop"

# 3. Save stops reporting objects that have no authored element.
run_mutation "save silently drops session-created objects" \
  src/canvas.rs \
  'old = """                outcome
                    .unsupported
                    .push(crate::project_save::UnsupportedEdit {
                        node: object.spool_id.clone(),
                        kind: "object",
                        reason: "created in this session, so it has no authored element yet".into(),
                    });"""
new = """                continue;"""
assert old in source, "unsupported pattern not found"
source = source.replace(old, new, 1)' \
  "a_session_created_object_is_reported_rather_than_silently_dropped"

# 4. Text no longer carries the authored colour into the renderer.
run_mutation "authored text colour is dropped" \
  src/document_runtime_bridge.rs \
  'old = """                    text_color: node.text_color,"""
new = """                    text_color: None,"""
assert old in source, "text_color bridge pattern not found"
source = source.replace(old, new, 1)' \
  "a_loaded_project_renders_its_authored_text_style_and_geometry"

# 5. Authored text is not extracted at all.
run_mutation "authored text is not extracted" \
  src/document_runtime_bridge.rs \
  'old = """                    text_content: node.text.clone(),"""
new = """                    text_content: None,"""
assert old in source, "text_content bridge pattern not found"
source = source.replace(old, new, 1)' \
  "a_loaded_project_renders_its_authored_text_style_and_geometry"

# 6. Inheritance through an unmanaged wrapper is dropped.
run_mutation "wrapper text colour no longer inherits" \
  src/visual.rs \
  'old = """                .or_else(|| {
                    ancestry.get(&id).and_then(|tags| {"""
new = """                .or_else(|| {
                    None::<String>.map(|value: String| { ancestry.get(&id).and_then(|tags| {"""
assert old in source, "wrapper inheritance pattern not found"
source = source.replace(old, new, 1)
old2 = """                        })
                    })
                });"""
new2 = """                        })
                    }); value })
                });"""
assert old2 in source, "wrapper inheritance tail not found"
source = source.replace(old2, new2, 1)' \
  "a_wrapper_carries_its_text_colour_into_the_nodes_inside_it"

# 7. Style ownership is assumed instead of checked.
run_mutation "style ownership is not checked" \
  src/project_save.rs \
  'old = """    let html = document.sources.get(&node.source.file)?;"""
new = """    let _ = node;\n    let html = document.sources.get(&node.source.file)?;"""
assert old in source, "ownership guard pattern not found"
source = source.replace(old, new, 1)
old2 = """        let sheet = Stylesheet::parse(contents);\n        for spelling in ownership_spellings(property) {\n            if let Some(range) = sheet.declaration_value_range(&tag, &classes, None, spelling) {\n                let authored = contents.get(range.0..range.1).unwrap_or(\"\").to_owned();\n                return Some(StyleTarget::Declaration {\n                    file: name.clone(),\n                    range,\n                    authored,\n                });\n            }\n        }"""
new2 = """        let _ = (contents, tag, classes);\n        if false {\n            unreachable!();\n        }"""
assert old2 in source, "ownership loop pattern not found"
source = source.replace(old2, new2, 1)' \
  "a_style_edit_rewrites_the_owning_declaration_only"

# 8. An existing style attribute is duplicated instead of replaced.
run_mutation "existing style attribute is duplicated" \
  src/project_save.rs \
  'old = """                Some(range) => {
                    replacements.push((absolute + range.start..absolute + range.end, merged))
                }"""
new = """                Some(_range) => {
                    let insert_at = absolute + open_end;
                    replacements.push((insert_at..insert_at, format!(" style=\\"{merged}\\"")));
                }"""
assert old in source, "style attribute replace pattern not found"
source = source.replace(old, new, 1)' \
  "a_geometry_edit_survives_the_round_trip"

# 9. HTML edits are applied front to back, so later ranges land in the wrong place.
run_mutation "html edits are applied in the wrong order" \
  src/project_save.rs \
  'old = """    replacements.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));"""
new = """    replacements.sort_by_key(|(range, _)| range.start);"""
assert old in source, "html ordering pattern not found"
source = source.replace(old, new, 1)' \
  "two_edits_to_the_same_element_both_land"

# 10. An explicit width is read as a content box again, so geometry drifts on reopen.
run_mutation "explicit dimensions read as content boxes" \
  src/visual.rs \
  'old = """    let outer_width = match explicit_width {
        Some(width) if content_box => width + padding[1] + padding[3],
        Some(width) => width,
        None => available_width,
    };"""
new = """    let _ = content_box;
    let outer_width = match explicit_width {
        Some(width) => width + padding[1] + padding[3],
        None => available_width,
    };"""
assert old in source, "box-sizing width pattern not found"
source = source.replace(old, new, 1)' \
  "a_geometry_edit_survives_the_round_trip"

# 11. Attribute facts are read from the whole element, so a child styles its parent.
run_mutation "attributes are read from the whole element" \
  src/visual.rs \
  'old = """    let open_tag_end = fragment.find('"'"'>'"'"').unwrap_or(fragment.len());
    (tag, attribute_values(&fragment[..open_tag_end], "class"))"""
new = """    (tag, attribute_values(fragment, "class"))"""
assert old in source, "class scope pattern not found"
source = source.replace(old, new, 1)' \
  "a_parent_never_inherits_its_childs_class_or_text"

# 12. Metadata is never written, so a rename does not survive.
run_mutation "renames are not persisted" \
  src/project_save.rs \
  'old = """    if on_disk.document.structure != updated.structure {"""
new = """    if false {"""
assert old in source, "metadata write guard pattern not found"
source = source.replace(old, new, 1)' \
  "a_rename_made_in_the_editor_is_written_to_the_metadata"

# ---------------------------------------------------------------------------
# Agent 8 — the text, style, layout and rename guarantees.
#
# Same contract as the list above: every mutation below breaks one claim the
# milestone makes, and the tests named are the ones that are supposed to catch
# it. A survivor means the claim is not actually covered.
# ---------------------------------------------------------------------------

# 13. A text edit writes the whole element content instead of the owned range.
run_mutation "text is written as the element's whole content" \
  src/project_save.rs \
  'old = """        if !original.is_char_boundary(range.start) || !original.is_char_boundary(range.end) {
            continue;
        }
        replacements.push((range, escape_text(text)));"""
new = """        let Some(binding) = index.find(node) else { continue };
        replacements.push((binding.element_range.clone(), escape_text(text)));"""
assert old in source, "text replacement pattern not found"
source = source.replace(old, new, 1)' \
  "saving_a_text_edit_leaves_every_other_byte_alone"

# 14. An element owns its text even when its content is not one run of text.
run_mutation "an element with mixed content owns text anyway" \
  src/source_binding.rs \
  'old = """            if child.kind() != \"text\" {
                return None;
            }"""
new = """            let _ = child;"""
assert old in source, "own_text_range guard pattern not found"
source = source.replace(old, new, 1)' \
  "a_bound_element_owns_exactly_the_text_the_author_wrote"

# 15. An inherited property is written on the ancestor instead of the element.
run_mutation "an inherited style edit is written on the ancestor" \
  src/project_save.rs \
  'old = """                    Some(StyleTarget::Element { .. }) => {\n                        inline\n                            .entry(file)\n                            .or_default()\n                            .entry(node_id.clone())"""
new = """                    Some(StyleTarget::Element { .. }) => {\n                        let owner = document\n                            .structure\n                            .nodes\n                            .iter()\n                            .find(|candidate| candidate.id == node_id)\n                            .and_then(|candidate| candidate.parent.clone())\n                            .unwrap_or_else(|| node_id.clone());\n                        inline\n                            .entry(file)\n                            .or_default()\n                            .entry(owner)"""
assert old in source, "local style write pattern not found"
source = source.replace(old, new, 1)' \
  "editing_an_inherited_property_writes_it_on_the_element_not_on_the_ancestor"

# 16. A geometry write stops merging into the element's existing inline style.
run_mutation "an inline style is replaced instead of merged" \
  src/project_save.rs \
  'old = """fn merge_inline_declarations(open_tag: &str, edits: &[(String, String)]) -> String {
    let mut declarations = parse_inline_declarations(open_tag);"""
new = """fn merge_inline_declarations(open_tag: &str, edits: &[(String, String)]) -> String {
    let mut declarations: Vec<(String, String)> = Vec::new();"""
assert old in source, "inline merge pattern not found"
source = source.replace(old, new, 1)' \
  "a_hand_written_inline_declaration_survives_an_unrelated_edit"

# 17. A moved flow element is lifted out of the flow again.
run_mutation "a move always becomes position: absolute" \
  src/project_save.rs \
  'old = """        Placement::Flow { dx, dy } => {
            let Some(target) = style_target(document, node, \"transform\") else {"""
new = """        Placement::Flow { dx, dy } => {
            writes
                .inline
                .push((\"position\".to_owned(), \"absolute\".to_owned()));
            writes
                .inline
                .push((\"left\".to_owned(), format!(\"{}px\", css_length(dx))));
            writes
                .inline
                .push((\"top\".to_owned(), format!(\"{}px\", css_length(dy))));
            if true {
                return Ok(writes);
            }
            let Some(target) = style_target(document, node, \"transform\") else {"""
assert old in source, "flow placement pattern not found"
source = source.replace(old, new, 1)' \
  "a_geometry_edit_survives_the_round_trip"

# 18. A move replaces the authored transform instead of adding to it.
run_mutation "a second move replaces the authored offset" \
  src/project_save.rs \
  'old = """            let moved = (base.0 + dx, base.1 + dy);"""
new = """            let moved = (dx, dy);
            let _ = base;"""
assert old in source, "transform composition pattern not found"
source = source.replace(old, new, 1)' \
  "a_second_move_adds_to_the_offset_the_author_wrote"

# 19. An unreadable transform is overwritten instead of reported.
# 19. A rotation is not an offset, so it must be reported rather than rewritten.
run_mutation "a rotation is overwritten by a translate" \
  src/style.rs \
  'old = """pub fn parse_translate(value: &str) -> Option<(f32, f32)> {"""
new = """pub fn parse_translate(value: &str) -> Option<(f32, f32)> {
    if value.contains("rotate") {
        return Some((0.0, 0.0));
    }"""
assert old in source, "parse_translate signature not found"
source = source.replace(old, new, 1)' \
  "a_transform_the_editor_cannot_read_is_reported_rather_than_overwritten"

# 20. A child writes its own offset when only its parent moved.
run_mutation "a moved child rewrites its own position" \
  src/canvas.rs \
  'old = """                        let now = sub_point(geometry.position, parent_origin);
                        crate::project_save::Placement::Flow {
                            dx: now.x - before.parent_relative.x,
                            dy: now.y - before.parent_relative.y,
                        }"""
new = """                        crate::project_save::Placement::Flow {
                            dx: geometry.position.x - before.position.x,
                            dy: geometry.position.y - before.position.y,
                        }"""
assert old in source, "parent-relative flow delta pattern not found"
source = source.replace(old, new, 1)' \
  "moving_a_frame_and_its_children_survives_the_round_trip"

# 21. A transform moves a box but is then treated as layout.
run_mutation "a transform advances the flow cursor" \
  src/visual.rs \
  'old = """        top - offset.1 + height + margin[2]"""
new = """        top + height + margin[2]"""
assert old in source, "flow cursor pattern not found"
source = source.replace(old, new, 1)' \
  "a_transform_moves_a_box_without_moving_the_flow_around_it"

# 22. Every Inspector drag is recorded, one entry per movement.
run_mutation "a geometry scrub records on every move" \
  src/canvas.rs \
  'old = """    pub fn scrub_geometry(&mut self, scrub: &GeometryScrub, geometry: Geometry) -> bool {
        self.session.runtime.set_geometry(scrub.id, geometry)
    }"""
new = """    pub fn scrub_geometry(&mut self, scrub: &GeometryScrub, geometry: Geometry) -> bool {
        if let Some(before) = self.session.runtime.geometry(scrub.id) {
            self.commit_geometry_change(scrub.id, before, geometry);
        }
        self.session.runtime.set_geometry(scrub.id, geometry)
    }"""
assert old in source, "scrub geometry pattern not found"
source = source.replace(old, new, 1)' \
  "an_inspector_field_drag_records_one_operation_not_one_per_movement"

# 23. A rename writes the runtime name without touching the document.
run_mutation "a rename only changes the runtime copy" \
  src/canvas.rs \
  'old = """        let changed = crate::operations::rename_node_in(&self.session.document, node.clone(), name)
            .and_then(|operation| self.session.execute(operation));
        let changed = match changed {
            Ok(changed) => changed,
            Err(error) => {
                self.refuse("rename", &error);
                return false;
            }
        };"""
new = """        let changed = self.session.runtime.set_object_name(id, name);"""
assert old in source, "rename operation pattern not found"
source = source.replace(old, new, 1)' \
  "renaming_an_object_is_one_operation_that_survives_save"

# 24. Undo no longer refreshes the mirrored name, so the two disagree.
run_mutation "undo leaves the runtime name stale" \
  src/canvas.rs \
  'old = """            self.retain_existing_selection();
            self.refresh_object_names();
        }
        changed
    }

    /// Redo the last undone operation."""
new = """            self.retain_existing_selection();
        }
        changed
    }

    /// Redo the last undone operation."""
assert old in source, "undo name refresh pattern not found"
source = source.replace(old, new, 1)' \
  "renaming_an_object_is_one_operation_that_survives_save"

# 25. A text change is written to the runtime without entering history.
run_mutation "a text edit skips the history stack" \
  src/canvas.rs \
  'old = """        self.commit(DocumentCommand::text(vec![change]));
        true
    }"""
new = """        let _ = DocumentCommand::text(vec![change]);
        true
    }"""
assert old in source, "text commit pattern not found"
source = source.replace(old, new, 1)' \
  "one_session_edits_text_style_geometry_and_a_name_and_all_of_it_survives"

if [ "$failures" -eq 0 ]; then
  echo
  echo "every mutation was caught"
else
  echo
  echo "$failures mutation(s) were not caught"
  exit 1
fi
