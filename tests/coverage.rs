//! Drift guard between the management API and the CLI's command surface.
//!
//! Reads a **checked-in snapshot** of `/v1/openapi.json`, never the network:
//! a test that fetches would be flaky, would fail offline, and would turn a
//! server deploy into a surprise red build on an unrelated commit. The
//! snapshot is refreshed by `scripts/refresh-openapi.sh`, so drift arrives as
//! a reviewable diff.
//!
//! The failure this exists to catch: the API grows an endpoint and no command
//! can reach it.

use keylight_cli::endpoints::{COVERED, NOT_COMMANDS, PENDING};
use std::collections::BTreeSet;

const SPEC: &str = include_str!("fixtures/openapi.json");

/// Every `(METHOD, path)` operation in the spec.
fn spec_operations() -> BTreeSet<(String, String)> {
    let spec: serde_json::Value = serde_json::from_str(SPEC).expect("snapshot is valid JSON");
    let paths = spec["paths"].as_object().expect("spec has a paths object");
    let mut out = BTreeSet::new();
    for (path, item) in paths {
        for method in item.as_object().expect("path item is an object").keys() {
            let m = method.to_uppercase();
            // `parameters`, `summary` and friends sit alongside the verbs.
            if matches!(m.as_str(), "GET" | "POST" | "PATCH" | "PUT" | "DELETE") {
                out.insert((m, path.clone()));
            }
        }
    }
    out
}

fn registry() -> BTreeSet<(String, String)> {
    COVERED
        .iter()
        .chain(NOT_COMMANDS)
        .chain(PENDING)
        .map(|(m, p, _)| (m.to_string(), p.to_string()))
        .collect()
}

/// The core guard: nothing in the API may be unaccounted for.
#[test]
fn every_endpoint_is_reachable_or_explicitly_accounted_for() {
    let missing: Vec<_> = spec_operations().difference(&registry()).cloned().collect();
    assert!(
        missing.is_empty(),
        "these endpoints exist in the API but no command reaches them, and \
         they are not listed in NOT_COMMANDS or PENDING in src/endpoints.rs:\n{}",
        missing
            .iter()
            .map(|(m, p)| format!("  {m} {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The other direction: a registry entry naming an endpoint the API no longer
/// has is a stale claim of coverage, which is worse than no claim.
#[test]
fn no_registry_entry_points_at_a_nonexistent_endpoint() {
    let spec = spec_operations();
    let stale: Vec<_> = registry().difference(&spec).cloned().collect();
    assert!(
        stale.is_empty(),
        "src/endpoints.rs claims these endpoints, but they are not in the \
         spec snapshot — a typo, or the API dropped them:\n{}",
        stale
            .iter()
            .map(|(m, p)| format!("  {m} {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// An endpoint must not be listed twice, which would let a `PENDING` entry
/// mask a real gap after the command shipped.
#[test]
fn no_endpoint_is_registered_in_two_places() {
    let mut seen = BTreeSet::new();
    let mut dupes = Vec::new();
    for (m, p, _) in COVERED.iter().chain(NOT_COMMANDS).chain(PENDING) {
        if !seen.insert((m, p)) {
            dupes.push(format!("  {m} {p}"));
        }
    }
    assert!(
        dupes.is_empty(),
        "listed more than once in src/endpoints.rs:\n{}",
        dupes.join("\n")
    );
}

/// Not an assertion about correctness — it prints what is left so the
/// remaining work stays visible instead of being quietly uncounted.
#[test]
fn report_pending_command_groups() {
    if PENDING.is_empty() {
        println!("command surface complete: every endpoint is reachable");
        return;
    }
    println!("{} endpoint(s) still without a command:", PENDING.len());
    for (m, p, cmd) in PENDING {
        println!("  {m:6} {p}  ->  keylight {cmd}");
    }
}
