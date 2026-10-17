//! A general-purpose projection: every event payload is a JSON Merge Patch.

use serde_json::{Map, Value};

use super::{Projection, ProjectionError};
use crate::domain::StoredEvent;
use crate::snapshot::ProjectionKey;

/// Folds a stream by applying each event's payload as an
/// [RFC 7386 JSON Merge Patch](https://www.rfc-editor.org/rfc/rfc7386) to
/// the state, starting from `{}`.
///
/// * An object payload merges member by member: `null` removes a member, and
///   nested objects merge recursively.
/// * Any other payload (array, string, number, `true`/`false`, `null`)
///   replaces the whole state.
///
/// The event type and schema version are ignored, so this projection
/// accepts every event of every stream. That makes it the default for
/// inspecting arbitrary streams, where no domain projection exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct MergePatch;

/// Applies `patch` to `target` in place, per RFC 7386.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    if let Value::Object(target) = target {
        for (key, value) in patch {
            if value.is_null() {
                target.remove(key);
            } else {
                merge_patch(target.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    }
}

impl Projection for MergePatch {
    type State = Value;

    fn key(&self) -> ProjectionKey {
        ProjectionKey::new("merge-patch", 1).unwrap_or_else(|_| unreachable!())
    }

    fn initial_state(&self) -> Value {
        Value::Object(Map::new())
    }

    fn apply(&self, mut state: Value, event: &StoredEvent) -> Result<Value, ProjectionError> {
        merge_patch(&mut state, &event.payload);
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The examples from RFC 7386, Appendix A.
    #[test]
    fn rfc_7386_appendix_a() {
        let cases = [
            (json!({"a":"b"}), json!({"a":"c"}), json!({"a":"c"})),
            (json!({"a":"b"}), json!({"b":"c"}), json!({"a":"b","b":"c"})),
            (json!({"a":"b"}), json!({"a":null}), json!({})),
            (
                json!({"a":"b","b":"c"}),
                json!({"a":null}),
                json!({"b":"c"}),
            ),
            (json!({"a":["b"]}), json!({"a":"c"}), json!({"a":"c"})),
            (json!({"a":"c"}), json!({"a":["b"]}), json!({"a":["b"]})),
            (
                json!({"a":{"b":"c"}}),
                json!({"a":{"b":"d","c":null}}),
                json!({"a":{"b":"d"}}),
            ),
            (json!({"a":[{"b":"c"}]}), json!({"a":[1]}), json!({"a":[1]})),
            (json!(["a", "b"]), json!(["c", "d"]), json!(["c", "d"])),
            (json!({"a":"b"}), json!(["c"]), json!(["c"])),
            (json!({"a":"foo"}), json!(null), json!(null)),
            (json!({"a":"foo"}), json!("bar"), json!("bar")),
            (json!({"e":null}), json!({"a":1}), json!({"e":null,"a":1})),
            (json!([1, 2]), json!({"a":"b","c":null}), json!({"a":"b"})),
            (
                json!({}),
                json!({"a":{"bb":{"ccc":null}}}),
                json!({"a":{"bb":{}}}),
            ),
        ];
        for (mut target, patch, expected) in cases {
            merge_patch(&mut target, &patch);
            assert_eq!(target, expected, "patch {patch}");
        }
    }

    #[test]
    fn key_and_initial_state() {
        assert_eq!(MergePatch.key().to_string(), "merge-patch@v1");
        assert_eq!(MergePatch.initial_state(), json!({}));
    }
}
