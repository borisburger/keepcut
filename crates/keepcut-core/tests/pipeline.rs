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
use keepcut_core::model::{Clip, EncodingMode, ExportSettings, Project, Segment};
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
    let infos: Vec<MediaInfo> = clips
        .iter()
        .map(|c| probe::probe(t, &c.path, None, None).unwrap())
        .collect();
    let project = Project {
        version: 1,
        id: uuid::Uuid::new_v4(),
        clips,
        export: ExportSettings {
            encoding,
            output_dir: Some(out_dir.to_path_buf()),
            ..Default::default()
        },
    };
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
    plan
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
