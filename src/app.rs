//! Application state and logic (no drawing code – see ui.rs).
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::savefile::{self, Meta};
use crate::scan::*;
use crate::theme::{self, Theme};
use crate::treemap::{Layout, Look};
use crate::units::{fmt_int, fmt_size, fmt_time, Base, Unit};
use crate::util::Target;

pub const LIST_LIMIT: usize = 300;
pub const SEARCH_LIMIT: usize = 2000;
pub const MAX_ROWS: usize = 100_000;
pub const MAX_RECENT: usize = 12;
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
    // window and layout, restored on start
    pub win_w: f32,
    pub win_h: f32,
    pub win_pos: Option<[f32; 2]>,
    pub win_max: bool,
    pub zoom: f32,
    pub side_w: f32,
    pub tm_h: Option<f32>,
    // history and saved scans
    pub recent: Vec<String>,
    pub auto_save: bool,
    pub search_files: bool,
    pub search_dirs: bool,
    pub search_case: bool,
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
            win_w: 1500.0,
            win_h: 900.0,
            win_pos: None,
            win_max: false,
            zoom: 1.0,
            side_w: 540.0,
            tm_h: None,
            recent: Vec::new(),
            auto_save: false,
            search_files: true,
            search_dirs: true,
            search_case: false,
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
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, &p);
            }
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
    /// one of the side-panel lists (the one on the current tab)
    List,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelMode {
    /// plain click: select only this
    Replace,
    /// Ctrl+click: add or remove this one
    Toggle,
    /// Shift+click / drag: everything between the anchor and this
    Range,
}

impl SelMode {
    pub fn from_mods(m: egui::Modifiers) -> SelMode {
        if m.shift {
            SelMode::Range
        } else if m.command || m.ctrl {
            SelMode::Toggle
        } else {
            SelMode::Replace
        }
    }
}

pub enum Action {
    Select(NodeId, Origin, SelMode),
    /// right click: keep a multi-selection that contains the node, else select just it
    Context(NodeId, Origin),
    Toggle(NodeId),
    Sort(usize),
    Hide(Vec<NodeId>),
    Show(Vec<NodeId>),
    Trash(Vec<NodeId>),
    Delete(Vec<NodeId>),
    Refresh(NodeId),
    Open(String),
    ShowInFm(String),
    Terminal(String),
    CopyText(String, usize),
    HighlightExt(Option<u16>),
    TmZoom(u32),
    FindExt(u16),
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
    Search,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    Trash,
    Delete,
}

pub struct Confirm {
    pub nodes: Vec<NodeId>,
    pub kind: ConfirmKind,
    pub text: String,
}

pub struct Picker {
    pub path: String,
    pub entries: Vec<String>,
    pub mounts: Vec<(String, String)>,
    pub error: String,
    pub saved: Vec<(PathBuf, Meta, u64)>,
}

pub enum JobKind {
    Scan,
    Load(PathBuf),
    /// rescan of one folder, grafted into the current tree
    Refresh(u32),
}

pub struct ScanJob {
    pub prog: Arc<Progress>,
    pub handle: Option<JoinHandle<Result<ScanResult, String>>>,
    pub started: Instant,
    pub kind: JobKind,
    pub path: String,
}

pub struct TmState {
    pub layout: Option<Layout>,
    pub tex: Option<egui::TextureHandle>,
    pub want: (usize, usize),
    pub want_since: Instant,
    pub layout_dirty: bool,
    pub paint_dirty: bool,
}

#[derive(Default)]
pub struct Search {
    pub query: String,
    pub results: Vec<NodeId>,
    pub matches: usize,
    pub ran: Option<String>,
    pub focus: bool,
}

pub struct SaveDialog {
    pub path: String,
    pub error: String,
}

pub struct ExportDialog {
    pub path: String,
    pub with_files: bool,
    pub error: String,
}

pub struct SavedScans {
    pub entries: Vec<(PathBuf, Meta, u64)>,
    pub open_path: String,
    pub error: String,
    pub ask_delete: Option<PathBuf>,
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
    /// what the current tree is: options, times, totals (also what gets saved)
    pub meta: Meta,
    /// the saved file the tree came from, and whether it changed since
    pub loaded_from: Option<PathBuf>,
    pub unsaved: bool,
    pub summary: String,
    pub free: Option<(u64, u64)>,
    pub ext_stats: Vec<[u64; 2]>,
    pub ext_rows: Vec<u16>,
    pub ext_colors: Vec<[u8; 3]>,
    pub top_files: Vec<NodeId>,

