//! High-performance parallel directory scanner + compact in-memory tree.
//!
//! Techniques used:
//!  * raw `getdents64` (256 KiB buffers) – no per-entry allocation, `d_type` avoids stats for
//!    symlinks / special files and lets us classify entries without touching inodes;
//!  * raw `statx` relative to an open directory fd (`openat`-style, no path walking per file)
//!    requesting only the fields we need, with an automatic `fstatat` fallback on old kernels;
//!  * work-stealing thread pool (crossbeam-deque), one task per directory, LIFO local queues
//!    for cache locality, FIFO stealing so thieves take the biggest/oldest subtrees;
//!  * extra helper threads for gigantic flat directories (idle capacity only);
//!  * thread-local accumulators – no locks on the hot path (hard-link dedupe is sharded);
//!  * compact arenas: one name buffer + one `FileEntry` vector per directory, u32 ids, no
//!    per-file allocation, no pointers between nodes;
//!  * parent ids are always smaller than child ids, so aggregation is one reverse linear pass;
//!  * automatic thread count (SSD vs rotational detection) and optional small-file compaction.
use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use crossbeam_utils::Backoff;
use rustc_hash::{FxHashMap, FxHashSet};

pub const NO_PARENT: u32 = u32::MAX;
pub const EXT_NONE: u16 = 0;
pub const EXT_AGG: u16 = u16::MAX; // "(N small files)" pseudo entry
pub const EXT_DEL: u16 = u16::MAX - 1; // deleted tombstone
const MAX_EXT: usize = (u16::MAX - 2) as usize;
const BUF_SIZE: usize = 256 * 1024;
const PAR_MIN: usize = 4096; // entries in one getdents batch before helper threads are considered
const TOP_FILES_PER_WORKER: usize = 3000;
pub const TOP_FILES_KEPT: usize = 3000;
const SHARDS: usize = 64;

const AT_FDCWD: c_int = -100;
const AT_SYMLINK_NOFOLLOW: c_int = 0x100;
const AT_NO_AUTOMOUNT: c_int = 0x800;
const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const DT_UNKNOWN: u8 = 0;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const STATX_MASK: u32 = libc::STATX_TYPE
    | libc::STATX_MODE
    | libc::STATX_NLINK
    | libc::STATX_INO
    | libc::STATX_SIZE
    | libc::STATX_BLOCKS
    | libc::STATX_MTIME;

// ------------------------------------------------------------------------------------------
// ids
// ------------------------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct NodeId(pub u64);
const FILE_BIT: u64 = 1 << 63;
const AGG_BIT: u64 = 1 << 62;

impl NodeId {
    #[inline]
    pub fn dir(d: u32) -> Self {
        NodeId(d as u64)
    }
    #[inline]
    pub fn file(d: u32, i: u32) -> Self {
        NodeId(FILE_BIT | ((d as u64) << 32) | i as u64)
    }
    /// Pseudo node: the "everything too small to draw" tail tile of directory `d`.
    #[inline]
    pub fn agg(d: u32) -> Self {
        NodeId(AGG_BIT | d as u64)
    }
    #[inline]
    pub fn is_dir(self) -> bool {
        self.0 & (FILE_BIT | AGG_BIT) == 0
    }
    #[inline]
    pub fn is_file(self) -> bool {
        self.0 & FILE_BIT != 0
    }
    #[inline]
    pub fn is_agg(self) -> bool {
        self.0 & AGG_BIT != 0
    }
    /// For dirs: itself. For files / agg: the containing directory.
    #[inline]
    pub fn dir_id(self) -> u32 {
        if self.is_file() {
            ((self.0 >> 32) & 0x3fff_ffff) as u32
        } else {
            self.0 as u32
        }
    }
    #[inline]
    pub fn file_idx(self) -> u32 {
        self.0 as u32
    }
}

// ------------------------------------------------------------------------------------------
// tree model
// ------------------------------------------------------------------------------------------
#[derive(Clone, Copy)]
pub struct FileEntry {
    pub size: u64,
    pub mtime: i64,
    pub name_off: u32,
    pub name_len: u16,
    pub ext: u16,
}

pub struct Dir {
    pub parent: u32,
    pub name_len: u16,
    pub alive: bool,
    pub dom_ext: u16,
    pub inode_size: u32,
    pub dfiles: u32,
    pub own_size: u64,
    pub size: u64,
    pub nfiles: u64,
    pub ndirs: u64,
    pub mtime: i64,
    pub names: Vec<u8>,
    pub files: Vec<FileEntry>,
    pub subdirs: Vec<u32>,
}

