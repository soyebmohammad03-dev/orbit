//! `orbit-lab`: run and inspect lab experiments from the command line.
//!
//! ```text
//! orbit-lab [--db PATH] [--lab DIR] <command>
//!
//! commands:
//!   implementations                   list registered implementations
//!   run --stream S --left ID --right ID [--from N] [--to N]
//!       [--reference left|right] [--no-determinism-check]
//!   run --spec FILE.json              run a spec read from a JSON file
//!   list [--limit N]                  newest experiments first
//!   show ID                           one experiment, as JSON
//!   verify ID                         check stored digests and artifacts
//!   reproduce ID                      execute again and compare (a
//!                                     performance experiment is measured again)
//!   trace ID [--from N] [--limit N]   per-boundary state digests
//!   workloads                         list the performance workloads
//!   measure WORKLOAD [--input-size N] [--warmup N] [--iterations N] [--seed N]
//!                                     run a performance experiment
//!   compare [--baseline ID] CANDIDATE... [--threshold PERCENT] [--alpha A]
//!                                     compare performance experiments
//!   compare-runs --baseline ID,ID,… --candidate ID,ID,… [--threshold P] [--alpha A]
//!                                     compare sets of independent runs on
//!                                     their per-run medians
//!   export ID --out FILE              write an evidence bundle (with the
//!                                     input history when --db has it)
//!   validate FILE [--trusted-keys F]  validate a bundle; nothing is stored
//!   import FILE [--trusted-keys F]    validate and import a bundle
//!   keygen --key-id ID --out KEY [--public-out TRUSTED]
//!                                     create an Ed25519 signing key (and a
//!                                     trusted-keys file with its public key)
//!   sign FILE --key KEY --out SIGNED  sign a valid bundle
//!   bundles [--limit N]               imported bundles, newest first
//!   reproduce-bundle DIGEST           reproduce an imported bundle
//! ```
//!
//! Performance numbers from a debug build are labelled `smoke`; measure
//! with `cargo run --release -p orbit-lab -- measure …`.
//!
//! `--db` defaults to `$ORBIT_DB_PATH` or `data/orbit.db`; `--lab` to
//! `$ORBIT_LAB_DIR` or `data/lab`. The event store must already exist; the
//! lab only reads it. Output is JSON on stdout.

use std::path::PathBuf;
use std::process::ExitCode;

use orbit_core::{EventStore, StoreConfig};
use orbit_lab::performance::{self, PerformanceSpec, RegressionPolicy};
use orbit_lab::{Catalog, DifferentialSpec, ExperimentId, Lab, Side};
use serde_json::{Value, json};

const USAGE: &str = "usage: orbit-lab [--db PATH] [--lab DIR] <implementations|run|list|show|verify|reproduce|trace|workloads|measure|compare> [options]
  run --stream S --left ID --right ID [--from N] [--to N] [--reference left|right] [--no-determinism-check]
  run --spec FILE.json
  list [--limit N]
  show ID | verify ID | reproduce ID
  trace ID [--from N] [--limit N]
  workloads
  measure WORKLOAD [--input-size N] [--warmup N] [--iterations N] [--seed N]
  compare [--baseline ID] CANDIDATE... [--threshold PERCENT] [--alpha A]
  compare-runs --baseline ID,ID,... --candidate ID,ID,... [--threshold PERCENT] [--alpha A]
  export ID --out FILE | validate FILE [--trusted-keys F] | import FILE [--trusted-keys F]
  bundles [--limit N] | reproduce-bundle DIGEST
  keygen --key-id ID --out KEY [--public-out TRUSTED] | sign FILE --key KEY --out SIGNED";

