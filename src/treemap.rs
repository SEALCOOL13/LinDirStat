//! Squarified treemap: layout (with "hide from treemap" support) and a fast CPU painter.
//! Tiles are flat-colored with soft edge shading (no specular highlight) and folder outlines.
use crate::scan::{NodeId, Tree, EXT_AGG};
use rustc_hash::{FxHashMap, FxHashSet};

const MIN_DIR: f64 = 36.0; // px^2 below which a folder is not subdivided
const MIN_TILE: f64 = 2.0; // px^2 below which items are merged into the "tail" tile
const MIN_BEVEL: usize = 100; // px^2 below which tiles are a single flat color

pub struct Leaf {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub node: NodeId,
    pub ext: u16,
}

pub struct Layout {
    pub w: usize,
    pub h: usize,
    pub leaves: Vec<Leaf>,
    pub dirs: Vec<[u32; 4]>,
    pub rects: FxHashMap<NodeId, [u32; 4]>,
    pub ids: Vec<u32>, // leaf index + 1 per pixel, 0 = nothing
}

impl Layout {
    pub fn node_at(&self, x: usize, y: usize) -> Option<NodeId> {
        if x >= self.w || y >= self.h {
            return None;
        }
        let i = self.ids[y * self.w + x];
        if i == 0 {
            return None;
        }
        let n = self.leaves[(i - 1) as usize].node;
        Some(if n.is_agg() { NodeId::dir(n.dir_id()) } else { n })
    }
}

#[inline]
fn rnd(v: f64) -> u32 {
    (v + 0.5).max(0.0) as u32
}

pub fn squarify(items: &[(NodeId, f64)], mut x: f64, mut y: f64, mut w: f64, mut h: f64) -> Vec<(NodeId, f64, f64, f64, f64)> {
    let mut out = Vec::with_capacity(items.len());
    let (mut i, n) = (0usize, items.len());
    while i < n {
        let short = w.min(h);
        if short <= 0.0 {
            break;
        }
        let (mut s, first, mut best, mut j) = (0.0f64, items[i].1, f64::INFINITY, i);
        while j < n {
            let a = items[j].1;
            let s2 = s + a;
            let worst = (short * short * first / (s2 * s2)).max(s2 * s2 / (short * short * a));
            if j > i && worst > best {
                break;
            }
            best = worst;
            s = s2;
            j += 1;
        }
        let row = &items[i..j];
        i = j;
        if w >= h {
            let rw = s / h;
            let mut cy = y;
            for &(node, a) in row {
                let ch = a / rw;
                out.push((node, x, cy, rw, ch));
                cy += ch;
            }
            x += rw;
            w -= rw;
        } else {
            let rh = s / w;
            let mut cx = x;
            for &(node, a) in row {
                let cw = a / rh;
                out.push((node, cx, y, cw, rh));
                cx += cw;
            }
            y += rh;
            h -= rh;
        }
    }
    out
}

struct Ctx<'a> {
    tree: &'a Tree,
    hidden: &'a FxHashSet<NodeId>,
    cut: &'a FxHashMap<u32, u64>,
    lay: Layout,
}

pub fn compute(tree: &Tree, hidden: &FxHashSet<NodeId>, cut: &FxHashMap<u32, u64>, root: u32, w: usize, h: usize) -> Layout {
    let mut c = Ctx {
        tree,
        hidden,
        cut,
        lay: Layout { w, h, leaves: Vec::new(), dirs: Vec::new(), rects: FxHashMap::default(), ids: vec![0; w * h] },
    };
    c.dir(root, 0.0, 0.0, w as f64, h as f64);
    c.lay
}

