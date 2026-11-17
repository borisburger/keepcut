//! Phase 0B player spike: libmpv rendered inside eframe/egui (glow renderer).
//! In release builds this is a GUI app (no console window); stdout still works when
//! redirected, which `--bench` uses.
//!
//! Usage: keepcut-player [FILE] [--sw] [--bench] [--project PROJECT.json]
//!   --sw       software render fallback instead of the OpenGL path
//!   --bench    run the scripted performance gate (§7) on FILE, print JSON, exit
//!   --project  cut-preview all segments of a keepcut-cli project file (EDL)
//!   --step-mode auto|mpv|seek|fast  how frame steps are done (see StepMode; default auto)
//!   --screenshot PATH  save a PNG of the app window 3 s after the file loaded, then exit
//!   (F12 saves a screenshot into the temp folder at any time)
//!   --hwdec M  override mpv's hwdec option (default auto-safe; auto-copy-safe with --sw)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use keepcut_player::{player, render};

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui;
use keepcut_core::ffmpeg::Tools;
use keepcut_core::probe::{self, FrameIndex};
use player::{Player, PlayerEvent, RenderKind};
use render::{GlRenderer, SwRenderer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepMode {
    /// forward: `frame-step` at speed 32; backward: exact seek to the previous frame
    /// from the index (best combination in the 0B measurements)
    Auto,
    /// mpv `frame-step` / `frame-back-step`
    Mpv,
    /// exact seek to the neighbouring frame from the frame index
    Seek,
    /// `frame-step` at speed 32 (forward only)
    Fast,
}

struct Args {
    step_mode: StepMode,
    file: Option<PathBuf>,
    sw: bool,
    bench: bool,
    project: Option<PathBuf>,
    hwdec: Option<String>,
    screenshot: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut a = Args {
        step_mode: StepMode::Auto,
        file: None,
        sw: false,
        bench: false,
        project: None,
        hwdec: None,
        screenshot: None,
    };
    let mut it = std::env::args_os().skip(1);
    while let Some(x) = it.next() {
        match x.to_str() {
            Some("--sw") => a.sw = true,
            Some("--bench") => a.bench = true,
            Some("--project") => a.project = it.next().map(PathBuf::from),
            Some("--step-mode") => {
                a.step_mode = match it.next().and_then(|s| s.into_string().ok()).as_deref() {
                    Some("seek") => StepMode::Seek,
                    Some("fast") => StepMode::Fast,
                    Some("mpv") => StepMode::Mpv,
                    _ => StepMode::Auto,
                }
            }
            Some("--screenshot") => a.screenshot = it.next().map(PathBuf::from),
            Some("--hwdec") => a.hwdec = it.next().and_then(|s| s.into_string().ok()),
            Some(s) if s.starts_with("--") => eprintln!("unknown option {s}"),
            _ => a.file = Some(PathBuf::from(x)),
        }
    }
    a
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = parse_args();
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_title("Keepcut player spike"),
        ..Default::default()
    };
    eframe::run_native(
        "keepcut-player",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, args)?))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

enum Renderer {
    Gl(GlRenderer),
    Sw(SwRenderer),
}

/// A latency measurement in progress (seek, frame step, …).
struct Op {
    kind: &'static str,
    start: Instant,
    from_time: f64,
    /// For seeks: playback-restart seen. For steps: time-pos changed.
    triggered: bool,
    /// A frame was rendered after the op started (mpv may render the new frame
    /// before or after its playback-restart event arrives here).
    rendered: bool,
}