impl Dir {
    fn placeholder() -> Dir {
        Dir {
            parent: NO_PARENT,
            name_len: 0,
            alive: false,
            dom_ext: 0,
            inode_size: 0,
            dfiles: 0,
            own_size: 0,
            size: 0,
            nfiles: 0,
            ndirs: 0,
            mtime: 0,
            names: Vec::new(),
            files: Vec::new(),
            subdirs: Vec::new(),
        }
    }
    #[inline]
    pub fn name(&self) -> &[u8] {
        &self.names[..self.name_len as usize]
    }
    #[inline]
    pub fn file_name(&self, f: &FileEntry) -> &[u8] {
        &self.names[f.name_off as usize..f.name_off as usize + f.name_len as usize]
    }
}

pub struct Tree {
    pub dirs: Vec<Dir>,
    pub ext_names: Vec<String>,
}

impl Tree {
    pub fn size(&self, n: NodeId) -> u64 {
        if n.is_dir() || n.is_agg() {
            self.dirs[n.dir_id() as usize].size
        } else {
            self.file(n).size
        }
    }
    #[inline]
    pub fn file(&self, n: NodeId) -> &FileEntry {
        &self.dirs[n.dir_id() as usize].files[n.file_idx() as usize]
    }
    pub fn is_deleted(&self, n: NodeId) -> bool {
        if n.is_dir() {
            !self.dirs[n.0 as usize].alive
        } else if n.is_file() {
            self.file(n).ext == EXT_DEL
        } else {
            false
        }
    }
    pub fn is_small_agg_file(&self, n: NodeId) -> bool {
        n.is_file() && self.file(n).ext == EXT_AGG
    }
    pub fn name(&self, n: NodeId) -> String {
        if n.is_agg() {
            return "(smaller items)".into();
        }
        let d = &self.dirs[n.dir_id() as usize];
        if n.is_dir() {
            String::from_utf8_lossy(d.name()).into_owned()
        } else {
            String::from_utf8_lossy(d.file_name(self.file(n))).into_owned()
        }
    }
    pub fn mtime(&self, n: NodeId) -> i64 {
        if n.is_file() {
            self.file(n).mtime
        } else {
            self.dirs[n.dir_id() as usize].mtime
        }
    }
    pub fn ext(&self, n: NodeId) -> u16 {
        if n.is_file() {
            self.file(n).ext
        } else if n.is_agg() {
            EXT_AGG
        } else {
            self.dirs[n.dir_id() as usize].dom_ext
        }
    }
    pub fn parent(&self, n: NodeId) -> Option<NodeId> {
        if n.is_dir() {
            let p = self.dirs[n.0 as usize].parent;
            (p != NO_PARENT).then(|| NodeId::dir(p))
        } else {
            Some(NodeId::dir(n.dir_id()))
        }
    }
    pub fn path(&self, n: NodeId) -> String {
        let mut parts: Vec<String> = Vec::new();
        if !n.is_dir() && !n.is_agg() {
            parts.push(self.name(n));
        }
        let mut d = n.dir_id();
        loop {
            let dir = &self.dirs[d as usize];
            let name = String::from_utf8_lossy(dir.name()).into_owned();
            if dir.parent == NO_PARENT {
                let mut out = name;
                for p in parts.iter().rev() {
                    if !out.ends_with('/') {
                        out.push('/');
                    }
                    out.push_str(p);
                }
                return out;
            }
            parts.push(name);
            d = dir.parent;
        }
    }
    /// Remove a node (after the user deleted it on disk). Returns (bytes, files, per-extension [bytes,count]).
    pub fn remove(&mut self, n: NodeId) -> (u64, u64, Vec<(u16, u64)>) {
        let mut ext_dec: Vec<(u16, u64)> = Vec::new();
        let (size, files, dirs);
        if n.is_file() {
            let d = n.dir_id() as usize;
            let f = self.dirs[d].files[n.file_idx() as usize];
            if f.ext != EXT_DEL {
                ext_dec.push((f.ext, f.size));
                let e = &mut self.dirs[d].files[n.file_idx() as usize];
                e.size = 0;
                e.ext = EXT_DEL;
            }
            self.dirs[d].dfiles = self.dirs[d].dfiles.saturating_sub(1);
            self.dirs[d].own_size = self.dirs[d].own_size.saturating_sub(f.size);
            size = f.size;
            files = 1;
            dirs = 0;
        } else {
            let id = n.dir_id() as usize;
            size = self.dirs[id].size;
            files = self.dirs[id].nfiles;
            dirs = self.dirs[id].ndirs + 1;
            let parent = self.dirs[id].parent;
            let mut stack = vec![id as u32];
            while let Some(c) = stack.pop() {
                let dir = &mut self.dirs[c as usize];
                dir.alive = false;
                for f in &dir.files {
                    if f.ext != EXT_DEL && f.ext != EXT_AGG {
                        ext_dec.push((f.ext, f.size));
                    }
                }
                stack.extend(dir.subdirs.iter().copied());
            }
            if parent != NO_PARENT {
                self.dirs[parent as usize].subdirs.retain(|&c| c as usize != id);
            }
        }
        let mut a = self.parent(n);
        while let Some(p) = a {
            let d = &mut self.dirs[p.0 as usize];
            d.size = d.size.saturating_sub(size);
            d.nfiles = d.nfiles.saturating_sub(files);
            d.ndirs = d.ndirs.saturating_sub(dirs);
            a = if d.parent == NO_PARENT { None } else { Some(NodeId::dir(d.parent)) };
        }
        (size, files, ext_dec)
    }
}