struct Args {
    db: PathBuf,
    lab: PathBuf,
    command: String,
    rest: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut db = std::env::var("ORBIT_DB_PATH")
        .map_or_else(|_| PathBuf::from("data/orbit.db"), PathBuf::from);
    let mut lab =
        std::env::var("ORBIT_LAB_DIR").map_or_else(|_| PathBuf::from("data/lab"), PathBuf::from);
    loop {
        match args.next().as_deref() {
            Some("--db") => db = args.next().ok_or("--db needs a path")?.into(),
            Some("--lab") => lab = args.next().ok_or("--lab needs a directory")?.into(),
            Some("-h" | "--help") | None => return Err(USAGE.to_owned()),
            Some(command) => {
                return Ok(Args {
                    db,
                    lab,
                    command: command.to_owned(),
                    rest: args.collect(),
                });
            }
        }
    }
}

fn option<'a>(rest: &'a [String], name: &str) -> Option<&'a str> {
    rest.iter()
        .position(|a| a == name)
        .and_then(|i| rest.get(i + 1))
        .map(String::as_str)
}

fn number(rest: &[String], name: &str) -> Result<Option<u64>, String> {
    option(rest, name)
        .map(|v| {
            v.parse::<u64>()
                .map_err(|_| format!("{name} needs a non-negative integer"))
        })
        .transpose()
}

fn experiment_id(rest: &[String]) -> Result<ExperimentId, String> {
    rest.first()
        .ok_or("an experiment id is required")?
        .parse()
        .map_err(|e: orbit_lab::LabError| e.to_string())
}

fn open_store(args: &Args) -> Result<EventStore, String> {
    if !args.db.is_file() {
        return Err(format!(
            "no event store at {} (set --db or ORBIT_DB_PATH)",
            args.db.display()
        ));
    }
    EventStore::open(StoreConfig::new(&args.db)).map_err(|e| e.to_string())
}

fn spec_from_args(rest: &[String]) -> Result<DifferentialSpec, String> {
    if let Some(file) = option(rest, "--spec") {
        let text = std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?;
        let value: Value = serde_json::from_str(&text).map_err(|e| format!("{file}: {e}"))?;
        return DifferentialSpec::from_json(value).map_err(|e| e.to_string());
    }
    let required = |name: &str| {
        option(rest, name)
            .map(str::to_owned)
            .ok_or_else(|| format!("run needs {name} (or --spec FILE)"))
    };
    let reference = match option(rest, "--reference") {
        None => None,
        Some("left") => Some(Side::Left),
        Some("right") => Some(Side::Right),
        Some(other) => return Err(format!("--reference must be left or right, not {other:?}")),
    };
    Ok(DifferentialSpec {
        stream: required("--stream")?,
        from: number(rest, "--from")?.unwrap_or(0),
        to: number(rest, "--to")?,
        left: required("--left")?,
        right: required("--right")?,
        reference,
        determinism_check: !rest.iter().any(|a| a == "--no-determinism-check"),
    })
}