struct App {
    // Declared before `player`: the render context must be freed before the mpv
    // handle (normally already done in `on_exit`, with the GL context current).
    renderer: Option<Renderer>,
    player: Player,
    step_mode: StepMode,
    kind: RenderKind,
    path: Option<PathBuf>,
    index: Option<FrameIndex>,
    index_rx: Option<Receiver<Result<FrameIndex, String>>>,
    segments_text: String,
    project: Option<PathBuf>,
    cut_preview: bool,
    op: Option<Op>,
    results: Vec<(String, f64)>,
    status: String,
    // render statistics
    frames_rendered: u64,
    ui_frames: u64,
    cpu: Cpu,
    bench: Option<Bench>,
    /// Pending `--screenshot` path and when the file finished loading.
    auto_shot: Option<(PathBuf, Option<Instant>)>,
    shot_counter: u32,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, args: Args) -> Result<App> {
        let mut kind = if args.sw {
            RenderKind::Sw
        } else {
            RenderKind::Gl
        };
        let mut player = Player::new(kind, args.hwdec.as_deref(), cc.egui_ctx.clone())?;
        let renderer = match (kind, cc.gl.clone(), cc.get_proc_address.clone()) {
            (RenderKind::Gl, Some(gl), Some(gp)) => {
                match GlRenderer::new(&player.mpv, gl, gp, cc.egui_ctx.clone()) {
                    Ok(r) => Renderer::Gl(r),
                    Err(e) => {
                        // automatic fallback (§7): GL render context creation failed
                        tracing::warn!("GL render path failed ({e}); using the software path");
                        kind = RenderKind::Sw;
                        drop(player);
                        player = Player::new(kind, args.hwdec.as_deref(), cc.egui_ctx.clone())?;
                        Renderer::Sw(SwRenderer::new(&player.mpv, cc.egui_ctx.clone())?)
                    }
                }
            }
            _ => {
                kind = RenderKind::Sw;
                Renderer::Sw(SwRenderer::new(&player.mpv, cc.egui_ctx.clone())?)
            }
        };
        let mut app = App {
            renderer: Some(renderer),
            player,
            step_mode: args.step_mode,
            kind,
            path: None,
            index: None,
            index_rx: None,
            segments_text: String::new(),
            project: args.project.clone(),
            cut_preview: false,
            op: None,
            results: Vec::new(),
            status: String::new(),
            frames_rendered: 0,
            ui_frames: 0,
            cpu: Cpu::new(),
            bench: None,
            auto_shot: args.screenshot.clone().map(|p| (p, None)),
            shot_counter: 0,
        };
        if let Some(f) = args.file {
            app.open(f);
        }
        if args.bench {
            app.bench = Some(Bench::new());
        }
        if let Some(p) = args.project {
            app.preview_project(&p);
        }
        Ok(app)
    }

    fn open(&mut self, path: PathBuf) {
        self.player.load(&path.to_string_lossy());
        self.index = None;
        self.cut_preview = false;
        let (tx, rx) = channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let r = Tools::locate().map_err(|e| e.to_string()).and_then(|t| {
                probe::probe(&t, &p, None, None)
                    .map(|m| m.frames)
                    .map_err(|e| e.to_string())
            });
            let _ = tx.send(r);
        });
        self.index_rx = Some(rx);
        self.segments_text.clear();
        self.path = Some(path);
    }

    fn preview_project(&mut self, project: &PathBuf) {
        let Ok(text) = std::fs::read_to_string(project) else {
            self.status = format!("cannot read {}", project.display());
            return;
        };
        let Ok(p) = serde_json::from_str::<keepcut_core::model::Project>(&text) else {
            self.status = "invalid project JSON".into();
            return;
        };
        let base = project.parent().map(PathBuf::from).unwrap_or_default();
        let mut items = Vec::new();
        for c in &p.clips {
            let path = if c.path.is_relative() {
                base.join(&c.path)
            } else {
                c.path.clone()
            };
            let path = std::path::absolute(path)
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            for s in c.segments.iter().flatten() {
                items.push((path.clone(), s.start, s.end));
            }
        }
        self.player.load(&player::edl_url(&items));
        self.status = format!("project preview: {} segments", items.len());
        self.cut_preview = true;
    }

    fn parse_segments(&self) -> Vec<(f64, f64)> {
        self.segments_text
            .split(',')
            .filter_map(|p| {
                let (a, b) = p.split_once('-')?;
                Some((
                    keepcut_core::timefmt::parse(a)?,
                    keepcut_core::timefmt::parse(b)?,
                ))
            })
            .filter(|(a, b)| b > a)
            .collect()
    }

    fn toggle_cut_preview(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let p = path.to_string_lossy().into_owned();
        if self.cut_preview {
            self.player.load(&p);
            self.cut_preview = false;
        } else {
            let segs = self.parse_segments();
            if segs.is_empty() {
                self.status = "enter segments like 1.5-4, 10-12.25".into();
                return;
            }
            let items: Vec<_> = segs.iter().map(|(a, b)| (p.clone(), *a, *b)).collect();
            self.player.load(&player::edl_url(&items));
            self.cut_preview = true;
        }
    }

    fn start_op(&mut self, kind: &'static str) {
        self.op = Some(Op {
            kind,
            start: Instant::now(),
            from_time: self.player.state.time_pos,
            triggered: false,
            rendered: false,
        });
    }

    fn seek(&mut self, t: f64) {
        self.start_op("seek");
        self.player.seek_exact(t.max(0.0));
    }

    fn step(&mut self, forward: bool) {
        self.start_op(if forward {
            "frame-step"
        } else {
            "frame-back-step"
        });
        let t = self.player.state.time_pos;
        match (self.step_mode, &self.index) {
            // exact seek to the neighbouring frame of the index: no real-time wait for
            // the next frame of VFR footage, but decodes from the keyframe
            (StepMode::Seek, Some(fi)) | (StepMode::Auto, Some(fi))
                if self.step_mode == StepMode::Seek || !forward =>
            {
                let target = if forward {
                    fi.frames.get(fi.idx_at_or_after(t + 1e-4)).map(|f| f.t)
                } else {
                    fi.prev_frame_time(t - 1e-4)
                };
                if let Some(x) = target {
                    self.player.seek_exact(x);
                }
            }
            // play the step at high speed so VFR gaps don't take real time
            (StepMode::Fast | StepMode::Auto, _) if forward => {
                self.player.command("set", &["speed", "32"]);
                self.player.command("frame-step", &[]);
            }
            _ => self.player.command(
                if forward {
                    "frame-step"
                } else {
                    "frame-back-step"
                },
                &[],
            ),
        }
    }

    fn keyframe_jump(&mut self, forward: bool) {
        let Some(fi) = &self.index else { return };
        let t = self.player.state.time_pos;
        let k = if forward {
            fi.next_key_at_or_after(t + 1e-3)
        } else {
            fi.last_key_at_or_before(t - 1e-3)
        };
        if let Some(k) = k {
            self.start_op(if forward {
                "next-keyframe"
            } else {
                "prev-keyframe"
            });
            self.player.seek_exact(k);
        }
    }

    fn handle_events(&mut self) {
        for e in self.player.poll() {
            match e {
                PlayerEvent::PlaybackRestart => {
                    if let Some(op) = &mut self.op {
                        op.triggered = true;
                    }
                    self.finish_op_if_done();
                }
                PlayerEvent::Prop("time-pos", _) => {
                    if let Some(op) = &mut self.op
                        && op.kind == "frame-step"
                        && (self.player.state.time_pos - op.from_time).abs() > 1e-6
                    {
                        op.triggered = true;
                    }
                    self.finish_op_if_done();
                }
                PlayerEvent::Shutdown => {}
                _ => {}
            }
        }
        if let Some(rx) = &self.index_rx
            && let Ok(r) = rx.try_recv()
        {
            match r {
                Ok(fi) => self.index = Some(fi),
                Err(e) => self.status = format!("probe failed: {e}"),
            }
            self.index_rx = None;
        }
    }

    /// Called after a new video frame reached the screen texture.
    fn on_frame_rendered(&mut self) {
        self.frames_rendered += 1;
        if let Some(op) = &mut self.op {
            op.rendered = true;
        }
        self.finish_op_if_done();
    }

    fn finish_op_if_done(&mut self) {
        if let Some(op) = &self.op
            && op.triggered
            && op.rendered
        {
            if matches!(self.step_mode, StepMode::Fast | StepMode::Auto) {
                self.player.command("set", &["speed", "1"]);
            }
            let ms = op.start.elapsed().as_secs_f64() * 1000.0;
            self.results.push((op.kind.to_string(), ms));
            if self.results.len() > 200 {
                self.results.remove(0);
            }
            self.op = None;
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Key;
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (space, left, right, shift, ctrl, home, end, p) = ctx.input(|i| {
            (
                i.key_pressed(Key::Space),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.modifiers.shift,
                i.modifiers.command,
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::P),
            )
        });
        if space {
            self.player.command("cycle", &["pause"]);
        }
        for (pressed, fwd) in [(left, false), (right, true)] {
            if !pressed {
                continue;
            }
            if shift {
                self.keyframe_jump(fwd);
            } else if ctrl {
                let t = self.player.state.time_pos + if fwd { 5.0 } else { -5.0 };
                self.seek(t);
            } else {
                self.step(fwd);
            }
        }
        if home {
            self.seek(0.0);
        }
        if end {
            let d = self.player.state.duration;
            self.seek((d - 0.001).max(0.0));
        }
        if p {
            self.toggle_cut_preview();
        }
    }

    fn timeline(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(width, 36.0), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let vis = ui.visuals();
        painter.rect_filled(rect, 3.0, vis.extreme_bg_color);
        let dur = self.player.state.duration.max(1e-6);
        let x_of = |t: f64| rect.left() + (t / dur) as f32 * rect.width();
        if !self.cut_preview {
            for (a, b) in self.parse_segments() {
                let r = egui::Rect::from_x_y_ranges(
                    x_of(a)..=x_of(b),
                    rect.top() + 4.0..=rect.bottom() - 4.0,
                );
                painter.rect_filled(r, 2.0, vis.selection.bg_fill);
            }
            if let Some(fi) = &self.index {
                // keyframe ticks only when at least 3 px apart
                let mut last_x = f32::MIN;
                for k in fi.keyframes() {
                    let x = x_of(k);
                    if x - last_x >= 3.0 {
                        painter.line_segment(
                            [
                                egui::pos2(x, rect.bottom() - 10.0),
                                egui::pos2(x, rect.bottom()),
                            ],
                            egui::Stroke::new(1.0, vis.warn_fg_color),
                        );
                        last_x = x;
                    }
                }
            }
        }
        let x = x_of(self.player.state.time_pos);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(2.0, vis.strong_text_color()),
        );
        if (resp.clicked() || resp.dragged())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * dur;
            self.seek(t);
        }
    }

    fn video(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let avail = ui.available_rect_before_wrap();
        let ds = self.player.state.dsize;
        let aspect = if ds[0] > 0 && ds[1] > 0 {
            ds[0] as f32 / ds[1] as f32
        } else {
            16.0 / 9.0
        };
        let mut size = avail.size();
        if size.x / size.y > aspect {
            size.x = size.y * aspect;
        } else {
            size.y = size.x / aspect;
        }
        let rect = egui::Rect::from_center_size(avail.center(), size);
        ui.allocate_rect(avail, egui::Sense::hover());
        let ppp = ui.ctx().pixels_per_point();
        let px = [(size.x * ppp).round() as u32, (size.y * ppp).round() as u32];
        let (tex, new_frame) = match self.renderer.as_mut() {
            None => (None, false),
            Some(Renderer::Gl(r)) => {
                let id = r.frame(frame, px);
                (Some(id), std::mem::take(&mut r.rendered))
            }
            Some(Renderer::Sw(r)) => {
                let id = r.frame(ui.ctx(), px);
                (id, std::mem::take(&mut r.uploaded))
            }
        };
        if new_frame {
            self.on_frame_rendered();
        }
        if let Some(id) = tex {
            ui.painter().image(
                id,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        self.overlay(ui, avail);
    }

    fn overlay(&self, ui: &mut egui::Ui, rect: egui::Rect) {
        let s = &self.player.state;
        let render_ms = match &self.renderer {
            Some(Renderer::Gl(r)) => r.last_render_ms,
            Some(Renderer::Sw(r)) => *r.last_render_ms.lock().unwrap(),
            None => 0.0,
        };
        let mut lines = vec![
            format!(
                "render: {:?}   hwdec: {}   codec: {}",
                self.kind, s.hwdec, s.codec
            ),
            format!(
                "video {}x{}  vf-fps {:.2}  drops vo/dec {}/{}  render {:.1} ms  frames {}",
                s.dsize[0],
                s.dsize[1],
                s.vf_fps,
                s.vo_drops,
                s.dec_drops,
                render_ms,
                self.frames_rendered
            ),
            format!("cpu {:.0}% of one core", self.cpu.last),
        ];
        for kind in [
            "seek",
            "frame-step",
            "frame-back-step",
            "next-keyframe",
            "prev-keyframe",
        ] {
            let v: Vec<f64> = self
                .results
                .iter()
                .filter(|(k, _)| k == kind)
                .map(|(_, ms)| *ms)
                .collect();
            if let Some(last) = v.last() {
                let max = v.iter().cloned().fold(0.0, f64::max);
                lines.push(format!(
                    "{kind}: last {last:.0} ms, max {max:.0} ms (n={})",
                    v.len()
                ));
            }
        }
        let painter = ui.painter_at(rect);
        let font = egui::FontId::monospace(12.0);
        let mut y = rect.top() + 6.0;
        for l in lines {
            let galley =
                painter.layout_no_wrap(l, font.clone(), egui::Color32::from_rgb(230, 230, 230));
            let r = egui::Rect::from_min_size(egui::pos2(rect.left() + 6.0, y), galley.size())
                .expand(2.0);
            painter.rect_filled(r, 2.0, egui::Color32::from_black_alpha(160));
            painter.galley(
                egui::pos2(rect.left() + 6.0, y),
                galley,
                egui::Color32::WHITE,
            );
            y += 16.0;
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&glow::Context>) {
        // free the render context while the GL context is still current
        self.renderer = None;
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.ui_frames += 1;
        self.handle_events();
        self.cpu.tick();
        self.shortcuts(&ctx);
        if self.bench.is_some() {
            self.run_bench(&ctx);
        }
        self.screenshots(&ctx);

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open…").clicked()
                    && let Some(f) = rfd::FileDialog::new()
                        .add_filter("video", &["mp4", "mkv", "mov", "ts"])
                        .pick_file()
                {
                    self.open(f);
                }
                if let Some(p) = self.project.clone()
                    && ui.button("Preview project").clicked()
                {
                    self.preview_project(&p);
                }
                ui.separator();
                ui.label("Segments:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.segments_text)
                        .hint_text("1.5-4, 10-12.25")
                        .desired_width(220.0),
                );
                if ui
                    .selectable_label(self.cut_preview, "Cut preview (P)")
                    .clicked()
                {
                    self.toggle_cut_preview();
                }
                ui.separator();
                ui.label(
                    self.path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                );
                ui.label(&self.status);
            });
        });
        egui::Panel::bottom("bottom").show(ui, |ui| {
            ui.add_space(4.0);
            self.timeline(ui);
            ui.horizontal(|ui| {
                let s = self.player.state.clone();
                if ui
                    .button("|< Key")
                    .on_hover_text("previous keyframe (Shift+←)")
                    .clicked()
                {
                    self.keyframe_jump(false);
                }
                if ui
                    .button("< Frame")
                    .on_hover_text("previous frame (←)")
                    .clicked()
                {
                    self.step(false);
                }
                if ui
                    .button(if s.pause { "Play" } else { "Pause" })
                    .on_hover_text("play/pause (Space)")
                    .clicked()
                {
                    self.player.command("cycle", &["pause"]);
                }
                if ui
                    .button("Frame >")
                    .on_hover_text("next frame (→)")
                    .clicked()
                {
                    self.step(true);
                }
                if ui
                    .button("Key >|")
                    .on_hover_text("next keyframe (Shift+→)")
                    .clicked()
                {
                    self.keyframe_jump(true);
                }
                ui.monospace(format!(
                    "{} / {}",
                    keepcut_core::timefmt::display(s.time_pos),
                    keepcut_core::timefmt::display(s.duration)
                ));
                if let Some(fi) = &self.index {
                    ui.label(format!(
                        "{} frames, {} keyframes",
                        fi.len(),
                        fi.keyframes().count()
                    ));
                } else if self.index_rx.is_some() {
                    ui.spinner();
                }
            });
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.video(ui, frame);
        });

        if let Some(Renderer::Gl(r)) = &self.renderer {
            r.report_swap();
        }
        if self.op.is_some() || self.index_rx.is_some() || self.bench.is_some() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