// ------------------------------------------------------------------------------------------
// public scan API
// ------------------------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct ScanOpts {
    pub path: String,
    pub one_fs: bool,
    pub apparent: bool,
    pub threads: usize, // 0 = auto
    pub compact_below: u64,
    pub precount: bool,
}

#[derive(Default)]
pub struct Progress {
    pub entries: AtomicU64,
    pub bytes: AtomicU64,
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub errors: AtomicU64,
    pub total: AtomicU64,
    pub total_ready: AtomicBool,
    pub phase: AtomicU8, // 0 starting, 1 scanning, 2 aggregating
    pub cancel: AtomicBool,
    pub threads: AtomicUsize,
    pub current: Mutex<String>,
}

pub struct ScanResult {
    pub tree: Tree,
    pub ext_stats: Vec<[u64; 2]>,
    pub top_files: Vec<NodeId>,
    pub errors: u64,
    pub cancelled: bool,
    pub threads: usize,
}

struct Task {
    id: u32,
    parent: u32,
    inode_size: u32,
    mtime: i64,
    path: Vec<u8>, // NUL terminated
}

struct Shared {
    one_fs: bool,
    apparent: bool,
    compact_below: u64,
    root_dev: u64,
    threads: usize,
    next_id: AtomicU32,
    pending: AtomicUsize,
    active: AtomicUsize,
    injector: Injector<Task>,
    seen: Vec<Mutex<FxHashSet<(u64, u64)>>>,
    prog: Arc<Progress>,
}

// ---- extension interning -----------------------------------------------------------------
struct ExtTable {
    names: Vec<Vec<u8>>,
    map: FxHashMap<Vec<u8>, u16>,
}
static EXT_TABLE: OnceLock<Mutex<ExtTable>> = OnceLock::new();
thread_local! {
    static EXT_CACHE: RefCell<FxHashMap<Box<[u8]>, u16>> = RefCell::new(FxHashMap::default());
}
fn ext_table() -> &'static Mutex<ExtTable> {
    EXT_TABLE.get_or_init(|| Mutex::new(ExtTable { names: vec![b"(none)".to_vec()], map: FxHashMap::default() }))
}
fn reset_ext_table() {
    let mut t = ext_table().lock().unwrap();
    t.names.clear();
    t.map.clear();
    t.names.push(b"(none)".to_vec());
}
fn ext_id(name: &[u8]) -> u16 {
    let n = name.len();
    let dot = match name.iter().rposition(|&b| b == b'.') {
        Some(p) if p > 0 && p + 1 < n => p,
        _ => return EXT_NONE,
    };
    let e = &name[dot + 1..];
    if e.len() > 31 {
        return EXT_NONE;
    }
    let mut buf = [0u8; 32];
    for (i, b) in e.iter().enumerate() {
        buf[i] = b.to_ascii_lowercase();
    }
    let key = &buf[..e.len()];
    EXT_CACHE.with(|c| {
        if let Some(&id) = c.borrow().get(key) {
            return id;
        }
        let id = {
            let mut t = ext_table().lock().unwrap();
            if let Some(&id) = t.map.get(key) {
                id
            } else if t.names.len() >= MAX_EXT {
                EXT_NONE
            } else {
                let id = t.names.len() as u16;
                let mut full = Vec::with_capacity(key.len() + 1);
                full.push(b'.');
                full.extend_from_slice(key);
                t.names.push(full);
                t.map.insert(key.to_vec(), id);
                id
            }
        };
        c.borrow_mut().insert(key.into(), id);
        id
    })
}

// ---- syscalls ----------------------------------------------------------------------------
struct St {
    mode: u32,
    nlink: u32,
    ino: u64,
    dev: u64,
    size: u64,
    blocks: u64,
    mtime: i64,
}
static USE_STATX: AtomicBool = AtomicBool::new(true);

#[inline]
fn mk_dev(major: u64, minor: u64) -> u64 {
    (major << 32) | minor
}

