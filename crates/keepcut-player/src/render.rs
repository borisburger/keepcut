//! libmpv render API (raw FFI over libmpv2-sys): the OpenGL path renders into an
//! app-owned framebuffer/texture shown by egui; the software path renders into a
//! CPU buffer on its own thread (fallback for drivers where GL interop misbehaves).

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use anyhow::{Result, bail};
use glow::{self, HasContext};
use libmpv2_sys as sys;

/// Shared "mpv has a new frame" flag set from mpv's update callback.
#[derive(Default)]
pub struct UpdateSignal {
    dirty: AtomicBool,
    lock: Mutex<()>,
    cvar: Condvar,
}

impl UpdateSignal {
    fn notify(&self) {
        self.dirty.store(true, Ordering::SeqCst);
        let _g = self.lock.lock().unwrap();
        self.cvar.notify_all();
    }
    pub fn take(&self) -> bool {
        self.dirty.swap(false, Ordering::SeqCst)
    }
    /// Waits until notified or the timeout passes (render thread).
    fn wait(&self, timeout: std::time::Duration) {
        let g = self.lock.lock().unwrap();
        if !self.dirty.load(Ordering::SeqCst) {
            let _ = self.cvar.wait_timeout(g, timeout);
        }
    }
}

struct CallbackData {
    signal: Arc<UpdateSignal>,
    egui_ctx: egui::Context,
}

/// Called by mpv from one of its threads: only flag and wake, never render here.
unsafe extern "C" fn on_update(data: *mut c_void) {
    let d = unsafe { &*(data as *const CallbackData) };
    d.signal.notify();
    d.egui_ctx.request_repaint();
}

type GetProc = Arc<dyn Fn(&CStr) -> *const c_void + Send + Sync>;

unsafe extern "C" fn get_proc_address(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    let f = unsafe { &*(ctx as *const GetProc) };
    f(unsafe { CStr::from_ptr(name) }) as *mut c_void
}

fn param(t: sys::mpv_render_param_type, data: *mut c_void) -> sys::mpv_render_param {
    sys::mpv_render_param { type_: t, data }
}

const END: sys::mpv_render_param = sys::mpv_render_param {
    type_: 0,
    data: ptr::null_mut(),
};

/// An `mpv_render_context` plus the boxed data its callbacks point to.
struct RawContext {
    ctx: *mut sys::mpv_render_context,
    _callback: Box<CallbackData>,
    _get_proc: Option<Box<GetProc>>,
}

// The render context may be used from one thread at a time; we guarantee that
// by owning it either on the UI thread (GL) or on the render thread (SW).
unsafe impl Send for RawContext {}

impl Drop for RawContext {
    fn drop(&mut self) {
        unsafe { sys::mpv_render_context_free(self.ctx) };
    }
}

impl RawContext {
    fn create(
        mpv: &libmpv2::Mpv,
        api: &'static [u8],
        get_proc: Option<GetProc>,
        signal: Arc<UpdateSignal>,
        egui_ctx: egui::Context,
    ) -> Result<RawContext> {
        let mut get_proc_box = get_proc.map(Box::new);
        let mut init = sys::mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address),
            get_proc_address_ctx: get_proc_box.as_mut().map_or(ptr::null_mut(), |b| {
                (&mut **b) as *mut GetProc as *mut c_void
            }),
        };
        let mut params = vec![param(
            sys::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
            api.as_ptr() as *mut c_void,
        )];
        if get_proc_box.is_some() {
            params.push(param(
                sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                &mut init as *mut _ as *mut c_void,
            ));
        }
        params.push(END);
        let mut ctx = ptr::null_mut();
        let err = unsafe {
            sys::mpv_render_context_create(&mut ctx, mpv.ctx.as_ptr(), params.as_mut_ptr())
        };
        if err < 0 {
            bail!("mpv_render_context_create failed: {}", mpv_error(err));
        }
        let mut cb = Box::new(CallbackData { signal, egui_ctx });
        unsafe {
            sys::mpv_render_context_set_update_callback(
                ctx,
                Some(on_update),
                (&mut *cb) as *mut CallbackData as *mut c_void,
            )
        };
        Ok(RawContext {
            ctx,
            _callback: cb,
            _get_proc: get_proc_box,
        })
    }

    /// True if a new frame should be rendered.
    fn wants_frame(&self) -> bool {
        let flags = unsafe { sys::mpv_render_context_update(self.ctx) };
        flags & sys::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64 != 0
    }
}

