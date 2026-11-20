//! Application state and logic (no drawing code – see ui.rs).
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::scan::*;
use crate::theme::{self, Theme};
use crate::treemap::{Layout, Look};
use crate::units::{fmt_size, Base, Unit};

pub const LIST_LIMIT: usize = 300;
pub const MAX_ROWS: usize = 100_000;
pub const COMPACT_OPTS: [(u64, &str); 5] = [
    (0, "Off (keep every file)"),
    (1024, "Smaller than 1 KiB"),
    (4096, "Smaller than 4 KiB"),
    (65536, "Smaller than 64 KiB"),
    (1 << 20, "Smaller than 1 MiB"),
];
pub const THREAD_OPTS: [usize; 8] = [0, 1, 2, 4, 8, 16, 32, 64];

// ---------------------------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------------------------
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub brightness: f32,
    pub saturation: f32,
    pub edge: f32,
    pub classic_colors: bool,
    pub one_fs: bool,
    pub apparent: bool,
    pub precount: bool,
    pub threads: usize,
    pub compact_idx: usize,
    pub base: Base,
    pub unit: Unit,
    pub folder_ge: bool,
    pub folder_n: u32,
    pub folder_recursive: bool,
    pub scope_selected: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: "Catppuccin Mocha".into(),
            brightness: 100.0,
            saturation: 100.0,
            edge: 50.0,
            classic_colors: false,
            one_fs: true,
            apparent: false,
            precount: true,
            threads: 0,
            compact_idx: 0,
            base: Base::Binary,
            unit: Unit::Auto,
            folder_ge: true,
            folder_n: 100,
            folder_recursive: true,
            scope_selected: false,
        }
    }
}

