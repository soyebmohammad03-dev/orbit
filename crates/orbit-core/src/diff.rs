//! Structural diff of JSON values.
//!
//! [`diff`] compares two JSON values and returns the list of [`Change`]s that
//! turn the first into the second.
//!
//! # Semantics
//!
//! * **Paths** are [RFC 6901 JSON Pointers]: `""` is the whole value,
//!   `/items/A-1/on_hand` an object member, `/discontinued/0` an array
//!   element. `~` and `/` inside keys are escaped as `~0` and `~1`.
//! * **Objects** are compared member by member. A key present only in the
//!   new value is [`ChangeKind::Added`]; a key present only in the old value
//!   is [`ChangeKind::Removed`]; a key in both is compared recursively.
//! * **Arrays** are compared *by index*. Indices present in both are compared
//!   recursively. Extra trailing elements in the new array are `Added`, and
//!   extra trailing elements in the old array are `Removed`. No alignment is
//!   attempted, so inserting at the front of an array shows up as a
//!   modification of every following index plus one addition at the end.
//!   This is predictable and linear-time, which is the point.
//! * **Scalars, and values of different JSON types**, that differ are a
//!   single [`ChangeKind::Modified`] at their path. Nothing below it is reported.
//! * Numbers compare as `serde_json` numbers: `1` and `1.0` are different values.
//! * **Ordering is deterministic**: a depth-first walk, visiting object keys
//!   in bytewise order (the canonical-JSON order) and array indices in
//!   ascending order. Identical inputs always produce identical output.
//!   Equal values produce an empty list.
//!
//! [RFC 6901 JSON Pointers]: https://www.rfc-editor.org/rfc/rfc6901

use serde::Serialize;
use serde_json::{Map, Value};

/// One difference between two JSON values.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Change {
    /// JSON Pointer to the changed location.
    pub path: String,
    /// What changed.
    #[serde(flatten)]
    pub kind: ChangeKind,
}

/// The nature of a [`Change`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ChangeKind {
    /// The path exists only in the new value.
    Added {
        /// The new value.
        value: Value,
    },
    /// The path exists only in the old value.
    Removed {
        /// The old value.
        value: Value,
    },
    /// The path exists in both with different values.
    Modified {
        /// The old value.
        from: Value,
        /// The new value.
        to: Value,
    },
}

/// Returns the changes that turn `old` into `new`, in deterministic order.
#[must_use]
pub fn diff(old: &Value, new: &Value) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut path = String::new();
    walk(&mut path, old, new, &mut changes);
    changes
}

fn walk(path: &mut String, old: &Value, new: &Value, out: &mut Vec<Change>) {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => walk_objects(path, a, b, out),
        (Value::Array(a), Value::Array(b)) => {
            for index in 0..a.len().max(b.len()) {
                let len = path.len();
                path.push('/');
                path.push_str(&index.to_string());
                match (a.get(index), b.get(index)) {
                    (Some(x), Some(y)) => walk(path, x, y, out),
                    (Some(x), None) => push(out, path, ChangeKind::Removed { value: x.clone() }),
                    (None, Some(y)) => push(out, path, ChangeKind::Added { value: y.clone() }),
                    (None, None) => {}
                }
                path.truncate(len);
            }
        }
        _ if old == new => {}
        _ => push(
            out,
            path,
            ChangeKind::Modified {
                from: old.clone(),
                to: new.clone(),
            },
        ),
    }
}

fn walk_objects(
    path: &mut String,
    a: &Map<String, Value>,
    b: &Map<String, Value>,
    out: &mut Vec<Change>,
) {
    let mut keys: Vec<&String> = a
        .keys()
        .chain(b.keys().filter(|k| !a.contains_key(*k)))
        .collect();
    keys.sort_unstable_by(|x, y| x.as_bytes().cmp(y.as_bytes()));
    for key in keys {
        let len = path.len();
        path.push('/');
        push_escaped(path, key);
        match (a.get(key), b.get(key)) {
            (Some(x), Some(y)) => walk(path, x, y, out),
            (Some(x), None) => push(out, path, ChangeKind::Removed { value: x.clone() }),
            (None, Some(y)) => push(out, path, ChangeKind::Added { value: y.clone() }),
            (None, None) => {}
        }
        path.truncate(len);
    }
}