pub fn mpv_error(code: c_int) -> String {
    unsafe { CStr::from_ptr(sys::mpv_error_string(code)) }
        .to_string_lossy()
        .into_owned()
}

// ---------------------------------------------------------------------------
// OpenGL path
// ---------------------------------------------------------------------------

pub struct GlRenderer {
    raw: RawContext,
    signal: Arc<UpdateSignal>,
    gl: Arc<glow::Context>,
    fbo: glow::Framebuffer,
    tex: glow::Texture,
    size: [u32; 2],
    texture_id: Option<egui::TextureId>,
    /// Time spent in the last `mpv_render_context_render` (ms).
    pub last_render_ms: f64,
    /// Set when a new frame was rendered; the caller resets it.
    pub rendered: bool,
}

impl GlRenderer {
    pub fn new(
        mpv: &libmpv2::Mpv,
        gl: Arc<glow::Context>,
        get_proc: GetProc,
        egui_ctx: egui::Context,
    ) -> Result<GlRenderer> {
        let signal = Arc::new(UpdateSignal::default());
        let raw = RawContext::create(
            mpv,
            sys::MPV_RENDER_API_TYPE_OPENGL,
            Some(get_proc),
            signal.clone(),
            egui_ctx,
        )?;
        let (fbo, tex) = unsafe {
            let tex = gl.create_texture().map_err(anyhow::Error::msg)?;
            let fbo = gl.create_framebuffer().map_err(anyhow::Error::msg)?;
            (fbo, tex)
        };
        let mut r = GlRenderer {
            raw,
            signal,
            gl,
            fbo,
            tex,
            size: [0, 0],
            texture_id: None,
            last_render_ms: 0.0,
            rendered: false,
        };
        r.resize([16, 16]);
        Ok(r)
    }

    /// (Re)allocates the texture storage; true if the size changed. The texture object stays
    /// the same, so the egui `TextureId` registered for it remains valid.
    fn resize(&mut self, size: [u32; 2]) -> bool {
        if size == self.size {
            return false;
        }
        let gl = &self.gl;
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                size[0] as i32,
                size[1] as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(self.tex),
                0,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
        self.size = size;
        true
    }

    /// Renders a new frame if mpv has one, or redraws the current one if the size changed
    /// (the reallocated texture is blank, and a paused video sends no new frame). Call from
    /// the UI thread, where the GL context is current. Returns the texture to show.
    pub fn frame(&mut self, frame: &mut eframe::Frame, size: [u32; 2]) -> egui::TextureId {
        let size = [size[0].max(16), size[1].max(16)];
        let resized = self.resize(size);
        let id = *self
            .texture_id
            .get_or_insert_with(|| frame.register_native_glow_texture(self.tex));
        let new_frame = self.signal.take() && self.raw.wants_frame();
        if resized || new_frame {
            let t0 = std::time::Instant::now();
            let mut fbo = sys::mpv_opengl_fbo {
                fbo: self.fbo.0.get() as c_int,
                w: size[0] as c_int,
                h: size[1] as c_int,
                internal_format: 0,
            };
            // no flip: with egui's texture coordinates the unflipped FBO is upright
            // (FLIP_Y=1 showed the video upside down)
            let mut flip: c_int = 0;
            let mut params = [
                param(
                    sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
                    &mut fbo as *mut _ as *mut c_void,
                ),
                param(
                    sys::mpv_render_param_type_MPV_RENDER_PARAM_FLIP_Y,
                    &mut flip as *mut _ as *mut c_void,
                ),
                END,
            ];
            unsafe {
                sys::mpv_render_context_render(self.raw.ctx, params.as_mut_ptr());
                // restore the state egui expects
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                self.gl.viewport(0, 0, size[0] as i32, size[1] as i32);
            }
            self.last_render_ms = t0.elapsed().as_secs_f64() * 1000.0;
            self.rendered = true;
        }
        id
    }

    /// Tells mpv a frame was presented (improves its frame timing).
    pub fn report_swap(&self) {
        unsafe { sys::mpv_render_context_report_swap(self.raw.ctx) };
    }
}

impl Drop for GlRenderer {
    fn drop(&mut self) {
        // the texture belongs to egui once registered; the FBO is ours
        unsafe { self.gl.delete_framebuffer(self.fbo) };
    }
}

// ---------------------------------------------------------------------------
// Software path
// ---------------------------------------------------------------------------

