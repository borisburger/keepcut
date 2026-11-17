//! Segment editing (§8.4): keep-ranges per clip, sorted and non-overlapping.
//! All times are expected to be snapped to frame timestamps by the caller.

use crate::model::Segment;
use crate::probe::EPS;

/// Index of the segment containing `t` (`start <= t < end`).
pub fn segment_at(segs: &[Segment], t: f64) -> Option<usize> {
    segs.iter()
        .position(|s| s.start <= t + EPS && t < s.end - EPS)
}

/// Inserts keeping the list sorted by start; returns the new index.
fn insert_sorted(segs: &mut Vec<Segment>, seg: Segment) -> usize {
    let i = segs.partition_point(|s| s.start < seg.start);
    segs.insert(i, seg);
    i
}

/// **I / Set start.** Inside a segment: move its start to `t`. Otherwise create
/// a segment from `t` to the next segment's start (or `clip_end`).
/// Returns the index of the affected segment, or `None` if nothing changed.
pub fn set_start(segs: &mut Vec<Segment>, t: f64, clip_end: f64) -> Option<usize> {
    if let Some(i) = segment_at(segs, t) {
        if (segs[i].start - t).abs() < EPS {
            return None;
        }
        segs[i].start = t;
        return Some(i);
    }
    if t >= clip_end - EPS {
        return None;
    }
    let next = segs
        .iter()
        .map(|s| s.start)
        .filter(|s| *s > t + EPS)
        .fold(clip_end, f64::min);
    Some(insert_sorted(segs, Segment::new(t, next)))
}

/// **O / Set end.** Inside a segment (`start < t <= end`): move its end to `t`.
/// Otherwise create a segment from the previous segment's end (or 0) to `t`.
pub fn set_end(segs: &mut Vec<Segment>, t: f64) -> Option<usize> {
    if let Some(i) = segs
        .iter()
        .position(|s| s.start < t - EPS && t < s.end + EPS)
    {
        if (segs[i].end - t).abs() < EPS {
            return None;
        }
        segs[i].end = t;
        return Some(i);
    }
    if t <= EPS {
        return None;
    }
    let prev = segs
        .iter()
        .map(|s| s.end)
        .filter(|e| *e < t - EPS)
        .fold(0.0, f64::max);
    Some(insert_sorted(segs, Segment::new(prev, t)))
}

/// **S / Split** the segment under `t` into `[start, t)` and `[t, end)`.
/// Returns the index of the second part.
pub fn split(segs: &mut Vec<Segment>, t: f64) -> Option<usize> {
    let i = segs
        .iter()
        .position(|s| s.start < t - EPS && t < s.end - EPS)?;
    let mut second = Segment::new(t, segs[i].end);
    second.chapter_title = None;
    segs[i].end = t;
    segs.insert(i + 1, second);
    Some(i + 1)
}

/// **Del**: removes segment `i` (the footage becomes a gap).
pub fn delete(segs: &mut Vec<Segment>, i: usize) -> Option<Segment> {
    (i < segs.len()).then(|| segs.remove(i))
}

/// Allowed range for moving segment `i`'s start: from the previous segment's
/// end (or 0) up to its own end minus `min_len`.
pub fn start_limits(segs: &[Segment], i: usize, min_len: f64) -> (f64, f64) {
    let lo = if i > 0 { segs[i - 1].end } else { 0.0 };
    (lo, segs[i].end - min_len)
}

/// Allowed range for moving segment `i`'s end.
pub fn end_limits(segs: &[Segment], i: usize, clip_end: f64, min_len: f64) -> (f64, f64) {
    let hi = segs.get(i + 1).map_or(clip_end, |s| s.start);
    (segs[i].start + min_len, hi)
}

/// Clamps every segment into `[0, clip_end]`, drops empty ones and merges
/// overlaps (used after a source file changed length).
pub fn normalize(segs: &mut Vec<Segment>, clip_end: f64) {
    for s in segs.iter_mut() {
        s.start = s.start.clamp(0.0, clip_end);
        s.end = s.end.clamp(0.0, clip_end);
    }
    segs.retain(|s| s.end - s.start > EPS);
    segs.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Segment> = Vec::with_capacity(segs.len());
    for s in segs.drain(..) {
        match out.last_mut() {
            Some(last) if s.start < last.end - EPS => last.end = last.end.max(s.end),
            _ => out.push(s),
        }
    }
    *segs = out;
}

pub fn kept_duration(segs: &[Segment]) -> f64 {
    segs.iter().map(Segment::duration).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(s: &[Segment]) -> Vec<(f64, f64)> {
        s.iter().map(|x| (x.start, x.end)).collect()
    }

    #[test]
    fn cut_out_the_middle() {
        // S at A, S at B, select the middle part, Del
        let mut s = vec![Segment::new(0.0, 10.0)];
        assert_eq!(split(&mut s, 3.0), Some(1));
        assert_eq!(split(&mut s, 6.0), Some(2));
        delete(&mut s, 1);
        assert_eq!(spans(&s), [(0.0, 3.0), (6.0, 10.0)]);
    }

    #[test]
    fn set_start_and_end() {
        let mut s = vec![Segment::new(0.0, 10.0)];
        assert_eq!(set_start(&mut s, 2.0, 10.0), Some(0));
        assert_eq!(set_end(&mut s, 8.0), Some(0));
        assert_eq!(spans(&s), [(2.0, 8.0)]);
        // in a gap: I creates up to the next segment / clip end, O from the previous end
        assert_eq!(set_start(&mut s, 9.0, 12.0), Some(1));
        assert_eq!(spans(&s), [(2.0, 8.0), (9.0, 12.0)]);
        assert_eq!(set_end(&mut s, 1.0), Some(0));
        assert_eq!(spans(&s), [(0.0, 1.0), (2.0, 8.0), (9.0, 12.0)]);
        // no-ops
        assert_eq!(set_start(&mut s, 2.0, 12.0), None);
        assert_eq!(set_start(&mut s, 12.0, 12.0), None);
        assert_eq!(set_end(&mut s, 0.0), None);
    }

    #[test]
    fn gap_before_first_segment() {
        let mut s = vec![Segment::new(5.0, 10.0)];
        assert_eq!(set_start(&mut s, 1.0, 10.0), Some(0));
        assert_eq!(spans(&s), [(1.0, 5.0), (5.0, 10.0)]);
    }

    #[test]
    fn limits_and_normalize() {
        let s = vec![Segment::new(0.0, 3.0), Segment::new(5.0, 9.0)];
        assert_eq!(start_limits(&s, 1, 0.1), (3.0, 8.9));
        assert_eq!(end_limits(&s, 0, 20.0, 0.1), (0.1, 5.0));
        let mut n = vec![
            Segment::new(4.0, 30.0),
            Segment::new(0.0, 5.0),
            Segment::new(25.0, 26.0),
        ];
        normalize(&mut n, 20.0);
        assert_eq!(spans(&n), [(0.0, 20.0)]);
    }
}