impl Settings {
    fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("windirstat-linux").join("settings-native.json")
    }
    pub fn load() -> Settings {
        std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save(&self) {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// small types
// ---------------------------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    Tree,
    Treemap,
    List,
}

pub enum Action {
    Select(NodeId, Origin),
    Toggle(NodeId),
    Sort(usize),
    Hide(NodeId),
    Show(NodeId),
    Trash(NodeId),
    Delete(NodeId),
    Open(String),
    CopyPath(String),
    HighlightExt(Option<u16>),
}

#[derive(Clone, Copy)]
pub struct Row {
    pub node: NodeId,
    pub depth: u16,
    pub psize: u64,
    pub more: u32, // > 0: "… N more items" row
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SideTab {
    Extensions,
    Folders,
    Files,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    Trash,
    Delete,
}

pub struct Confirm {
    pub node: NodeId,
    pub kind: ConfirmKind,
    pub text: String,
}

pub struct Picker {
    pub path: String,
    pub entries: Vec<String>,
    pub mounts: Vec<(String, String)>,
    pub error: String,
}

pub struct ScanJob {
    pub prog: Arc<Progress>,
    pub handle: Option<JoinHandle<Option<ScanResult>>>,
    pub started: Instant,
}

pub struct TmState {
    pub layout: Option<Layout>,
    pub tex: Option<egui::TextureHandle>,
    pub want: (usize, usize),
    pub want_since: Instant,
    pub layout_dirty: bool,
    pub paint_dirty: bool,
}

// ---------------------------------------------------------------------------------------------
// app
// ---------------------------------------------------------------------------------------------
pub struct App {
    pub cfg: Settings,
    pub cfg_dirty: Option<Instant>,
    pub themes: Vec<Theme>,
    pub theme_idx: usize,

    pub job: Option<ScanJob>,
    pub tree: Option<Tree>,
    pub scan_path: String,
    pub summary: String,
    pub free: Option<(u64, u64)>,
    pub ext_stats: Vec<[u64; 2]>,
    pub ext_rows: Vec<u16>,
    pub ext_colors: Vec<[u8; 3]>,
    pub top_files: Vec<NodeId>,

    pub selected: Option<NodeId>,
    pub scope_dir: u32,
    pub expanded: FxHashSet<NodeId>,
    pub rows: Vec<Row>,
    pub rows_dirty: bool,
    pub scroll_pending: bool,
    pub scroll_to: Option<usize>,
    pub sort_col: usize,
    pub sort_desc: bool,

    pub hidden: FxHashSet<NodeId>,
    pub cut: FxHashMap<u32, u64>,

    pub tm: TmState,
    pub hl_ext: Option<u16>,
    pub hover: Option<NodeId>,
    pub ctx_node: Option<NodeId>,

    pub tab: SideTab,
    pub folder_list: Vec<u32>,
    pub folder_matches: usize,
    pub file_list: Vec<NodeId>,
    pub lists_dirty: Option<Instant>,

    pub picker: Option<Picker>,
    pub confirm: Option<Confirm>,
    pub about: bool,
    pub acts: Vec<Action>,
    pub message: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Option<String>) -> App {
        let cfg = Settings::load();
        let themes = theme::all_themes();
        let theme_idx = themes.iter().position(|t| t.name == cfg.theme).unwrap_or(3);
        theme::apply(&cc.egui_ctx, &themes[theme_idx]);
        let mut app = App {
            cfg,
            cfg_dirty: None,
            themes,
            theme_idx,
            job: None,
            tree: None,
            scan_path: String::new(),
            summary: String::new(),
            free: None,
            ext_stats: Vec::new(),
            ext_rows: Vec::new(),
            ext_colors: Vec::new(),
            top_files: Vec::new(),
            selected: None,
            scope_dir: 0,
            expanded: FxHashSet::default(),
            rows: Vec::new(),
            rows_dirty: false,
            scroll_pending: false,
            scroll_to: None,
            sort_col: 3,
            sort_desc: true,
            hidden: FxHashSet::default(),
            cut: FxHashMap::default(),
            tm: TmState {
                layout: None,
                tex: None,
                want: (0, 0),
                want_since: Instant::now(),
                layout_dirty: false,
                paint_dirty: false,
            },
            hl_ext: None,
            hover: None,
            ctx_node: None,
            tab: SideTab::Folders,
            folder_list: Vec::new(),
            folder_matches: 0,
            file_list: Vec::new(),
            lists_dirty: None,
            picker: None,
            confirm: None,
            about: false,
            acts: Vec::new(),
            message: String::new(),
        };
        match start {
            Some(p) => app.start_scan(p),
            None => app.open_picker(),
        }
        app
    }

    pub fn fmt(&self, bytes: u64) -> String {
        fmt_size(bytes, self.cfg.base, self.cfg.unit)
    }
    pub fn touch_cfg(&mut self) {
        self.cfg_dirty = Some(Instant::now());
    }
    pub fn theme(&self) -> &Theme {
        &self.themes[self.theme_idx]
    }
    pub fn scanning(&self) -> bool {
        self.job.is_some()
    }
    pub fn look(&self) -> Look {
        Look { brightness: self.cfg.brightness / 100.0, saturation: self.cfg.saturation / 100.0, edge: self.cfg.edge / 100.0 }
    }

    // ---- scanning -----------------------------------------------------------------------------
    pub fn open_picker(&mut self) {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let mut p = Picker { path: home, entries: Vec::new(), mounts: crate::util::list_mounts(), error: String::new() };
        p.refresh();
        self.picker = Some(p);
    }

    pub fn start_scan(&mut self, path: String) {
        if self.job.is_some() {
            return;
        }
        let opts = ScanOpts {
            path: path.clone(),
            one_fs: self.cfg.one_fs,
            apparent: self.cfg.apparent,
            threads: self.cfg.threads,
            compact_below: COMPACT_OPTS[self.cfg.compact_idx.min(COMPACT_OPTS.len() - 1)].0,
            precount: self.cfg.precount,
        };
        let prog = Arc::new(Progress::default());
        let p2 = prog.clone();
        let handle = std::thread::Builder::new()
            .name("scan-main".into())
            .spawn(move || run_scan(opts, p2))
            .expect("spawn scan thread");
        self.job = Some(ScanJob { prog, handle: Some(handle), started: Instant::now() });
        self.scan_path = path;
        self.hidden.clear();
        self.cut.clear();
        self.message.clear();
    }

    pub fn stop_scan(&mut self) {
        if let Some(j) = &self.job {
            j.prog.cancel.store(true, Relaxed);
        }
    }

    pub fn poll_scan(&mut self, ctx: &egui::Context) {
        let done = self.job.as_ref().is_some_and(|j| j.handle.as_ref().is_some_and(|h| h.is_finished()));
        if done {
            let mut job = self.job.take().unwrap();
            let res = job.handle.take().unwrap().join().ok().flatten();
            self.finish_scan(res, job.started.elapsed());
        } else if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn finish_scan(&mut self, res: Option<ScanResult>, wall: Duration) {
        let Some(r) = res else {
            self.message = format!("Cannot scan {}", self.scan_path);
            return;
        };
        let n_files = r.tree.dirs[0].nfiles;
        let n_dirs = r.tree.dirs[0].ndirs;
        let size = r.tree.dirs[0].size;
        self.summary = format!(
            "{}{} files in {} folders, {} — {:.2}s with {} threads{}",
            if r.cancelled { "Scan stopped, partial results: " } else { "Scanned " },
            crate::units::fmt_int(n_files),
            crate::units::fmt_int(n_dirs),
            fmt_size(size, self.cfg.base, self.cfg.unit),
            wall.as_secs_f64(),
            r.threads,
            if r.errors > 0 { format!("  ({} items unreadable)", crate::units::fmt_int(r.errors)) } else { String::new() }
        );
        self.ext_stats = r.ext_stats;
        self.top_files = r.top_files;
        self.tree = Some(r.tree);
        self.free = crate::util::free_space(&self.scan_path);
        self.selected = Some(NodeId::dir(0));
        self.scope_dir = 0;
        self.expanded.clear();
        self.expanded.insert(NodeId::dir(0));
        self.hidden.clear();
        self.cut.clear();
        self.hl_ext = None;
        self.sort_col = 3;
        self.sort_desc = true;
        self.rows_dirty = true;
        self.recompute_colors();
        self.tm.layout_dirty = true;
        self.lists_dirty = Some(Instant::now() - Duration::from_secs(1));
    }

    pub fn recompute_colors(&mut self) {
        let Some(tree) = &self.tree else { return };
        let t = &self.themes[self.theme_idx];
        let pal: Vec<[u8; 3]> = match (&t.palette, self.cfg.classic_colors) {
            (Some(p), false) => p.clone(),
            _ => theme::classic_palette(),
        };
        let mut ids: Vec<u16> =
            (0..tree.ext_names.len()).filter(|&i| self.ext_stats.get(i).is_some_and(|s| s[1] > 0)).map(|i| i as u16).collect();
        ids.sort_unstable_by_key(|&i| Reverse(self.ext_stats[i as usize][0]));
        let other = if t.palette.is_some() { t.other } else { [150, 150, 150] };
        let mut cols = vec![other; tree.ext_names.len()];
        for (rank, &id) in ids.iter().enumerate() {
            if let Some(c) = pal.get(rank) {
                cols[id as usize] = *c;
            }
        }
        self.ext_rows = ids;
        self.ext_colors = cols;
        self.tm.paint_dirty = true;
    }

    // ---- rows ---------------------------------------------------------------------------------
    pub fn rebuild_rows(&mut self) {
        self.rows_dirty = false;
        let Some(tree) = &self.tree else {
            self.rows.clear();
            return;
        };
        let mut rows = Vec::with_capacity(self.rows.len().max(64));
        add_rows(tree, &self.expanded, self.sort_col, self.sort_desc, &mut rows, NodeId::dir(0), 0, 0);
        self.rows = rows;
        if self.scroll_pending {
            self.scroll_pending = false;
            if let Some(sel) = self.selected {
                self.scroll_to = self.rows.iter().position(|r| r.node == sel && r.more == 0);
            }
        }
    }

    pub fn select(&mut self, node: NodeId, origin: Origin) {
        let Some(tree) = &self.tree else { return };
        if tree.is_deleted(node) || node.is_agg() {
            return;
        }
        self.selected = Some(node);
        if origin != Origin::Tree {
            let mut a = tree.parent(node);
            while let Some(p) = a {
                self.expanded.insert(p);
                a = tree.parent(p);
            }
            self.rows_dirty = true;
            self.scroll_pending = true;
        }
        if origin != Origin::List {
            let sd = node.dir_id();
            if sd != self.scope_dir {
                self.scope_dir = sd;
                if self.cfg.scope_selected {
                    self.lists_dirty = Some(Instant::now());
                }
            }
        }
    }

    // ---- hiding -------------------------------------------------------------------------------
    pub fn set_hidden(&mut self, node: NodeId, flag: bool) {
        if flag {
            self.hidden.insert(node);
        } else {
            self.hidden.remove(&node);
        }
        self.after_hidden_change();
    }

    pub fn clear_hidden(&mut self) {
        self.hidden.clear();
        self.after_hidden_change();
    }

    fn after_hidden_change(&mut self) {
        if let Some(t) = &self.tree {
            self.cut = compute_cut(t, &self.hidden);
        }
        self.tm.layout_dirty = true;
    }

    // ---- lists --------------------------------------------------------------------------------
    pub fn refresh_lists(&mut self) {
        self.lists_dirty = None;
        let Some(tree) = &self.tree else {
            self.folder_list.clear();
            self.file_list.clear();
            return;
        };
        let scoped = self.cfg.scope_selected && self.scope_dir != 0;
        let sub: Option<Vec<u32>> = scoped.then(|| subtree_dirs(tree, self.scope_dir));
        let (list, matches) = top_dirs(
            tree,
            sub.as_deref(),
            self.cfg.folder_recursive,
            self.cfg.folder_ge,
            self.cfg.folder_n as u64,
            LIST_LIMIT,
        );
        self.folder_list = list;
        self.folder_matches = matches;
        self.file_list = if scoped {
            let mut dirs = vec![self.scope_dir];
            dirs.extend(sub.unwrap_or_default());
            top_files_in(tree, &dirs, LIST_LIMIT)
        } else {
            self.top_files.iter().copied().filter(|&n| !tree.is_deleted(n)).take(LIST_LIMIT).collect()
        };
    }

    pub fn schedule_lists(&mut self) {
        self.lists_dirty = Some(Instant::now());
    }

    // ---- actions ------------------------------------------------------------------------------
    pub fn process_actions(&mut self) {
        let acts = std::mem::take(&mut self.acts);
        for a in acts {
            match a {
                Action::Select(n, o) => self.select(n, o),
                Action::Toggle(n) => {
                    if !self.expanded.remove(&n) {
                        self.expanded.insert(n);
                    }
                    self.rows_dirty = true;
                }
                Action::Sort(col) => {
                    if self.sort_col == col {
                        self.sort_desc = !self.sort_desc;
                    } else {
                        self.sort_col = col;
                        self.sort_desc = col != 0;
                    }
                    self.rows_dirty = true;
                }
                Action::Hide(n) => self.set_hidden(n, true),
                Action::Show(n) => self.set_hidden(n, false),
                Action::Trash(n) => self.ask(n, ConfirmKind::Trash),
                Action::Delete(n) => self.ask(n, ConfirmKind::Delete),
                Action::Open(p) => crate::util::open_path(&p),
                Action::CopyPath(p) => self.message = format!("Copied: {p}"),
                Action::HighlightExt(e) => {
                    self.hl_ext = e;
                    self.tm.paint_dirty = true;
                }
            }
        }
    }

    fn ask(&mut self, node: NodeId, kind: ConfirmKind) {
        let Some(tree) = &self.tree else { return };
        let mut text = format!("{}\n\n{}", tree.path(node), self.fmt(tree.size(node)));
        if node.is_dir() {
            text += &format!(", {} files", crate::units::fmt_int(tree.dirs[node.0 as usize].nfiles));
        }
        if kind == ConfirmKind::Delete {
            text += "\n\nThis cannot be undone.";
        }
        self.confirm = Some(Confirm { node, kind, text });
    }

    pub fn run_confirmed(&mut self) {
        let Some(c) = self.confirm.take() else { return };
        let Some(tree) = &self.tree else { return };
        let path = tree.path(c.node);
        let res: Result<(), String> = match c.kind {
            ConfirmKind::Trash => std::process::Command::new("gio")
                .args(["trash", &path])
                .output()
                .map_err(|e| e.to_string())
                .and_then(|o| if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).into_owned()) }),
            ConfirmKind::Delete => {
                if c.node.is_dir() {
                    std::fs::remove_dir_all(&path).map_err(|e| e.to_string())
                } else {
                    std::fs::remove_file(&path).map_err(|e| e.to_string())
                }
            }
        };
        match res {
            Err(e) => self.message = format!("Could not remove: {e}"),
            Ok(()) => self.remove_node(c.node),
        }
    }

    fn remove_node(&mut self, node: NodeId) {
        let Some(tree) = &mut self.tree else { return };
        let parent = tree.parent(node);
        let (_, _, dec) = tree.remove(node);
        for (e, sz) in dec {
            if let Some(s) = self.ext_stats.get_mut(e as usize) {
                s[0] = s[0].saturating_sub(sz);
                s[1] = s[1].saturating_sub(1);
            }
        }
        let tree = self.tree.as_ref().unwrap();
        self.hidden.retain(|&h| !tree.is_deleted(h));
        self.cut = compute_cut(tree, &self.hidden);
        if let Some(p) = parent {
            self.selected = Some(p);
            if tree.is_deleted(NodeId::dir(self.scope_dir)) {
                self.scope_dir = p.dir_id();
            }
        }
        self.rows_dirty = true;
        self.recompute_colors();
        self.tm.layout_dirty = true;
        self.refresh_lists();
        self.message = "Removed".into();
    }
}