fn main_inner() -> Result<Value, String> {
    let args = parse_args()?;
    let lab = Lab::open(&args.lab, Catalog::builtin()).map_err(|e| e.to_string())?;
    Ok(match args.command.as_str() {
        "implementations" => {
            let all: Vec<_> = lab
                .catalog()
                .all()
                .iter()
                .map(|i| i.descriptor().clone())
                .collect();
            json(&all)?
        }
        "run" => {
            let spec = spec_from_args(&args.rest)?;
            let store = open_store(&args)?;
            json(
                &lab.create_and_run(&spec, &store)
                    .map_err(|e| e.to_string())?,
            )?
        }
        "list" => {
            let limit = number(&args.rest, "--limit")?.unwrap_or(20);
            let limit = usize::try_from(limit).map_err(|_| "--limit is too large")?;
            json(&lab.list(limit, None).map_err(|e| e.to_string())?)?
        }
        "show" => {
            let id = experiment_id(&args.rest)?;
            let record = lab.experiment(id).map_err(|e| e.to_string())?;
            let verifications = lab.verifications(id).map_err(|e| e.to_string())?;
            json!({ "experiment": record, "verifications": verifications })
        }
        "verify" => {
            let id = experiment_id(&args.rest)?;
            let (report, _) = lab.verify_artifacts(id).map_err(|e| e.to_string())?;
            json(&report)?
        }
        "reproduce" => {
            let id = experiment_id(&args.rest)?;
            let record = lab.experiment(id).map_err(|e| e.to_string())?;
            if record.kind == performance::PERFORMANCE {
                let (verification, attempt) = lab
                    .reproduce_performance(id, &RegressionPolicy::default())
                    .map_err(|e| e.to_string())?;
                json!({ "verification": verification, "attempt": attempt })
            } else {
                let store = open_store(&args)?;
                json(&lab.reproduce(id, &store).map_err(|e| e.to_string())?)?
            }
        }
        "workloads" => json(&performance::WORKLOADS)?,
        "measure" => {
            let workload = args
                .rest
                .first()
                .filter(|w| !w.starts_with("--"))
                .ok_or("measure needs a workload id (see `workloads`)")?;
            let spec = PerformanceSpec {
                workload: workload.clone(),
                workload_version: None,
                input_size: number(&args.rest, "--input-size")?,
                warmup_iterations: number(&args.rest, "--warmup")?,
                measured_iterations: number(&args.rest, "--iterations")?,
                seed: number(&args.rest, "--seed")?,
            };
            json(&lab.measure_performance(&spec).map_err(|e| e.to_string())?)?
        }
        "compare" => {
            let baseline = option(&args.rest, "--baseline")
                .map(|v| v.parse::<ExperimentId>().map_err(|e| e.to_string()))
                .transpose()?;
            let mut candidates = Vec::new();
            let mut rest = args.rest.iter();
            while let Some(a) = rest.next() {
                if a.starts_with("--") {
                    rest.next();
                } else {
                    candidates.push(a.parse::<ExperimentId>().map_err(|e| e.to_string())?);
                }
            }
            let float = |name: &str, default: f64| -> Result<f64, String> {
                option(&args.rest, name).map_or(Ok(default), |v| {
                    v.parse().map_err(|_| format!("{name} needs a number"))
                })
            };
            let defaults = RegressionPolicy::default();
            let policy = RegressionPolicy {
                threshold_percent: float("--threshold", defaults.threshold_percent)?,
                alpha: float("--alpha", defaults.alpha)?,
            }
            .validated()?;
            lab.compare_performance(baseline, &candidates, &policy)
                .map_err(|e| e.to_string())?
        }
        "trace" => {
            let id = experiment_id(&args.rest)?;
            let limit = number(&args.rest, "--limit")?.unwrap_or(100);
            let limit = usize::try_from(limit).map_err(|_| "--limit is too large")?;
            let from = number(&args.rest, "--from")?;
            json(&lab.trace(id, from, limit).map_err(|e| e.to_string())?)?
        }
        "compare-runs" => {
            let ids = |name: &str| -> Result<Vec<ExperimentId>, String> {
                option(&args.rest, name)
                    .ok_or_else(|| format!("compare-runs needs {name} ID,ID,..."))?
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.parse::<ExperimentId>().map_err(|e| e.to_string()))
                    .collect()
            };
            let float = |name: &str, default: f64| -> Result<f64, String> {
                option(&args.rest, name).map_or(Ok(default), |v| {
                    v.parse().map_err(|_| format!("{name} needs a number"))
                })
            };
            let defaults = RegressionPolicy::default();
            let policy = RegressionPolicy {
                threshold_percent: float("--threshold", defaults.threshold_percent)?,
                alpha: float("--alpha", defaults.alpha)?,
            }
            .validated()?;
            lab.compare_performance_runs(&ids("--baseline")?, &ids("--candidate")?, &policy)
                .map_err(|e| e.to_string())?
        }
        "export" => {
            let id = experiment_id(&args.rest)?;
            let out = option(&args.rest, "--out").ok_or("export needs --out FILE")?;
            let store = if args.db.is_file() {
                Some(open_store(&args)?)
            } else {
                None
            };
            let bundle = lab
                .export_bundle(id, store.as_ref())
                .map_err(|e| e.to_string())?;
            std::fs::write(out, &bundle.bytes).map_err(|e| format!("cannot write {out}: {e}"))?;
            json!({
                "bundle_digest": bundle.digest,
                "bytes": bundle.bytes.len(),
                "history_included": bundle.history_included,
                "file": out,
            })
        }
        "validate" | "import" => {
            let file = args.rest.first().ok_or("a bundle file is required")?;
            let bytes = read_bounded(file)?;
            let trust = option(&args.rest, "--trusted-keys")
                .map(|f| {
                    orbit_lab::signing::TrustStore::read(f.as_ref()).map_err(|e| e.to_string())
                })
                .transpose()?;
            if args.command == "validate" {
                json(&orbit_lab::bundle::validate_with(&bytes, trust.as_ref()).0)?
            } else {
                match lab.import_bundle_with(&bytes, trust.as_ref()) {
                    Ok(outcome) => json(&outcome)?,
                    Err(orbit_lab::LabError::InvalidBundle(report)) => {
                        return Err(format!(
                            "the bundle is not valid; nothing was imported\n{}",
                            serde_json::to_string_pretty(&report).unwrap_or_default()
                        ));
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
        }
        "keygen" => {
            let key_id = option(&args.rest, "--key-id").ok_or("keygen needs --key-id ID")?;
            let out = option(&args.rest, "--out").ok_or("keygen needs --out FILE")?;
            let key =
                orbit_lab::signing::SigningKeyFile::generate(key_id).map_err(|e| e.to_string())?;
            key.write_new(out.as_ref())
                .map_err(|e| format!("cannot write {out}: {e}"))?;
            let public = key.public().map_err(|e| e.to_string())?;
            if let Some(trusted) = option(&args.rest, "--public-out") {
                let store = orbit_lab::signing::TrustStore::new(vec![public.clone()])
                    .map_err(|e| e.to_string())?;
                std::fs::write(
                    trusted,
                    serde_json::to_string_pretty(&store).map_err(|e| e.to_string())?,
                )
                .map_err(|e| format!("cannot write {trusted}: {e}"))?;
            }
            // The secret key is written only to the key file, never printed.
            json!({ "key_file": out, "public": public })
        }
        "sign" => {
            let file = args.rest.first().ok_or("a bundle file is required")?;
            let key = option(&args.rest, "--key").ok_or("sign needs --key KEY")?;
            let out = option(&args.rest, "--out").ok_or("sign needs --out FILE")?;
            let key = orbit_lab::signing::SigningKeyFile::read(key.as_ref())
                .map_err(|e| e.to_string())?;
            let signed =
                orbit_lab::bundle::sign(&read_bounded(file)?, &key).map_err(|e| e.to_string())?;
            std::fs::write(out, &signed).map_err(|e| format!("cannot write {out}: {e}"))?;
            json!({ "file": out, "key_id": key.key_id, "bytes": signed.len() })
        }
        "bundles" => {
            let limit = number(&args.rest, "--limit")?.unwrap_or(20);
            let limit = usize::try_from(limit).map_err(|_| "--limit is too large")?;
            json(
                &lab.registry()
                    .imported_bundles(limit, None)
                    .map_err(|e| e.to_string())?,
            )?
        }
        "reproduce-bundle" => {
            let digest = args.rest.first().ok_or("a bundle digest is required")?;
            let digest = orbit_core::Digest::from_hex(digest).map_err(|e| e.to_string())?;
            json(
                &lab.reproduce_bundle(digest, &RegressionPolicy::default())
                    .map_err(|e| e.to_string())?,
            )?
        }
        other => return Err(format!("unknown command {other:?}\n{USAGE}")),
    })
}

/// Reads at most one byte more than a bundle may have, so an oversized
/// file is rejected by validation without being read whole.
fn read_bounded(file: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(file)
        .and_then(|f| {
            f.take(orbit_lab::bundle::MAX_BUNDLE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|e| format!("cannot read {file}: {e}"))?;
    Ok(bytes)
}

fn json<T: serde::Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    match main_inner() {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
            );
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("orbit-lab: {message}");
            ExitCode::FAILURE
        }
    }
}
