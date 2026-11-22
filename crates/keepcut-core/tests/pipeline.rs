//! Integration tests on synthetic media generated with ffmpeg (§12).
//! Skipped (with a message) when ffmpeg/ffprobe are not available.
//!
//! The local-corpus test (`corpus_projects`) is `#[ignore]`d; run it with
//! `cargo test -p keepcut-core --test pipeline -- --ignored`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use keepcut_core::encoders::EncoderSet;
use keepcut_core::exec::{self, ExecOptions};
use keepcut_core::ffmpeg::{self, CancelToken, LogSink, RunOpts, Tools};
use keepcut_core::model::{
    Chapters, Clip, EncodingMode, ExportSettings, Loudness, OutputMode, Project, Quality,
    Resolution, Segment, Soundtrack, VideoCodec,
};
use keepcut_core::plan::{self, PieceKind};
use keepcut_core::probe::{self, MediaInfo};
use keepcut_core::verify::DecodeCheck;

fn tools() -> Option<Tools> {
    match Tools::locate() {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

fn log() -> LogSink {
    Arc::new(|_| {})
}

/// Generates a test clip with ffmpeg; `video` / `audio` are extra args.
fn make_clip(t: &Tools, dir: &Path, name: &str, input: &[&str], out_args: &[&str]) -> PathBuf {
    let p = dir.join(name);
    let cmd = t
        .ffmpeg()
        .args(["-hide_banner", "-nostdin", "-v", "error", "-y"])
        .args(input)
        .args(out_args)
        .arg(&p);
    ffmpeg::run_checked(&cmd, RunOpts::default()).expect("generating test media");
    p
}

const SRC_360: &[&str] = &[
    "-f",
    "lavfi",
    "-i",
    "testsrc2=s=640x360:r=15",
    "-f",
    "lavfi",
    "-i",
    "sine=f=440:r=48000",
];

fn clip(path: &Path, segs: &[(f64, f64)]) -> Clip {
    Clip {
        id: uuid::Uuid::new_v4(),
        path: path.to_path_buf(),
        file_size: 0,
        file_mtime: 0,
        enabled: true,
        segments: Some(segs.iter().map(|(a, b)| Segment::new(*a, *b)).collect()),
        chapter_title: None,
    }
}

/// Plans and exports; asserts verification passed. Returns the plan.
fn export(t: &Tools, out_dir: &Path, clips: Vec<Clip>, encoding: EncodingMode) -> plan::ExportPlan {
    export_with(
        t,
        clips,
        ExportSettings {
            encoding,
            output_dir: Some(out_dir.to_path_buf()),
            ..Default::default()
        },
    )
    .0
}

/// Like [`export`] with explicit settings; also returns the output paths.
fn export_with(
    t: &Tools,
    clips: Vec<Clip>,
    settings: ExportSettings,
) -> (plan::ExportPlan, Vec<PathBuf>) {
    export_project(
        t,
        Project {
            version: 1,
            id: uuid::Uuid::new_v4(),
            clips,
            export: settings,
            soundtrack: None,
        },
    )
}

/// Plans and exports a whole project; asserts verification passed.
fn export_project(t: &Tools, project: Project) -> (plan::ExportPlan, Vec<PathBuf>) {
    let infos: Vec<MediaInfo> = project
        .clips
        .iter()
        .map(|c| probe::probe(t, &c.path, None, None).unwrap())
        .collect();
    let plan = plan::build(&project, &infos, &EncoderSet::software_only()).unwrap();
    let opts = ExecOptions {
        decode_check: DecodeCheck::Full,
        ..Default::default()
    };
    let results = exec::execute(
        t,
        &plan,
        &infos,
        &project.export,
        &opts,
        &CancelToken::new(),
        &log(),
        &mut |_| {},
    )
    .unwrap();
    for r in &results {
        let v = &r.verification;
        assert!(
            v.ok(),
            "{}: {:?} {:?}",
            r.path.display(),
            v.problems,
            v.new_errors
        );
        assert!(
            v.max_timestamp_error < 0.002,
            "timestamps off by {}",
            v.max_timestamp_error
        );
    }
    let paths = results.into_iter().map(|r| r.path).collect();
    (plan, paths)
}

#[test]
fn full_hevc_10bit_downscale_constant_fps() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let hevc10 = make_clip(
        &t,
        dir.path(),
        "hevc10.mp4",
        SRC_360,
        &[
            "-t",
            "6",
            "-c:v",
            "libx265",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p10le",
            "-x265-params",
            "log-level=error",
            "-c:a",
            "aac",
        ],
    );
    let small43 = make_clip(
        &t,
        dir.path(),
        "small43.mp4",
        &["-f", "lavfi", "-i", "testsrc2=s=320x240:r=15"],
        &["-t", "4", "-c:v", "libx264", "-preset", "ultrafast"],
    );
    let mut s = ExportSettings {
        encoding: EncodingMode::Full,
        output_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    s.full.codec = VideoCodec::Hevc;
    s.full.resolution = Resolution::ShortSide(240);
    s.full.fps = Some(25);
    s.full.quality = Quality::Crf(30);
    let (plan, paths) = export_with(
        &t,
        vec![clip(&hevc10, &[(0.5, 5.5)]), clip(&small43, &[(1.0, 3.0)])],
        s,
    );
    assert_eq!(plan.outputs.len(), 1);
    let m = probe::probe(&t, &paths[0], None, None).unwrap();
    let v = m.video.unwrap();
    // target = the 10-bit HEVC clip (longer kept part): 640×360 → 426×240, 10-bit kept
    assert_eq!(
        (v.codec_name.as_str(), v.width, v.height),
        ("hevc", 426, 240)
    );
    assert_eq!(v.pix_fmt, "yuv420p10le");
    assert_eq!(m.frames.len(), 7 * 25, "constant 25 fps over 7 s");
}

#[test]
fn per_clip_and_per_segment_outputs() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let enc = [
        "-t",
        "8",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-bf",
        "0",
        "-g",
        "30",
        "-c:a",
        "aac",
    ];
    let a = make_clip(&t, dir.path(), "a.mp4", SRC_360, &enc);
    let b = make_clip(&t, dir.path(), "b.mp4", SRC_360, &enc);
    let clips = || vec![clip(&a, &[(0.7, 2.1), (3.0, 5.0)]), clip(&b, &[(2.5, 7.9)])];
    let settings = |mode| ExportSettings {
        output_mode: mode,
        output_dir: Some(dir.path().join("out")),
        ..Default::default()
    };
    std::fs::create_dir_all(dir.path().join("out")).unwrap();
    let (_, paths) = export_with(&t, clips(), settings(OutputMode::PerClip));
    let names: Vec<String> = paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["a - cut.mp4", "b - cut.mp4"]);
    let (_, paths) = export_with(&t, clips(), settings(OutputMode::PerSegment));
    let names: Vec<String> = paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["a - part 01.mp4", "a - part 02.mp4", "b - part 01.mp4"]
    );
    // exporting again never overwrites
    let (_, again) = export_with(&t, clips(), settings(OutputMode::PerClip));
    assert!(again[0].ends_with("a - cut (2).mp4"));
}

