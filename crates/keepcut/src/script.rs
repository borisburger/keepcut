//! Test automation: `keepcut --script FILE [CLIPS…]` runs one command per line
//! through the same code paths as the UI, prints results to stdout, and can save
//! screenshots of the app's own frame. Lines starting with `#` are comments.
//!
//! Commands: wait-probed · select N · seek T · step · back · key-next · key-prev ·
//! set-start · set-end · split · delete · select-seg N · trim N start|end T [key] ·
//! undo · redo · cut-preview · cancel-export ·
//! preview-all · toggle-clip N · move-clip FROM TO · sleep SECS · screenshot PATH ·
//! encoding smart|full · output-dir PATH · output-name NAME · open-export ·
//! close-export · export · wait-export · print-state · quit

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use keepcut_core::model::EncodingMode;

use crate::app::App;

pub struct Script {
    lines: Vec<String>,
    pos: usize,
    wait_until: Option<Instant>,
    started: Instant,
}

impl Script {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Script> {
        let lines = std::fs::read_to_string(path)?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect();
        Ok(Script {
            lines,
            pos: 0,
            wait_until: None,
            started: Instant::now(),
        })
    }
}

fn out(s: impl AsRef<str>) {
    use std::io::Write;
    println!("{}", s.as_ref());
    let _ = std::io::stdout().flush();
}

