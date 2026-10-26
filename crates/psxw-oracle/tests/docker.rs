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
