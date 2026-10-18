//! `orbit-reliability`: runs the lab's reliability scenarios.
//!
//! Built only with the `reliability` feature:
//!
//! ```text
//! cargo run -p orbit-lab --features reliability --bin orbit-reliability -- <command>
//!
//! commands:
//!   scenarios                          list the supported scenarios
//!   run SCENARIO [--param NAME=VALUE]  create and run a reliability experiment
//!   run-id ID                          run an already recorded (created)
//!                                      reliability experiment
//!   reproduce ID                       run it again and compare
//! ```
//!
//! `--lab` (default `$ORBIT_LAB_DIR` or `data/lab`) selects the lab. Every
//! scenario works on files it creates in `<lab>/reliability-work/`, never on
//! an existing event store. Output is JSON on stdout.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use orbit_lab::reliability::{ReliabilitySpec, SCENARIOS, runner};
use orbit_lab::{Catalog, ExperimentId, Lab};
use serde_json::Value;

const USAGE: &str = "usage: orbit-reliability [--lab DIR] <scenarios|run SCENARIO [--param NAME=VALUE]...|run-id ID|reproduce ID>";

fn json<T: serde::Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn main_inner() -> Result<Value, String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("__child") {
        let role = args.get(1).ok_or("missing child role")?;
        orbit_lab::reliability::child::main(role);
    }
    let mut lab_dir =
        std::env::var("ORBIT_LAB_DIR").map_or_else(|_| PathBuf::from("data/lab"), PathBuf::from);
    if args.first().map(String::as_str) == Some("--lab") {
        lab_dir = args.get(1).ok_or("--lab needs a directory")?.into();
        args.drain(..2);
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this executable: {e}"))?;
    match args.first().map(String::as_str) {
        Some("scenarios") => json(&SCENARIOS),
        Some("run") => {
            let scenario = args.get(1).ok_or("run needs a scenario id")?.clone();
            let mut parameters = BTreeMap::new();
            let mut rest = args[2..].iter();
            while let Some(flag) = rest.next() {
                if flag != "--param" {
                    return Err(format!("unexpected argument {flag:?}\n{USAGE}"));
                }
                let pair = rest.next().ok_or("--param needs NAME=VALUE")?;
                let (name, value) = pair.split_once('=').ok_or("--param needs NAME=VALUE")?;
                let value: u64 = value
                    .parse()
                    .map_err(|_| format!("{name} must be a non-negative integer"))?;
                parameters.insert(name.to_owned(), value);
            }
            let lab = Lab::open(&lab_dir, Catalog::builtin()).map_err(|e| e.to_string())?;
            let spec = ReliabilitySpec {
                scenario,
                scenario_version: None,
                parameters,
            };
            let record = lab.create_reliability(&spec).map_err(|e| e.to_string())?;
            json(&runner::run(&lab, record.id, &exe).map_err(|e| e.to_string())?)
        }
        Some("run-id") => {
            // Runs an experiment that was already recorded (status
            // `created`), for example by orbit-api's reliability launcher.
            // Its specification comes from the registry, never from here.
            let id: ExperimentId = args
                .get(1)
                .ok_or("run-id needs an experiment id")?
                .parse()
                .map_err(|e: orbit_lab::LabError| e.to_string())?;
            if args.len() > 2 {
                return Err(format!("unexpected argument {:?}\n{USAGE}", args[2]));
            }
            let lab = Lab::open(&lab_dir, Catalog::builtin()).map_err(|e| e.to_string())?;
            json(&runner::run(&lab, id, &exe).map_err(|e| e.to_string())?)
        }
        Some("reproduce") => {
            let id: ExperimentId = args
                .get(1)
                .ok_or("reproduce needs an experiment id")?
                .parse()
                .map_err(|e: orbit_lab::LabError| e.to_string())?;
            let lab = Lab::open(&lab_dir, Catalog::builtin()).map_err(|e| e.to_string())?;
            json(&runner::reproduce(&lab, id, &exe).map_err(|e| e.to_string())?)
        }
        _ => Err(USAGE.to_owned()),
    }
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
            eprintln!("orbit-reliability: {message}");
            ExitCode::FAILURE
        }
    }
}
