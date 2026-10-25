//! RFC 6902 patches between state documents: the diff a state change is
//! published as, and applying one operation to the shrunk cache.
//!
//! The diff is json-patch's (objects: changed and added keys in order, then
//! removed keys; arrays: index by index, extra elements removed or added),
//! here because json-patch needs std.

use serde_json::{Map, Value};

use crate::prelude::*;

/// `(op, path, value)` for each operation turning `old` into `new`.
pub fn diff_ops(old: &Value, new: &Value) -> Vec<(String, String, Value)> {
    let mut ops = vec![];
    diff(old, new, &mut String::new(), &mut ops);
    ops
}

fn diff(left: &Value, right: &Value, pointer: &mut String, ops: &mut Vec<(String, String, Value)>) {
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => diff_object(left, right, pointer, ops),
        (Value::Array(left), Value::Array(right)) => diff_array(left, right, pointer, ops),
        _ if left == right => {}
        _ => ops.push(("replace".into(), pointer.clone(), right.clone())),
    }
}

fn diff_array(
    left: &[Value],
    right: &[Value],
    pointer: &mut String,
    ops: &mut Vec<(String, String, Value)>,
) {
    let mut shift = 0usize;
    for idx in 0..left.len().max(right.len()) {
        let len = pointer.len();
        push_token(pointer, &(idx - shift).to_string());
        match (left.get(idx), right.get(idx)) {
            (Some(left), Some(right)) => diff(left, right, pointer, ops),
            (Some(_), None) => {
                shift += 1;
                ops.push(("remove".into(), pointer.clone(), Value::Null));
            }
            (None, Some(right)) => ops.push(("add".into(), pointer.clone(), right.clone())),
            (None, None) => {}
        }
        pointer.truncate(len);
    }
}

fn diff_object(
    left: &Map<String, Value>,
    right: &Map<String, Value>,
    pointer: &mut String,
    ops: &mut Vec<(String, String, Value)>,
) {
    for (key, right_value) in right {
        let len = pointer.len();
        push_token(pointer, key);
        match left.get(key) {
            Some(left_value) => diff(left_value, right_value, pointer, ops),
            None => ops.push(("add".into(), pointer.clone(), right_value.clone())),
        }
        pointer.truncate(len);
    }
    for key in left.keys() {
        if !right.contains_key(key) {
            let len = pointer.len();
            push_token(pointer, key);
            ops.push(("remove".into(), pointer.clone(), Value::Null));
            pointer.truncate(len);
        }
    }
}

/// Append `/token`, escaped (`~` as `~0`, `/` as `~1`).
fn push_token(pointer: &mut String, token: &str) {
    pointer.push('/');
    for c in token.chars() {
        match c {
            '~' => pointer.push_str("~0"),
            '/' => pointer.push_str("~1"),
            c => pointer.push(c),
        }
    }
}

/// Why an operation does not apply.
#[derive(Debug, PartialEq)]
pub struct PatchError(String);