fn has_copy(plan: &plan::ExportPlan) -> bool {
    plan.outputs
        .iter()
        .flat_map(|o| &o.segments)
        .flat_map(|s| &s.pieces)
        .any(|p| matches!(p.kind, PieceKind::Copy { .. }))
}

#[test]
fn cfr_no_bframes_both_modes_with_mismatch() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let enc = [
        "-t",
        "8",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-bf",
        "0",
        "-g",
        "30",
        "-c:a",
        "aac",
    ];
    let a = make_clip(&t, dir.path(), "a.mp4", SRC_360, &enc);
    let b = make_clip(&t, dir.path(), "b.mp4", SRC_360, &enc);
    let small = make_clip(
        &t,
        dir.path(),
        "small.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x180:r=15",
            "-f",
            "lavfi",
            "-i",
            "sine=f=220:r=44100",
        ],
        &enc,
    );
    let clips = || {
        vec![
            clip(&a, &[(0.7, 5.1)]),
            clip(&b, &[(2.5, 7.9)]),
            clip(&small, &[(1.0, 3.0)]),
        ]
    };
    let plan = export(&t, dir.path(), clips(), EncodingMode::Smart);
    assert!(has_copy(&plan));
    assert!(!plan.clips[2].copyable, "320x180 clip must be re-encoded");
    export(&t, dir.path(), clips(), EncodingMode::Full);
}

