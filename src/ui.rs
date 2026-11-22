//! All egui drawing code.
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use egui::{pos2, vec2, Align, Align2, Color32, Key, Layout, Modifiers, Rect, RichText, Sense, Stroke, StrokeKind, Ui};
use egui_extras::{Column, TableBuilder};
use rustc_hash::FxHashSet;

use crate::app::*;
use crate::savefile;
use crate::scan::*;
use crate::theme;
use crate::treemap;
use crate::units::{fmt_int, fmt_size, fmt_time, Base, Unit};

const ROW_H: f32 = 18.0;
const MAX_OUTLINES: usize = 400;

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn display_color(c: [u8; 3], look: &treemap::Look) -> Color32 {
    let a = treemap::adjust(c, look, 1.0);
    Color32::from_rgb(a[0].clamp(0.0, 255.0) as u8, a[1].clamp(0.0, 255.0) as u8, a[2].clamp(0.0, 255.0) as u8)
}

fn parent_path(p: &str) -> String {
    std::path::Path::new(p).parent().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Right-click menu for one item or a multi-selection.
pub fn node_menu(ui: &mut Ui, tree: &Tree, hidden: &FxHashSet<NodeId>, targets: &[NodeId], busy: bool, acts: &mut Vec<Action>) {
    if targets.is_empty() {
        ui.close();
        return;
    }
    if targets.len() == 1 {
        single_menu(ui, tree, hidden, targets[0], busy, acts);
    } else {
        multi_menu(ui, tree, hidden, targets, acts);
    }
}

fn single_menu(ui: &mut Ui, tree: &Tree, hidden: &FxHashSet<NodeId>, node: NodeId, busy: bool, acts: &mut Vec<Action>) {
    let path = tree.path(node);
    let agg = tree.is_small_agg_file(node);
    let folder = if node.is_dir() { path.clone() } else { parent_path(&path) };
    if node.is_dir() {
        if ui.button("Open Folder").clicked() {
            acts.push(Action::Open(path.clone()));
            ui.close();
        }
    } else {
        if ui.add_enabled(!agg, egui::Button::new("Open File")).clicked() {
            acts.push(Action::Open(path.clone()));
            ui.close();
        }
        if ui.button("Open Containing Folder").clicked() {
            acts.push(Action::Open(folder.clone()));
            ui.close();
        }
    }
    if ui.add_enabled(!agg, egui::Button::new("Show in File Manager")).clicked() {
        acts.push(Action::ShowInFm(path.clone()));
        ui.close();
    }
    if ui.button("Open Terminal Here").clicked() {
        acts.push(Action::Terminal(folder.clone()));
        ui.close();
    }
    ui.separator();
    if ui.button("Copy Path").clicked() {
        acts.push(Action::CopyText(path.clone(), 1));
        ui.close();
    }
    if ui.button("Copy Name").clicked() {
        acts.push(Action::CopyText(tree.name(node), 1));
        ui.close();
    }
    ui.separator();
    let refresh_label = if node.is_dir() { "Refresh This Folder" } else { "Refresh Containing Folder" };
    if ui.add_enabled(!busy, egui::Button::new(refresh_label).shortcut_text("Shift+F5")).on_hover_text("Rescan only this folder and update the results").clicked() {
        acts.push(Action::Refresh(NodeId::dir(node.dir_id())));
        ui.close();
    }
    if node.is_dir() && ui.button("Zoom Treemap Here").clicked() {
        acts.push(Action::TmZoom(node.dir_id()));
        ui.close();
    }
    let can_hide = tree.parent(node).is_some();
    match hidden_owner(tree, hidden, node) {
        _ if !can_hide => {
            ui.add_enabled(false, egui::Button::new("Hide from Treemap"));
        }
        Some(o) if o == node => {
            if ui.button("Show on Treemap").clicked() {
                acts.push(Action::Show(vec![node]));
                ui.close();
            }
        }
        Some(o) => {
            if ui.button(format!("Show on Treemap (hidden via “{}”)", tree.name(o))).clicked() {
                acts.push(Action::Show(vec![o]));
                ui.close();
            }
        }
        None => {
            if ui.button("Hide from Treemap").clicked() {
                acts.push(Action::Hide(vec![node]));
                ui.close();
            }
        }
    }
    ui.separator();
    let can_del = can_hide && !agg;
    if ui.add_enabled(can_del, egui::Button::new("Move to Trash…").shortcut_text("Del")).clicked() {
        acts.push(Action::Trash(vec![node]));
        ui.close();
    }
    if ui.add_enabled(can_del, egui::Button::new("Delete Permanently…").shortcut_text("Shift+Del")).clicked() {
        acts.push(Action::Delete(vec![node]));
        ui.close();
    }
}

fn multi_menu(ui: &mut Ui, tree: &Tree, hidden: &FxHashSet<NodeId>, targets: &[NodeId], acts: &mut Vec<Action>) {
    let top = top_level(tree, targets);
    let total: u64 = top.iter().map(|&n| tree.size(n)).sum();
    ui.label(RichText::new(format!("{} items selected  —  {}", fmt_int(targets.len() as u64), fmt_size(total, Base::Binary, Unit::Auto))).weak());
    ui.separator();
    let real: Vec<NodeId> = targets.iter().copied().filter(|&n| !tree.is_small_agg_file(n)).collect();
    if ui.add_enabled(real.len() <= 20, egui::Button::new(format!("Open All ({})", real.len()))).on_disabled_hover_text("Select at most 20 items to open them all").clicked() {
        for &n in &real {
            acts.push(Action::Open(tree.path(n)));
        }
        ui.close();
    }
    if ui.button("Copy Paths").clicked() {
        let s: Vec<String> = real.iter().map(|&n| tree.path(n)).collect();
        acts.push(Action::CopyText(s.join("\n"), s.len()));
        ui.close();
    }
    if ui.button("Copy Names").clicked() {
        let s: Vec<String> = real.iter().map(|&n| tree.name(n)).collect();
        acts.push(Action::CopyText(s.join("\n"), s.len()));
        ui.close();
    }
    ui.separator();
    let hideable: Vec<NodeId> = top.iter().copied().filter(|&n| tree.parent(n).is_some() && hidden_owner(tree, hidden, n).is_none()).collect();
    let shown: Vec<NodeId> = targets.iter().copied().filter(|n| hidden.contains(n)).collect();
    if ui.add_enabled(!hideable.is_empty(), egui::Button::new(format!("Hide {} from Treemap", hideable.len()))).clicked() {
        acts.push(Action::Hide(hideable));
        ui.close();
    }
    if !shown.is_empty() && ui.button(format!("Show {} on Treemap", shown.len())).clicked() {
        acts.push(Action::Show(shown));
        ui.close();
    }
    ui.separator();
    let deletable: Vec<NodeId> = top.iter().copied().filter(|&n| tree.parent(n).is_some() && !tree.is_small_agg_file(n)).collect();
    let n = deletable.len();
    if ui.add_enabled(n > 0, egui::Button::new(format!("Move {n} Items to Trash…")).shortcut_text("Del")).clicked() {
        acts.push(Action::Trash(deletable.clone()));
        ui.close();
    }
    if ui.add_enabled(n > 0, egui::Button::new(format!("Delete {n} Items Permanently…")).shortcut_text("Shift+Del")).clicked() {
        acts.push(Action::Delete(deletable));
        ui.close();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scan(ctx);
        // The very first frame can report a placeholder screen size (10000 px): laying out the
        // panels then would give them absurd initial sizes, so wait for the real window size.
        let c = ctx.content_rect();
        if ctx.cumulative_frame_nr() < 5 && (c.width() * ctx.zoom_factor() >= 9000.0 || c.height() * ctx.zoom_factor() >= 9000.0) {
            ctx.request_repaint();
            return;
        }
        self.handle_keys(ctx);
        if self.rows_dirty {
            self.rebuild_rows();
        }
        if let Some(t) = self.lists_dirty {
            if t.elapsed() >= Duration::from_millis(80) {
                self.refresh_lists();
            } else {
                ctx.request_repaint_after(Duration::from_millis(80));
            }
        }

        egui::TopBottomPanel::top("menu").show(ctx, |ui| self.ui_menu(ui, ctx));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.ui_status(ui));
        egui::TopBottomPanel::bottom("tm_controls").show(ctx, |ui| self.ui_tm_controls(ui));
        let content = ctx.content_rect();
        let max_tm = (content.height() - 220.0).max(130.0);
        let h = self.cfg.tm_h.unwrap_or(content.height() * 0.42).clamp(120.0, max_tm);
        let tm = egui::TopBottomPanel::bottom("treemap")
            .resizable(true)
            .default_height(h)
            .min_height(120.0)
            .max_height(max_tm)
            .show(ctx, |ui| self.ui_treemap(ui, ctx));
        let side = egui::SidePanel::right("side")
            .resizable(true)
            .default_width(self.cfg.side_w.clamp(320.0, (content.width() - 360.0).max(330.0)))
            .min_width(320.0)
            .max_width((content.width() - 360.0).max(330.0))
            .show(ctx, |ui| self.ui_side(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.ui_tree(ui));
        self.ui_dialogs(ctx);
        self.process_actions(ctx);
        self.track_layout(ctx, tm.response.rect.height(), side.response.rect.width());

        let title = self.window_title();
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if let Some(t) = self.cfg_dirty {
            if t.elapsed() > Duration::from_millis(600) {
                self.cfg.save();
                self.cfg_dirty = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(650));
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.cfg.save();
    }
}

impl App {
    fn set_theme(&mut self, ctx: &egui::Context, i: usize) {
        self.theme_idx = i;
        self.cfg.theme = self.themes[i].name.to_string();
        theme::apply(ctx, &self.themes[i]);
        self.recompute_colors();
        self.touch_cfg();
    }

    /// Remembers window size/position/maximized, UI zoom and panel sizes.
    fn track_layout(&mut self, ctx: &egui::Context, tm_h: f32, side_w: f32) {
        let (inner, outer, maxed, fullscreen, native, dragging) = ctx.input(|i| {
            let v = i.viewport();
            (v.inner_rect, v.outer_rect, v.maximized, v.fullscreen, v.native_pixels_per_point, i.pointer.primary_down())
        });
        let mut ch = false;
        let zoom = ctx.zoom_factor();
        if (zoom - self.cfg.zoom).abs() > 0.001 {
            self.cfg.zoom = zoom;
            ch = true;
        }
        // egui reports the window in zoomed points; settings keep logical (unzoomed) pixels
        let scale = native.map(|n| ctx.pixels_per_point() / n).unwrap_or(zoom);
        if let Some(m) = maxed {
            if m != self.cfg.win_max {
                self.cfg.win_max = m;
                ch = true;
            }
        }
        if maxed != Some(true) && fullscreen != Some(true) {
            if let Some(r) = inner {
                let (w, h) = (r.width() * scale, r.height() * scale);
                if w > 100.0 && h > 100.0 && ((w - self.cfg.win_w).abs() > 1.0 || (h - self.cfg.win_h).abs() > 1.0) {
                    self.cfg.win_w = w.round();
                    self.cfg.win_h = h.round();
                    ch = true;
                }
            }
            if let Some(r) = outer {
                let p = [(r.min.x * scale).round(), (r.min.y * scale).round()];
                if self.cfg.win_pos != Some(p) {
                    self.cfg.win_pos = Some(p);
                    ch = true;
                }
            }
        }
        if dragging {
            if (side_w - self.cfg.side_w).abs() > 0.5 {
                self.cfg.side_w = side_w;
                ch = true;
            }
            if self.cfg.tm_h.is_none_or(|h| (h - tm_h).abs() > 0.5) && self.tm.layout.is_some() {
                // only once the user has actually dragged the divider
                if self.cfg.tm_h.is_some() || (tm_h - ctx.content_rect().height() * 0.42).abs() > 2.0 {
                    self.cfg.tm_h = Some(tm_h);
                    ch = true;
                }
            }
        }
        if ch {
            self.touch_cfg();
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let modal = self.modal_open();
        let k = |key: Key, m: Modifiers| ctx.input_mut(|i| i.consume_key(m, key));
        let none = Modifiers::NONE;
        let cmd = Modifiers::COMMAND;
        let shift = Modifiers::SHIFT;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.confirm.is_some() {
                self.confirm = None;
            } else if self.save_dialog.is_some() {
                self.save_dialog = None;
            } else if self.export_dialog.is_some() {
                self.export_dialog = None;
            } else if self.saved_scans.is_some() {
                self.saved_scans = None;
            } else if self.picker.is_some() {
                self.picker = None;
            } else if self.scanning() {
                self.stop_scan();
            } else if self.tm_root != 0 {
                self.acts.push(Action::TmZoom(0));
            }
        }
        if k(Key::Q, cmd) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if k(Key::O, cmd_shift) && !self.scanning() {
            self.open_saved_scans();
        }
        if k(Key::O, cmd) && !self.scanning() {
            self.open_picker();
        }
        if k(Key::S, cmd) {
            self.open_save_dialog();
        }
        if !self.scanning() && k(Key::F5, shift) {
            if let Some(s) = self.selected {
                self.start_refresh(s);
            }
        }
        if !self.scanning() && k(Key::F5, none) && !self.scan_path.is_empty() {
            self.start_scan(self.scan_path.clone());
        }
        if k(Key::F, cmd) {
            self.tab = SideTab::Search;
            self.search.focus = true;
        }
        if typing || modal || self.tree.is_none() {
            return;
        }
        let page = 20isize;
        for (key, d) in [(Key::ArrowDown, 1isize), (Key::ArrowUp, -1), (Key::PageDown, page), (Key::PageUp, -page), (Key::End, isize::MAX / 2), (Key::Home, -isize::MAX / 2)] {
            if k(key, none) {
                self.move_focus(d, false);
            }
            if k(key, shift) {
                self.move_focus(d, true);
            }
        }
        if let Some(s) = self.selected {
            if k(Key::ArrowRight, none) && s.is_dir() {
                if !self.expanded.contains(&s) {
                    self.acts.push(Action::Toggle(s));
                } else {
                    self.move_focus(1, false);
                }
            }
            if k(Key::ArrowLeft, none) {
                if s.is_dir() && self.expanded.contains(&s) {
                    self.acts.push(Action::Toggle(s));
                } else if let Some(p) = self.tree.as_ref().and_then(|t| t.parent(s)) {
                    self.select(p, Origin::Tree, SelMode::Replace);
                    self.scroll_to = self.rows.iter().position(|r| r.node == p && r.more == 0);
                }
            }
            if k(Key::Enter, none) && s.is_dir() {
                self.acts.push(Action::Toggle(s));
            }
            if k(Key::Delete, shift) {
                self.acts.push(Action::Delete(self.selection_targets()));
            }
            if k(Key::Delete, none) {
                self.acts.push(Action::Trash(self.selection_targets()));
            }
            if k(Key::C, cmd) {
                let tree = self.tree.as_ref().unwrap();
                let v: Vec<String> = self.selection_targets().iter().filter(|&&n| !tree.is_small_agg_file(n)).map(|&n| tree.path(n)).collect();
                if !v.is_empty() {
                    let n = v.len();
                    self.acts.push(Action::CopyText(v.join("\n"), n));
                }
            }
        }
        if k(Key::A, cmd) {
            self.select_all_rows();
        }
        if k(Key::Backspace, none) {
            self.tm_zoom_out();
        }
    }

    // ---------------------------------------------------------------------------------------
    // menu
    // ---------------------------------------------------------------------------------------
    fn ui_menu(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                let idle = !self.scanning();
                let has = self.tree.is_some();
                if ui.add_enabled(idle, egui::Button::new("Scan…").shortcut_text("Ctrl+O")).clicked() {
                    self.open_picker();
                    ui.close();
                }
                if ui.add_enabled(idle && !self.scan_path.is_empty(), egui::Button::new("Rescan All").shortcut_text("F5")).clicked() {
                    self.start_scan(self.scan_path.clone());
                    ui.close();
                }
                if ui
                    .add_enabled(idle && has && self.selected.is_some(), egui::Button::new("Refresh Selected Folder").shortcut_text("Shift+F5"))
                    .on_hover_text("Rescan only the selected folder and update the results")
                    .clicked()
                {
                    if let Some(s) = self.selected {
                        self.start_refresh(s);
                    }
                    ui.close();
                }
                if ui.add_enabled(!idle, egui::Button::new("Stop").shortcut_text("Esc")).clicked() {
                    self.stop_scan();
                    ui.close();
                }
                ui.add_enabled_ui(idle && !self.cfg.recent.is_empty(), |ui| {
                    ui.menu_button("Recent Folders", |ui| {
                        let mut go = None;
                        for p in &self.cfg.recent {
                            if ui.button(p).clicked() {
                                go = Some(p.clone());
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button("Clear List").clicked() {
                            self.cfg.recent.clear();
                            self.touch_cfg();
                            ui.close();
                        }
                        if let Some(p) = go {
                            self.start_scan(p);
                        }
                    });
                });
                ui.separator();
                if ui.add_enabled(idle && has, egui::Button::new("Save Scan…").shortcut_text("Ctrl+S")).clicked() {
                    self.open_save_dialog();
                    ui.close();
                }
                if ui.add_enabled(idle, egui::Button::new("Open Saved Scan…").shortcut_text("Ctrl+Shift+O")).clicked() {
                    self.open_saved_scans();
                    ui.close();
                }
                if ui.add_enabled(has, egui::Button::new("Export to CSV…")).clicked() {
                    self.open_export_dialog();
                    ui.close();
                }
                ui.separator();
                if ui.add(egui::Button::new("Quit").shortcut_text("Ctrl+Q")).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                let has = self.tree.is_some() && self.selected.is_some();
                if ui.add_enabled(has, egui::Button::new("Copy Path(s)").shortcut_text("Ctrl+C")).clicked() {
                    if let Some(tree) = &self.tree {
                        let v: Vec<String> = self.selection_targets().iter().map(|&n| tree.path(n)).collect();
                        let n = v.len();
                        self.acts.push(Action::CopyText(v.join("\n"), n));
                    }
                    ui.close();
                }
                if ui.add_enabled(self.tree.is_some(), egui::Button::new("Select All Visible Rows").shortcut_text("Ctrl+A")).clicked() {
                    self.select_all_rows();
                    ui.close();
                }
                if ui.add_enabled(self.tree.is_some(), egui::Button::new("Find…").shortcut_text("Ctrl+F")).clicked() {
                    self.tab = SideTab::Search;
                    self.search.focus = true;
                    ui.close();
                }
                ui.separator();
                if ui.add_enabled(has, egui::Button::new("Move to Trash…").shortcut_text("Del")).clicked() {
                    self.acts.push(Action::Trash(self.selection_targets()));
                    ui.close();
                }
                if ui.add_enabled(has, egui::Button::new("Delete Permanently…").shortcut_text("Shift+Del")).clicked() {
                    self.acts.push(Action::Delete(self.selection_targets()));
                    ui.close();
                }
            });
            ui.menu_button("Options", |ui| {
                let mut ch = false;
                ch |= ui.checkbox(&mut self.cfg.one_fs, "Stay on one filesystem").changed();
                ch |= ui.checkbox(&mut self.cfg.apparent, "Use apparent size (instead of disk usage)").changed();
                ch |= ui.checkbox(&mut self.cfg.precount, "Accurate progress (count items in the background)").changed();
                ch |= ui
                    .checkbox(&mut self.cfg.auto_save, "Auto-save every completed scan")
                    .on_hover_text(format!("Keeps the latest scan of each folder in\n{}\nso you can reopen it later without rescanning.", savefile::scans_dir().display()))
                    .changed();
                ui.separator();
                ui.menu_button("Size units", |ui| {
                    ui.label(RichText::new("Base").weak());
                    ch |= ui.radio_value(&mut self.cfg.base, Base::Binary, "Base 2 — 1024  (KiB, MiB, GiB …)").changed();
                    ch |= ui.radio_value(&mut self.cfg.base, Base::Decimal, "Base 10 — 1000  (KB, MB, GB …)").changed();
                    ui.separator();
                    ui.label(RichText::new("Show sizes as").weak());
                    for u in Unit::ALL {
                        ch |= ui.radio_value(&mut self.cfg.unit, u, u.label(self.cfg.base)).changed();
                    }
                });
                ui.menu_button("Scan threads", |ui| {
                    for t in THREAD_OPTS {
                        let label = if t == 0 { "Auto (SSD/HDD aware)".to_string() } else { format!("{t}") };
                        ch |= ui.radio_value(&mut self.cfg.threads, t, label).changed();
                    }
                });
                ui.menu_button("Compact small files", |ui| {
                    for (i, (_, label)) in COMPACT_OPTS.iter().enumerate() {
                        ch |= ui.radio_value(&mut self.cfg.compact_idx, i, *label).changed();
                    }
                    ui.label(RichText::new("Merges tiny files into one entry per folder:\nless memory, faster UI on huge trees.").weak().small());
                });
                if ch {
                    self.touch_cfg();
                }
            });
            ui.menu_button("View", |ui| {
                ui.menu_button("Theme", |ui| {
                    for i in 0..self.themes.len() {
                        let name = self.themes[i].name;
                        if ui.radio(self.theme_idx == i, name).clicked() {
                            self.set_theme(ctx, i);
                        }
                    }
                });
                ui.menu_button(format!("Interface Zoom ({:.0}%)", ctx.zoom_factor() * 100.0), |ui| {
                    if ui.add(egui::Button::new("Zoom In").shortcut_text("Ctrl+=")).clicked() {
                        egui::gui_zoom::zoom_in(ctx);
                    }
                    if ui.add(egui::Button::new("Zoom Out").shortcut_text("Ctrl+-")).clicked() {
                        egui::gui_zoom::zoom_out(ctx);
                    }
                    if ui.add(egui::Button::new("Reset (100%)").shortcut_text("Ctrl+0")).clicked() {
                        ctx.set_zoom_factor(1.0);
                    }
                    ui.separator();
                    for z in [0.8f32, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0] {
                        if ui.radio((ctx.zoom_factor() - z).abs() < 0.01, format!("{:.0}%", z * 100.0)).clicked() {
                            ctx.set_zoom_factor(z);
                        }
                    }
                    ui.label(RichText::new("Remembered between sessions, along with\nthe window size and panel sizes.").weak().small());
                });
                if ui.button("Reset Panel Sizes").clicked() {
                    self.cfg.tm_h = None;
                    self.cfg.side_w = Settings::default().side_w;
                    ctx.memory_mut(|m| m.reset_areas());
                    self.touch_cfg();
                    ui.close();
                }
                ui.separator();
                let has = self.tree.is_some();
                let can_zoom = self.selected.is_some_and(|s| s.dir_id() != self.tm_root);
                if ui.add_enabled(has && can_zoom, egui::Button::new("Zoom Treemap to Selected Folder").shortcut_text("double-click")).clicked() {
                    if let Some(s) = self.selected {
                        self.acts.push(Action::TmZoom(s.dir_id()));
                    }
                    ui.close();
                }
                if ui.add_enabled(self.tm_root != 0, egui::Button::new("Zoom Treemap Out").shortcut_text("Backspace")).clicked() {
                    self.tm_zoom_out();
                    ui.close();
                }
                if ui.add_enabled(self.tm_root != 0, egui::Button::new("Reset Treemap Zoom").shortcut_text("Esc")).clicked() {
                    self.acts.push(Action::TmZoom(0));
                    ui.close();
                }
                if ui.add_enabled(!self.hidden.is_empty(), egui::Button::new("Show All Hidden in Treemap")).clicked() {
                    self.clear_hidden();
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard Shortcuts and About").clicked() {
                    self.about = true;
                    ui.close();
                }
            });
        });
    }

    // ---------------------------------------------------------------------------------------
    // status bar
    // ---------------------------------------------------------------------------------------
    fn ui_status(&mut self, ui: &mut Ui) {
        let accent = self.theme().accent;
        let (base, unit) = (self.cfg.base, self.cfg.unit);
        let mut stop = false;
        ui.horizontal(|ui| {
            if let Some(job) = &self.job {
                let p = &job.prog;
                let entries = p.entries.load(Relaxed);
                let total = p.total.load(Relaxed);
                let ready = p.total_ready.load(Relaxed);
                let el = job.started.elapsed().as_secs_f64();
                let known = total > 0 && (ready || p.phase.load(Relaxed) == 1 && total >= entries);
                let loading = matches!(job.kind, JobKind::Load(_));
                let what = if loading { "folders" } else { "items" };
                let (frac, text) = if p.phase.load(Relaxed) == 2 {
                    (1.0, if loading { "Preparing…".to_string() } else { "Building tree…".to_string() })
                } else if known && ready {
                    let f = (entries as f32 / total.max(1) as f32).min(0.99);
                    let eta = if f > 0.03 && el > 2.0 { format!("  ·  ETA {}", fmt_dur(el * (1.0 - f as f64) / f as f64)) } else { String::new() };
                    let extra = if loading { format!("{} files", fmt_int(p.files.load(Relaxed))) } else { fmt_size(p.bytes.load(Relaxed), base, unit) };
                    (f, format!("{:.0}%  ·  {} {what}  ·  {}{}", f * 100.0, fmt_int(entries), extra, eta))
                } else {
                    (0.0, format!("{} items  ·  {}  ·  {} threads", fmt_int(entries), fmt_size(p.bytes.load(Relaxed), base, unit), p.threads.load(Relaxed)))
                };
                let mut bar = egui::ProgressBar::new(frac).desired_width(440.0).text(text).fill(accent);
                if !(known && ready) && p.phase.load(Relaxed) != 2 {
                    bar = bar.animate(true);
                }
                let verb = match job.kind {
                    JobKind::Scan => "Scanning",
                    JobKind::Load(_) => "Loading",
                    JobKind::Refresh(_) => "Refreshing",
                };
                ui.label(verb);
                ui.add(bar);
                if ui.button("Stop").clicked() {
                    stop = true;
                }
                let cur = if loading { job.path.clone() } else { p.current.lock().map(|g| g.clone()).unwrap_or_default() };
                ui.add(egui::Label::new(RichText::new(cur).weak()).truncate());
            } else {
                let text = self.status_text();
                ui.add(egui::Label::new(text).truncate());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some((free, total)) = self.free {
                    ui.label(format!("Free: {} of {}", fmt_size(free, base, unit), fmt_size(total, base, unit)));
                }
                if !self.hidden.is_empty() {
                    ui.separator();
                    ui.label(format!("{} hidden from treemap", fmt_int(self.hidden.len() as u64)));
                }
                if self.loaded_from.is_some() && self.tree.is_some() {
                    ui.separator();
                    ui.label(RichText::new(format!("Saved scan · {}", fmt_age(now_unix() - self.meta.scanned_at))).color(accent))
                        .on_hover_text("These results come from a saved scan. Use Shift+F5 on a folder to refresh just that folder, or F5 to rescan everything.");
                }
            });
        });
        if stop {
            self.stop_scan();
        }
    }

    fn status_text(&self) -> String {
        let Some(tree) = &self.tree else {
            return if self.message.is_empty() { "Use File > Scan… to begin".into() } else { self.message.clone() };
        };
        let describe = |n: NodeId, pct: bool| -> String {
            let sz = tree.size(n);
            let mut s = format!("{}  —  {}", tree.path(n), self.fmt(sz));
            if pct {
                let root = tree.dirs[0].size.max(1);
                s += &format!(" ({:.1}% of scan)", sz as f64 / root as f64 * 100.0);
                if n.is_dir() {
                    let d = &tree.dirs[n.0 as usize];
                    s += &format!(", {} files, {} folders", fmt_int(d.nfiles), fmt_int(d.ndirs));
                }
            }
            s
        };
        if let Some(h) = self.hover {
            return describe(h, false);
        }
        if !self.message.is_empty() {
            return self.message.clone();
        }
        if self.sel.len() > 1 {
            if self.sel.len() > 50_000 {
                return format!("{} items selected", fmt_int(self.sel.len() as u64));
            }
            let v: Vec<NodeId> = self.sel.iter().copied().filter(|&n| !tree.is_deleted(n)).collect();
            let top = top_level(tree, &v);
            let total: u64 = top.iter().map(|&n| tree.size(n)).sum();
            let files: u64 = top.iter().map(|&n| if n.is_dir() { tree.dirs[n.0 as usize].nfiles } else { 1 }).sum();
            return format!("{} items selected  —  {}, {} files", fmt_int(v.len() as u64), self.fmt(total), fmt_int(files));
        }
        match self.selected {
            Some(n) if !tree.is_deleted(n) => describe(n, true),
            _ => self.summary.clone(),
        }
    }

    // ---------------------------------------------------------------------------------------
    // treemap controls + treemap
    // ---------------------------------------------------------------------------------------
    fn ui_tm_controls(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(tree) = &self.tree {
                if self.tm_root != 0 {
                    if ui.button("⬆").on_hover_text("Zoom treemap out one level (Backspace)").clicked() {
                        self.acts.push(Action::TmZoom(tree.parent(NodeId::dir(self.tm_root)).map(|p| p.dir_id()).unwrap_or(0)));
                    }
                    if ui.button("⟲").on_hover_text("Show the whole scan (Esc)").clicked() {
                        self.acts.push(Action::TmZoom(0));
                    }
                    let p = tree.path(NodeId::dir(self.tm_root));
                    ui.add(egui::Label::new(RichText::new(format!("Treemap: {p}")).strong()).truncate()).on_hover_text(p);
                    ui.separator();
                }
            }
            ui.label("Colors:");
            let mut ch = false;
            ch |= ui.add(egui::Slider::new(&mut self.cfg.brightness, 30.0..=170.0).text("Brightness").suffix("%").fixed_decimals(0)).changed();
            ch |= ui.add(egui::Slider::new(&mut self.cfg.saturation, 0.0..=200.0).text("Saturation").suffix("%").fixed_decimals(0)).changed();
            ch |= ui.add(egui::Slider::new(&mut self.cfg.edge, 0.0..=100.0).text("Edge shading").suffix("%").fixed_decimals(0)).changed();
            if ui.checkbox(&mut self.cfg.classic_colors, "Classic WinDirStat palette").changed() {
                self.recompute_colors();
                ch = true;
            }
            if ui.button("Reset").clicked() {
                let d = Settings::default();
                self.cfg.brightness = d.brightness;
                self.cfg.saturation = d.saturation;
                self.cfg.edge = d.edge;
                ch = true;
            }
            if ch {
                self.tm.paint_dirty = true;
                self.touch_cfg();
            }
        });
    }

    fn ui_treemap(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let size = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let ppp = ctx.pixels_per_point();
        let bg = self.theme().tbg;
        ui.painter().rect_filled(rect, 0.0, rgb(bg));
        let Some(tree) = &self.tree else {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Nothing scanned yet", egui::FontId::proportional(16.0), Color32::GRAY);
            return;
        };
        if tree.is_deleted(NodeId::dir(self.tm_root)) {
            self.tm_root = 0;
            self.tm.layout_dirty = true;
        }
        let want = ((rect.width() * ppp) as usize, (rect.height() * ppp) as usize);
        if want.0 >= 8 && want.1 >= 8 {
            if self.tm.want != want {
                self.tm.want = want;
                self.tm.want_since = std::time::Instant::now();
            }
            let size_stale = self.tm.layout.as_ref().map(|l| (l.w, l.h)) != Some(want);
            let settled = self.tm.want_since.elapsed() > Duration::from_millis(150);
            if self.tm.layout_dirty || self.tm.layout.is_none() || (size_stale && settled) {
                self.tm.layout = Some(treemap::compute(tree, &self.hidden, &self.cut, self.tm_root, want.0, want.1));
                self.tm.layout_dirty = false;
                self.tm.paint_dirty = true;
            } else if size_stale {
                ctx.request_repaint_after(Duration::from_millis(160));
            }
            if self.tm.paint_dirty {
                if let Some(l) = &self.tm.layout {
                    let t = &self.themes[self.theme_idx];
                    let other = if t.palette.is_some() { t.other } else { [150, 150, 150] };
                    let px = treemap::paint(l, &self.ext_colors, other, bg, &self.look(), self.hl_ext);
                    let img = egui::ColorImage::from_rgba_premultiplied([l.w, l.h], bytemuck::cast_slice(&px));
                    match &mut self.tm.tex {
                        Some(t) => t.set(img, egui::TextureOptions::NEAREST),
                        None => self.tm.tex = Some(ctx.load_texture("treemap", img, egui::TextureOptions::NEAREST)),
                    }
                }
                self.tm.paint_dirty = false;
            }
        }
        let (Some(tex), Some(layout)) = (&self.tm.tex, &self.tm.layout) else { return };
        ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);

        let stale = (layout.w, layout.h) != want;
        self.hover = None;
        if !stale {
            if let Some(p) = resp.hover_pos() {
                let (px, py) = (((p.x - rect.min.x) * ppp) as usize, ((p.y - rect.min.y) * ppp) as usize);
                self.hover = layout.node_at(px, py);
            }
        }
        let mods = ui.input(|i| i.modifiers);
        if resp.double_clicked() {
            if let Some(n) = self.hover {
                // zoom into the folder under the pointer, one level below the current root
                let target = n.dir_id();
                let mut t = target;
                while let Some(p) = tree.parent(NodeId::dir(t)) {
                    if p.dir_id() == self.tm_root {
                        break;
                    }
                    t = p.dir_id();
                }
                let pick = if tree.parent(NodeId::dir(t)).map(|p| p.dir_id()) == Some(self.tm_root) { t } else { target };
                if pick != self.tm_root && !tree.is_deleted(NodeId::dir(pick)) {
                    self.acts.push(Action::TmZoom(pick));
                }
            }
        } else if resp.clicked() {
            if let Some(n) = self.hover {
                self.acts.push(Action::Select(n, Origin::Treemap, SelMode::from_mods(mods)));
            }
        }
        if resp.secondary_clicked() {
            self.ctx_node = self.hover;
            if let Some(n) = self.hover {
                self.acts.push(Action::Context(n, Origin::Treemap));
            }
        }
        // selection outlines
        if !stale {
            let outline = |n0: NodeId, w: f32| {
                let mut n = Some(n0);
                while let Some(x) = n {
                    if let Some(r) = layout.rects.get(&x) {
                        let rr = Rect::from_min_max(
                            pos2(rect.min.x + r[0] as f32 / ppp + 1.0, rect.min.y + r[1] as f32 / ppp + 1.0),
                            pos2(rect.min.x + r[2] as f32 / ppp - 1.0, rect.min.y + r[3] as f32 / ppp - 1.0),
                        );
                        ui.painter().rect_stroke(rr, 0.0, Stroke::new(w + 2.0_f32, Color32::BLACK), StrokeKind::Inside);
                        ui.painter().rect_stroke(rr, 0.0, Stroke::new(w, Color32::WHITE), StrokeKind::Inside);
                        return;
                    }
                    n = tree.parent(x);
                }
            };
            for &s in self.sel.iter().take(MAX_OUTLINES) {
                if Some(s) != self.selected && hidden_owner(tree, &self.hidden, s).is_none() && !tree.is_deleted(s) {
                    outline(s, 1.0);
                }
            }
            if let Some(sel) = self.selected {
                if hidden_owner(tree, &self.hidden, sel).is_none() && !tree.is_deleted(sel) {
                    outline(sel, 2.0);
                }
            }
        }
        let (ctx_node, hidden, acts, sel, busy) = (self.ctx_node, &self.hidden, &mut self.acts, &self.sel, self.job.is_some());
        resp.context_menu(|ui| match ctx_node {
            Some(n) => node_menu(ui, tree, hidden, &targets_for(Some(tree), sel, n), busy, acts),
            None => ui.close(),
        });
    }

    // ---------------------------------------------------------------------------------------
    // directory tree
    // ---------------------------------------------------------------------------------------
    fn ui_tree(&mut self, ui: &mut Ui) {
        let Some(tree) = &self.tree else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(if self.scanning() { "Working…" } else { "Use File > Scan… to choose a drive or folder" }).weak());
            });
            return;
        };
        let (base, unit) = (self.cfg.base, self.cfg.unit);
        let accent = self.theme().accent;
        let trough = self.theme().bg3;
        let rows = &self.rows;
        let selected = self.selected;
        let sel = &self.sel;
        let hidden = &self.hidden;
        let expanded = &self.expanded;
        let acts = &mut self.acts;
        let busy = self.job.is_some();
        let (sort_col, sort_desc) = (self.sort_col, self.sort_desc);
        let (pointer, primary_down, mods) = ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_down(), i.modifiers));
        let dragging = self.drag_anchor.is_some() && primary_down;
        let mut drag_start: Option<NodeId> = None;
        let mut drag_over: Option<usize> = None;
        let mut first_vis: Option<(usize, Rect)> = None;
        let mut last_vis: Option<(usize, Rect)> = None;

        let mut table = TableBuilder::new(ui)
            .id_salt("tree_table")
            .striped(true)
            .resizable(true)
            .sense(Sense::click_and_drag())
            .drag_to_scroll(false)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(330.0).at_least(120.0).clip(true))
            .column(Column::initial(120.0).at_least(60.0))
            .column(Column::initial(80.0).at_least(50.0))
            .column(Column::initial(95.0).at_least(60.0))
            .column(Column::initial(80.0).at_least(50.0))
            .column(Column::initial(80.0).at_least(50.0))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::remainder().at_least(110.0))
            .min_scrolled_height(0.0);
        if let Some(r) = self.scroll_to.take() {
            table = table.scroll_to_row(r, if dragging { None } else { Some(Align::Center) });
        }
        let titles = ["Name", "Subtree Percentage", "Percentage", "Size", "Items", "Files", "Subdirs", "Last Change"];
        table
            .header(22.0, |mut header| {
                for (i, t) in titles.iter().enumerate() {
                    header.col(|ui| {
                        let active = sort_col == i;
                        let arrow = if active { if sort_desc { " ⏷" } else { " ⏶" } } else { "" };
                        let r = ui.add(egui::Button::new(RichText::new(format!("{t}{arrow}")).strong()).frame(false));
                        if r.clicked() {
                            acts.push(Action::Sort(if i == 1 || i == 2 { 3 } else { i }));
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_H, rows.len(), |mut row| {
                    let idx = row.index();
                    let r = rows[idx];
                    if r.more > 0 {
                        row.col(|ui| {
                            ui.add_space(r.depth as f32 * 14.0 + 30.0);
                            ui.label(RichText::new(format!("… {} more items not shown", fmt_int(r.more as u64))).weak());
                        });
                        for _ in 0..7 {
                            row.col(|_| {});
                        }
                        return;
                    }
                    let n = r.node;
                    row.set_selected(sel.contains(&n) || selected == Some(n));
                    let is_dir = n.is_dir();
                    let size = tree.size(n);
                    let owner = hidden_owner(tree, hidden, n);
                    let dimmed = owner.is_some();
                    let has_kids = is_dir && {
                        let d = &tree.dirs[n.0 as usize];
                        !d.subdirs.is_empty() || !d.files.is_empty()
                    };
                    let frac = if r.psize > 0 { size as f64 / r.psize as f64 } else { 1.0 };
                    row.col(|ui| {
                        ui.add_space(r.depth as f32 * 14.0);
                        let (rc, resp) = ui.allocate_exact_size(vec2(14.0, ROW_H - 2.0), Sense::click());
                        if has_kids {
                            let c = rc.center();
                            let pts = if expanded.contains(&n) {
                                vec![pos2(c.x - 4.0, c.y - 2.5), pos2(c.x + 4.0, c.y - 2.5), pos2(c.x, c.y + 3.5)]
                            } else {
                                vec![pos2(c.x - 2.5, c.y - 4.0), pos2(c.x - 2.5, c.y + 4.0), pos2(c.x + 3.5, c.y)]
                            };
                            ui.painter().add(egui::Shape::convex_polygon(pts, ui.visuals().text_color(), Stroke::NONE));
                            if resp.clicked() {
                                acts.push(Action::Toggle(n));
                            }
                        }
                        let (ir, _) = ui.allocate_exact_size(vec2(14.0, 12.0), Sense::hover());
                        let a = if dimmed { 90 } else { 255 };
                        if is_dir {
                            ui.painter().rect_filled(ir.shrink2(vec2(0.0, 1.0)), 2.0, Color32::from_rgba_unmultiplied(230, 185, 80, a));
                        } else {
                            ui.painter().rect_stroke(ir.shrink2(vec2(2.0, 0.0)), 1.0, Stroke::new(1.2_f32, Color32::from_rgba_unmultiplied(190, 190, 200, a)), StrokeKind::Inside);
                        }
                        let mut name = tree.name(n);
                        if owner == Some(n) {
                            name += "   (hidden from treemap)";
                        }
                        let mut t = RichText::new(name);
                        if dimmed {
                            t = t.weak();
                        }
                        ui.add(egui::Label::new(t).truncate().selectable(false));
                    });
                    row.col(|ui| {
                        let (rc, _) = ui.allocate_exact_size(vec2(ui.available_width() - 4.0, ROW_H - 6.0), Sense::hover());
                        ui.painter().rect_filled(rc, 2.0, trough);
                        let mut f = rc;
                        f.set_width((rc.width() * frac as f32).max(0.0));
                        ui.painter().rect_filled(f, 2.0, if dimmed { accent.gamma_multiply(0.35) } else { accent });
                    });
                    let right = |ui: &mut Ui, s: String| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let mut t = RichText::new(s);
                            if dimmed {
                                t = t.weak();
                            }
                            ui.add(egui::Label::new(t).selectable(false));
                        });
                    };
                    row.col(|ui| right(ui, format!("{:.1}%", frac * 100.0)));
                    row.col(|ui| right(ui, fmt_size(size, base, unit)));
                    row.col(|ui| right(ui, if is_dir { fmt_int(tree.dirs[n.0 as usize].nfiles + tree.dirs[n.0 as usize].ndirs) } else { String::new() }));
                    row.col(|ui| right(ui, if is_dir { fmt_int(tree.dirs[n.0 as usize].nfiles) } else { String::new() }));
                    row.col(|ui| right(ui, if is_dir { fmt_int(tree.dirs[n.0 as usize].ndirs) } else { String::new() }));
                    row.col(|ui| {
                        ui.add(egui::Label::new(fmt_time(tree.mtime(n))).selectable(false));
                    });
                    let resp = row.response();
                    if first_vis.is_none() {
                        first_vis = Some((idx, resp.rect));
                    }
                    last_vis = Some((idx, resp.rect));
                    if resp.drag_started() && !mods.shift && !mods.command {
                        drag_start = Some(n);
                    }
                    if dragging {
                        if let Some(p) = pointer {
                            if p.y >= resp.rect.top() && p.y < resp.rect.bottom() {
                                drag_over = Some(idx);
                            }
                        }
                    }
                    if resp.clicked() {
                        acts.push(Action::Select(n, Origin::Tree, SelMode::from_mods(mods)));
                    }
                    if resp.double_clicked() && is_dir {
                        acts.push(Action::Toggle(n));
                    }
                    if resp.secondary_clicked() {
                        acts.push(Action::Context(n, Origin::Tree));
                    }
                    resp.context_menu(|ui| node_menu(ui, tree, hidden, &targets_for(Some(tree), sel, n), busy, acts));
                });
            });

        // drag selection: rubber-band over rows, scrolling when the pointer leaves the list
        if let Some(n) = drag_start {
            self.drag_anchor = Some(n);
            self.anchor = Some(n);
            self.acts.push(Action::Select(n, Origin::Tree, SelMode::Replace));
        }
        if dragging {
            if let (Some(p), Some((fi, fr)), Some((li, lr))) = (pointer, first_vis, last_vis) {
                if p.y < fr.top() && fi > 0 {
                    drag_over = Some(fi - 1);
                    self.scroll_to = Some(fi - 1);
                } else if p.y >= lr.bottom() && li + 1 < self.rows.len() {
                    drag_over = Some(li + 1);
                    self.scroll_to = Some(li + 1);
                }
            }
            if let Some(i) = drag_over {
                let mut i = i;
                while i > 0 && self.rows[i].more > 0 {
                    i -= 1;
                }
                let node = self.rows[i].node;
                if self.selected != Some(node) {
                    self.anchor = self.drag_anchor;
                    self.acts.push(Action::Select(node, Origin::Tree, SelMode::Range));
                }
            }
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        } else if !primary_down {
            self.drag_anchor = None;
        }
    }

    // ---------------------------------------------------------------------------------------
    // side panel
    // ---------------------------------------------------------------------------------------
    fn ui_side(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, SideTab::Extensions, "Extensions");
            ui.selectable_value(&mut self.tab, SideTab::Folders, "Largest folders");
            ui.selectable_value(&mut self.tab, SideTab::Files, "Largest files");
            ui.selectable_value(&mut self.tab, SideTab::Search, "🔍 Search");
        });
        ui.separator();
        match self.tab {
            SideTab::Extensions => self.ui_ext(ui),
            SideTab::Folders => self.ui_folders(ui),
            SideTab::Files => self.ui_files(ui),
            SideTab::Search => self.ui_search(ui),
        }
    }

    fn ui_ext(&mut self, ui: &mut Ui) {
        let Some(tree) = &self.tree else { return };
        let total: u64 = self.ext_stats.iter().map(|s| s[0]).sum::<u64>().max(1);
        let (base, unit) = (self.cfg.base, self.cfg.unit);
        let look = self.look();
        let (rows, stats, colors, hl, acts) = (&self.ext_rows, &self.ext_stats, &self.ext_colors, self.hl_ext, &mut self.acts);
        ui.label(RichText::new("Click to highlight on the treemap · right-click to list the files").weak().small());
        TableBuilder::new(ui)
            .id_salt("ext_table")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(90.0).clip(true))
            .column(Column::initial(50.0))
            .column(Column::initial(100.0))
            .column(Column::initial(70.0))
            .column(Column::remainder())
            .header(20.0, |mut h| {
                for t in ["Extension", "Color", "Bytes", "% Bytes", "Files"] {
                    h.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_H, rows.len(), |mut row| {
                    let id = rows[row.index()];
                    let [bytes, count] = stats[id as usize];
                    row.set_selected(hl == Some(id));
                    row.col(|ui| {
                        ui.add(egui::Label::new(&tree.ext_names[id as usize]).selectable(false));
                    });
                    row.col(|ui| {
                        let (rc, _) = ui.allocate_exact_size(vec2(36.0, 12.0), Sense::hover());
                        ui.painter().rect_filled(rc, 2.0, display_color(colors[id as usize], &look));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(fmt_size(bytes, base, unit)).selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(format!("{:.1}%", bytes as f64 / total as f64 * 100.0)).selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(fmt_int(count)).selectable(false));
                    });
                    let resp = row.response();
                    if resp.clicked() {
                        acts.push(Action::HighlightExt(if hl == Some(id) { None } else { Some(id) }));
                    }
                    resp.context_menu(|ui| {
                        if id != EXT_NONE && ui.button(format!("Find All {} Files", tree.ext_names[id as usize])).clicked() {
                            acts.push(Action::FindExt(id));
                            ui.close();
                        }
                        if ui.button(if hl == Some(id) { "Remove Highlight" } else { "Highlight on Treemap" }).clicked() {
                            acts.push(Action::HighlightExt(if hl == Some(id) { None } else { Some(id) }));
                            ui.close();
                        }
                    });
                });
            });
    }

    fn ui_folders(&mut self, ui: &mut Ui) {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Show folders with");
            let label = if self.cfg.folder_ge { "at least" } else { "at most" };
            egui::ComboBox::from_id_salt("folder_op").selected_text(label).width(80.0).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut self.cfg.folder_ge, false, "at most").changed();
                changed |= ui.selectable_value(&mut self.cfg.folder_ge, true, "at least").changed();
            });
            changed |= ui.add(egui::DragValue::new(&mut self.cfg.folder_n).range(0..=10_000_000u32).speed(1.0)).changed();
            ui.label("files inside");
        });
        let mut nf = self.cfg.folder_n as f64;
        let w = ui.available_width() - 12.0;
        ui.spacing_mut().slider_width = w.max(100.0);
        if ui.add(egui::Slider::new(&mut nf, 0.0..=1_000_000.0).logarithmic(true).smallest_positive(1.0).show_value(false)).changed() {
            self.cfg.folder_n = nf.round() as u32;
            changed = true;
        }
        changed |= ui.checkbox(&mut self.cfg.folder_recursive, "Count files recursively (include all subfolders)").changed();
        if ui.checkbox(&mut self.cfg.scope_selected, "Only inside the selected folder").changed() {
            changed = true;
        }
        if changed {
            self.schedule_lists();
            self.touch_cfg();
        }
        ui.separator();
        let Some(tree) = &self.tree else { return };
        let (base, unit) = (self.cfg.base, self.cfg.unit);
        let rec = self.cfg.folder_recursive;
        let root = tree.dirs[0].size.max(1);
        let (list, hidden, acts, selected, sel, busy) = (&self.folder_list, &self.hidden, &mut self.acts, self.selected, &self.sel, self.job.is_some());
        ui.label(RichText::new(format!("{} folders match — showing the largest {}", fmt_int(self.folder_matches as u64), list.len())).weak());
        let mods = ui.input(|i| i.modifiers);
        let h = ui.available_height();
        TableBuilder::new(ui)
            .id_salt("folder_table")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .max_scroll_height(h)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::remainder().at_least(120.0).clip(true))
            .column(Column::initial(85.0))
            .column(Column::initial(if rec { 95.0 } else { 80.0 }))
            .column(Column::initial(60.0))
            .column(Column::initial(48.0))
            .header(20.0, |mut hd| {
                for t in ["Folder", "Size", if rec { "Files (all levels)" } else { "Files (direct)" }, "Folders", "%"] {
                    hd.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_H, list.len(), |mut row| {
                    let id = list[row.index()];
                    let d = &tree.dirs[id as usize];
                    let node = NodeId::dir(id);
                    row.set_selected(sel.contains(&node) || selected == Some(node));
                    row.col(|ui| {
                        ui.add(egui::Label::new(tree.path(node)).truncate().selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(fmt_size(d.size, base, unit)).selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(fmt_int(if rec { d.nfiles } else { d.dfiles as u64 })).selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(fmt_int(d.ndirs)).selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(format!("{:.1}%", d.size as f64 / root as f64 * 100.0)).selectable(false));
                    });
                    let resp = row.response();
                    if resp.clicked() {
                        acts.push(Action::Select(node, Origin::List, SelMode::from_mods(mods)));
                    }
                    if resp.double_clicked() {
                        acts.push(Action::Open(tree.path(node)));
                    }
                    if resp.secondary_clicked() {
                        acts.push(Action::Context(node, Origin::List));
                    }
                    resp.context_menu(|ui| node_menu(ui, tree, hidden, &targets_for(Some(tree), sel, node), busy, acts));
                });
            });
    }

    fn ui_files(&mut self, ui: &mut Ui) {
        if ui.checkbox(&mut self.cfg.scope_selected, "Only inside the selected folder").changed() {
            self.schedule_lists();
            self.touch_cfg();
        }
        let Some(tree) = &self.tree else { return };
        ui.label(RichText::new(format!("Showing the {} largest files", self.file_list.len())).weak());
        let (base, unit) = (self.cfg.base, self.cfg.unit);
        node_table(ui, "file_table", tree, &self.file_list, &self.hidden, &self.sel, self.selected, self.job.is_some(), &mut self.acts, base, unit);
    }

    fn ui_search(&mut self, ui: &mut Ui) {
        let mut run = false;
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.search.query).hint_text("name contains…  or wildcards: *.iso, IMG_????.jpg").desired_width(ui.available_width() - 70.0));
            if self.search.focus {
                r.request_focus();
                self.search.focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                run = true;
            }
            if ui.button("Search").clicked() {
                run = true;
            }
        });
        let mut ch = false;
        ui.horizontal(|ui| {
            ch |= ui.checkbox(&mut self.cfg.search_files, "Files").changed();
            ch |= ui.checkbox(&mut self.cfg.search_dirs, "Folders").changed();
            ch |= ui.checkbox(&mut self.cfg.search_case, "Match case").changed();
            ch |= ui.checkbox(&mut self.cfg.scope_selected, "Only inside the selected folder").changed();
        });
        if ch {
            self.touch_cfg();
            if self.search.ran.is_some() {
                run = true;
            }
        }
        if run {
            self.run_search();
        }
        ui.separator();
        let Some(tree) = &self.tree else { return };
        match &self.search.ran {
            None => {
                ui.label(RichText::new("Type a name and press Enter. Results are sorted by size.\nShift/Ctrl+click to select several, then right-click.").weak());
                return;
            }
            Some(q) => {
                let total: u64 = self.search.results.iter().map(|&n| tree.size(n)).sum();
                ui.label(
                    RichText::new(format!(
                        "{} matches for “{q}”{} — top {} total {}",
                        fmt_int(self.search.matches as u64),
                        if self.cfg.scope_selected && self.scope_dir != 0 { " in the selected folder" } else { "" },
                        fmt_int(self.search.results.len() as u64),
                        self.fmt(total)
                    ))
                    .weak(),
                );
            }
        }
        let (base, unit) = (self.cfg.base, self.cfg.unit);
        node_table(ui, "search_table", tree, &self.search.results, &self.hidden, &self.sel, self.selected, self.job.is_some(), &mut self.acts, base, unit);
    }

    // ---------------------------------------------------------------------------------------
    // dialogs
    // ---------------------------------------------------------------------------------------
    fn ui_dialogs(&mut self, ctx: &egui::Context) {
        self.ui_picker(ctx);
        self.ui_save_dialog(ctx);
        self.ui_export_dialog(ctx);
        self.ui_saved_scans(ctx);

        let mut decision: Option<bool> = None;
        if let Some(c) = &self.confirm {
            egui::Window::new(if c.kind == ConfirmKind::Trash { "Move to Trash?" } else { "Delete permanently?" })
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.set_min_width(560.0);
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        ui.label(&c.text);
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            decision = Some(false);
                        }
                        if ui.button(RichText::new("Yes").strong()).clicked() {
                            decision = Some(true);
                        }
                    });
                });
        }
        match decision {
            Some(true) => self.run_confirmed(),
            Some(false) => self.confirm = None,
            None => {}
        }
        if self.about {
            let mut open = true;
            egui::Window::new("About").open(&mut open).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
                ui.heading("WinDirStat for Linux (native)");
                ui.label("Parallel disk-usage scanner, directory tree, extension statistics\nand treemap. Written in Rust.\nInspired by WinDirStat, WizTree and KDirStat. GPL-2.0-or-later.");
                ui.separator();
                ui.strong("Keyboard and mouse");
                egui::Grid::new("keys").num_columns(2).striped(true).show(ui, |ui| {
                    for (k, d) in [
                        ("Ctrl+O", "Scan a drive or folder"),
                        ("F5 / Shift+F5", "Rescan everything / refresh the selected folder"),
                        ("Ctrl+S / Ctrl+Shift+O", "Save scan / open a saved scan"),
                        ("Ctrl+F", "Search by name or wildcard"),
                        ("Click, Shift+click, Ctrl+click", "Select one, a range, or add/remove"),
                        ("Drag over rows", "Select a range in the tree"),
                        ("↑ ↓ PgUp PgDn Home End", "Move in the tree (with Shift: extend)"),
                        ("← →, Enter", "Collapse / expand"),
                        ("Ctrl+A, Ctrl+C", "Select all rows, copy paths"),
                        ("Del / Shift+Del", "Move to trash / delete permanently"),
                        ("Double-click treemap", "Zoom into that folder"),
                        ("Backspace / Esc", "Treemap zoom out / reset"),
                        ("Ctrl+= / Ctrl+- / Ctrl+0", "Interface zoom (remembered)"),
                    ] {
                        ui.monospace(k);
                        ui.label(d);
                        ui.end_row();
                    }
                });
            });
            self.about = open;
        }
    }

    fn ui_picker(&mut self, ctx: &egui::Context) {
        let mut start: Option<String> = None;
        let mut load: Option<std::path::PathBuf> = None;
        let mut close_picker = false;
        let mut manage = false;
        let recent = self.cfg.recent.clone();
        if let Some(p) = &mut self.picker {
            egui::Window::new("Select what to scan")
                .collapsible(false)
                .resizable(true)
                .default_width(720.0)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    if !p.error.is_empty() && p.entries.is_empty() {
                        ui.colored_label(Color32::LIGHT_RED, &p.error);
                    }
                    ui.label("Pick a mounted drive, or browse to a folder:");
                    let mut pick: Option<String> = None;
                    egui::ScrollArea::vertical().id_salt("mounts").max_height(120.0).show(ui, |ui| {
                        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
                        if ui.selectable_label(p.path == home, format!("Home folder   —   {home}")).clicked() {
                            pick = Some(home);
                        }
                        for (mnt, desc) in &p.mounts {
                            if ui.selectable_label(&p.path == mnt, desc).clicked() {
                                pick = Some(mnt.clone());
                            }
                        }
                    });
                    if !recent.is_empty() {
                        ui.collapsing(format!("Recent folders ({})", recent.len()), |ui| {
                            egui::ScrollArea::vertical().id_salt("recent").max_height(110.0).show(ui, |ui| {
                                for r in &recent {
                                    let resp = ui.selectable_label(&p.path == r, r);
                                    if resp.clicked() {
                                        pick = Some(r.clone());
                                    }
                                    if resp.double_clicked() {
                                        start = Some(r.clone());
                                    }
                                }
                            });
                        });
                    }
                    if !p.saved.is_empty() {
                        egui::CollapsingHeader::new(format!("Saved scans ({}) — open instantly, no rescanning", p.saved.len())).default_open(true).show(ui, |ui| {
                            egui::ScrollArea::vertical().id_salt("saved").max_height(130.0).show(ui, |ui| {
                                for (path, m, _) in p.saved.iter().take(50) {
                                    ui.horizontal(|ui| {
                                        if ui.button("Open").clicked() {
                                            load = Some(path.clone());
                                        }
                                        ui.label(format!(
                                            "{}  —  {}, {} files  ·  {} ({})",
                                            m.root,
                                            fmt_size(m.size, Base::Binary, Unit::Auto),
                                            fmt_int(m.files),
                                            fmt_time(m.scanned_at),
                                            fmt_age(now_unix() - m.scanned_at)
                                        ));
                                    });
                                }
                            });
                            if ui.small_button("Manage saved scans…").clicked() {
                                manage = true;
                            }
                        });
                    }
                    if let Some(x) = pick {
                        p.path = x;
                        p.refresh();
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("⬆ Up").clicked() {
                            p.go_up();
                        }
                        let r = ui.add(egui::TextEdit::singleline(&mut p.path).desired_width(f32::INFINITY));
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            p.refresh();
                        }
                    });
                    let mut enter: Option<String> = None;
                    egui::ScrollArea::vertical().id_salt("subdirs").max_height(200.0).show(ui, |ui| {
                        for e in &p.entries {
                            if ui.selectable_label(false, format!("{e}/")).double_clicked() {
                                enter = Some(e.clone());
                            }
                        }
                        if !p.error.is_empty() && !p.entries.is_empty() {
                            ui.colored_label(Color32::LIGHT_RED, &p.error);
                        }
                    });
                    if let Some(e) = enter {
                        p.enter(&e);
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Scan this folder").strong()).clicked() {
                            start = Some(p.path.clone());
                        }
                        if ui.button("Open Saved Scan…").clicked() {
                            manage = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close_picker = true;
                        }
                    });
                });
        }
        if close_picker {
            self.picker = None;
        }
        if manage {
            self.picker = None;
            self.open_saved_scans();
        }
        if let Some(path) = start {
            self.picker = None;
            self.start_scan(path);
        } else if let Some(f) = load {
            self.picker = None;
            self.start_load(f);
        }
    }

    fn ui_save_dialog(&mut self, ctx: &egui::Context) {
        let mut action: Option<bool> = None;
        let size = self.tree.as_ref().map(|t| (t.dirs[0].nfiles, t.dirs[0].ndirs)).unwrap_or_default();
        if let Some(d) = &mut self.save_dialog {
            egui::Window::new("Save scan").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
                ui.set_width(600.0);
                ui.label(format!("Saves the complete results ({} files, {} folders) so you can reopen them later without scanning the drive again.", fmt_int(size.0), fmt_int(size.1)));
                ui.add_space(4.0);
                ui.label("File:");
                let r = ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(f32::INFINITY));
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    action = Some(true);
                }
                ui.label(RichText::new(format!("Files saved in {} are listed under File > Open Saved Scan.", savefile::scans_dir().display())).weak().small());
                if !d.error.is_empty() {
                    ui.colored_label(Color32::LIGHT_RED, &d.error);
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        action = Some(false);
                    }
                    if ui.button(RichText::new("Save").strong()).clicked() {
                        action = Some(true);
                    }
                });
            });
        }
        match action {
            Some(false) => self.save_dialog = None,
            Some(true) => {
                let mut p = self.save_dialog.as_ref().map(|d| d.path.trim().to_string()).unwrap_or_default();
                if let Some(rest) = p.strip_prefix("~/") {
                    p = format!("{}/{rest}", std::env::var("HOME").unwrap_or_default());
                }
                let mut path = std::path::PathBuf::from(&p);
                if path.extension().and_then(|e| e.to_str()) != Some(savefile::EXT) {
                    path.set_extension(savefile::EXT);
                }
                if p.is_empty() {
                    if let Some(d) = &mut self.save_dialog {
                        d.error = "Enter a file name".into();
                    }
                } else if self.save_to(&path) {
                    self.save_dialog = None;
                } else if let Some(d) = &mut self.save_dialog {
                    d.error = self.message.clone();
                }
            }
            None => {}
        }
    }

    fn ui_export_dialog(&mut self, ctx: &egui::Context) {
        let mut action: Option<bool> = None;
        if let Some(d) = &mut self.export_dialog {
            egui::Window::new("Export to CSV").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0)).show(ctx, |ui| {
                ui.set_width(600.0);
                ui.label("File:");
                ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(f32::INFINITY));
                ui.checkbox(&mut d.with_files, "Include every file (not only folders)");
                ui.label(RichText::new("Columns: Path, Type, Size (bytes), Files, Folders, Last Change").weak().small());
                if !d.error.is_empty() {
                    ui.colored_label(Color32::LIGHT_RED, &d.error);
                }
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        action = Some(false);
                    }
                    if ui.button(RichText::new("Export").strong()).clicked() {
                        action = Some(true);
                    }
                });
            });
        }
        match action {
            Some(false) => self.export_dialog = None,
            Some(true) => {
                let (p, wf) = self.export_dialog.as_ref().map(|d| (d.path.trim().to_string(), d.with_files)).unwrap_or_default();
                match self.export_csv(&p, wf) {
                    Ok(rows) => {
                        self.message = format!("Exported {} rows to {p}", fmt_int(rows));
                        self.export_dialog = None;
                    }
                    Err(e) => {
                        if let Some(d) = &mut self.export_dialog {
                            d.error = e;
                        }
                    }
                }
            }
            None => {}
        }
    }

    fn ui_saved_scans(&mut self, ctx: &egui::Context) {
        let mut load: Option<std::path::PathBuf> = None;
        let mut close = false;
        let mut reload = false;
        if let Some(s) = &mut self.saved_scans {
            let mut open = true;
            egui::Window::new("Saved scans")
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_width(760.0)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(RichText::new(format!("In {}", savefile::scans_dir().display())).weak());
                    if s.entries.is_empty() {
                        ui.label("No saved scans yet. Use File > Save Scan (Ctrl+S) after a scan, or turn on Options > Auto-save.");
                    }
                    let mut del: Option<std::path::PathBuf> = None;
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        egui::Grid::new("saved_grid").num_columns(4).striped(true).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                            ui.strong("");
                            ui.strong("Scanned folder");
                            ui.strong("Size / files");
                            ui.strong("Scanned on");
                            ui.end_row();
                            for (path, m, len) in &s.entries {
                                ui.horizontal(|ui| {
                                    if ui.button("Open").clicked() {
                                        load = Some(path.clone());
                                    }
                                    if ui.button("🗑").on_hover_text("Delete this saved scan file").clicked() {
                                        del = Some(path.clone());
                                    }
                                });
                                ui.label(&m.root).on_hover_text(format!("{}\nfile size {}", path.display(), fmt_size(*len, Base::Binary, Unit::Auto)));
                                ui.label(format!("{}, {} files", fmt_size(m.size, Base::Binary, Unit::Auto), fmt_int(m.files)));
                                ui.label(format!("{} ({}){}", fmt_time(m.scanned_at), fmt_age(now_unix() - m.scanned_at), if m.cancelled { ", partial" } else { "" }));
                                ui.end_row();
                            }
                        });
                    });
                    if let Some(d) = del {
                        s.ask_delete = Some(d);
                    }
                    if let Some(d) = s.ask_delete.clone() {
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label(format!("Delete {}?", d.display()));
                            if ui.button("Delete").clicked() {
                                if let Err(e) = std::fs::remove_file(&d) {
                                    s.error = e.to_string();
                                }
                                s.ask_delete = None;
                                reload = true;
                            }
                            if ui.button("Keep").clicked() {
                                s.ask_delete = None;
                            }
                        });
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("Open a file:");
                        let r = ui.add(egui::TextEdit::singleline(&mut s.open_path).hint_text("/path/to/scan.ldscan").desired_width(420.0));
                        let go = ui.button("Open").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
                        if go && !s.open_path.trim().is_empty() {
                            let p = std::path::PathBuf::from(s.open_path.trim());
                            match savefile::read_meta(&p) {
                                Ok(_) => load = Some(p),
                                Err(e) => s.error = e,
                            }
                        }
                    });
                    if !s.error.is_empty() {
                        ui.colored_label(Color32::LIGHT_RED, &s.error);
                    }
                });
            if !open {
                close = true;
            }
        }
        if reload {
            if let Some(s) = &mut self.saved_scans {
                s.entries = savefile::list_saved();
            }
        }
        if close {
            self.saved_scans = None;
        }
        if let Some(f) = load {
            self.saved_scans = None;
            if !self.scanning() {
                self.start_load(f);
            }
        }
    }
}