// ---------------------------------------------------------------------------
// CPU usage of this process
// ---------------------------------------------------------------------------

struct Cpu {
    sys: sysinfo::System,
    pid: sysinfo::Pid,
    at: Instant,
    last: f32,
}

impl Cpu {
    fn new() -> Cpu {
        Cpu {
            sys: sysinfo::System::new().expect("sysinfo"),
            pid: sysinfo::get_current_pid().expect("pid"),
            at: Instant::now(),
            last: 0.0,
        }
    }
    /// Refreshes at most once per second; value is % of one core.
    fn tick(&mut self) {
        if self.at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.at = Instant::now();
        self.sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[self.pid]),
            true,
            sysinfo::ProcessRefreshKind::nothing().with_cpu(),
        );
        if let Some(p) = self.sys.process(self.pid) {
            self.last = p.cpu_usage();
        }
    }
}

// ---------------------------------------------------------------------------
// Scripted performance gate (§7)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    WaitLoad,
    Play,
    Seeks(usize),
    Steps(usize),
    BackSteps(usize),
    Done,
    Finished,
}

struct Bench {
    step: Step,
    at: Instant,
    play_start: Option<(Instant, u64, i64, i64, f64)>,
    cpu_samples: Vec<f32>,
    report: serde_json::Map<String, serde_json::Value>,
    rng: u64,
}

