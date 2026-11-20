mod app;
mod scan;
mod theme;
mod treemap;
mod ui;
mod units;
mod util;

use mimalloc::MiMalloc;
use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const HELP: &str = "\
WinDirStat for Linux (native, Rust)

USAGE:
  windirstat-linux [FOLDER]          open the GUI (optionally start scanning FOLDER)
  windirstat-linux --bench FOLDER    headless scan benchmark, prints timings

BENCH OPTIONS:
  --threads N      worker threads (default: auto)
  --apparent       use apparent size instead of disk usage
  --cross-fs       cross filesystem boundaries
  --compact BYTES  merge files smaller than BYTES into one entry per folder
  --no-precount    skip the background item counter
  --runs N         repeat the scan N times (default 1)
";

fn bench(args: &[String]) {
    let flag = |f: &str| args.iter().any(|a| a == f);
    let val = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let path = args
        .iter()
        .enumerate()
        .find(|(i, a)| {
            !a.starts_with("--") && *i > 0 && !matches!(args[i - 1].as_str(), "--threads" | "--compact" | "--runs")
        })
        .map(|(_, a)| a.clone())
        .unwrap_or_else(|| ".".into());
    let runs: usize = val("--runs").and_then(|v| v.parse().ok()).unwrap_or(1);
    for run in 1..=runs {
        let opts = scan::ScanOpts {
            path: path.clone(),
            one_fs: !flag("--cross-fs"),
            apparent: flag("--apparent"),
            threads: val("--threads").and_then(|v| v.parse().ok()).unwrap_or(0),
            compact_below: val("--compact").and_then(|v| v.parse().ok()).unwrap_or(0),
            precount: !flag("--no-precount"),
        };
        let prog = Arc::new(scan::Progress::default());
        let t0 = std::time::Instant::now();
        let Some(r) = scan::run_scan(opts, prog.clone()) else {
            eprintln!("cannot scan {path}");
            std::process::exit(1);
        };
        let dt = t0.elapsed().as_secs_f64();
        let entries = prog.entries.load(Relaxed);
        let d = &r.tree.dirs[0];
        println!(
            "run {run}: {path}\n  threads {}  time {:.3}s  entries {}  ({:.0} entries/s)\n  files {}  folders {}  size {}  errors {}\n  stored dirs {}  extensions {}",
            r.threads,
            dt,
            units::fmt_int(entries),
            entries as f64 / dt,
            units::fmt_int(d.nfiles),
            units::fmt_int(d.ndirs),
            units::fmt_size(d.size, units::Base::Binary, units::Unit::Auto),
            r.errors,
            units::fmt_int(r.tree.dirs.len() as u64),
            r.tree.ext_names.len()
        );
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(());
    }
    if args.iter().any(|a| a == "--bench") {
        bench(&args);
        return Ok(());
    }
    let start = args.iter().find(|a| !a.starts_with('-') && Path::new(a).is_dir()).cloned();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1500.0, 900.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("WinDirStat for Linux")
            .with_app_id("windirstat-linux"),
        ..Default::default()
    };
    eframe::run_native("WinDirStat for Linux", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, start)))))
}
