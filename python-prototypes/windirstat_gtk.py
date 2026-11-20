#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""
WinDirStat for Linux (GTK3)
===========================
Directory tree + extension statistics + treemap, with dark-theme flavors,
live scan progress, adjustable treemap colors and filterable "largest
folders / files" lists.

Requirements (Ubuntu):
    sudo apt install python3-gi python3-gi-cairo gir1.2-gtk-3.0 python3-numpy
Run:
    python3 windirstat_gtk.py [folder]
"""
import heapq
import json
import math
import os
import re
import shutil
import stat
import subprocess
import sys
import threading
import time

try:
    import gi
    gi.require_version("Gtk", "3.0")
    gi.require_version("Gdk", "3.0")
    from gi.repository import Gdk, GLib, GObject, Gtk, Pango
    import cairo
    import numpy as np
except (ImportError, ValueError) as exc:  # pragma: no cover
    sys.exit(f"Missing dependency: {exc}\nInstall with:\n  sudo apt install python3-gi "
             "python3-gi-cairo gir1.2-gtk-3.0 python3-numpy")

APP_NAME = "WinDirStat for Linux"
MAX_ROWS = 5000        # rows shown per folder in the tree view
HIDDEN_SUFFIX = "   (hidden from treemap)"
LIST_LIMIT = 300       # rows in the "largest folders/files" lists
TOP_FILES_KEPT = 3000  # largest files remembered during the scan

CONFIG = os.path.join(os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config")),
                      "windirstat-linux", "settings.json")
DEFAULTS = dict(theme="Catppuccin Mocha", brightness=100, saturation=100, edge=50,
                classic_colors=False, one_fs=True, apparent=False, precount=True,
                folder_op="ge", folder_n=100, folder_recursive=True, scope_selected=False)


def load_settings():
    cfg = dict(DEFAULTS)
    try:
        with open(CONFIG) as f:
            cfg.update({k: v for k, v in json.load(f).items() if k in DEFAULTS})
    except (OSError, ValueError):
        pass
    return cfg


def save_settings(cfg):
    try:
        os.makedirs(os.path.dirname(CONFIG), exist_ok=True)
        with open(CONFIG, "w") as f:
            json.dump(cfg, f, indent=2)
    except OSError:
        pass


# --------------------------------------------------------------------------- #
# helpers
# --------------------------------------------------------------------------- #
def human(n):
    n = float(n)
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if n < 1024 or unit == "TB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024


def fmt_time(ts):
    try:
        return time.strftime("%Y-%m-%d %H:%M", time.localtime(ts))
    except (OverflowError, OSError, ValueError):
        return ""


def fmt_dur(sec):
    sec = int(max(0, sec))
    return f"{sec // 60}:{sec % 60:02d}" if sec < 3600 else f"{sec // 3600}:{sec % 3600 // 60:02d}:{sec % 60:02d}"


def ext_of(name):
    e = os.path.splitext(name)[1].lower()
    return sys.intern(e if e else "(none)")


def open_path(path):
    try:
        subprocess.Popen(["xdg-open", path], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except OSError:
        pass


def hex2rgb(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def rgb2hex(c):
    return "#%02x%02x%02x" % tuple(int(v) for v in c)


# --------------------------------------------------------------------------- #
# data model + scanner
# --------------------------------------------------------------------------- #
class Node:
    __slots__ = ("name", "parent", "is_dir", "size", "files", "dirs", "dfiles",
                 "mtime", "children", "ext", "agg")

    def __init__(self, name, parent, is_dir, size=0, mtime=0.0, ext=None):
        self.name, self.parent, self.is_dir = name, parent, is_dir
        self.size, self.mtime, self.ext = size, mtime, ext
        self.files = self.dirs = self.dfiles = 0     # recursive files / subdirs, direct files
        self.children = [] if is_dir else None
        self.agg = False

    def path(self):
        parts, n = [], self
        while n.parent is not None:
            parts.append(n.name)
            n = n.parent
        return os.path.join(n.name, *reversed(parts))


class Scanner(threading.Thread):
    """Walks the tree in a worker thread; the UI polls the public counters."""

    def __init__(self, path, one_fs, apparent, precount):
        super().__init__(daemon=True)
        self.path, self.one_fs, self.apparent, self.want_precount = path, one_fs, apparent, precount
        self.phase, self.entries, self.nbytes, self.errors = "starting", 0, 0, 0
        self.total, self.current = 0, path
        self.done = self.cancel = False
        self.root, self.ext_stats, self.dirs_all, self.top_files = None, {}, [], []
        self._heap, self._seq, self._seen = [], 0, set()

    def _size(self, st):
        return st.st_size if self.apparent else st.st_blocks * 512

    def run(self):
        try:
            self._run()
        finally:
            self.done = True

    def _run(self):
        try:
            st = os.lstat(self.path)
        except OSError:
            return
        self.dev = st.st_dev
        if self.one_fs and os.path.ismount(self.path):     # whole filesystem: use inode count
            try:
                v = os.statvfs(self.path)
                self.total = max(0, v.f_files - v.f_ffree)
            except OSError:
                pass
        if not self.total and self.want_precount:
            self.phase = "counting"
            self.total = self._precount()
            if self.cancel:
                return
        self.phase = "scanning"
        root = Node(self.path, None, True, self._size(st), st.st_mtime)
        self._walk(root, self.path)
        self.top_files = [n for _, _, n in sorted(self._heap, reverse=True)]
        self.root = root

    def _precount(self):
        n, stack = 0, [self.path]
        while stack and not self.cancel:
            try:
                with os.scandir(stack.pop()) as it:
                    for e in it:
                        try:
                            if e.is_dir(follow_symlinks=False):
                                if self.one_fs and e.stat(follow_symlinks=False).st_dev != self.dev:
                                    continue
                                stack.append(e.path)
                        except OSError:
                            pass
                        n += 1
            except OSError:
                pass
            self.entries = n          # shown as "counting…" progress
        self.entries = 0
        return n

    def _walk(self, node, path):
        try:
            it = os.scandir(path)
        except OSError:
            self.errors += 1
            return
        children = node.children
        with it:
            while not self.cancel:
                try:
                    e = next(it)
                except StopIteration:
                    break
                except OSError:
                    self.errors += 1
                    break
                try:
                    st = e.stat(follow_symlinks=False)
                except OSError:
                    self.errors += 1
                    continue
                self.entries += 1
                if not self.entries & 255:
                    self.current = e.path
                mode = st.st_mode
                if stat.S_ISDIR(mode):
                    if self.one_fs and st.st_dev != self.dev:
                        continue
                    d = Node(e.name, node, True, self._size(st), st.st_mtime)
                    self._walk(d, e.path)
                    children.append(d)
                    node.size += d.size
                    node.files += d.files
                    node.dirs += d.dirs + 1
                    if d.mtime > node.mtime:
                        node.mtime = d.mtime
                else:
                    size = self._size(st)
                    if st.st_nlink > 1 and not stat.S_ISLNK(mode):
                        key = (st.st_dev, st.st_ino)            # count hardlinks once
                        if key in self._seen:
                            size = 0
                        else:
                            self._seen.add(key)
                    f = Node(e.name, node, False, size, st.st_mtime, ext_of(e.name))
                    children.append(f)
                    node.size += size
                    node.files += 1
                    node.dfiles += 1
                    if st.st_mtime > node.mtime:
                        node.mtime = st.st_mtime
                    s = self.ext_stats.get(f.ext)
                    if s is None:
                        self.ext_stats[f.ext] = [size, 1]
                    else:
                        s[0] += size
                        s[1] += 1
                    self.nbytes += size
                    h = self._heap
                    if len(h) < TOP_FILES_KEPT:
                        self._seq += 1
                        heapq.heappush(h, (size, self._seq, f))
                    elif size > h[0][0]:
                        self._seq += 1
                        heapq.heapreplace(h, (size, self._seq, f))
        children.sort(key=lambda c: c.size, reverse=True)
        if children:
            node.ext = children[0].ext
        self.dirs_all.append(node)


# --------------------------------------------------------------------------- #
# themes (UI flavors + matching treemap palettes)
# --------------------------------------------------------------------------- #
def _pal(s):
    return [hex2rgb(x) for x in s.split()]


CLASSIC_BASE = [(0, 0, 255), (255, 0, 0), (0, 255, 0), (0, 255, 255), (255, 0, 255), (255, 255, 0),
                (150, 150, 255), (255, 150, 150), (150, 255, 150), (150, 255, 255),
                (255, 150, 255), (255, 255, 150)]


def _normalize(color, brightness=0.6):
    r = [c / 255.0 for c in color]
    total = sum(r) or 1.0
    r = [c * 3 * brightness / total for c in r]
    for _ in range(3):
        over = sum(max(0.0, c - 1) for c in r)
        if over <= 0:
            break
        r = [min(c, 1.0) for c in r]
        free = [i for i, c in enumerate(r) if c < 1.0]
        if not free:
            break
        for i in free:
            r[i] += over / len(free)
    return tuple(int(min(1.0, c * 1.15) * 255) for c in r)


CLASSIC = [_normalize(c) for c in CLASSIC_BASE]


def _theme(dark, bg, bg2, bg3, fg, dim, border, accent, selbg, selfg, other, palette=None):
    return dict(dark=dark, custom=True, bg=bg, bg2=bg2, bg3=bg3, fg=fg, dim=dim, border=border,
                accent=accent, selbg=selbg, selfg=selfg, other=hex2rgb(other),
                tbg=hex2rgb(bg2), palette=_pal(palette) if palette else None)


THEMES = {
    "System default": dict(dark=None, custom=False, tbg=(32, 32, 32), other=None, palette=None),
    "Light": dict(dark=False, custom=False, tbg=(235, 235, 235), other=None, palette=None),
    "Adwaita Dark": dict(dark=True, custom=False, tbg=(30, 30, 30), other=None, palette=None),
    "Catppuccin Mocha": _theme(True, "#1e1e2e", "#181825", "#313244", "#cdd6f4", "#a6adc8", "#45475a",
                               "#89b4fa", "#45475a", "#cdd6f4", "#7f849c",
                               "89b4fa f38ba8 a6e3a1 f9e2af cba6f7 94e2d5 fab387 f5c2e7 74c7ec b4befe eba0ac 89dceb"),
    "Catppuccin Macchiato": _theme(True, "#24273a", "#1e2030", "#363a4f", "#cad3f5", "#a5adcb", "#494d64",
                                   "#8aadf4", "#494d64", "#cad3f5", "#8087a2",
                                   "8aadf4 ed8796 a6da95 eed49f c6a0f6 8bd5ca f5a97f f5bde6 7dc4e4 b7bdf8 ee99a0 91d7e3"),
    "Catppuccin Frappé": _theme(True, "#303446", "#292c3c", "#414559", "#c6d0f5", "#a5adce", "#51576d",
                                "#8caaee", "#51576d", "#c6d0f5", "#838ba7",
                                "8caaee e78284 a6d189 e5c890 ca9ee6 81c8be ef9f76 f4b8e4 85c1dc babbf1 ea999c 99d1db"),
    "Dracula": _theme(True, "#282a36", "#21222c", "#44475a", "#f8f8f2", "#bdc0d0", "#44475a",
                      "#bd93f9", "#44475a", "#f8f8f2", "#6272a4",
                      "bd93f9 ff5555 50fa7b f1fa8c 8be9fd ffb86c ff79c6 d6acff ff6e6e 69ff94 a4ffff ffffa5"),
    "Nord": _theme(True, "#2e3440", "#272c36", "#3b4252", "#d8dee9", "#a3acbd", "#434c5e",
                   "#88c0d0", "#434c5e", "#eceff4", "#616e88",
                   "88c0d0 bf616a a3be8c ebcb8b b48ead 8fbcbb d08770 81a1c1 5e81ac d98891 c0d4ae f3deb0"),
    "Gruvbox Dark": _theme(True, "#282828", "#1d2021", "#3c3836", "#ebdbb2", "#a89984", "#504945",
                           "#fabd2f", "#504945", "#ebdbb2", "#928374",
                           "83a598 fb4934 b8bb26 fabd2f d3869b 8ec07c fe8019 458588 cc241d 98971a d79921 b16286"),
    "Tokyo Night": _theme(True, "#1a1b26", "#16161e", "#292e42", "#c0caf5", "#a9b1d6", "#3b4261",
                          "#7aa2f7", "#33467c", "#c0caf5", "#565f89",
                          "7aa2f7 f7768e 9ece6a e0af68 bb9af7 73daca ff9e64 7dcfff 2ac3de b4f9f8 3d59a1 db4b4b"),
    "One Dark": _theme(True, "#282c34", "#21252b", "#2c313c", "#abb2bf", "#8b93a1", "#3e4451",
                       "#61afef", "#3e4451", "#d7dae0", "#5c6370",
                       "61afef e06c75 98c379 e5c07b c678dd 56b6c2 d19a66 be5046 4d78cc 89ca78 e8d09a a9b1d6"),
    "Solarized Dark": _theme(True, "#002b36", "#00212b", "#073642", "#93a1a1", "#839496", "#0f4a59",
                             "#268bd2", "#0f4a59", "#eee8d5", "#586e75",
                             "268bd2 dc322f 859900 b58900 6c71c4 2aa198 cb4b16 d33682 5fb0e8 e8645f a4b82a 5ec4bb"),
}

CSS_TEMPLATE = """
window, .background, dialog, messagedialog { background-color: {{bg}}; color: {{fg}}; }
label, checkbutton, radiobutton { color: {{fg}}; }
.dim-label { color: {{dim}}; }
headerbar, .titlebar { background-image: none; background-color: {{bg2}}; color: {{fg}};
                       border-bottom: 1px solid {{border}}; box-shadow: none; }
