//! keepcut-cli: thin command-line front end over keepcut-core.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use keepcut_core::encoders::{self, EncoderSet};
use keepcut_core::exec::{self, ExecOptions};
use keepcut_core::ffmpeg::{CancelToken, LogLine, LogSink, Tools};
use keepcut_core::model::{EncodingMode, OutputMode, Project};
use keepcut_core::plan::{self, PieceKind};
use keepcut_core::probe::{self, MediaInfo};
use keepcut_core::timefmt;
use keepcut_core::verify::DecodeCheck;

#[derive(Parser)]
#[command(
    name = "keepcut-cli",
    version,
    about = "Keepcut export pipeline from the command line"
)]
struct Cli {
    /// Print every ffmpeg command line (and stderr with -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Probe files and print stream info and keyframes.
    Probe {
        files: Vec<PathBuf>,
        /// Print all keyframe times.
        #[arg(long)]
        keyframes: bool,
    },
    /// Detect working video encoders.
    Encoders,
    /// Print the export plan for a project JSON file.
    Plan {
        project: PathBuf,
        #[command(flatten)]
        ov: Overrides,
        /// Print the full plan as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Export a project JSON file.
    Export {
        project: PathBuf,
        #[command(flatten)]
        ov: Overrides,
        /// Keep the intermediate files.
        #[arg(long)]
        keep_temp: bool,
        #[arg(long, value_enum, default_value_t = Check::Seams)]
        check: Check,
        /// Skip encoder detection (software encoders only).
        #[arg(long)]
        no_detect: bool,
    },
}

#[derive(clap::Args)]
struct Overrides {
    #[arg(long, value_enum)]
    encoding: Option<Encoding>,
    #[arg(long, value_enum)]
    mode: Option<Mode>,
    #[arg(long)]
    out_dir: Option<PathBuf>,
    #[arg(long)]
    out_name: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Encoding {
    Smart,
    Full,
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Joined,
    PerClip,
    PerSegment,
}

#[derive(Clone, Copy, ValueEnum)]
enum Check {
    Seams,
    Full,
    None,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let tools = Tools::locate()?;
    let verbose = cli.verbose;
    let log: LogSink = Arc::new(move |l: LogLine<'_>| match l {
        LogLine::Command(c) if verbose >= 1 => eprintln!("$ {c}"),
        LogLine::Stderr(s) if verbose >= 2 => eprintln!("  {s}"),
        LogLine::Info(s) => eprintln!("i {s}"),
        _ => {}
    });

    match cli.cmd {
        Command::Probe { files, keyframes } => {
            for f in files {
                let m = probe::probe(&tools, &f, None, Some(&log))?;
                print_info(&m, keyframes);
            }
        }
        Command::Encoders => {
            let set = encoders::detect(&tools, None, Some(&log))?;
            println!("working: {}", set.working.join(", "));
            for (e, why) in &set.failed {
                println!("  unavailable {e}: {why}");
            }
        }
        Command::Plan { project, ov, json } => {
            let (p, infos) = load(&tools, &project, &ov, &log)?;
            let plan = plan::build(&p, &infos, &EncoderSet::software_only())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                print_plan(&plan, &infos);
            }
        }
        Command::Export {
            project,
            ov,
            keep_temp,
            check,
            no_detect,
        } => {
            exec::cleanup_stale(None);
            let (p, infos) = load(&tools, &project, &ov, &log)?;
            let set = if no_detect {
                EncoderSet::software_only()
            } else {
                encoders::detect(&tools, None, Some(&log))?
            };
            let plan = plan::build(&p, &infos, &set)?;
            print_plan(&plan, &infos);
            let opts = ExecOptions {
                temp_root: None,
                keep_temp,
                decode_check: match check {
                    Check::Seams => DecodeCheck::Seams,
                    Check::Full => DecodeCheck::Full,
                    Check::None => DecodeCheck::None,
                },
            };
            let cancel = CancelToken::new();
            let t0 = std::time::Instant::now();
            let mut last = String::new();
            let mut progress = |pr: exec::Progress| {
                let line = format!("[{:5.1}%] {}", pr.fraction * 100.0, pr.step);
                if line != last {
                    eprint!("\r{line:<70}");
                    let _ = std::io::stderr().flush();
                    last = line;
                }
            };
            let results = exec::execute(
                &tools,
                &plan,
                &infos,
                &p.export,
                &opts,
                &cancel,
                &log,
                &mut progress,
            )?;
            eprintln!();
            println!("\nExport finished in {:.1} s", t0.elapsed().as_secs_f64());
            let mut all_ok = true;
            for r in &results {
                let v = &r.verification;
                println!("{}", r.path.display());
                println!(
                    "  video {:.3} s (expected {:.3}), audio {:.3} s, A-V {:+.1} ms, max ts error {:.3} ms, frames {}/{}",
                    v.video_duration,
                    v.expected_duration,
                    v.audio_duration,
                    (v.audio_duration - v.video_duration) * 1000.0,
                    v.max_timestamp_error * 1000.0,
                    v.frames,
                    v.expected_frames
                );
                for (what, secs) in &r.timings {
                    if verbose >= 1 {
                        println!("  {secs:7.2} s  {what}");
                    }
                }
                if v.ok() {
                    println!("  Verified ✓");
                } else {
                    all_ok = false;
                    for pr in &v.problems {
                        println!("  ✗ {pr}");
                    }
                    for e in &v.new_errors {
                        println!("    new decoder message: {e}");
                    }
                }
            }
            if !all_ok {
                bail!("verification failed");
            }
        }
    }
    Ok(())
}

