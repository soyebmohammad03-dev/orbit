//! Read-only loader for Criterion results (`<dir>/**/new/estimates.json`).
//!
//! Only files named `benchmark.json` and `estimates.json` inside the
//! configured directory are read, at a bounded depth. Nothing is executed and
//! no path supplied by a client is ever used.

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::dto::{BenchmarkResult, BenchmarksResponse};

const MAX_DEPTH: usize = 5;

/// Loads every benchmark result under `dir`.
pub fn load(dir: Option<&Path>) -> BenchmarksResponse {
    let note = "Results come from `cargo bench` (Criterion) and are read from disk; run \
                `cargo bench -p orbit-core` to produce or refresh them."
        .to_owned();
    let Some(dir) = dir.filter(|d| d.is_dir()) else {
        return BenchmarksResponse {
            available: false,
            results: Vec::new(),
            note,
        };
    };
    let mut results = Vec::new();
    walk(dir, 0, &mut results);
    results.sort_by(|a, b| a.id.cmp(&b.id));
    BenchmarksResponse {
        available: true,
        results,
        note,
    }
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<BenchmarkResult>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if path.file_name().is_some_and(|n| n == "new") {
                if let Some(result) = read_result(&path) {
                    out.push(result);
                }
            } else if path.file_name().is_none_or(|n| n != "report") {
                walk(&path, depth + 1, out);
            }
        }
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn read_result(new_dir: &Path) -> Option<BenchmarkResult> {
    let benchmark = read_json(&new_dir.join("benchmark.json"))?;
    let estimates_path = new_dir.join("estimates.json");
    let estimates = read_json(&estimates_path)?;
    let f = |v: &Value| v.as_f64().unwrap_or(f64::NAN);
    let measured_at = fs::metadata(&estimates_path)
        .and_then(|m| m.modified())
        .ok()
        .map(rfc3339);
    Some(BenchmarkResult {
        id: benchmark["full_id"].as_str()?.to_owned(),
        group: benchmark["group_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        function: benchmark["function_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        median_ns: f(&estimates["median"]["point_estimate"]),
        median_lower_ns: f(&estimates["median"]["confidence_interval"]["lower_bound"]),
        median_upper_ns: f(&estimates["median"]["confidence_interval"]["upper_bound"]),
        mean_ns: f(&estimates["mean"]["point_estimate"]),
        confidence_level: f(&estimates["median"]["confidence_interval"]["confidence_level"]),
        throughput: benchmark
            .get("throughput")
            .filter(|t| !t.is_null())
            .cloned(),
        measured_at,
    })
}

fn rfc3339(time: std::time::SystemTime) -> String {
    let micros = time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    orbit_core::Timestamp::from_unix_micros(micros)
        .map_or_else(|_| String::new(), |t| t.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_directory_is_unavailable() {
        let r = load(Some(Path::new("/definitely/not/here")));
        assert!(!r.available);
        assert!(r.results.is_empty());
        assert!(!load(None).available);
    }

    #[test]
    fn reads_criterion_layout() {
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("replay").join("execute_1k").join("new");
        fs::create_dir_all(&new).unwrap();
        fs::write(
            new.join("benchmark.json"),
            json!({"group_id": "replay", "function_id": "execute_1k", "full_id": "replay/execute_1k", "throughput": null}).to_string(),
        )
        .unwrap();
        fs::write(
            new.join("estimates.json"),
            json!({
                "mean": {"point_estimate": 2.0e6, "confidence_interval": {"confidence_level": 0.95, "lower_bound": 1.9e6, "upper_bound": 2.1e6}},
                "median": {"point_estimate": 1.5e6, "confidence_interval": {"confidence_level": 0.95, "lower_bound": 1.4e6, "upper_bound": 1.6e6}}
            })
            .to_string(),
        )
        .unwrap();
        // An unrelated, malformed directory is ignored.
        fs::create_dir_all(dir.path().join("junk").join("new")).unwrap();
        let r = load(Some(dir.path()));
        assert!(r.available);
        assert_eq!(r.results.len(), 1);
        let b = &r.results[0];
        assert_eq!(b.id, "replay/execute_1k");
        assert!((b.median_ns - 1.5e6).abs() < 1.0);
        assert!((b.confidence_level - 0.95).abs() < 1e-9);
        assert!(b.throughput.is_none());
        assert!(b.measured_at.is_some());
    }
}
