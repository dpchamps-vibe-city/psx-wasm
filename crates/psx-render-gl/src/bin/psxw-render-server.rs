//! `psxw-render-server`: runs the GPU backend on a headless EGL context
//! and executes command streams from stdin ([`psx_render_gl::server`]).
//! The oracle runs it inside the pinned `psxw-render-gl` image (Mesa
//! llvmpipe).
//!
//! ```text
//! psxw-render-server            serve requests on stdin/stdout
//! psxw-render-server --info     print the GL implementation and exit
//! ```

use anyhow::Result;
use psx_render_gl::server::{self, INFO, QUIT, RESET, RUN, STATUS_ERROR, STATUS_OK};
use psx_render_gl::{BACKEND_VERSION, Renderer, headless::Headless};
use std::io::{BufReader, BufWriter};

fn info_json(r: &Renderer, h: &Headless) -> String {
    let i = r.info();
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "{{\"backend\": \"{}\", \"gl_vendor\": \"{}\", \"gl_renderer\": \"{}\", \"gl_version\": \"{}\", \"glsl_version\": \"{}\", \"egl\": \"{}\"}}",
        esc(BACKEND_VERSION),
        esc(&i.vendor),
        esc(&i.renderer),
        esc(&i.version),
        esc(&i.glsl),
        esc(&h.egl_info())
    )
}

fn main() -> Result<()> {
    let (headless, gl) = Headless::new()?;
    let mut r = Renderer::new(gl)?;
    if std::env::args().any(|a| a == "--info") {
        println!("{}", info_json(&r, &headless));
        return Ok(());
    }
    let mut input = BufReader::new(std::io::stdin().lock());
    let mut output = BufWriter::new(std::io::stdout().lock());
    while let Some((kind, words)) = server::read_request(&mut input)? {
        match kind {
            RUN => match r.execute(&words) {
                Ok(()) => {
                    let px = r.take_readbacks();
                    let bytes: Vec<u8> = px.iter().flat_map(|p| p.to_le_bytes()).collect();
                    server::write_response(&mut output, STATUS_OK, &bytes)?;
                }
                Err(e) => {
                    r.take_readbacks();
                    server::write_response(&mut output, STATUS_ERROR, format!("{e:#}").as_bytes())?;
                }
            },
            RESET => {
                r.reset_vram();
                server::write_response(&mut output, STATUS_OK, &[])?;
            }
            INFO => server::write_response(&mut output, STATUS_OK, info_json(&r, &headless).as_bytes())?,
            QUIT => break,
            other => server::write_response(&mut output, STATUS_ERROR, format!("unknown request {other}").as_bytes())?,
        }
    }
    drop(r);
    drop(headless);
    Ok(())
}
