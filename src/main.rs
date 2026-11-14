//! circular-scroll: ring-style circular scrolling for round touchpads on Linux.
//!
//! Grabs the physical touchpad and re-emits its events on a virtual clone, so
//! normal pointer, tap and gesture behaviour is untouched. A single-finger
//! touch that starts in the outer ring of the pad is taken over: moving around
//! the ring emits mouse-wheel events instead (clockwise = scroll down), until
//! the finger lifts.

mod config;
mod device;
mod scroll;

use std::{
    io::{self, BufRead, IsTerminal, Write},
    path::PathBuf,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use config::Config;
use device::Pad;
use evdev::{AbsoluteAxisCode, EventType, KeyCode, SynchronizationCode};
use scroll::{Geometry, Router, Scroller, Settings};

const USAGE: &str = "\
circular-scroll: ring-style circular scrolling for round touchpads on Linux

usage (as root):
  circular-scroll                          run the daemon
  circular-scroll --debug                  run the daemon with verbose logging
  circular-scroll --calibrate 15           trace the pad edge for 15 s, print geometry
  circular-scroll --calibrate 15 --write   ...and save it to the config file
  circular-scroll --list                   list input devices that look like touchpads

options:
  --config PATH   settings file (default /etc/circular-scroll.toml)
  --version       print the version
  -h, --help      show this help";

struct Args {
    calibrate: Option<u64>,
    write: bool,
    list: bool,
    debug: bool,
    config: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        calibrate: None,
        write: false,
        list: false,
        debug: false,
        config: config::DEFAULT_PATH.into(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |what: &str| {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or(format!("{flag} needs {what}"))
        };
        match flag.as_str() {
            "--calibrate" => {
                let v = value("a number of seconds")?;
                a.calibrate = Some(
                    v.parse()
                        .ok()
                        .filter(|s| *s > 0)
                        .ok_or(format!("bad --calibrate value: {v}"))?,
                );
            }
            "--config" => a.config = value("a path")?.into(),
            "--write" => a.write = true,
            "--list" => a.list = true,
            "--debug" => a.debug = true,
            "--version" | "-V" => {
                println!("circular-scroll {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {arg}\n\n{USAGE}")),
        }
    }
    if a.write && a.calibrate.is_none() {
        return Err("--write only works with --calibrate".into());
    }
    Ok(a)
}

fn main() -> ExitCode {
    let result = parse_args().and_then(|args| {
        let cfg = Config::load(&args.config)?;
        if args.list {
            list();
            Ok(())
        } else if let Some(secs) = args.calibrate {
            calibrate(cfg, &args, secs)
        } else {
            run(&cfg, args.debug)
        }
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn list() {
    let pads = device::touchpads();
    if pads.is_empty() {
        println!("No touchpads found (are you root?)");
    }
    for p in pads {
        match (
            p.absinfo(AbsoluteAxisCode::ABS_X),
            p.absinfo(AbsoluteAxisCode::ABS_Y),
        ) {
            (Ok(ax), Ok(ay)) => println!(
                "{}: \"{}\"  X {}..{}  Y {}..{}",
                p.path.display(),
                p.name(),
                ax.minimum(),
                ax.maximum(),
                ay.minimum(),
                ay.maximum()
            ),
            _ => println!("{}: \"{}\"", p.path.display(), p.name()),
        }
    }
}

fn geometry(pad: &Pad, cfg: &Config) -> Result<Geometry, String> {
    let ax = pad
        .absinfo(AbsoluteAxisCode::ABS_X)
        .map_err(|e| e.to_string())?;
    let ay = pad
        .absinfo(AbsoluteAxisCode::ABS_Y)
        .map_err(|e| e.to_string())?;
    let or = |v: f64, fallback: f64| if v != 0.0 { v } else { fallback };
    let g = Geometry {
        cx: or(cfg.center_x, (ax.minimum() + ax.maximum()) as f64 / 2.0),
        cy: or(cfg.center_y, (ay.minimum() + ay.maximum()) as f64 / 2.0),
        rx: or(cfg.radius_x, (ax.maximum() - ax.minimum()) as f64 / 2.0),
        ry: or(cfg.radius_y, (ay.maximum() - ay.minimum()) as f64 / 2.0),
    };
    if g.rx <= 0.0 || g.ry <= 0.0 {
        return Err("touchpad reports no usable range; run calibration".into());
    }
    Ok(g)
}

// --------------------------------------------------------------------------- daemon

fn err(what: &'static str) -> impl Fn(io::Error) -> String {
    move |e| format!("{what}: {e}")
}

fn run(cfg: &Config, debug: bool) -> Result<(), String> {
    let mut pad = device::find_touchpad(&cfg.device_name, true)?;
    let geom = geometry(&pad, cfg)?;
    println!(
        "circular-scroll: using {} ({}) center=({:.0},{:.0}) radius=({:.0},{:.0})",
        pad.name(),
        pad.path.display(),
        geom.cx,
        geom.cy,
        geom.rx,
        geom.ry
    );

    let mut clone = device::clone_of(&pad)
        .map_err(err("cannot create virtual touchpad (is uinput loaded?)"))?;
    let mut wheel = device::wheel().map_err(err("cannot create virtual wheel"))?;
    let settings = Settings {
        ring_start: cfg.ring_start,
        degrees_per_notch: cfg.degrees_per_notch,
        reverse: cfg.reverse,
        min_track_radius: cfg.min_track_radius,
    };
    let mut router = Router::new(Scroller::new(geom, settings));

    // The grab is released automatically when the process exits, however it exits.
    pad.dev.grab().map_err(err("cannot grab touchpad"))?;
    loop {
        let events: Vec<_> = pad
            .dev
            .fetch_events()
            .map_err(err("touchpad read failed"))?
            .collect();
        for ev in events {
            let Some(out) = router.event(ev) else {
                continue;
            };
            if let Some(frame) = out.clone {
                clone
                    .emit(&frame)
                    .map_err(err("virtual touchpad write failed"))?;
            }
            if let Some(w) = out.wheel {
                wheel
                    .emit(&w.events())
                    .map_err(err("virtual wheel write failed"))?;
            }
            if debug && let Some(note) = out.note {
                eprintln!("{note}");
            }
        }
    }
}

// --------------------------------------------------------------------------- calibrate

fn calibrate(mut cfg: Config, args: &Args, seconds: u64) -> Result<(), String> {
    let mut pad = device::find_touchpad(&cfg.device_name, false)?;
    println!("Touchpad: {} ({})", pad.name(), pad.path.display());
    let ax = pad
        .absinfo(AbsoluteAxisCode::ABS_X)
        .map_err(|e| e.to_string())?;
    let ay = pad
        .absinfo(AbsoluteAxisCode::ABS_Y)
        .map_err(|e| e.to_string())?;
    println!(
        "Reported range: X {}..{}  Y {}..{}\n",
        ax.minimum(),
        ax.maximum(),
        ay.minimum(),
        ay.maximum()
    );
    println!("Trace slowly around the VERY EDGE of the touchpad, lap after lap,");
    println!("until the timer runs out ({seconds} s).");
    if io::stdin().is_terminal() {
        print!("Press Enter when ready... ");
        io::stdout().flush().ok();
        io::stdin().lock().read_line(&mut String::new()).ok();
    }
    for n in [3, 2, 1] {
        println!("  {n}...");
        thread::sleep(Duration::from_secs(1));
    }
    println!("  GO - trace the edge now!");

    let io_err = |e: io::Error| e.to_string();
    // Discard events queued before the start.
    while pad.wait_readable(0).map_err(io_err)? {
        pad.dev.fetch_events().map_err(io_err)?.for_each(drop);
    }
    // Seed state from the device: if a finger landed during the countdown, its
    // touch/position events were just discarded and won't be re-sent.
    let abs = pad.dev.get_abs_state().map_err(io_err)?;
    let (mut x, mut y) = (
        abs[AbsoluteAxisCode::ABS_X.0 as usize].value,
        abs[AbsoluteAxisCode::ABS_Y.0 as usize].value,
    );
    let mut touching = pad
        .dev
        .get_key_state()
        .map_err(io_err)?
        .contains(KeyCode::BTN_TOUCH);

    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut last_shown = None;
    while let Some(left) = end
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
    {
        let left = left.as_secs() + 1;
        if last_shown != Some(left) {
            print!("\r  {left:2} s left, {} samples ", xs.len());
            io::stdout().flush().ok();
            last_shown = Some(left);
        }
        if !pad.wait_readable(200).map_err(io_err)? {
            continue;
        }
        for ev in pad.dev.fetch_events().map_err(io_err)? {
            match (ev.event_type(), ev.code()) {
                (EventType::ABSOLUTE, c) if c == AbsoluteAxisCode::ABS_X.0 => x = ev.value(),
                (EventType::ABSOLUTE, c) if c == AbsoluteAxisCode::ABS_Y.0 => y = ev.value(),
                (EventType::KEY, c) if c == KeyCode::BTN_TOUCH.0 => touching = ev.value() != 0,
                (EventType::SYNCHRONIZATION, c)
                    if c == SynchronizationCode::SYN_REPORT.0 && touching =>
                {
                    xs.push(x);
                    ys.push(y);
                }
                _ => {}
            }
        }
    }
    println!();
    if xs.len() < 200 {
        return Err(format!(
            "only {} samples captured - run it again and keep tracing the edge the whole time",
            xs.len()
        ));
    }
    // Use percentiles to reject outliers/noise at the extremes.
    xs.sort_unstable();
    ys.sort_unstable();
    let p = |v: &[i32], q: f64| v[((q * v.len() as f64) as usize).min(v.len() - 1)];
    let (xmin, xmax, ymin, ymax) = (p(&xs, 0.01), p(&xs, 0.99), p(&ys, 0.01), p(&ys, 0.99));
    cfg.device_name = pad.name().to_string();
    cfg.center_x = ((xmin + xmax) as f64 / 2.0).round();
    cfg.center_y = ((ymin + ymax) as f64 / 2.0).round();
    cfg.radius_x = ((xmax - xmin) as f64 / 2.0).round();
    cfg.radius_y = ((ymax - ymin) as f64 / 2.0).round();
    println!(
        "\nSamples: {}  observed X {xmin}..{xmax}  Y {ymin}..{ymax}",
        xs.len()
    );
    for key in [
        "device_name",
        "center_x",
        "center_y",
        "radius_x",
        "radius_y",
    ] {
        println!("{}", cfg.line(key));
    }
    if args.write {
        cfg.save(&args.config)
            .map_err(|e| format!("cannot write {}: {e}", args.config.display()))?;
        println!("Saved to {}", args.config.display());
    }
    Ok(())
}
