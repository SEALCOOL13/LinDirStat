//! Saving and loading complete scans (`.ldscan` files), so huge, rarely-changing trees do not
//! have to be rescanned.
//!
//! Layout: 8-byte magic, u32 little-endian header length, JSON header ([`Meta`], readable
//! without decompressing the rest), then an LZ4 frame holding the tree in a compact binary form.
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::scan::*;

const MAGIC: &[u8; 8] = b"LDSCAN\0\x01";
pub const EXT: &str = "ldscan";

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Meta {
    pub version: u32,
    pub root: String,
    pub scanned_at: i64,
    pub scan_seconds: f64,
    pub apparent: bool,
    pub one_fs: bool,
    pub compact_below: u64,
    pub size: u64,
    pub files: u64,
    pub dirs: u64,
    pub errors: u64,
    pub cancelled: bool,
    pub threads: usize,
}

impl Meta {
    pub fn from_result(r: &ScanResult) -> Meta {
        let d = &r.tree.dirs[0];
        Meta {
            version: 1,
            root: r.opts.path.clone(),
            scanned_at: r.scanned_at,
            scan_seconds: r.scan_seconds,
            apparent: r.opts.apparent,
            one_fs: r.opts.one_fs,
            compact_below: r.opts.compact_below,
            size: d.size,
            files: d.nfiles,
            dirs: d.ndirs,
            errors: r.errors,
            cancelled: r.cancelled,
            threads: r.threads,
        }
    }
}

/// Folder that holds saved scans: `$XDG_DATA_HOME/windirstat-linux/scans`.
pub fn scans_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("windirstat-linux").join("scans")
}

/// A file name derived from the scanned path, e.g. `home_me_Videos`.
pub fn slug(root: &str) -> String {
    let s: String = root.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() { "root".into() } else { s.chars().take(80).collect() }
}

pub fn default_save_path(root: &str, when: i64) -> PathBuf {
    let stamp = crate::units::fmt_time(when).replace([' ', ':'], "-");
    scans_dir().join(format!("{}__{}.{EXT}", slug(root), stamp))
}

pub fn auto_save_path(root: &str) -> PathBuf {
    scans_dir().join(format!("{}__latest.{EXT}", slug(root)))
}

// ---------------------------------------------------------------------------------------------
// writing
// ---------------------------------------------------------------------------------------------
struct Out<W: Write> {
    w: W,
    buf: Vec<u8>,
}

impl<W: Write> Out<W> {
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.buf.extend_from_slice(b);
    }
    fn flush_if_big(&mut self) -> std::io::Result<()> {
        if self.buf.len() >= 1 << 20 {
            self.w.write_all(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
}

pub struct SaveData<'a> {
    pub meta: &'a Meta,
    pub tree: &'a Tree,
    pub ext_stats: &'a [[u64; 2]],
    pub top_files: &'a [NodeId],
}

/// Writes atomically: to `<path>.tmp` first, then renamed over `path`.
pub fn save(path: &Path, d: &SaveData) -> Result<u64, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("{EXT}.tmp"));
    let res = write_file(&tmp, d);
    match res {
        Ok(()) => {
            std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
            Ok(std::fs::metadata(path).map(|m| m.len()).unwrap_or(0))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.to_string())
        }
    }
}