impl core::fmt::Display for PatchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Apply one `add`, `replace` or `remove` at `path` to `doc`.
pub fn apply(doc: &mut Value, op: &str, path: &str, value: &Value) -> Result<(), PatchError> {
    if path.is_empty() {
        return match op {
            "add" | "replace" => {
                *doc = value.clone();
                Ok(())
            }
            _ => Err(PatchError("cannot remove the whole document".into())),
        };
    }
    let Some(rest) = path.strip_prefix('/') else {
        return Err(PatchError(format!("invalid pointer {path}")));
    };
    let tokens: Vec<String> = rest.split('/').map(unescape).collect();
    let (last, parents) = tokens.split_last().expect("a non-empty path has a token");
    let mut target = doc;
    for token in parents {
        target = match target {
            Value::Object(map) => map.get_mut(token.as_str()),
            Value::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get_mut(i)),
            _ => None,
        }
        .ok_or_else(|| PatchError(format!("{path}: no {token}")))?;
    }
    match (target, op) {
        (Value::Object(map), "add") => {
            map.insert(last.clone(), value.clone());
        }
        (Value::Object(map), "replace") => match map.get_mut(last.as_str()) {
            Some(slot) => *slot = value.clone(),
            None => return Err(PatchError(format!("{path}: nothing to replace"))),
        },
        (Value::Object(map), "remove") => {
            if map.remove(last.as_str()).is_none() {
                return Err(PatchError(format!("{path}: nothing to remove")));
            }
        }
        (Value::Array(items), "add") => {
            let index = if last == "-" {
                items.len()
            } else {
                index(last, items.len() + 1, path)?
            };
            items.insert(index, value.clone());
        }
        (Value::Array(items), "replace") => {
            let index = index(last, items.len(), path)?;
            items[index] = value.clone();
        }
        (Value::Array(items), "remove") => {
            let index = index(last, items.len(), path)?;
            items.remove(index);
        }
        (_, op @ ("add" | "replace" | "remove")) => {
            return Err(PatchError(format!("{path}: cannot {op} in a scalar")))
        }
        (_, op) => return Err(PatchError(format!("unsupported operation {op}"))),
    }
    Ok(())
}

fn index(token: &str, bound: usize, path: &str) -> Result<usize, PatchError> {
    token
        .parse::<usize>()
        .ok()
        .filter(|i| *i < bound)
        .ok_or_else(|| PatchError(format!("{path}: index out of range")))
}

fn unescape(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The pairs the state hub sees: nested objects, arrays that grow and
    /// shrink, scalars that change type, keys that need escaping.
    fn cases() -> Vec<(Value, Value)> {
        vec![
            (json!({"a": 1}), json!({"a": 2})),
            (json!({"a": 1, "b": 2}), json!({"b": 3, "c": 4})),
            (json!({"x": {"y": [1, 2, 3]}}), json!({"x": {"y": [1, 3]}})),
            (json!([1, 2, 3, 4]), json!([1])),
            (json!([1]), json!([1, {"k": "v"}, null])),
            (json!({"a/b": 1, "m~n": 2}), json!({"a/b": 3})),
            (json!({"a": [1, 2]}), json!({"a": "now a string"})),
            (json!(null), json!({"fresh": true})),
            (
                json!({"deep": {"er": {"est": [ {"v": 1}, {"v": 2} ]}}}),
                json!({"deep": {"er": {"est": [ {"v": 2} ]}}}),
            ),
        ]
    }

    #[test]
    fn the_diff_is_json_patchs() {
        for (old, new) in cases() {
            let theirs = serde_json::to_value(json_patch::diff(&old, &new)).unwrap();
            let ours: Vec<Value> = diff_ops(&old, &new)
                .into_iter()
                .map(|(op, path, value)| {
                    if op == "remove" {
                        json!({"op": op, "path": path})
                    } else {
                        json!({"op": op, "path": path, "value": value})
                    }
                })
                .collect();
            assert_eq!(Value::Array(ours), theirs, "{old} -> {new}");
        }
    }

    #[test]
    fn applying_the_diff_one_op_at_a_time_gets_there() {
        for (old, new) in cases() {
            let mut doc = old.clone();
            for (op, path, value) in diff_ops(&old, &new) {
                apply(&mut doc, &op, &path, &value).unwrap();
            }
            assert_eq!(doc, new, "{old} -> {new}");
        }
    }

    #[test]
    fn bad_operations_are_refused() {
        let mut doc = json!({"a": [1]});
        assert!(apply(&mut doc, "replace", "/b", &json!(1)).is_err());
        assert!(apply(&mut doc, "remove", "/a/5", &Value::Null).is_err());
        assert!(apply(&mut doc, "add", "/a/0/x", &json!(1)).is_err());
        assert!(apply(&mut doc, "move", "/a", &Value::Null).is_err());
        apply(&mut doc, "add", "/a/-", &json!(2)).unwrap();
        assert_eq!(doc, json!({"a": [1, 2]}));
    }
}