#[inline]
unsafe fn stat_at(dirfd: c_int, name: *const c_char, follow: bool) -> Option<St> {
    let flags = (if follow { 0 } else { AT_SYMLINK_NOFOLLOW }) | AT_NO_AUTOMOUNT;
    if USE_STATX.load(Relaxed) {
        let mut sx: libc::statx = std::mem::zeroed();
        let r = libc::syscall(
            libc::SYS_statx,
            dirfd as libc::c_long,
            name,
            flags as libc::c_long,
            STATX_MASK as libc::c_long,
            &mut sx as *mut libc::statx,
        );
        if r == 0 {
            return Some(St {
                mode: sx.stx_mode as u32,
                nlink: sx.stx_nlink,
                ino: sx.stx_ino,
                dev: mk_dev(sx.stx_dev_major as u64, sx.stx_dev_minor as u64),
                size: sx.stx_size,
                blocks: sx.stx_blocks,
                mtime: sx.stx_mtime.tv_sec,
            });
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOSYS) {
            return None;
        }
        USE_STATX.store(false, Relaxed); // very old kernel: use fstatat from now on
    }
    let mut s: libc::stat = std::mem::zeroed();
    if libc::fstatat(dirfd, name, &mut s, flags & !AT_NO_AUTOMOUNT) == 0 {
        Some(St {
            mode: s.st_mode,
            nlink: s.st_nlink as u32,
            ino: s.st_ino,
            dev: mk_dev(libc::major(s.st_dev) as u64, libc::minor(s.st_dev) as u64),
            size: s.st_size as u64,
            blocks: s.st_blocks as u64,
            mtime: s.st_mtime,
        })
    } else {
        None
    }
}

#[derive(Clone, Copy)]
struct Item {
    off: u32,
    len: u16,
    typ: u8,
}

fn parse_dirents(buf: &[u8], n: usize, items: &mut Vec<Item>) {
    let mut pos = 0usize;
    while pos + 19 <= n {
        let reclen = u16::from_ne_bytes([buf[pos + 16], buf[pos + 17]]) as usize;
        if reclen < 19 || pos + reclen > n {
            break;
        }
        let typ = buf[pos + 18];
        let nm = &buf[pos + 19..pos + reclen];
        let len = nm.iter().position(|&b| b == 0).unwrap_or(nm.len());
        let dot = len == 1 && nm[0] == b'.' || len == 2 && nm[0] == b'.' && nm[1] == b'.';
        if len > 0 && !dot {
            items.push(Item { off: (pos + 19) as u32, len: len as u16, typ });
        }
        pos += reclen;
    }
}