fn load(
    tools: &Tools,
    path: &Path,
    ov: &Overrides,
    log: &LogSink,
) -> Result<(Project, Vec<MediaInfo>)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut p: Project =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    // relative clip paths are relative to the project file
    let base = path.parent().unwrap_or(Path::new("."));
    for c in &mut p.clips {
        if c.path.is_relative() {
            c.path = std::path::absolute(base.join(&c.path))?;
        }
    }
    if let Some(e) = ov.encoding {
        p.export.encoding = match e {
            Encoding::Smart => EncodingMode::Smart,
            Encoding::Full => EncodingMode::Full,
        };
    }
    if let Some(m) = ov.mode {
        p.export.output_mode = match m {
            Mode::Joined => OutputMode::Joined,
            Mode::PerClip => OutputMode::PerClip,
            Mode::PerSegment => OutputMode::PerSegment,
        };
    }
    if let Some(d) = &ov.out_dir {
        std::fs::create_dir_all(d)?;
        p.export.output_dir = Some(std::path::absolute(d)?);
    }
    if let Some(n) = &ov.out_name {
        p.export.output_name = Some(n.clone());
    }
    let mut infos = Vec::new();
    for c in &p.clips {
        infos.push(probe::probe(tools, &c.path, None, Some(log))?);
    }
    Ok((p, infos))
}

fn print_info(m: &MediaInfo, keyframes: bool) {
    println!("{}", m.path.display());
    println!(
        "  format {} duration {} start {:.3} encoder {:?} created {:?}",
        m.format_name,
        timefmt::display(m.duration),
        m.start_time,
        m.encoder,
        m.creation_time
    );
    if let Some(v) = &m.video {
        println!(
            "  video #{} {} {:?} level {:?} {}x{} {} tb {} r {} avg {:.3} b-frames {} rot {} colors {:?}/{:?}/{:?}",
            v.index,
            v.codec_name,
            v.profile,
            v.level,
            v.width,
            v.height,
            v.pix_fmt,
            v.time_base,
            v.r_frame_rate,
            v.avg_frame_rate.as_f64(),
            v.has_b_frames,
            v.rotation,
            v.color_primaries,
            v.color_transfer,
            v.color_space
        );
    }
    for a in &m.audio {
        println!(
            "  audio #{} {} {} Hz {} ch",
            a.index, a.codec_name, a.sample_rate, a.channels
        );
    }
    let keys: Vec<f64> = m.frames.keyframes().collect();
    let gaps: Vec<f64> = keys.windows(2).map(|w| w[1] - w[0]).collect();
    let avg = if gaps.is_empty() {
        0.0
    } else {
        gaps.iter().sum::<f64>() / gaps.len() as f64
    };
    println!(
        "  frames {} end {:.3} keyframes {} (avg gap {:.2} s, max frame delta {:.3} s)",
        m.frames.len(),
        m.frames.end,
        keys.len(),
        avg,
        m.frames.max_frame_delta()
    );
    if keyframes {
        let s: Vec<String> = keys.iter().map(|k| format!("{k:.3}")).collect();
        println!("  keyframes: {}", s.join(" "));
    }
}

fn print_plan(plan: &plan::ExportPlan, infos: &[MediaInfo]) {
    println!("Encoding mode: {:?}", plan.encoding);
    if let Some(t) = plan.target_idx {
        println!("Target: {}", plan.signatures[t].0.label());
    }
    for v in &plan.clips {
        println!(
            "  [{}] {}: {}",
            v.clip_idx,
            infos[v.clip_idx].file_stem(),
            v.summary
        );
        for w in &v.soft_warnings {
            println!("      note: {w}");
        }
    }
    for w in &plan.warnings {
        println!("  warning: {w}");
    }
    for o in &plan.outputs {
        println!(
            "Output {} ({})",
            o.path.display(),
            timefmt::display(o.duration())
        );
        for s in &o.segments {
            println!(
                "  {} [{:.3} – {:.3})",
                s.src.file_name().unwrap_or_default().to_string_lossy(),
                s.start,
                s.end
            );
            for p in &s.pieces {
                let what = match &p.kind {
                    PieceKind::Copy { packets, .. } => format!("copy   {packets} packets"),
                    PieceKind::Encode {
                        frames,
                        role,
                        enc,
                        filter,
                        ..
                    } => format!(
                        "encode {frames} frames ({role:?}, {}{})",
                        enc.encoder,
                        if filter.is_some() { ", scaled" } else { "" }
                    ),
                };
                println!("      [{:.3} – {:.3}) {what}", p.start, p.end);
            }
        }
    }
}
