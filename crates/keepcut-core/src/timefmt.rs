//! Time formatting and parsing.

/// Seconds formatted for ffmpeg arguments (6 decimals, never negative zero).
pub fn ff_secs(t: f64) -> String {
    let t = if t.abs() < 5e-7 { 0.0 } else { t };
    format!("{t:.6}")
}

/// `h:mm:ss.mmm` (or `m:ss.mmm` below one hour) for display.
pub fn display(t: f64) -> String {
    let ms_total = (t.max(0.0) * 1000.0).round() as u64;
    let (h, rem) = (ms_total / 3_600_000, ms_total % 3_600_000);
    let (m, rem) = (rem / 60_000, rem % 60_000);
    let (s, ms) = (rem / 1000, rem % 1000);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

/// YouTube chapter timestamp: `m:ss`, or `h:mm:ss` when `with_hours`.
pub fn youtube(t: f64, with_hours: bool) -> String {
    let s_total = t.max(0.0).floor() as u64;
    let (h, m, s) = (s_total / 3600, (s_total % 3600) / 60, s_total % 60);
    if with_hours {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{}:{s:02}", h * 60 + m)
    }
}

/// Parses `ss`, `ss.fff`, `mm:ss(.fff)` or `hh:mm:ss(.fff)`.
pub fn parse(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = 0.0;
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    for (i, p) in parts.iter().enumerate() {
        let v: f64 = p.parse().ok()?;
        if v < 0.0 || (i > 0 && v >= 60.0) {
            return None;
        }
        total = total * 60.0 + v;
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(ff_secs(1.5), "1.500000");
        assert_eq!(ff_secs(-1e-9), "0.000000");
        assert_eq!(display(83.4), "1:23.400");
        assert_eq!(display(3723.0), "1:02:03.000");
        assert_eq!(youtube(83.9, false), "1:23");
        assert_eq!(youtube(3723.0, true), "1:02:03");
        assert_eq!(youtube(65.0, true), "0:01:05");
    }

    #[test]
    fn parses() {
        assert_eq!(parse("12.5"), Some(12.5));
        assert_eq!(parse("1:02.5"), Some(62.5));
        assert_eq!(parse("1:00:00"), Some(3600.0));
        assert_eq!(parse("1:60"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("x"), None);
    }
}
