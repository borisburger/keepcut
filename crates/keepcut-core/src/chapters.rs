//! Chapters: ffmetadata writer and the YouTube description block (§6.13).

use crate::timefmt;

#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    pub start: f64,
    pub end: f64,
    pub title: String,
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '=' | ';' | '#' | '\\' | '\n') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

pub fn ffmetadata(chapters: &[Chapter]) -> String {
    let mut s = String::from(";FFMETADATA1\n");
    for c in chapters {
        s.push_str(&format!(
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART={}\nEND={}\ntitle={}\n",
            (c.start * 1000.0).round() as i64,
            (c.end * 1000.0).round() as i64,
            escape(&c.title)
        ));
    }
    s
}

/// Default chapter title of a segment: the clip's title, numbered ("Intro – 2")
/// when the clip has more than one kept segment.
pub fn default_segment_title(clip_title: &str, seg_idx: usize, kept_segments: usize) -> String {
    if kept_segments <= 1 {
        clip_title.to_string()
    } else {
        format!("{clip_title} – {}", seg_idx + 1)
    }
}

/// `0:00 Intro` lines, plus warnings if YouTube's rules are not met.
pub fn youtube_block(chapters: &[Chapter]) -> (String, Vec<String>) {
    let hours = chapters.last().is_some_and(|c| c.start >= 3600.0);
    let text = chapters
        .iter()
        .map(|c| format!("{} {}", timefmt::youtube(c.start, hours), c.title))
        .collect::<Vec<_>>()
        .join("\n");
    let mut warn = Vec::new();
    if chapters.first().is_some_and(|c| c.start > 0.0) {
        warn.push("the first chapter must start at 0:00".into());
    }
    if chapters.len() < 3 {
        warn.push("YouTube needs at least 3 chapters".into());
    }
    if chapters.iter().any(|c| c.end - c.start < 10.0) {
        warn.push("every chapter must be at least 10 seconds long".into());
    }
    (text, warn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_and_youtube() {
        let ch = vec![
            Chapter {
                start: 0.0,
                end: 83.4,
                title: "Intro=1; #x".into(),
            },
            Chapter {
                start: 83.4,
                end: 90.0,
                title: "B".into(),
            },
        ];
        let m = ffmetadata(&ch);
        assert!(m.contains("START=0\nEND=83400\ntitle=Intro\\=1\\; \\#x\n"));
        let (t, w) = youtube_block(&ch);
        assert_eq!(t, "0:00 Intro=1; #x\n1:23 B");
        assert_eq!(w.len(), 2);
    }
}