impl Ctx<'_> {
    fn dir(&mut self, d: u32, x: f64, y: f64, w: f64, h: f64) {
        let r = [rnd(x), rnd(y), rnd(x + w), rnd(y + h)];
        self.lay.rects.insert(NodeId::dir(d), r);
        self.lay.dirs.push(r);
        let dir = &self.tree.dirs[d as usize];
        let cut_d = self.cut.get(&d).copied().unwrap_or(0);
        let total = dir.size.saturating_sub(dir.inode_size as u64).saturating_sub(cut_d);
        if total == 0 {
            return;
        }
        let area = w * h;
        let totf = total as f64;
        let mut items: Vec<(NodeId, f64)> = Vec::new();
        let mut taken = 0u64;
        if cut_d == 0 {
            // fast path: children are already sorted by size, walk both lists and stop at the first tiny one
            let (subs, files) = (&dir.subdirs, &dir.files);
            let (mut i, mut j) = (0usize, 0usize);
            loop {
                let ds = subs.get(i).map(|&c| self.tree.dirs[c as usize].size).unwrap_or(0);
                let fs = files.get(j).map(|f| f.size).unwrap_or(0);
                let (node, size) = if ds >= fs {
                    if ds == 0 {
                        break;
                    }
                    i += 1;
                    (NodeId::dir(subs[i - 1]), ds)
                } else {
                    j += 1;
                    (NodeId::file(d, (j - 1) as u32), fs)
                };
                let a = size as f64 / totf * area;
                if a < MIN_TILE {
                    break;
                }
                taken += size;
                items.push((node, a));
            }
        } else {
            // some descendants are hidden: effective sizes differ, so collect and sort
            let mut all: Vec<(NodeId, u64)> = Vec::new();
            for &c in &dir.subdirs {
                let n = NodeId::dir(c);
                if self.hidden.contains(&n) {
                    continue;
                }
                let s = self.tree.dirs[c as usize].size.saturating_sub(self.cut.get(&c).copied().unwrap_or(0));
                if s > 0 {
                    all.push((n, s));
                }
            }
            for (j, f) in dir.files.iter().enumerate() {
                let n = NodeId::file(d, j as u32);
                if f.size > 0 && !self.hidden.contains(&n) {
                    all.push((n, f.size));
                }
            }
            all.sort_unstable_by(|a, b| b.1.cmp(&a.1));
            for (n, s) in all {
                let a = s as f64 / totf * area;
                if a < MIN_TILE {
                    break;
                }
                taken += s;
                items.push((n, a));
            }
        }
        if total > taken {
            let a = (total - taken) as f64 / totf * area;
            let pos = items.iter().position(|it| it.1 < a).unwrap_or(items.len());
            items.insert(pos, (NodeId::agg(d), a));
        }
        for (node, cx, cy, cw, ch) in squarify(&items, x, y, w, h) {
            if node.is_dir() && cw * ch >= MIN_DIR {
                let cd = &self.tree.dirs[node.0 as usize];
                if cd.size > cd.inode_size as u64 && (!cd.subdirs.is_empty() || !cd.files.is_empty()) {
                    self.dir(node.0 as u32, cx, cy, cw, ch);
                    continue;
                }
            }
            self.leaf(node, cx, cy, cw, ch);
        }
    }

    fn leaf(&mut self, node: NodeId, x: f64, y: f64, w: f64, h: f64) {
        let (x0, y0, x1, y1) = (rnd(x), rnd(y), rnd(x + w).min(self.lay.w as u32), rnd(y + h).min(self.lay.h as u32));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let ext = self.tree.ext(node);
        self.lay.rects.insert(node, [x0, y0, x1, y1]);
        self.lay.leaves.push(Leaf { x0, y0, x1, y1, node, ext });
        let idx = self.lay.leaves.len() as u32;
        let wd = self.lay.w;
        for yy in y0..y1 {
            self.lay.ids[yy as usize * wd + x0 as usize..yy as usize * wd + x1 as usize].fill(idx);
        }
    }
}

// ------------------------------------------------------------------------------------------
// painting
// ------------------------------------------------------------------------------------------
#[derive(Clone, Copy)]
pub struct Look {
    pub brightness: f32,
    pub saturation: f32,
    pub edge: f32,
}

#[inline]
fn pack(c: [f32; 3]) -> u32 {
    u32::from_le_bytes([c[0].clamp(0.0, 255.0) as u8, c[1].clamp(0.0, 255.0) as u8, c[2].clamp(0.0, 255.0) as u8, 255])
}

pub fn adjust(c: [u8; 3], look: &Look, dim: f32) -> [f32; 3] {
    let (r, g, b) = (c[0] as f32, c[1] as f32, c[2] as f32);
    let gray = 0.299 * r + 0.587 * g + 0.114 * b;
    let k = look.brightness * dim;
    [(gray + (r - gray) * look.saturation) * k, (gray + (g - gray) * look.saturation) * k, (gray + (b - gray) * look.saturation) * k]
}

