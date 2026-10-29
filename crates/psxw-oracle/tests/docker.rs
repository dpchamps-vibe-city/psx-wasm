//! End-to-end through the dockerized emulator. Skips (with a message) when
//! Docker or the images are not available; `psxw oracle ci` runs the full
//! selftest.

use psxw_inputs::Workspace;
use psxw_oracle::store::Store;
use psxw_oracle::target::{DockerTarget, run_vectors};
use psxw_oracle::{Vector, exact, images};

fn target() -> Option<(Workspace, DockerTarget)> {
    if !images::docker_available() {
        eprintln!("skipping: docker is not available");
        return None;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ws = Workspace::open(&root).ok()?;
    match DockerTarget::open(&ws) {
        Ok(t) => Some((ws, t)),
        Err(e) => {
            eprintln!("skipping: {e:#} (run `psxw oracle images build`)");
            None
        }
    }
}

#[test]
fn echo_and_exact_vectors_round_trip() {
    let Some((_ws, mut target)) = target() else { return };
    let dir = std::env::temp_dir().join(format!("psxw-oracle-docker-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::new(&dir);
    let generated = exact::generate(&store, 0xD0C4E2, 14).unwrap();
    let mut vectors = vec![(Vector::Echo(b"ping".to_vec()).id(), Vector::Echo(b"ping".to_vec()))];
    vectors.extend(generated.iter().map(|g| (g.vector.id(), g.vector.clone())));
    let results = run_vectors(&mut target, &store, &vectors).unwrap();
    assert_eq!(results[0].as_deref(), Ok(&b"ping"[..]));
    for (g, r) in generated.iter().zip(&results[1..]) {
        assert_eq!(r.as_ref().unwrap(), &g.expected, "{} vector differs from its closed form", g.generator);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `psxw-emu-host play` on the runner disc (the game disc is the user's):
/// a replay drives the pad, the mailbox magic is the traced word, and RAM
/// is dumped by frame and by counter value, identically on two runs.
#[test]
fn play_traces_and_dumps_deterministically() {
    use psxw_oracle::play::{self, PlayOptions};
    use psxw_oracle::wire;
    let Some((ws, _)) = target() else { return };
    let im = images::load(&ws).unwrap();
    let replay = play::replay_from_presses("runner", 600, &["START@100".into()], 0, 2).unwrap();
    let base = ws.out_dir().join("oracle").join(format!("test-play-{}", std::process::id()));
    let run = |n: u32| {
        let o = PlayOptions {
            disc: ws.root.join(&im.runner.dir).join("psxw-runner.cue"),
            replay: replay.clone(),
            frames: 600,
            trace_u32: Some(wire::MBX_ADDR + wire::MBX_OFF_MAGIC),
            dump_at: vec![550],
            dump_counter: vec![wire::MBX_MAGIC],
            frames_every: None,
            out_dir: base.join(format!("run-{n}")),
        };
        play::play(&ws, &o).unwrap()
    };
    let one = run(1);
    assert_eq!(one.run.frames, 600);
    assert_eq!(one.trace.len(), 600);
    let ready = one.trace.iter().position(|&w| w == wire::MBX_MAGIC).expect("the runner never came up") as u64 + 1;
    let files: Vec<(&str, u64, Option<u32>)> = one.run.dumps.iter().map(|d| (d.file.as_str(), d.frame, d.counter)).collect();
    let by_counter = format!("ram_c{:010}.bin", wire::MBX_MAGIC);
    assert_eq!(files, [(by_counter.as_str(), ready, Some(wire::MBX_MAGIC)), ("ram_000550.bin", 550, None)]);
    let ram = play::read_dump(&one.out_dir, "ram_000550.bin").unwrap();
    let at = (wire::MBX_ADDR & 0x1F_FFFF) as usize;
    assert_eq!(u32::from_le_bytes(ram[at..at + 4].try_into().unwrap()), wire::MBX_MAGIC);
    let two = run(2);
    assert_eq!(one.run.dumps, two.run.dumps);
    assert_eq!(one.trace, two.trace);
    let _ = std::fs::remove_dir_all(&base);
}
