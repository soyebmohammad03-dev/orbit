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
//!   reproduce ID                      execute again and compare
//!   trace ID [--from N] [--limit N]   per-boundary state digests
//! ```
//!
//! `--db` defaults to `$ORBIT_DB_PATH` or `data/orbit.db`; `--lab` to
//! `$ORBIT_LAB_DIR` or `data/lab`. The event store must already exist; the
//! lab only reads it. Output is JSON on stdout.

use std::path::PathBuf;
use std::process::ExitCode;

use orbit_core::{EventStore, StoreConfig};
use orbit_lab::{Catalog, DifferentialSpec, ExperimentId, Lab, Side};
use serde_json::{Value, json};

const USAGE: &str = "usage: orbit-lab [--db PATH] [--lab DIR] <implementations|run|list|show|verify|reproduce> [options]
  run --stream S --left ID --right ID [--from N] [--to N] [--reference left|right] [--no-determinism-check]
  run --spec FILE.json
  list [--limit N]
  show ID | verify ID | reproduce ID
  trace ID [--from N] [--limit N]";

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
            let store = open_store(&args)?;
            json(&lab.reproduce(id, &store).map_err(|e| e.to_string())?)?
        }
        "trace" => {
            let id = experiment_id(&args.rest)?;
            let limit = number(&args.rest, "--limit")?.unwrap_or(100);
            let limit = usize::try_from(limit).map_err(|_| "--limit is too large")?;
            let from = number(&args.rest, "--from")?;
            json(&lab.trace(id, from, limit).map_err(|e| e.to_string())?)?
        }
        other => return Err(format!("unknown command {other:?}\n{USAGE}")),
    })
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
