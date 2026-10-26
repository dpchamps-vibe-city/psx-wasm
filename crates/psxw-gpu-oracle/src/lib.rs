//! psx-wasm's GPU as a device under test for `psxw oracle check --device
//! gpu` (plans/completed/M04-pixels.md, tasks 5–6).
//!
//! A vector is replayed exactly as the runner executes it on the device
//! target (`psxw_oracle::script` compiles it into the same ops): GP1
//! resets, CPU→VRAM uploads, GP0(01h), the state words and the GP0
//! packets go through the front end (`psx_devices::gpu`); its renderer
//! command stream, with a READBACK wherever the runner reads VRAM, runs in
//! the backend (`psx-render-gl`) on Mesa llvmpipe inside the pinned
//! `psxw-render-gl` image ([`image`]). The READBACK pixels are the result,
//! in the same layout as the target's.

pub mod image;
pub mod record;

use anyhow::{Context, Result, bail, ensure};
use psx_devices::gpu::Gpu;
use psx_render_gl::server;
use psxw_abi::render::{self, header, op};
use psxw_inputs::Workspace;
use psxw_oracle::commands::DeviceUnderTest;
use psxw_oracle::script;
use psxw_oracle::store::Store;
use psxw_oracle::vector::Vector;
use psxw_oracle::wire;
use std::cell::RefCell;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn u16le(b: &[u8], at: usize) -> u32 {
    u16::from_le_bytes([b[at], b[at + 1]]) as u32
}

fn u32le(b: &[u8], at: usize) -> Result<u32> {
    ensure!(b.len() >= at + 4, "op payload truncated");
    Ok(u32::from_le_bytes(b[at..at + 4].try_into()?))
}

/// The renderer command stream for a vector, as the runner would drive
/// the GPU: front end decode, plus a READBACK for each VRAM read.
pub fn replay(v: &Vector, store: &Store) -> Result<Vec<u32>> {
    let ops = script::compile(v, store)?;
    let mut gpu = Gpu::new();
    let mut stream = Vec::new();
    for o in &ops {
        let p = &o.payload;
        match o.code {
            wire::OP_GPU_RESET => {
                gpu.gp1(0x0000_0000);
                gpu.gp1(0x0100_0000);
                gpu.gp1(0x0400_0000);
            }
            wire::OP_VRAM_WRITE => {
                ensure!(p.len() >= 8, "VRAM_WRITE payload");
                let (x, y, w, h) = (u16le(p, 0), u16le(p, 2), u16le(p, 4), u16le(p, 6));
                let n = (w * h) as usize;
                ensure!(p.len() >= 8 + 2 * n, "VRAM_WRITE pixels");
                gpu.gp0_words(&[0xA000_0000, y << 16 | x, h << 16 | w]);
                for i in 0..n.div_ceil(2) {
                    let lo = u16le(p, 8 + 4 * i);
                    let hi = if 2 * i + 1 < n { u16le(p, 10 + 4 * i) } else { 0 };
                    gpu.gp0(lo | hi << 16);
                }
            }
            wire::OP_GPU_EXEC => {
                let n_state = u32le(p, 4)? as usize;
                let mut at = 8;
                gpu.gp0(0x0100_0000); // the runner clears the texture cache first
                for _ in 0..n_state {
                    gpu.gp0(u32le(p, at)?);
                    at += 4;
                }
                let n_packets = u32le(p, at)? as usize;
                at += 4;
                for _ in 0..n_packets {
                    let n = u32le(p, at)? as usize;
                    at += 4;
                    for _ in 0..n {
                        gpu.gp0(u32le(p, at)?);
                        at += 4;
                    }
                }
            }
            wire::OP_VRAM_READ => {
                ensure!(p.len() >= 8, "VRAM_READ payload");
                let (x, y, w, h) = (u16le(p, 0), u16le(p, 2), u16le(p, 4), u16le(p, 6));
                gpu.gp0_words(&[0xC000_0000, y << 16 | x, h << 16 | w]);
                let r = gpu.pending_readback().context("GP0(C0h) started no VRAM read")?;
                stream.extend(gpu.take_commands());
                let rect = render::Rect { x: r.x, y: r.y, w: r.w, h: r.h };
                stream.push(header(op::READBACK, 2));
                stream.extend_from_slice(&rect.words());
                gpu.finish_readback(None);
            }
            other => bail!("op {other:#x} is not a GPU op"),
        }
    }
    stream.extend(gpu.take_commands());
    Ok(stream)
}

/// The render server in its container.
struct Server {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<ChildStdout>,
    info: String,
}

