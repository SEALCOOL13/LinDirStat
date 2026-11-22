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
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push(((b[i + 1] - b'0') << 6) | ((b[i + 2] - b'0') << 3) | (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------------------------
// command-line targets
// ---------------------------------------------------------------------------------------------
#[derive(Debug, PartialEq)]
pub enum Target {
    Scan(String),
    Load(String),
}

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hexval(b[i + 1]), hexval(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Turns one raw argument (as a file manager may pass it) into a plain path.
fn clean_arg(a: &str) -> String {
    let mut s = a.trim().to_string();
    // stray quotes left over from double quoting, e.g. Exec="%P" plus Quote=double
    while s.len() >= 2 && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''))) {
        s = s[1..s.len() - 1].trim().to_string();
    }
    let s = s.trim_matches('"').to_string();
    let s = if let Some(rest) = s.strip_prefix("file://") {
        // file:///path or file://localhost/path
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        percent_decode(rest)
    } else {
        s
    };
    if s == "~" || s.starts_with("~/") {
        if let Ok(h) = std::env::var("HOME") {
            return format!("{h}{}", &s[1..]);
        }
    }
    s
}

/// Mount point of a block device such as /dev/sdb1.
fn mount_of_device(dev: &str) -> Option<String> {
    let real = std::fs::canonicalize(dev).ok()?;
    let s = std::fs::read_to_string("/proc/mounts").ok()?;
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let (Some(d), Some(m)) = (it.next(), it.next()) else { continue };
        if std::fs::canonicalize(d).ok().as_ref() == Some(&real) {
            return Some(unescape(m));
        }
    }
    None
}

fn classify(p: &str) -> Option<Target> {
    use std::os::unix::fs::FileTypeExt;
    if p.is_empty() {
        return None;
    }
    let path = std::path::Path::new(p);
    let md = std::fs::metadata(path).ok()?;
    if md.is_dir() {
        let abs = std::fs::canonicalize(path).map(|x| x.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string());
        return Some(Target::Scan(abs));
    }
    if md.file_type().is_block_device() {
        return mount_of_device(p).map(Target::Scan);
    }
    if md.is_file() {
        if path.extension().and_then(|e| e.to_str()) == Some(crate::savefile::EXT) {
            return Some(Target::Load(p.to_string()));
        }
        // a file was passed: scan the folder it is in
        return path.parent().map(|d| Target::Scan(d.to_string_lossy().into_owned()));
    }
    None
}

/// Works out what to open from the positional arguments. Tolerates what file managers send:
/// `file://` URIs, leftover quotes, a path split into several arguments at its spaces, a file
/// instead of a folder, and a device path (Nemo's `%D`). `Ok(None)`: nothing was passed.
pub fn resolve_target(args: &[String]) -> Result<Option<Target>, String> {
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--") && a.as_str() != "-").collect();
    let raw: Vec<String> = pos.iter().map(|a| a.to_string()).filter(|a| !a.trim().is_empty()).collect();
    if raw.is_empty() {
        return if pos.is_empty() { Ok(None) } else { Err("The folder argument was empty".into()) };
    }
    // a path containing spaces that was split into several arguments (checked first, so that
    // a fragment such as "Videos" is not mistaken for a folder of its own)
    if raw.len() > 1 {
        if let Some(t) = classify(&clean_arg(&raw.join(" "))) {
            return Ok(Some(t));
        }
    }
    for a in &raw {
        if let Some(t) = classify(&clean_arg(a)) {
            return Ok(Some(t));
        }
    }
    Err(format!("Cannot open “{}”: not a folder, drive or saved scan", raw.join(" ")))
}

// ---------------------------------------------------------------------------------------------
// desktop integration
// ---------------------------------------------------------------------------------------------
pub fn file_uri(p: &str) -> String {
    let mut s = String::from("file://");
    for &b in p.as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// Opens the file manager with `p` selected (freedesktop FileManager1), else opens its folder.
pub fn show_in_file_manager(p: &str) {
    let ok = Command::new("dbus-send")
        .args([
            "--session",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
            &format!("array:string:{}", file_uri(p)),
            "string:",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        if let Some(parent) = std::path::Path::new(p).parent() {
            open_path(&parent.to_string_lossy());
        }
    }
}

/// Opens a terminal in folder `dir`, trying the usual Linux terminals.
pub fn open_terminal(dir: &str) -> bool {
    let mut cands: Vec<String> = Vec::new();
    if let Ok(t) = std::env::var("TERMINAL") {
        cands.push(t);
    }
    for t in ["x-terminal-emulator", "gnome-terminal", "kgx", "konsole", "xfce4-terminal", "mate-terminal", "tilix", "kitty", "alacritty", "wezterm", "foot", "xterm"] {
        cands.push(t.into());
    }
    for t in cands {
        if Command::new(&t).current_dir(dir).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        let tmp = std::env::temp_dir().join(format!("ld target {}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let t = tmp.to_string_lossy().into_owned();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(resolve_target(&s(&[])), Ok(None));
        assert_eq!(resolve_target(&s(&[&t])), Ok(Some(Target::Scan(t.clone()))));
        assert_eq!(resolve_target(&s(&[&format!("\"{t}\"")])), Ok(Some(Target::Scan(t.clone()))));
        assert_eq!(resolve_target(&s(&[&file_uri(&t)])), Ok(Some(Target::Scan(t.clone()))));
        let parts: Vec<&str> = t.split(' ').collect();
        assert_eq!(resolve_target(&s(&parts)), Ok(Some(Target::Scan(t.clone()))));
        std::fs::write(tmp.join("f.txt"), b"x").unwrap();
        assert_eq!(resolve_target(&s(&[&format!("{t}/f.txt")])), Ok(Some(Target::Scan(t.clone()))));
        assert!(resolve_target(&s(&[""])).is_err());
        assert!(resolve_target(&s(&["/definitely/not/here"])).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