impl App {
    /// Runs script commands until one has to wait. Called once per frame.
    pub fn run_script(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.script.take() else {
            return;
        };
        if s.pos >= s.lines.len() {
            // finished: keep `Some` so settings stay untouched on exit
            self.script = Some(s);
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(50));
        if s.started.elapsed() > Duration::from_secs(900) {
            out("script: timeout");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        while let Some(line) = s.lines.get(s.pos).cloned() {
            if s.wait_until.is_some_and(|t| Instant::now() < t) {
                break;
            }
            s.wait_until = None;
            let mut parts = line.splitn(2, ' ');
            let cmd = parts.next().unwrap_or("");
            let arg = parts.next().unwrap_or("").trim();
            let num = || arg.parse::<f64>().unwrap_or(0.0);
            let done = match cmd {
                "wait-probed" => {
                    let selected_readable = self.selected.and_then(|id| self.info(id)).is_some();
                    !self.project.clips.is_empty()
                        && self.pending_count() == 0
                        && (!selected_readable || self.player.state.duration > 0.0)
                }
                "select" => {
                    let id = self.project.clips.get(num() as usize).map(|c| c.id);
                    self.select_clip(id);
                    true
                }
                "seek" => {
                    self.seek(num());
                    s.wait_until = Some(Instant::now() + Duration::from_millis(600));
                    true
                }
                "step" => {
                    self.step(true);
                    s.wait_until = Some(Instant::now() + Duration::from_millis(400));
                    true
                }
                "back" => {
                    self.step(false);
                    s.wait_until = Some(Instant::now() + Duration::from_millis(600));
                    true
                }
                "key-next" | "key-prev" => {
                    self.keyframe_jump(cmd == "key-next");
                    s.wait_until = Some(Instant::now() + Duration::from_millis(600));
                    true
                }
                "set-start" => {
                    self.set_start();
                    true
                }
                "set-end" => {
                    self.set_end();
                    true
                }
                "split" => {
                    self.split();
                    true
                }
                "delete" => {
                    self.delete_selected_segment();
                    true
                }
                "select-seg" => {
                    self.selected_seg = self
                        .selected
                        .and_then(|c| self.segments(c).get(num() as usize).map(|s| s.id));
                    true
                }
                "trim" => {
                    // trim SEG start|end T [key]: like dragging an edge (one undo step)
                    let v: Vec<&str> = arg.split_whitespace().collect();
                    if let (Some(clip), [i, edge, t, rest @ ..]) = (self.selected, &v[..]) {
                        let seg = i
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| self.segments(clip).get(i).map(|s| s.id));
                        let edge = if *edge == "start" {
                            crate::app::Edge::Start
                        } else {
                            crate::app::Edge::End
                        };
                        if let (Some(seg), Ok(t)) = (seg, t.parse::<f64>()) {
                            let before = self.project.clips.clone();
                            self.trim_edge(clip, seg, edge, t, rest.first() == Some(&"key"));
                            self.history.push(before);
                        }
                    }
                    s.wait_until = Some(Instant::now() + Duration::from_millis(300));
                    true
                }
                "cancel-export" => {
                    self.cancel_export();
                    true
                }
                "undo" => {
                    self.undo();
                    true
                }
                "redo" => {
                    self.redo();
                    true
                }
                "cut-preview" => {
                    self.toggle_cut_preview();
                    true
                }
                "preview-all" => {
                    self.toggle_preview_all();
                    true
                }
                "toggle-clip" => {
                    let i = num() as usize;
                    self.edit(|clips| clips.get_mut(i).map(|c| c.enabled = !c.enabled).is_some());
                    true
                }
                "move-clip" => {
                    let v: Vec<usize> = arg
                        .split_whitespace()
                        .filter_map(|x| x.parse().ok())
                        .collect();
                    if let [from, to] = v[..] {
                        self.edit(|clips| {
                            if from >= clips.len() || to >= clips.len() || from == to {
                                return false;
                            }
                            let c = clips.remove(from);
                            clips.insert(to, c);
                            true
                        });
                    }
                    true
                }
                "sleep" => {
                    s.wait_until = Some(Instant::now() + Duration::from_secs_f64(num()));
                    true
                }
                "screenshot" => {
                    request_screenshot(ctx, PathBuf::from(arg));
                    // give the capture a few frames
                    s.wait_until = Some(Instant::now() + Duration::from_millis(500));
                    true
                }
                "encoding" => {
                    self.project.export.encoding = if arg == "full" {
                        EncodingMode::Full
                    } else {
                        EncodingMode::Smart
                    };
                    self.mark_plan_dirty();
                    true
                }
                "output-dir" => {
                    self.config.output_location = crate::config::OutputLocation::Custom;
                    self.config.custom_output_dir = Some(PathBuf::from(arg));
                    self.project.export.output_dir = Some(PathBuf::from(arg));
                    self.mark_plan_dirty();
                    true
                }
                "output-name" => {
                    self.project.export.output_name = Some(arg.to_string());
                    self.mark_plan_dirty();
                    true
                }
                "open-export" => {
                    self.open_export();
                    true
                }
                "close-export" => {
                    self.close_export_windows();
                    true
                }
                "export" => {
                    self.start_export_from_script();
                    true
                }
                "wait-export" => {
                    if let Some(summary) = self.export_finished_summary() {
                        out(format!("export: {summary}"));
                        true
                    } else {
                        false
                    }
                }
                "print-state" => {
                    out(self.state_json());
                    true
                }
                "quit" => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    true
                }
                other => {
                    out(format!("script: unknown command {other}"));
                    true
                }
            };
            if !done {
                break;
            }
            out(format!("> {line}"));
            s.pos += 1;
        }
        self.script = Some(s);
    }

    fn state_json(&self) -> String {
        let clips: Vec<serde_json::Value> = self
            .project
            .clips
            .iter()
            .map(|c| {
                serde_json::json!({
                    "file": c.path.file_name().map(|n| n.to_string_lossy().into_owned()),
                    "enabled": c.enabled,
                    "segments": c.segments.iter().flatten().map(|s| [ (s.start * 1000.0).round() / 1000.0, (s.end * 1000.0).round() / 1000.0 ]).collect::<Vec<_>>(),
                })
            })
            .collect();
        let plan = match &self.plan {
            Some(Ok(p)) => serde_json::json!({
                "target": p.target_idx.map(|t| p.signatures[t].0.label()),
                "clips": p.clips.iter().map(|v| v.summary.clone()).collect::<Vec<_>>(),
                "outputs": p.outputs.iter().map(|o| o.path.display().to_string()).collect::<Vec<_>>(),
            }),
            Some(Err(e)) => serde_json::json!({ "error": e }),
            None => serde_json::Value::Null,
        };
        serde_json::json!({
            "clips": clips,
            "selected": self.selected.and_then(|id| self.project.clips.iter().position(|c| c.id == id)),
            "selected_seg": self.selected.and_then(|c| self.segments(c).iter().position(|s| Some(s.id) == self.selected_seg)),
            "time": (self.source_time() * 1000.0).round() / 1000.0,
            "player_time": (self.player.state.time_pos * 1000.0).round() / 1000.0,
            "loaded": format!("{:?}", std::mem::discriminant(&self.loaded)),
            "preview": matches!(self.loaded, crate::app::Loaded::Preview { .. }),
            "can_undo": self.history.can_undo(),
            "can_redo": self.history.can_redo(),
            "plan": plan,
        })
        .to_string()
    }
}

fn request_screenshot(ctx: &egui::Context, path: PathBuf) {
    let ctx2 = ctx.clone();
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
        egui::ScreenshotCallback::new(move |img| {
            match save_png(&img, &path) {
                Ok(()) => out(format!("screenshot: {}", path.display())),
                Err(e) => out(format!("screenshot failed: {e}")),
            }
            ctx2.request_repaint();
        }),
    ));
}

fn save_png(img: &egui::ColorImage, path: &std::path::Path) -> anyhow::Result<()> {
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
