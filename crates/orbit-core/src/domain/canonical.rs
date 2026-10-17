//! Canonical JSON encoding.
//!
//! Payloads and metadata are stored in a single canonical textual form so
//! that the bytes covered by the integrity digest are a deterministic function
//! of the JSON *value*, independent of how the client formatted it.
//!
//! The canonical form of a value is:
//!
//! * no insignificant whitespace;
//! * object members sorted by key, comparing keys as UTF-8 byte strings
//!   (equivalently, Unicode code points); duplicate keys cannot occur because
//!   parsing keeps the last occurrence;
//! * strings escaped exactly as `serde_json` escapes them (`"`, `\`, and
//!   control characters; all other characters are emitted verbatim as UTF-8);
//! * numbers rendered by `serde_json`: integers in plain decimal, floats in
//!   the shortest representation that round-trips to the same `f64`.
//!
//! Key ordering is implemented here rather than relying on `serde_json`'s
//! map type, so the canonical form does not change if another crate in the
//! dependency graph enables `serde_json/preserve_order`.

use serde_json::{Map, Value};

/// Returns the canonical encoding of `value`.
#[must_use]
pub fn to_canonical_string(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

/// Returns the canonical encoding of a JSON object.
#[must_use]
pub fn object_to_canonical_string(map: &Map<String, Value>) -> String {
    let mut out = String::new();
    write_object(&mut out, map);
    out
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(string) => write_string(out, string),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => write_object(out, map),
    }
}

fn write_object(out: &mut String, map: &Map<String, Value>) {
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    out.push('{');
    for (index, (key, item)) in entries.into_iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_string(out, key);
        out.push(':');
        write_value(out, item);
    }
    out.push('}');
}

fn write_string(out: &mut String, value: &str) {
    // Serializing a `str` to JSON cannot fail; the fallback keeps this path
    // panic-free regardless.
    match serde_json::to_string(value) {
        Ok(escaped) => out.push_str(&escaped),
        Err(_) => out.push_str("\"\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_keys_and_strips_whitespace() {
        let value: Value =
            serde_json::from_str(r#" { "b" : 1, "a" : [ true , null ], "c": {"z":1,"y":2} } "#)
                .unwrap();
        assert_eq!(
            to_canonical_string(&value),
            r#"{"a":[true,null],"b":1,"c":{"y":2,"z":1}}"#
        );
    }

    #[test]
    fn key_order_is_bytewise() {
        let value = json!({"é": 1, "z": 2, "Z": 3, "aa": 4, "a": 5});
        assert_eq!(
            to_canonical_string(&value),
            r#"{"Z":3,"a":5,"aa":4,"z":2,"é":1}"#
        );
    }

    #[test]
    fn escapes_strings() {
        let value = json!("quote\" backslash\\ newline\n tab\t nul\u{0} snow☃");
        assert_eq!(
            to_canonical_string(&value),
            r#""quote\" backslash\\ newline\n tab\t nul\u0000 snow☃""#
        );
    }

    #[test]
    fn numbers() {
        let value: Value = serde_json::from_str(
            "[0, -1, 18446744073709551615, -9223372036854775808, 1.5, 1e3, 0.1, -0.0]",
        )
        .unwrap();
        assert_eq!(
            to_canonical_string(&value),
            "[0,-1,18446744073709551615,-9223372036854775808,1.5,1000.0,0.1,-0.0]"
        );
    }

    #[test]
    fn canonical_form_is_a_fixed_point() {
        let value = json!({"k": [1, 2.25, "x", {"b": null, "a": false}]});
        let once = to_canonical_string(&value);
        let reparsed: Value = serde_json::from_str(&once).unwrap();
        assert_eq!(reparsed, value);
        assert_eq!(to_canonical_string(&reparsed), once);
    }
}
