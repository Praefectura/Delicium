use std::{ffi::OsString, path::PathBuf, time::Duration};

use crate::renderer::BackendChoice;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchMode {
    Pixel,
    Interactive,
}

impl BenchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pixel => "pixel",
            Self::Interactive => "interactive",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BenchOptions {
    pub instances: usize,
    pub mode: BenchMode,
    pub seconds: Option<Duration>,
    pub reloads: Option<u32>,
}

#[derive(Debug)]
pub struct Options {
    pub models: Vec<PathBuf>,
    pub backend: BackendChoice,
    /// An explicit bounded scene size, or `None` for the ordinary launch mode
    /// where the scene covers the primary monitor. `--smoke-test` and
    /// `--bench` normalize an omitted value to the bounded 960x640 scene.
    pub overlay_size: Option<[u32; 2]>,
    pub exit_after: Option<Duration>,
    pub smoke_test: bool,
    pub bench: Option<BenchOptions>,
    /// Autonomous wandering is on for ordinary launches; `--no-wander` turns
    /// it off. `--smoke-test` always normalizes it off, and `--bench`
    /// normalizes it off unless `--bench-wander` selects the wandering
    /// workload, so the pinned workloads stay deterministic by default.
    pub wander: bool,
    /// `--bench-wander` runs the `--bench` workload with autonomous wandering
    /// enabled; it is only meaningful together with `--bench`.
    pub bench_wander: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            backend: BackendChoice::Auto,
            overlay_size: None,
            exit_after: None,
            smoke_test: false,
            bench: None,
            wander: true,
            bench_wander: false,
        }
    }
}

pub enum ParseResult {
    Run(Options),
    Help,
}

pub const HELP: &str = "Delicium - native 2D/3D desktop character prototype\n\n\
Usage: delicium [OPTIONS]\n\n\
  --model PATH            Load a self-contained .dmodel.json model (repeatable)\n\
  --backend auto|dx12|vulkan\n\
                          Select the GPU backend\n\
  --overlay-size WIDTHxHEIGHT\n\
                          Explicit bounded scene size in logical pixels\n\
                          (default: the scene covers the entire primary\n\
                          monitor; characters keep their native size)\n\
  --exit-after SECONDS     Exit after 0.1 to 600 seconds, useful for smoke tests\n\
  --smoke-test             Use bundled examples; no --model; exit >= 10 seconds\n\
  --no-wander              Disable autonomous character wandering\n\
  --bench 4|16|32|64       Run a local bundled-model benchmark\n\
  --bench-mode pixel|interactive\n\
                          Benchmark input mode (default: pixel)\n\
  --bench-seconds SECONDS  Measure for 0.1 to 300 seconds (default: 30)\n\
  --bench-reloads COUNT    Replace one model 1 to 100 times (4/pixel only)\n\
  --bench-wander           Run the benchmark with autonomous wandering enabled\n\
  -h, --help              Show this help\n\n\
Without --model, the two bundled example models are loaded.\n\
Character hit regions are enabled by default; empty scene areas pass clicks through.\n\
The control window remains interactive in either scene input mode.\n\
Ordinary launches let characters wander horizontally; --smoke-test keeps them\n\
stationary, and --bench does too unless --bench-wander is given.\n";

pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<ParseResult, String> {
    let mut args = args.into_iter();
    let mut options = Options::default();
    let mut bench_instances = None;
    let mut bench_mode = None;
    let mut bench_seconds = None;
    let mut bench_reloads = None;
    let mut bench_wander = false;
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("-h" | "--help") => return Ok(ParseResult::Help),
            Some("--model") => {
                let path = args.next().ok_or("--model requires a path")?;
                if path.is_empty() {
                    return Err("--model requires a nonempty path".into());
                }
                options.models.push(PathBuf::from(path));
                if options.models.len() > 64 {
                    return Err("At most 64 initial models may be supplied".into());
                }
            }
            Some("--backend") => {
                let backend = next_text(&mut args, "--backend")?;
                options.backend = match backend.as_str() {
                    "auto" => BackendChoice::Auto,
                    "dx12" => BackendChoice::Dx12,
                    "vulkan" => BackendChoice::Vulkan,
                    _ => return Err("--backend must be auto, dx12, or vulkan".into()),
                };
            }
            Some("--overlay-size") => {
                let size = next_text(&mut args, "--overlay-size")?;
                let (width, height) = size
                    .split_once('x')
                    .ok_or("--overlay-size must be WIDTHxHEIGHT")?;
                let width: u32 = width.parse().map_err(|_| "Invalid overlay width")?;
                let height: u32 = height.parse().map_err(|_| "Invalid overlay height")?;
                if !(320..=3840).contains(&width) || !(240..=2160).contains(&height) {
                    return Err("Overlay size must be within 320x240 and 3840x2160".into());
                }
                options.overlay_size = Some([width, height]);
            }
            Some("--exit-after") => {
                let seconds: f64 = next_text(&mut args, "--exit-after")?
                    .parse()
                    .map_err(|_| "Invalid exit duration")?;
                if !seconds.is_finite() || !(0.1..=600.0).contains(&seconds) {
                    return Err("--exit-after must be finite and within 0.1 to 600 seconds".into());
                }
                options.exit_after = Some(Duration::from_secs_f64(seconds));
            }
            Some("--smoke-test") => options.smoke_test = true,
            Some("--no-wander") => options.wander = false,
            Some("--bench-wander") => bench_wander = true,
            Some("--bench") => {
                let count = next_text(&mut args, "--bench")?
                    .parse::<usize>()
                    .map_err(|_| "Invalid benchmark instance count")?;
                if ![4, 16, 32, 64].contains(&count) {
                    return Err("--bench must be 4, 16, 32, or 64".into());
                }
                bench_instances = Some(count);
            }
            Some("--bench-mode") => {
                bench_mode = Some(match next_text(&mut args, "--bench-mode")?.as_str() {
                    "pixel" => BenchMode::Pixel,
                    "interactive" => BenchMode::Interactive,
                    _ => return Err("--bench-mode must be pixel or interactive".into()),
                });
            }
            Some("--bench-seconds") => {
                let seconds = next_text(&mut args, "--bench-seconds")?
                    .parse::<f64>()
                    .map_err(|_| "Invalid benchmark duration")?;
                if !seconds.is_finite() || !(0.1..=300.0).contains(&seconds) {
                    return Err("--bench-seconds must be finite and within 0.1 to 300".into());
                }
                bench_seconds = Some(Duration::from_secs_f64(seconds));
            }
            Some("--bench-reloads") => {
                let count = next_text(&mut args, "--bench-reloads")?
                    .parse::<u32>()
                    .map_err(|_| "Invalid benchmark replacement count")?;
                if !(1..=100).contains(&count) {
                    return Err("--bench-reloads must be within 1 to 100".into());
                }
                bench_reloads = Some(count);
            }
            _ => return Err(format!("Unknown option: {}", argument.to_string_lossy())),
        }
    }
    if let Some(instances) = bench_instances {
        if options.smoke_test || !options.models.is_empty() {
            return Err("--bench cannot be combined with --model or --smoke-test".into());
        }
        if bench_wander && !options.wander {
            return Err("--bench-wander cannot be combined with --no-wander".into());
        }
        options.bench_wander = bench_wander;
        let mode = bench_mode.unwrap_or(BenchMode::Pixel);
        if bench_reloads.is_some() {
            if bench_seconds.is_some() {
                return Err("--bench-reloads cannot be combined with --bench-seconds".into());
            }
            if instances != 4 || mode != BenchMode::Pixel {
                return Err("--bench-reloads requires --bench 4 and pixel mode".into());
            }
        }
        options.bench = Some(BenchOptions {
            instances,
            mode,
            seconds: bench_reloads
                .is_none()
                .then(|| bench_seconds.unwrap_or(Duration::from_secs(30))),
            reloads: bench_reloads,
        });
    } else if bench_mode.is_some()
        || bench_seconds.is_some()
        || bench_reloads.is_some()
        || bench_wander
    {
        return Err("Benchmark modifiers require --bench".into());
    }
    if options.smoke_test {
        if !options.models.is_empty() {
            return Err(
                "--smoke-test uses bundled examples and cannot be combined with --model".into(),
            );
        }
        if options
            .exit_after
            .is_some_and(|duration| duration < Duration::from_secs(10))
        {
            return Err("--smoke-test requires at least 10 seconds before exit".into());
        }
        if options.exit_after.is_none() {
            options.exit_after = Some(Duration::from_secs(12));
        }
    }
    // Ordinary launches span the primary monitor; the automated smoke test and
    // the benchmark workload keep the bounded 960x640 scene unless an explicit
    // size was given.
    if options.overlay_size.is_none() && (options.smoke_test || options.bench.is_some()) {
        options.overlay_size = Some([960, 640]);
    }
    // The automated smoke test keeps its characters pinned in place. The
    // benchmark workload is stationary by default and runs with wandering
    // only under --bench-wander, keeping the pinned baseline deterministic.
    // --no-wander with a plain --bench is accepted and means the same thing;
    // --no-wander with --bench-wander was rejected above.
    if options.smoke_test {
        options.wander = false;
    } else if options.bench.is_some() {
        options.wander = bench_wander;
    }
    Ok(ParseResult::Run(options))
}

