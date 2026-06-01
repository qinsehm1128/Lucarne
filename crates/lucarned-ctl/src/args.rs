use std::{ffi::OsString, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    RunDaemon,
    Help,
    Version,
    Init,
    Paths,
    Doctor,
    Update,
    Autostart(AutostartCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutostartCommand {
    Install { start: bool, bin: Option<PathBuf> },
    Uninstall { stop: bool },
    Start,
    Stop,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
}

impl ParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub fn parse<I>(args: I) -> Result<Command, ParseError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let _program = args.next();
    let Some(first) = args.next() else {
        return Ok(Command::RunDaemon);
    };
    parse_first(first, args.collect())
}

fn parse_first(first: OsString, rest: Vec<OsString>) -> Result<Command, ParseError> {
    let first_text = first.to_string_lossy();
    match first_text.as_ref() {
        "help" | "--help" | "-h" => require_no_args(Command::Help, rest),
        "--version" | "version" => require_no_args(Command::Version, rest),
        "init" => require_no_args(Command::Init, rest),
        "paths" => require_no_args(Command::Paths, rest),
        "doctor" => require_no_args(Command::Doctor, rest),
        "update" => require_no_args(Command::Update, rest),
        "autostart" => parse_autostart(rest),
        other => Err(ParseError::new(format!("unknown command: {other}"))),
    }
}

fn require_no_args(command: Command, rest: Vec<OsString>) -> Result<Command, ParseError> {
    if rest.is_empty() {
        Ok(command)
    } else {
        Err(ParseError::new(format!(
            "unexpected argument: {}",
            rest[0].to_string_lossy()
        )))
    }
}

fn parse_autostart(args: Vec<OsString>) -> Result<Command, ParseError> {
    let mut args = args.into_iter();
    let Some(subcommand) = args.next() else {
        return Err(ParseError::new("missing autostart subcommand"));
    };
    match subcommand.to_string_lossy().as_ref() {
        "install" => parse_autostart_install(args.collect()),
        "uninstall" => parse_autostart_uninstall(args.collect()),
        "start" => require_no_os_args(AutostartCommand::Start, args.collect()),
        "stop" => require_no_os_args(AutostartCommand::Stop, args.collect()),
        "status" => require_no_os_args(AutostartCommand::Status, args.collect()),
        other => Err(ParseError::new(format!(
            "unknown autostart subcommand: {other}"
        ))),
    }
    .map(Command::Autostart)
}

fn require_no_os_args(
    command: AutostartCommand,
    rest: Vec<OsString>,
) -> Result<AutostartCommand, ParseError> {
    if rest.is_empty() {
        Ok(command)
    } else {
        Err(ParseError::new(format!(
            "unexpected argument: {}",
            rest[0].to_string_lossy()
        )))
    }
}

fn parse_autostart_install(args: Vec<OsString>) -> Result<AutostartCommand, ParseError> {
    let mut start = false;
    let mut bin = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.to_string_lossy().as_ref() {
            "--start" => start = true,
            "--bin" => {
                let Some(path) = iter.next() else {
                    return Err(ParseError::new("--bin requires a path"));
                };
                bin = Some(PathBuf::from(path));
            }
            other => return Err(ParseError::new(format!("unexpected argument: {other}"))),
        }
    }
    Ok(AutostartCommand::Install { start, bin })
}

fn parse_autostart_uninstall(args: Vec<OsString>) -> Result<AutostartCommand, ParseError> {
    let mut stop = false;
    for arg in args {
        match arg.to_string_lossy().as_ref() {
            "--stop" => stop = true,
            other => return Err(ParseError::new(format!("unexpected argument: {other}"))),
        }
    }
    Ok(AutostartCommand::Uninstall { stop })
}

pub fn usage() -> &'static str {
    "lucarned - lucarne daemon and local service manager\n\n\
Usage:\n\
  lucarned                         Run daemon\n\
  lucarned init                    Configure lucarned interactively\n\
  lucarned doctor                  Diagnose install and runtime state\n\
  lucarned update                  Check latest Lucarne release status\n\
  lucarned paths                   Print resolved paths\n\
  lucarned autostart install [--start] [--bin PATH]\n\
  lucarned autostart uninstall [--stop]\n\
  lucarned autostart start\n\
  lucarned autostart stop\n\
  lucarned autostart status\n\
  lucarned help\n\
  lucarned --version\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_words(words: &[&str]) -> Result<Command, ParseError> {
        parse(words.iter().map(OsString::from))
    }

    #[test]
    fn no_command_runs_daemon() {
        assert_eq!(parse_words(&["lucarned"]).unwrap(), Command::RunDaemon);
    }

    #[test]
    fn parses_top_level_commands() {
        assert_eq!(parse_words(&["lucarned", "init"]).unwrap(), Command::Init);
        assert_eq!(parse_words(&["lucarned", "paths"]).unwrap(), Command::Paths);
        assert_eq!(
            parse_words(&["lucarned", "doctor"]).unwrap(),
            Command::Doctor
        );
        assert_eq!(
            parse_words(&["lucarned", "update"]).unwrap(),
            Command::Update
        );
        assert_eq!(
            parse_words(&["lucarned", "--version"]).unwrap(),
            Command::Version
        );
    }

    #[test]
    fn parses_autostart_install_flags() {
        assert_eq!(
            parse_words(&[
                "lucarned",
                "autostart",
                "install",
                "--start",
                "--bin",
                "/tmp/lucarned",
            ])
            .unwrap(),
            Command::Autostart(AutostartCommand::Install {
                start: true,
                bin: Some(PathBuf::from("/tmp/lucarned")),
            })
        );
    }

    #[test]
    fn parses_autostart_uninstall_stop() {
        assert_eq!(
            parse_words(&["lucarned", "autostart", "uninstall", "--stop"]).unwrap(),
            Command::Autostart(AutostartCommand::Uninstall { stop: true })
        );
    }

    #[test]
    fn usage_lists_update_command() {
        assert!(usage().contains("lucarned update"));
    }

    #[test]
    fn rejects_unknown_command() {
        let err = parse_words(&["lucarned", "nope"]).unwrap_err();
        assert_eq!(err.message, "unknown command: nope");
    }

    #[test]
    fn rejects_missing_bin_path() {
        let err = parse_words(&["lucarned", "autostart", "install", "--bin"]).unwrap_err();
        assert_eq!(err.message, "--bin requires a path");
    }
}
