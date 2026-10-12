use std::{ffi::OsString, path::PathBuf, time::Duration};

use crate::renderer::BackendChoice;

#[derive(Debug)]
pub struct Options {
    pub models: Vec<PathBuf>,
    pub backend: BackendChoice,
    pub overlay_size: [u32; 2],
    pub exit_after: Option<Duration>,
    pub smoke_test: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            backend: BackendChoice::Auto,
            overlay_size: [960, 640],
            exit_after: None,
            smoke_test: false,
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
                          Scene size in logical pixels (default: 960x640)\n\
  --exit-after SECONDS     Exit after 0.1 to 600 seconds, useful for smoke tests\n\
  --smoke-test             Use bundled examples; no --model; exit >= 10 seconds\n\
  -h, --help              Show this help\n\n\
Without --model, the two bundled example models are loaded.\n\
The control window remains interactive when the scene is click-through.\n";

pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<ParseResult, String> {
    let mut args = args.into_iter();
    let mut options = Options::default();
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
                options.overlay_size = [width, height];
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
            _ => return Err(format!("Unknown option: {}", argument.to_string_lossy())),
        }
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
        assert_eq!(options.overlay_size, [960, 640]);
        assert_eq!(options.exit_after, Some(Duration::from_secs(12)));
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
