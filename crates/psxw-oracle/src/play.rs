//! The whole-game RAM oracle's emulator side (plans/completed/M03-oracle-harness.md,
//! "Whole-game RAM oracle"): `psxw-emu-host play` runs the user's disc
//! under the pinned emulator from a psx-wasm replay, tracing the game's
//! frame counter and dumping RAM. The disc is mounted read-only; nothing
//! from it is kept outside the run directory under `out/`.

use crate::images::{self, Images};
use anyhow::{Context, Result, bail, ensure};
use psxw_abi::replay;
use psxw_inputs::Workspace;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;


/// A digital-pad replay from presses written `[2:]BUTTON[+BUTTON..]@FRAME[:HOLD]`
/// (port 2 with the `2:` prefix; hold defaults to 4 frames).
/// `clock`: 0 for entries by vblank, or the address of the game's frame
/// counter (psxw-abi `replay`), so `@FRAME` is that counter's value.
/// `pads`: 1 leaves port 2 unplugged, 2 (or anything else) plugs both.
pub fn replay_from_presses(game_id: &str, frames: u32, presses: &[String], clock: u32, pads: u32) -> Result<Vec<u8>, String> {
    let mut id = [0u8; 16];
    let n = game_id.len().min(15);
    id[..n].copy_from_slice(&game_id.as_bytes()[..n]);
    let unplugged = if pads == 1 { replay::UNPLUGGED_PORT2 } else { 0 };
    let h = replay::Header { version: replay::VERSION, pad_type: 0x41, frames, game_id: id, disc_sha1: [0; 20], clock, unplugged };
    let mut head = [0u8; replay::HEADER_LEN];
    h.encode(&mut head);
    let mut words = vec![[replay::NOTHING_PRESSED; 2]; frames as usize];
    for p in presses {
        let (port, rest) = match p.strip_prefix("2:") {
            Some(r) => (1, r),
            None => (0, p.as_str()),
        };
        let (buttons, when) = rest.split_once('@').ok_or_else(|| format!("{p}: expected BUTTON@FRAME"))?;
        let (at, hold) = match when.split_once(':') {
            Some((a, h)) => (a, h.parse::<u32>().map_err(|e| format!("{p}: {e}"))?),
            None => (when, 4),
        };
        let at: u32 = at.parse().map_err(|e| format!("{p}: {e}"))?;
        let mut mask = 0u16;
        for b in buttons.split('+') {
            let bit = replay::BUTTONS.iter().position(|n| n.eq_ignore_ascii_case(b)).ok_or_else(|| format!("{p}: unknown button {b}"))?;
            mask |= 1 << bit;
        }
        for f in at..at.saturating_add(hold).min(frames) {
            words[f as usize][port] &= !mask;
        }
    }
    let mut out = head.to_vec();
    for w in words {
        out.extend_from_slice(&w[0].to_le_bytes());
        out.extend_from_slice(&w[1].to_le_bytes());
    }
    Ok(out)
}


