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

## Build and run on Linux Mint / Ubuntu / Debian

Clone the repository, then build:

```bash
git clone https://github.com/SEALCOOL13/lindirstat.git
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
windirstat-linux --bench /usr    # headless benchmark: prints timing and totals
windirstat-linux --help
```

Settings are saved in `~/.config/windirstat-linux/settings-native.json`.

## Project layout

| Path | What it is |
|------|------------|
| `src/scan.rs` | Parallel scanner, compact tree model, aggregation |
| `src/treemap.rs` | Squarified layout, hide/show support, CPU painter |
| `src/app.rs` | Application state and logic (no drawing code) |
| `src/ui.rs` | All egui drawing: menu, tree, treemap, side panel, dialogs |
| `src/theme.rs` | Theme definitions and classic palette |
| `src/units.rs` | Size and time formatting, Base 2 / Base 10 |
| `src/util.rs` | Mount list, free space, opening files |
| `src/main.rs` | Command-line handling and window setup |
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