#[test]
fn vfr_screen_like() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    // mpdecimate drops near-duplicate frames → VFR, like Bandicam's frame skipping
    let a = make_clip(
        &t,
        dir.path(),
        "vfr.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=15,select='not(mod(floor(t*2),3))+eq(mod(n,7),0)',mpdecimate",
            "-f",
            "lavfi",
            "-i",
            "sine=r=48000",
        ],
        &[
            "-t",
            "10",
            "-fps_mode",
            "vfr",
            "-video_track_timescale",
            "15000",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-bf",
            "0",
            "-g",
            "40",
            "-c:a",
            "aac",
        ],
    );
    let plan = export(
        &t,
        dir.path(),
        vec![clip(&a, &[(1.2, 4.4), (5.0, 9.5)])],
        EncodingMode::Smart,
    );
    assert!(has_copy(&plan));
}

#[test]
fn bframes_head_and_tail() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = make_clip(
        &t,
        dir.path(),
        "bf.mp4",
        SRC_360,
        &[
            "-t", "8", "-c:v", "libx264", "-preset", "fast", "-bf", "3", "-g", "15", "-c:a", "aac",
        ],
    );
    let plan = export(
        &t,
        dir.path(),
        vec![clip(&a, &[(0.6, 3.3), (4.1, 6.9)])],
        EncodingMode::Smart,
    );
    let roles: Vec<bool> = plan.outputs[0].segments[0]
        .pieces
        .iter()
        .map(|p| p.is_copy())
        .collect();
    assert_eq!(roles, [false, true, false], "head, body, tail expected");
}

#[test]
fn hevc_open_gop() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = make_clip(
        &t,
        dir.path(),
        "hevc.mp4",
        SRC_360,
        &[
            "-t",
            "8",
            "-c:v",
            "libx265",
            "-preset",
            "ultrafast",
            "-x265-params",
            "keyint=30:min-keyint=30:bframes=3:open-gop=1:log-level=error",
            "-c:a",
            "aac",
        ],
    );
    let plan = export(
        &t,
        dir.path(),
        vec![clip(&a, &[(0.5, 4.3), (5.2, 8.0)])],
        EncodingMode::Smart,
    );
    assert!(has_copy(&plan));
}

#[test]
fn no_audio_and_two_audio_tracks() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let enc = [
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-bf",
        "0",
        "-g",
        "30",
    ];
    let silent = make_clip(
        &t,
        dir.path(),
        "silent.mp4",
        &["-f", "lavfi", "-i", "testsrc2=s=640x360:r=15"],
        &[&["-t", "6"][..], &enc].concat(),
    );
    let two = make_clip(
        &t,
        dir.path(),
        "two.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=15",
            "-f",
            "lavfi",
            "-i",
            "sine=f=300:r=44100",
            "-f",
            "lavfi",
            "-i",
            "sine=f=500:r=48000",
        ],
        &[
            &["-t", "6", "-map", "0", "-map", "1", "-map", "2"][..],
            &enc,
            &["-c:a", "aac"],
        ]
        .concat(),
    );
    export(
        &t,
        dir.path(),
        vec![clip(&silent, &[(0.4, 3.0)]), clip(&two, &[(1.3, 5.5)])],
        EncodingMode::Smart,
    );
}

/// Integrated loudness of `[start, start + dur)` of a file.
fn lufs(t: &Tools, file: &Path, start: f64, dur: f64) -> f64 {
    band_lufs(t, file, start, dur, "anull")
}

/// Like [`lufs`], after the audio filter `filter` (e.g. a low-pass, to tell tones apart).
fn band_lufs(t: &Tools, file: &Path, start: f64, dur: f64, filter: &str) -> f64 {
    let cmd = t
        .ffmpeg()
        .args(["-hide_banner", "-nostdin", "-ss"])
        .arg(format!("{start}"))
        .arg("-t")
        .arg(format!("{dur}"))
        .arg("-i")
        .arg(file)
        .arg("-af")
        .arg(format!("{filter},ebur128=framelog=quiet"))
        .args(["-f", "null", "-"]);
    let o = ffmpeg::run_checked(&cmd, RunOpts::default()).unwrap();
    exec::parse_ebur128(&o.stderr).unwrap()
}

