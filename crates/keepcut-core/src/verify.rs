//! Post-export checks (§6.11).

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::ffmpeg::{self, CancelToken, LogSink, RunOpts, Tools};
use crate::probe::{self, Rational};
use crate::timefmt::ff_secs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DecodeCheck {
    /// Decode ±2 s around every seam.
    #[default]
    Seams,
    /// Decode the whole output (slow).
    Full,
    None,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Verification {
    pub expected_duration: f64,
    pub video_duration: f64,
    pub audio_duration: f64,
    pub expected_frames: usize,
    pub frames: usize,
    /// Decoder messages not produced by the sources themselves.
    pub new_errors: Vec<String>,
    /// Largest difference between output and expected frame timestamps (seconds).
    pub max_timestamp_error: f64,
    pub problems: Vec<String>,
}

impl Verification {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// What the verifier needs to know about the plan of one output.
pub struct Expect<'a> {
    pub duration: f64,
    pub frames: usize,
    /// Output times where pieces join.
    pub seams: Vec<f64>,
    /// Largest frame delta of the sources (gap tolerance).
    pub max_frame_delta: f64,
    /// Source ranges `(path, start, end)` used for the decoder-message baseline.
    pub sources: Vec<(&'a Path, f64, f64)>,
    /// Expected presentation times of all output frames (passthrough timing only).
    pub frame_times: Option<Vec<f64>>,
}

fn stream_durations(tools: &Tools, out: &Path, log: Option<&LogSink>) -> Result<(f64, f64)> {
    let cmd = tools
        .ffprobe()
        .args([
            "-v",
            "error",
            "-of",
            "json",
            "-show_entries",
            "stream=codec_type,duration",
        ])
        .arg(out);
    let o = ffmpeg::run_checked(
        &cmd,
        RunOpts {
            log,
            ..Default::default()
        },
    )?;
    let j: Value = serde_json::from_slice(&o.stdout)?;
    let mut v = 0.0;
    let mut a = 0.0;
    for s in j["streams"].as_array().into_iter().flatten() {
        let d: f64 = s["duration"]
            .as_str()
            .and_then(|x| x.parse().ok())
            .unwrap_or(0.0);
        match s["codec_type"].as_str() {
            Some("video") if v == 0.0 => v = d,
            Some("audio") if a == 0.0 => a = d,
            _ => {}
        }
    }
    Ok((v, a))
}

/// Strips the `[h264 @ 0000020a…]` address so messages from different runs compare equal.
fn normalize(line: &str) -> String {
    let mut s = String::new();
    let mut rest = line.trim();
    while let Some(i) = rest.find(" @ ") {
        s.push_str(&rest[..i]);
        let tail = &rest[i + 3..];
        let end = tail.find(']').unwrap_or(tail.len());
        rest = &tail[end..];
    }
    s.push_str(rest);
    s
}

fn decode_messages(
    tools: &Tools,
    file: &Path,
    range: Option<(f64, f64)>,
    cancel: Option<&CancelToken>,
    log: Option<&LogSink>,
) -> Result<(bool, Vec<String>)> {
    let mut cmd = tools
        .ffmpeg()
        .args(["-hide_banner", "-nostdin", "-v", "error"]);
    if let Some((a, b)) = range {
        cmd = cmd
            .arg("-ss")
            .arg(ff_secs(a.max(0.0)))
            .arg("-t")
            .arg(ff_secs(b - a.max(0.0)));
    }
    cmd = cmd.arg("-i").arg(file).args([
        "-fps_mode",
        "passthrough",
        "-enc_time_base:v",
        "demux",
        "-f",
        "null",
        "-",
    ]);
    let o = ffmpeg::run(
        &cmd,
        RunOpts {
            cancel,
            log,
            ..Default::default()
        },
    )?;
    Ok((
        o.success(),
        o.stderr
            .lines()
            .map(normalize)
            .filter(|l| !l.is_empty())
            .collect(),
    ))
}

pub fn verify(
    tools: &Tools,
    out: &Path,
    expect: &Expect<'_>,
    check: DecodeCheck,
    cancel: Option<&CancelToken>,
    log: Option<&LogSink>,
) -> Result<Verification> {
    let mut v = Verification {
        expected_duration: expect.duration,
        expected_frames: expect.frames,
        ..Default::default()
    };
    let (vd, ad) = stream_durations(tools, out, log)?;
    v.video_duration = vd;
    v.audio_duration = ad;
    // the final mux sets the last frame duration exactly, so allow only rounding
    let frame_tol = 0.002;
    if (vd - expect.duration).abs() > frame_tol {
        v.problems.push(format!(
            "video duration {:.3} s differs from the expected {:.3} s",
            vd, expect.duration
        ));
    }
    if (ad - vd).abs() > 0.030 {
        v.problems.push(format!(
            "audio {:.3} s vs video {:.3} s (A/V difference {:.0} ms)",
            ad,
            vd,
            (ad - vd) * 1000.0
        ));
    }

    // Frame index of the output: count, monotonic, seam gaps
    let cmd = tools
        .ffprobe()
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=time_base",
            "-of",
            "csv=p=0",
        ])
        .arg(out);
    let tb = ffmpeg::run_checked(
        &cmd,
        RunOpts {
            log,
            ..Default::default()
        },
    )?;
    let tb = Rational::parse(String::from_utf8_lossy(&tb.stdout).trim())
        .unwrap_or(Rational { num: 1, den: 90000 });
    let cmd = tools
        .ffprobe()
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts,dts,duration,flags",
            "-of",
            "compact=p=0",
        ])
        .arg(out);
    let o = ffmpeg::run_checked(
        &cmd,
        RunOpts {
            log,
            quiet_stderr: true,
            ..Default::default()
        },
    )?;
    let fi = probe::parse_packets(&String::from_utf8_lossy(&o.stdout), tb, 0.0);
    v.frames = fi.len();
    if v.frames != expect.frames {
        v.problems.push(format!(
            "{} video frames, expected {}",
            v.frames, expect.frames
        ));
    }
    let dupes = fi
        .frames
        .windows(2)
        .filter(|w| w[1].t - w[0].t < 1e-6)
        .count();
    if dupes > 0 {
        v.problems
            .push(format!("{dupes} duplicate video timestamps"));
    }
    let origin = fi.frames.first().map_or(0.0, |f| f.t);
    if let Some(exp) = expect.frame_times.as_ref().filter(|e| e.len() == fi.len()) {
        let worst = exp
            .iter()
            .zip(&fi.frames)
            .map(|(e, f)| ((f.t - origin) - e).abs())
            .fold(0.0_f64, f64::max);
        v.max_timestamp_error = worst;
        if worst > 0.002 {
            v.problems.push(format!(
                "frame timestamps deviate from the source by up to {:.1} ms",
                worst * 1000.0
            ));
        }
    }
    for &t in &expect.seams {
        let i = fi.idx_at_or_after(origin + t - 1e-3);
        if i > 0 && i < fi.len() {
            let gap = fi.frames[i].t - fi.frames[i - 1].t;
            if gap > 2.0 * expect.max_frame_delta + 1e-3 {
                v.problems
                    .push(format!("timestamp gap of {:.3} s at seam {:.3} s", gap, t));
            }
        }
    }

    // Decoder messages vs. the sources' own baseline
    if check != DecodeCheck::None {
        let mut baseline = BTreeSet::new();
        for (src, a, b) in &expect.sources {
            let (_, msgs) = decode_messages(tools, src, Some((*a, *b)), cancel, log)?;
            baseline.extend(msgs);
        }
        let mut found = Vec::new();
        let mut failed = false;
        match check {
            DecodeCheck::Full => {
                let (ok, msgs) = decode_messages(tools, out, None, cancel, log)?;
                failed |= !ok;
                found.extend(msgs);
            }
            _ => {
                for &t in &expect.seams {
                    let (ok, msgs) =
                        decode_messages(tools, out, Some((t - 2.0, t + 2.0)), cancel, log)?;
                    failed |= !ok;
                    found.extend(msgs);
                }
            }
        }
        let new: BTreeSet<String> = found
            .into_iter()
            .filter(|m| !baseline.contains(m))
            .collect();
        v.new_errors = new.into_iter().collect();
        if failed {
            v.problems.push("decoder exited with an error".into());
        }
        if !v.new_errors.is_empty() {
            v.problems
                .push(format!("{} new decoder message(s)", v.new_errors.len()));
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalizes_addresses() {
        assert_eq!(
            normalize("[hevc @ 000001f2a3b4c5d6] VPS 0 does not exist"),
            normalize("[hevc @ 0000aaaa] VPS 0 does not exist")
        );
    }
}
