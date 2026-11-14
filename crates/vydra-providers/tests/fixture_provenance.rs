//! Fixture hygiene: a fixture without provenance is a rumour.
//!
//! `tests/fixtures/README.md` states four rules for the corpus, and until Phase 5B only prose enforced
//! them. This module enforces the ones a machine can check, so the next fixture cannot be added without
//! saying what it is:
//!
//! 1. every `*.json` fixture has a sibling `*.meta.json`;
//! 2. the sidecar declares a `class` from the closed set used by the corpus;
//! 3. the directory a fixture lives in agrees with its declared class — a synthetic payload cannot be
//!    filed under `recorded/`, which is the one mistake that would make a hand-built document look like
//!    evidence;
//! 4. a `synthetic` sidecar carries a note saying it is not a capture.
//!
//! What this module does **not** do is prove a recorded fixture is genuine; that is what the SHA-256
//! digests in `docs/research/PLAYER_FIXTURE_PROVENANCE.md` and
//! `docs/research/data/**/manifest.json` are for.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};

use serde_json::Value;

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext.to_string_lossy() == "json")
            && !path.to_string_lossy().ends_with(".meta.json")
        {
            out.push(path);
        }
    }
}

fn fixtures() -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(&fixtures_root(), &mut out);
    out.sort();
    assert!(!out.is_empty(), "the fixture corpus must not be empty");
    out
}

fn sidecar(fixture: &Path) -> PathBuf {
    fixture.with_extension("meta.json")
}

fn declared_class(fixture: &Path) -> String {
    let path = sidecar(fixture);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{} has no provenance sidecar: {err}", fixture.display()));
    let meta: Value = serde_json::from_str(&raw).expect("sidecar is JSON");
    meta.get("class")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{} declares no class", path.display()))
        .to_owned()
}

#[test]
fn every_fixture_declares_a_class_from_the_closed_set() {
    const CLASSES: [&str; 4] = [
        "synthetic",
        "recorded_vydra_probe",
        "recorded_upstream_subset",
        "derived_malformed",
    ];
    for fixture in fixtures() {
        let class = declared_class(&fixture);
        assert!(
            CLASSES.contains(&class.as_str()),
            "{}: unknown class {class}",
            fixture.display()
        );
        // A recorded or synthetic payload must be parseable JSON: a corrupt fixture would fail at read
        // time in a way that looks like a parser bug. `derived_malformed/` is the one class where
        // invalid JSON is the *point* — those files exist to prove the parser fails closed — so only
        // their sidecars are checked.
        if class != "derived_malformed" {
            let raw = std::fs::read_to_string(&fixture).expect("fixture readable");
            serde_json::from_str::<Value>(&raw)
                .unwrap_or_else(|err| panic!("{} is not JSON: {err}", fixture.display()));
        }
    }
}

#[test]
fn the_directory_agrees_with_the_declared_class() {
    for fixture in fixtures() {
        let directory = fixture
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        let class = declared_class(&fixture);
        let expected: &[&str] = match directory.as_str() {
            "synthetic" => &["synthetic"],
            "recorded" => &["recorded_vydra_probe", "recorded_upstream_subset"],
            "malformed" => &["derived_malformed"],
            other => panic!("unexpected fixture directory {other}"),
        };
        assert!(
            expected.contains(&class.as_str()),
            "{} is filed under {directory}/ but declares {class}",
            fixture.display()
        );
    }
}

#[test]
fn a_synthetic_fixture_says_it_is_not_a_capture() {
    for fixture in fixtures() {
        if declared_class(&fixture) != "synthetic" {
            continue;
        }
        let raw = std::fs::read_to_string(sidecar(&fixture)).expect("sidecar readable");
        let meta: Value = serde_json::from_str(&raw).expect("sidecar is JSON");
        let note = meta
            .get("note")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            note.contains("not a captur"),
            "{}: a synthetic fixture must say it is not a capture (note: {note})",
            fixture.display()
        );
        let not_verified = meta.get("not_verified").and_then(Value::as_array);
        assert!(
            not_verified.is_some_and(|entries| !entries.is_empty()),
            "{}: a synthetic fixture must list what it does not prove",
            fixture.display()
        );
    }
}
