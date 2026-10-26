//! `psxw run <game>`: run `out/<game>/game.wasm` headless on the user's
//! disc and write `out/run/<game>/run.json`.

use anyhow::{Result, bail};
use psxw_host_native::{Options, RunReport, Stop};
use psxw_inputs::Workspace;

pub fn options(ws: &Workspace, game: &str, args: &[&str]) -> Result<(Options, Vec<String>)> {
    let mut o = Options {
        wasm: psxw_build::game::game_path(ws, game),
        disc: ws.disc(game)?,
        frames: 600,
        storage_dir: Some(ws.out_dir().join("run").join(game).join("storage")),
        ..Default::default()
    };
    let mut checks = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().map(|s| s.to_string()).ok_or_else(|| anyhow::anyhow!("{a} needs a value"));
        match *a {
            "--headless" => {}
            "--frames" => o.frames = val()?.parse()?,
            "--replay" => o.replay = Some(std::fs::read(val()?)?),
            "--cd" => o.boot.cd_timing = if val()? == "instant" { 1 } else { 0 },
            "--hash-at" => o.hash_at = val()?.split(',').map(str::parse).collect::<Result<_, _>>()?,
            "--dump-at" => o.dump_at = val()?.split(',').map(str::parse).collect::<Result<_, _>>()?,
            "--dump-dir" => o.dump_dir = Some(val()?.into()),
            "--trace-u32" => o.trace_u32 = psxw_inputs::parse_u32(&val()?),
            "--wasm" => o.wasm = val()?.into(),
            "--check" => checks.push(val()?),
            "--verbose" => o.verbose = true,
            other => bail!("unknown run option {other}"),
        }
    }
    Ok((o, checks))
}

pub fn summary(r: &RunReport) -> String {
    let mut s = format!("stop: {:?} after {} frames\n", r.stop, r.frames);
    if let Some(b) = &r.boot {
        s += &format!("boot: {} (text matches disc: {:?})\n", b["exe"], r.boot_text_matches);
    }
    if let Some(rep) = &r.report {
        s += &format!(
            "vblanks {}, cd sectors {}, edc errors {}\n",
            rep["vblanks"],
            rep["cd_sectors"],
            rep["edc_errors"].as_array().map_or(0, Vec::len)
        );
    }
    for l in r.logs.iter().rev().take(10).collect::<Vec<_>>().into_iter().rev() {
        s += &format!("  log {l}\n");
    }
    s
}

pub fn run(ws: &Workspace, game: &str, args: &[&str]) -> Result<bool> {
    let (o, checks) = options(ws, game, args)?;
    let r = psxw_host_native::run(o)?;
    let dir = ws.out_dir().join("run").join(game);
    std::fs::create_dir_all(&dir)?;
    let json = dir.join("run.json");
    std::fs::write(&json, serde_json::to_string_pretty(&r)?)?;
    print!("{}", summary(&r));
    eprintln!("full report: {}", json.display());
    let ok = matches!(r.stop, Stop::FrameLimit | Stop::Exited);
    let _ = checks;
    Ok(ok)
}