fn push(out: &mut Vec<Change>, path: &str, kind: ChangeKind) {
    out.push(Change {
        path: path.to_owned(),
        kind,
    });
}

fn push_escaped(path: &mut String, key: &str) {
    for c in key.chars() {
        match c {
            '~' => path.push_str("~0"),
            '/' => path.push_str("~1"),
            c => path.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ops(changes: &[Change]) -> Vec<(String, &'static str)> {
        changes
            .iter()
            .map(|c| {
                let op = match c.kind {
                    ChangeKind::Added { .. } => "added",
                    ChangeKind::Removed { .. } => "removed",
                    ChangeKind::Modified { .. } => "modified",
                };
                (c.path.clone(), op)
            })
            .collect()
    }

    #[test]
    fn identical_values_have_no_changes() {
        let v = json!({"a": [1, {"b": null}], "c": "x"});
        assert!(diff(&v, &v).is_empty());
        assert!(diff(&json!(null), &json!(null)).is_empty());
    }

    #[test]
    fn object_members() {
        let changes = diff(
            &json!({"keep": 1, "gone": true, "edit": "a"}),
            &json!({"keep": 1, "edit": "b", "new": [1]}),
        );
        assert_eq!(
            changes,
            vec![
                Change {
                    path: "/edit".into(),
                    kind: ChangeKind::Modified {
                        from: json!("a"),
                        to: json!("b")
                    }
                },
                Change {
                    path: "/gone".into(),
                    kind: ChangeKind::Removed { value: json!(true) }
                },
                Change {
                    path: "/new".into(),
                    kind: ChangeKind::Added { value: json!([1]) }
                },
            ]
        );
    }

    #[test]
    fn nested_objects_report_leaf_paths() {
        let changes = diff(
            &json!({"items": {"A-1": {"on_hand": 5, "reserved": 0}}}),
            &json!({"items": {"A-1": {"on_hand": 3, "reserved": 2}, "B-2": {"on_hand": 1}}}),
        );
        assert_eq!(
            ops(&changes),
            vec![
                ("/items/A-1/on_hand".into(), "modified"),
                ("/items/A-1/reserved".into(), "modified"),
                ("/items/B-2".into(), "added"),
            ]
        );
    }

    #[test]
    fn arrays_compare_by_index() {
        assert_eq!(
            ops(&diff(&json!([1, 2]), &json!([1, 3, 4, 5]))),
            vec![
                ("/1".into(), "modified"),
                ("/2".into(), "added"),
                ("/3".into(), "added")
            ]
        );
        assert_eq!(
            ops(&diff(&json!(["a", "b", "c"]), &json!(["a"]))),
            vec![("/1".into(), "removed"), ("/2".into(), "removed")]
        );
        // Front insertion is reported positionally, not aligned.
        assert_eq!(
            ops(&diff(&json!(["x"]), &json!(["w", "x"]))),
            vec![("/0".into(), "modified"), ("/1".into(), "added")]
        );
        assert_eq!(
            ops(&diff(&json!([{"n": 1}]), &json!([{"n": 2}]))),
            vec![("/0/n".into(), "modified")]
        );
    }

    #[test]
    fn type_changes_and_root_changes() {
        assert_eq!(
            ops(&diff(&json!({"a": [1]}), &json!({"a": {"0": 1}}))),
            vec![("/a".into(), "modified")]
        );
        assert_eq!(
            ops(&diff(&json!(1), &json!("1"))),
            vec![("".into(), "modified")]
        );
        assert_eq!(
            ops(&diff(&json!(1), &json!(1.0))),
            vec![("".into(), "modified")]
        );
    }

    #[test]
    fn keys_are_escaped_and_sorted_bytewise() {
        let changes = diff(&json!({}), &json!({"b/c": 1, "a~": 2, "Z": 3}));
        assert_eq!(
            ops(&changes),
            vec![
                ("/Z".into(), "added"),
                ("/a~0".into(), "added"),
                ("/b~1c".into(), "added")
            ]
        );
    }

    #[test]
    fn serializes_for_api_consumers() {
        let changes = diff(&json!({"a": 1}), &json!({"a": 2}));
        assert_eq!(
            serde_json::to_value(&changes).unwrap(),
            json!([{"path": "/a", "op": "modified", "from": 1, "to": 2}])
        );
    }
}