    /// focused item (status bar, keyboard navigation, scroll target)
    pub selected: Option<NodeId>,
    /// every selected item, including `selected`
    pub sel: FxHashSet<NodeId>,
    pub anchor: Option<NodeId>,
    /// row where a mouse drag selection started in the tree
    pub drag_anchor: Option<NodeId>,
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
    pub tm_root: u32,
    pub hl_ext: Option<u16>,
    pub hover: Option<NodeId>,
    pub ctx_node: Option<NodeId>,

    pub tab: SideTab,
    pub folder_list: Vec<u32>,
    pub folder_matches: usize,
    pub file_list: Vec<NodeId>,
    pub lists_dirty: Option<Instant>,
    pub search: Search,

    pub picker: Option<Picker>,
    pub confirm: Option<Confirm>,
    pub save_dialog: Option<SaveDialog>,
    pub export_dialog: Option<ExportDialog>,
    pub saved_scans: Option<SavedScans>,
    pub about: bool,
    pub acts: Vec<Action>,
    pub message: String,
    pub title: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Result<Option<Target>, String>) -> App {
        let cfg = Settings::load();
        let themes = theme::all_themes();
        let theme_idx = themes.iter().position(|t| t.name == cfg.theme).unwrap_or(3);
        theme::apply(&cc.egui_ctx, &themes[theme_idx]);
        if (0.3..=4.0).contains(&cfg.zoom) {
            cc.egui_ctx.set_zoom_factor(cfg.zoom);
        }
        let mut app = App {
            cfg,
            cfg_dirty: None,
            themes,
            theme_idx,
            job: None,
            tree: None,
            scan_path: String::new(),
            meta: Meta::default(),
            loaded_from: None,
            unsaved: false,
            summary: String::new(),
            free: None,
            ext_stats: Vec::new(),
            ext_rows: Vec::new(),
            ext_colors: Vec::new(),
            top_files: Vec::new(),
            selected: None,
            sel: FxHashSet::default(),
            anchor: None,
            drag_anchor: None,
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
            tm_root: 0,
            hl_ext: None,
            hover: None,
            ctx_node: None,
            tab: SideTab::Folders,
            folder_list: Vec::new(),
            folder_matches: 0,
            file_list: Vec::new(),
            lists_dirty: None,
            search: Search::default(),
            picker: None,
            confirm: None,
            save_dialog: None,
            export_dialog: None,
            saved_scans: None,
            about: false,
            acts: Vec::new(),
            message: String::new(),
            title: String::new(),
        };
        match start {
            Ok(Some(Target::Scan(p))) => app.start_scan(p),
            Ok(Some(Target::Load(p))) => app.start_load(PathBuf::from(p)),
            Ok(None) => app.open_picker(),
            Err(e) => {
                app.open_picker();
                if let Some(p) = &mut app.picker {
                    p.error = e;
                }
            }
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
    pub fn modal_open(&self) -> bool {
        self.picker.is_some()
            || self.confirm.is_some()
            || self.save_dialog.is_some()
            || self.export_dialog.is_some()
            || self.saved_scans.is_some()
    }

    // ---- scanning -----------------------------------------------------------------------------
    pub fn open_picker(&mut self) {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let mut p = Picker {
            path: home,
            entries: Vec::new(),
            mounts: crate::util::list_mounts(),
            error: String::new(),
            saved: savefile::list_saved(),
        };
        p.refresh();
        self.picker = Some(p);
    }

    fn scan_opts(&self, path: String) -> ScanOpts {
        ScanOpts {
            path,
            one_fs: self.cfg.one_fs,
            apparent: self.cfg.apparent,
            threads: self.cfg.threads,
            compact_below: COMPACT_OPTS[self.cfg.compact_idx.min(COMPACT_OPTS.len() - 1)].0,
            precount: self.cfg.precount,
        }
    }

    fn spawn_job(&mut self, kind: JobKind, path: String, f: impl FnOnce(Arc<Progress>) -> Result<ScanResult, String> + Send + 'static) {
        let prog = Arc::new(Progress::default());
        let p2 = prog.clone();
        let handle = std::thread::Builder::new().name("scan-main".into()).spawn(move || f(p2)).expect("spawn scan thread");
        self.job = Some(ScanJob { prog, handle: Some(handle), started: Instant::now(), kind, path });
        self.message.clear();
    }

    pub fn start_scan(&mut self, path: String) {
        if self.job.is_some() {
            return;
        }
        let opts = self.scan_opts(path.clone());
        let p = path.clone();
        self.spawn_job(JobKind::Scan, path, move |prog| run_scan(opts, prog).ok_or_else(|| format!("Cannot scan {p}")));
    }

    pub fn start_load(&mut self, file: PathBuf) {
        if self.job.is_some() {
            return;
        }
        let f2 = file.clone();
        self.spawn_job(JobKind::Load(file.clone()), file.to_string_lossy().into_owned(), move |prog| {
            savefile::load(&f2, prog).map(|(_, r)| r)
        });
    }

    /// Rescan one folder and splice the result into the current tree (WinDirStat's "Refresh
    /// Selected"). Uses the options the tree was made with, so sizes stay comparable.
    pub fn start_refresh(&mut self, node: NodeId) {
        let Some(tree) = &self.tree else { return };
        if self.job.is_some() || tree.is_deleted(node) {
            return;
        }
        let d = node.dir_id();
        if d == 0 {
            let p = self.scan_path.clone();
            self.start_scan(p);
            return;
        }
        let path = tree.path(NodeId::dir(d));
        let mut opts = ScanOpts {
            path: path.clone(),
            one_fs: self.meta.one_fs,
            apparent: self.meta.apparent,
            threads: self.cfg.threads,
            compact_below: self.meta.compact_below,
            precount: false,
        };
        if self.meta.root.is_empty() {
            opts = self.scan_opts(path.clone());
        }
        let p = path.clone();
        self.spawn_job(JobKind::Refresh(d), path, move |prog| run_scan(opts, prog).ok_or_else(|| format!("Cannot scan {p}")));
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
            let res = job.handle.take().unwrap().join().unwrap_or_else(|_| Err("scan thread crashed".into()));
            let wall = job.started.elapsed();
            match (job.kind, res) {
                (_, Err(e)) => {
                    self.message = e.clone();
                    if self.tree.is_none() {
                        self.open_picker();
                        if let Some(p) = &mut self.picker {
                            p.error = e;
                        }
                    }
                }
                (JobKind::Scan, Ok(r)) => {
                    let cancelled = r.cancelled;
                    self.finish_scan(r, None, wall);
                    if self.cfg.auto_save && !cancelled {
                        let p = savefile::auto_save_path(&self.scan_path);
                        self.save_to(&p);
                    }
                }
                (JobKind::Load(f), Ok(r)) => self.finish_scan(r, Some(f), wall),
                (JobKind::Refresh(d), Ok(r)) => self.finish_refresh(d, r, wall),
            }
        } else if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn push_recent(&mut self, path: &str) {
        self.cfg.recent.retain(|p| p != path);
        self.cfg.recent.insert(0, path.to_string());
        self.cfg.recent.truncate(MAX_RECENT);
        self.touch_cfg();
    }

    fn finish_scan(&mut self, r: ScanResult, loaded: Option<PathBuf>, wall: Duration) {
        let n_files = r.tree.dirs[0].nfiles;
        let n_dirs = r.tree.dirs[0].ndirs;
        let size = r.tree.dirs[0].size;
        self.meta = Meta::from_result(&r);
        self.scan_path = r.opts.path.clone();
        let totals = format!("{} files in {} folders, {}", fmt_int(n_files), fmt_int(n_dirs), fmt_size(size, self.cfg.base, self.cfg.unit));
        self.summary = if let Some(f) = &loaded {
            format!(
                "Saved scan from {} ({}): {} — loaded in {:.2}s from {}",
                fmt_time(r.scanned_at),
                fmt_age(now_unix() - r.scanned_at),
                totals,
                wall.as_secs_f64(),
                f.display()
            )
        } else {
            format!(
                "{}{} — {:.2}s with {} threads{}",
                if r.cancelled { "Scan stopped, partial results: " } else { "Scanned " },
                totals,
                wall.as_secs_f64(),
                r.threads,
                if r.errors > 0 { format!("  ({} items unreadable)", fmt_int(r.errors)) } else { String::new() }
            )
        };
        self.unsaved = loaded.is_none();
        self.loaded_from = loaded;
        self.ext_stats = r.ext_stats;
        self.top_files = r.top_files;
        self.tree = Some(r.tree);
        self.free = crate::util::free_space(&self.scan_path);
        self.selected = Some(NodeId::dir(0));
        self.sel.clear();
        self.sel.insert(NodeId::dir(0));
        self.anchor = Some(NodeId::dir(0));
        self.scope_dir = 0;
        self.tm_root = 0;
        self.expanded.clear();
        self.expanded.insert(NodeId::dir(0));
        self.hidden.clear();
        self.cut.clear();
        self.hl_ext = None;
        self.sort_col = 3;
        self.sort_desc = true;
        self.rows_dirty = true;
        self.search.results.clear();
        self.search.ran = None;
        self.recompute_colors();
        self.tm.layout_dirty = true;
        self.lists_dirty = Some(Instant::now() - Duration::from_secs(1));
        let sp = self.scan_path.clone();
        self.push_recent(&sp);
    }

    fn finish_refresh(&mut self, d: u32, r: ScanResult, wall: Duration) {
        let Some(tree) = &mut self.tree else { return };
        if r.cancelled {
            self.message = "Refresh stopped — kept the previous data for that folder".into();
            return;
        }
        if tree.is_deleted(NodeId::dir(d)) || d == 0 {
            return;
        }
        let was_expanded = self.expanded.contains(&NodeId::dir(d));
        let path = tree.path(NodeId::dir(d));
        let before = tree.dirs[d as usize].size;
        let nd = tree.graft(d, r, &mut self.ext_stats);
        let after = tree.dirs[nd as usize].size;
        let all: Vec<u32> = (0..tree.dirs.len() as u32).collect();
        self.top_files = top_files_in(tree, &all, TOP_FILES_KEPT);
        let tree = self.tree.as_ref().unwrap();
        self.sel.retain(|&n| !tree.is_deleted(n));
        self.expanded.retain(|&n| !tree.is_deleted(n));
        self.hidden.retain(|&n| !tree.is_deleted(n));
        self.cut = compute_cut(tree, &self.hidden);
        if was_expanded {
            self.expanded.insert(NodeId::dir(nd));
        }
        if tree.is_deleted(NodeId::dir(self.tm_root)) {
            self.tm_root = 0;
        }
        if tree.is_deleted(NodeId::dir(self.scope_dir)) {
            self.scope_dir = nd;
        }
        self.search.results.retain(|&n| !tree.is_deleted(n));
        self.unsaved = true;
        self.select(NodeId::dir(nd), Origin::Treemap, SelMode::Replace);
        self.recompute_colors();
        self.rows_dirty = true;
        self.tm.layout_dirty = true;
        self.refresh_lists();
        let delta = after as i128 - before as i128;
        self.message = format!(
            "Refreshed {path} in {:.2}s: {} ({}{})",
            wall.as_secs_f64(),
            self.fmt(after),
            if delta >= 0 { "+" } else { "−" },
            self.fmt(delta.unsigned_abs() as u64)
        );
    }

    // ---- saving -------------------------------------------------------------------------------
    pub fn current_meta(&self) -> Meta {
        let mut m = self.meta.clone();
        if let Some(t) = &self.tree {
            m.size = t.dirs[0].size;
            m.files = t.dirs[0].nfiles;
            m.dirs = t.dirs[0].ndirs;
        }
        m.root = self.scan_path.clone();
        m.version = 1;
        m
    }

    pub fn save_to(&mut self, path: &Path) -> bool {
        let Some(tree) = &self.tree else { return false };
        let meta = self.current_meta();
        let t0 = Instant::now();
        let data = savefile::SaveData { meta: &meta, tree, ext_stats: &self.ext_stats, top_files: &self.top_files };
        match savefile::save(path, &data) {
            Ok(bytes) => {
                self.message = format!("Saved scan to {} ({}, {:.2}s)", path.display(), self.fmt(bytes), t0.elapsed().as_secs_f64());
                self.loaded_from = Some(path.to_path_buf());
                self.unsaved = false;
                true
            }
            Err(e) => {
                self.message = format!("Could not save: {e}");
                false
            }
        }
    }

    pub fn open_save_dialog(&mut self) {
        if self.tree.is_none() || self.scanning() {
            return;
        }
        let path = savefile::default_save_path(&self.scan_path, self.meta.scanned_at);
        self.save_dialog = Some(SaveDialog { path: path.to_string_lossy().into_owned(), error: String::new() });
    }

    pub fn open_saved_scans(&mut self) {
        self.saved_scans = Some(SavedScans {
            entries: savefile::list_saved(),
            open_path: String::new(),
            error: String::new(),
            ask_delete: None,
        });
    }

    pub fn open_export_dialog(&mut self) {
        if self.tree.is_none() {
            return;
        }
        let dir = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
        let name = format!("{}.csv", savefile::slug(&self.scan_path));
        self.export_dialog = Some(ExportDialog { path: dir.join(name).to_string_lossy().into_owned(), with_files: false, error: String::new() });
    }

    pub fn export_csv(&self, path: &str, with_files: bool) -> Result<u64, String> {
        let Some(tree) = &self.tree else { return Err("nothing scanned".into()) };
        export_csv(tree, Path::new(path), with_files).map_err(|e| e.to_string())
    }

    pub fn window_title(&self) -> String {
        let mut t = String::from("WinDirStat for Linux");
        if !self.scan_path.is_empty() && self.tree.is_some() {
            t += " — ";
            t += &self.scan_path;
            if self.loaded_from.is_some() {
                t += &format!("  [saved scan, {}]", fmt_time(self.meta.scanned_at));
            }
            if self.unsaved && self.loaded_from.is_some() {
                t += " *";
            }
        }
        t
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

    // ---- selection ----------------------------------------------------------------------------
    /// The list a range selection walks for `origin`.
    fn order_for(&self, origin: Origin) -> Vec<NodeId> {
        match origin {
            Origin::Tree | Origin::Treemap => self.rows.iter().filter(|r| r.more == 0).map(|r| r.node).collect(),
            Origin::List => match self.tab {
                SideTab::Folders => self.folder_list.iter().map(|&d| NodeId::dir(d)).collect(),
                SideTab::Files => self.file_list.clone(),
                SideTab::Search => self.search.results.clone(),
                SideTab::Extensions => Vec::new(),
            },
        }
    }

    pub fn select(&mut self, node: NodeId, origin: Origin, mode: SelMode) {
        let Some(tree) = &self.tree else { return };
        if tree.is_deleted(node) || node.is_agg() {
            return;
        }
        match mode {
            SelMode::Replace => {
                self.sel.clear();
                self.sel.insert(node);
                self.anchor = Some(node);
            }
            SelMode::Toggle => {
                if self.sel.len() > 1 && self.sel.remove(&node) {
                    if self.selected == Some(node) {
                        self.selected = self.sel.iter().next().copied();
                    }
                    self.anchor = Some(node);
                    return;
                }
                if self.sel.len() == 1 && self.sel.contains(&node) {
                    return;
                }
                self.sel.insert(node);
                self.anchor = Some(node);
            }
            SelMode::Range => {
                let order = if origin == Origin::Treemap { Vec::new() } else { self.order_for(origin) };
                let a = self.anchor.and_then(|a| order.iter().position(|&n| n == a));
                let b = order.iter().position(|&n| n == node);
                match (a, b) {
                    (Some(a), Some(b)) => {
                        self.sel.clear();
                        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                        self.sel.extend(order[lo..=hi].iter().copied());
                    }
                    _ => {
                        self.sel.insert(node);
                        if self.anchor.is_none() {
                            self.anchor = Some(node);
                        }
                    }
                }
            }
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

    /// Selected items for a context-menu or keyboard action on `node`: the whole selection if
    /// `node` is part of it, else just `node`.
    pub fn targets_for(&self, node: NodeId) -> Vec<NodeId> {
        targets_for(self.tree.as_ref(), &self.sel, node)
    }

    pub fn selection_targets(&self) -> Vec<NodeId> {
        match self.selected {
            Some(n) => self.targets_for(n),
            None => Vec::new(),
        }
    }

    /// Select every row currently shown in the tree.
    pub fn select_all_rows(&mut self) {
        self.sel = self.rows.iter().filter(|r| r.more == 0).map(|r| r.node).collect();
    }

    /// Moves the focus in the tree by `delta` rows (keyboard navigation).
    pub fn move_focus(&mut self, delta: isize, extend: bool) {
        let order: Vec<NodeId> = self.rows.iter().filter(|r| r.more == 0).map(|r| r.node).collect();
        if order.is_empty() {
            return;
        }
        let cur = self.selected.and_then(|s| order.iter().position(|&n| n == s)).unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, order.len() as isize - 1) as usize;
        let n = order[next];
        self.select(n, Origin::Tree, if extend { SelMode::Range } else { SelMode::Replace });
        self.scroll_to = self.rows.iter().position(|r| r.node == n && r.more == 0);
    }

    // ---- hiding -------------------------------------------------------------------------------
    pub fn set_hidden(&mut self, nodes: &[NodeId], flag: bool) {
        for &node in nodes {
            if flag {
                if self.tree.as_ref().is_some_and(|t| t.parent(node).is_some()) {
                    self.hidden.insert(node);
                }
            } else {
                self.hidden.remove(&node);
            }
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
        if self.search.ran.is_some() {
            self.run_search();
        }
    }

    pub fn schedule_lists(&mut self) {
        self.lists_dirty = Some(Instant::now());
    }

    pub fn run_search(&mut self) {
        let Some(tree) = &self.tree else { return };
        let q = self.search.query.trim().to_string();
        if q.is_empty() {
            self.search.results.clear();
            self.search.matches = 0;
            self.search.ran = None;
            return;
        }
        let scoped = self.cfg.scope_selected && self.scope_dir != 0;
        let dirs: Vec<u32> = if scoped {
            let mut v = vec![self.scope_dir];
            v.extend(subtree_dirs(tree, self.scope_dir));
            v
        } else {
            (0..tree.dirs.len() as u32).collect()
        };
        let m = Matcher::new(&q, self.cfg.search_case);
        let (res, n) = search(tree, &dirs, &m, self.cfg.search_files, self.cfg.search_dirs, SEARCH_LIMIT);
        self.search.results = res;
        self.search.matches = n;
        self.search.ran = Some(q);
    }

    // ---- actions ------------------------------------------------------------------------------
    pub fn process_actions(&mut self, ctx: &egui::Context) {
        let acts = std::mem::take(&mut self.acts);
        for a in acts {
            match a {
                Action::Select(n, o, m) => self.select(n, o, m),
                Action::Context(n, o) => {
                    if !self.sel.contains(&n) {
                        self.select(n, o, SelMode::Replace);
                    } else {
                        self.selected = Some(n);
                    }
                }
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
                Action::Hide(v) => self.set_hidden(&v, true),
                Action::Show(v) => self.set_hidden(&v, false),
                Action::Trash(v) => self.ask(v, ConfirmKind::Trash),
                Action::Delete(v) => self.ask(v, ConfirmKind::Delete),
                Action::Refresh(n) => self.start_refresh(n),
                Action::Open(p) => crate::util::open_path(&p),
                Action::ShowInFm(p) => crate::util::show_in_file_manager(&p),
                Action::Terminal(p) => {
                    if !crate::util::open_terminal(&p) {
                        self.message = "No terminal found (set $TERMINAL)".into();
                    }
                }
                Action::CopyText(t, n) => {
                    ctx.copy_text(t.clone());
                    self.message = if n == 1 { format!("Copied: {t}") } else { format!("Copied {n} paths") };
                }
                Action::HighlightExt(e) => {
                    self.hl_ext = e;
                    self.tm.paint_dirty = true;
                }
                Action::TmZoom(d) => {
                    if self.tree.as_ref().is_some_and(|t| !t.is_deleted(NodeId::dir(d))) && self.tm_root != d {
                        self.tm_root = d;
                        self.tm.layout_dirty = true;
                    }
                }
                Action::FindExt(e) => {
                    if let Some(t) = &self.tree {
                        self.search.query = format!("*{}", t.ext_names[e as usize]);
                        self.cfg.search_files = true;
                        self.tab = SideTab::Search;
                        self.run_search();
                    }
                }
            }
        }
    }

    pub fn tm_zoom_out(&mut self) {
        if let Some(t) = &self.tree {
            if let Some(p) = t.parent(NodeId::dir(self.tm_root)) {
                self.tm_root = p.dir_id();
                self.tm.layout_dirty = true;
            }
        }
    }

    fn ask(&mut self, nodes: Vec<NodeId>, kind: ConfirmKind) {
        let Some(tree) = &self.tree else { return };
        let nodes: Vec<NodeId> = top_level(tree, &nodes).into_iter().filter(|&n| tree.parent(n).is_some() && !tree.is_small_agg_file(n)).collect();
        if nodes.is_empty() {
            return;
        }
        let total: u64 = nodes.iter().map(|&n| tree.size(n)).sum();
        let files: u64 = nodes.iter().map(|&n| if n.is_dir() { tree.dirs[n.0 as usize].nfiles } else { 1 }).sum();
        let mut text = String::new();
        for &n in nodes.iter().take(12) {
            text += &tree.path(n);
            text.push('\n');
        }
        if nodes.len() > 12 {
            text += &format!("… and {} more\n", fmt_int((nodes.len() - 12) as u64));
        }
        text += &format!("\n{} item{}, {}, {} file{}", fmt_int(nodes.len() as u64), if nodes.len() == 1 { "" } else { "s" }, self.fmt(total), fmt_int(files), if files == 1 { "" } else { "s" });
        if kind == ConfirmKind::Delete {
            text += "\n\nThis cannot be undone.";
        }
        self.confirm = Some(Confirm { nodes, kind, text });
    }

    pub fn run_confirmed(&mut self) {
        let Some(c) = self.confirm.take() else { return };
        let Some(tree) = &self.tree else { return };
        let mut done = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for &node in &c.nodes {
            let path = tree.path(node);
            let res: Result<(), String> = match c.kind {
                ConfirmKind::Trash => std::process::Command::new("gio")
                    .args(["trash", "--", &path])
                    .output()
                    .map_err(|e| format!("gio: {e}"))
                    .and_then(|o| if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }),
                ConfirmKind::Delete => {
                    if node.is_dir() {
                        std::fs::remove_dir_all(&path).map_err(|e| format!("{path}: {e}"))
                    } else {
                        std::fs::remove_file(&path).map_err(|e| format!("{path}: {e}"))
                    }
                }
            };
            match res {
                Ok(()) => done.push(node),
                Err(e) => errors.push(e),
            }
        }
        if !done.is_empty() {
            self.remove_nodes(&done);
        }
        self.message = match (done.len(), errors.len()) {
            (_, 0) => format!("Removed {} item{}", done.len(), if done.len() == 1 { "" } else { "s" }),
            (0, _) => format!("Could not remove: {}", errors.join("; ")),
            (d, e) => format!("Removed {d}, failed {e}: {}", errors.join("; ")),
        };
    }

    fn remove_nodes(&mut self, nodes: &[NodeId]) {
        let Some(tree) = &mut self.tree else { return };
        let mut new_sel = None;
        for &node in nodes {
            if tree.is_deleted(node) {
                continue;
            }
            new_sel = tree.parent(node);
            let (_, _, dec) = tree.remove(node);
            for (e, sz) in dec {
                if let Some(s) = self.ext_stats.get_mut(e as usize) {
                    s[0] = s[0].saturating_sub(sz);
                    s[1] = s[1].saturating_sub(1);
                }
            }
        }
        let tree = self.tree.as_ref().unwrap();
        self.hidden.retain(|&h| !tree.is_deleted(h));
        self.sel.retain(|&h| !tree.is_deleted(h));
        self.search.results.retain(|&h| !tree.is_deleted(h));
        self.cut = compute_cut(tree, &self.hidden);
        if let Some(p) = new_sel {
            let mut p = p;
            while tree.is_deleted(p) {
                p = tree.parent(p).unwrap_or(NodeId::dir(0));
            }
            if self.selected.is_none_or(|s| tree.is_deleted(s)) {
                self.selected = Some(p);
                self.sel.insert(p);
            }
            if tree.is_deleted(NodeId::dir(self.scope_dir)) {
                self.scope_dir = p.dir_id();
            }
        }
        if tree.is_deleted(NodeId::dir(self.tm_root)) {
            self.tm_root = 0;
        }
        self.unsaved = true;
        self.rows_dirty = true;
        self.recompute_colors();
        self.tm.layout_dirty = true;
        self.refresh_lists();
    }
}

pub fn fmt_age(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=89 => "just now".into(),
        90..=5399 => format!("{} min ago", s / 60),
        5400..=129_599 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
}

/// Drops nodes that have an ancestor in the same list (acting on the ancestor covers them).
pub fn top_level(tree: &Tree, nodes: &[NodeId]) -> Vec<NodeId> {
    let set: FxHashSet<NodeId> = nodes.iter().copied().collect();
    let mut out: Vec<NodeId> = nodes
        .iter()
        .copied()
        .filter(|&n| {
            let mut a = tree.parent(n);
            while let Some(p) = a {
                if set.contains(&p) {
                    return false;
                }
                a = tree.parent(p);
            }
            true
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

pub fn targets_for(tree: Option<&Tree>, sel: &FxHashSet<NodeId>, node: NodeId) -> Vec<NodeId> {
    let Some(tree) = tree else { return Vec::new() };
    if sel.contains(&node) && sel.len() > 1 {
        let mut v: Vec<NodeId> = sel.iter().copied().filter(|&n| !tree.is_deleted(n) && !n.is_agg()).collect();
        v.sort_unstable_by_key(|&n| Reverse(tree.size(n)));
        v
    } else {
        vec![node]
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

// ---------------------------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------------------------
/// Name matcher: `*` and `?` wildcards match the whole name; otherwise a substring match.
pub struct Matcher {
    pat: Vec<u8>,
    glob: bool,
    case: bool,
}

impl Matcher {
    pub fn new(q: &str, case: bool) -> Matcher {
        let pat: Vec<u8> = if case { q.as_bytes().to_vec() } else { q.as_bytes().iter().map(u8::to_ascii_lowercase).collect() };
        Matcher { glob: pat.contains(&b'*') || pat.contains(&b'?'), pat, case }
    }
    pub fn is_match(&self, name: &[u8], scratch: &mut Vec<u8>) -> bool {
        let n: &[u8] = if self.case {
            name
        } else {
            scratch.clear();
            scratch.extend(name.iter().map(u8::to_ascii_lowercase));
            scratch
        };
        if self.glob {
            glob_match(&self.pat, n)
        } else {
            n.windows(self.pat.len().max(1)).any(|w| w == self.pat.as_slice())
        }
    }
}

pub fn glob_match(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = pi;
            mark = si;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Largest matching items among `dirs` (and their files). Returns (top `k`, total matches).
pub fn search(tree: &Tree, dirs: &[u32], m: &Matcher, files: bool, folders: bool, k: usize) -> (Vec<NodeId>, usize) {
    let mut heap: BinaryHeap<Reverse<(u64, NodeId)>> = BinaryHeap::new();
    let mut count = 0usize;
    let mut scratch = Vec::new();
    let push = |heap: &mut BinaryHeap<Reverse<(u64, NodeId)>>, size: u64, n: NodeId| {
        if heap.len() < k {
            heap.push(Reverse((size, n)));
        } else if size > heap.peek().map(|r| r.0 .0).unwrap_or(0) {
            heap.pop();
            heap.push(Reverse((size, n)));
        }
    };
    for &d in dirs {
        let dir = &tree.dirs[d as usize];
        if !dir.alive {
            continue;
        }
        if folders && d != 0 && m.is_match(dir.name(), &mut scratch) {
            count += 1;
            push(&mut heap, dir.size, NodeId::dir(d));
        }
        if files {
            for (i, f) in dir.files.iter().enumerate() {
                if f.ext == EXT_DEL || f.ext == EXT_AGG {
                    continue;
                }
                if m.is_match(dir.file_name(f), &mut scratch) {
                    count += 1;
                    push(&mut heap, f.size, NodeId::file(d, i as u32));
                }
            }
        }
    }
    let mut v: Vec<(u64, NodeId)> = heap.into_iter().map(|r| r.0).collect();
    v.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    (v.into_iter().map(|(_, n)| n).collect(), count)
}

// ---------------------------------------------------------------------------------------------
// CSV export
// ---------------------------------------------------------------------------------------------
fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Writes every folder (and optionally every file) with sizes. Returns the number of rows.
pub fn export_csv(tree: &Tree, path: &Path, with_files: bool) -> std::io::Result<u64> {
    let mut w = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(path)?);
    writeln!(w, "Path,Type,Size,Files,Folders,Last Change")?;
    let mut rows = 0u64;
    let mut stack: Vec<(u32, String)> = vec![(0, String::from_utf8_lossy(tree.dirs[0].name()).into_owned())];
    while let Some((d, p)) = stack.pop() {
        let dir = &tree.dirs[d as usize];
        if !dir.alive {
            continue;
        }
        writeln!(w, "{},Folder,{},{},{},{}", csv_field(&p), dir.size, dir.nfiles, dir.ndirs, fmt_time(dir.mtime))?;
        rows += 1;
        let join = |name: &[u8]| {
            let mut s = p.clone();
            if !s.ends_with('/') {
                s.push('/');
            }
            s.push_str(&String::from_utf8_lossy(name));
            s
        };
        if with_files {
            for f in &dir.files {
                if f.ext == EXT_DEL {
                    continue;
                }
                let kind = if f.ext == EXT_AGG { "Small files" } else { "File" };
                writeln!(w, "{},{kind},{},,,{}", csv_field(&join(dir.file_name(f))), f.size, fmt_time(f.mtime))?;
                rows += 1;
            }
        }
        for &c in dir.subdirs.iter().rev() {
            stack.push((c, join(tree.dirs[c as usize].name())));
        }
    }
    w.flush()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn globbing() {
        assert!(glob_match(b"*.mp4", b"movie.mp4"));
        assert!(!glob_match(b"*.mp4", b"movie.mp4.part"));
        assert!(glob_match(b"a?c*", b"abcdef"));
        assert!(glob_match(b"*", b""));
        let m = Matcher::new("Report", false);
        let mut s = Vec::new();
        assert!(m.is_match(b"q3-report.pdf", &mut s));
        assert!(!Matcher::new("Report", true).is_match(b"q3-report.pdf", &mut s));
    }
}