pub struct SwFrame {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

pub struct SwRenderer {
    stop: Arc<AtomicBool>,
    /// Wanted target size in physical pixels (set by the UI).
    target: Arc<Mutex<[u32; 2]>>,
    latest: Arc<Mutex<Option<SwFrame>>>,
    signal: Arc<UpdateSignal>,
    thread: Option<std::thread::JoinHandle<()>>,
    texture: Option<egui::TextureHandle>,
    pub last_render_ms: Arc<Mutex<f64>>,
    /// Set when a new frame was uploaded; the caller resets it.
    pub uploaded: bool,
}

impl SwRenderer {
    pub fn new(mpv: &libmpv2::Mpv, egui_ctx: egui::Context) -> Result<SwRenderer> {
        let signal = Arc::new(UpdateSignal::default());
        let raw = RawContext::create(
            mpv,
            sys::MPV_RENDER_API_TYPE_SW,
            None,
            signal.clone(),
            egui_ctx.clone(),
        )?;
        let stop = Arc::new(AtomicBool::new(false));
        let target = Arc::new(Mutex::new([640u32, 360]));
        let latest: Arc<Mutex<Option<SwFrame>>> = Arc::new(Mutex::new(None));
        let last_ms = Arc::new(Mutex::new(0.0));
        let thread = {
            let (stop, target, latest, signal, last_ms, repaint) = (
                stop.clone(),
                target.clone(),
                latest.clone(),
                signal.clone(),
                last_ms.clone(),
                egui_ctx.clone(),
            );
            std::thread::Builder::new()
                .name("mpv-sw-render".into())
                .spawn(move || {
                    let raw = raw; // owned by this thread from now on
                    let mut buf: Vec<u8> = Vec::new();
                    let mut last_size = [0u32; 2];
                    while !stop.load(Ordering::SeqCst) {
                        signal.wait(std::time::Duration::from_millis(100));
                        let size = *target.lock().unwrap();
                        let resized = size != last_size;
                        if !(signal.take() && raw.wants_frame()) && !resized {
                            continue;
                        }
                        last_size = size;
                        let t0 = std::time::Instant::now();
                        let (w, h) = (size[0] as usize, size[1] as usize);
                        let stride = w * 4;
                        buf.resize(stride * h, 0);
                        let mut sz: [c_int; 2] = [w as c_int, h as c_int];
                        let mut stride_v: usize = stride;
                        let fmt = b"rgb0\0";
                        let mut params = [
                            param(
                                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_SIZE,
                                sz.as_mut_ptr() as *mut c_void,
                            ),
                            param(
                                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_FORMAT,
                                fmt.as_ptr() as *mut c_void,
                            ),
                            param(
                                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_STRIDE,
                                &mut stride_v as *mut usize as *mut c_void,
                            ),
                            param(
                                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_POINTER,
                                buf.as_mut_ptr() as *mut c_void,
                            ),
                            END,
                        ];
                        let err =
                            unsafe { sys::mpv_render_context_render(raw.ctx, params.as_mut_ptr()) };
                        if err < 0 {
                            tracing::warn!("sw render failed: {}", mpv_error(err));
                            continue;
                        }
                        // rgb0 → RGBA with opaque alpha
                        let mut rgba = buf.clone();
                        for px in rgba.as_chunks_mut::<4>().0 {
                            px[3] = 255;
                        }
                        *last_ms.lock().unwrap() = t0.elapsed().as_secs_f64() * 1000.0;
                        *latest.lock().unwrap() = Some(SwFrame { size: [w, h], rgba });
                        repaint.request_repaint();
                    }
                    drop(raw);
                })?
        };
        Ok(SwRenderer {
            stop,
            target,
            latest,
            signal,
            thread: Some(thread),
            texture: None,
            last_render_ms: last_ms,
            uploaded: false,
        })
    }

    /// Uploads the newest CPU frame (if any) and returns the texture to show.
    pub fn frame(&mut self, ctx: &egui::Context, size: [u32; 2]) -> Option<egui::TextureId> {
        *self.target.lock().unwrap() = [size[0].max(16), size[1].max(16)];
        self.signal.notify_render_thread();
        if let Some(f) = self.latest.lock().unwrap().take() {
            let img = egui::ColorImage::from_rgba_unmultiplied(f.size, &f.rgba);
            match &mut self.texture {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("mpv-sw", img, egui::TextureOptions::LINEAR))
                }
            }
            self.uploaded = true;
        }
        self.texture.as_ref().map(|t| t.id())
    }
}

impl UpdateSignal {
    /// Wakes the SW render thread to check for a size change (without marking a frame).
    fn notify_render_thread(&self) {
        let _g = self.lock.lock().unwrap();
        self.cvar.notify_all();
    }
}

impl Drop for SwRenderer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.signal.notify_render_thread();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