fn write_file(path: &Path, d: &SaveData) -> std::io::Result<()> {
    let mut f = BufWriter::with_capacity(1 << 20, File::create(path)?);
    let hdr = serde_json::to_vec(d.meta).map_err(std::io::Error::other)?;
    f.write_all(MAGIC)?;
    f.write_all(&(hdr.len() as u32).to_le_bytes())?;
    f.write_all(&hdr)?;
    let enc = lz4_flex::frame::FrameEncoder::new(f);
    let mut o = Out { w: enc, buf: Vec::with_capacity(2 << 20) };
    let t = d.tree;
    o.u32(t.ext_names.len() as u32);
    for n in &t.ext_names {
        o.bytes(n.as_bytes());
    }
    o.u32(d.ext_stats.len() as u32);
    for s in d.ext_stats {
        o.u64(s[0]);
        o.u64(s[1]);
    }
    o.u32(d.top_files.len() as u32);
    for n in d.top_files {
        o.u64(n.0);
    }
    o.u32(t.dirs.len() as u32);
    for dir in &t.dirs {
        o.u32(dir.parent);
        o.u8(dir.alive as u8);
        if !dir.alive {
            continue; // deleted / replaced folders keep their id but no content
        }
        o.u16(dir.name_len);
        o.u16(dir.dom_ext);
        o.u32(dir.inode_size);
        o.u32(dir.dfiles);
        o.u64(dir.own_size);
        o.u64(dir.size);
        o.u64(dir.nfiles);
        o.u64(dir.ndirs);
        o.i64(dir.mtime);
        o.bytes(&dir.names);
        o.u32(dir.files.len() as u32);
        for fe in &dir.files {
            o.u64(fe.size);
            o.i64(fe.mtime);
            o.u32(fe.name_off);
            o.u16(fe.name_len);
            o.u16(fe.ext);
        }
        o.u32(dir.subdirs.len() as u32);
        for &s in &dir.subdirs {
            o.u32(s);
        }
        o.flush_if_big()?;
    }
    o.w.write_all(&o.buf)?;
    let mut f = o.w.finish().map_err(std::io::Error::other)?;
    f.flush()?;
    f.get_ref().sync_all()?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// reading
// ---------------------------------------------------------------------------------------------
struct In<R: Read> {
    r: R,
}

impl<R: Read> In<R> {
    fn arr<const N: usize>(&mut self) -> std::io::Result<[u8; N]> {
        let mut b = [0u8; N];
        self.r.read_exact(&mut b)?;
        Ok(b)
    }
    fn u8(&mut self) -> std::io::Result<u8> {
        Ok(self.arr::<1>()?[0])
    }
    fn u16(&mut self) -> std::io::Result<u16> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    fn u32(&mut self) -> std::io::Result<u32> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    fn u64(&mut self) -> std::io::Result<u64> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    fn i64(&mut self) -> std::io::Result<i64> {
        Ok(i64::from_le_bytes(self.arr()?))
    }
    fn bytes(&mut self, limit: usize) -> std::io::Result<Vec<u8>> {
        let n = self.u32()? as usize;
        if n > limit {
            return Err(bad("corrupt length"));
        }
        let mut v = vec![0u8; n];
        self.r.read_exact(&mut v)?;
        Ok(v)
    }
}

fn bad(s: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, s.to_string())
}

fn read_header<R: Read>(r: &mut R) -> std::io::Result<Meta> {
    let mut m = [0u8; 8];
    r.read_exact(&mut m)?;
    if &m != MAGIC {
        return Err(bad("not a LinDirStat saved scan"));
    }
    let mut l = [0u8; 4];
    r.read_exact(&mut l)?;
    let n = u32::from_le_bytes(l) as usize;
    if n > 1 << 20 {
        return Err(bad("corrupt header"));
    }
    let mut h = vec![0u8; n];
    r.read_exact(&mut h)?;
    serde_json::from_slice(&h).map_err(|e| bad(&e.to_string()))
}

/// Reads only the small JSON header (for listing saved scans).
pub fn read_meta(path: &Path) -> Result<Meta, String> {
    let mut f = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    read_header(&mut f).map_err(|e| e.to_string())
}

/// Loads a saved scan. Reports progress through `prog` like a scan (entries = folders read).
pub fn load(path: &Path, prog: Arc<Progress>) -> Result<(Meta, ScanResult), String> {
    load_inner(path, &prog).map_err(|e| format!("{}: {e}", path.display()))
}

