//! Census defaults and build prerequisites without real compilers or a disc.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> (PathBuf, &'static str) {
    let root = std::env::temp_dir().join(format!("psxw-census-{name}-{}", std::process::id()));
    let snapshot = root.join(".inputs/test-1234567");
    let clang = root.join(".toolchain/test/bin/clang");
    for dir in [snapshot.join("splat"), root.join("games/test"), clang.parent().unwrap().to_path_buf()] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(root.join("inputs.lock"), r#"
[toolchain.clang]
dist = "test"
url = "unused"
sha256 = "unused"
[game.test]
repo = "missing-checkout"
commit = "1234567890"
"#).unwrap();
    // An empty, already-materialized snapshot: no git repository or TUs needed.
    fs::write(snapshot.join(".psxw-snapshot"), "1234567890").unwrap();
    fs::write(snapshot.join("splat/exe.yaml"), "options:\n  src_path: src\nsegments: []\n").unwrap();
    // Only --version should run; any actual compilation is unexpected.
    fs::write(&clang, "#!/bin/sh\n[ \"$*\" = --version ] || exit 1\necho test-clang\n").unwrap();
    fs::set_permissions(&clang, fs::Permissions::from_mode(0o755)).unwrap();
    let manifest = r#"
[game]
id = "test"
title = "Test"
input = "test"
[containers]
splat_dir = "splat"
symbols_dir = "symbols"
main = "exe"
[c]
include = []
[probe]
include_dirs = []
[reference]
cpp = "missing-cpp"
cppflags = []
cc1 = "missing/cc1"
cc1flags = []
"#;
    (root, manifest)
}

#[test]
fn census_honors_manifest_probe_default_and_cli_overrides() {
    let (root, manifest) = fixture("defaults");
    for (setting, flags, skipped) in [
        ("[ci]\ncensus_no_probe = true", vec![], true),
        ("[ci]\ncensus_no_probe = true", vec!["--probe"], false),
        ("[ci]\ncensus_no_probe = false", vec![], false),
        ("", vec![], false),
        ("", vec!["--no-probe"], true),
    ] {
        fs::write(root.join("games/test/game.toml"), format!("{manifest}\n{setting}\n")).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_psxw"))
            .args(["census", "test"])
            .args(&flags)
            .current_dir(&root)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.success(), skipped, "{setting}, {flags:?}: {stderr}");
        if skipped {
            assert!(String::from_utf8_lossy(&output.stdout).contains("Layout probe: skipped"));
            let report: serde_json::Value = serde_json::from_slice(
                &fs::read(root.join("out/census/test/1234567/census.json")).unwrap(),
            ).unwrap();
            assert!(report["layout"].is_null());
            assert_eq!(report["verdict"]["pass"], true);
        } else {
            assert!(stderr.contains("missing/cc1"), "{stderr}");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn build_runs_census_before_building_the_runtime() {
    let (root, manifest) = fixture("build");
    fs::write(root.join("games/test/game.toml"), format!("{manifest}\n[ci]\ncensus_no_probe = true\n")).unwrap();
    assert!(!root.join("out/census").exists());
    let output = Command::new(env!("CARGO_BIN_EXE_psxw"))
        .args(["build", "test"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("build: census passed"), "{stderr}");
    let report: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("out/census/test/1234567/census.json")).unwrap(),
    ).unwrap();
    assert!(report["layout"].is_null());
    assert_eq!(report["verdict"]["pass"], true);
    // The synthetic workspace has no Cargo.toml: the next step fails, not census.
    assert!(!output.status.success());
    assert!(stderr.contains("building the runtime failed"), "{stderr}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn build_stops_when_census_reports_a_failure() {
    let (root, manifest) = fixture("failed-build");
    fs::write(root.join("games/test/game.toml"), format!("{manifest}\n[ci]\ncensus_no_probe = true\n")).unwrap();
    let snapshot = root.join(".inputs/test-1234567");
    fs::write(snapshot.join("splat/exe.yaml"), "options:\n  src_path: src\nsegments:\n  - {type: code, start: 0, vram: 0x80010000}\n  - [4]\n").unwrap();
    fs::create_dir_all(snapshot.join("symbols")).unwrap();
    fs::write(snapshot.join("symbols/exe.txt"), "missing = 0x80010000; // type:func size:4\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_psxw"))
        .args(["build", "test"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("census failed: 1 game functions with no implementation"), "{stderr}");
    assert!(stderr.contains("census.json"), "{stderr}");
    assert!(!stderr.contains("building the runtime failed"), "{stderr}");
    fs::remove_dir_all(root).unwrap();
}
