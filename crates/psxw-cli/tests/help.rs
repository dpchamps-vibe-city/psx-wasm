//! Exercise the actual binary outside a psx-wasm or mods workspace.

use std::process::{Command, Output};

fn psxw(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_psxw"))
        .args(args)
        .current_dir(std::env::temp_dir())
        .env("NO_COLOR", "1")
        .output()
        .expect("run psxw")
}

fn help(args: &[&str]) -> String {
    let output = psxw(args);
    assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
    assert!(output.stderr.is_empty(), "{args:?}: unexpected stderr");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("PSX WASM // MIPS IN. WASM OUT."));
    assert!(text.contains("/ __ \\/ ___/"), "missing ASCII banner");
    assert!(!text.contains('\x1b'), "redirected help must not contain ANSI escapes");
    text
}

#[test]
fn help_aliases_work_without_a_workspace() {
    let expected = help(&["--help"]);
    for args in [&[][..], &["-h"], &["help"]] {
        assert_eq!(help(args), expected);
    }
    assert!(expected.contains("Build & play:"));
    assert!(expected.contains("Inspect & translate:"));
    assert!(expected.contains("Test & validate:"));
    assert!(expected.contains("Examples:"));
}

#[test]
fn command_help_works_before_or_after_game_arguments() {
    let expected = help(&["run", "--help"]);
    for args in [&["help", "run"][..], &["run", "-h"], &["run", "btn", "--help"]] {
        assert_eq!(help(args), expected);
    }
    assert!(expected.contains("Usage: psxw run <game> [options]"));
    assert!(expected.contains("--replay <name|file>"));
    assert!(expected.contains("--load-mod <file>"));
    assert!(expected.contains("Capture & diagnostics:"));
}

#[test]
fn every_top_level_command_has_help() {
    for command in [
        "toolchain", "inputs", "build", "build-web", "run", "serve", "record",
        "runtime", "census", "lift", "identity", "report", "protos", "disasm",
        "layout", "difftest", "link-test", "ci", "conform", "web-test", "mod", "oracle",
    ] {
        assert!(help(&[command, "--help"]).contains(&format!("Usage: psxw {command}")));
    }
}

#[test]
fn grouped_help_includes_nested_commands_and_options() {
    let mods = help(&["mod", "--help"]);
    assert!(mods.contains("In a mods workspace (psxmods.json):"));
    assert!(mods.contains("In the psx-wasm project:"));
    assert!(mods.contains("--script"));
    let oracle = help(&["oracle", "--help"]);
    assert!(oracle.contains("images build"));
    assert!(oracle.contains("--missing-hw"));
    let web = help(&["web-test", "--help"]);
    assert!(web.contains("pacing <game>"));
    assert!(web.contains("--browsers <name,...>"));
}

#[test]
fn unknown_help_topics_are_errors_not_workspace_lookups() {
    for args in [&["help", "not-a-command"][..], &["not-a-command", "--help"]] {
        let output = psxw(args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("unknown help topic"));
        assert!(error.contains("psxw --help"));
    }
}