fn load_inner(path: &Path, prog: &Progress) -> std::io::Result<(Meta, ScanResult)> {
    let mut f = BufReader::with_capacity(1 << 20, File::open(path)?);
    let meta = read_header(&mut f)?;
    prog.phase.store(1, Relaxed);
    let mut i = In { r: BufReader::with_capacity(1 << 20, lz4_flex::frame::FrameDecoder::new(f)) };
    const LIM: usize = 1 << 31;
    let n_ext = i.u32()? as usize;
    if n_ext > u16::MAX as usize {
        return Err(bad("corrupt extension table"));
    }
    let mut ext_names = Vec::with_capacity(n_ext);
    for _ in 0..n_ext {
        ext_names.push(String::from_utf8_lossy(&i.bytes(4096)?).into_owned());
    }
    let n_stats = i.u32()? as usize;
    if n_stats > u16::MAX as usize {
        return Err(bad("corrupt extension stats"));
    }
    let mut ext_stats = Vec::with_capacity(n_stats);
    for _ in 0..n_stats {
        ext_stats.push([i.u64()?, i.u64()?]);
    }
    let n_top = i.u32()? as usize;
    if n_top > 1 << 24 {
        return Err(bad("corrupt file list"));
    }
    let mut top_files = Vec::with_capacity(n_top);
    for _ in 0..n_top {
        top_files.push(NodeId(i.u64()?));
    }
    let n_dirs = i.u32()? as usize;
    prog.total.store(n_dirs as u64, Relaxed);
    prog.total_ready.store(true, Relaxed);
    let mut dirs: Vec<Dir> = Vec::with_capacity(n_dirs);
    let mut files_seen = 0u64;
    for k in 0..n_dirs {
        let parent = i.u32()?;
        let alive = i.u8()? != 0;
        let mut d = Dir::empty(parent);
        if alive {
            d.alive = true;
            d.name_len = i.u16()?;
            d.dom_ext = i.u16()?;
            d.inode_size = i.u32()?;
            d.dfiles = i.u32()?;
            d.own_size = i.u64()?;
            d.size = i.u64()?;
            d.nfiles = i.u64()?;
            d.ndirs = i.u64()?;
            d.mtime = i.i64()?;
            d.names = i.bytes(LIM)?;
            let nf = i.u32()? as usize;
            d.files.reserve_exact(nf);
            for _ in 0..nf {
                let fe = FileEntry { size: i.u64()?, mtime: i.i64()?, name_off: i.u32()?, name_len: i.u16()?, ext: i.u16()? };
                if fe.name_off as usize + fe.name_len as usize > d.names.len() {
                    return Err(bad("corrupt file entry"));
                }
                d.files.push(fe);
            }
            files_seen += nf as u64;
            let ns = i.u32()? as usize;
            d.subdirs.reserve_exact(ns);
            for _ in 0..ns {
                let s = i.u32()?;
                if s as usize >= n_dirs || s as usize <= k {
                    return Err(bad("corrupt folder link"));
                }
                d.subdirs.push(s);
            }
            if (d.name_len as usize) > d.names.len() {
                return Err(bad("corrupt folder name"));
            }
        }
        dirs.push(d);
        if k % 1024 == 0 {
            if prog.cancel.load(Relaxed) {
                return Err(bad("cancelled"));
            }
            prog.entries.store(k as u64, Relaxed);
            prog.files.store(files_seen, Relaxed);
        }
    }
    if dirs.is_empty() || !dirs[0].alive {
        return Err(bad("empty scan"));
    }
    top_files.retain(|n| {
        let d = n.dir_id() as usize;
        n.is_file() && d < dirs.len() && (n.file_idx() as usize) < dirs[d].files.len()
    });
    ext_stats.resize(ext_names.len().max(ext_stats.len()), [0, 0]);
    prog.entries.store(n_dirs as u64, Relaxed);
    prog.phase.store(2, Relaxed);
    let opts = ScanOpts {
        path: meta.root.clone(),
        one_fs: meta.one_fs,
        apparent: meta.apparent,
        threads: 0,
        compact_below: meta.compact_below,
        precount: true,
    };
    let r = ScanResult {
        tree: Tree { dirs, ext_names },
        ext_stats,
        top_files,
        errors: meta.errors,
        cancelled: meta.cancelled,
        threads: meta.threads,
        opts,
        scanned_at: meta.scanned_at,
        scan_seconds: meta.scan_seconds,
    };
    Ok((meta, r))
}