/// A joined export with a soundtrack: the clips' 440 Hz tone is replaced by the soundtrack
/// from its start offset, cut at the end of the video, or followed by silence when it is
/// shorter; loudness falls back to "whole output"; separate outputs keep their own audio.
#[test]
fn soundtrack_replaces_the_audio() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let src = make_clip(
        &t,
        dir.path(),
        "a.mp4",
        SRC_360,
        &[
            "-t",
            "8",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-bf",
            "0",
            "-g",
            "30",
            "-c:a",
            "aac",
        ],
    );
    // 1 kHz for 5 s, then 2.5 kHz: with offset 5 only the 2.5 kHz part may be heard
    let music = |name: &str, secs: u32| {
        let source = format!(
            "aevalsrc='if(lt(t,5),sin(2*PI*1000*t),sin(2*PI*2500*t))*0.3':s=44100:d={secs}"
        );
        make_clip(
            &t,
            dir.path(),
            name,
            &["-f", "lavfi", "-i", &source],
            &["-c:a", "aac"],
        )
    };
    let long = music("long.m4a", 30);
    let short = music("short.m4a", 9);
    let clips = || {
        vec![
            clip(&src, &[(0.0, 3.0), (4.0, 7.0)]),
            clip(&src, &[(1.0, 6.0)]),
        ]
    };
    let project = |st: &Path, loudness| Project {
        version: 1,
        id: uuid::Uuid::new_v4(),
        clips: clips(),
        export: ExportSettings {
            output_dir: Some(dir.path().to_path_buf()),
            output_name: Some(format!("{}.mp4", st.file_stem().unwrap().to_string_lossy())),
            loudness,
            loudness_target: -16.0,
            ..Default::default()
        },
        soundtrack: Some(Soundtrack {
            path: st.to_path_buf(),
            offset: 5.0,
        }),
    };
    let low = "lowpass=f=700,lowpass=f=700"; // the clips' 440 Hz
    let mid = "bandpass=f=1000:w=300,bandpass=f=1000:w=300"; // the skipped part
    let high = "highpass=f=2000,highpass=f=2000"; // the soundtrack after the offset

    // longer soundtrack, per-clip loudness requested: cut at 11 s, normalized as a whole
    let (plan, paths) = export_project(&t, project(&long, Loudness::PerClip));
    assert!(plan.outputs[0].soundtrack.is_some());
    let out = &paths[0];
    let whole = lufs(&t, out, 0.0, 11.0);
    assert!((whole + 16.0).abs() < 1.0, "loudness {whole}");
    assert!(band_lufs(&t, out, 0.0, 11.0, high) > -25.0);
    assert!(
        band_lufs(&t, out, 0.0, 11.0, low) < -50.0,
        "clip audio left"
    );
    assert!(band_lufs(&t, out, 0.0, 11.0, mid) < -40.0, "offset ignored");

    // shorter soundtrack (4 s left after the offset): silence after it
    let (_, paths) = export_project(&t, project(&short, Loudness::Off));
    let out = &paths[0];
    assert!(band_lufs(&t, out, 0.0, 3.8, high) > -25.0);
    assert!(
        lufs(&t, out, 4.3, 6.5) < -65.0,
        "not silent after the soundtrack"
    );

    // separate outputs keep the clips' audio
    let mut p = project(&long, Loudness::Off);
    p.export.output_mode = OutputMode::PerClip;
    let infos: Vec<MediaInfo> = p
        .clips
        .iter()
        .map(|c| probe::probe(&t, &c.path, None, None).unwrap())
        .collect();
    let plan = plan::build(&p, &infos, &EncoderSet::software_only()).unwrap();
    assert!(plan.outputs.iter().all(|o| o.soundtrack.is_none()));
    assert!(plan.warnings.iter().any(|w| w.contains("soundtrack")));
}