// ---------------------------------------------------------------------------------------------
// free helpers
// ---------------------------------------------------------------------------------------------
pub fn hidden_owner(tree: &Tree, hidden: &FxHashSet<NodeId>, node: NodeId) -> Option<NodeId> {
    if hidden.is_empty() {
        return None;
    }
    let mut n = Some(node);
    while let Some(x) = n {
        if hidden.contains(&x) {
            return Some(x);
        }
        n = tree.parent(x);
    }
    None
}

pub fn compute_cut(tree: &Tree, hidden: &FxHashSet<NodeId>) -> FxHashMap<u32, u64> {
    let mut cut: FxHashMap<u32, u64> = FxHashMap::default();
    for &h in hidden {
        if tree.is_deleted(h) {
            continue;
        }
        if let Some(p) = tree.parent(h) {
            if hidden_owner(tree, hidden, p).is_some() {
                continue; // already covered by a hidden ancestor
            }
        }
        let sz = tree.size(h);
        let mut a = tree.parent(h);
        while let Some(p) = a {
            *cut.entry(p.dir_id()).or_insert(0) += sz;
            a = tree.parent(p);
        }
    }
    cut
}

fn cmp_nodes(tree: &Tree, col: usize, a: NodeId, b: NodeId) -> Ordering {
    let items = |n: NodeId| if n.is_dir() { tree.dirs[n.0 as usize].nfiles + tree.dirs[n.0 as usize].ndirs } else { 0 };
    let files = |n: NodeId| if n.is_dir() { tree.dirs[n.0 as usize].nfiles } else { 0 };
    let subs = |n: NodeId| if n.is_dir() { tree.dirs[n.0 as usize].ndirs } else { 0 };
    match col {
        0 => {
            let (da, db) = (&tree.dirs[a.dir_id() as usize], &tree.dirs[b.dir_id() as usize]);
            let na: &[u8] = if a.is_dir() { da.name() } else { da.file_name(tree.file(a)) };
            let nb: &[u8] = if b.is_dir() { db.name() } else { db.file_name(tree.file(b)) };
            na.iter().map(u8::to_ascii_lowercase).cmp(nb.iter().map(u8::to_ascii_lowercase))
        }
        4 => items(a).cmp(&items(b)),
        5 => files(a).cmp(&files(b)),
        6 => subs(a).cmp(&subs(b)),
        7 => tree.mtime(a).cmp(&tree.mtime(b)),
        _ => tree.size(a).cmp(&tree.size(b)),
    }
}