impl Server {
    fn start(ws: &Workspace) -> Result<Server> {
        let rec = image::ensure(ws)?;
        let bin = image::build_server(ws, &rec)?;
        let user = psxw_oracle::images::owner_of(&image::work_dir(ws))?;
        let log_dir = ws.out_dir().join("oracle").join("runs");
        std::fs::create_dir_all(&log_dir)?;
        let stderr = std::fs::File::create(log_dir.join("render-server.log"))?;
        let mut child = Command::new("docker")
            .args(["run", "--rm", "-i", "--network=none", "--user", &user, "-v"])
            .arg(format!("{}:/opt/server:ro", bin.display()))
            .arg(&rec.id)
            .arg("/opt/server/psxw-render-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .context("starting the render server container")?;
        let input = BufWriter::new(child.stdin.take().context("server stdin")?);
        let output = BufReader::new(child.stdout.take().context("server stdout")?);
        let mut s = Server { child, input, output, info: String::new() };
        let info = s.call(server::INFO, &[]).context("render server did not answer; see out/oracle/runs/render-server.log")?;
        let info: serde_json::Value = serde_json::from_slice(&info)?;
        let field = |k: &str| info.get(k).and_then(|v| v.as_str()).unwrap_or("?").to_string();
        s.info = format!(
            "{} + psx-devices gpu front end, on {} ({}) in {} {}",
            field("backend"),
            field("gl_renderer"),
            field("gl_version"),
            image::IMAGE,
            &rec.id[..rec.id.len().min(19)]
        );
        Ok(s)
    }

    fn call(&mut self, kind: u32, words: &[u32]) -> Result<Vec<u8>> {
        server::write_request(&mut self.input, kind, words)?;
        server::read_response(&mut self.output)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = server::write_request(&mut self.input, server::QUIT, &[]);
        let _ = self.child.wait();
    }
}

/// The GPU device under test. The image, server build and container
/// start lazily, on the first vector.
pub struct GpuDevice {
    ws_root: PathBuf,
    server: RefCell<Option<Result<Server, String>>>,
}

impl GpuDevice {
    pub fn new(ws: &Workspace) -> Self {
        GpuDevice { ws_root: ws.root.clone(), server: RefCell::new(None) }
    }

    fn ensure_server(&self) {
        let mut s = self.server.borrow_mut();
        if s.is_none() {
            *s = Some(Workspace::open(&self.ws_root).and_then(|ws| Server::start(&ws)).map_err(|e| format!("{e:#}")));
        }
    }

    /// One request to the render server, starting it if needed.
    fn call(&self, kind: u32, words: &[u32]) -> Result<Vec<u8>> {
        self.ensure_server();
        let mut guard = self.server.borrow_mut();
        match guard.as_mut() {
            Some(Ok(s)) => s.call(kind, words),
            Some(Err(e)) => bail!("render server unavailable: {e}"),
            None => bail!("render server not started"),
        }
    }

    /// Zero the backend's VRAM.
    pub fn reset(&mut self) -> Result<()> {
        self.call(server::RESET, &[]).map(|_| ())
    }

    /// Execute a renderer command stream; returns its READBACK pixels.
    pub fn execute(&mut self, stream: &[u32]) -> Result<Vec<u16>> {
        let bytes = self.call(server::RUN, stream)?;
        Ok(bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect())
    }

    /// The last presented frame: width, height, RGBA8 rows from the top.
    pub fn frame(&mut self) -> Result<(u32, u32, Vec<u8>)> {
        let bytes = self.call(server::FRAME, &[])?;
        ensure!(bytes.len() >= 8, "short frame");
        let w = u32::from_le_bytes(bytes[..4].try_into()?);
        let h = u32::from_le_bytes(bytes[4..8].try_into()?);
        ensure!(bytes.len() == 8 + (w * h * 4) as usize, "frame of {} bytes for {w}x{h}", bytes.len());
        Ok((w, h, bytes[8..].to_vec()))
    }
}

impl DeviceUnderTest for GpuDevice {
    fn device(&self) -> &str {
        "gpu"
    }

    fn name(&self) -> String {
        self.ensure_server();
        match self.server.borrow().as_ref() {
            Some(Ok(s)) => s.info.clone(),
            Some(Err(e)) => format!("{} (render server unavailable: {e})", psx_render_gl::BACKEND_VERSION),
            None => psx_render_gl::BACKEND_VERSION.to_string(),
        }
    }

    fn run(&mut self, v: &Vector, store: &Store) -> Result<Vec<u8>> {
        let stream = replay(v, store)?;
        self.call(server::RESET, &[])?;
        let bytes = self.call(server::RUN, &stream)?;
        ensure!(bytes.len() == v.result_len(), "{} result bytes, the vector has {}", bytes.len(), v.result_len());
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psxw_oracle::vector::{GpuDraw, GpuMode, Rect, VramRect, pixels_to_bytes};

    #[test]
    fn replay_mirrors_the_runner_ops() {
        let dir = std::env::temp_dir().join(format!("psxw-gpu-oracle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);
        let px: Vec<u16> = (0..12).collect();
        let blob = store.put_blob(&pixels_to_bytes(&px)).unwrap();
        let v = Vector::GpuDraw(GpuDraw {
            init: vec![VramRect { rect: Rect::new(10, 20, 4, 3), pixels: blob }],
            state: vec![0xE300_0000, 0xE400_0000 | 511 << 10 | 1023],
            mode: GpuMode::List,
            packets: vec![vec![0x6000_00FF, 20 << 16 | 10, 1 << 16 | 2]],
            readback: vec![Rect::new(10, 20, 4, 3)],
        });
        let stream = replay(&v, &store).unwrap();
        let cmds: Vec<_> = render::commands(&stream).collect::<Result<_, _>>().unwrap();
        assert!(matches!(cmds[0], render::Command::Upload { rect, .. } if rect.w == 4 && rect.h == 3));
        assert!(matches!(cmds.last(), Some(render::Command::Readback(r)) if r.x == 10 && r.y == 20 && r.w == 4 && r.h == 3));
        assert!(cmds.iter().any(|c| matches!(c, render::Command::Draw(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