#[derive(Debug, Clone, Default)]
pub struct PlayOptions {
    /// The game's `.cue`; its directory is mounted read-only.
    pub disc: PathBuf,
    pub replay: Vec<u8>,
    pub frames: u64,
    pub trace_u32: Option<u32>,
    pub dump_at: Vec<u64>,
    pub dump_counter: Vec<u32>,
    /// Every Nth presented frame as `frames/frame_NNNNNN.ppm`.
    pub frames_every: Option<u64>,
    /// Each RAM dump also writes the picture presented in that frame
    /// beside it (`ram_X.bin` → `frame_X.ppm`).
    pub dump_frames: bool,
    /// Each RAM dump also writes VRAM beside it (`vram_X.bin`, 1024×512
    /// 16-bit words), from the core's savestate.
    pub dump_vram: bool,
    /// Receives `trace.bin`, `ram_*.bin`, `play.json` and `host.log`.
    pub out_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlayDump {
    pub file: String,
    pub frame: u64,
    #[serde(default)]
    pub counter: Option<u32>,
    /// With `dump_frames` and `dump_vram`: the files beside the dump.
    #[serde(default)]
    pub picture: Option<String>,
    #[serde(default)]
    pub vram: Option<String>,
    pub sha1: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayRun {
    pub replay_sha1: String,
    pub frames: u64,
    pub frames_presented: u64,
    pub dumps: Vec<PlayDump>,
    /// Why the host wrote no VRAM, when `dump_vram` asked for it.
    #[serde(default)]
    pub vram_unavailable: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Played {
    pub run: PlayRun,
    pub trace: Vec<u32>,
    pub out_dir: PathBuf,
}

/// Runs the emulator on the user's disc. The host must report the BIOS,
/// core and options `images.json` records.
pub fn play(ws: &Workspace, o: &PlayOptions) -> Result<Played> {
    ensure!(images::docker_available(), "docker is not available");
    let imgs: Images = images::load(ws)?;
    images::verify(ws, &imgs)?;
    let cue = o.disc.canonicalize().with_context(|| format!("disc {}", o.disc.display()))?;
    let disc_dir = cue.parent().context("disc has no directory")?.to_path_buf();
    let cue_name = cue.file_name().context("disc has no file name")?.to_string_lossy().into_owned();
    let _ = std::fs::remove_dir_all(&o.out_dir);
    std::fs::create_dir_all(&o.out_dir)?;
    let out_dir = o.out_dir.canonicalize()?;
    std::fs::write(out_dir.join("replay.rpl"), &o.replay)?;
    let mut args: Vec<String> = vec![
        "play".into(),
        "--disc".into(),
        format!("/disc/{cue_name}"),
        "--replay".into(),
        "/work/replay.rpl".into(),
        "--frames".into(),
        o.frames.to_string(),
        "--out".into(),
        "/work".into(),
        "--work".into(),
        "/work".into(),
        "--log".into(),
        "/work/host.log".into(),
    ];
    if let Some(a) = o.trace_u32 {
        args.extend(["--trace-u32".into(), format!("0x{a:08X}")]);
    }
    let list = |v: Vec<String>| v.join(",");
    if !o.dump_at.is_empty() {
        args.extend(["--dump-at".into(), list(o.dump_at.iter().map(u64::to_string).collect())]);
    }
    if !o.dump_counter.is_empty() {
        args.extend(["--dump-counter".into(), list(o.dump_counter.iter().map(u32::to_string).collect())]);
    }
    if let Some(n) = o.frames_every {
        args.extend(["--frames-every".into(), n.to_string()]);
    }
    if o.dump_frames {
        args.push("--dump-frames".into());
    }
    if o.dump_vram {
        args.push("--dump-vram".into());
    }
    let argv = images::docker_run(
        &imgs.emu.id,
        &images::owner_of(&out_dir)?,
        &[(disc_dir, "/disc", true), (out_dir.clone(), "/work", false)],
        None,
        &args,
    );
    let output = Command::new("docker").args(&argv).output().context("running docker")?;
    if !output.status.success() {
        bail!(
            "psxw-emu-host play failed ({}): {}\nhost log: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
            out_dir.join("host.log").display()
        );
    }
    let host: serde_json::Value = serde_json::from_slice(&std::fs::read(out_dir.join("play.json"))?)?;
    let facts = images::parse_facts(&host)?;
    ensure!(facts == imgs.emu_facts, "the emulator reported different BIOS, core or options than images.json records");
    let run: PlayRun = serde_json::from_value(host["run"].clone()).context("play.json run section")?;
    let trace = match std::fs::read(out_dir.join("trace.bin")) {
        Ok(b) => b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
        Err(_) => Vec::new(),
    };
    Ok(Played { run, trace, out_dir })
}

/// A dump's bytes (2 MiB of RAM).
pub fn read_dump(dir: &Path, file: &str) -> Result<Vec<u8>> {
    let b = std::fs::read(dir.join(file)).with_context(|| format!("{}", dir.join(file).display()))?;
    ensure!(b.len() == 0x20_0000, "{file}: {} bytes, expected 2 MiB", b.len());
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presses() {
        let r = replay_from_presses("game", 10, &["START@2:2".into(), "2:cross+up@9".into()], 0, 2).unwrap();
        assert_eq!(replay::Header::parse(&r).unwrap().frames, 10);
        assert_eq!(replay::frame(&r, 1), [replay::NOTHING_PRESSED; 2]);
        assert_eq!(replay::frame(&r, 2), [!(1 << 3), replay::NOTHING_PRESSED]);
        assert_eq!(replay::frame(&r, 3), [!(1 << 3), replay::NOTHING_PRESSED]);
        assert_eq!(replay::frame(&r, 4), [replay::NOTHING_PRESSED; 2]);
        assert_eq!(replay::frame(&r, 9), [replay::NOTHING_PRESSED, !(1 << 14 | 1 << 4)]);
        assert!(replay_from_presses("game", 10, &["JUMP@1".into()], 0, 2).is_err());
    }
}