/// A list of files/folders (largest files, search results) with multi-select and context menu.
#[allow(clippy::too_many_arguments)]
fn node_table(
    ui: &mut Ui,
    id: &str,
    tree: &Tree,
    list: &[NodeId],
    hidden: &FxHashSet<NodeId>,
    sel: &FxHashSet<NodeId>,
    selected: Option<NodeId>,
    busy: bool,
    acts: &mut Vec<Action>,
    base: Base,
    unit: Unit,
) {
    let mods = ui.input(|i| i.modifiers);
    let h = ui.available_height();
    TableBuilder::new(ui)
        .id_salt(id)
        .striped(true)
        .resizable(true)
        .sense(Sense::click())
        .max_scroll_height(h)
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::remainder().at_least(120.0).clip(true))
        .column(Column::initial(90.0))
        .column(Column::initial(120.0))
        .header(20.0, |mut hd| {
            for t in ["Path", "Size", "Modified"] {
                hd.col(|ui| {
                    ui.strong(t);
                });
            }
        })
        .body(|body| {
            body.rows(ROW_H, list.len(), |mut row| {
                let node = list[row.index()];
                if tree.is_deleted(node) {
                    row.col(|_| {});
                    row.col(|_| {});
                    row.col(|_| {});
                    return;
                }
                row.set_selected(sel.contains(&node) || selected == Some(node));
                row.col(|ui| {
                    let (ir, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                    if node.is_dir() {
                        ui.painter().rect_filled(ir.shrink2(vec2(0.0, 1.0)), 2.0, Color32::from_rgb(230, 185, 80));
                    } else {
                        ui.painter().rect_stroke(ir.shrink2(vec2(2.0, 0.0)), 1.0, Stroke::new(1.2_f32, Color32::from_rgb(190, 190, 200)), StrokeKind::Inside);
                    }
                    ui.add(egui::Label::new(tree.path(node)).truncate().selectable(false));
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(fmt_size(tree.size(node), base, unit)).selectable(false));
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(fmt_time(tree.mtime(node))).selectable(false));
                });
                let resp = row.response();
                if resp.clicked() {
                    acts.push(Action::Select(node, Origin::List, SelMode::from_mods(mods)));
                }
                if resp.double_clicked() {
                    let p = tree.path(node);
                    acts.push(Action::Open(if node.is_dir() { p } else { parent_path(&p) }));
                }
                if resp.secondary_clicked() {
                    acts.push(Action::Context(node, Origin::List));
                }
                resp.context_menu(|ui| node_menu(ui, tree, hidden, &targets_for(Some(tree), sel, node), busy, acts));
            });
        });
}

fn fmt_dur(sec: f64) -> String {
    let s = sec.max(0.0) as u64;
    if s < 3600 {
        format!("{}:{:02}", s / 60, s % 60)
    } else {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    }
}