#[test]
fn loudness_per_clip_and_chapters() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let enc = [
        "-t",
        "8",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-bf",
        "0",
        "-g",
        "30",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
    ];
    let tone = |name: &str, db: i32| {
        let src = format!("sine=f=440:r=48000,volume={db}dB");
        make_clip(
            &t,
            dir.path(),
            name,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=640x360:r=15",
                "-f",
                "lavfi",
                "-i",
                &src,
            ],
            &enc,
        )
    };
    let quiet = tone("quiet.mp4", -14);
    let loud = tone("loud.mp4", 10);
    let mut first = clip(&quiet, &[(0.0, 3.0), (4.0, 7.0)]);
    first.chapter_title = Some("Intro".into());
    let clips = vec![first, clip(&loud, &[(1.0, 6.0)])];
    let settings = ExportSettings {
        output_dir: Some(dir.path().to_path_buf()),
        loudness: Loudness::PerClip,
        loudness_target: -16.0,
        chapters: Chapters::PerClip,
        ..Default::default()
    };
    let before = (lufs(&t, &quiet, 0.0, 8.0), lufs(&t, &loud, 0.0, 8.0));
    assert!(
        before.0 < -28.0 && before.1 > -13.0,
        "test tones: {before:?}"
    );
    let (_, paths) = export_with(&t, clips, settings);
    let out = &paths[0];
    // each clip's part lands near the target (the limiter only touches peaks)
    let (a, b) = (lufs(&t, out, 0.2, 5.6), lufs(&t, out, 6.2, 4.6));
    assert!((a + 16.0).abs() < 1.0, "quiet clip: {a} LUFS");
    assert!((b + 16.0).abs() < 1.0, "loud clip: {b} LUFS");
    // chapters: one per clip, the first with its custom title
    let cmd = t
        .ffprobe()
        .args([
            "-v",
            "error",
            "-show_entries",
            "chapter=start_time,end_time:chapter_tags=title",
            "-of",
            "csv=p=0",
        ])
        .arg(out);
    let o = ffmpeg::run_checked(&cmd, RunOpts::default()).unwrap();
    let lines: Vec<String> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines,
        ["0.000000,6.000000,Intro", "6.000000,11.000000,loud"],
        "{lines:?}"
    );
}

#[test]
fn thumbnail_and_waveform() {
    use keepcut_core::assets;
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    // 2 s of silence, then a tone; two audio tracks (mixed for the waveform)
    let a = make_clip(
        &t,
        dir.path(),
        "w.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x240:r=15",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc='if(gte(t,2),0.5*sin(2*PI*440*t),0)':s=48000",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=44100:cl=mono",
        ],
        &[
            "-t",
            "4",
            "-map",
            "0",
            "-map",
            "1",
            "-map",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "30",
            "-c:a",
            "aac",
        ],
    );
    let info = probe::probe(&t, &a, None, None).unwrap();
    let rgba = assets::thumbnail(&t, &info).unwrap();
    assert_eq!(rgba.len(), (assets::THUMB_W * assets::THUMB_H * 4) as usize);
    // 4:3 in 16:9: black bars left and right, picture in the middle
    assert_eq!(&rgba[..3], &[0, 0, 0]);
    let mid = ((assets::THUMB_H / 2 * assets::THUMB_W + assets::THUMB_W / 2) * 4) as usize;
    assert!(
        rgba[mid..mid + 3].iter().any(|&c| c > 20),
        "picture in the middle"
    );
    let peaks = assets::waveform(&t, &info, &CancelToken::new()).unwrap();
    assert!(
        (395..=405).contains(&peaks.len()),
        "4 s at 100/s: {}",
        peaks.len()
    );
    assert!(peaks[20..180].iter().all(|&p| p < 3), "silence first");
    assert!(
        peaks[220..380].iter().all(|&p| p > 100),
        "then the tone (0.5 ≈ 127)"
    );
}

/// Runs every `samples/projects/*.json` in both modes (local corpus; slow).
#[test]
#[ignore]
fn corpus_projects() {
    let Some(t) = tools() else { return };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/projects");
    let dir = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(&root).expect("samples/projects") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let mut p: Project =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for c in &mut p.clips {
            c.path = std::path::absolute(root.join(&c.path)).unwrap();
        }
        for mode in [EncodingMode::Smart, EncodingMode::Full] {
            eprintln!("{} {:?}", path.display(), mode);
            export(&t, dir.path(), p.clips.clone(), mode);
        }
    }
}

#[test]
fn av1_nut_intermediates() {
    let Some(t) = tools() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = make_clip(
        &t,
        dir.path(),
        "av1.mp4",
        SRC_360,
        &[
            "-t",
            "6",
            "-c:v",
            "libaom-av1",
            "-cpu-used",
            "8",
            "-usage",
            "realtime",
            "-crf",
            "35",
            "-g",
            "30",
            "-c:a",
            "aac",
        ],
    );
    let plan = export(
        &t,
        dir.path(),
        vec![clip(&a, &[(0.7, 4.4), (4.8, 6.0)])],
        EncodingMode::Smart,
    );
    assert!(has_copy(&plan));
    assert_eq!(plan.outputs[0].intermediate().0, "nut");
}