menubar { background-image: none; background-color: {{bg2}}; color: {{fg}}; border-bottom: 1px solid {{border}}; }
menubar > menuitem, menu menuitem, menuitem label, menuitem accelerator { color: {{fg}}; }
menubar > menuitem:hover, menu menuitem:hover { background-color: {{selbg}}; color: {{selfg}}; }
menu, .menu, .popup, popover, popover.background { background-image: none; background-color: {{bg2}};
                                                    color: {{fg}}; border: 1px solid {{border}}; }
separator { background-color: {{border}}; min-height: 1px; min-width: 1px; }
paned > separator { background-color: {{border}}; background-image: none; }
.view, treeview.view, iconview, textview text { background-color: {{bg}}; color: {{fg}}; }
treeview.view:selected, treeview.view:selected:focus, .view:selected { background-color: {{selbg}}; color: {{selfg}}; }
treeview.view header button { background-image: none; background-color: {{bg2}}; color: {{fg}}; border-radius: 0;
                              border-style: solid; border-color: {{border}}; border-width: 0 1px 1px 0;
                              box-shadow: none; text-shadow: none; padding: 3px 6px; }
treeview.view header button:hover { background-color: {{bg3}}; }
treeview.view.progressbar, treeview.view.progressbar:selected, treeview.view.progressbar:selected:focus,
.progressbar, .progressbar:selected { background-image: none; background-color: {{accent}}; color: {{bg}};
                                      border-color: {{accent}}; box-shadow: none; }
treeview.view.trough, treeview.view.trough:selected, treeview.view.trough:selected:focus {
    background-image: none; background-color: {{bg3}}; color: {{fg}}; border-color: {{border}}; box-shadow: none; }
scrolledwindow { border-color: {{border}}; }
scrollbar { background-color: {{bg}}; }
scrollbar slider { background-color: {{border}}; border-radius: 6px; min-width: 8px; min-height: 8px;
                   border: 2px solid transparent; }
scrollbar slider:hover { background-color: {{dim}}; }
button { background-image: none; background-color: {{bg3}}; color: {{fg}}; border: 1px solid {{border}};
         box-shadow: none; text-shadow: none; }
button:hover { background-color: {{selbg}}; color: {{selfg}}; }
button:disabled { color: {{dim}}; }
button.suggested-action { background-color: {{accent}}; color: {{bg}}; }
entry, spinbutton, spinbutton button, combobox button, combobox entry {
    background-image: none; background-color: {{bg2}}; color: {{fg}}; border: 1px solid {{border}};
    box-shadow: none; }
scale trough { background-color: {{bg3}}; border: none; min-height: 6px; min-width: 6px; border-radius: 3px; }
scale highlight { background-color: {{accent}}; border: none; border-radius: 3px; }
scale slider { background-image: none; background-color: {{fg}}; border: 2px solid {{accent}};
               min-width: 14px; min-height: 14px; border-radius: 50%; box-shadow: none; }
