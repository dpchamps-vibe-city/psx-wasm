//! Workspace-independent help for the hand-written CLI parser.

use anyhow::{Result, bail};
use std::io::IsTerminal;

const BANNER: &str = r"   ____  ______  __  _       _____   _____ __  ___
  / __ \/ ___/ |/ / | |     / /   | / ___//  |/  /
 / /_/ /\__ \|   /  | | /| / / /| | \__ \/ /|_/ /
/ ____/___/ /   |   | |/ |/ / ___ |___/ / /  / /
/_/    /____/_/|_|   |__/|__/_/  |_/____/_/  /_/";

const WIDTH: usize = 80;
const DESCRIPTION_COLUMN: usize = 32;
type Row<'a> = (&'a str, &'a str);
type Section<'a> = (&'a str, &'a [Row<'a>]);

/// Handle help before discovering either a game or mods workspace.
pub fn show(args: &[String]) -> Result<bool> {
    let topic = if args.is_empty() {
        args
    } else if args[0] == "help" {
        &args[1..]
    } else if let Some(i) = args.iter().position(|a| a == "--help" || a == "-h") {
        &args[..i]
    } else {
        return Ok(false);
    };
    let topic: Vec<&str> = topic.iter().map(String::as_str).collect();
    let text = render(&topic)?;
    let color = std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var_os("TERM").is_none_or(|t| t != "dumb");
    println!("{}", decorate(&text, color));
    Ok(true)
}

