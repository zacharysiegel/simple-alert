mod prompt;
mod register;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use clap::Subcommand;
use clap::ValueEnum;
use simple_alert::alert::Alert;
use simple_alert::alert::AlertLevel;
use simple_alert::config;
use simple_alert::error::AppError;
use simple_alert::send;

#[derive(Parser)]
#[command(name = "simple-alert", version, about = "Posts an alert to a Telegram channel.")]
struct Cli {
    #[arg(
        long,
        value_name = "FILE",
        global = true,
        help = "Config file [default: ~/.config/simple-alert/config.toml]",
    )]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Post one alert to the configured chat")]
    Send {
        #[arg(long, value_name = "NAME", help = "Letters, digits, '-' and '_'")]
        service: String,

        #[arg(long, value_enum)]
        level: LevelArgument,

        #[arg(value_name = "MESSAGE")]
        message: String,
    },
    #[command(about = "Post \"INFO [simple-alert] test alert\" to the configured chat")]
    Test,
    #[command(about = "Store the bot token, choose the chat and write the config, interactively")]
    Register,
}

#[derive(Clone, Copy, ValueEnum)]
enum LevelArgument {
    Info,
    Warning,
    Error,
}

impl LevelArgument {
    fn to_alert_level(self) -> AlertLevel {
        match self {
            LevelArgument::Info => AlertLevel::Info,
            LevelArgument::Warning => AlertLevel::Warning,
            LevelArgument::Error => AlertLevel::Error,
        }
    }
}

fn main() -> ExitCode {
    let result: Result<(), AppError> = run();

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("simple-alert: {}", error.message);

            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), AppError> {
    let cli: Cli = Cli::parse();

    let config_path: PathBuf = match &cli.config {
        Some(path) => path.clone(),
        None => config::default_config_path()?,
    };

    match cli.command {
        Command::Send {
            service,
            level,
            message,
        } => {
            let alert: Alert = Alert::new(&service, level.to_alert_level(), &message)?;

            send::send_alert(&config_path, &alert)
        }
        Command::Test => {
            let test_alert: Alert = send::create_test_alert()?;

            send::send_alert(&config_path, &test_alert)
        }
        Command::Register => register::run(&config_path, cli.config.as_deref()),
    }
}
