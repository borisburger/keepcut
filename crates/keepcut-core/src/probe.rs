//! ffprobe → [`MediaInfo`] and the per-file frame index.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::ffmpeg::{self, CancelToken, LogSink, RunOpts, Tools};

/// A rational number such as a time base or a frame rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rational {
    pub num: i64,
    pub den: i64,
}

impl Rational {
    pub fn parse(s: &str) -> Option<Rational> {
        let (a, b) = s.split_once(['/', ':'])?;
        Some(Rational {
            num: a.trim().parse().ok()?,
            den: b.trim().parse().ok()?,
        })
    }
    pub fn as_f64(self) -> f64 {
        if self.den == 0 {
            0.0
        } else {
            self.num as f64 / self.den as f64
        }
    }
}

impl std::fmt::Display for Rational {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoStream {
    pub index: u32,
    pub codec_name: String,
    pub profile: Option<String>,
    pub level: Option<i64>,
    pub width: u32,
    pub height: u32,
    /// Sample aspect ratio as reported (`1:1`, `N/A` mapped to `1:1`).
    pub sar: String,
    pub pix_fmt: String,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub color_space: Option<String>,
    pub color_range: Option<String>,
    pub field_order: Option<String>,
    pub time_base: Rational,
    pub r_frame_rate: Rational,
    pub avg_frame_rate: Rational,
    pub has_b_frames: u32,
    /// Display rotation in degrees (counter-clockwise, as ffprobe reports it).
    pub rotation: i32,
    pub bit_rate: Option<u64>,
    pub encoder: Option<String>,
}

impl VideoStream {
    pub fn is_hdr(&self) -> bool {
        matches!(
            self.color_transfer.as_deref(),
            Some("smpte2084" | "arib-std-b67")
        )
    }
    pub fn bit_depth(&self) -> u32 {
        if self.pix_fmt.contains("10") {
            10
        } else if self.pix_fmt.contains("12") {
            12
        } else {
            8
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStream {
    pub index: u32,
    pub codec_name: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub channel_layout: Option<String>,
}

/// One video frame (packet), in presentation order.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Frame {
    /// Seconds relative to the file start (`pts_time - format.start_time`).
    pub t: f64,
    pub key: bool,
    /// Decode timestamp, relative seconds.
    pub dts: f64,
    /// Position of this packet in decode order.
    pub decode_idx: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FrameIndex {
    /// Sorted by `t`.
    pub frames: Vec<Frame>,
    /// Decode order: `decode[k]` is the index into `frames` of the k-th packet.
    #[serde(default)]
    pub decode: Vec<u32>,
    /// End of the last frame (its pts + duration), relative seconds.
    pub end: f64,
}

/// Tolerance used when comparing times that should be frame timestamps.
pub const EPS: f64 = 1e-5;

impl FrameIndex {
    pub fn len(&self) -> usize {
        self.frames.len()
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
    /// Index of the first frame with `t >= time` (within [`EPS`]); `len()` if none.
    pub fn idx_at_or_after(&self, time: f64) -> usize {
        self.frames.partition_point(|f| f.t < time - EPS)
    }
    /// Time of frame `i`, or the end time for `i == len()`.
    pub fn time_of(&self, i: usize) -> f64 {
        self.frames.get(i).map_or(self.end, |f| f.t)
    }
    /// Snaps to the nearest frame start (or the end of the clip).
    pub fn snap(&self, time: f64) -> f64 {
        let i = self.idx_at_or_after(time);
        let after = self.time_of(i);
        if i == 0 {
            return after;
        }
        let before = self.frames[i - 1].t;
        if (time - before) <= (after - time) {
            before
        } else {
            after
        }
    }
    /// Number of frames with `t` in `[a, b)`.
    pub fn count_in(&self, a: f64, b: f64) -> usize {
        self.idx_at_or_after(b)
            .saturating_sub(self.idx_at_or_after(a))
    }
    /// The time of the frame before `time`, if any.
    pub fn prev_frame_time(&self, time: f64) -> Option<f64> {
        let i = self.idx_at_or_after(time);
        (i > 0).then(|| self.frames[i - 1].t)
    }
    pub fn is_key_at(&self, time: f64) -> bool {
        let i = self.idx_at_or_after(time);
        self.frames
            .get(i)
            .is_some_and(|f| f.key && (f.t - time).abs() < EPS)
    }
    /// First keyframe with `t >= time`.
    pub fn next_key_at_or_after(&self, time: f64) -> Option<f64> {
        let i = self.idx_at_or_after(time);
        self.frames[i..].iter().find(|f| f.key).map(|f| f.t)
    }
    /// Last keyframe with `t <= time`.
    pub fn last_key_at_or_before(&self, time: f64) -> Option<f64> {
        let i = self.frames.partition_point(|f| f.t <= time + EPS);
        self.frames[..i].iter().rev().find(|f| f.key).map(|f| f.t)
    }
    pub fn keyframes(&self) -> impl Iterator<Item = f64> + '_ {
        self.frames.iter().filter(|f| f.key).map(|f| f.t)
    }
    /// Number of packets in decode order from the packet of the frame at `a`
    /// up to (excluding) the packet of the frame at `b` (or the end).
    pub fn decode_count(&self, a: f64, b: f64) -> usize {
        let ia = self.idx_at_or_after(a);
        let ib = self.idx_at_or_after(b);
        let da = self.frames.get(ia).map_or(0, |f| f.decode_idx as usize);
        let db = self
            .frames
            .get(ib)
            .map_or(self.frames.len(), |f| f.decode_idx as usize);
        db.saturating_sub(da)
    }
    /// Leading pictures of the keyframe at `key` (open GOP): the packets right
    /// after it in decode order that are displayed before it. Returns their count
    /// and the earliest of their times (`key` if there are none).
    pub fn leading(&self, key: f64) -> (usize, f64) {
        let i = self.idx_at_or_after(key);
        let Some(f) = self.frames.get(i) else {
            return (0, key);
        };
        let mut n = 0;
        let mut min_t = key;
        for &j in self.decode.iter().skip(f.decode_idx as usize + 1) {
            let t = self.frames[j as usize].t;
            if t >= key - EPS {
                break;
            }
            n += 1;
            min_t = min_t.min(t);
        }
        (n, min_t)
    }
    /// Largest gap between consecutive frames.
    pub fn max_frame_delta(&self) -> f64 {
        self.frames
            .windows(2)
            .map(|w| w[1].t - w[0].t)
            .fold(0.0, f64::max)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub file_size: u64,
    pub file_mtime: i64,
    pub format_name: String,
    pub duration: f64,
    pub start_time: f64,
    pub bit_rate: Option<u64>,
    pub creation_time: Option<String>,
    pub encoder: Option<String>,
    pub video: Option<VideoStream>,
    pub audio: Vec<AudioStream>,
    pub data_streams: u32,
    pub frames: FrameIndex,
}

impl MediaInfo {
    /// Duration of the video in relative seconds (end of last frame).
    pub fn video_end(&self) -> f64 {
        if self.frames.is_empty() {
            self.duration
        } else {
            self.frames.end
        }
    }
    pub fn file_stem(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    /// Recording time (unix seconds) for default sorting: `creation_time` tag,
    /// then a Bandicam file name timestamp, then the file mtime.
    pub fn recording_time(&self) -> f64 {
        if let Some(t) = self.creation_time.as_deref().and_then(parse_iso8601) {
            return t;
        }
        if let Some(t) = parse_bandicam_name(&self.file_stem()) {
            return t;
        }
        self.file_mtime as f64
    }
}

fn str_field(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| s != "unknown" && s != "N/A")
}

fn num_field<T: std::str::FromStr>(v: &Value, k: &str) -> Option<T> {
    match v.get(k)? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.to_string().parse().ok(),
        _ => None,
    }
}

/// Probes streams/format and builds the frame index.
pub fn probe(
    tools: &Tools,
    path: &Path,
    cancel: Option<&CancelToken>,
    log: Option<&LogSink>,
) -> Result<MediaInfo> {
    let perr = |msg: String| Error::Probe {
        path: path.to_path_buf(),
        msg,
    };
    let meta = std::fs::metadata(path).map_err(|e| perr(e.to_string()))?;
    let file_mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);

    let cmd = tools
        .ffprobe()
        .args([
            "-v",
            "error",
            "-of",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path);
    let out = ffmpeg::run_checked(
        &cmd,
        RunOpts {
            cancel,
            log,
            ..Default::default()
        },
    )
    .map_err(|e| perr(e.to_string()))?;
    let json: Value = serde_json::from_slice(&out.stdout)?;
    let mut info = parse_probe_json(&json).map_err(perr)?;
    info.path = path.to_path_buf();
    info.file_size = meta.len();
    info.file_mtime = file_mtime;

    if let Some(v) = &info.video {
        let cmd = tools
            .ffprobe()
            .args(["-v", "error", "-select_streams"])
            .arg(format!("{}", v.index))
            .args([
                "-show_entries",
                "packet=pts,dts,duration,flags",
                "-of",
                "compact=p=0",
            ])
            .arg(path);
        let out = ffmpeg::run_checked(
            &cmd,
            RunOpts {
                cancel,
                log,
                quiet_stderr: true,
                ..Default::default()
            },
        )
        .map_err(|e| perr(e.to_string()))?;
        info.frames = parse_packets(
            &String::from_utf8_lossy(&out.stdout),
            v.time_base,
            info.start_time,
        );
    }
    Ok(info)
}

/// Parses `ffprobe -show_format -show_streams -of json` output (without the frame index).
pub fn parse_probe_json(json: &Value) -> std::result::Result<MediaInfo, String> {
    let format = json.get("format").ok_or("no format section")?;
    let format_tags = format.get("tags").cloned().unwrap_or(Value::Null);
    let mut info = MediaInfo {
        path: PathBuf::new(),
        file_size: 0,
        file_mtime: 0,
        format_name: str_field(format, "format_name").unwrap_or_default(),
        duration: num_field(format, "duration").unwrap_or(0.0),
        start_time: num_field(format, "start_time").unwrap_or(0.0),
        bit_rate: num_field(format, "bit_rate"),
        creation_time: str_field(&format_tags, "creation_time"),
        encoder: str_field(&format_tags, "encoder"),
        video: None,
        audio: Vec::new(),
        data_streams: 0,
        frames: FrameIndex::default(),
    };
    let streams = json
        .get("streams")
        .and_then(Value::as_array)
        .ok_or("no streams")?;
    for s in streams {
        let index: u32 = num_field(s, "index").unwrap_or(0);
        let disposition_pic = s
            .get("disposition")
            .and_then(|d| d.get("attached_pic"))
            .and_then(Value::as_i64)
            == Some(1);
        match s.get("codec_type").and_then(Value::as_str) {
            Some("video") if info.video.is_none() && !disposition_pic => {
                let tags = s.get("tags").cloned().unwrap_or(Value::Null);
                let rotation = s
                    .get("side_data_list")
                    .and_then(Value::as_array)
                    .and_then(|l| l.iter().find_map(|sd| num_field::<f64>(sd, "rotation")))
                    .map_or(0, |r| r.round() as i32);
                let rat = |k: &str| str_field(s, k).and_then(|x| Rational::parse(&x));
                info.video = Some(VideoStream {
                    index,
                    codec_name: str_field(s, "codec_name").unwrap_or_default(),
                    profile: str_field(s, "profile"),
                    level: num_field(s, "level"),
                    width: num_field(s, "width").unwrap_or(0),
                    height: num_field(s, "height").unwrap_or(0),
                    sar: str_field(s, "sample_aspect_ratio")
                        .filter(|x| x != "0:1")
                        .unwrap_or_else(|| "1:1".into()),
                    pix_fmt: str_field(s, "pix_fmt").unwrap_or_default(),
                    color_primaries: str_field(s, "color_primaries"),
                    color_transfer: str_field(s, "color_transfer"),
                    color_space: str_field(s, "color_space"),
                    color_range: str_field(s, "color_range"),
                    field_order: str_field(s, "field_order"),
                    time_base: rat("time_base").ok_or("video stream without time_base")?,
                    r_frame_rate: rat("r_frame_rate").unwrap_or(Rational { num: 0, den: 1 }),
                    avg_frame_rate: rat("avg_frame_rate").unwrap_or(Rational { num: 0, den: 1 }),
                    has_b_frames: num_field(s, "has_b_frames").unwrap_or(0),
                    rotation,
                    bit_rate: num_field(s, "bit_rate"),
                    encoder: str_field(&tags, "encoder"),
                });
            }
            Some("audio") => info.audio.push(AudioStream {
                index,
                codec_name: str_field(s, "codec_name").unwrap_or_default(),
                sample_rate: num_field(s, "sample_rate").unwrap_or(0),
                channels: num_field(s, "channels").unwrap_or(0),
                channel_layout: str_field(s, "channel_layout"),
            }),
            Some("data") => info.data_streams += 1,
            _ => {}
        }
    }
    Ok(info)
}

/// Parses `-show_entries packet=pts,dts,duration,flags -of compact=p=0` lines.
pub fn parse_packets(text: &str, tb: Rational, start_time: f64) -> FrameIndex {
    struct P {
        pts: Option<i64>,
        dts: Option<i64>,
        dur: Option<i64>,
        key: bool,
        discard: bool,
    }
    let mut packets = Vec::new();
    for line in text.lines() {
        let mut p = P {
            pts: None,
            dts: None,
            dur: None,
            key: false,
            discard: false,
        };
        for kv in line.split('|') {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            match k {
                "pts" => p.pts = v.parse().ok(),
                "dts" => p.dts = v.parse().ok(),
                "duration" => p.dur = v.parse().ok(),
                "flags" => {
                    p.key = v.contains('K');
                    p.discard = v.contains('D');
                }
                _ => {}
            }
        }
        if p.pts.is_some() || p.dts.is_some() {
            packets.push(p);
        }
    }
    let tbf = tb.as_f64();
    let mut frames = Vec::with_capacity(packets.len());
    let mut end_ticks: i64 = i64::MIN;
    for p in packets.iter().filter(|p| !p.discard) {
        let pts = p.pts.or(p.dts).unwrap_or(0);
        end_ticks = end_ticks.max(pts + p.dur.unwrap_or(0));
        let dts = p.dts.unwrap_or(pts);
        let decode_idx = frames.len() as u32;
        frames.push(Frame {
            t: pts as f64 * tbf - start_time,
            dts: dts as f64 * tbf - start_time,
            key: p.key,
            decode_idx,
        });
    }
    frames.sort_by(|a, b| a.t.total_cmp(&b.t));
    let mut decode = vec![0u32; frames.len()];
    for (i, f) in frames.iter().enumerate() {
        decode[f.decode_idx as usize] = i as u32;
    }
    let end = if frames.is_empty() {
        0.0
    } else {
        end_ticks as f64 * tbf - start_time
    };
    FrameIndex {
        frames,
        decode,
        end,
    }
}

/// `2025-05-19T14:58:11.000000Z` → unix seconds.
pub fn parse_iso8601(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once(['T', ' '])?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let sec: f64 = t.next().unwrap_or("0").parse().ok()?;
    if y < 1971 {
        return None; // 1904/1970 epochs mean "unset"
    }
    Some((days_from_civil(y, mo, da) * 86400 + h * 3600 + mi * 60) as f64 + sec)
}

/// `bandicam 2025-05-19 16-58-11-314` → unix seconds (local time treated as UTC;
/// only used for relative ordering).
pub fn parse_bandicam_name(stem: &str) -> Option<f64> {
    let rest = stem.strip_prefix("bandicam ")?;
    let (date, time) = rest.split_once(' ')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let t: Vec<i64> = time
        .split('-')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    if t.len() < 3 {
        return None;
    }
    let ms = t.get(3).copied().unwrap_or(0);
    Some(
        (days_from_civil(y, mo, da) * 86400 + t[0] * 3600 + t[1] * 60 + t[2]) as f64
            + ms as f64 / 1000.0,
    )
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx() -> FrameIndex {
        // B-frame stream: decode order I P B B (pts 0, 3, 1, 2) then I at 4
        let txt = "pts=0|dts=-2|duration=1|flags=K__\npts=3|dts=-1|duration=1|flags=___\n\
                   pts=1|dts=0|duration=1|flags=___\npts=2|dts=1|duration=1|flags=___\n\
                   pts=4|dts=2|duration=1|flags=K__\npts=5|dts=3|duration=1|flags=___\n";
        parse_packets(txt, Rational { num: 1, den: 10 }, 0.0)
    }

    #[test]
    fn frame_index() {
        let f = idx();
        assert_eq!(f.len(), 6);
        assert!((f.end - 0.6).abs() < 1e-9);
        assert_eq!(f.count_in(0.1, 0.4), 3);
        assert!((f.snap(0.26) - 0.3).abs() < 1e-9);
        assert_eq!(f.next_key_at_or_after(0.1), Some(0.4));
        assert_eq!(f.last_key_at_or_before(0.39), Some(0.0));
        assert_eq!(f.last_key_at_or_before(0.4), Some(0.4));
        assert!(f.is_key_at(0.4));
        assert_eq!(f.decode_count(0.0, 0.4), 4);
        assert_eq!(f.decode_count(0.4, 0.6), 2);
        assert!((f.prev_frame_time(0.4).unwrap() - 0.3).abs() < 1e-9);
    }

    #[test]
    fn times() {
        let a = parse_iso8601("2025-05-19T14:58:11.000000Z").unwrap();
        let b = parse_bandicam_name("bandicam 2025-05-19 14-58-11-500").unwrap();
        assert!((b - a - 0.5).abs() < 1e-6);
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), None);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }
}
