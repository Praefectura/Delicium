//! Command line for the launcher.
//!
//! The launcher accepts a few switches of its own and forwards everything
//! after `--` to the engine unchanged, so a shortcut or a script can reach
//! every engine option (the acceptance scripts use this to run a bounded
//! scene through the launcher).

use std::ffi::OsString;

/// What the command line asked for.
pub enum ParseResult {
    Run(Options),
    Help,
    Version,
}

/// Launcher options.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Open the settings page and do not start the engine.
    pub settings: bool,
    /// Skip the update check entirely (offline runs and tests).
    pub no_update: bool,
    /// Run this engine binary instead of an installed version directory.
    /// A developer override; the path is reported in the log.
    pub engine: Option<std::path::PathBuf>,
    /// Arguments forwarded to the engine, after `--`.
    pub engine_args: Vec<OsString>,
}

pub const HELP: &str = "Delicium launcher\n\n\
Usage: delicium-launcher [OPTIONS] [-- ENGINE-ARGS...]\n\n\
  --settings           Open the settings page\n\
  --no-update          Skip the update check\n\
  --engine PATH        Run this engine binary instead of an installed one\n\
  -h, --help           Show this help\n\
  --version            Show the launcher version\n\n\
Everything after `--` is passed to the engine unchanged.\n";

/// Parses the launcher command line.
pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<ParseResult, String> {
    let mut args = args.into_iter();
    let mut options = Options::default();
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("-h" | "--help") => return Ok(ParseResult::Help),
            Some("--version") => return Ok(ParseResult::Version),
            Some("--settings") => options.settings = true,
            Some("--no-update") => options.no_update = true,
            Some("--engine") => {
                let path = args.next().ok_or("--engine requires a path")?;
                if path.is_empty() {
                    return Err("--engine requires a nonempty path".into());
                }
                options.engine = Some(path.into());
            }
            Some("--") => {
                options.engine_args.extend(args);
                break;
            }
            Some(other) if other.starts_with("--") => {
                return Err(format!("unknown option `{other}`; see --help"));
            }
            Some(other) => {
                return Err(format!(
                    "unexpected argument `{other}`; engine arguments go after `--`"
                ));
            }
            None => return Err("an argument is not valid Unicode".into()),
        }
    }
    Ok(ParseResult::Run(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<ParseResult, String> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_the_switches() {
        let ParseResult::Run(options) = parse(&["--settings", "--no-update"]).unwrap() else {
            panic!("expected run options");
        };
        assert!(options.settings);
        assert!(options.no_update);
        assert!(options.engine.is_none());
        assert!(options.engine_args.is_empty());
        assert!(matches!(parse(&["-h"]).unwrap(), ParseResult::Help));
        assert!(matches!(
            parse(&["--version"]).unwrap(),
            ParseResult::Version
        ));
    }

    #[test]
    fn forwards_engine_arguments_verbatim() {
        let ParseResult::Run(options) =
            parse(&["--settings", "--", "--smoke-test", "--model", "a b.json"]).unwrap()
        else {
            panic!("expected run options");
        };
        assert_eq!(
            options.engine_args,
            ["--smoke-test", "--model", "a b.json"]
                .iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
        // A second `--` is an engine argument, not a second separator.
        let ParseResult::Run(options) = parse(&["--", "--", "x"]).unwrap() else {
            panic!("expected run options");
        };
        assert_eq!(
            options.engine_args,
            ["--", "x"].iter().map(OsString::from).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejects_unknown_and_malformed_options() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["stray"]).is_err());
        assert!(parse(&["--engine"]).is_err());
        let ParseResult::Run(options) = parse(&["--engine", "x.exe"]).unwrap() else {
            panic!("expected run options");
        };
        assert_eq!(options.engine.unwrap(), std::path::PathBuf::from("x.exe"));
    }
}
