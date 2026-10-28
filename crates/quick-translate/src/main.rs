//! quick-translate: CLI dispatch and daemon entry point.

mod cli;
mod forward;
mod gate;
mod instance;
mod logging;
mod runtime;
mod settings;

use std::process::ExitCode;

use cli::Command;

fn main() -> ExitCode {
    let command = match cli::parse(std::env::args_os().skip(1)) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("quick-translate: {err}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    let gate = match command {
        Command::Version | Command::Help | Command::Quit | Command::RemoveIntegration => Ok(()),
        Command::Daemon | Command::SettingsWindow => gate::check_connection(),
        Command::Default
        | Command::Toggle
        | Command::Show
        | Command::Settings
        | Command::Status => gate::check_env(),
    };
    if let Err(err) = gate {
        eprintln!("{err}");
        return ExitCode::FAILURE;
    }
    match command {
        Command::Version => {
            println!("quick-translate {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Help => {
            print!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        Command::Daemon => runtime::run(),
        Command::SettingsWindow => settings::run_window(),
        Command::Quit => forward::quit(),
        Command::Status => forward::status(),
        Command::RemoveIntegration => forward::remove_integration(),
        Command::Default | Command::Toggle | Command::Show | Command::Settings => {
            forward::activate(command)
        }
    }
}