/// Children of directory `d` in the requested order (dirs and files interleaved).
pub fn child_nodes(tree: &Tree, d: u32, col: usize, desc: bool) -> Vec<NodeId> {
    let dir = &tree.dirs[d as usize];
    let mut v: Vec<NodeId> = Vec::with_capacity(dir.subdirs.len() + dir.files.len());
    let by_size = matches!(col, 1 | 2 | 3);
    if by_size && desc {
        let (mut i, mut j) = (0usize, 0usize);
        loop {
            while j < dir.files.len() && dir.files[j].ext == EXT_DEL {
                j += 1;
            }
            let ds = dir.subdirs.get(i).map(|&c| tree.dirs[c as usize].size);
            let fs = dir.files.get(j).map(|f| f.size);
            match (ds, fs) {
                (None, None) => break,
                (Some(_), None) => {
                    v.push(NodeId::dir(dir.subdirs[i]));
                    i += 1;
                }
                (None, Some(_)) => {
                    v.push(NodeId::file(d, j as u32));
                    j += 1;
                }
                (Some(a), Some(b)) => {
                    if a >= b {
                        v.push(NodeId::dir(dir.subdirs[i]));
                        i += 1;
                    } else {
                        v.push(NodeId::file(d, j as u32));
                        j += 1;
                    }
                }
            }
        }
        return v;
    }
    v.extend(dir.subdirs.iter().map(|&c| NodeId::dir(c)));
    v.extend(dir.files.iter().enumerate().filter(|(_, f)| f.ext != EXT_DEL).map(|(i, _)| NodeId::file(d, i as u32)));
    v.sort_by(|&a, &b| {
        let o = cmp_nodes(tree, col, a, b);
        if desc { o.reverse() } else { o }
    });
    v
}

