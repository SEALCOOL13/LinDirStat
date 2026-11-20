#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""
WinDirStat for Linux
====================
A native Linux take on WinDirStat: directory tree, extension list and a
cushion-shaded squarified treemap, all linked together, plus cleanup actions.

Requirements (Ubuntu):   sudo apt install python3-pyqt5 python3-numpy
Run:                     python3 windirstat_linux.py [folder]
"""
import math
import os
import re
import shutil
import stat
import subprocess
import sys
import time

try:
    import numpy as np
    from PyQt5.QtCore import Qt, QRect, QRectF, QThread, QTimer, pyqtSignal
    from PyQt5.QtGui import QBrush, QColor, QImage, QPainter, QPen, QPixmap
    from PyQt5.QtWidgets import (
        QAbstractItemView, QAction, QApplication, QDialog, QDialogButtonBox,
        QFileDialog, QLabel, QListWidget, QListWidgetItem, QMainWindow,
        QMenu, QMessageBox, QProgressBar, QPushButton, QSplitter, QStyle,
        QStyledItemDelegate, QStyleOptionViewItem, QTreeWidget,
        QTreeWidgetItem, QVBoxLayout, QWidget,
    )
except ImportError as exc:  # pragma: no cover
    sys.exit(f"Missing dependency: {exc}\n"
             "Install with:  sudo apt install python3-pyqt5 python3-numpy")

APP_NAME = "WinDirStat for Linux"
MAX_ROWS = 20000          # max rows shown per folder in the tree view


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


def ext_of(name):
    e = os.path.splitext(name)[1].lower()
    return sys.intern(e if e else "(none)")


def open_path(path):
    try:
        subprocess.Popen(["xdg-open", path], stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL)
    except OSError:
        pass


# --------------------------------------------------------------------------- #
# data model
# --------------------------------------------------------------------------- #
class Node:
    __slots__ = ("name", "parent", "is_dir", "size", "files", "dirs",
                 "mtime", "children", "ext", "agg")

    def __init__(self, name, parent, is_dir, size=0, mtime=0.0, ext=None):
        self.name = name
        self.parent = parent
        self.is_dir = is_dir
        self.size = size
        self.files = 0
        self.dirs = 0
        self.mtime = mtime
        self.children = [] if is_dir else None
        self.ext = ext
        self.agg = False

    def path(self):
        parts, n = [], self
        while n.parent is not None:
            parts.append(n.name)
            n = n.parent
        return os.path.join(n.name, *reversed(parts))


class Scanner(QThread):
    progress = pyqtSignal(int, int, str)
    finished_scan = pyqtSignal(object, object, int, bool)

    def __init__(self, path, one_fs, apparent):
        super().__init__()
        self.path, self.one_fs, self.apparent = path, one_fs, apparent
        self.cancel = False
        self.setStackSize(128 * 1024 * 1024)

    def _size(self, st):
        return st.st_size if self.apparent else st.st_blocks * 512

    def run(self):
        self.nfiles = self.nbytes = self.errors = 0
        self.seen = set()
        self.ext_stats = {}
        self._last = 0.0
        try:
            st = os.lstat(self.path)
        except OSError:
            self.finished_scan.emit(None, {}, 1, False)
            return
        self.dev = st.st_dev
        root = Node(self.path, None, True, self._size(st), st.st_mtime)
        self._walk(root, self.path)
        self.finished_scan.emit(root, self.ext_stats, self.errors, self.cancel)

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
                        key = (st.st_dev, st.st_ino)       # count hardlinks once
                        if key in self.seen:
                            size = 0
                        else:
                            self.seen.add(key)
                    f = Node(e.name, node, False, size, st.st_mtime, ext_of(e.name))
                    children.append(f)
                    node.size += size
                    node.files += 1
                    if st.st_mtime > node.mtime:
                        node.mtime = st.st_mtime
                    s = self.ext_stats.get(f.ext)
                    if s is None:
                        self.ext_stats[f.ext] = [size, 1]
                    else:
                        s[0] += size
                        s[1] += 1
                    self.nfiles += 1
                    self.nbytes += size
                    now = time.monotonic()
                    if now - self._last > 0.1:
                        self._last = now
                        self.progress.emit(self.nfiles, self.nbytes, e.path)
        children.sort(key=lambda c: c.size, reverse=True)
        if children:
            node.ext = children[0].ext      # dominant colour for tiny folders


# --------------------------------------------------------------------------- #
# colours (WinDirStat-style palette, normalised to equal brightness)
# --------------------------------------------------------------------------- #
PALETTE = [(0, 0, 255), (255, 0, 0), (0, 255, 0), (0, 255, 255),
           (255, 0, 255), (255, 255, 0), (150, 150, 255), (255, 150, 150),
           (150, 255, 150), (150, 255, 255), (255, 150, 255), (255, 255, 150)]


def normalize(color, brightness=0.6):
    r = [c / 255.0 for c in color]
    total = sum(r)
    if total == 0:
        return (0, 0, 0)
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
    return tuple(int(min(1.0, c) * 255) for c in r)


OTHER = normalize((255, 255, 255))
PALETTE_N = [normalize(c) for c in PALETTE]


def assign_colors(ext_stats):
    ranked = sorted(ext_stats.items(), key=lambda kv: kv[1][0], reverse=True)
    return {ext: PALETTE_N[i] if i < len(PALETTE_N) else OTHER
            for i, (ext, _) in enumerate(ranked)}


# --------------------------------------------------------------------------- #
# squarified treemap layout (Bruls, Huizing, van Wijk)
# --------------------------------------------------------------------------- #
def squarify(items, x, y, w, h):
    """items: [(obj, area)] sorted by area desc; returns [(obj, x, y, w, h)]."""
    out, i, n = [], 0, len(items)
    while i < n:
        short = min(w, h)
        if short <= 0:
            break
        s, first, best, j = 0.0, items[i][1], float("inf"), i
        while j < n:
            a = items[j][1]
            s2 = s + a
            worst = max(short * short * first / (s2 * s2),
                        s2 * s2 / (short * short * a))
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


# --------------------------------------------------------------------------- #
# treemap widget (cushion shading as in van Wijk & van de Wetering)
# --------------------------------------------------------------------------- #
class TreeMap(QWidget):
    clicked = pyqtSignal(object)
    rightClicked = pyqtSignal(object, object)
    hovered = pyqtSignal(object)

    HEIGHT, SCALE = 0.38, 0.91          # ridge height, per-level falloff
    AMBIENT = 0.13
    FACTOR = 0.88 / 0.6                 # brightness / palette brightness
    LIGHT = (-1.0, -1.0, 10.0)
    MIN_CUSHION = 100                   # px^2: smaller rects are shaded flat
    MIN_DIR = 36                        # px^2: smaller folders are not opened

    def __init__(self):
        super().__init__()
        self.setMouseTracking(True)
        self.setMinimumHeight(140)
        lx, ly, lz = self.LIGHT
        n = math.sqrt(lx * lx + ly * ly + lz * lz)
        self._l = (lx / n, ly / n, lz / n)
        self.root, self.colors = None, {}
        self.selected, self.highlight = None, None
        self._pix = self._base = self._ids = self._qimg = None
        self._hl_key, self._hl_pix = None, None
        self._rects, self._leaves, self._leaf_ext, self._ext_ids = {}, [None], [-1], {}
        self._dpr, self._stale = 1.0, False
        self._timer = QTimer(self)
        self._timer.setSingleShot(True)
        self._timer.setInterval(150)
        self._timer.timeout.connect(self._rerender)

    # ---- public API ----
    def set_data(self, root, colors):
        self.root, self.colors = root, colors
        self.highlight = None
        self._rerender()

    def set_selected(self, node):
        self.selected = node
        self.update()

    def set_highlight(self, ext):
        self.highlight = ext
        self.update()

    # ---- rendering ----
    def _rerender(self):
        self._stale = False
        self._render()
        self.update()

    def _render(self):
        self._rects, self._leaves, self._leaf_ext, self._ext_ids = {}, [None], [-1], {}
        self._hl_key = self._hl_pix = None
        self._dpr = self.devicePixelRatioF()
        w, h = int(self.width() * self._dpr), int(self.height() * self._dpr)
        if self.root is None or self.root.size <= 0 or w < 8 or h < 8:
            self._pix = self._base = self._ids = None
            return
        out = np.zeros((h, w, 4), np.uint8)
        out[..., :3] = 30
        out[..., 3] = 255
        ids = np.zeros((h, w), np.int32)
        self._layout(self.root, 0.0, 0.0, float(w), float(h),
                     (0.0, 0.0, 0.0, 0.0), 0, out, ids)
        self._base, self._ids = out, ids
        self._leaf_ext_arr = np.array(self._leaf_ext, np.int32)
        self._pix = self._to_pixmap(out)

    def _to_pixmap(self, arr):
        h, w = arr.shape[:2]
        self._qimg = QImage(arr.data, w, h, w * 4, QImage.Format_RGBA8888)
        pix = QPixmap.fromImage(self._qimg)
        pix.setDevicePixelRatio(self._dpr)
        return pix

    @staticmethod
    def _ridge(s, x0, y0, x1, y1, h):
        wf, hf = 4 * h / (x1 - x0), 4 * h / (y1 - y0)
        return (s[0] - wf, s[1] - hf, s[2] + wf * (x1 + x0), s[3] + hf * (y1 + y0))

    def _ext_id(self, ext):
        if ext is None:
            return -1
        return self._ext_ids.setdefault(ext, len(self._ext_ids))

    def _layout(self, node, x, y, w, h, s, level, out, ids):
        self._rects[node] = (int(x + 0.5), int(y + 0.5),
                             int(x + w + 0.5), int(y + h + 0.5))
        s = self._ridge(s, x, y, x + w, y + h, self.HEIGHT * self.SCALE ** level)
        kids = [c for c in node.children if c.size > 0]
        if not kids:
            return
        total = float(sum(c.size for c in kids))
        area = w * h
        items, cut = [], len(kids)
        for i, c in enumerate(kids):                    # sorted by size desc
            a = c.size / total * area
            if a < 2.0:
                cut = i
                break
            items.append((c, a))
        if cut < len(kids):
            small = sum(c.size for c in kids[cut:])
            agg = Node(f"({len(kids) - cut:,} small items)", node, False, small)
            agg.agg = True
            items.append((agg, small / total * area))
            items.sort(key=lambda t: -t[1])
        for c, cx, cy, cw, ch in squarify(items, x, y, w, h):
            if c.is_dir and c.children and cw * ch >= self.MIN_DIR:
                self._layout(c, cx, cy, cw, ch, s, level + 1, out, ids)
            else:
                self._leaf(c, cx, cy, cw, ch, s, level + 1, out, ids)

    def _leaf(self, c, x, y, w, h, s, level, out, ids):
        x0, y0 = int(x + 0.5), int(y + 0.5)
        x1, y1 = int(x + w + 0.5), int(y + h + 0.5)
        if x1 <= x0 or y1 <= y0:
            return
        s = self._ridge(s, x, y, x + w, y + h, self.HEIGHT * self.SCALE ** level)
        col = self.colors.get(c.ext, OTHER) if c.ext else OTHER
        self._rects[c] = (x0, y0, x1, y1)
        self._leaves.append(c)
        self._leaf_ext.append(self._ext_id(c.ext))
        ids[y0:y1, x0:x1] = len(self._leaves) - 1
        ia, lx, ly, lz = self.AMBIENT, *self._l
        if (x1 - x0) * (y1 - y0) < self.MIN_CUSHION:     # cheap flat shading
            px, py = (x0 + x1) / 2.0, (y0 + y1) / 2.0
            nx = -(2 * s[0] * px + s[2])
            ny = -(2 * s[1] * py + s[3])
            cosa = (nx * lx + ny * ly + lz) / math.sqrt(nx * nx + ny * ny + 1.0)
            shade = (min(1.0, max(0.0, cosa)) * (1 - ia) + ia) * self.FACTOR
            out[y0:y1, x0:x1, :3] = [min(255, int(k * shade)) for k in col]
        else:                                            # full cushion
            xs = np.arange(x0, x1, dtype=np.float32) + 0.5
            ys = np.arange(y0, y1, dtype=np.float32) + 0.5
            nx = (-(2 * s[0] * xs + s[2]))[None, :]
            ny = (-(2 * s[1] * ys + s[3]))[:, None]
            cosa = (nx * lx + ny * ly + lz) / np.sqrt(nx * nx + ny * ny + 1.0)
            shade = (np.clip(cosa, 0, 1) * (1 - ia) + ia) * self.FACTOR
            block = shade[..., None] * np.array(col, np.float32)
            out[y0:y1, x0:x1, :3] = np.minimum(block, 255).astype(np.uint8)

    # ---- painting ----
    def _current_pix(self):
        if self.highlight is None or self._base is None:
            return self._pix
        if self._hl_key != self.highlight:
            eid = self._ext_ids.get(self.highlight, -2)
            mask = self._leaf_ext_arr[self._ids] == eid
            img = self._base.copy()
            img[..., :3] = np.where(mask[..., None], self._base[..., :3],
                                    (self._base[..., :3] * 0.3).astype(np.uint8))
            self._hl_pix, self._hl_key = self._to_pixmap(img), self.highlight
        return self._hl_pix

    def paintEvent(self, _ev):
        p = QPainter(self)
        p.fillRect(self.rect(), QColor(30, 30, 30))
        pix = self._current_pix()
        if pix is None:
            p.setPen(QColor(150, 150, 150))
            p.drawText(self.rect(), Qt.AlignCenter,
                       "Nothing scanned yet — use File ▸ Scan…" if self.root is None else "")
            return
        p.drawPixmap(self.rect(), pix) if self._stale else p.drawPixmap(0, 0, pix)
        n = self.selected
        while n is not None and n not in self._rects:
            n = n.parent
        if n is not None and not self._stale:
            x0, y0, x1, y1 = self._rects[n]
            d = self._dpr
            p.setPen(QPen(QColor(255, 255, 255), 2))
            p.setBrush(Qt.NoBrush)
            p.drawRect(QRectF(x0 / d + 1, y0 / d + 1, (x1 - x0) / d - 2, (y1 - y0) / d - 2))

    def resizeEvent(self, _ev):
        if self.root is not None:
            self._stale = True
            self._timer.start()

    # ---- mouse ----
    def _node_at(self, pos):
        if self._ids is None:
            return None
        x, y = int(pos.x() * self._dpr), int(pos.y() * self._dpr)
        if 0 <= y < self._ids.shape[0] and 0 <= x < self._ids.shape[1]:
            n = self._leaves[self._ids[y, x]]
            if n is not None and n.agg:
                n = n.parent
            return n
        return None

    def mousePressEvent(self, ev):
        n = self._node_at(ev.pos())
        if n is None:
            return
        if ev.button() == Qt.LeftButton:
            self.clicked.emit(n)
        elif ev.button() == Qt.RightButton:
            self.clicked.emit(n)
            self.rightClicked.emit(n, ev.globalPos())

    def mouseMoveEvent(self, ev):
        self.hovered.emit(self._node_at(ev.pos()))

    def leaveEvent(self, _ev):
        self.hovered.emit(None)


# --------------------------------------------------------------------------- #
# tree / list widgets
# --------------------------------------------------------------------------- #
class KeyItem(QTreeWidgetItem):
    """Tree item that sorts on a per-column numeric/str key."""

    def __init__(self):
        super().__init__()
        self.keys = []

    def __lt__(self, other):
        c = self.treeWidget().sortColumn()
        try:
            return self.keys[c] < other.keys[c]
        except (IndexError, TypeError, AttributeError):
            return False


class DirItem(KeyItem):
    ICON_DIR = ICON_FILE = None

    def __init__(self, node, parent_size=0):
        super().__init__()
        self.node, self.populated = node, False
        d = node.is_dir
        frac = node.size / parent_size if parent_size > 0 else 1.0
        items = node.files + node.dirs if d else 0
        self.keys = [node.name.lower(), frac, frac, node.size, items,
                     node.files if d else 0, node.dirs if d else 0, node.mtime]
        self.setText(0, node.name)
        self.setIcon(0, self.ICON_DIR if d else self.ICON_FILE)
        self.setData(1, Qt.UserRole, frac)
        self.setText(2, f"{frac * 100:.1f}%")
        self.setText(3, human(node.size))
        if d:
            self.setText(4, f"{items:,}")
            self.setText(5, f"{node.files:,}")
            self.setText(6, f"{node.dirs:,}")
        self.setText(7, fmt_time(node.mtime))
        for c in (2, 3, 4, 5, 6):
            self.setTextAlignment(c, Qt.AlignRight | Qt.AlignVCenter)
        if d and node.children:
            self.setChildIndicatorPolicy(QTreeWidgetItem.ShowIndicator)
            self.addChild(QTreeWidgetItem(["…"]))


class BarDelegate(QStyledItemDelegate):
    """Draws the 'Subtree Percentage' bar."""

    def paint(self, painter, option, index):
        opt = QStyleOptionViewItem(option)
        self.initStyleOption(opt, index)
        opt.text = ""
        style = option.widget.style() if option.widget else QApplication.style()
        style.drawControl(QStyle.CE_ItemViewItem, opt, painter, option.widget)
        frac = index.data(Qt.UserRole)
        if frac is None:
            return
        r = option.rect.adjusted(3, 3, -3, -3)
        painter.save()
        painter.setPen(QColor(130, 130, 130))
        painter.setBrush(Qt.NoBrush)
        painter.drawRect(r)
        painter.fillRect(QRect(r.left() + 1, r.top() + 1,
                               int((r.width() - 1) * frac), r.height() - 1),
                         QColor(70, 130, 220))
        painter.restore()


# --------------------------------------------------------------------------- #
# "what to scan" dialog
# --------------------------------------------------------------------------- #
PSEUDO_FS = {"proc", "sysfs", "devtmpfs", "devpts", "tmpfs", "cgroup", "cgroup2",
             "securityfs", "pstore", "bpf", "autofs", "mqueue", "hugetlbfs",
             "debugfs", "tracefs", "fusectl", "configfs", "binfmt_misc",
             "squashfs", "ramfs", "efivarfs", "nsfs", "overlay", "fuse.portal",
             "fuse.gvfsd-fuse", "rpc_pipefs", "selinuxfs"}


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
        if total <= 0:
            continue
        seen.add(mnt)
        res.append((mnt, f"{mnt}    —    {human(total)} total, {human(free)} free  [{fs}]"))
    return res


class DriveDialog(QDialog):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setWindowTitle("Select what to scan")
        self.resize(620, 360)
        self.chosen = None
        lay = QVBoxLayout(self)
        lay.addWidget(QLabel("Pick a mounted drive, your home folder, or browse for a folder:"))
        self.list = QListWidget()
        lay.addWidget(self.list)
        home = os.path.expanduser("~")
        for path, text in [(home, f"Home folder    —    {home}")] + list_mounts():
            it = QListWidgetItem(text)
            it.setData(Qt.UserRole, path)
            self.list.addItem(it)
        self.list.setCurrentRow(0)
        self.list.itemDoubleClicked.connect(lambda _i: self.accept())
        bb = QDialogButtonBox(QDialogButtonBox.Ok | QDialogButtonBox.Cancel)
        browse = QPushButton("Browse folder…")
        bb.addButton(browse, QDialogButtonBox.ActionRole)
        browse.clicked.connect(self._browse)
        bb.accepted.connect(self.accept)
        bb.rejected.connect(self.reject)
        lay.addWidget(bb)

    def _browse(self):
        d = QFileDialog.getExistingDirectory(self, "Folder to scan", os.path.expanduser("~"))
        if d:
            self.chosen = d
            super().accept()

    def accept(self):
        it = self.list.currentItem()
        if it is not None:
            self.chosen = it.data(Qt.UserRole)
        super().accept()


# --------------------------------------------------------------------------- #
# main window
# --------------------------------------------------------------------------- #
class MainWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle(APP_NAME)
        self.resize(1280, 820)
        st = self.style()
        DirItem.ICON_DIR = st.standardIcon(QStyle.SP_DirIcon)
        DirItem.ICON_FILE = st.standardIcon(QStyle.SP_FileIcon)
        self.root = self.scanner = self.scan_path = None
        self.ext_stats, self.colors, self._t0 = {}, {}, 0.0

        # directory tree
        self.tree = QTreeWidget()
        self.tree.setHeaderLabels(["Name", "Subtree Percentage", "Percentage", "Size",
                                   "Items", "Files", "Subdirs", "Last Change"])
        self.tree.setUniformRowHeights(True)
        self.tree.setSortingEnabled(True)
        self.tree.sortByColumn(3, Qt.DescendingOrder)
        self.tree.setItemDelegateForColumn(1, BarDelegate(self.tree))
        self.tree.setContextMenuPolicy(Qt.CustomContextMenu)
        for i, wd in enumerate((330, 150, 95, 90, 80, 80, 75, 135)):
            self.tree.header().resizeSection(i, wd)
        self.tree.itemExpanded.connect(self._populate)
        self.tree.currentItemChanged.connect(self._tree_selected)
        self.tree.customContextMenuRequested.connect(self._tree_menu)

        # extension list
        self.exts = QTreeWidget()
        self.exts.setHeaderLabels(["Extension", "Color", "Bytes", "% Bytes", "Files"])
        self.exts.setRootIsDecorated(False)
        self.exts.setUniformRowHeights(True)
        self.exts.setSortingEnabled(True)
        self.exts.sortByColumn(2, Qt.DescendingOrder)
        for i, wd in enumerate((90, 55, 95, 75, 65)):
            self.exts.header().resizeSection(i, wd)
        self.exts.currentItemChanged.connect(self._ext_selected)

        # treemap
        self.treemap = TreeMap()
        self.treemap.clicked.connect(self.select_node)
        self.treemap.rightClicked.connect(lambda n, pos: self._menu(n).exec_(pos))
        self.treemap.hovered.connect(self._hover)

        top = QSplitter(Qt.Horizontal)
        top.addWidget(self.tree)
        top.addWidget(self.exts)
        top.setSizes([900, 380])
        split = QSplitter(Qt.Vertical)
        split.addWidget(top)
        split.addWidget(self.treemap)
        split.setSizes([380, 420])
        self.setCentralWidget(split)

        # status bar
        self.sel_label, self.free_label = QLabel(""), QLabel("")
        self.busy = QProgressBar()
        self.busy.setRange(0, 0)
        self.busy.setMaximumWidth(120)
        self.busy.hide()
        self.statusBar().addPermanentWidget(self.sel_label, 1)
        self.statusBar().addPermanentWidget(self.busy)
        self.statusBar().addPermanentWidget(self.free_label)

        self._build_menus(top)

    def _build_menus(self, top_splitter):
        mb = self.menuBar()
        f = mb.addMenu("&File")
        self.act_scan = f.addAction("&Scan…", self.choose_and_scan, "Ctrl+O")
        self.act_rescan = f.addAction("&Rescan", self.rescan, "F5")
        self.act_stop = f.addAction("S&top", self.stop_scan, "Esc")
        self.act_stop.setEnabled(False)
        f.addSeparator()
        f.addAction("&Quit", self.close, "Ctrl+Q")

        o = mb.addMenu("&Options")
        self.act_onefs = o.addAction("Stay on one filesystem")
        self.act_onefs.setCheckable(True)
        self.act_onefs.setChecked(True)
        self.act_apparent = o.addAction("Use apparent size (instead of disk usage)")
        self.act_apparent.setCheckable(True)

        v = mb.addMenu("&View")
        for text, widget in (("Extension list", self.exts), ("Treemap", self.treemap)):
            a = v.addAction(text)
            a.setCheckable(True)
            a.setChecked(True)
            a.toggled.connect(widget.setVisible)

        h = mb.addMenu("&Help")
        h.addAction("&About", lambda: QMessageBox.about(
            self, APP_NAME,
            "<b>WinDirStat for Linux</b><br>Directory tree, extension statistics and a "
            "cushion treemap for disk usage.<br><br>Inspired by WinDirStat and "
            "KDirStat. Licensed GPL-2.0-or-later."))

    # ---- scanning ----
    def choose_and_scan(self):
        dlg = DriveDialog(self)
        if dlg.exec_() and dlg.chosen:
            self.start_scan(dlg.chosen)

    def rescan(self):
        if self.scan_path:
            self.start_scan(self.scan_path)

    def stop_scan(self):
        if self.scanner and self.scanner.isRunning():
            self.scanner.cancel = True

    def start_scan(self, path):
        if self.scanner and self.scanner.isRunning():
            return
        self.scan_path = os.path.abspath(path)
        self.setWindowTitle(f"{self.scan_path} — {APP_NAME}")
        self.scanner = Scanner(self.scan_path, self.act_onefs.isChecked(),
                               self.act_apparent.isChecked())
        self.scanner.progress.connect(self._on_progress)
        self.scanner.finished_scan.connect(self._on_done)
        self._t0 = time.monotonic()
        self.busy.show()
        self.act_stop.setEnabled(True)
        self.act_scan.setEnabled(False)
        self.act_rescan.setEnabled(False)
        self.scanner.start()

    def _on_progress(self, nfiles, nbytes, path):
        if len(path) > 90:
            path = "…" + path[-88:]
        self.statusBar().showMessage(f"Scanning…  {nfiles:,} files · {human(nbytes)} · {path}")

    def _on_done(self, root, ext_stats, errors, cancelled):
        self.busy.hide()
        self.act_stop.setEnabled(False)
        self.act_scan.setEnabled(True)
        self.act_rescan.setEnabled(True)
        if root is None:
            QMessageBox.warning(self, APP_NAME, f"Cannot read {self.scan_path}")
            return
        self.root, self.ext_stats = root, ext_stats
        self.colors = assign_colors(ext_stats)
        self._rebuild_views(root)
        try:
            v = os.statvfs(self.scan_path)
            self.free_label.setText(
                f"Free: {human(v.f_bavail * v.f_frsize)} of {human(v.f_blocks * v.f_frsize)}  ")
        except OSError:
            self.free_label.setText("")
        msg = (f"{'Scan stopped — partial results: ' if cancelled else 'Scanned '}"
               f"{root.files:,} files in {root.dirs:,} folders, {human(root.size)}, "
               f"{time.monotonic() - self._t0:.1f}s")
        if errors:
            msg += f"  ({errors:,} items could not be read)"
        self.statusBar().showMessage(msg)

    # ---- views ----
    def _rebuild_views(self, select):
        self.tree.clear()
        item = DirItem(self.root)
        self.tree.addTopLevelItem(item)
        item.setExpanded(True)
        total = sum(v[0] for v in self.ext_stats.values()) or 1
        rows = []
        for ext, (b, n) in sorted(self.ext_stats.items(), key=lambda kv: -kv[1][0])[:3000]:
            it = KeyItem()
            it.ext = ext
            it.keys = [ext, 0, b, b / total, n]
            it.setText(0, ext)
            it.setBackground(1, QBrush(QColor(*self.colors.get(ext, OTHER))))
            it.setText(2, human(b))
            it.setText(3, f"{b / total * 100:.1f}%")
            it.setText(4, f"{n:,}")
            for c in (2, 3, 4):
                it.setTextAlignment(c, Qt.AlignRight | Qt.AlignVCenter)
            rows.append(it)
        self.exts.clear()
        self.exts.addTopLevelItems(rows)
        self.treemap.set_data(self.root, self.colors)
        self.select_node(select)

    def _populate(self, item):
        if not isinstance(item, DirItem) or item.populated:
            return
        item.populated = True
        item.takeChildren()
        node = item.node
        shown = node.children[:MAX_ROWS]
        rows = [DirItem(c, node.size) for c in shown]
        extra = len(node.children) - len(shown)
        if extra > 0:
            more = KeyItem()
            more.keys = ["\uffff"] + [-1] * 7
            more.setText(0, f"… {extra:,} smaller items not shown")
            rows.append(more)
        item.addChildren(rows)
        item.sortChildren(self.tree.sortColumn(), self.tree.header().sortIndicatorOrder())

    def select_node(self, node):
        if node is None or self.tree.topLevelItemCount() == 0:
            return
        chain, n = [], node
        while n is not None:
            chain.append(n)
            n = n.parent
        chain.reverse()
        item = self.tree.topLevelItem(0)
        if item.node is not chain[0]:
            return
        for n in chain[1:]:
            item.setExpanded(True)
            nxt = None
            for i in range(item.childCount()):
                c = item.child(i)
                if getattr(c, "node", None) is n:
                    nxt = c
                    break
            if nxt is None:
                break
            item = nxt
        self.tree.setCurrentItem(item)
        self.tree.scrollToItem(item, QAbstractItemView.PositionAtCenter)

    def _tree_selected(self, item, _prev):
        node = getattr(item, "node", None)
        self.treemap.set_selected(node)
        if node is None:
            self.sel_label.setText("")
            return
        pct = node.size / self.root.size * 100 if self.root and self.root.size else 100
        text = f"{node.path()}  —  {human(node.size)} ({pct:.1f}% of scan)"
        if node.is_dir:
            text += f", {node.files:,} files, {node.dirs:,} folders"
        self.sel_label.setText(text)

    def _ext_selected(self, item, _prev):
        self.treemap.set_highlight(getattr(item, "ext", None))

    def _hover(self, node):
        if node is None:
            self.statusBar().clearMessage()
        else:
            self.statusBar().showMessage(f"{node.path()}  —  {human(node.size)}")

    # ---- cleanup actions ----
    def _tree_menu(self, pos):
        item = self.tree.itemAt(pos)
        node = getattr(item, "node", None)
        if node is not None:
            self._menu(node).exec_(self.tree.viewport().mapToGlobal(pos))

    def _menu(self, node):
        m = QMenu(self)
        path = node.path()
        if node.is_dir:
            m.addAction("Open Folder", lambda: open_path(path))
        else:
            m.addAction("Open File", lambda: open_path(path))
            m.addAction("Open Containing Folder", lambda: open_path(os.path.dirname(path)))
        m.addAction("Copy Path", lambda: QApplication.clipboard().setText(path))
        m.addSeparator()
        can = node.parent is not None
        m.addAction("Move to Trash…", lambda: self._trash(node)).setEnabled(can)
        m.addAction("Delete Permanently…", lambda: self._delete(node)).setEnabled(can)
        return m

    def _confirm(self, title, node, extra=""):
        box = QMessageBox(QMessageBox.Question, title,
                          f"{node.path()}\n\n{human(node.size)}"
                          f"{', ' + format(node.files, ',') + ' files' if node.is_dir else ''}{extra}",
                          QMessageBox.Yes | QMessageBox.No, self)
        box.setDefaultButton(QMessageBox.No)
        return box.exec_() == QMessageBox.Yes

    def _trash(self, node):
        if not self._confirm("Move to Trash?", node):
            return
        try:
            subprocess.run(["gio", "trash", node.path()], check=True, capture_output=True)
        except (OSError, subprocess.CalledProcessError) as e:
            QMessageBox.warning(self, APP_NAME, f"Could not move to trash:\n{e}")
            return
        self._remove_node(node)

    def _delete(self, node):
        if not self._confirm("Delete permanently?", node, "\n\nThis cannot be undone."):
            return
        path = node.path()
        try:
            if node.is_dir:
                shutil.rmtree(path)
            else:
                os.remove(path)
        except OSError as e:
            QMessageBox.warning(self, APP_NAME, f"Could not delete:\n{e}")
        else:
            self._remove_node(node)
            return
        self.rescan()

    def _remove_node(self, node):
        parent, stack = node.parent, [node]
        while stack:
            n = stack.pop()
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
            a = a.parent
        parent.children.remove(node)
        self.ext_stats = {k: v for k, v in self.ext_stats.items() if v[1] > 0}
        self._rebuild_views(parent)

    def closeEvent(self, ev):
        if self.scanner and self.scanner.isRunning():
            self.scanner.cancel = True
            self.scanner.wait(5000)
        ev.accept()


def main():
    sys.setrecursionlimit(20000)
    app = QApplication(sys.argv)
    app.setApplicationName(APP_NAME)
    win = MainWindow()
    win.show()
    if len(sys.argv) > 1 and os.path.isdir(sys.argv[1]):
        win.start_scan(sys.argv[1])
    else:
        QTimer.singleShot(0, win.choose_and_scan)
    sys.exit(app.exec_())


if __name__ == "__main__":
    main()
