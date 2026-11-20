use crate::units::{fmt_size, Base, Unit};
use std::process::{Command, Stdio};

pub fn open_path(p: &str) {
    let _ = Command::new("xdg-open").arg(p).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// (free, total) bytes of the filesystem containing `path`.
pub fn free_space(path: &str) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(path).ok()?;
    let mut v: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut v) } != 0 {
        return None;
    }
    Some((v.f_bavail as u64 * v.f_frsize as u64, v.f_blocks as u64 * v.f_frsize as u64))
}

const PSEUDO: [&str; 26] = [
    "proc", "sysfs", "devtmpfs", "devpts", "tmpfs", "cgroup", "cgroup2", "securityfs", "pstore", "bpf", "autofs",
    "mqueue", "hugetlbfs", "debugfs", "tracefs", "fusectl", "configfs", "binfmt_misc", "squashfs", "ramfs",
    "efivarfs", "nsfs", "overlay", "fuse.portal", "fuse.gvfsd-fuse", "rpc_pipefs",
];

/// Mounted real filesystems: (mount point, description).
pub fn list_mounts() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let Ok(s) = std::fs::read_to_string("/proc/mounts") else { return out };
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let (Some(dev), Some(mnt), Some(fs)) = (it.next(), it.next(), it.next()) else { continue };
        let mnt = unescape(mnt);
        if PSEUDO.contains(&fs)
            || dev.starts_with("/dev/loop")
            || ["/snap", "/var/snap", "/run", "/sys", "/proc", "/dev"].iter().any(|p| mnt.starts_with(p))
            || out.iter().any(|(m, _)| *m == mnt)
        {
            continue;
        }
        if let Some((free, total)) = free_space(&mnt) {
            if total > 0 {
                out.push((
                    mnt.clone(),
                    format!("{mnt}   —   {} total, {} free  [{fs}]", fmt_size(total, Base::Binary, Unit::Auto), fmt_size(free, Base::Binary, Unit::Auto)),
                ));
            }
        }
    }
    out
}

fn unescape(s: &str) -> String {
    // /proc/mounts escapes space, tab, newline and backslash as \040 \011 \012 \134
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() + 0 && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push(((b[i + 1] - b'0') << 6) | ((b[i + 2] - b'0') << 3) | (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
