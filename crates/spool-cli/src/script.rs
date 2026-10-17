//! `script`: a JSON array of command steps run as **one grouped transaction**.
//!
//! This is the shape an agent wants — "make these ten changes, or none of them". Every step goes
//! through the engine's single mutation path ([`Session::execute`]); the group is the engine's own
//! ([`Session::grouped`]), so a failing step rolls the document back to where the script started
//! and becomes one undo step.
//!
//! A step is `{"id": "node.create", "params": {...}}`. `"command"` is accepted as a synonym of
//! `"id"` (the engine's own script spelling). Inside `params`, `{"$step": n}` is replaced by a
//! field of the result of the earlier step `n` (`"id"` by default), so a step can act on a layer an
//! earlier step created.

use figcraft_engine::{EngineError, Session};
use serde::Deserialize;
use serde_json::{json, Value};

/// How deep `$step` substitution may nest before it is refused.
const MAX_REF_DEPTH: usize = 64;

/// One step of a script.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// The engine command id, e.g. `node.create`.
    #[serde(alias = "command")]
    pub id: String,
    #[serde(default)]
    pub params: Option<Value>,
}

impl Step {
    /// The step's params, with `$step` references to `results` resolved.
    pub fn resolved_params(&self, results: &[Value], step: usize) -> Result<Value, String> {
        match &self.params {
            None => Ok(json!({})),
            Some(Value::Null) => Ok(json!({})),
            Some(v @ Value::Object(_)) => resolve(v, results, step, 0),
            Some(_) => Err(format!(
                "step {step} ({}): `params` must be an object",
                self.id
            )),
        }
    }
}

/// What running a script produced: one record per step attempted, and the group's outcome.
pub struct Run {
    /// One `{step, command, ok, result|error}` record per step, in order.
    pub steps: Vec<Value>,
    /// The failing step's error (the document was rolled back).
    pub error: Option<EngineError>,
}

/// Parse script text: a JSON array of steps, or `{"commands": [...]}` / `{"steps": [...]}`.
pub fn parse_script(text: &str) -> Result<Vec<Step>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let array = match &value {
        Value::Array(a) => a.clone(),
        Value::Object(o) => {
            let key = ["steps", "commands"]
                .into_iter()
                .find(|k| o.contains_key(*k));
            match key.and_then(|k| o.get(k)) {
                Some(Value::Array(a)) => a.clone(),
                _ => return Err("a script is a JSON array of {\"id\", \"params\"} steps, or {\"commands\": [...]}".into()),
            }
        }
        _ => return Err("a script is a JSON array of {\"id\", \"params\"} steps".into()),
    };
    let steps: Vec<Step> =
        serde_json::from_value::<Vec<Step>>(Value::Array(array)).map_err(|e| e.to_string())?;
    // `params` is an object (or absent) in every step: catch a typo'd script before it touches a
    // document, rather than half way through a transaction.
    for (i, step) in steps.iter().enumerate() {
        if let Some(params) = &step.params {
            if !matches!(params, Value::Null | Value::Object(_)) {
                return Err(format!(
                    "step {i} ({}): `params` must be an object",
                    step.id
                ));
            }
        }
    }
    Ok(steps)
}

/// Run `steps` in `session` as one group. Every step attempted before a failure is recorded, with
/// its result; the document is left as it was before the script.
pub fn execute_all(session: &mut Session, steps: &[Step]) -> Run {
    let mut records: Vec<Value> = Vec::with_capacity(steps.len());
    let mut results: Vec<Value> = Vec::with_capacity(steps.len());
    let outcome = session.grouped("spool-cli script", |s| {
        for (step, s_step) in steps.iter().enumerate() {
            let params = match s_step.resolved_params(&results, step) {
                Ok(p) => p,
                Err(msg) => return Err(EngineError::BadParams { cmd: "command.batch".to_string(), msg }),
            };
            match s.execute(&s_step.id, &params) {
                Ok(value) => {
                    results.push(value.clone());
                    records.push(json!({"step": step, "command": s_step.id, "ok": true, "result": value}));
                }
                Err(e) => {
                    records.push(json!({"step": step, "command": s_step.id, "ok": false, "error": e.to_string()}));
                    return Err(e);
                }
            }
        }
        Ok(())
    });
    Run {
        steps: records,
        error: outcome.err(),
    }
}

