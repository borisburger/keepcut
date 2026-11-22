//! Clip thumbnails and audio waveforms for the UI (§8.2, §8.3, §6.14).

use std::io::Read;

use crate::error::{Error, Result};
use crate::ffmpeg::{self, CancelToken, RunOpts, Tools};
use crate::probe::MediaInfo;
use crate::timefmt::ff_secs;

/// Thumbnail size (RGBA): 16:9, letterboxed; sharp at 96×54 points on HiDPI.
pub const THUMB_W: u32 = 192;
pub const THUMB_H: u32 = 108;

/// Waveform resolution: one peak per 10 ms.
pub const WAVE_RATE: f64 = 100.0;
/// Audio is decoded at this rate for the peaks (enough for the envelope).
const DECODE_RATE: usize = 8000;

/// A frame near 10% of the clip as RGBA (`THUMB_W`×`THUMB_H`). Seeks to the
/// keyframe at or before that point, so even 4K with long GOPs decodes only
/// one frame (§6.14).
pub fn thumbnail(tools: &Tools, info: &MediaInfo) -> Result<Vec<u8>> {
    let target = info.video_end() * 0.1;
    let t = info.frames.last_key_at_or_before(target).unwrap_or(0.0);
    let vf = format!(
        "scale={THUMB_W}:{THUMB_H}:force_original_aspect_ratio=decrease,pad={THUMB_W}:{THUMB_H}:(ow-iw)/2:(oh-ih)/2,format=rgba"
    );
    let cmd = tools
        .ffmpeg()
        .args([
            "-hide_banner",
            "-nostdin",
            "-v",
            "error",
            "-noaccurate_seek",
            "-ss",
        ])
        .arg(ff_secs(t))
        .arg("-i")
        .arg(&info.path)
        .args(["-map", "0:v:0", "-frames:v", "1", "-vf"])
        .arg(vf)
        .args(["-f", "rawvideo", "-"]);
    let o = ffmpeg::run_checked(
        &cmd,
        RunOpts {
            quiet_stderr: true,
            ..Default::default()
        },
    )?;
    let want = (THUMB_W * THUMB_H * 4) as usize;
    if o.stdout.len() < want {
        return Err(Error::Plan(format!(
            "thumbnail: got {} bytes",
            o.stdout.len()
        )));
    }
    Ok(o.stdout[..want].to_vec())
}

/// Peak level per 10 ms (0–255, linear) of all audio tracks mixed to mono;
/// empty without audio.
pub fn waveform(tools: &Tools, info: &MediaInfo, cancel: &CancelToken) -> Result<Vec<u8>> {
    if info.audio.is_empty() {
        return Ok(Vec::new());
    }
    let inputs: String = (0..info.audio.len())
        .map(|i| format!("[0:a:{i}]"))
        .collect();
    let mix = if info.audio.len() > 1 {
        format!("{inputs}amix=inputs={}:normalize=0,", info.audio.len())
    } else {
        inputs
    };
    let graph =
        format!("{mix}aresample={DECODE_RATE},aformat=sample_fmts=s16:channel_layouts=mono[a]");
    let cmd = tools
        .ffmpeg()
        .args(["-hide_banner", "-nostdin", "-v", "error", "-i"])
        .arg(&info.path)
        .arg("-filter_complex")
        .arg(graph)
        .args(["-map", "[a]", "-f", "s16le", "-"]);
    let mut child = ffmpeg::spawn_stdout(&cmd)?;
    let mut out = child.stdout.take().expect("piped stdout");
    let mut acc = PeakAcc::new(DECODE_RATE / WAVE_RATE as usize);
    let mut buf = vec![0u8; 64 * 1024];
    let mut carry: Option<u8> = None;
    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Cancelled);
        }
        let n = out.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut bytes = &buf[..n];
        if let Some(lo) = carry.take() {
            acc.push(i16::from_le_bytes([lo, bytes[0]]));
            bytes = &bytes[1..];
        }
        let (pairs, rest) = bytes.as_chunks::<2>();
        for c in pairs {
            acc.push(i16::from_le_bytes(*c));
        }
        carry = rest.first().copied();
    }
    let _ = child.wait();
    Ok(acc.finish())
}

/// Running peak per `per` samples.
struct PeakAcc {
    per: usize,
    n: usize,
    peak: u16,
    out: Vec<u8>,
}

impl PeakAcc {
    fn new(per: usize) -> Self {
        PeakAcc {
            per: per.max(1),
            n: 0,
            peak: 0,
            out: Vec::new(),
        }
    }
    fn push(&mut self, s: i16) {
        self.peak = self.peak.max(s.unsigned_abs());
        self.n += 1;
        if self.n == self.per {
            self.flush();
        }
    }
    fn flush(&mut self) {
        self.out
            .push((self.peak as u32 * 255 / 32768).min(255) as u8);
        self.n = 0;
        self.peak = 0;
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.flush();
        }
        self.out
    }
}

/// Peak of `peaks` over `[t0, t1)` seconds, as a display height 0–1 on a
/// −48 dB…0 dB scale (quiet speech stays visible next to loud parts).
pub fn wave_height(peaks: &[u8], t0: f64, t1: f64) -> f32 {
    if peaks.is_empty() {
        return 0.0;
    }
    let a = ((t0 * WAVE_RATE).floor().max(0.0) as usize).min(peaks.len() - 1);
    let b = ((t1 * WAVE_RATE).ceil() as usize).clamp(a + 1, peaks.len());
    let p = peaks[a..b].iter().copied().max().unwrap_or(0);
    if p == 0 {
        return 0.0;
    }
    let db = 20.0 * (p as f32 / 255.0).log10();
    (1.0 + db / 48.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_and_heights() {
        let mut acc = PeakAcc::new(4);
        for s in [0, 100, -32768, 5, 1, 2, 3, 4, 7] {
            acc.push(s);
        }
        let p = acc.finish();
        assert_eq!(p, vec![255, 0, 0]);
        let peaks = vec![0u8, 255, 64, 0];
        assert_eq!(wave_height(&peaks, 0.0, 0.01), 0.0);
        assert_eq!(wave_height(&peaks, 0.0, 0.02), 1.0);
        let h = wave_height(&peaks, 0.02, 0.03);
        assert!((h - (1.0 - 12.0 / 48.0)).abs() < 0.01, "{h}");
        assert_eq!(
            wave_height(&peaks, 9.0, 10.0),
            0.0,
            "past the end: last bucket"
        );
        assert_eq!(wave_height(&[], 0.0, 1.0), 0.0);
    }
}