fn open_dir(path_nul: &[u8]) -> c_int {
    unsafe {
        libc::open(
            path_nul.as_ptr() as *const c_char,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    }
}

fn getdents(fd: c_int, buf: &mut [u8]) -> isize {
    unsafe { libc::syscall(libc::SYS_getdents64, fd as libc::c_long, buf.as_mut_ptr(), buf.len()) as isize }
}

// ---- per-directory accumulation ------------------------------------------------------------
#[derive(Default)]
struct Acc {
    files: Vec<FileEntry>,
    names: Vec<u8>,
    subdirs: Vec<u32>,
    tasks: Vec<Task>,
    own_size: u64,
    dfiles: u32,
    mtime: i64,
    small_count: u64,
    small_size: u64,
    small_mtime: i64,
    bytes: u64,
    entries: u64,
    nfiles: u64,
    errors: u64,
}

fn merge_acc(dst: &mut Acc, mut src: Acc) {
    let base = dst.names.len() as u32;
    dst.names.extend_from_slice(&src.names);
    for f in src.files.iter_mut() {
        f.name_off += base;
    }
    dst.files.append(&mut src.files);
    dst.subdirs.append(&mut src.subdirs);
    dst.tasks.append(&mut src.tasks);
    dst.own_size += src.own_size;
    dst.dfiles += src.dfiles;
    dst.mtime = dst.mtime.max(src.mtime);
    dst.small_count += src.small_count;
    dst.small_size += src.small_size;
    dst.small_mtime = dst.small_mtime.max(src.small_mtime);
    dst.bytes += src.bytes;
    dst.entries += src.entries;
    dst.nfiles += src.nfiles;
    dst.errors += src.errors;
}

fn merge_ext(dst: &mut Vec<[u64; 2]>, src: &[[u64; 2]]) {
    if dst.len() < src.len() {
        dst.resize(src.len(), [0, 0]);
    }
    for (d, s) in dst.iter_mut().zip(src) {
        d[0] += s[0];
        d[1] += s[1];
    }
}

#[allow(clippy::too_many_arguments)]
fn process_items(
    sh: &Shared,
    dirfd: c_int,
    buf: &[u8],
    items: &[Item],
    ppath: &[u8],
    pid: u32,
    acc: &mut Acc,
    ext: &mut Vec<[u64; 2]>,
) {
    for it in items {
        acc.entries += 1;
        if it.typ != DT_DIR && it.typ != DT_REG && it.typ != DT_UNKNOWN {
            continue; // symlinks, sockets, fifos, devices: ignored without any syscall
        }
        let name = &buf[it.off as usize..it.off as usize + it.len as usize];
        let st = match unsafe { stat_at(dirfd, buf.as_ptr().add(it.off as usize) as *const c_char, false) } {
            Some(s) => s,
            None => {
                acc.errors += 1;
                continue;
            }
        };
        let fmt = st.mode & S_IFMT;
        let sz = if sh.apparent { st.size } else { st.blocks * 512 };
        if fmt == S_IFDIR {
            if sh.one_fs && st.dev != sh.root_dev {
                continue;
            }
            let id = sh.next_id.fetch_add(1, Relaxed);
            acc.subdirs.push(id);
            let mut path = Vec::with_capacity(ppath.len() + name.len() + 2);
            path.extend_from_slice(ppath);
            if path.last() != Some(&b'/') {
                path.push(b'/');
            }
            path.extend_from_slice(name);
            path.push(0);
            acc.tasks.push(Task { id, parent: pid, inode_size: sz.min(u32::MAX as u64) as u32, mtime: st.mtime, path });
        } else if fmt == S_IFREG {
            let mut size = sz;
            if st.nlink > 1 {
                let key = (st.dev, st.ino);
                let shard = (st.ino.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) as usize % SHARDS;
                if !sh.seen[shard].lock().unwrap().insert(key) {
                    size = 0; // another name of an already counted hard link
                }
            }
            let e = ext_id(name) as usize;
            if e >= ext.len() {
                ext.resize(e + 1, [0, 0]);
            }
            ext[e][0] += size;
            ext[e][1] += 1;
            acc.bytes += size;
            acc.nfiles += 1;
            acc.dfiles += 1;
            acc.own_size += size;
            if st.mtime > acc.mtime {
                acc.mtime = st.mtime;
            }
            if sh.compact_below > 0 && size < sh.compact_below {
                acc.small_count += 1;
                acc.small_size += size;
                acc.small_mtime = acc.small_mtime.max(st.mtime);
            } else {
                let off = acc.names.len() as u32;
                acc.names.extend_from_slice(name);
                acc.files.push(FileEntry { size, mtime: st.mtime, name_off: off, name_len: it.len, ext: e as u16 });
            }
        }
    }
}

struct WorkerCtx {
    buf: Vec<u8>,
    items: Vec<Item>,
    dirs: Vec<(u32, Dir)>,
    ext: Vec<[u64; 2]>,
    top: BinaryHeap<Reverse<(u64, u32, u32)>>,
    dir_count: u64,
}

fn scan_dir(sh: &Shared, w: &mut WorkerCtx, local: &Worker<Task>, task: Task, idx: usize) {
    let path = &task.path[..task.path.len() - 1];
    let name_start = if task.parent == NO_PARENT {
        0 // the root keeps its full path as its name
    } else {
        path.iter().rposition(|&b| b == b'/').map(|p| p + 1).unwrap_or(0)
    };
    let name = &path[name_start..];
    let mut acc = Acc::default();
    acc.names.extend_from_slice(name);
    acc.mtime = task.mtime;
    let name_len = name.len().min(u16::MAX as usize);

    let fd = open_dir(&task.path);
    if fd < 0 {
        sh.prog.errors.fetch_add(1, Relaxed);
    } else {
        loop {
            if sh.prog.cancel.load(Relaxed) {
                break;
            }
            let n = getdents(fd, &mut w.buf);
            if n <= 0 {
                if n < 0 {
                    acc.errors += 1;
                }
                break;
            }
            w.items.clear();
            parse_dirents(&w.buf, n as usize, &mut w.items);
            let (e0, b0, f0, er0) = (acc.entries, acc.bytes, acc.nfiles, acc.errors);
            if w.items.len() >= PAR_MIN {
                let idle = sh.threads.saturating_sub(sh.active.load(Relaxed)) + 1;
                let helpers = idle.clamp(1, 16).min(w.items.len() / 1024).max(1);
                if helpers > 1 {
                    let chunk = w.items.len().div_ceil(helpers);
                    let (buf, items) = (&w.buf, &w.items);
                    let ext_ref = &mut w.ext;
                    std::thread::scope(|s| {
                        let mut handles = Vec::new();
                        for part in items.chunks(chunk).skip(1) {
                            handles.push(s.spawn(move || {
                                let mut a = Acc::default();
                                let mut e = Vec::new();
                                process_items(sh, fd, buf, part, path, task.id, &mut a, &mut e);
                                (a, e)
                            }));
                        }
                        if let Some(first) = items.chunks(chunk).next() {
                            process_items(sh, fd, buf, first, path, task.id, &mut acc, ext_ref);
                        }
                        for h in handles {
                            let (a, e) = h.join().unwrap();
                            merge_acc(&mut acc, a);
                            merge_ext(ext_ref, &e);
                        }
                    });
                } else {
                    process_items(sh, fd, &w.buf, &w.items, path, task.id, &mut acc, &mut w.ext);
                }
            } else {
                process_items(sh, fd, &w.buf, &w.items, path, task.id, &mut acc, &mut w.ext);
            }
            let p = &sh.prog;
            p.entries.fetch_add(acc.entries - e0, Relaxed);
            p.bytes.fetch_add(acc.bytes - b0, Relaxed);
            p.files.fetch_add(acc.nfiles - f0, Relaxed);
            if acc.errors > er0 {
                p.errors.fetch_add(acc.errors - er0, Relaxed);
            }
            if !acc.tasks.is_empty() {
                // publish subdirectories immediately so other workers can steal them
                sh.pending.fetch_add(acc.tasks.len(), Relaxed);
                for t in acc.tasks.drain(..) {
                    local.push(t);
                }
            }
        }
        unsafe {
            libc::close(fd);
        }
    }

    // finalize
    if acc.small_count > 0 {
        let nm = format!("({} small files)", acc.small_count);
        let off = acc.names.len() as u32;
        acc.names.extend_from_slice(nm.as_bytes());
        acc.files.push(FileEntry {
            size: acc.small_size,
            mtime: acc.small_mtime,
            name_off: off,
            name_len: nm.len() as u16,
            ext: EXT_AGG,
        });
    }
    acc.files.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    for (i, f) in acc.files.iter().enumerate() {
        if f.ext == EXT_AGG {
            continue;
        }
        if w.top.len() >= TOP_FILES_PER_WORKER {
            if f.size <= w.top.peek().map(|r| r.0 .0).unwrap_or(0) {
                break;
            }
            w.top.pop();
        }
        w.top.push(Reverse((f.size, task.id, i as u32)));
    }
    acc.files.shrink_to_fit();
    acc.names.shrink_to_fit();
    acc.subdirs.shrink_to_fit();
    let dir = Dir {
        parent: task.parent,
        name_len: name_len as u16,
        alive: true,
        dom_ext: 0,
        inode_size: task.inode_size,
        dfiles: acc.dfiles,
        own_size: task.inode_size as u64 + acc.own_size,
        size: 0,
        nfiles: 0,
        ndirs: 0,
        mtime: acc.mtime,
        names: acc.names,
        files: acc.files,
        subdirs: acc.subdirs,
    };
    w.dirs.push((task.id, dir));
    sh.prog.dirs.fetch_add(1, Relaxed);
    w.dir_count += 1;
    if idx == 0 && w.dir_count % 64 == 1 {
        if let Ok(mut g) = sh.prog.current.try_lock() {
            *g = String::from_utf8_lossy(path).into_owned();
        }
    }
}

fn worker_main(sh: &Shared, idx: usize, local: Worker<Task>, stealers: &[Stealer<Task>]) -> WorkerCtx {
    let mut w = WorkerCtx {
        buf: vec![0u8; BUF_SIZE],
        items: Vec::with_capacity(8192),
        dirs: Vec::new(),
        ext: Vec::new(),
        top: BinaryHeap::new(),
        dir_count: 0,
    };
    let backoff = Backoff::new();
    loop {
        if sh.prog.cancel.load(Relaxed) {
            break;
        }
        let task = local.pop().or_else(|| {
            std::iter::repeat_with(|| {
                sh.injector
                    .steal_batch_and_pop(&local)
                    .or_else(|| stealers.iter().enumerate().filter(|(i, _)| *i != idx).map(|(_, s)| s.steal()).collect())
            })
            .find(|s| !s.is_retry())
            .and_then(Steal::success)
        });
        match task {
            Some(t) => {
                backoff.reset();
                sh.active.fetch_add(1, Relaxed);
                scan_dir(sh, &mut w, &local, t, idx);
                sh.active.fetch_sub(1, Relaxed);
                sh.pending.fetch_sub(1, Relaxed);
            }
            None => {
                if sh.pending.load(Relaxed) == 0 {
                    break;
                }
                if backoff.is_completed() {
                    std::thread::sleep(Duration::from_micros(100));
                } else {
                    backoff.snooze();
                }
            }
        }
    }
    w
}

// ---- progress estimation ----------------------------------------------------------------------
/// Fast entry counter (getdents only, no stat except for directories on `one_fs` scans).
fn count_entries(path: &[u8], one_fs: bool, root_dev: u64, prog: &Progress, stop: &AtomicBool) {
    let mut buf = vec![0u8; BUF_SIZE];
    let mut items: Vec<Item> = Vec::with_capacity(8192);
    let mut stack: Vec<Vec<u8>> = vec![{
        let mut p = path.to_vec();
        p.push(0);
        p
    }];
    let mut total = 0u64;
    while let Some(p) = stack.pop() {
        if stop.load(Relaxed) || prog.cancel.load(Relaxed) {
            return;
        }
        let fd = open_dir(&p);
        if fd < 0 {
            continue;
        }
        loop {
            let n = getdents(fd, &mut buf);
            if n <= 0 {
                break;
            }
            items.clear();
            parse_dirents(&buf, n as usize, &mut items);
            total += items.len() as u64;
            for it in &items {
                let is_dir = match it.typ {
                    DT_DIR => true,
                    DT_UNKNOWN => unsafe {
                        stat_at(fd, buf.as_ptr().add(it.off as usize) as *const c_char, false)
                            .map(|s| s.mode & S_IFMT == S_IFDIR)
                            .unwrap_or(false)
                    },
                    _ => false,
                };
                if !is_dir {
                    continue;
                }
                if one_fs {
                    let ok = unsafe { stat_at(fd, buf.as_ptr().add(it.off as usize) as *const c_char, false) }
                        .map(|s| s.dev == root_dev)
                        .unwrap_or(false);
                    if !ok {
                        continue;
                    }
                }
                let name = &buf[it.off as usize..it.off as usize + it.len as usize];
                let mut np = Vec::with_capacity(p.len() + name.len() + 1);
                np.extend_from_slice(&p[..p.len() - 1]);
                if np.last() != Some(&b'/') {
                    np.push(b'/');
                }
                np.extend_from_slice(name);
                np.push(0);
                stack.push(np);
            }
            prog.total.store(total, Relaxed);
        }
        unsafe {
            libc::close(fd);
        }
    }
    prog.total.store(total, Relaxed);
    prog.total_ready.store(true, Relaxed);
}

fn is_rotational(dev: u64) -> bool {
    let (major, minor) = (dev >> 32, dev & 0xffff_ffff);
    if major == 0 {
        return false; // btrfs/zfs/overlay: unknown, assume SSD
    }
    for suffix in ["queue/rotational", "../queue/rotational"] {
        if let Ok(s) = std::fs::read_to_string(format!("/sys/dev/block/{major}:{minor}/{suffix}")) {
            return s.trim() == "1";
        }
    }
    false
}

pub fn auto_threads(root_dev: u64) -> usize {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    if is_rotational(root_dev) {
        return 4.min(cores.max(2));
    }
    (cores * 2).clamp(4, 64)
}

// ---- driver -------------------------------------------------------------------------------------
pub fn run_scan(opts: ScanOpts, prog: Arc<Progress>) -> Option<ScanResult> {
    let root = std::fs::canonicalize(&opts.path).ok()?;
    let root_s = root.to_string_lossy().into_owned();
    let root_c = std::ffi::CString::new(root_s.clone()).ok()?;
    let rst = unsafe { stat_at(AT_FDCWD, root_c.as_ptr(), true)? };
    if rst.mode & S_IFMT != S_IFDIR {
        return None;
    }
    reset_ext_table();
    let threads = if opts.threads == 0 { auto_threads(rst.dev) } else { opts.threads.max(1) };
    prog.threads.store(threads, Relaxed);
    prog.phase.store(1, Relaxed);

    // progress denominator: inode count for whole filesystems, background counter otherwise
    let mut use_counter = false;
    if opts.one_fs && is_mount_point(&root_s) {
        let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(root_c.as_ptr(), &mut vfs) } == 0 && vfs.f_files > 0 {
            prog.total.store((vfs.f_files as u64).saturating_sub(vfs.f_ffree as u64), Relaxed);
            prog.total_ready.store(true, Relaxed);
        } else {
            use_counter = opts.precount;
        }
    } else {
        use_counter = opts.precount;
    }

    let sh = Shared {
        one_fs: opts.one_fs,
        apparent: opts.apparent,
        compact_below: opts.compact_below,
        root_dev: rst.dev,
        threads,
        next_id: AtomicU32::new(1),
        pending: AtomicUsize::new(1),
        active: AtomicUsize::new(0),
        injector: Injector::new(),
        seen: (0..SHARDS).map(|_| Mutex::new(FxHashSet::default())).collect(),
        prog: prog.clone(),
    };
    let rsz = if opts.apparent { rst.size } else { rst.blocks * 512 };
    let mut rp = root_s.clone().into_bytes();
    rp.push(0);
    sh.injector.push(Task { id: 0, parent: NO_PARENT, inode_size: rsz.min(u32::MAX as u64) as u32, mtime: rst.mtime, path: rp });

    let workers: Vec<Worker<Task>> = (0..threads).map(|_| Worker::new_lifo()).collect();
    let stealers: Vec<Stealer<Task>> = workers.iter().map(|w| w.stealer()).collect();
    let stop_counter = AtomicBool::new(false);
    let locals: Vec<WorkerCtx> = std::thread::scope(|s| {
        if use_counter {
            let (p, st) = (&*prog, &stop_counter);
            let (rs, od, rd) = (root_s.as_bytes(), opts.one_fs, rst.dev);
            s.spawn(move || count_entries(rs, od, rd, p, st));
        }
        let handles: Vec<_> = workers
            .into_iter()
            .enumerate()
            .map(|(i, w)| {
                let (sh, stealers) = (&sh, &stealers);
                std::thread::Builder::new()
                    .name(format!("scan-{i}"))
                    .spawn_scoped(s, move || worker_main(sh, i, w, stealers))
                    .expect("spawn worker")
            })
            .collect();
        let out: Vec<WorkerCtx> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        stop_counter.store(true, Relaxed);
        out
    });
    let cancelled = prog.cancel.load(Relaxed);
    prog.phase.store(2, Relaxed);

    // gather
    let n = sh.next_id.load(Relaxed) as usize;
    let mut slots: Vec<Option<Dir>> = Vec::new();
    slots.resize_with(n, || None);
    let mut ext_stats: Vec<[u64; 2]> = Vec::new();
    let mut top_all: Vec<(u64, u32, u32)> = Vec::new();
    for l in locals {
        for (id, d) in l.dirs {
            slots[id as usize] = Some(d);
        }
        merge_ext(&mut ext_stats, &l.ext);
        top_all.extend(l.top.into_iter().map(|r| r.0));
    }
    let mut dirs: Vec<Dir> = slots.into_iter().map(|o| o.unwrap_or_else(Dir::placeholder)).collect();
    if dirs.is_empty() || !dirs[0].alive {
        return None;
    }
    aggregate(&mut dirs);
    top_all.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    top_all.truncate(TOP_FILES_KEPT);
    let top_files = top_all.into_iter().map(|(_, d, i)| NodeId::file(d, i)).collect();
    let ext_names: Vec<String> =
        ext_table().lock().unwrap().names.iter().map(|n| String::from_utf8_lossy(n).into_owned()).collect();
    ext_stats.resize(ext_names.len().max(ext_stats.len()), [0, 0]);
    Some(ScanResult {
        tree: Tree { dirs, ext_names },
        ext_stats,
        top_files,
        errors: prog.errors.load(Relaxed),
        cancelled,
        threads,
    })
}

