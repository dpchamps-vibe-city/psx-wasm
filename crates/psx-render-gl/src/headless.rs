//! A native headless GLES 3.0 context through EGL surfaceless
//! (`EGL_MESA_platform_surfaceless`), as Mesa provides it. In CI and in
//! the oracle this is Mesa's llvmpipe inside the pinned
//! `psxw-render-gl` Docker image; `libEGL.so.1` is loaded at run time.

use anyhow::{Context as _, Result, anyhow, bail};
use khronos_egl as egl;

/// `EGL_PLATFORM_SURFACELESS_MESA`.
const PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;

/// The EGL display and context; current on the creating thread while it
/// lives.
pub struct Headless {
    egl: egl::DynamicInstance<egl::EGL1_5>,
    display: egl::Display,
    context: egl::Context,
}

impl Headless {
    /// Create the context and make it current; returns it with a `glow`
    /// context over it.
    pub fn new() -> Result<(Headless, glow::Context)> {
        let egl = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() }
            .map_err(|e| anyhow!("loading libEGL.so.1 (EGL 1.5): {e}"))?;
        let display = unsafe { egl.get_platform_display(PLATFORM_SURFACELESS_MESA, egl::DEFAULT_DISPLAY, &[egl::ATTRIB_NONE]) }
            .context("eglGetPlatformDisplay(surfaceless)")?;
        egl.initialize(display).context("eglInitialize")?;
        let exts = egl.query_string(Some(display), egl::EXTENSIONS).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if !exts.split(' ').any(|e| e == "EGL_KHR_surfaceless_context") {
            bail!("EGL_KHR_surfaceless_context missing (extensions: {exts})");
        }
        egl.bind_api(egl::OPENGL_ES_API).context("eglBindAPI(OpenGL ES)")?;
        // SURFACE_TYPE defaults to WINDOW_BIT, which surfaceless has none of.
        let config = egl
            .choose_first_config(display, &[egl::RENDERABLE_TYPE, egl::OPENGL_ES3_BIT, egl::SURFACE_TYPE, 0, egl::NONE])
            .context("eglChooseConfig")?
            .context("no EGL config renders OpenGL ES 3")?;
        let context = egl
            .create_context(display, config, None, &[egl::CONTEXT_MAJOR_VERSION, 3, egl::CONTEXT_MINOR_VERSION, 0, egl::NONE])
            .context("eglCreateContext(ES 3.0)")?;
        egl.make_current(display, None, None, Some(context)).context("eglMakeCurrent(surfaceless)")?;
        let gl = unsafe {
            glow::Context::from_loader_function(|name| egl.get_proc_address(name).map_or(std::ptr::null(), |p| p as *const _))
        };
        Ok((Headless { egl, display, context }, gl))
    }

    /// The EGL vendor and version strings.
    pub fn egl_info(&self) -> String {
        let q = |what| self.egl.query_string(Some(self.display), what).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        format!("{} {}", q(egl::VENDOR), q(egl::VERSION))
    }
}

impl Drop for Headless {
    fn drop(&mut self) {
        let _ = self.egl.make_current(self.display, None, None, None);
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}
