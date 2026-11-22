mod app;
mod savefile;
mod scan;
mod theme;
mod treemap;
mod ui;
mod units;
mod util;

use mimalloc::MiMalloc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const HELP: &str = "\
WinDirStat for Linux (native, Rust)

USAGE:
  windirstat-linux [FOLDER]          open the GUI (optionally start scanning FOLDER)
  windirstat-linux FILE.ldscan       open the GUI with a saved scan
  windirstat-linux --bench FOLDER    headless scan benchmark, prints timings

BENCH OPTIONS:
  --threads N      worker threads (default: auto)
  --apparent       use apparent size instead of disk usage
  --cross-fs       cross filesystem boundaries
  --compact BYTES  merge files smaller than BYTES into one entry per folder
  --no-precount    skip the background item counter
  --runs N         repeat the scan N times (default 1)
  --save FILE      also save the result to FILE and time loading it back
";

fn bench(args: &[String]) {
    let flag = |f: &str| args.iter().any(|a| a == f);
    let val = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let path = args
        .iter()
        .enumerate()
        .find(|(i, a)| {
            !a.starts_with("--") && *i > 0 && !matches!(args[i - 1].as_str(), "--threads" | "--compact" | "--runs" | "--save")
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
        if let Some(f) = val("--save") {
            let f = std::path::PathBuf::from(f);
            let meta = savefile::Meta::from_result(&r);
            let t1 = std::time::Instant::now();
            let data = savefile::SaveData { meta: &meta, tree: &r.tree, ext_stats: &r.ext_stats, top_files: &r.top_files };
            match savefile::save(&f, &data) {
                Ok(bytes) => println!("  saved {} in {:.3}s", units::fmt_size(bytes, units::Base::Binary, units::Unit::Auto), t1.elapsed().as_secs_f64()),
                Err(e) => eprintln!("  save failed: {e}"),
            }
            let t2 = std::time::Instant::now();
            match savefile::load(&f, Arc::new(scan::Progress::default())) {
                Ok((m, _)) => println!("  loaded {} files in {:.3}s", units::fmt_int(m.files), t2.elapsed().as_secs_f64()),
                Err(e) => eprintln!("  load failed: {e}"),
            }
        }
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
    let target = util::resolve_target(&args);
    let cfg = app::Settings::load();
    let mut vp = egui::ViewportBuilder::default()
        .with_inner_size([cfg.win_w.max(400.0), cfg.win_h.max(300.0)])
        .with_min_inner_size([640.0, 400.0])
        .with_maximized(cfg.win_max)
        .with_title("WinDirStat for Linux")
        .with_app_id("windirstat-linux");
    if let Some([x, y]) = cfg.win_pos.filter(|p| p[0] >= 0.0 && p[1] >= 0.0) {
        vp = vp.with_position([x, y]);
    }
    let options = eframe::NativeOptions { viewport: vp, ..Default::default() };
    let start = target;
    eframe::run_native("WinDirStat for Linux", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, start)))))
}