#[allow(clippy::too_many_arguments)]
fn add_rows(
    tree: &Tree,
    expanded: &FxHashSet<NodeId>,
    col: usize,
    desc: bool,
    rows: &mut Vec<Row>,
    node: NodeId,
    depth: u16,
    psize: u64,
) {
    rows.push(Row { node, depth, psize, more: 0 });
    if node.is_dir() && expanded.contains(&node) {
        let kids = child_nodes(tree, node.0 as u32, col, desc);
        let size = tree.size(node);
        for &k in kids.iter().take(MAX_ROWS) {
            add_rows(tree, expanded, col, desc, rows, k, depth + 1, size);
        }
        if kids.len() > MAX_ROWS {
            rows.push(Row { node: NodeId::agg(node.0 as u32), depth: depth + 1, psize: size, more: (kids.len() - MAX_ROWS) as u32 });
        }
    }
}

pub fn subtree_dirs(tree: &Tree, root: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        for &c in &tree.dirs[d as usize].subdirs {
            out.push(c);
            stack.push(c);
        }
    }
    out
}

/// Largest folders that satisfy the files-inside filter (parallel over all cores for big trees).
pub fn top_dirs(tree: &Tree, cand: Option<&[u32]>, recursive: bool, ge: bool, n: u64, k: usize) -> (Vec<u32>, usize) {
    let total = cand.map(|c| c.len()).unwrap_or(tree.dirs.len());
    let nt = if total < 200_000 { 1 } else { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16) };
    let chunk = total.div_ceil(nt).max(1);
    let work = |lo: usize, hi: usize| -> (BinaryHeap<Reverse<(u64, u32)>>, usize) {
        let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
        let mut cnt = 0usize;
        for i in lo..hi {
            let id = match cand {
                Some(c) => c[i],
                None => i as u32,
            };
            if cand.is_none() && id == 0 {
                continue;
            }
            let d = &tree.dirs[id as usize];
            if !d.alive {
                continue;
            }
            let v = if recursive { d.nfiles } else { d.dfiles as u64 };
            if if ge { v < n } else { v > n } {
                continue;
            }
            cnt += 1;
            if heap.len() < k {
                heap.push(Reverse((d.size, id)));
            } else if d.size > heap.peek().map(|r| r.0 .0).unwrap_or(0) {
                heap.pop();
                heap.push(Reverse((d.size, id)));
            }
        }
        (heap, cnt)
    };
    let mut merged: Vec<(u64, u32)> = Vec::new();
    let mut matches = 0usize;
    if nt == 1 {
        let (h, c) = work(0, total);
        merged.extend(h.into_iter().map(|r| r.0));
        matches = c;
    } else {
        std::thread::scope(|s| {
            let hs: Vec<_> = (0..nt)
                .map(|t| {
                    let (lo, hi) = (t * chunk, ((t + 1) * chunk).min(total));
                    let work = &work;
                    s.spawn(move || work(lo.min(hi), hi))
                })
                .collect();
            for h in hs {
                let (heap, c) = h.join().unwrap();
                merged.extend(heap.into_iter().map(|r| r.0));
                matches += c;
            }
        });
    }
    merged.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    merged.truncate(k);
    (merged.into_iter().map(|(_, id)| id).collect(), matches)
}