/// Replace every `{"$step": n}` in `v` with field `field` (default `id`) of `results[n]`.
fn resolve(v: &Value, results: &[Value], step: usize, depth: usize) -> Result<Value, String> {
    if depth > MAX_REF_DEPTH {
        return Err(format!(
            "step {step}: `params` nest deeper than {MAX_REF_DEPTH} levels"
        ));
    }
    Ok(match v {
        Value::Object(o) if o.contains_key("$step") => {
            if let Some(k) = o.keys().find(|k| *k != "$step" && *k != "field") {
                return Err(format!(
                    "step {step}: a `$step` reference takes only `field`, not `{k}`"
                ));
            }
            let n = o
                .get("$step")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("step {step}: `$step` must be a step index (0, 1, …)"))?;
            let field = match o.get("field") {
                None => "id",
                Some(Value::String(f)) => f.as_str(),
                Some(_) => return Err(format!("step {step}: `field` must be a string")),
            };
            let found = usize::try_from(n).ok().and_then(|n| results.get(n));
            let Some(result) = found else {
                let range = if step == 0 {
                    "there is none before step 0".to_string()
                } else {
                    format!("0 to {}", step - 1)
                };
                return Err(format!(
                    "step {step}: `$step` {n} must be an earlier step ({range})"
                ));
            };
            result
                .get(field)
                .cloned()
                .ok_or_else(|| format!("step {step}: step {n}'s result has no `{field}`"))?
        }
        Value::Object(o) => {
            let mut out = serde_json::Map::with_capacity(o.len());
            for (k, v) in o {
                out.insert(k.clone(), resolve(v, results, step, depth + 1)?);
            }
            Value::Object(out)
        }
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for v in a {
                out.push(resolve(v, results, step, depth + 1)?);
            }
            Value::Array(out)
        }
        other => other.clone(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn steps(text: &str) -> Vec<Step> {
        parse_script(text).unwrap()
    }

    #[test]
    fn parses_the_documented_shape_and_the_command_synonym() {
        let s = steps(
            r#"[{"id": "a"}, {"command": "b", "params": {"x": 1}}, {"id": "c", "params": null}]"#,
        );
        assert_eq!(s.len(), 3);
        assert_eq!(s[1].id, "b");
        assert_eq!(s[0].resolved_params(&[], 0).unwrap(), json!({}));
        assert_eq!(s[2].resolved_params(&[], 2).unwrap(), json!({}));
    }

    #[test]
    fn parses_the_object_wrappers() {
        assert_eq!(steps(r#"{"commands": [{"id": "a"}]}"#).len(), 1);
        assert_eq!(steps(r#"{"steps": [{"id": "a"}]}"#).len(), 1);
    }

    #[test]
    fn rejects_what_is_not_a_step() {
        for bad in [
            r#"not json"#,
            r#"42"#,
            r#"[{"params": {}}]"#,
            r#"[{"id": "a", "parms": {}}]"#,
            r#"[{"id": "a", "params": 3}]"#,
            r#"{"commands": 3}"#,
            r#"[{"id": 7}]"#,
        ] {
            assert!(parse_script(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn step_refs_pick_a_field_of_an_earlier_result() {
        let s = steps(
            r#"[{"id": "a", "params": {"parent": {"$step": 0}}}, {"id": "b", "params": {"p": {"$step": 0, "field": "name"}, "l": [{"$step": 1}]}}]"#,
        );
        let results = vec![json!({"id": "1:2", "name": "Hero"}), json!({"id": "1:3"})];
        assert_eq!(
            s[0].resolved_params(&results, 1).unwrap(),
            json!({"parent": "1:2"})
        );
        assert_eq!(
            s[1].resolved_params(&results, 1).unwrap(),
            json!({"p": "Hero", "l": ["1:3"]})
        );
    }

    #[test]
    fn step_refs_that_point_nowhere_are_errors() {
        let s = steps(r#"[{"id": "a", "params": {"p": {"$step": 3}}}]"#);
        assert!(s[0].resolved_params(&[json!({"id": "x"})], 0).is_err());
        // A forward reference.
        let s = steps(r#"[{"id": "a", "params": {"p": {"$step": 1}}}]"#);
        assert!(s[0].resolved_params(&[], 0).is_err());
        // A field the result doesn't have.
        let s = steps(r#"[{"id": "a", "params": {"p": {"$step": 0, "field": "nope"}}}]"#);
        assert!(s[0].resolved_params(&[json!({"id": "x"})], 1).is_err());
        // A malformed reference.
        let s = steps(r#"[{"id": "a", "params": {"p": {"$step": 0, "x": 1}}}]"#);
        assert!(s[0].resolved_params(&[json!({"id": "x"})], 1).is_err());
        let s = steps(r#"[{"id": "a", "params": {"p": {"$step": "x"}}}]"#);
        assert!(s[0].resolved_params(&[], 1).is_err());
        // Nesting past the cap.
        let mut deep = String::from(r#"{"$step":0}"#);
        for _ in 0..(MAX_REF_DEPTH + 2) {
            deep = format!("{{\"a\":{deep}}}");
        }
        assert!(resolve(
            &serde_json::from_str(&deep).unwrap(),
            &[json!({"id": "x"})],
            1,
            0
        )
        .is_err());
    }

    // The next two drive the real engine: in process, no window, no files.
    #[test]
    fn a_script_runs_its_steps_and_a_step_can_act_on_an_earlier_result() {
        let mut session = Session::new();
        let steps = steps(
            r#"[{"id": "node.create", "params": {"type": "frame", "name": "Hero", "width": 80, "height": 60}},
                {"id": "shape.rectangle", "params": {"x": 4, "y": 4, "width": 16, "height": 16, "parent": {"$step": 0}}},
                {"id": "node.set", "params": {"ids": [{"$step": 0}], "props": {"name": "Renamed"}}}]"#,
        );
        let run = execute_all(&mut session, &steps);
        assert!(run.error.is_none(), "{:?}", run.error);
        assert_eq!(run.steps.len(), 3);
        assert!(run.steps.iter().all(|r| r["ok"] == json!(true)));
        let doc = session.doc().unwrap();
        assert!(
            doc.find_by_name("Hero", None).is_empty(),
            "the third step renamed the frame"
        );
        assert_eq!(doc.find_by_name("Renamed", None).len(), 1);
        // One group: one undo step for the whole script.
        assert_eq!(session.undo_depth().0, 1);
    }

    #[test]
    fn a_failing_step_rolls_the_whole_script_back() {
        let mut session = Session::new();
        let steps = steps(
            r#"[{"id": "node.create", "params": {"type": "frame", "name": "Doomed", "width": 80, "height": 60}},
                {"id": "node.set", "params": {"ids": [999], "props": {"name": "x"}}}]"#,
        );
        let run = execute_all(&mut session, &steps);
        assert!(run.error.is_some());
        assert_eq!(run.steps.len(), 2, "both attempted steps are reported");
        assert_eq!(run.steps[0]["ok"], json!(true));
        assert_eq!(run.steps[1]["ok"], json!(false));
        assert!(run.steps[1]["error"].as_str().unwrap().contains("999"));
        // Nothing survives, and no undo step was left behind.
        assert!(session
            .doc()
            .unwrap()
            .find_by_name("Doomed", None)
            .is_empty());
        assert_eq!(session.undo_depth(), (0, 0));
    }
}
