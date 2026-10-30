//! The browser host's renderer (M5): `psx-render-gl` over WebGL2 on an
//! OffscreenCanvas, exported to the Worker through wasm-bindgen. The
//! Worker (`web/worker.js`) wires `game.wasm`'s `host.render_*` and
//! `host.present` imports to it.

#[cfg(target_arch = "wasm32")]
mod web {
    use psx_render_gl::Renderer;
    use psxw_abi::render::{PresentDesc, Rect, op};
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;

    fn js(e: impl std::fmt::Display) -> JsValue {
        JsValue::from_str(&e.to_string())
    }

    fn header(o: u8, words: u32) -> u32 {
        o as u32 | words << 8
    }

    #[wasm_bindgen]
    pub struct WebRenderer {
        r: Renderer,
        canvas: web_sys::OffscreenCanvas,
    }

    #[wasm_bindgen]
    impl WebRenderer {
        /// A renderer drawing into `canvas` (WebGL2, no antialiasing, a
        /// preserved drawing buffer for screenshots).
        #[wasm_bindgen(constructor)]
        pub fn new(canvas: web_sys::OffscreenCanvas) -> Result<WebRenderer, JsValue> {
            let opts = js_sys_object(&[("antialias", false), ("preserveDrawingBuffer", true), ("depth", false)]);
            let ctx = canvas
                .get_context_with_context_options("webgl2", &opts)?
                .ok_or_else(|| js("WebGL2 is not available"))?
                .dyn_into::<web_sys::WebGl2RenderingContext>()?;
            let gl = glow::Context::from_webgl2_context(ctx);
            let r = Renderer::new(gl).map_err(js)?;
            Ok(WebRenderer { r, canvas })
        }

        /// The backend's GL description (vendor, renderer, version).
        pub fn info(&self) -> String {
            format!("{:?}", self.r.info())
        }

        /// Zero VRAM and the colour-table slots (tests).
        pub fn reset(&mut self) {
            self.r.reset_vram();
        }

        /// Execute a command stream; its READBACK pixels, in order (tests:
        /// the GPU oracle's vectors, as the render server runs them).
        pub fn run(&mut self, words: &[u32]) -> Result<Vec<u16>, JsValue> {
            self.r.execute(words).map_err(js)?;
            Ok(self.r.take_readbacks())
        }

        /// `render_submit`: execute renderer commands.
        pub fn submit(&mut self, words: &[u32]) -> Result<(), JsValue> {
            self.r.execute(words).map_err(js)
        }

        /// `render_readback`: a VRAM rectangle, `w * h` pixels.
        pub fn readback(&mut self, x: u32, y: u32, w: u32, h: u32) -> Result<Vec<u16>, JsValue> {
            let mut c = vec![header(op::READBACK, 2)];
            c.extend_from_slice(&Rect { x, y, w, h }.words());
            self.r.execute(&c).map_err(js)?;
            Ok(self.r.take_readbacks())
        }

        /// `present`: draw the display (five descriptor words) and show it.
        pub fn present(&mut self, desc: &[u32]) -> Result<(), JsValue> {
            if desc.len() < psxw_abi::render::PRESENT_WORDS {
                return Err(js("short present descriptor"));
            }
            let d = PresentDesc { flags: desc[0], x: desc[1], y: desc[2], width: desc[3], height: desc[4] };
            let mut c = vec![header(op::PRESENT, psxw_abi::render::PRESENT_WORDS as u32)];
            c.extend_from_slice(&d.to_words());
            self.r.execute(&c).map_err(js)?;
            self.r.blit_to_screen(self.canvas.width(), self.canvas.height());
            Ok(())
        }

        /// The backend's work since the last call: draw calls, syncs, rows
        /// drawn one by one, uploads, colour-table loads.
        pub fn stats(&mut self) -> Vec<u32> {
            let s = self.r.take_stats();
            vec![s.draws, s.syncs, s.serial_rows, s.uploads, s.clut_loads]
        }

        /// All of VRAM (1024 × 512), for hashes.
        pub fn vram(&mut self) -> Vec<u16> {
            self.r.read_vram(0, 0, 1024, 512)
        }

        /// Write all of VRAM (1024 × 512 pixels): a shadow copy back after
        /// the context was lost and restored.
        pub fn upload_vram(&mut self, px: &[u16]) -> Result<(), JsValue> {
            if px.len() != 1024 * 512 {
                return Err(js("upload_vram needs 1024 × 512 pixels"));
            }
            let mut c = Vec::with_capacity(4 + px.len() / 2);
            c.push(header(op::UPLOAD, 3 + px.len() as u32 / 2));
            c.extend_from_slice(&[0, 1024 | 512 << 16, 0]);
            c.extend(px.chunks_exact(2).map(|p| p[0] as u32 | (p[1] as u32) << 16));
            self.r.execute(&c).map_err(js)
        }

        /// FNV-1a (64-bit, hex) of VRAM as little-endian bytes: the native
        /// host's `vram_hashes`.
        pub fn vram_hash(&mut self) -> String {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for p in self.vram() {
                for b in p.to_le_bytes() {
                    h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
                }
            }
            format!("{h:016x}")
        }
    }

    fn js_sys_object(pairs: &[(&str, bool)]) -> JsValue {
        let o = js_sys::Object::new();
        for (k, v) in pairs {
            let _ = js_sys::Reflect::set(&o, &JsValue::from_str(k), &JsValue::from_bool(*v));
        }
        o.into()
    }
}