pub fn top_files_in(tree: &Tree, dirs: &[u32], k: usize) -> Vec<NodeId> {
    let mut heap: BinaryHeap<Reverse<(u64, NodeId)>> = BinaryHeap::new();
    for &d in dirs {
        let dir = &tree.dirs[d as usize];
        if !dir.alive {
            continue;
        }
        for (i, f) in dir.files.iter().enumerate() {
            if f.ext == EXT_AGG || f.ext == EXT_DEL {
                continue;
            }
            if heap.len() >= k {
                if f.size <= heap.peek().map(|r| r.0 .0).unwrap_or(0) {
                    break; // files are sorted by size, nothing larger follows
                }
                heap.pop();
            }
            heap.push(Reverse((f.size, NodeId::file(d, i as u32))));
        }
    }
    let mut v: Vec<(u64, NodeId)> = heap.into_iter().map(|r| r.0).collect();
    v.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|(_, n)| n).collect()
}

impl Picker {
    pub fn refresh(&mut self) {
        self.entries.clear();
        self.error.clear();
        match std::fs::read_dir(&self.path) {
            Ok(rd) => {
                for e in rd.flatten() {
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        self.entries.push(e.file_name().to_string_lossy().into_owned());
                    }
                }
                self.entries.sort_by_key(|s| s.to_lowercase());
            }
            Err(e) => self.error = e.to_string(),
        }
    }
    pub fn go_up(&mut self) {
        if let Some(p) = std::path::Path::new(&self.path).parent() {
            self.path = p.to_string_lossy().into_owned();
            if self.path.is_empty() {
                self.path = "/".into();
            }
            self.refresh();
        }
    }
    pub fn enter(&mut self, name: &str) {
        let p = std::path::Path::new(&self.path).join(name);
        self.path = p.to_string_lossy().into_owned();
        self.refresh();
    }
}
