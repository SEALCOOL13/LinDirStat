//! All egui drawing code.
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use egui::{pos2, vec2, Align, Align2, Color32, Key, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui};
use egui_extras::{Column, TableBuilder};
use rustc_hash::FxHashSet;

use crate::app::*;
use crate::scan::*;
use crate::theme;
use crate::treemap;
use crate::units::{fmt_int, fmt_size, fmt_time, Base, Unit};

const ROW_H: f32 = 18.0;

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn display_color(c: [u8; 3], look: &treemap::Look) -> Color32 {
    let a = treemap::adjust(c, look, 1.0);
    Color32::from_rgb(a[0].clamp(0.0, 255.0) as u8, a[1].clamp(0.0, 255.0) as u8, a[2].clamp(0.0, 255.0) as u8)
}

pub fn node_menu(ui: &mut Ui, tree: &Tree, hidden: &FxHashSet<NodeId>, node: NodeId, acts: &mut Vec<Action>) {
    let path = tree.path(node);
    if node.is_dir() {
        if ui.button("Open Folder").clicked() {
            acts.push(Action::Open(path.clone()));
            ui.close();
        }
    } else {
        if ui.button("Open File").clicked() {
            acts.push(Action::Open(path.clone()));
            ui.close();
        }
        if ui.button("Open Containing Folder").clicked() {
            let dir = std::path::Path::new(&path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            acts.push(Action::Open(dir));
            ui.close();
        }
    }
    if ui.button("Copy Path").clicked() {
        ui.ctx().copy_text(path.clone());
        acts.push(Action::CopyPath(path.clone()));
        ui.close();
    }
    ui.separator();
    let can_hide = tree.parent(node).is_some();
    match hidden_owner(tree, hidden, node) {
        _ if !can_hide => {
            ui.add_enabled(false, egui::Button::new("Hide from Treemap"));
        }
        Some(o) if o == node => {
            if ui.button("Show on Treemap").clicked() {
                acts.push(Action::Show(node));
                ui.close();
            }
        }
        Some(o) => {
            if ui.button(format!("Show on Treemap (hidden via “{}”)", tree.name(o))).clicked() {
                acts.push(Action::Show(o));
                ui.close();
            }
        }
        None => {
            if ui.button("Hide from Treemap").clicked() {
                acts.push(Action::Hide(node));
                ui.close();
            }
        }
    }
    ui.separator();
    let can_del = can_hide && !tree.is_small_agg_file(node);
    if ui.add_enabled(can_del, egui::Button::new("Move to Trash…")).clicked() {
        acts.push(Action::Trash(node));
        ui.close();
    }
    if ui.add_enabled(can_del, egui::Button::new("Delete Permanently…")).clicked() {
        acts.push(Action::Delete(node));
        ui.close();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scan(ctx);
        let (open, rescan, esc) =
            ctx.input(|i| (i.modifiers.command && i.key_pressed(Key::O), i.key_pressed(Key::F5), i.key_pressed(Key::Escape)));
        if open && !self.scanning() {
            self.open_picker();
        }
        if rescan && !self.scanning() && !self.scan_path.is_empty() {
            self.start_scan(self.scan_path.clone());
        }
        if esc {
            if self.picker.is_some() {
                self.picker = None;
            } else if self.confirm.is_some() {
                self.confirm = None;
            } else {
                self.stop_scan();
            }
        }
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
        let h = ctx.content_rect().height() * 0.42;
        egui::TopBottomPanel::bottom("treemap")
            .resizable(true)
            .default_height(h)
            .min_height(120.0)
            .show(ctx, |ui| self.ui_treemap(ui, ctx));
        egui::SidePanel::right("side").resizable(true).default_width(540.0).min_width(320.0).show(ctx, |ui| self.ui_side(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.ui_tree(ui));
        self.ui_dialogs(ctx);
        self.process_actions();

        if let Some(t) = self.cfg_dirty {
            if t.elapsed() > Duration::from_millis(600) {
                self.cfg.save();
                self.cfg_dirty = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(650));
            }
        }
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

    // ---------------------------------------------------------------------------------------
    // menu
    // ---------------------------------------------------------------------------------------
    fn ui_menu(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                let idle = !self.scanning();
                if ui.add_enabled(idle, egui::Button::new("Scan…").shortcut_text("Ctrl+O")).clicked() {
                    self.open_picker();
                    ui.close();
                }
                if ui.add_enabled(idle && !self.scan_path.is_empty(), egui::Button::new("Rescan").shortcut_text("F5")).clicked() {
                    self.start_scan(self.scan_path.clone());
                    ui.close();
                }
                if ui.add_enabled(!idle, egui::Button::new("Stop").shortcut_text("Esc")).clicked() {
                    self.stop_scan();
                    ui.close();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Options", |ui| {
                let mut ch = false;
                ch |= ui.checkbox(&mut self.cfg.one_fs, "Stay on one filesystem").changed();
                ch |= ui.checkbox(&mut self.cfg.apparent, "Use apparent size (instead of disk usage)").changed();
                ch |= ui.checkbox(&mut self.cfg.precount, "Accurate progress (count items in the background)").changed();
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
                ui.separator();
                if ui.add_enabled(!self.hidden.is_empty(), egui::Button::new("Show All Hidden in Treemap")).clicked() {
                    self.clear_hidden();
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("About").clicked() {
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
                let (frac, text) = if p.phase.load(Relaxed) == 2 {
                    (1.0, "Building tree…".to_string())
                } else if known && ready {
                    let f = (entries as f32 / total.max(1) as f32).min(0.99);
                    let eta = if f > 0.03 && el > 2.0 { format!("  ·  ETA {}", fmt_dur(el * (1.0 - f as f64) / f as f64)) } else { String::new() };
                    (f, format!("{:.0}%  ·  {} items  ·  {}{}", f * 100.0, fmt_int(entries), fmt_size(p.bytes.load(Relaxed), base, unit), eta))
                } else {
                    (0.0, format!("{} items  ·  {}  ·  {} threads", fmt_int(entries), fmt_size(p.bytes.load(Relaxed), base, unit), p.threads.load(Relaxed)))
                };
                let mut bar = egui::ProgressBar::new(frac).desired_width(440.0).text(text).fill(accent);
                if !(known && ready) && p.phase.load(Relaxed) != 2 {
                    bar = bar.animate(true);
                }
                ui.add(bar);
                if ui.button("Stop").clicked() {
                    stop = true;
                }
                let cur = p.current.lock().map(|g| g.clone()).unwrap_or_default();
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
            ui.label("Treemap colors:");
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
        let want = ((rect.width() * ppp) as usize, (rect.height() * ppp) as usize);
        if want.0 >= 8 && want.1 >= 8 {
            if self.tm.want != want {
                self.tm.want = want;
                self.tm.want_since = std::time::Instant::now();
            }
            let size_stale = self.tm.layout.as_ref().map(|l| (l.w, l.h)) != Some(want);
            let settled = self.tm.want_since.elapsed() > Duration::from_millis(150);
            if self.tm.layout_dirty || self.tm.layout.is_none() || (size_stale && settled) {
                self.tm.layout = Some(treemap::compute(tree, &self.hidden, &self.cut, want.0, want.1));
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
        if resp.clicked() {
            if let Some(n) = self.hover {
                self.acts.push(Action::Select(n, Origin::Treemap));
            }
        }
        if resp.secondary_clicked() {
            self.ctx_node = self.hover;
            if let Some(n) = self.hover {
                self.acts.push(Action::Select(n, Origin::Treemap));
            }
        }
        // selection outline
        if let Some(sel) = self.selected {
            if !stale && hidden_owner(tree, &self.hidden, sel).is_none() {
                let mut n = Some(sel);
                while let Some(x) = n {
                    if let Some(r) = layout.rects.get(&x) {
                        let rr = Rect::from_min_max(
                            pos2(rect.min.x + r[0] as f32 / ppp + 1.0, rect.min.y + r[1] as f32 / ppp + 1.0),
                            pos2(rect.min.x + r[2] as f32 / ppp - 1.0, rect.min.y + r[3] as f32 / ppp - 1.0),
                        );
                        ui.painter().rect_stroke(rr, 0.0, Stroke::new(4.0, Color32::BLACK), StrokeKind::Inside);
                        ui.painter().rect_stroke(rr, 0.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
                        break;
                    }
                    n = tree.parent(x);
                }
            }
        }
        let (ctx_node, hidden, acts) = (self.ctx_node, &self.hidden, &mut self.acts);
        resp.context_menu(|ui| match ctx_node {
            Some(n) => node_menu(ui, tree, hidden, n, acts),
            None => ui.close(),
        });
    }

    // ---------------------------------------------------------------------------------------
    // directory tree
    // ---------------------------------------------------------------------------------------
    fn ui_tree(&mut self, ui: &mut Ui) {
        let Some(tree) = &self.tree else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(if self.scanning() { "Scanning…" } else { "Use File > Scan… to choose a drive or folder" }).weak());
            });
            return;
        };
        let (base, unit) = (self.cfg.base, self.cfg.unit);
        let accent = self.theme().accent;
        let trough = self.theme().bg3;
        let rows = &self.rows;
        let selected = self.selected;
        let hidden = &self.hidden;
        let expanded = &self.expanded;
        let acts = &mut self.acts;
        let (sort_col, sort_desc) = (self.sort_col, self.sort_desc);

        let mut table = TableBuilder::new(ui)
            .id_salt("tree_table")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
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
            table = table.scroll_to_row(r, Some(Align::Center));
        }
        let titles = ["Name", "Subtree Percentage", "Percentage", "Size", "Items", "Files", "Subdirs", "Last Change"];
        table
            .header(22.0, |mut header| {
                for (i, t) in titles.iter().enumerate() {
                    header.col(|ui| {
                        let active = sort_col == i || (i <= 2 && i > 0 && sort_col == 3 && false);
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
                    let r = rows[row.index()];
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
                    row.set_selected(selected == Some(n));
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
                            ui.painter().rect_stroke(ir.shrink2(vec2(2.0, 0.0)), 1.0, Stroke::new(1.2, Color32::from_rgba_unmultiplied(190, 190, 200, a)), StrokeKind::Inside);
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
                    if resp.clicked() {
                        acts.push(Action::Select(n, Origin::Tree));
                    }
                    if resp.secondary_clicked() {
                        acts.push(Action::Select(n, Origin::Tree));
                    }
                    resp.context_menu(|ui| node_menu(ui, tree, hidden, n, acts));
                });
            });
    }

    // ---------------------------------------------------------------------------------------
    // side panel
    // ---------------------------------------------------------------------------------------
    fn ui_side(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, SideTab::Extensions, "Extensions");
            ui.selectable_value(&mut self.tab, SideTab::Folders, "Largest folders");
            ui.selectable_value(&mut self.tab, SideTab::Files, "Largest files");
        });
        ui.separator();
        match self.tab {
            SideTab::Extensions => self.ui_ext(ui),
            SideTab::Folders => self.ui_folders(ui),
            SideTab::Files => self.ui_files(ui),
        }
    }

    fn ui_ext(&mut self, ui: &mut Ui) {
        let Some(tree) = &self.tree else { return };
        let total: u64 = self.ext_stats.iter().map(|s| s[0]).sum::<u64>().max(1);
        let (base, unit) = (self.cfg.base, self.cfg.unit);
        let look = self.look();
        let (rows, stats, colors, hl, acts) = (&self.ext_rows, &self.ext_stats, &self.ext_colors, self.hl_ext, &mut self.acts);
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
                    if row.response().clicked() {
                        acts.push(Action::HighlightExt(if hl == Some(id) { None } else { Some(id) }));
                    }
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
        let (list, hidden, acts, selected) = (&self.folder_list, &self.hidden, &mut self.acts, self.selected);
        ui.label(RichText::new(format!("{} folders match — showing the largest {}", fmt_int(self.folder_matches as u64), list.len())).weak());
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
                    row.set_selected(selected == Some(node));
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
                        acts.push(Action::Select(node, Origin::List));
                    }
                    if resp.double_clicked() {
                        acts.push(Action::Open(tree.path(node)));
                    }
                    resp.context_menu(|ui| node_menu(ui, tree, hidden, node, acts));
                });
            });
    }

    fn ui_files(&mut self, ui: &mut Ui) {
        if ui.checkbox(&mut self.cfg.scope_selected, "Only inside the selected folder").changed() {
            self.schedule_lists();
            self.touch_cfg();
        }
        let Some(tree) = &self.tree else { return };
        let (base, unit) = (self.cfg.base, self.cfg.unit);
        let (list, hidden, acts, selected) = (&self.file_list, &self.hidden, &mut self.acts, self.selected);
        ui.label(RichText::new(format!("Showing the {} largest files", list.len())).weak());
        let h = ui.available_height();
        TableBuilder::new(ui)
            .id_salt("file_table")
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .max_scroll_height(h)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::remainder().at_least(120.0).clip(true))
            .column(Column::initial(90.0))
            .column(Column::initial(120.0))
            .header(20.0, |mut hd| {
                for t in ["File", "Size", "Modified"] {
                    hd.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_H, list.len(), |mut row| {
                    let node = list[row.index()];
                    row.set_selected(selected == Some(node));
                    row.col(|ui| {
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
                        acts.push(Action::Select(node, Origin::List));
                    }
                    if resp.double_clicked() {
                        let p = tree.path(node);
                        acts.push(Action::Open(std::path::Path::new(&p).parent().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default()));
                    }
                    resp.context_menu(|ui| node_menu(ui, tree, hidden, node, acts));
                });
            });
    }

    // ---------------------------------------------------------------------------------------
    // dialogs
    // ---------------------------------------------------------------------------------------
    fn ui_dialogs(&mut self, ctx: &egui::Context) {
        let mut start: Option<String> = None;
        let mut close_picker = false;
        if let Some(p) = &mut self.picker {
            egui::Window::new("Select what to scan")
                .collapsible(false)
                .resizable(true)
                .default_width(680.0)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
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
                        if !p.error.is_empty() {
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
                        if ui.button("Cancel").clicked() {
                            close_picker = true;
                        }
                    });
                });
        }
        if close_picker {
            self.picker = None;
        }
        if let Some(path) = start {
            self.picker = None;
            self.start_scan(path);
        }

        let mut decision: Option<bool> = None;
        if let Some(c) = &self.confirm {
            egui::Window::new(if c.kind == ConfirmKind::Trash { "Move to Trash?" } else { "Delete permanently?" })
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(&c.text);
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
                ui.label("Parallel disk-usage scanner, directory tree, extension statistics\nand treemap. Written in Rust.\nInspired by WinDirStat and KDirStat. GPL-2.0-or-later.");
            });
            self.about = open;
        }
    }
}

fn fmt_dur(sec: f64) -> String {
    let s = sec.max(0.0) as u64;
    if s < 3600 {
        format!("{}:{:02}", s / 60, s % 60)
    } else {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    }
}
