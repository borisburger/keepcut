//! File-system helpers: free space for the pre-export check (§6.10).

use std::path::{Path, PathBuf};

/// The nearest existing ancestor of `p` (the folder may not exist yet).
fn existing(p: &Path) -> Option<PathBuf> {
    p.ancestors().find(|a| a.exists()).map(Path::to_path_buf)
}

/// Bytes available to the current user on the volume holding `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    let dir = existing(path)?;
    imp::free_space(&dir)
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    pub fn free_space(dir: &Path) -> Option<u64> {
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut avail = 0u64;
        // SAFETY: `wide` is NUL-terminated; the out pointers are valid or null.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut avail,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(avail)
    }
}

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub fn free_space(dir: &Path) -> Option<u64> {
        let c = CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a valid C string and `st` a valid out struct.
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        Some(st.f_bavail as u64 * st.f_frsize as u64)
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    pub fn free_space(_: &std::path::Path) -> Option<u64> {
        None
    }
}

/// "1.4 GB", "820 MB".
pub fn human_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", b / 1e3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_has_space() {
        let missing = std::env::temp_dir()
            .join("keepcut-does-not-exist")
            .join("x");
        assert!(free_space(&missing).is_some_and(|b| b > 0));
        assert_eq!(human_bytes(1_400_000_000), "1.4 GB");
        assert_eq!(human_bytes(820_000_000), "820 MB");
    }
}
