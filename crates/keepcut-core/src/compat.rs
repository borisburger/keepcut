//! Copy signatures, target selection and per-clip verdicts (§6.2).

use serde::{Deserialize, Serialize};

use crate::probe::VideoStream;

/// Hard keys: a clip may be stream-copied into an output only if these match.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Signature {
    pub codec_name: String,
    pub profile: Option<String>,
    pub width: u32,
    pub height: u32,
    pub pix_fmt: String,
    pub sar: String,
    pub field_order: Option<String>,
    pub rotation: i32,
}

impl Signature {
    pub fn of(v: &VideoStream) -> Signature {
        Signature {
            codec_name: v.codec_name.clone(),
            profile: v.profile.clone(),
            width: v.width,
            height: v.height,
            pix_fmt: v.pix_fmt.clone(),
            sar: v.sar.clone(),
            field_order: v.field_order.clone().filter(|f| f != "progressive"),
            rotation: v.rotation,
        }
    }

    pub fn label(&self) -> String {
        format!(
            "{}×{} {} {}{}",
            self.width,
            self.height,
            codec_label(&self.codec_name),
            self.profile.as_deref().unwrap_or(""),
            if self.pix_fmt.contains("10") {
                " 10-bit"
            } else {
                ""
            }
        )
    }

    /// Human-readable reasons why `self` (a clip) can't be copied into `target`.
    pub fn mismatch_reasons(&self, target: &Signature) -> Vec<String> {
        let mut r = Vec::new();
        if (self.width, self.height) != (target.width, target.height) {
            let up = (self.width as u64 * self.height as u64)
                < (target.width as u64 * target.height as u64);
            r.push(format!(
                "{}×{} vs {}×{} (scaled {})",
                self.width,
                self.height,
                target.width,
                target.height,
                if up { "up" } else { "down" }
            ));
        }
        if self.codec_name != target.codec_name {
            r.push(format!(
                "codec {} vs {}",
                codec_label(&self.codec_name),
                codec_label(&target.codec_name)
            ));
        } else if self.profile != target.profile {
            r.push(format!(
                "profile {} vs {}",
                self.profile.as_deref().unwrap_or("?"),
                target.profile.as_deref().unwrap_or("?")
            ));
        }
        if self.pix_fmt != target.pix_fmt {
            r.push(format!(
                "pixel format {} vs {}",
                self.pix_fmt, target.pix_fmt
            ));
        }
        if self.sar != target.sar {
            r.push(format!(
                "sample aspect ratio {} vs {}",
                self.sar, target.sar
            ));
        }
        if self.field_order != target.field_order {
            r.push("interlacing differs".into());
        }
        if self.rotation != target.rotation {
            r.push(format!(
                "rotation {}° vs {}°",
                self.rotation, target.rotation
            ));
        }
        r
    }
}

pub fn codec_label(c: &str) -> &str {
    match c {
        "h264" => "H.264",
        "hevc" => "HEVC",
        "av1" => "AV1",
        "vp9" => "VP9",
        other => other,
    }
}

/// Soft-key differences (warn only).
pub fn soft_warnings(clip: &VideoStream, target: &VideoStream) -> Vec<String> {
    let mut w = Vec::new();
    if clip.level != target.level {
        w.push(format!("level {:?} vs {:?}", clip.level, target.level));
    }
    let col = |v: &VideoStream| {
        (
            v.color_primaries.clone(),
            v.color_transfer.clone(),
            v.color_space.clone(),
            v.color_range.clone(),
        )
    };
    if col(clip) != col(target) {
        w.push("color tags differ".into());
    }
    if clip.time_base != target.time_base {
        w.push(format!(
            "time base {} vs {} (joined at a 90 kHz timescale)",
            clip.time_base, target.time_base
        ));
    }
    if clip.r_frame_rate != target.r_frame_rate {
        w.push(format!(
            "frame rate {} vs {}",
            clip.r_frame_rate, target.r_frame_rate
        ));
    }
    w
}

/// Groups clips by signature and picks the group with the largest total kept
/// duration (tie: larger pixel count). Input: `(signature, kept seconds)`.
/// Returns the distinct signatures (ordered by preference) and the chosen index.
pub fn choose_target(clips: &[(Signature, f64)]) -> (Vec<(Signature, f64)>, usize) {
    let mut groups: Vec<(Signature, f64)> = Vec::new();
    for (sig, d) in clips {
        match groups.iter_mut().find(|(s, _)| s == sig) {
            Some(g) => g.1 += d,
            None => groups.push((sig.clone(), *d)),
        }
    }
    groups.sort_by(|a, b| {
        b.1.total_cmp(&a.1).then_with(|| {
            let pa = a.0.width as u64 * a.0.height as u64;
            let pb = b.0.width as u64 * b.0.height as u64;
            pb.cmp(&pa)
        })
    });
    (groups, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(w: u32, h: u32) -> Signature {
        Signature {
            codec_name: "h264".into(),
            profile: Some("High".into()),
            width: w,
            height: h,
            pix_fmt: "yuv420p".into(),
            sar: "1:1".into(),
            field_order: None,
            rotation: 0,
        }
    }

    #[test]
    fn target_by_duration_then_pixels() {
        let (g, i) = choose_target(&[
            (sig(1280, 720), 100.0),
            (sig(1920, 1080), 60.0),
            (sig(1920, 1080), 50.0),
        ]);
        assert_eq!(g[i].0.width, 1920);
        let (g, _) = choose_target(&[(sig(1280, 720), 10.0), (sig(1920, 1080), 10.0)]);
        assert_eq!(g[0].0.width, 1920);
    }

    #[test]
    fn reasons() {
        let r = sig(1280, 720).mismatch_reasons(&sig(1920, 1080));
        assert_eq!(r.len(), 1);
        assert!(r[0].contains("scaled up"));
        assert!(
            sig(1920, 1080)
                .mismatch_reasons(&sig(1920, 1080))
                .is_empty()
        );
    }
}
