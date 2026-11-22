# LinDirStat

A fast disk-usage analyzer for Linux, inspired by WinDirStat and KDirStat.
It scans a drive or folder in parallel, shows a sortable directory tree, per-extension
statistics, and a squarified treemap, and lets you clean up what you find.

Written in Rust with [egui](https://github.com/emilk/egui) (a single native executable).

## Features

* **Fast parallel scanner**: raw `getdents64` and `statx` system calls, a work-stealing thread
  pool, and an SSD/HDD-aware thread count. Hard links are counted once, and symlinks are never followed.
* **Whole-drive scans**: scan `/` or any mounted drive. Stays on one filesystem by default.
* **Live progress**: percentage, items scanned, bytes, ETA, and a Stop button.
* **Size units**: Base 2 (KiB, MiB, …) or Base 10 (KB, MB, …), shown as Auto, Bytes, Bits,
  or any fixed unit from KiB/KB up to PiB/PB.
* **Treemap**: flat tiles with soft edge shading, plus brightness, saturation and edge sliders.
  Classic WinDirStat palette is available.
* **Hide from Treemap**: right-click any folder or file to remove it from the treemap; the rest
  expands to fill the space. Reversible with *Show on Treemap*. Cleared on rescan.
* **Largest folders**: filter by number of files inside ("at most" / "at least" a slider value),
  counting recursively or only direct files, optionally limited to the selected folder.
* **Largest files** list, with the same selected-folder option.
* **12 themes**: Catppuccin (Mocha, Macchiato, Frappé), Dracula, Nord, Gruvbox Dark,
  Tokyo Night, One Dark, Solarized Dark, plus System, Light and Dark.
* **Cleanup**: open, copy path, move to trash (via `gio trash`), or delete permanently,
  both behind a confirmation dialog.
* **Compact small files** (Options): merges tiny files into one entry per folder to reduce
  memory on huge trees.
* **Saved scans**: *File > Save Scan* (Ctrl+S) writes the complete results to a compressed
  `.ldscan` file; *File > Open Saved Scan* (Ctrl+Shift+O), the drive picker, or
  `windirstat-linux FILE.ldscan` reopens it in a fraction of a second, without touching the
  scanned drive. *Options > Auto-save every completed scan* keeps the latest scan of each folder.
  A 220,000-file tree saves to 3.4 MiB in about 40 ms.
* **Refresh selected folder** (Shift+F5 or right-click): rescans only one folder and splices it
  into the results, so a saved scan of a huge drive can be brought up to date where it changed.
* **Multi-select**: Shift+click for a range, Ctrl+click to add or remove, or drag over rows in
  the tree. The right-click menu then acts on all of them (copy paths, hide, trash, delete), and
  the status bar shows the combined size.
* **Search** (Ctrl+F): find files and folders by name or wildcard (`*.iso`, `IMG_????.jpg`),
  largest first, optionally only inside the selected folder. Right-click an extension to list
  all its files.
* **Treemap zoom**: double-click a tile to zoom into that folder; Backspace zooms out, Esc resets.
* **Remembers the window**: size, position, maximized state, interface zoom (Ctrl+= / Ctrl+- /
  Ctrl+0, or *View > Interface Zoom*) and panel sizes are restored on the next start.
* **Keyboard navigation**: arrow keys, Page Up/Down, Home/End (Shift extends the selection),
  Enter or Left/Right to collapse/expand, Del to trash, Shift+Del to delete.
* **More**: recent folders, Show in File Manager, Open Terminal Here, Copy Name, and
  *File > Export to CSV* (folders, optionally every file).

## Build and run on Linux Mint / Ubuntu / Debian

Clone the repository, then build:

```bash
git clone https://github.com/SEALCOOL13/LinDirStat.git
cd lindirstat
./build.sh            # installs prerequisites and Rust (rustup), then builds
./build.sh install    # same, plus installs to ~/.local/bin with a menu entry
```

Manual build:

```bash
sudo apt install build-essential curl pkg-config libgl1 libegl1 libxkbcommon0 \
                 libxkbcommon-x11-0 libx11-6 libxcursor1 libxrandr2 libxi6 libwayland-client0
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust >= 1.88
. "$HOME/.cargo/env"
cargo build --release
./target/release/windirstat-linux
```

The first build downloads and compiles about 300 crates (a few minutes). The result,
`target/release/windirstat-linux`, is a single executable you can copy anywhere that has
a compatible glibc.

## Usage

```bash
windirstat-linux                 # opens the drive picker
windirstat-linux /home           # scans a folder immediately
windirstat-linux scan.ldscan     # opens a saved scan
windirstat-linux --bench /usr    # headless benchmark: prints timing and totals
windirstat-linux --help
```

The folder argument also accepts `file://` URIs, a file (its folder is scanned), a device such
as `/dev/sdb1` (its mount point is scanned), and a path with spaces that a file manager split
into several arguments.

Settings are saved in `~/.config/windirstat-linux/settings-native.json`, saved scans in
`~/.local/share/windirstat-linux/scans/`.

### File manager integration (Nemo)

Copy `extras/lindirstat.nemo_action` (right-click the background of a folder) and/or
`extras/lindirstat-folder.nemo_action` (right-click a folder) to `~/.local/share/nemo/actions/`,
and change `windirstat-linux` in `Exec=` to the full path of the binary if it is not on your
`PATH`. Use `%P` (the current folder), not `%D` (the device, such as `/dev/sda1`), and do not put
quotes around it when the action also has `Quote=double`.

## Project layout

| Path | What it is |
|------|------------|
| `src/scan.rs` | Parallel scanner, compact tree model, aggregation |
| `src/treemap.rs` | Squarified layout, hide/show support, CPU painter |
| `src/app.rs` | Application state and logic (no drawing code) |
| `src/ui.rs` | All egui drawing: menu, tree, treemap, side panel, dialogs |
| `src/theme.rs` | Theme definitions and classic palette |
| `src/units.rs` | Size and time formatting, Base 2 / Base 10 |
| `src/savefile.rs` | Saving and loading scans (`.ldscan`) |
| `src/util.rs` | Mount list, free space, command-line targets, desktop integration |
| `src/main.rs` | Command-line handling and window setup |
| `extras/` | Nemo actions |
| `python-prototypes/` | Earlier Python versions, kept for reference |

## Status and known limits

* Tested on Ubuntu-based Linux under a virtual display with simulated mouse input.
  It has not been tested on a physical desktop, or on Wayland-only sessions.
* Windows is not supported. The scanner uses Linux system calls.
* Scanning is heavily I/O bound. On spinning disks, the thread count is capped at 4.

## Contributing

Issues and pull requests are welcome. Run `cargo test --release` and `cargo build --release`
before submitting. Keep changes small and tell me in the description what you tested.

## License

GPL-2.0-or-later. See [LICENSE](LICENSE). Inspired by WinDirStat (GPL) and KDirStat.
