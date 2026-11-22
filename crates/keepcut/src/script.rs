//! Test automation: `keepcut --script FILE [CLIPS…]` runs one command per line
//! through the same code paths as the UI, prints results to stdout, and can save
//! screenshots of the app's own frame. Lines starting with `#` are comments.
//!
//! Commands: wait-probed · select N · seek T · step · back · key-next · key-prev ·
//! set-start · set-end · split · delete · select-seg N · trim N start|end T [key] ·
//! undo · redo · cut-preview · cancel-export ·
//! preview-all · toggle-clip N · move-clip FROM TO · sleep SECS · screenshot PATH ·
//! encoding smart|full · output-dir PATH · output-name NAME · open-export ·
//! close-export · export · wait-export · print-state · quit ·
//! set KEY VALUE (export options, see `set_export_option`) · wait-encoders · log-panel ·
//! clip-title N TEXT · seg-title N TEXT (selected clip) · print-youtube ·
//! save PATH · open PATH · new · relink N PATH · settings · show-save-prompt · close-dialogs ·
//! add-dialog · window-size W H · soundtrack PATH · soundtrack-offset T · soundtrack-off · mute · play (play/pause) · mpv-set NAME VALUE · hw-decoding auto|off

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
                "add-dialog" => {
                    // "Add clips…" with the real file dialog (blocks until it closes)
                    self.pick_and_add_files();
                    true
                }
                "output-name" => {
                    self.project.export.output_name = Some(arg.to_string());
                    self.mark_plan_dirty();
                    true
                }
                "set" => {
                    // export settings, as the dialog would set them
                    let (key, val) = arg.split_once(' ').unwrap_or((arg, ""));
                    if let Err(e) = self.set_export_option(key, val.trim()) {
                        out(format!("script: set {key}: {e}"));
                    }
                    self.mark_plan_dirty();
                    true
                }
                "clip-title" => {
                    // clip-title N TEXT: through the Chapter title dialog's code path
                    let (n, text) = arg.split_once(' ').unwrap_or((arg, ""));
                    if let Some(c) = n
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| self.project.clips.get(i))
                    {
                        self.title_dialog = Some(crate::clips::TitleDialog {
                            clip: c.id,
                            text: text.to_string(),
                            focus: false,
                        });
                        self.apply_title_dialog();
                    }
                    true
                }
                "seg-title" => {
                    // seg-title N TEXT: segment N of the selected clip (one undo step)
                    let (n, text) = arg.split_once(' ').unwrap_or((arg, ""));
                    let n: usize = n.parse().unwrap_or(0);
                    if let Some(id) = self.selected {
                        self.edit(|clips| {
                            let Some(s) = clips
                                .iter_mut()
                                .find(|c| c.id == id)
                                .and_then(|c| c.segments.as_mut())
                                .and_then(|s| s.get_mut(n))
                            else {
                                return false;
                            };
                            s.chapter_title = (!text.trim().is_empty()).then(|| text.to_string());
                            true
                        });
                    }
                    true
                }
                "save" => {
                    // save PATH (Save as without the file dialog)
                    let ok = self.save_to(std::path::Path::new(arg));
                    out(format!("save: {ok}"));
                    true
                }
                "open" => {
                    self.open_project(std::path::Path::new(arg));
                    true
                }
                "new" => {
                    self.new_project();
                    true
                }
                "relink" => {
                    // relink N PATH (Locate… without the file dialog)
                    let (n, p) = arg.split_once(' ').unwrap_or((arg, ""));
                    if let Some(c) = n
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| self.project.clips.get(i))
                    {
                        self.relink(c.id, PathBuf::from(p.trim()));
                    }
                    true
                }
                "settings" => {
                    self.open_settings();
                    true
                }
                "show-save-prompt" => {
                    // the unsaved-changes prompt, for a screenshot (answer: Cancel)
                    self.proj.prompt = Some(crate::project::After::New);
                    true
                }
                "close-dialogs" => {
                    self.proj.prompt = None;
                    self.proj.relink_open = false;
                    self.title_dialog = None;
                    self.portal_notice = false;
                    self.close_settings();
                    true
                }
                "print-youtube" => {
                    if let Some(Ok(p)) = &self.plan {
                        for o in &p.outputs {
                            let ch =
                                keepcut_core::exec::build_chapters(o, self.project.export.chapters);
                            let (text, warn) = keepcut_core::chapters::youtube_block(&ch);
                            out(format!(
                                "youtube: {} | {} | warnings: {warn:?}",
                                o.path.display(),
                                text.replace('\n', " / ")
                            ));
                        }
                    }
                    true
                }
                "wait-encoders" => self.encoders_detected(),
                "show-export-log" => {
                    self.export.show_finished_log();
                    true
                }
                "soundtrack" => {
                    self.set_soundtrack(PathBuf::from(arg));
                    true
                }
                "soundtrack-offset" => {
                    self.set_soundtrack_offset(keepcut_core::timefmt::parse(arg).unwrap_or(0.0));
                    true
                }
                "hw-decoding" => {
                    // like the Settings choice (scripted runs never save the settings)
                    self.config.hw_decoding = if arg == "off" {
                        crate::config::HwDecoding::Off
                    } else {
                        crate::config::HwDecoding::Auto
                    };
                    self.apply_hw_decoding();
                    s.wait_until = Some(Instant::now() + Duration::from_millis(800));
                    true
                }
                "mpv-set" => {
                    // debugging: any mpv property, e.g. `mpv-set hwdec no`
                    let (k, v) = arg.split_once(' ').unwrap_or((arg, ""));
                    self.player.command("set", &[k, v]);
                    true
                }
                "play" => {
                    self.play_pause();
                    true
                }
                "mute" => {
                    self.player.command("set", &["mute", "yes"]);
                    true
                }
                "soundtrack-off" => {
                    self.remove_soundtrack();
                    true
                }
                "window-size" => {
                    // inner size in points, e.g. `window-size 1100 750`
                    let mut it = arg
                        .split_whitespace()
                        .map(|v| v.parse::<f32>().unwrap_or(0.0));
                    let (w, h) = (it.next().unwrap_or(0.0), it.next().unwrap_or(0.0));
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
                    true
                }
                "log-panel" => {
                    self.export.toggle_log_panel();
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

    /// `set KEY VALUE`: mode joined|per-clip|per-segment · codec h264|hevc ·
    /// encoder NAME|auto · smart-encoder NAME|auto · quality match|crf N|bitrate N ·
    /// smart-quality … · res same|1080|WxH · fps keep|N · content auto|screen|camera ·
    /// audio mix|first · aac KBPS · target auto|N (index in the plan's format list) ·
    /// loudness off|whole|per-clip|per-segment · lufs N · chapters none|per-clip|per-segment.
    fn set_export_option(&mut self, key: &str, val: &str) -> anyhow::Result<()> {
        use keepcut_core::model::{
            AudioTracks, Chapters, ContentType, Loudness, OutputMode, Quality, Resolution,
            VideoCodec,
        };
        let quality = |v: &str| -> anyhow::Result<Quality> {
            let (k, n) = v.split_once(' ').unwrap_or((v, ""));
            Ok(match k {
                "match" => Quality::MatchSource,
                "crf" => Quality::Crf(n.trim().parse()?),
                "bitrate" => Quality::BitrateKbps(n.trim().parse()?),
                _ => anyhow::bail!("quality: match | crf N | bitrate KBPS"),
            })
        };
        let enc = |v: &str| (v != "auto").then(|| v.to_string());
        let x = &mut self.project.export;
        match key {
            "mode" => {
                x.output_mode = match val {
                    "joined" => OutputMode::Joined,
                    "per-clip" => OutputMode::PerClip,
                    "per-segment" => OutputMode::PerSegment,
                    _ => anyhow::bail!("joined | per-clip | per-segment"),
                }
            }
            "codec" => {
                x.full.codec = if val == "hevc" {
                    VideoCodec::Hevc
                } else {
                    VideoCodec::H264
                }
            }
            "encoder" => x.full.encoder = enc(val),
            "smart-encoder" => x.smart_encoder = enc(val),
            "quality" => x.full.quality = quality(val)?,
            "smart-quality" => x.smart_quality = quality(val)?,
            "res" => {
                x.full.resolution = match val.split_once('x') {
                    _ if val == "same" => Resolution::Same,
                    Some((w, h)) => Resolution::Custom(w.parse()?, h.parse()?),
                    None => Resolution::ShortSide(val.trim_end_matches('p').parse()?),
                }
            }
            "fps" => {
                x.full.fps = if val == "keep" {
                    None
                } else {
                    Some(val.parse()?)
                }
            }
            "content" => {
                x.full.content = match val {
                    "screen" => ContentType::Screen,
                    "camera" => ContentType::Camera,
                    _ => ContentType::Auto,
                }
            }
            "audio" => {
                x.audio_tracks = if val == "first" {
                    AudioTracks::FirstOnly
                } else {
                    AudioTracks::MixAll
                }
            }
            "aac" => x.audio_bitrate_kbps = val.parse()?,
            "loudness" => {
                x.loudness = match val {
                    "off" => Loudness::Off,
                    "whole" => Loudness::Whole,
                    "per-clip" => Loudness::PerClip,
                    "per-segment" => Loudness::PerSegment,
                    _ => anyhow::bail!("off | whole | per-clip | per-segment"),
                }
            }
            "lufs" => x.loudness_target = val.parse()?,
            "chapters" => {
                x.chapters = match val {
                    "none" => Chapters::None,
                    "per-clip" => Chapters::PerClip,
                    "per-segment" => Chapters::PerSegment,
                    _ => anyhow::bail!("none | per-clip | per-segment"),
                }
            }
            "target" => {
                x.target_override = match (&self.plan, val) {
                    (_, "auto") => None,
                    (Some(Ok(p)), i) => Some(
                        p.signatures
                            .get(i.parse::<usize>()?)
                            .ok_or_else(|| anyhow::anyhow!("no format {i}"))?
                            .0
                            .clone(),
                    ),
                    _ => anyhow::bail!("no plan yet"),
                }
            }
            _ => anyhow::bail!("unknown option"),
        }
        Ok(())
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
                "formats": p.signatures.iter().map(|(s, _)| s.label()).collect::<Vec<_>>(),
                "est_bytes": p.outputs.iter().map(|o| o.est_bytes).collect::<Vec<_>>(),
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
            "project": self.project_name(),
            "dirty": self.proj.dirty,
            "missing": self.missing_count(),
            "relink_open": self.proj.relink_open,
            "titles": self.project.clips.iter().map(|c| serde_json::json!({
                "clip": c.chapter_title,
                "segments": c.segments.iter().flatten().map(|s| s.chapter_title.clone()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "can_undo": self.history.can_undo(),
            "can_redo": self.history.can_redo(),
            "plan": plan,
            "soundtrack": self.project.soundtrack.as_ref().map(|st| serde_json::json!({
                "file": st.path.file_name().map(|n| n.to_string_lossy().into_owned()),
                "offset": st.offset,
            })),
            "loudness": format!("{:?}", self.project.export.loudness),
            "hwdec": self.player.state.hwdec,
            // the audio track mpv plays: an external file (the soundtrack) or the clips' own
            "audio_file": self.player.mpv.get_property::<String>("current-tracks/audio/external-filename").ok(),
            "audio_track": self.player.mpv.get_property::<i64>("current-tracks/audio/id").ok(),
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