fn hline(buf: &mut [u32], w: usize, y: usize, x0: usize, x1: usize, c: u32) {
    buf[y * w + x0..y * w + x1].fill(c);
}
fn vline(buf: &mut [u32], w: usize, x: usize, y0: usize, y1: usize, c: u32) {
    for y in y0..y1 {
        buf[y * w + x] = c;
    }
}

/// Returns packed RGBA pixels (little-endian u32 = [r,g,b,a]).
pub fn paint(layout: &Layout, ext_colors: &[[u8; 3]], other: [u8; 3], bg: [u8; 3], look: &Look, highlight: Option<u16>) -> Vec<u32> {
    let (w, h) = (layout.w, layout.h);
    let mut buf = vec![pack([bg[0] as f32, bg[1] as f32, bg[2] as f32]); w * h];
    let n = ext_colors.len();
    let norm: Vec<[f32; 3]> = ext_colors.iter().copied().chain(std::iter::once(other)).map(|c| adjust(c, look, 1.0)).collect();
    let dim: Vec<[f32; 3]> = ext_colors.iter().copied().chain(std::iter::once(other)).map(|c| adjust(c, look, 0.25)).collect();
    let e = look.edge;
    for l in &layout.leaves {
        let ci = if (l.ext as usize) < n && l.ext != EXT_AGG { l.ext as usize } else { n };
        let col = if highlight.is_some_and(|hl| hl != l.ext) { dim[ci] } else { norm[ci] };
        let (x0, y0, x1, y1) = (l.x0 as usize, l.y0 as usize, l.x1 as usize, l.y1 as usize);
        let (bw, bh) = (x1 - x0, y1 - y0);
        if bw * bh < MIN_BEVEL || e <= 0.01 {
            let f = if bw * bh < MIN_BEVEL { 1.0 - 0.3 * e } else { 1.0 };
            let c = pack([col[0] * f, col[1] * f, col[2] * f]);
            for y in y0..y1 {
                hline(&mut buf, w, y, x0, x1, c);
            }
            continue;
        }
        let c = pack(col);
        for y in y0..y1 {
            hline(&mut buf, w, y, x0, x1, c);
        }
        let rings = (bw.min(bh) / 6).clamp(1, 6);
        for k in 0..rings {
            let f = 1.0 - 0.7 * e * (1.0 - k as f32 / rings as f32).powf(1.5);
            let rc = pack([col[0] * f, col[1] * f, col[2] * f]);
            let (xa, xb, ya, yb) = (x0 + k, x1 - k, y0 + k, y1 - k);
            if xb <= xa || yb <= ya {
                break;
            }
            hline(&mut buf, w, ya, xa, xb, rc);
            if yb - 1 > ya {
                hline(&mut buf, w, yb - 1, xa, xb, rc);
            }
            if yb - ya > 2 {
                vline(&mut buf, w, xa, ya + 1, yb - 1, rc);
                if xb - 1 > xa {
                    vline(&mut buf, w, xb - 1, ya + 1, yb - 1, rc);
                }
            }
        }
    }
    if e > 0.01 {
        let f = 1.0 - 0.55 * e;
        let darken = |p: &mut u32| {
            let b = p.to_le_bytes();
            *p = u32::from_le_bytes([(b[0] as f32 * f) as u8, (b[1] as f32 * f) as u8, (b[2] as f32 * f) as u8, 255]);
        };
        for r in &layout.dirs {
            let (x0, y0, x1, y1) = (r[0] as usize, r[1] as usize, (r[2] as usize).min(w), (r[3] as usize).min(h));
            if x1 < x0 + 4 || y1 < y0 + 4 {
                continue;
            }
            for x in x0..x1 {
                darken(&mut buf[y0 * w + x]);
                darken(&mut buf[(y1 - 1) * w + x]);
            }
            for y in y0..y1 {
                darken(&mut buf[y * w + x0]);
                darken(&mut buf[y * w + x1 - 1]);
            }
        }
    }
    buf
}