scale marks, scale mark label, scale value { color: {{dim}}; }
progressbar trough { background-color: {{bg3}}; border: 1px solid {{border}}; min-height: 18px; border-radius: 3px; }
progressbar progress { background-image: none; background-color: {{accent}}; border: none; border-radius: 3px; }
progressbar.horizontal trough, progressbar.horizontal progress { min-height: 22px; }
notebook > header { background-color: {{bg2}}; border-color: {{border}}; }
notebook > header > tabs > tab { background-color: {{bg2}}; color: {{dim}}; border: none; padding: 4px 12px; }
notebook > header > tabs > tab:checked { background-color: {{bg}}; color: {{fg}}; box-shadow: inset 0 -2px {{accent}}; }
notebook, notebook > stack { background-color: {{bg}}; border-color: {{border}}; }
notebook > header.top { border-bottom: 1px solid {{border}}; }
check, radio { background-image: none; background-color: {{bg3}}; border: 1px solid {{border}}; color: {{bg}}; }
check:checked, radio:checked, menu check:checked, menu radio:checked { background-color: {{accent}}; color: {{bg}}; }
tooltip, tooltip.background { background-color: {{bg3}}; color: {{fg}}; border: 1px solid {{border}}; }
.statusbox { background-color: {{bg2}}; border-top: 1px solid {{border}}; }
.treestrip { background-color: {{bg2}}; border-top: 1px solid {{border}}; }
"""


def build_css(theme):
    return re.sub(r"\{\{(\w+)\}\}", lambda m: theme[m.group(1)], CSS_TEMPLATE)


# --------------------------------------------------------------------------- #
# squarified treemap layout
# --------------------------------------------------------------------------- #
def squarify(items, x, y, w, h):
    out, i, n = [], 0, len(items)
    while i < n:
        short = min(w, h)
        if short <= 0:
            break
        s, first, best, j = 0.0, items[i][1], float("inf"), i
        while j < n:
            a = items[j][1]
            s2 = s + a
            worst = max(short * short * first / (s2 * s2), s2 * s2 / (short * short * a))
            if j > i and worst > best:
                break
            best, s, j = worst, s2, j + 1
        row, i = items[i:j], j
        if w >= h:
            rw, cy = s / h, y
            for obj, a in row:
                ch = a / rw
                out.append((obj, x, cy, rw, ch))
                cy += ch
            x += rw
            w -= rw
        else:
            rh, cx = s / w, x
            for obj, a in row:
                cw = a / rh
                out.append((obj, cx, y, cw, rh))
                cx += cw
            y += rh
            h -= rh
    return out


class TreeMap(Gtk.DrawingArea):
    """Flat-colored treemap: no specular light, soft edge shading + folder outlines."""
    MIN_BEVEL = 100     # px^2 below which a tile is drawn as a single flat color
    MIN_DIR = 36        # px^2 below which a folder is not subdivided

    def __init__(self, on_click, on_right, on_hover):
        super().__init__()
        self.cb_click, self.cb_right, self.cb_hover = on_click, on_right, on_hover
        self.set_size_request(200, 150)
        self.add_events(Gdk.EventMask.BUTTON_PRESS_MASK | Gdk.EventMask.POINTER_MOTION_MASK
                        | Gdk.EventMask.LEAVE_NOTIFY_MASK)
        self.connect("draw", self._on_draw)
        self.connect("size-allocate", self._on_size)
        self.connect("button-press-event", self._on_press)
        self.connect("motion-notify-event", lambda w, ev: self.cb_hover(self._node_at(ev.x, ev.y)) or False)
        self.connect("leave-notify-event", lambda w, ev: self.cb_hover(None) or False)
        self.root, self.colors, self.selected, self.highlight = None, {}, None, None
        self.brightness, self.saturation, self.edge = 1.0, 1.0, 0.5
        self.bg, self.other = (30, 30, 30), (140, 140, 140)
        self._surface = self._ids = self._arr = None
        self._leafs, self._dirs, self._rects, self._leaf_nodes = [], [], {}, [None]
        self._scale, self._lw, self._lh, self._W, self._H = 1, 1, 1, 1, 1
        self._stale, self._timer = False, None
        self.hidden, self._cut = set(), {}     # nodes hidden by the user; hidden bytes per ancestor

    # ---- public ----
    def set_data(self, root, colors):
        self.root, self.colors, self.highlight = root, colors, None
        self.relayout()

    def set_colors(self, colors):
        self.colors = colors
        self.repaint()

    def set_look(self, brightness, saturation, edge):
        self.brightness, self.saturation, self.edge = brightness, saturation, edge
        self.repaint()

    def set_theme(self, bg, other):
        self.bg, self.other = bg, other or (140, 140, 140)
        self.repaint()

    def set_selected(self, node):
        self.selected = node
        self.queue_draw()

    def set_highlight(self, ext):
        self.highlight = ext
        self.repaint()

    def display_rgb(self, ext, dim=1.0):
        r, g, b = self.colors.get(ext, self.other) if ext is not None else self.other
        gray = 0.299 * r + 0.587 * g + 0.114 * b
        k = self.brightness * dim
        return tuple(max(0, min(255, int((gray + (v - gray) * self.saturation) * k))) for v in (r, g, b))

    # ---- layout ----
    def relayout(self):
        a = self.get_allocation()
        self._scale = max(1, self.get_scale_factor())
        self._lw, self._lh = max(1, a.width), max(1, a.height)
        self._W, self._H = self._lw * self._scale, self._lh * self._scale
        self._stale = False
        self._leafs, self._dirs, self._rects, self._leaf_nodes = [], [], {}, [None]
        self._ids = self._surface = self._arr = None
        self._cut = {}
        for h in self.hidden:                      # only top-most hidden nodes count
            if self._is_hidden(h.parent):
                continue
            a = h.parent
            while a is not None:
                self._cut[a] = self._cut.get(a, 0) + h.size
                a = a.parent
        if self.root is None or self.root.size <= 0 or self._W < 8 or self._H < 8:
            self.queue_draw()
            return
        self._ids = np.zeros((self._H, self._W), np.int32)
        self._layout(self.root, 0.0, 0.0, float(self._W), float(self._H), 0)
        self.repaint()

    def _layout(self, node, x, y, w, h, level):
        r = (int(x + 0.5), int(y + 0.5), int(x + w + 0.5), int(y + h + 0.5))
        self._rects[node] = r
        self._dirs.append(r)
        if self.hidden:        # visible size = size minus hidden descendants; siblings refill the space
            cutd = self._cut
            kids = [(c, c.size - cutd.get(c, 0)) for c in node.children if c not in self.hidden]
            kids = [t for t in kids if t[1] > 0]
            if node in cutd:
                kids.sort(key=lambda t: -t[1])
        else:
            kids = [(c, c.size) for c in node.children if c.size > 0]
        if not kids:
            return
        total, area = float(sum(sz for _, sz in kids)), w * h
        items, cut = [], len(kids)
        for i, (c, sz) in enumerate(kids):
            a = sz / total * area
            if a < 2.0:
                cut = i
                break
            items.append((c, a))
        if cut < len(kids):
            small = sum(sz for _, sz in kids[cut:])
            agg = Node(f"({len(kids) - cut:,} small items)", node, False, small)
            agg.agg = True
            items.append((agg, small / total * area))
            items.sort(key=lambda t: -t[1])
        for c, cx, cy, cw, ch in squarify(items, x, y, w, h):
            if c.is_dir and c.children and cw * ch >= self.MIN_DIR:
                self._layout(c, cx, cy, cw, ch, level + 1)
            else:
                x0, y0, x1, y1 = int(cx + 0.5), int(cy + 0.5), int(cx + cw + 0.5), int(cy + ch + 0.5)
                if x1 > x0 and y1 > y0:
                    self._rects[c] = (x0, y0, x1, y1)
                    self._leafs.append((x0, y0, x1, y1, c))
                    self._leaf_nodes.append(c)
                    self._ids[y0:y1, x0:x1] = len(self._leaf_nodes) - 1

    def _is_hidden(self, node):
        while node is not None:
            if node in self.hidden:
                return True
            node = node.parent
        return False

    # ---- painting ----
    def repaint(self):
        if self._ids is None:
            self._surface = self._arr = None
            self.queue_draw()
            return
        W, H, e = self._W, self._H, self.edge
        out = np.empty((H, W, 4), np.uint8)
        out[..., 0], out[..., 1], out[..., 2], out[..., 3] = self.bg[2], self.bg[1], self.bg[0], 255
        cache, hl = {}, self.highlight
        for x0, y0, x1, y1, node in self._leafs:
            ext = node.ext
            ck = ext
            col = cache.get(ck)
            if col is None:
                col = cache[ck] = self.display_rgb(ext, 0.25 if (hl is not None and ext != hl) else 1.0)
            bw, bh = x1 - x0, y1 - y0
            if bw * bh < self.MIN_BEVEL or e <= 0.01:
                f = 1.0 - 0.3 * e if bw * bh < self.MIN_BEVEL else 1.0
                out[y0:y1, x0:x1, :3] = (int(col[2] * f), int(col[1] * f), int(col[0] * f))
                continue
            out[y0:y1, x0:x1, :3] = (col[2], col[1], col[0])
            rings = max(1, min(6, min(bw, bh) // 6))
            for k in range(rings):
                f = 1.0 - 0.7 * e * (1.0 - k / rings) ** 1.5
                c = (int(col[2] * f), int(col[1] * f), int(col[0] * f))
                xa, xb, ya, yb = x0 + k, x1 - k, y0 + k, y1 - k
                if xb - xa <= 0 or yb - ya <= 0:
                    break
                out[ya, xa:xb, :3] = c
                if yb - 1 > ya:
                    out[yb - 1, xa:xb, :3] = c
                if yb - ya > 2:
                    out[ya + 1:yb - 1, xa, :3] = c
                    if xb - 1 > xa:
                        out[ya + 1:yb - 1, xb - 1, :3] = c
        if e > 0.01:                                   # folder outlines
            f = 1.0 - 0.55 * e
            for x0, y0, x1, y1 in self._dirs:
                if x1 - x0 < 4 or y1 - y0 < 4:
                    continue
                for sl in (out[y0, x0:x1], out[y1 - 1, x0:x1], out[y0:y1, x0], out[y0:y1, x1 - 1]):
                    sl[:, :3] = (sl[:, :3] * f).astype(np.uint8)
        self._arr = out
        self._surface = cairo.ImageSurface.create_for_data(out, cairo.FORMAT_ARGB32, W, H, W * 4)
        self._surface.set_device_scale(self._scale, self._scale)
        self.queue_draw()

    def _on_draw(self, _w, cr):
        cr.set_source_rgb(*(c / 255.0 for c in self.bg))
        cr.paint()
        if self._surface is None:
            return True
        cr.save()
        if self._stale:
            a = self.get_allocation()
            cr.scale(a.width / self._lw, a.height / self._lh)
        cr.set_source_surface(self._surface, 0, 0)
        cr.paint()
        cr.restore()
        if self.selected is not None and not self._stale and not self._is_hidden(self.selected):
            n = self.selected
            while n is not None and n not in self._rects:
                n = n.parent
            if n is not None:
                s = self._scale
                x0, y0, x1, y1 = self._rects[n]
                cr.rectangle(x0 / s + 1, y0 / s + 1, (x1 - x0) / s - 2, (y1 - y0) / s - 2)
                cr.set_source_rgb(0, 0, 0)
                cr.set_line_width(4)
                cr.stroke_preserve()
                cr.set_source_rgb(1, 1, 1)
                cr.set_line_width(2)
                cr.stroke()
        return True

    def _on_size(self, _w, _alloc):
        if self.root is None:
            return
        self._stale = True
        if self._timer:
            GLib.source_remove(self._timer)

        def fire():
            self._timer = None
            self.relayout()
            return False
        self._timer = GLib.timeout_add(150, fire)

    # ---- mouse ----
    def _node_at(self, x, y):
        if self._ids is None or self._stale:
            return None
        ix, iy = int(x * self._scale), int(y * self._scale)
        if 0 <= iy < self._H and 0 <= ix < self._W:
            n = self._leaf_nodes[self._ids[iy, ix]]
            if n is not None and n.agg:
                n = n.parent
            return n
        return None

    def _on_press(self, _w, ev):
        n = self._node_at(ev.x, ev.y)
        if n is None:
            return False
        if ev.button == 1 and ev.type == Gdk.EventType.BUTTON_PRESS:
            self.cb_click(n)
        elif ev.button == 3:
            self.cb_click(n)
            self.cb_right(n, ev)
        return True


# --------------------------------------------------------------------------- #
# mount list + chooser dialog
# --------------------------------------------------------------------------- #
PSEUDO_FS = {"proc", "sysfs", "devtmpfs", "devpts", "tmpfs", "cgroup", "cgroup2", "securityfs",
             "pstore", "bpf", "autofs", "mqueue", "hugetlbfs", "debugfs", "tracefs", "fusectl",
             "configfs", "binfmt_misc", "squashfs", "ramfs", "efivarfs", "nsfs", "overlay",
             "fuse.portal", "fuse.gvfsd-fuse", "rpc_pipefs", "selinuxfs"}


def list_mounts():
    res, seen = [], set()
    try:
        lines = open("/proc/mounts").read().splitlines()
    except OSError:
        return res
    for line in lines:
        parts = line.split()
        if len(parts) < 3:
            continue
        dev, mnt, fs = parts[0], parts[1], parts[2]
        mnt = re.sub(r"\\([0-7]{3})", lambda m: chr(int(m.group(1), 8)), mnt)
        if (fs in PSEUDO_FS or dev.startswith("/dev/loop") or mnt in seen
                or mnt.startswith(("/snap", "/var/snap", "/run", "/sys", "/proc", "/dev"))):
            continue
        try:
            v = os.statvfs(mnt)
        except OSError:
            continue
        total, free = v.f_blocks * v.f_frsize, v.f_bavail * v.f_frsize
        if total > 0:
            seen.add(mnt)
            res.append((mnt, f"{mnt}    —    {human(total)} total, {human(free)} free  [{fs}]"))
    return res


class DriveDialog(Gtk.Dialog):
    BROWSE = 1

    def __init__(self, parent):
        super().__init__(title="Select what to scan", transient_for=parent, modal=True)
        self.set_default_size(660, 380)
        self.add_button("Browse folder…", self.BROWSE)
        self.add_button("Cancel", Gtk.ResponseType.CANCEL)
        self.add_button("Scan", Gtk.ResponseType.OK).get_style_context().add_class("suggested-action")
        box = self.get_content_area()
        box.set_spacing(8)
        box.set_border_width(10)
        box.pack_start(Gtk.Label(label="Pick a mounted drive, your home folder, or browse for a folder:",
                                 xalign=0), False, False, 0)
        self.store = Gtk.ListStore(str, str)
        home = os.path.expanduser("~")
        for path, text in [(home, f"Home folder    —    {home}")] + list_mounts():
            self.store.append([path, text])
        self.view = Gtk.TreeView(model=self.store)
        self.view.set_headers_visible(False)
        self.view.append_column(Gtk.TreeViewColumn("", Gtk.CellRendererText(), text=1))
        self.view.connect("row-activated", lambda *a: self.response(Gtk.ResponseType.OK))
        self.view.get_selection().select_path(Gtk.TreePath.new_first())
        sc = Gtk.ScrolledWindow()
        sc.add(self.view)
        sc.set_vexpand(True)
        box.pack_start(sc, True, True, 0)
        self.show_all()

    def choose(self):
        path = None
        while True:
            r = self.run()
            if r == Gtk.ResponseType.OK:
                model, it = self.view.get_selection().get_selected()
                path = model[it][0] if it else None
                break
            if r == self.BROWSE:
                d = Gtk.FileChooserDialog(title="Folder to scan", transient_for=self,
                                          action=Gtk.FileChooserAction.SELECT_FOLDER)
                d.add_buttons("Cancel", Gtk.ResponseType.CANCEL, "Open", Gtk.ResponseType.ACCEPT)
                d.set_current_folder(os.path.expanduser("~"))
                resp, p = d.run(), d.get_filename()
                d.destroy()
                if resp == Gtk.ResponseType.ACCEPT and p:
                    path = p
                    break
                continue
            break
        self.destroy()
        return path


# --------------------------------------------------------------------------- #
# helpers for list widgets
# --------------------------------------------------------------------------- #
def add_text_col(view, title, text_col, sort_col=None, align=0.0, expand=False, ellipsize=None, width=0):
    r = Gtk.CellRendererText()
    r.set_property("xalign", align)
    if ellipsize is not None:
        r.set_property("ellipsize", ellipsize)
    c = Gtk.TreeViewColumn(title, r, text=text_col)
    c.set_resizable(True)
    c.set_expand(expand)
    if width:
        c.set_min_width(width)
    if sort_col is not None:
        c.set_sort_column_id(sort_col)
    view.append_column(c)
    return c


def scrolled(widget):
    sc = Gtk.ScrolledWindow()
    sc.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.AUTOMATIC)
    sc.add(widget)
    return sc


# tree store columns
ICON, NAME, PCT, PCTT, SIZET, ITEMST, FILEST, DIRST, MTIMET, NODE, POP, VIS = range(12)
SORT_KEYS = {
    1: lambda n: n.name.lower(), 2: lambda n: n.size, 3: lambda n: n.size, 4: lambda n: n.size,
    5: lambda n: n.files + n.dirs if n.is_dir else 0, 6: lambda n: n.files, 7: lambda n: n.dirs,
    8: lambda n: n.mtime,
}


def pos2val(pos):
    pos = int(round(pos))
    return 0 if pos <= 0 else int(round(10 ** ((pos - 1) * 6 / 99.0)))


def val2pos(v):
    return 0 if v <= 0 else int(min(100, max(1, round(math.log10(v) * 99 / 6) + 1)))


# --------------------------------------------------------------------------- #
# main window
# --------------------------------------------------------------------------- #
class App(Gtk.Window):
    def __init__(self):
        super().__init__(title=APP_NAME)
        self.set_default_size(1440, 900)
        self.cfg = load_settings()
        if self.cfg["theme"] not in THEMES:
            self.cfg["theme"] = DEFAULTS["theme"]
        self.root = self.scanner = self.scan_path = self.scope_node = None
        self.ext_stats, self.colors, self.dirs_all, self.top_files = {}, {}, [], []
        self.sort_col, self.sort_desc = 4, True
        self._css = self._poll_id = self._origin = None
        self._timers, self._syncing, self._updating = {}, False, False
        self._scope_cache = (None, None)
        self._sel_text, self._t0 = "", 0.0
        self.hidden = set()          # nodes hidden from the treemap (cleared on rescan)
        self._initial_dark = Gtk.Settings.get_default().get_property("gtk-application-prefer-dark-theme")
        self.accel = Gtk.AccelGroup()
        self.add_accel_group(self.accel)

        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self.add(outer)
        self.treemap = TreeMap(lambda n: self.select_node(n, "treemap"), self._popup, self._hover)
        self.treemap.hidden = self.hidden
        self._build_tree()
        self._build_side()
        outer.pack_start(self._build_menubar(), False, False, 0)

        top = Gtk.Paned(orientation=Gtk.Orientation.HORIZONTAL)
        top.pack1(scrolled(self.tree), True, False)
        top.pack2(self.side, False, False)
        top.set_position(820)
        bottom = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        bottom.pack_start(self.treemap, True, True, 0)
        bottom.pack_start(self._build_strip(), False, False, 0)
        vp = Gtk.Paned(orientation=Gtk.Orientation.VERTICAL)
        vp.pack1(top, True, False)
        vp.pack2(bottom, True, False)
        vp.set_position(420)
        outer.pack_start(vp, True, True, 0)
        outer.pack_start(self._build_status(), False, False, 0)

        self.connect("destroy", self._on_destroy)
        self._apply_theme(initial=True)
        self._load_filter_widgets()

    # ================= UI construction =================
    def _build_menubar(self):
        bar = Gtk.MenuBar()

        def top_menu(label):
            mi, m = Gtk.MenuItem(label=label, use_underline=True), Gtk.Menu()
            mi.set_submenu(m)
            bar.append(mi)
            return m

        def item(menu, label, cb, accel=None):
            it = Gtk.MenuItem(label=label, use_underline=True)
            it.connect("activate", lambda _w: cb())
            if accel:
                k, mod = Gtk.accelerator_parse(accel)
                it.add_accelerator("activate", self.accel, k, mod, Gtk.AccelFlags.VISIBLE)
            menu.append(it)
            return it

        def check(menu, label, key):
            it = Gtk.CheckMenuItem(label=label)
            it.set_active(bool(self.cfg[key]))
            it.connect("toggled", lambda w: self._set_cfg(key, w.get_active()))
            menu.append(it)

        f = top_menu("_File")
        item(f, "_Scan…", self.choose_and_scan, "<Primary>o")
        self.mi_rescan = item(f, "_Rescan", self.rescan, "F5")
        self.mi_stop = item(f, "S_top", self.stop_scan, "Escape")
        self.mi_stop.set_sensitive(False)
        f.append(Gtk.SeparatorMenuItem())
        item(f, "_Quit", self.close, "<Primary>q")

        o = top_menu("_Options")
        check(o, "Stay on one filesystem", "one_fs")
        check(o, "Use apparent size (instead of disk usage)", "apparent")
        check(o, "Accurate progress (count items first for folders)", "precount")

        v = top_menu("_View")
        theme_mi, theme_menu = Gtk.MenuItem(label="Theme"), Gtk.Menu()
        theme_mi.set_submenu(theme_menu)
        v.append(theme_mi)
        group = None
        for name in THEMES:
            r = Gtk.RadioMenuItem.new_with_label_from_widget(group, name) if group else \
                Gtk.RadioMenuItem.new_with_label(None, name)
            group = group or r
            r.set_active(name == self.cfg["theme"])
            r.connect("toggled", lambda w, n=name: w.get_active() and self.set_theme(n))
            theme_menu.append(r)
        v.append(Gtk.SeparatorMenuItem())
        self.mi_show_all = item(v, "Show All Hidden in Treemap", self._clear_hidden)
        self.mi_show_all.set_sensitive(False)

        h = top_menu("_Help")
        item(h, "_About", self._about)
        return bar

    def _build_tree(self):
        self.tree_store = Gtk.TreeStore(str, str, int, str, str, str, str, str, str, GObject.TYPE_PYOBJECT, bool, bool)
        t = self.tree = Gtk.TreeView(model=self.tree_store)
        t.set_enable_search(False)
        t.connect("test-expand-row", self._on_test_expand)
        t.connect("button-press-event", self._tree_button)
        t.get_selection().connect("changed", self._on_tree_selection)
        self.tree_cols = {}

        c = Gtk.TreeViewColumn("Name")
        pr, tr = Gtk.CellRendererPixbuf(), Gtk.CellRendererText()
        c.pack_start(pr, False)
        c.pack_start(tr, True)
        c.add_attribute(pr, "icon-name", ICON)
        c.add_attribute(pr, "sensitive", VIS)
        c.add_attribute(tr, "text", NAME)
        c.add_attribute(tr, "sensitive", VIS)
        c.set_resizable(True)
        c.set_min_width(300)
        self._tree_col(c, 1)

        bar = Gtk.CellRendererProgress()
        bar.set_property("text", " ")
        c = Gtk.TreeViewColumn("Subtree Percentage", bar, value=PCT)
        c.set_resizable(True)
        c.set_min_width(120)
        self._tree_col(c, 2)
        for idx, (title, col) in enumerate((("Percentage", PCTT), ("Size", SIZET), ("Items", ITEMST),
                                            ("Files", FILEST), ("Subdirs", DIRST), ("Last Change", MTIMET))):
            r = Gtk.CellRendererText()
            r.set_property("xalign", 1.0 if title != "Last Change" else 0.0)
            c = Gtk.TreeViewColumn(title, r, text=col)
            c.set_resizable(True)
            c.set_min_width(70)
            self._tree_col(c, 3 + idx)
        self._update_indicators()

    def _tree_col(self, col, idx):
        col.set_clickable(True)
        col.connect("clicked", self._on_header, idx)
        self.tree.append_column(col)
        self.tree_cols[idx] = col

    def _build_side(self):
        nb = self.side = Gtk.Notebook()
        nb.set_size_request(520, -1)

        # --- extensions ---
        self.ext_store = Gtk.ListStore(str, str, GObject.TYPE_INT64, str, float, str, GObject.TYPE_INT64, str)
        ev = self.ext_view = Gtk.TreeView(model=self.ext_store)
        ev.set_enable_search(False)
        add_text_col(ev, "Extension", 0, 0, width=90)
        r = Gtk.CellRendererText()
        c = Gtk.TreeViewColumn("Color", r, background=1)
        c.set_min_width(50)
        ev.append_column(c)
        add_text_col(ev, "Bytes", 3, 2, 1.0, width=90)
        add_text_col(ev, "% Bytes", 5, 4, 1.0, width=70)
        add_text_col(ev, "Files", 7, 6, 1.0, width=70)
        self.ext_store.set_sort_column_id(2, Gtk.SortType.DESCENDING)
        ev.get_selection().connect("changed", self._on_ext_selection)
        nb.append_page(scrolled(ev), Gtk.Label(label="Extensions"))

        # --- largest folders ---
        page = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        page.set_border_width(6)
        grid = Gtk.Grid(column_spacing=8, row_spacing=6)
        self.cmb_op = Gtk.ComboBoxText()
        self.cmb_op.append("le", "at most")
        self.cmb_op.append("ge", "at least")
        self.spin = Gtk.SpinButton.new_with_range(0, 10_000_000, 1)
        self.spin.set_width_chars(8)
        grid.attach(Gtk.Label(label="Show folders with", xalign=0), 0, 0, 1, 1)
        grid.attach(self.cmb_op, 1, 0, 1, 1)
        grid.attach(self.spin, 2, 0, 1, 1)
        grid.attach(Gtk.Label(label="files inside", xalign=0), 3, 0, 1, 1)
        self.scale = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 0, 100, 1)
        self.scale.set_draw_value(False)
        self.scale.set_hexpand(True)
        for v in (1, 10, 100, 1000, 10000, 100000, 1000000):
            lab = {1000: "1k", 10000: "10k", 100000: "100k", 1000000: "1M"}.get(v, str(v))
            self.scale.add_mark(val2pos(v), Gtk.PositionType.BOTTOM, lab)
        self.scale.add_mark(0, Gtk.PositionType.BOTTOM, "0")
        grid.attach(self.scale, 0, 1, 4, 1)
        self.chk_rec = Gtk.CheckButton(label="Count files recursively (include all subfolders)")
        self.chk_scope = Gtk.CheckButton(label="Only inside the selected folder")
        grid.attach(self.chk_rec, 0, 2, 4, 1)
        grid.attach(self.chk_scope, 0, 3, 4, 1)
        page.pack_start(grid, False, False, 0)

        self.fold_store = Gtk.ListStore(str, str, GObject.TYPE_INT64, str, GObject.TYPE_INT64, str,
                                        GObject.TYPE_INT64, str, GObject.TYPE_PYOBJECT)
        fv = self.fold_view = Gtk.TreeView(model=self.fold_store)
        fv.set_enable_search(False)
        fv.set_tooltip_column(0)
        add_text_col(fv, "Folder", 0, 0, expand=True, ellipsize=Pango.EllipsizeMode.START, width=140)
        add_text_col(fv, "Size", 1, 2, 1.0)
        self.col_files = add_text_col(fv, "Files", 3, 4, 1.0)
        add_text_col(fv, "Folders", 5, 6, 1.0)
        add_text_col(fv, "%", 7, None, 1.0)
        self.fold_store.set_sort_column_id(2, Gtk.SortType.DESCENDING)
        fv.get_selection().connect("changed", self._on_list_selection)
        fv.connect("button-press-event", self._list_button)
        fv.connect("row-activated", lambda v, p, c: open_path(self.fold_store[p][8].path()))
        page.pack_start(scrolled(fv), True, True, 0)
        self.lbl_fold = Gtk.Label(xalign=0)
        self.lbl_fold.get_style_context().add_class("dim-label")
        page.pack_start(self.lbl_fold, False, False, 0)
        nb.append_page(page, Gtk.Label(label="Largest folders"))

        # --- largest files ---
        page2 = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        page2.set_border_width(6)
        self.chk_scope2 = Gtk.CheckButton(label="Only inside the selected folder")
        page2.pack_start(self.chk_scope2, False, False, 0)
        self.file_store = Gtk.ListStore(str, str, GObject.TYPE_INT64, str, float, GObject.TYPE_PYOBJECT)
        lv = self.file_view = Gtk.TreeView(model=self.file_store)
        lv.set_enable_search(False)
        lv.set_tooltip_column(0)
        add_text_col(lv, "File", 0, 0, expand=True, ellipsize=Pango.EllipsizeMode.START, width=140)
        add_text_col(lv, "Size", 1, 2, 1.0)
        add_text_col(lv, "Modified", 3, 4, 0.0)
        self.file_store.set_sort_column_id(2, Gtk.SortType.DESCENDING)
        lv.get_selection().connect("changed", self._on_list_selection)
        lv.connect("button-press-event", self._list_button)
        lv.connect("row-activated", lambda v, p, c: open_path(os.path.dirname(self.file_store[p][5].path())))
        page2.pack_start(scrolled(lv), True, True, 0)
        self.lbl_files = Gtk.Label(xalign=0)
        self.lbl_files.get_style_context().add_class("dim-label")
        page2.pack_start(self.lbl_files, False, False, 0)
        nb.append_page(page2, Gtk.Label(label="Largest files"))
        nb.set_current_page(1)

        self.cmb_op.connect("changed", self._on_filter_changed)
        self.spin.connect("value-changed", self._on_spin)
        self.scale.connect("value-changed", self._on_scale)
        self.chk_rec.connect("toggled", self._on_filter_changed)
        self.chk_scope.connect("toggled", self._on_scope_toggle)
        self.chk_scope2.connect("toggled", self._on_scope_toggle)

    def _build_strip(self):
        box = Gtk.Box(spacing=10)
        box.get_style_context().add_class("treestrip")
        box.set_border_width(5)
        box.pack_start(Gtk.Label(label="Treemap colors:"), False, False, 4)
        self.sc_look = {}
        for key, label, lo, hi in (("brightness", "Brightness", 30, 170), ("saturation", "Saturation", 0, 200),
                                   ("edge", "Edge shading", 0, 100)):
            box.pack_start(Gtk.Label(label=label), False, False, 0)
            s = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, lo, hi, 1)
            s.set_value(self.cfg[key])
            s.set_digits(0)
            s.set_value_pos(Gtk.PositionType.RIGHT)
            s.set_size_request(150, -1)
            s.connect("value-changed", self._on_look)
            self.sc_look[key] = s
            box.pack_start(s, False, False, 0)
        self.chk_classic = Gtk.CheckButton(label="Classic WinDirStat palette")
        self.chk_classic.set_active(self.cfg["classic_colors"])
        self.chk_classic.connect("toggled", self._on_classic)
        box.pack_start(self.chk_classic, False, False, 6)
        reset = Gtk.Button(label="Reset")
        reset.connect("clicked", self._reset_look)
        box.pack_start(reset, False, False, 0)
        self.treemap.set_look(self.cfg["brightness"] / 100.0, self.cfg["saturation"] / 100.0,
                              self.cfg["edge"] / 100.0)
        return box

    def _build_status(self):
        box = Gtk.Box(spacing=10)
        box.get_style_context().add_class("statusbox")
        box.set_border_width(5)
        self.lbl_status = Gtk.Label(xalign=0)
        self.lbl_status.set_ellipsize(Pango.EllipsizeMode.MIDDLE)
        self.lbl_status.set_hexpand(True)
        box.pack_start(self.lbl_status, True, True, 0)
        self.progress = Gtk.ProgressBar()
        self.progress.set_size_request(420, 22)
        self.prog_label = Gtk.Label()
        self.prog_label.set_halign(Gtk.Align.CENTER)
        self.prog_label.set_valign(Gtk.Align.CENTER)
        self.prog_box = Gtk.Overlay()
        self.prog_box.add(self.progress)
        self.prog_box.add_overlay(self.prog_label)
        self.prog_box.set_valign(Gtk.Align.CENTER)
        self.prog_box.set_no_show_all(True)
        box.pack_start(self.prog_box, False, False, 0)
        self.btn_stop = Gtk.Button(label="Stop")
        self.btn_stop.set_no_show_all(True)
        self.btn_stop.connect("clicked", lambda _b: self.stop_scan())
        box.pack_start(self.btn_stop, False, False, 0)
        self.lbl_free = Gtk.Label()
        box.pack_end(self.lbl_free, False, False, 0)
        self.lbl_hidden = Gtk.Label()
        self.lbl_hidden.set_no_show_all(True)
        self.lbl_hidden.set_tooltip_text("Right-click an item (or use View ▸ Show All Hidden) to show it again")
        box.pack_end(self.lbl_hidden, False, False, 12)
        return box

    # ================= settings / theme =================
    def _set_cfg(self, key, value):
        self.cfg[key] = value
        save_settings(self.cfg)

    def debounce(self, key, ms, fn):
        old = self._timers.pop(key, None)
        if old:
            GLib.source_remove(old)

        def run():
            self._timers.pop(key, None)
            fn()
            return False
        self._timers[key] = GLib.timeout_add(ms, run)

    def set_theme(self, name):
        self._set_cfg("theme", name)
        self._apply_theme()

    def _apply_theme(self, initial=False):
        t = THEMES[self.cfg["theme"]]
        settings, screen = Gtk.Settings.get_default(), Gdk.Screen.get_default()
        settings.set_property("gtk-application-prefer-dark-theme",
                              self._initial_dark if t["dark"] is None else t["dark"])
        if self._css is not None:
            Gtk.StyleContext.remove_provider_for_screen(screen, self._css)
            self._css = None
        if t["custom"]:
            prov = Gtk.CssProvider()
            try:
                prov.load_from_data(build_css(t).encode())
                Gtk.StyleContext.add_provider_for_screen(screen, prov, Gtk.STYLE_PROVIDER_PRIORITY_USER)
                self._css = prov
            except GLib.Error as e:
                print("CSS error:", e, file=sys.stderr)
        self.treemap.set_theme(t["tbg"], t["other"])
        self._apply_palette()

    def _palette(self):
        t = THEMES[self.cfg["theme"]]
        return t["palette"] if (t["palette"] and not self.cfg["classic_colors"]) else CLASSIC

    def _apply_palette(self):
        pal, other = self._palette(), THEMES[self.cfg["theme"]]["other"]
        ranked = sorted(self.ext_stats.items(), key=lambda kv: kv[1][0], reverse=True)
        self.colors = {ext: pal[i] if i < len(pal) else (other or (150, 150, 150))
                       for i, (ext, _) in enumerate(ranked)}
        self.treemap.set_colors(self.colors)
        self._refresh_swatches()

    def _refresh_swatches(self):
        for row in self.ext_store:
            row[1] = rgb2hex(self.treemap.display_rgb(row[0]))

    def _on_look(self, _s):
        for k, s in self.sc_look.items():
            self.cfg[k] = int(s.get_value())
        self.treemap.set_look(self.cfg["brightness"] / 100.0, self.cfg["saturation"] / 100.0,
                              self.cfg["edge"] / 100.0)
        self.debounce("swatch", 150, self._refresh_swatches)
        self.debounce("save", 600, lambda: save_settings(self.cfg))

    def _reset_look(self, _b):
        for k in ("brightness", "saturation", "edge"):
            self.sc_look[k].set_value(DEFAULTS[k])

    def _on_classic(self, w):
        self._set_cfg("classic_colors", w.get_active())
        self._apply_palette()

    # ================= scanning =================
    def choose_and_scan(self):
        if self.scanner and self.scanner.is_alive():
            return
        path = DriveDialog(self).choose()
        if path:
            self.start_scan(path)

    def rescan(self):
        if self.scan_path:
            self.start_scan(self.scan_path)

    def stop_scan(self):
        if self.scanner and self.scanner.is_alive():
            self.scanner.cancel = True

    def start_scan(self, path):
        if self.scanner and self.scanner.is_alive():
            return
        self.scan_path = os.path.abspath(path)
        self.hidden.clear()
        self._update_hidden_ui()
        self.set_title(f"{self.scan_path} — {APP_NAME}")
        self.scanner = Scanner(self.scan_path, self.cfg["one_fs"], self.cfg["apparent"], self.cfg["precount"])
        self._t0 = time.monotonic()
        self.prog_box.show_all()
        self.progress.set_fraction(0)
        self.prog_label.set_text("Starting…")
        self.btn_stop.show()
        self.mi_stop.set_sensitive(True)
        self.mi_rescan.set_sensitive(False)
        self.scanner.start()
        self._poll_id = GLib.timeout_add(100, self._poll)

    def _poll(self):
        s = self.scanner
        if s.done:
            self._poll_id = None
            self._finish_scan()
            return False
        el = time.monotonic() - self._t0
        path = s.current if len(s.current) < 100 else "…" + s.current[-98:]
        if s.phase == "counting":
            self.progress.pulse()
            self.prog_label.set_text(f"Counting items…  {s.entries:,}")
            self.lbl_status.set_text(f"Counting items before scanning (for an accurate progress bar)…")
        else:
            if s.total > 0:
                frac = min(0.99, s.entries / s.total)
                eta = f"  ·  ETA {fmt_dur(el * (1 - frac) / frac)}" if frac > 0.03 and el > 2 else ""
                self.progress.set_fraction(frac)
                self.prog_label.set_text(f"{frac * 100:.0f}%  ·  {s.entries:,} items  ·  {human(s.nbytes)}{eta}")
            else:
                self.progress.pulse()
                self.prog_label.set_text(f"{s.entries:,} items  ·  {human(s.nbytes)}")
            self.lbl_status.set_text(f"Scanning…  {path}")
        return True

    def _finish_scan(self):
        s = self.scanner
        self.prog_box.hide()
        self.btn_stop.hide()
        self.mi_stop.set_sensitive(False)
        self.mi_rescan.set_sensitive(True)
        if s.root is None:
            self.lbl_status.set_text("Scan stopped." if s.cancel else f"Cannot read {self.scan_path}")
            return
        self.root, self.ext_stats = s.root, s.ext_stats
        self.dirs_all, self.top_files = s.dirs_all, s.top_files
        self.scope_node, self._scope_cache = self.root, (None, None)
        self.hidden.clear()
        self._rebuild_views(self.root)
        self._update_hidden_ui()
        try:
            v = os.statvfs(self.scan_path)
            self.lbl_free.set_text(f"Free: {human(v.f_bavail * v.f_frsize)} of {human(v.f_blocks * v.f_frsize)}")
        except OSError:
            self.lbl_free.set_text("")
        msg = (f"{'Scan stopped — partial results: ' if s.cancel else 'Scanned '}{self.root.files:,} files in "
               f"{self.root.dirs:,} folders, {human(self.root.size)}, {time.monotonic() - self._t0:.1f}s")
        if s.errors:
            msg += f"  ({s.errors:,} items could not be read)"
        self._sel_text = msg
        self.lbl_status.set_text(msg)

    # ================= tree view =================
    def _sorted_children(self, node):
        if self.sort_col in (2, 3, 4) and self.sort_desc:
            return node.children
        return sorted(node.children, key=SORT_KEYS[self.sort_col], reverse=self.sort_desc)

    def _add_row(self, parent_it, node, parent_size):
        d = node.is_dir
        frac = node.size / parent_size if parent_size > 0 else 1.0
        items = node.files + node.dirs if d else 0
        name = node.name + (HIDDEN_SUFFIX if node in self.hidden else "")
        it = self.tree_store.append(parent_it, [
            "folder" if d else "text-x-generic", name, int(frac * 100), f"{frac * 100:.1f}%",
            human(node.size), f"{items:,}" if d else "", f"{node.files:,}" if d else "",
            f"{node.dirs:,}" if d else "", fmt_time(node.mtime), node, False,
            self._hidden_owner(node) is None])
        if d and node.children:
            self.tree_store.append(it, ["", "…", 0, "", "", "", "", "", "", None, False, True])
        return it

    def _on_test_expand(self, _view, it, _path):
        row = self.tree_store[it]
        if row[POP] or row[NODE] is None:
            return False
        row[POP] = True
        node = row[NODE]
        placeholder = self.tree_store.iter_children(it)
        kids = self._sorted_children(node)
        for c in kids[:MAX_ROWS]:
            self._add_row(it, c, node.size)
        extra = len(kids) - MAX_ROWS
        if extra > 0:
            self.tree_store.append(it, ["", f"… {extra:,} more items not shown", 0, "", "", "", "", "", "",
                                        None, False, True])
        if placeholder is not None:
            self.tree_store.remove(placeholder)
        return False

    def _on_header(self, _col, idx):
        if self.sort_col == idx:
            self.sort_desc = not self.sort_desc
        else:
            self.sort_col, self.sort_desc = idx, idx != 1
        self._update_indicators()
        self._resort_level(None)

    def _update_indicators(self):
        for idx, c in self.tree_cols.items():
            on = idx == self.sort_col
            c.set_sort_indicator(on)
            if on:
                c.set_sort_order(Gtk.SortType.DESCENDING if self.sort_desc else Gtk.SortType.ASCENDING)

    def _resort_level(self, parent_it):
        st = self.tree_store
        entries, it = [], st.iter_children(parent_it)
        while it is not None:
            entries.append((st[it][NODE], it.copy()))
            it = st.iter_next(it)
        if not entries:
            return
        idx = list(range(len(entries)))
        real = [i for i in idx if entries[i][0] is not None]
        fixed = [i for i in idx if entries[i][0] is None]
        key = SORT_KEYS[self.sort_col]
        real.sort(key=lambda i: key(entries[i][0]), reverse=self.sort_desc)
        new_order = real + fixed
        if new_order != idx:                       # TreeStore.reorder isn't exposed by PyGObject
            first_diff = next(k for k, (a, b) in enumerate(zip(new_order, idx)) if a != b)
            for i in new_order[first_diff:]:
                st.move_before(entries[i][1], None)    # push to the end, in the wanted order
        for node, ch in entries:
            if node is not None and node.is_dir and st[ch][POP]:
                self._resort_level(ch)

    def select_node(self, node, origin="app"):
        if node is None or self.root is None:
            return
        chain, n = [], node
        while n is not None:
            chain.append(n)
            n = n.parent
        chain.reverse()
        st = self.tree_store
        it = st.get_iter_first()
        if it is None or st[it][NODE] is not chain[0]:
            return
        path = st.get_path(it)
        for n in chain[1:]:
            self.tree.expand_row(path, False)
            ch, found = st.iter_children(it), None
            while ch is not None:
                if st[ch][NODE] is n:
                    found = ch
                    break
                ch = st.iter_next(ch)
            if found is None:
                break
            it, path = found, st.get_path(found)
        self._origin = origin
        self.tree.set_cursor(path)
        self.tree.scroll_to_cell(path, None, True, 0.5, 0.0)
        self._origin = None

    def _on_tree_selection(self, sel):
        model, it = sel.get_selected()
        node = model[it][NODE] if it is not None else None
        if node is None:
            return
        origin = self._origin or "tree"
        self.treemap.set_selected(node)
        pct = node.size / self.root.size * 100 if self.root and self.root.size else 100
        text = f"{node.path()}  —  {human(node.size)} ({pct:.1f}% of scan)"
        if node.is_dir:
            text += f", {node.files:,} files, {node.dirs:,} folders"
        self._sel_text = text
        if not (self.scanner and self.scanner.is_alive()):
            self.lbl_status.set_text(text)
        if origin != "list":
            scope = node if node.is_dir else node.parent
            if scope is not self.scope_node:
                self.scope_node, self._scope_cache = scope, (None, None)
                if self.cfg["scope_selected"]:
                    self.debounce("lists", 150, self.refresh_lists)

    def _tree_button(self, view, ev):
        if ev.button == 3 and ev.type == Gdk.EventType.BUTTON_PRESS:
            hit = view.get_path_at_pos(int(ev.x), int(ev.y))
            if hit:
                node = self.tree_store[hit[0]][NODE]
                if node is not None:
                    view.set_cursor(hit[0])
                    self._popup(node, ev)
                    return True
        return False

    # ================= extension list =================
    def _on_ext_selection(self, sel):
        model, it = sel.get_selected()
        self.treemap.set_highlight(model[it][0] if it is not None else None)

    # ================= lists =================
    def _list_button(self, view, ev):
        if ev.button == 3 and ev.type == Gdk.EventType.BUTTON_PRESS:
            hit = view.get_path_at_pos(int(ev.x), int(ev.y))
            if hit:
                model = view.get_model()
                node = model[hit[0]][8 if view is self.fold_view else 5]
                view.get_selection().select_path(hit[0])
                self._popup(node, ev)
                return True
        return False

    def _on_list_selection(self, sel):
        if self._updating:
            return
        model, it = sel.get_selected()
        if it is None:
            return
        node = model[it][8 if model is self.fold_store else 5]
        self.select_node(node, "list")

    def _scope_nodes(self):
        """(dirs, files-iterable-or-None) for the current scope."""
        if not (self.cfg["scope_selected"] and self.scope_node is not None):
            return None
        if self._scope_cache[0] is self.scope_node:
            return self._scope_cache[1]
        dirs, files, stack = [], [], [self.scope_node]
        while stack:
            n = stack.pop()
            for c in n.children:
                if c.is_dir:
                    dirs.append(c)
                    stack.append(c)
                else:
                    files.append(c)
        self._scope_cache = (self.scope_node, (dirs, files))
        return dirs, files

    def refresh_lists(self):
        self.refresh_folders()
        self.refresh_files()

    def refresh_folders(self):
        self._updating = True
        st = self.fold_store
        self.fold_view.set_model(None)
        st.clear()
        rec = self.chk_rec.get_active()
        self.col_files.set_title("Files (all levels)" if rec else "Files (direct)")
        if self.root is None:
            self.fold_view.set_model(st)
            self._updating = False
            self.lbl_fold.set_text("Scan something to see the largest folders.")
            return
        scope = self._scope_nodes()
        pool = scope[0] if scope else (d for d in self.dirs_all if d is not self.root)
        n, op = self.spin.get_value_as_int(), self.cmb_op.get_active_id() or "ge"
        if rec:
            match = [d for d in pool if (d.files <= n if op == "le" else d.files >= n)]
        else:
            match = [d for d in pool if (d.dfiles <= n if op == "le" else d.dfiles >= n)]
        top = heapq.nlargest(LIST_LIMIT, match, key=lambda d: d.size)
        total = self.root.size or 1
        for d in top:
            files = d.files if rec else d.dfiles
            st.append([d.path(), human(d.size), d.size, f"{files:,}", files, f"{d.dirs:,}", d.dirs,
                       f"{d.size / total * 100:.1f}%", d])
        self.fold_view.set_model(st)
        self._updating = False
        self.lbl_fold.set_text(f"{len(match):,} folders match — showing the largest {len(top):,}")

    def refresh_files(self):
        self._updating = True
        st = self.file_store
        self.file_view.set_model(None)
        st.clear()
        if self.root is None:
            self.file_view.set_model(st)
            self._updating = False
            self.lbl_files.set_text("Scan something to see the largest files.")
            return
        scope = self._scope_nodes()
        files = heapq.nlargest(LIST_LIMIT, scope[1], key=lambda f: f.size) if scope \
            else self.top_files[:LIST_LIMIT]
        for f in files:
            st.append([f.path(), human(f.size), f.size, fmt_time(f.mtime), f.mtime, f])
        self.file_view.set_model(st)
        self._updating = False
        self.lbl_files.set_text(f"Showing the {len(files):,} largest files")

    def _load_filter_widgets(self):
        self._syncing = True
        self.cmb_op.set_active_id(self.cfg["folder_op"])
        self.spin.set_value(self.cfg["folder_n"])
        self.scale.set_value(val2pos(self.cfg["folder_n"]))
        self.chk_rec.set_active(self.cfg["folder_recursive"])
        self.chk_scope.set_active(self.cfg["scope_selected"])
        self.chk_scope2.set_active(self.cfg["scope_selected"])
        self._syncing = False

    def _on_scale(self, scale):
        if self._syncing:
            return
        self._syncing = True
        self.spin.set_value(pos2val(scale.get_value()))
        self._syncing = False
        self._on_filter_changed()

    def _on_spin(self, spin):
        if self._syncing:
            return
        self._syncing = True
        self.scale.set_value(val2pos(spin.get_value_as_int()))
        self._syncing = False
        self._on_filter_changed()

    def _on_filter_changed(self, *_a):
        if self._syncing:
            return
        self.cfg.update(folder_op=self.cmb_op.get_active_id() or "ge", folder_n=self.spin.get_value_as_int(),
                        folder_recursive=self.chk_rec.get_active())
        self.debounce("save", 600, lambda: save_settings(self.cfg))
        self.debounce("folders", 80, self.refresh_folders)

    def _on_scope_toggle(self, w):
        if self._syncing:
            return
        self._syncing = True
        self.chk_scope.set_active(w.get_active())
        self.chk_scope2.set_active(w.get_active())
        self._syncing = False
        self._set_cfg("scope_selected", w.get_active())
        self.debounce("lists", 50, self.refresh_lists)

    # ================= views =================
    def _rebuild_views(self, select):
        self.tree_store.clear()
        it = self._add_row(None, self.root, 0)
        self.tree.expand_row(self.tree_store.get_path(it), False)
        total = sum(v[0] for v in self.ext_stats.values()) or 1
        self.ext_view.set_model(None)
        self.ext_store.clear()
        for ext, (b, n) in sorted(self.ext_stats.items(), key=lambda kv: -kv[1][0])[:3000]:
            self.ext_store.append([ext, "#000000", b, human(b), b / total * 100, f"{b / total * 100:.1f}%", n,
                                   f"{n:,}"])
        self.ext_view.set_model(self.ext_store)
        self.treemap.root = self.root
        self._apply_palette()
        self.treemap.set_data(self.root, self.colors)
        self.refresh_lists()
        self.select_node(select)

    def _hover(self, node):
        if self.scanner and self.scanner.is_alive():
            return
        self.lbl_status.set_text(self._sel_text if node is None else f"{node.path()}  —  {human(node.size)}")

    # ================= cleanup actions =================
    def _hidden_owner(self, node):
        """Nearest hidden node among node and its ancestors, or None."""
        if self.hidden:
            while node is not None:
                if node in self.hidden:
                    return node
                node = node.parent
        return None

    def _popup(self, node, event):
        self._ctx_menu = m = self._menu(node)      # keep a reference while the menu is open
        m.show_all()
        m.popup_at_pointer(event)

    def _menu(self, node):
        m = Gtk.Menu()
        path = node.path()

        def add(label, cb, enabled=True):
            mi = Gtk.MenuItem(label=label)
            mi.connect("activate", lambda _w: cb())
            mi.set_sensitive(enabled)
            m.append(mi)

        if node.is_dir:
            add("Open Folder", lambda: open_path(path))
        else:
            add("Open File", lambda: open_path(path))
            add("Open Containing Folder", lambda: open_path(os.path.dirname(path)))
        add("Copy Path", lambda: Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD).set_text(path, -1))
        m.append(Gtk.SeparatorMenuItem())
        can = node.parent is not None
        owner = self._hidden_owner(node)
        if not can:
            add("Hide from Treemap", lambda: None, False)
        elif owner is node:
            add("Show on Treemap", lambda: self._set_hidden(node, False))
        elif owner is not None:
            add(f"Show on Treemap (hidden via “{owner.name}”)", lambda: self._set_hidden(owner, False))
        else:
            add("Hide from Treemap", lambda: self._set_hidden(node, True))
        m.append(Gtk.SeparatorMenuItem())
        add("Move to Trash…", lambda: self._trash(node), can)
        add("Delete Permanently…", lambda: self._delete(node), can)
        return m

    def _set_hidden(self, node, flag):
        (self.hidden.add if flag else self.hidden.discard)(node)
        self._refresh_hidden_rows()
        self.treemap.relayout()
        self._update_hidden_ui()

    def _clear_hidden(self):
        self.hidden.clear()
        self._refresh_hidden_rows()
        self.treemap.relayout()
        self._update_hidden_ui()

    def _update_hidden_ui(self):
        n = len(self.hidden)
        self.lbl_hidden.set_text(f"{n:,} hidden from treemap")
        self.lbl_hidden.set_visible(n > 0)
        self.mi_show_all.set_sensitive(n > 0)

    def _refresh_hidden_rows(self):
        st = self.tree_store

        def walk(parent_it, inherited):
            it = st.iter_children(parent_it)
            while it is not None:
                row = st[it]
                node = row[NODE]
                if node is not None:
                    own = node in self.hidden
                    hid = inherited or own
                    row[VIS] = not hid
                    row[NAME] = node.name + (HIDDEN_SUFFIX if own else "")
                    if node.is_dir and row[POP]:
                        walk(it, hid)
                it = st.iter_next(it)
        walk(None, False)

    def _confirm(self, title, node, extra=""):
        d = Gtk.MessageDialog(transient_for=self, modal=True, message_type=Gtk.MessageType.QUESTION,
                              buttons=Gtk.ButtonsType.YES_NO, text=title)
        d.format_secondary_text(f"{node.path()}\n\n{human(node.size)}"
                                f"{', ' + format(node.files, ',') + ' files' if node.is_dir else ''}{extra}")
        d.set_default_response(Gtk.ResponseType.NO)
        ok = d.run() == Gtk.ResponseType.YES
        d.destroy()
        return ok

    def _error(self, text):
        d = Gtk.MessageDialog(transient_for=self, modal=True, message_type=Gtk.MessageType.ERROR,
                              buttons=Gtk.ButtonsType.CLOSE, text=text)
        d.run()
        d.destroy()

    def _trash(self, node):
        if not self._confirm("Move to Trash?", node):
            return
        try:
            subprocess.run(["gio", "trash", node.path()], check=True, capture_output=True)
        except (OSError, subprocess.CalledProcessError) as e:
            self._error(f"Could not move to trash:\n{e}")
            return
        self._remove_node(node)

    def _delete(self, node):
        if not self._confirm("Delete permanently?", node, "\n\nThis cannot be undone."):
            return
        try:
            shutil.rmtree(node.path()) if node.is_dir else os.remove(node.path())
        except OSError as e:
            self._error(f"Could not delete:\n{e}")
            self.rescan()
            return
        self._remove_node(node)

    def _remove_node(self, node):
        parent, stack, removed = node.parent, [node], set()
        while stack:
            n = stack.pop()
            removed.add(id(n))
            if n.is_dir:
                stack.extend(n.children)
            elif n.ext in self.ext_stats:
                self.ext_stats[n.ext][0] -= n.size
                self.ext_stats[n.ext][1] -= 1
        files = node.files if node.is_dir else 1
        dirs = node.dirs + 1 if node.is_dir else 0
        a = parent
        while a is not None:
            a.size -= node.size
            a.files -= files
            a.dirs -= dirs
            if not node.is_dir and a is parent:
                a.dfiles -= 1
            a = a.parent
        parent.children.remove(node)
        self.dirs_all = [d for d in self.dirs_all if id(d) not in removed]
        self.top_files = [f for f in self.top_files if id(f) not in removed]
        self.ext_stats = {k: v for k, v in self.ext_stats.items() if v[1] > 0}
        if self.scope_node is not None and id(self.scope_node) in removed:
            self.scope_node = parent
        self._scope_cache = (None, None)
        self.hidden.difference_update([n for n in self.hidden if id(n) in removed])
        self._rebuild_views(parent)
        self._update_hidden_ui()

    # ================= misc =================
    def _about(self):
        d = Gtk.AboutDialog(transient_for=self, modal=True)
        d.set_program_name(APP_NAME)
        d.set_comments("Directory tree, extension statistics and a treemap for disk usage.\n"
                       "Inspired by WinDirStat and KDirStat.")
        d.set_license_type(Gtk.License.GPL_2_0)
        d.run()
        d.destroy()

    def _on_destroy(self, *_a):
        if self.scanner and self.scanner.is_alive():
            self.scanner.cancel = True
        Gtk.main_quit()


def main():
    sys.setrecursionlimit(20000)
    threading.stack_size(256 * 1024 * 1024)       # deep directory trees
    win = App()
    win.show_all()
    win.prog_box.hide()
    win.btn_stop.hide()
    if len(sys.argv) > 1 and os.path.isdir(sys.argv[1]):
        win.start_scan(sys.argv[1])
    else:
        GLib.idle_add(lambda: win.choose_and_scan() or False)
    Gtk.main()


if __name__ == "__main__":
    main()