impl Bench {
    fn new() -> Bench {
        Bench {
            step: Step::WaitLoad,
            at: Instant::now(),
            play_start: None,
            cpu_samples: Vec::new(),
            report: Default::default(),
            rng: 0x2545F4914F6CDD1D,
        }
    }
    fn rand(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % 1_000_000) as f64 / 1_000_000.0
    }
}

const PLAY_SECS: f64 = 10.0;
const REPEAT: usize = 8;

impl App {
    fn run_bench(&mut self, ctx: &egui::Context) {
        let mut b = self.bench.take().expect("bench");
        let s = self.player.state.clone();
        let idle = self.op.is_none();
        match b.step {
            Step::WaitLoad => {
                let probe_failed = self.status.starts_with("probe failed");
                if probe_failed || b.at.elapsed() > Duration::from_secs(30) {
                    let why = if probe_failed {
                        self.status.clone()
                    } else {
                        "file did not load within 30 s".into()
                    };
                    b.report.insert("error".into(), why.into());
                    b.report.insert(
                        "file".into(),
                        self.path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                            .into(),
                    );
                    println!("{}", serde_json::Value::Object(b.report.clone()));
                    b.step = Step::Finished;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if s.duration > 0.0
                    && self.index.is_some()
                    && s.dsize[0] > 0
                    && b.at.elapsed() > Duration::from_secs(1)
                {
                    self.player.command("set", &["pause", "no"]);
                    b.step = Step::Play;
                    b.at = Instant::now();
                }
            }
            Step::Play => {
                let start = *b.play_start.get_or_insert((
                    Instant::now(),
                    self.frames_rendered,
                    s.vo_drops,
                    s.dec_drops,
                    s.time_pos,
                ));
                if b.at.elapsed() > Duration::from_secs(2) {
                    b.cpu_samples.push(self.cpu.last);
                }
                if start.0.elapsed().as_secs_f64() >= PLAY_SECS || s.eof {
                    let secs = start.0.elapsed().as_secs_f64();
                    let media = s.time_pos - start.4;
                    let rendered = self.frames_rendered - start.1;
                    let cpu = b.cpu_samples.iter().sum::<f32>() / b.cpu_samples.len().max(1) as f32;
                    b.report.insert("play_wall_s".into(), secs.into());
                    b.report.insert("play_media_s".into(), media.into());
                    b.report
                        .insert("rendered_fps".into(), (rendered as f64 / secs).into());
                    b.report.insert("vf_fps".into(), s.vf_fps.into());
                    b.report
                        .insert("vo_drops".into(), (s.vo_drops - start.2).into());
                    b.report
                        .insert("decoder_drops".into(), (s.dec_drops - start.3).into());
                    b.report
                        .insert("cpu_percent_of_core".into(), (cpu as f64).into());
                    b.report.insert("hwdec".into(), s.hwdec.clone().into());
                    self.player.command("set", &["pause", "yes"]);
                    b.step = Step::Seeks(0);
                    b.at = Instant::now();
                }
            }
            Step::Seeks(n) if idle && b.at.elapsed() > Duration::from_millis(300) => {
                if n == REPEAT {
                    b.step = Step::Steps(0);
                } else {
                    let t = b.rand() * (s.duration - 2.0).max(0.1);
                    self.seek(t);
                    b.step = Step::Seeks(n + 1);
                }
                b.at = Instant::now();
            }
            Step::Steps(n) if idle && b.at.elapsed() > Duration::from_millis(200) => {
                if n == REPEAT {
                    // land in the middle of a GOP for the back-steps
                    let t = b.rand() * (s.duration - 2.0).max(0.1);
                    let t = self.index.as_ref().map_or(t, |fi| {
                        let k0 = fi.last_key_at_or_before(t).unwrap_or(0.0);
                        let k1 = fi.next_key_at_or_after(t + 1e-3).unwrap_or(s.duration);
                        (k0 + k1) / 2.0
                    });
                    self.seek(t);
                    b.step = Step::BackSteps(0);
                } else {
                    self.step(true);
                    b.step = Step::Steps(n + 1);
                }
                b.at = Instant::now();
            }
            Step::BackSteps(n) if idle && b.at.elapsed() > Duration::from_millis(300) => {
                if n == REPEAT {
                    b.step = Step::Done;
                } else {
                    self.step(false);
                    b.step = Step::BackSteps(n + 1);
                }
                b.at = Instant::now();
            }
            Step::Finished => {}
            Step::Done => {
                b.step = Step::Finished;
                for kind in ["seek", "frame-step", "frame-back-step"] {
                    let v: Vec<f64> = self
                        .results
                        .iter()
                        .filter(|(k, _)| k == kind)
                        .map(|(_, ms)| *ms)
                        .collect();
                    let max = v.iter().cloned().fold(0.0, f64::max);
                    let avg = v.iter().sum::<f64>() / v.len().max(1) as f64;
                    b.report.insert(format!("{kind}_ms_avg"), avg.into());
                    b.report.insert(format!("{kind}_ms_max"), max.into());
                    b.report.insert(format!("{kind}_n"), v.len().into());
                }
                b.report
                    .insert("render".into(), format!("{:?}", self.kind).into());
                b.report
                    .insert("step_mode".into(), format!("{:?}", self.step_mode).into());
                b.report.insert(
                    "video".into(),
                    format!("{}x{} {}", s.dsize[0], s.dsize[1], s.codec).into(),
                );
                b.report.insert(
                    "file".into(),
                    self.path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                        .into(),
                );
                println!("{}", serde_json::Value::Object(b.report.clone()));
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }
        // safety net: an op that never completes (e.g. step at EOF) is dropped after 5 s
        if let Some(op) = &self.op
            && op.start.elapsed() > Duration::from_secs(5)
        {
            self.results.push((format!("{}-timeout", op.kind), 5000.0));
            self.op = None;
        }
        self.bench = Some(b);
    }
}

// ---------------------------------------------------------------------------
// Screenshots of the app's own frame (never the screen around it)
// ---------------------------------------------------------------------------

impl App {
    fn screenshots(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(egui::Key::F12)) {
            self.shot_counter += 1;
            let path =
                std::env::temp_dir().join(format!("keepcut-player-{}.png", self.shot_counter));
            self.status = format!("screenshot: {}", path.display());
            request_screenshot(ctx, path, false);
        }
        let loaded = self.player.state.dsize[0] > 0
            && self.player.state.duration > 0.0
            && (self.index.is_some() || self.path.is_none());
        if let Some((path, since)) = &mut self.auto_shot {
            match since {
                None if loaded => *since = Some(Instant::now()),
                Some(t) if t.elapsed() > Duration::from_secs(3) => {
                    request_screenshot(ctx, path.clone(), true);
                    self.auto_shot = None;
                }
                _ => {}
            }
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

fn request_screenshot(ctx: &egui::Context, path: PathBuf, exit_after: bool) {
    let ctx2 = ctx.clone();
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
        egui::ScreenshotCallback::new(move |img| {
            if let Err(e) = save_png(&img, &path) {
                eprintln!("screenshot failed: {e}");
            } else {
                println!("screenshot saved: {}", path.display());
            }
            if exit_after {
                ctx2.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx2.request_repaint();
        }),
    ));
}

fn save_png(img: &egui::ColorImage, path: &std::path::Path) -> Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header()?;
    let bytes: Vec<u8> = img
        .pixels
        .iter()
        .flat_map(|c| c.to_srgba_unmultiplied())
        .collect();
    w.write_image_data(&bytes)?;
    Ok(())
}
