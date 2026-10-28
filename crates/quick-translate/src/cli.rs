//! Argument parsing. Deliberately tiny: it runs before anything else on the forward path.

use std::ffi::OsString;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// No arguments: start the daemon if needed, otherwise open settings.
    Default,
    Toggle,
    Show,
    Settings,
    Quit,
    Daemon,
    RemoveIntegration,
    Status,
    Version,
    Help,
    /// Internal: the settings window process spawned by the daemon. Not in [`USAGE`].
    SettingsWindow,
}

/// Internal flag the daemon passes to the settings process it spawns.
pub const SETTINGS_WINDOW_FLAG: &str = "--settings-window";

pub const USAGE: &str = "\
Usage: quick-translate [COMMAND]

Select text, press a shortcut, see the translation at the cursor (Wayland only).

Commands:
  (none)                 Start the daemon if it is not running, otherwise open settings
  --toggle               Show the popup for the current selection, or hide it if visible
  --show                 Show the popup for the current selection
  --settings             Open the settings window
  --quit                 Stop the daemon
  --daemon               Run the daemon without showing any UI
  --remove-integration   Remove the installed .desktop file, icon and autostart entry
  --status               Print whether the daemon is running and the detected capabilities
  --version              Print the version and exit
  --help                 Print this help and exit

--toggle, --show and --settings start the daemon first when it is not running.
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Parses the arguments after the program name. At most one command is accepted.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, UsageError> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(Command::Default);
    };
    let Some(arg) = first.to_str() else {
        return Err(UsageError(format!(
            "invalid argument {:?}",
            first.to_string_lossy()
        )));
    };
    let command = match arg {
        "--toggle" => Command::Toggle,
        "--show" => Command::Show,
        "--settings" => Command::Settings,
        "--quit" => Command::Quit,
        "--daemon" => Command::Daemon,
        "--remove-integration" => Command::RemoveIntegration,
        "--status" => Command::Status,
        "--version" | "-V" => Command::Version,
        "--help" | "-h" => Command::Help,
        SETTINGS_WINDOW_FLAG => Command::SettingsWindow,
        other => return Err(UsageError(format!("unknown argument '{other}'"))),
    };
    if let Some(extra) = args.next() {
        return Err(UsageError(format!(
            "unexpected argument '{}' after '{arg}'",
            extra.to_string_lossy()
        )));
    }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, UsageError> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn every_flag_parses() {
        let cases = [
            (&[][..], Command::Default),
            (&["--toggle"], Command::Toggle),
            (&["--show"], Command::Show),
            (&["--settings"], Command::Settings),
            (&["--quit"], Command::Quit),
            (&["--daemon"], Command::Daemon),
            (&["--remove-integration"], Command::RemoveIntegration),
            (&["--status"], Command::Status),
            (&["--version"], Command::Version),
            (&["-V"], Command::Version),
            (&["--help"], Command::Help),
            (&["-h"], Command::Help),
            (&["--settings-window"], Command::SettingsWindow),
        ];
        for (args, expected) in cases {
            assert_eq!(parse_strs(args), Ok(expected), "{args:?}");
        }
    }

    #[test]
    fn unknown_and_extra_arguments_are_rejected() {
        for args in [
            &["--bogus"][..],
            &["toggle"],
            &["-t"],
            &["--toggle", "--show"],
            &["--daemon", "extra"],
            &["--toggle="],
        ] {
            assert!(parse_strs(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn internal_flag_is_not_documented() {
        assert!(!USAGE.contains(SETTINGS_WINDOW_FLAG));
    }

    #[test]
    fn non_utf8_argument_is_rejected() {
        use std::os::unix::ffi::OsStringExt;
        assert!(parse([OsString::from_vec(vec![0xff, 0xfe])]).is_err());
    }
}