/// Saved scans in [`scans_dir`], newest first.
pub fn list_saved() -> Vec<(PathBuf, Meta, u64)> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(scans_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some(EXT) {
                continue;
            }
            if let Ok(m) = read_meta(&p) {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                v.push((p, m, len));
            }
        }
    }
    v.sort_by_key(|e| std::cmp::Reverse(e.1.scanned_at));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let tmp = std::env::temp_dir().join(format!("ldscan-test-{}", std::process::id()));
        let src = tmp.join("data");
        std::fs::create_dir_all(src.join("a/b")).unwrap();
        std::fs::write(src.join("a/x.txt"), vec![1u8; 5000]).unwrap();
        std::fs::write(src.join("a/b/y.bin"), vec![2u8; 9000]).unwrap();
        std::fs::write(src.join("z"), b"hi").unwrap();
        let opts = ScanOpts {
            path: src.to_string_lossy().into_owned(),
            one_fs: true,
            apparent: true,
            threads: 2,
            compact_below: 0,
            precount: false,
        };
        let r = run_scan(opts, Arc::new(Progress::default())).unwrap();
        let meta = Meta::from_result(&r);
        let file = tmp.join("t.ldscan");
        save(&file, &SaveData { meta: &meta, tree: &r.tree, ext_stats: &r.ext_stats, top_files: &r.top_files }).unwrap();
        let (m2, r2) = load(&file, Arc::new(Progress::default())).unwrap();
        assert_eq!(m2.files, 3);
        assert_eq!(r2.tree.dirs.len(), r.tree.dirs.len());
        assert_eq!(r2.tree.dirs[0].size, r.tree.dirs[0].size);
        assert_eq!(r2.tree.ext_names, r.tree.ext_names);
        assert_eq!(r2.top_files.len(), r.top_files.len());
        for (a, b) in r.tree.dirs.iter().zip(&r2.tree.dirs) {
            assert_eq!(a.names, b.names);
            assert_eq!(a.subdirs, b.subdirs);
            assert_eq!(a.files.len(), b.files.len());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn refresh_graft_matches_full_rescan() {
        let tmp = std::env::temp_dir().join(format!("ldgraft-test-{}", std::process::id()));
        let src = tmp.join("data");
        std::fs::create_dir_all(src.join("keep/deep")).unwrap();
        std::fs::create_dir_all(src.join("change/sub")).unwrap();
        std::fs::write(src.join("keep/deep/a.txt"), vec![1u8; 7000]).unwrap();
        std::fs::write(src.join("change/b.mp4"), vec![2u8; 3000]).unwrap();
        std::fs::write(src.join("change/sub/c.mp4"), vec![2u8; 1000]).unwrap();
        let opts = |p: &Path| ScanOpts {
            path: p.to_string_lossy().into_owned(),
            one_fs: true,
            apparent: true,
            threads: 2,
            compact_below: 0,
            precount: false,
        };
        let r = run_scan(opts(&src), Arc::new(Progress::default())).unwrap();
        // save + load first, so the graft runs on a loaded tree
        let file = tmp.join("t.ldscan");
        let meta = Meta::from_result(&r);
        save(&file, &SaveData { meta: &meta, tree: &r.tree, ext_stats: &r.ext_stats, top_files: &r.top_files }).unwrap();
        let (_, r) = load(&file, Arc::new(Progress::default())).unwrap();
        let mut tree = r.tree;
        let mut ext = r.ext_stats;
        // change the folder on disk
        std::fs::write(src.join("change/new.iso"), vec![3u8; 50_000]).unwrap();
        std::fs::remove_file(src.join("change/b.mp4")).unwrap();
        let d = tree.dirs.iter().position(|d| d.alive && d.name() == b"change").unwrap() as u32;
        let sub = run_scan(opts(&src.join("change")), Arc::new(Progress::default())).unwrap();
        let nd = tree.graft(d, sub, &mut ext);
        assert_eq!(tree.name(NodeId::dir(nd)), "change");
        assert_eq!(tree.path(NodeId::dir(nd)), src.join("change").to_string_lossy());
        let full = run_scan(opts(&src), Arc::new(Progress::default())).unwrap();
        assert_eq!(tree.dirs[0].size, full.tree.dirs[0].size);
        assert_eq!(tree.dirs[0].nfiles, full.tree.dirs[0].nfiles);
        assert_eq!(tree.dirs[0].ndirs, full.tree.dirs[0].ndirs);
        let by_name = |names: &[String], st: &[[u64; 2]], n: &str| names.iter().position(|x| x == n).map(|i| st[i]).unwrap_or([0, 0]);
        for e in [".iso", ".mp4", ".txt"] {
            assert_eq!(by_name(&tree.ext_names, &ext, e), by_name(&full.tree.ext_names, &full.ext_stats, e), "{e}");
        }
        // file entries in the grafted folder resolve to the right names and extensions
        let gd = &tree.dirs[nd as usize];
        let names: Vec<String> = gd.files.iter().map(|f| String::from_utf8_lossy(gd.file_name(f)).into_owned()).collect();
        assert_eq!(names, vec!["new.iso".to_string()]);
        assert_eq!(tree.ext_names[gd.files[0].ext as usize], ".iso");
        // saving after a graft round-trips too
        let meta = Meta::default();
        save(&file, &SaveData { meta: &meta, tree: &tree, ext_stats: &ext, top_files: &[] }).unwrap();
        let (_, r3) = load(&file, Arc::new(Progress::default())).unwrap();
        assert_eq!(r3.tree.dirs[0].size, full.tree.dirs[0].size);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