fn next_text(args: &mut impl Iterator<Item = OsString>, option: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{option} requires a value"))?
        .into_string()
        .map_err(|_| format!("{option} requires a Unicode value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<ParseResult, String> {
        parse_args(args.iter().map(|argument| OsString::from(*argument)))
    }

    #[test]
    fn defaults_and_smoke_timeout() {
        let ParseResult::Run(options) = parse(&["--smoke-test"]).unwrap() else {
            panic!("expected run options");
        };
        assert_eq!(options.overlay_size, Some([960, 640]));
        assert_eq!(options.exit_after, Some(Duration::from_secs(12)));
    }

    #[test]
    fn overlay_size_presence_and_normalization() {
        // Ordinary launch: no explicit size, so the scene spans the monitor.
        let ParseResult::Run(options) = parse(&[]).unwrap() else {
            panic!("expected run options");
        };
        assert_eq!(options.overlay_size, None);
        // Explicit size keeps the bounded path, including the old default.
        let ParseResult::Run(options) = parse(&["--overlay-size", "960x640"]).unwrap() else {
            panic!("expected run options");
        };
        assert_eq!(options.overlay_size, Some([960, 640]));
        let ParseResult::Run(options) = parse(&["--overlay-size", "1707x1067"]).unwrap() else {
            panic!("expected run options");
        };
        assert_eq!(options.overlay_size, Some([1707, 1067]));
        // Smoke and bench normalize an omitted size to the bounded default.
        let ParseResult::Run(options) = parse(&["--smoke-test"]).unwrap() else {
            panic!("expected run options");
        };
        assert_eq!(options.overlay_size, Some([960, 640]));
        let ParseResult::Run(options) = parse(&["--bench", "4"]).unwrap() else {
            panic!("expected run options");
        };
        assert_eq!(options.overlay_size, Some([960, 640]));
        // An explicit size wins over the normalization in both modes.
        let ParseResult::Run(options) =
            parse(&["--smoke-test", "--overlay-size", "1280x800"]).unwrap()
        else {
            panic!("expected run options");
        };
        assert_eq!(options.overlay_size, Some([1280, 800]));
        let ParseResult::Run(options) =
            parse(&["--bench", "4", "--overlay-size", "1280x800"]).unwrap()
        else {
            panic!("expected run options");
        };
        assert_eq!(options.overlay_size, Some([1280, 800]));
    }

    #[test]
    fn smoke_rejects_external_models_and_short_timeouts() {
        for args in [
            vec!["--smoke-test", "--model", "first.json"],
            vec!["--model", "first.json", "--smoke-test"],
            vec!["--smoke-test", "--exit-after", "9.9"],
            vec!["--exit-after", "9.9", "--smoke-test"],
        ] {
            assert!(parse(&args).is_err(), "accepted {args:?}");
        }
        let ParseResult::Run(options) = parse(&["--smoke-test", "--exit-after", "10"]).unwrap()
        else {
            panic!("expected run options");
        };
        assert_eq!(options.exit_after, Some(Duration::from_secs(10)));
    }

    #[test]
    fn wander_defaults_and_automated_normalization() {
        // Ordinary launch: wandering is on.
        let ParseResult::Run(options) = parse(&[]).unwrap() else {
            panic!("expected run options");
        };
        assert!(options.wander);
        // --no-wander turns it off.
        let ParseResult::Run(options) = parse(&["--no-wander"]).unwrap() else {
            panic!("expected run options");
        };
        assert!(!options.wander);
        // Smoke and bench normalize it off.
        let ParseResult::Run(options) = parse(&["--smoke-test"]).unwrap() else {
            panic!("expected run options");
        };
        assert!(!options.wander);
        let ParseResult::Run(options) = parse(&["--bench", "4"]).unwrap() else {
            panic!("expected run options");
        };
        assert!(!options.wander);
        // Combining the flag with an automated mode is accepted.
        let ParseResult::Run(options) = parse(&["--no-wander", "--bench", "4"]).unwrap() else {
            panic!("expected run options");
        };
        assert!(!options.wander);
        // --bench-wander selects the wandering benchmark workload.
        let ParseResult::Run(options) = parse(&["--bench", "4", "--bench-wander"]).unwrap() else {
            panic!("expected run options");
        };
        assert!(options.wander);
        assert!(options.bench_wander);
    }

    #[test]
    fn wander_flag_takes_no_value() {
        for args in [
            vec!["--no-wander", "true"],
            vec!["--wander"],
            vec!["--no-wander", "--unknown"],
        ] {
            assert!(parse(&args).is_err(), "accepted {args:?}");
        }
    }

    #[test]
    fn repeated_models_and_backend() {
        let ParseResult::Run(options) = parse(&[
            "--model",
            "first.json",
            "--model",
            "second.json",
            "--backend",
            "vulkan",
        ])
        .unwrap() else {
            panic!("expected run options");
        };
        assert_eq!(options.models.len(), 2);
        assert_eq!(options.backend, BackendChoice::Vulkan);
    }

    #[test]
    fn benchmark_defaults_modes_and_bounds() {
        for count in ["4", "16", "32", "64"] {
            let ParseResult::Run(options) = parse(&["--bench", count]).unwrap() else {
                panic!("expected run")
            };
            let bench = options.bench.unwrap();
            assert_eq!(bench.instances, count.parse::<usize>().unwrap());
            assert_eq!(bench.mode, BenchMode::Pixel);
            assert_eq!(bench.seconds, Some(Duration::from_secs(30)));
            assert_eq!(bench.reloads, None);
        }
        let ParseResult::Run(options) = parse(&[
            "--bench-mode",
            "interactive",
            "--bench-seconds",
            "300",
            "--bench",
            "64",
        ])
        .unwrap() else {
            panic!("expected run")
        };
        let bench = options.bench.unwrap();
        assert_eq!(bench.mode, BenchMode::Interactive);
        assert_eq!(bench.seconds, Some(Duration::from_secs(300)));
        let ParseResult::Run(options) = parse(&["--bench", "4", "--bench-reloads", "100"]).unwrap()
        else {
            panic!("expected run")
        };
        assert_eq!(options.bench.as_ref().unwrap().seconds, None);
        assert_eq!(options.bench.unwrap().reloads, Some(100));
        // The reload workload can run with wandering too: it is rejected with
        // --no-wander but accepted on its own.
        let ParseResult::Run(options) =
            parse(&["--bench", "4", "--bench-reloads", "100", "--bench-wander"]).unwrap()
        else {
            panic!("expected run")
        };
        assert!(options.wander);
        assert!(options.bench_wander);
        assert_eq!(options.bench.unwrap().reloads, Some(100));
    }

    #[test]
    fn benchmark_rejects_ambiguous_or_invalid_workloads() {
        for args in [
            vec!["--bench"],
            vec!["--bench", "8"],
            vec!["--bench", "65"],
            vec!["--bench-mode", "pixel"],
            vec!["--bench-seconds", "30"],
            vec!["--bench-reloads", "1"],
            vec!["--bench", "4", "--bench-mode", "unknown"],
            vec!["--bench", "4", "--bench-seconds", "NaN"],
            vec!["--bench", "4", "--bench-seconds", "inf"],
            vec!["--bench", "4", "--bench-seconds", "0"],
            vec!["--bench", "4", "--bench-seconds", "301"],
            vec!["--bench", "4", "--bench-reloads", "0"],
            vec!["--bench", "4", "--bench-reloads", "101"],
            vec!["--bench", "16", "--bench-reloads", "1"],
            vec![
                "--bench",
                "4",
                "--bench-mode",
                "interactive",
                "--bench-reloads",
                "1",
            ],
            vec![
                "--bench",
                "4",
                "--bench-seconds",
                "30",
                "--bench-reloads",
                "1",
            ],
            vec!["--bench", "4", "--smoke-test"],
            vec!["--smoke-test", "--bench", "4"],
            vec!["--bench", "4", "--model", "model.json"],
            vec!["--model", "model.json", "--bench", "4"],
            vec!["--bench-wander"],
            vec!["--smoke-test", "--bench-wander"],
            vec!["--bench-wander", "true"],
            vec!["--bench", "4", "--bench-wander", "--no-wander"],
            vec!["--no-wander", "--bench", "4", "--bench-wander"],
        ] {
            assert!(parse(&args).is_err(), "accepted {args:?}");
        }
        let ParseResult::Run(options) = parse(&[
            "--bench",
            "4",
            "--bench-seconds",
            "0.1",
            "--exit-after",
            "0.1",
        ])
        .unwrap() else {
            panic!("expected run")
        };
        assert_eq!(options.exit_after, Some(Duration::from_secs_f64(0.1)));
    }

    #[test]
    fn ordinary_defaults_do_not_enable_benchmarking() {
        let ParseResult::Run(options) = parse(&[]).unwrap() else {
            panic!("expected run")
        };
        assert!(options.bench.is_none());
    }

    #[test]
    fn rejects_invalid_or_missing_values() {
        for args in [
            vec!["--model"],
            vec!["--backend", "unknown"],
            vec!["--overlay-size", "0x640"],
            vec!["--overlay-size", "4294967296x640"],
            vec!["--exit-after", "NaN"],
            vec!["--exit-after", "inf"],
            vec!["--exit-after", "-1"],
            vec!["--unknown"],
        ] {
            assert!(parse(&args).is_err(), "accepted {args:?}");
        }
    }
}
