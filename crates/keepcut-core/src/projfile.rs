//! The `.keepcut` project file (§9): JSON with `"version": 1`, absolute clip
//! paths plus each clip's path relative to the project file (when it is in the
//! same folder tree), so a folder moved together with its project still opens.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{PROJECT_VERSION, Project};

pub const EXTENSION: &str = "keepcut";

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    #[serde(flatten)]
    project: Project,
    /// Per clip: the path relative to the project file's folder, if inside it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    relative_paths: Vec<Option<PathBuf>>,
    /// The soundtrack's path relative to the project file's folder, if inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    soundtrack_relative_path: Option<PathBuf>,
}

/// Serializes a project for `path` (relative paths are computed against its folder).
pub fn to_json(project: &Project, path: Option<&Path>) -> String {
    let base = path.and_then(Path::parent);
    let relative_paths = match base {
        Some(b) => project
            .clips
            .iter()
            .map(|c| c.path.strip_prefix(b).ok().map(Path::to_path_buf))
            .collect(),
        None => Vec::new(),
    };
    let soundtrack_relative_path = base.and_then(|b| {
        let st = project.soundtrack.as_ref()?;
        st.path.strip_prefix(b).ok().map(Path::to_path_buf)
    });
    let mut project = project.clone();
    project.version = PROJECT_VERSION;
    serde_json::to_string_pretty(&ProjectFile {
        project,
        relative_paths,
        soundtrack_relative_path,
    })
    .expect("a project always serializes")
}

/// Parses a project; `path` is the file it came from (for relative paths).
/// A clip whose absolute path is gone but whose relative path exists next to
/// the project file is pointed there.
pub fn from_json(text: &str, path: Option<&Path>) -> Result<Project> {
    let f: ProjectFile = serde_json::from_str(text)
        .map_err(|e| Error::Project(format!("not a Keepcut project: {e}")))?;
    let mut p = f.project;
    if p.version > PROJECT_VERSION {
        return Err(Error::Project(format!(
            "this project was saved by a newer Keepcut (format {}, this version reads {})",
            p.version, PROJECT_VERSION
        )));
    }
    let base = path.and_then(Path::parent);
    for (c, rel) in p.clips.iter_mut().zip(f.relative_paths) {
        if let (Some(b), Some(r)) = (base, rel)
            && !c.path.exists()
            && b.join(&r).is_file()
        {
            c.path = b.join(r);
        }
    }
    if let (Some(b), Some(r), Some(st)) = (base, f.soundtrack_relative_path, p.soundtrack.as_mut())
        && !st.path.exists()
        && b.join(&r).is_file()
    {
        st.path = b.join(r);
    }
    p.version = PROJECT_VERSION;
    Ok(p)
}

/// Writes atomically (temp file + rename), so a crash never leaves half a file.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, text)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

pub fn save(project: &Project, path: &Path) -> Result<()> {
    write_atomic(path, &to_json(project, Some(path)))
}

pub fn load(path: &Path) -> Result<Project> {
    let text = std::fs::read_to_string(path)?;
    from_json(&text, Some(path))
}

/// Size and modification time (seconds) of a file, as stored per clip to
/// detect sources that changed since the project was saved.
pub fn file_stamp(path: &Path) -> Option<(u64, i64)> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    Some((m.len(), mtime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clip, Segment};

    fn clip(path: PathBuf) -> Clip {
        Clip {
            id: uuid::Uuid::new_v4(),
            path,
            file_size: 3,
            file_mtime: 7,
            enabled: false,
            segments: Some(vec![Segment::new(1.0, 2.5)]),
            chapter_title: Some("Intro".into()),
        }
    }

    #[test]
    fn round_trip_and_moved_folder() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        std::fs::create_dir_all(old.join("videos")).unwrap();
        std::fs::write(old.join("videos/a.mp4"), b"x").unwrap();
        std::fs::write(old.join("music.m4a"), b"x").unwrap();
        let outside = PathBuf::from(if cfg!(windows) {
            r"Z:\elsewhere\b.mp4"
        } else {
            "/elsewhere/b.mp4"
        });
        let mut p = Project {
            version: PROJECT_VERSION,
            id: uuid::Uuid::new_v4(),
            clips: vec![clip(old.join("videos/a.mp4")), clip(outside.clone())],
            export: Default::default(),
            soundtrack: Some(crate::model::Soundtrack {
                path: old.join("music.m4a"),
                offset: 30.5,
            }),
        };
        p.export.loudness_target = -16.0;
        let file = old.join("p.keepcut");
        save(&p, &file).unwrap();
        let back = load(&file).unwrap();
        assert_eq!(back.id, p.id);
        assert_eq!(back.clips[0].path, p.clips[0].path);
        assert_eq!(back.clips[0].segments, p.clips[0].segments);
        assert_eq!(back.clips[0].chapter_title.as_deref(), Some("Intro"));
        assert!(!back.clips[0].enabled);
        assert_eq!(back.export.loudness_target, -16.0);
        assert_eq!(back.soundtrack, p.soundtrack);
        // move the whole folder: the clip inside follows, the outside one doesn't
        let new = dir.path().join("new");
        std::fs::rename(&old, &new).unwrap();
        let moved = load(&new.join("p.keepcut")).unwrap();
        assert_eq!(moved.clips[0].path, new.join("videos").join("a.mp4"));
        assert_eq!(moved.clips[1].path, outside);
        let st = moved.soundtrack.unwrap();
        assert_eq!((st.path, st.offset), (new.join("music.m4a"), 30.5));
    }

    #[test]
    fn newer_versions_are_refused() {
        let e = from_json(r#"{"version": 99, "clips": []}"#, None).unwrap_err();
        assert!(e.to_string().contains("newer Keepcut"), "{e}");
        assert!(from_json("{}", None).is_err());
        let p = from_json(r#"{"clips": [{"path": "a.mp4"}]}"#, None).unwrap();
        assert!(p.clips[0].segments.is_none() && p.clips[0].enabled);
    }
}