fn is_mount_point(path: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    let p = std::path::Path::new(path);
    match (std::fs::metadata(p), p.parent().and_then(|q| std::fs::metadata(q).ok())) {
        (Ok(a), Some(b)) => a.dev() != b.dev(),
        (Ok(_), None) => true, // "/"
        _ => false,
    }
}

/// Sizes bottom-up. Child ids are always larger than their parent's id, so a single reverse
/// pass sees every child complete before its parent.
fn aggregate(dirs: &mut [Dir]) {
    let n = dirs.len();
    for i in 0..n {
        let subs = std::mem::take(&mut dirs[i].subdirs);
        for &c in &subs {
            dirs[c as usize].parent = i as u32;
        }
        dirs[i].subdirs = subs;
        dirs[i].size = dirs[i].own_size;
        dirs[i].nfiles = dirs[i].dfiles as u64;
        dirs[i].ndirs = 0;
    }
    for i in (0..n).rev() {
        let mut subs = std::mem::take(&mut dirs[i].subdirs);
        subs.retain(|&c| dirs[c as usize].alive);
        subs.sort_unstable_by_key(|&c| Reverse(dirs[c as usize].size));
        let mut best_size = 0u64;
        let mut best_ext = EXT_NONE;
        if let Some(f) = dirs[i].files.iter().take(2).find(|f| f.ext != EXT_AGG) {
            best_size = f.size;
            best_ext = f.ext;
        }
        if let Some(&c) = subs.first() {
            if dirs[c as usize].size > best_size {
                best_ext = dirs[c as usize].dom_ext;
            }
        }
        dirs[i].subdirs = subs;
        dirs[i].dom_ext = best_ext;
        let p = dirs[i].parent;
        if p != NO_PARENT && dirs[i].alive {
            let (s, f, d, m) = (dirs[i].size, dirs[i].nfiles, dirs[i].ndirs + 1, dirs[i].mtime);
            let pd = &mut dirs[p as usize];
            pd.size += s;
            pd.nfiles += f;
            pd.ndirs += d;
            if m > pd.mtime {
                pd.mtime = m;
            }
        }
    }
}