fn decorate(text: &str, color: bool) -> String {
    if !color {
        return format!("\n{BANNER}\n\n  PSX WASM // MIPS IN. WASM OUT.\n\n{text}");
    }
    let mut out = format!("\n\x1b[1;36m{BANNER}\x1b[0m\n\n  \x1b[1;35mPSX WASM // MIPS IN. WASM OUT.\x1b[0m\n\n");
    for line in text.lines() {
        if !line.starts_with(' ') && line.ends_with(':') {
            out.push_str(&format!("\x1b[1;35m{line}\x1b[0m\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Keep descriptions in one column, wrapping long rows rather than letting
/// terminal wrapping run into the next command. Long invocations get a line
/// of their own.
fn page(usage: &str, summary: &str, sections: &[Section<'_>]) -> String {
    let mut out = format!("{summary}\n\nUsage: psxw {usage}\n");
    for (title, rows) in sections {
        out.push_str(&format!("\n{title}:\n"));
        for (label, description) in *rows {
            if label.len() + 4 > DESCRIPTION_COLUMN {
                out.push_str(&format!("  {label}\n{:DESCRIPTION_COLUMN$}", ""));
            } else {
                out.push_str(&format!("  {label:<width$}  ", width = DESCRIPTION_COLUMN - 4));
            }
            let mut column = DESCRIPTION_COLUMN;
            for word in description.split_whitespace() {
                if column + usize::from(column > DESCRIPTION_COLUMN) + word.len() > WIDTH {
                    out.push_str(&format!("\n{:DESCRIPTION_COLUMN$}", ""));
                    column = DESCRIPTION_COLUMN;
                } else if column > DESCRIPTION_COLUMN {
                    out.push(' ');
                    column += 1;
                }
                out.push_str(word);
                column += word.len();
            }
            out.push('\n');
        }
    }
    out
}

fn render(topic: &[&str]) -> Result<String> {
    Ok(match topic {
        [] | ["--help" | "-h"] => overview(),
        ["toolchain", ..] => page("toolchain fetch", "Download and verify the pinned toolchain.", &[
            ("Commands", &[("fetch", "Fetch clang, binaryen, wasm-bindgen and signatures.")]),
        ]),
        ["inputs", ..] => page("inputs sync", "Snapshot the pinned game, SDK and mod repositories.", &[
            ("Commands", &[("sync", "Download pinned repositories into .inputs/.")]),
        ]),
        ["build", ..] => page("build <game> [options]", "Compile a PlayStation game to WebAssembly.", &[
            ("Options", &[
                ("--debug", "Build game-debug.wasm with C source and DWARF."),
                ("--debug-io", "Enable guest I/O diagnostics."),
                ("--ubsan", "Enable undefined-behavior checks."),
                ("--original <lib,...>", "Use original code for these SDK libraries."),
                ("--moddable", "Build a base that can load mod modules at run time."),
                ("--mod <dir>", "Link a mod directory; repeat for multiple mods."),
                ("--mods <dir,...>", "Link a comma-separated list of mod directories."),
            ]),
            ("Notes", &[("With --mod / --mods", "Other build options cannot be combined with mods yet.")]),
        ]),
        ["build-web", ..] => page("build-web <game> [options]", "Build a browser-ready game bundle.", &[
            ("Options", &[
                ("--debug", "Include C source for browser DevTools; no mods yet."),
                ("--moddable", "Load mods in the page instead of linking them."),
                ("--mod <dir>", "Link a mod directory; repeat for multiple mods."),
                ("--mods <dir,...>", "Link a comma-separated list of mod directories."),
            ]),
            ("Next step", &[("psxw serve out/web/<game>", "Serve the bundle locally.")]),
        ]),
        ["run", ..] => page("run <game> [options]", "Run game.wasm headless, replay input and evaluate checks.", &[
            ("Run options", &[
                ("--frames <N>", "Stop after N frames [default: 600]."),
                ("--replay <name|file>", "Use a named replay or a recorded replay file."),
                ("--check <check>", "boot, determinism, no-trap or card; repeatable."),
                ("--cd <mode>", "Use instant or timed CD access."),
                ("--wasm <file>", "Run a specific wasm module."),
                ("--debug", "Run the game-debug.wasm build."),
                ("--headless", "Run without a window (the default)."),
                ("--verbose", "Print detailed runtime diagnostics."),
            ]),
            ("Capture & diagnostics", &[
                ("--render server", "Render through the Docker render server."),
                ("--dump-frames <dir>", "Write rendered frames to a directory."),
                ("--dump-every <N>", "Frame capture interval; set after --dump-frames."),
                ("--audio-wav <file>", "Write the run's audio as a WAV file."),
                ("--hash-at <frame,...>", "Hash RAM at the selected frames."),
                ("--vram-hash-at <frame,...>", "Hash VRAM at the selected frames."),
                ("--dump-at <frame,...>", "Dump RAM at the selected frames."),
                ("--dump-dir <dir>", "Directory for RAM dumps."),
                ("--trace-u32 <address>", "Trace a guest memory word each frame."),
            ]),
            ("Mods & execution", &[
                ("--mod <dir>", "Build with this mod first; repeat for multiple mods."),
                ("--mods <dir,...>", "Build with a comma-separated list of mods."),
                ("--load-mod <file>", "Load a mod module into a moddable base; repeatable."),
                ("--swap-mod <frame:index:path>", "Swap a loaded mod at the given frame; repeatable."),
                ("--execute <frame:func[:args]>", "Call a guest function with up to 4 comma-separated words."),
            ]),
        ]),
        ["serve", ..] => page("serve [dir] [options]", "Serve a browser bundle over HTTP.", &[
            ("Arguments", &[("[dir]", "Bundle directory [default: out/web].")]),
            ("Options", &[("--port <N>", "Listen on this port [default: 8080].")]),
        ]),
        ["record", ..] => page("record <game> [options]", "Serve a game bundle with a replay recorder.", &[
            ("Options", &[
                ("--mod <dir>", "Include a mod directory; repeat for multiple mods."),
                ("--port <N>", "HTTP port [default: 8081]."),
                ("--test", "Run browser/native comparisons instead of serving."),
            ]),
        ]),
        ["runtime", ..] => page("runtime build", "Build the game-independent runtime.wasm module.", &[
            ("Commands", &[("build", "Compile the shared PlayStation runtime to wasm.")]),
        ]),
        ["census", ..] => page("census <game> [options]", "Compile each translation unit and compare it with the original.", &[
            ("Options", &[
                ("--only <filter>", "Only compile translation units whose paths match."),
                ("--only-header <header>", "Limit the census to a header."),
                ("--no-probe", "Skip the layout probe."),
            ]),
        ]),
        ["lift", ..] => page("lift <game> [options]", "Translate original MIPS functions to C.", &[
            ("Options", &[
                ("--function <name>", "Translate one function."),
                ("--container <name>", "Translate functions in one container."),
                ("--all", "Translate every function (the default)."),
            ]),
        ]),
        ["identity", ..] => page("identity <game>", "Report SDK object identities and matching PsyQ versions.", &[]),
        ["report", ..] => page("report tiers <game>", "Report each function's implementation tier, grouped by library.", &[]),
        ["protos", ..] => page("protos <game>", "Inspect cached C prototypes, parameter types and struct returns.", &[]),
        ["disasm", ..] => page("disasm <game> <container> <address> [count]", "Disassemble original MIPS instructions.", &[
            ("Arguments", &[
                ("<address>", "Start address; decimal or 0x-prefixed hex."),
                ("[count]", "Number of instructions [default: 32]."),
            ]),
        ]),
        ["layout", ..] => page("layout <game>", "Regenerate SDK struct layouts declared by the game's boundary.", &[]),
        ["difftest", ..] => page("difftest <game> [options]", "Compare translated functions with decompiled C.", &[
            ("Options", &[
                ("--only <symbol>", "Test a single function."),
                ("--seeds <N>", "Number of test seeds [default: 64]."),
                ("--keep", "Keep generated test modules."),
                ("--verbose", "Print detailed test diagnostics."),
            ]),
        ]),
        ["link-test", ..] => page("link-test <game> --function <name>", "Link, validate and run one original/translated function pair.", &[
            ("Options", &[("--function <name>", "Function to test (required).")]),
        ]),
        ["ci", ..] => page("ci <game>", "Run every implemented milestone's validation gates.", &[]),
        ["conform", ..] => page("conform <game> --lib <library> [options]", "Compare an SDK library's adapters with its original code.", &[
            ("Options", &[
                ("--lib <library>", "Required: libpad, libgpu, libmcrd, libetc, libsnd or libspu."),
                ("--frames <N>", "Scenario length [default: 120]."),
                ("--render server", "Use the Docker render server."),
                ("--dump-frames <dir>", "Write frames from both sides of the comparison."),
            ]),
        ]),
        ["web-test", ..] => web_test(),
        ["mod", ..] => mods(),
        ["oracle", ..] => oracle(),
        [other, ..] => bail!("unknown help topic {other:?}; run `psxw --help` for commands"),
    })
}

fn overview() -> String {
    page("<command> [arguments] [options]", "Turn PlayStation decompilations into WebAssembly.", &[
        ("Setup", &[
            ("toolchain fetch", "Download and verify the pinned toolchain."),
            ("inputs sync", "Snapshot pinned game, SDK and mod repositories."),
        ]),
        ("Build & play", &[
            ("build <game>", "Compile a game to game.wasm."),
            ("build-web <game>", "Build a browser-ready bundle."),
            ("run <game>", "Run headless, replay input and evaluate checks."),
            ("serve [dir]", "Serve a browser bundle over HTTP."),
            ("record <game>", "Serve a bundle with a replay recorder."),
            ("runtime build", "Build the shared runtime.wasm module."),
        ]),
        ("Inspect & translate", &[
            ("census <game>", "Compile and compare every translation unit."),
            ("lift <game>", "Translate original MIPS functions to C."),
            ("identity <game>", "Identify SDK objects and PsyQ versions."),
            ("report tiers <game>", "Summarize implementation tiers by library."),
            ("protos <game>", "Inspect cached C function prototypes."),
            ("disasm <game> ...", "Disassemble original MIPS instructions."),
            ("layout <game>", "Regenerate SDK struct layouts."),
        ]),
        ("Test & validate", &[
            ("ci <game>", "Run all implemented milestone gates."),
            ("difftest <game>", "Compare translated functions with decompiled C."),
            ("link-test <game>", "Link and test a single function pair."),
            ("conform <game>", "Check SDK adapters against original code."),
            ("web-test <command>", "Run browser GPU, storage and game tests."),
        ]),
        ("Modkit & oracle", &[
            ("mod <command>", "Create, check, build, test and play mods."),
            ("oracle <command>", "Capture and compare device reference results."),
        ]),
        ("Help", &[
            ("-h, --help", "Show help; add after a command for its options."),
            ("help [command]", "Show this screen or a command's help."),
        ]),
        ("Examples", &[
            ("psxw build btn", "Compile the game."),
            ("psxw run btn --check boot", "Verify a headless boot from your disc."),
            ("psxw run --help", "See run options, captures and mod loading."),
        ]),
    ])
}

fn web_test() -> String {
    page("web-test <command> [options]", "Run browser tests in the pinned browsers container.", &[
        ("Commands", &[
            ("gpu", "Test browser GPU rendering."),
            ("storage", "Test browser storage."),
            ("conform <game>", "Test SDK conformance in the browser."),
            ("game <game>", "Compare browser and native game runs."),
            ("pacing <game>", "Measure browser frame pacing."),
        ]),
        ("Options", &[
            ("--browsers <name,...>", "Browsers to test, e.g. chromium,firefox."),
            ("--timeout <seconds>", "Browser test timeout (except pacing)."),
            ("--replay <name|file>", "Input replay for game or pacing tests."),
            ("--frames <N>", "Frame limit for game tests."),
            ("--hash <frame,...>", "Comparison frames for game tests."),
            ("--record", "Record reference hashes for game tests."),
            ("--debug", "Use a debug bundle for game tests."),
            ("--seconds <N>", "Duration for pacing tests."),
            ("--moddable", "Use a moddable base for pacing tests."),
            ("--mod <dir>", "Include a mod in pacing tests; repeatable."),
            ("--tag <name>", "Label a pacing test report."),
        ]),
    ])
}

fn mods() -> String {
    page("mod <command> [arguments] [options]", "The modkit: compiled and script mods, from source to play.", &[
        ("In a mods workspace (psxmods.json)", &[
            ("sdk", "Generate or update the pinned mod SDK."),
            ("check [mod|set]", "Validate hooks, types and compatibility."),
            ("build [mod|set]", "Build selected mods."),
            ("test [mod|set]", "Run mod tests."),
            ("play <mod|set>", "Build and serve selected mods."),
            ("watch <mod|set>", "Rebuild and reload mods as they change."),
            ("new <id>", "Create a mod skeleton."),
            ("pack <id>", "Package a script mod."),
            ("show <symbol>", "Inspect a symbol's type, source and tier."),
            ("writers <symbol>", "Find code that writes to a symbol."),
        ]),
        ("In the psx-wasm project", &[
            ("check <game> <dir>...", "Validate mod directories."),
            ("build <game> <dir>...", "Build mod directories."),
            ("test <game> <dir>...", "Run tests for mod directories."),
            ("watch <game> <dir>...", "Rebuild and reload mod directories."),
            ("new <game> <id>", "Create a mod skeleton."),
            ("show <game> <symbol>", "Inspect a symbol."),
            ("writers <game> <symbol>", "Find code that writes to a symbol."),
            ("pack <game> <dir>", "Package a script mod."),
            ("snapshot <game>", "Export or verify a game type snapshot."),
            ("pass-through <game>", "Generate a pass-through mod."),
            ("ci <game>", "Run the modkit validation gate."),
        ]),
        ("Command options", &[
            ("--bump", "sdk: update pins and report API changes."),
            ("--modules", "build: emit standalone mod modules."),
            ("--out <path>", "build, pack, snapshot: output path (project only)."),
            ("--dir <dir>", "new: parent directory (project only)."),
            ("--script", "new, pass-through: generate a JavaScript mod."),
            ("--dynamic", "play: load mods at run time (workspace only)."),
            ("--test", "play: compare browsers with native (compiled mods only)."),
            ("--port <N>", "play, watch: HTTP port [default: 8081]."),
            ("--source <symbols|c|dwarf>", "snapshot: type source [default: c]."),
            ("--at <commit>", "snapshot: use a specific decompilation commit."),
            ("--verify", "snapshot: check layouts and symbol addresses."),
            ("--agree", "snapshot: compare DWARF and header types."),
            ("--out <dir>", "pass-through: output directory (required)."),
            ("--tiers <c,lift,adapter>", "pass-through: restrict compiled hooks to these tiers."),
        ]),
    ])
}

fn oracle() -> String {
    page("oracle <command> [arguments] [options]", "Device reference captures, emulator runs and hardware comparisons.", &[
        ("Commands", &[
            ("gen-header", "Regenerate the oracle wire protocol C header."),
            ("images build", "Build oracle images and the runner."),
            ("images verify", "Verify recorded images against inputs.lock."),
            ("selftest", "Run exact-by-construction GPU vectors."),
            ("capture", "Capture golden results for stored vectors."),
            ("check <game> --device <D>", "Compare a device implementation with goldens."),
            ("ci", "Verify images, self-test and capture."),
            ("play <game>", "Run a replay under the emulator."),
            ("ramdiff <game>", "Compare native and emulator RAM by symbol."),
            ("record <game>", "Record GPU/SPU vectors from a native run."),
            ("export --out <dir>", "Export batches for capture on real hardware."),
            ("import <dir>", "Import hardware capture results."),
            ("disagree", "Compare emulator and hardware goldens."),
        ]),
        ("Image & vector options", &[
            ("--no-cache", "images build: disable the Docker build cache."),
            ("--seed <N>", "selftest: random seed."),
            ("--count <N>", "selftest: number of vectors."),
            ("--kind <kind>", "capture, export: filter by vector kind."),
            ("--missing", "capture: only capture missing goldens."),
            ("--set <name>", "export: select a vector set; repeatable."),
            ("--missing-hw", "export: only vectors without hardware goldens."),
        ]),
        ("Replay & comparison options", &[
            ("--replay <name|file>", "play, ramdiff, record: input replay."),
            ("--frames <N>", "play, ramdiff, record: frame limit."),
            ("--dump-at <frame,...>", "play: dump RAM at selected frames."),
            ("--dump-counter <value,...>", "play: dump RAM at game-counter values."),
            ("--frames-every <N>", "play: capture an image every N frames."),
            ("--check determinism", "play: run twice and compare results."),
            ("--every <N>", "ramdiff: comparison interval; record: capture interval."),
            ("--at <value,...>", "ramdiff: compare at game-counter values."),
            ("--require-equal", "ramdiff: fail if compared RAM differs."),
            ("--compare-frames", "ramdiff: also frames and VRAM; play: dump them."),
            ("--device <gpu|spu>", "record: select a device [default: both]."),
            ("--budget <N>", "record: maximum number of vectors."),
            ("--samples <N>", "record: SPU sample window [default: 8820]."),
        ]),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMANDS: &[&str] = &[
        "toolchain", "inputs", "build", "build-web", "run", "serve", "record",
        "runtime", "census", "lift", "identity", "report", "protos", "disasm",
        "layout", "difftest", "link-test", "ci", "conform", "web-test", "mod", "oracle",
    ];

    #[test]
    fn help_fits_an_eighty_column_terminal() {
        for topic in std::iter::once(&[][..]).chain(COMMANDS.iter().map(std::slice::from_ref)) {
            let text = decorate(&render(topic).unwrap(), false);
            assert!(text.is_ascii());
            assert!(!text.contains('\x1b'));
            for line in text.lines() {
                assert!(line.len() <= WIDTH, "{topic:?}: {} columns: {line}", line.len());
            }
        }
    }

    #[test]
    fn overview_lists_every_command() {
        let text = overview();
        for command in COMMANDS {
            assert!(text.lines().any(|line| line.trim_start().starts_with(command)), "missing {command}");
        }
    }

    #[test]
    fn descriptions_align_and_wrap() {
        let text = page("test", "Test.", &[("Options", &[
            ("--short", "Short description."),
            ("--a-very-long-option <argument>", "Another description."),
            ("--wrapped", "This description is deliberately longer than the remaining space in an eighty-column terminal."),
        ])]);
        assert!(text.contains(&format!("  {:<28}  Short description.", "--short")));
        assert!(text.contains(&format!("  --a-very-long-option <argument>\n{:32}Another", "")));
        assert!(text.lines().filter(|l| l.starts_with(&" ".repeat(32))).count() >= 2);
        assert!(text.lines().all(|line| line.len() <= WIDTH));
    }

    #[test]
    fn a_description_can_fill_the_last_column() {
        let word = "x".repeat(WIDTH - DESCRIPTION_COLUMN);
        let text = page("test", "Test.", &[("Options", &[("--exact", &word)])]);
        assert!(text.lines().any(|line| line.len() == WIDTH && line.ends_with(&word)));
        assert!(!text.lines().any(|line| !line.is_empty() && line.trim().is_empty()));
    }

    #[test]
    fn color_is_optional() {
        let plain = decorate(&overview(), false);
        let color = decorate(&overview(), true);
        assert!(!plain.contains('\x1b'));
        assert!(color.contains("\x1b[1;36m"));
        assert!(color.contains("\x1b[1;35mSetup:\x1b[0m"));
    }
}
