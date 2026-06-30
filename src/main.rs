//! suzume-sql: a table-first terminal database client.
//!
//! The UI thread owns the render/event loop and never touches the database
//! directly; all I/O happens on the worker thread (see [`worker`]).

mod app;
mod clipboard;
mod config;
mod db;
mod error;
mod input;
mod model;
mod ui;
mod worker;

use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event};

use crate::app::state::App;
use crate::config::Config;
use crate::error::ConfigError;

/// Fallback config path used when the OS config directory cannot be resolved.
const FALLBACK_CONFIG: &str = "suzume-sql.toml";
const TICK: Duration = Duration::from_millis(120);

struct Args {
    config_path: String,
    /// The positional argument: a `scheme://` connection string, or otherwise a
    /// connection name to look up in the config.
    connection: Option<String>,
}

fn main() {
    let args = match parse_args() {
        ParseOutcome::Run(args) => args,
        ParseOutcome::Help => {
            print_help();
            return;
        }
        ParseOutcome::Version => {
            println!("suzume-sql {}", env!("CARGO_PKG_VERSION"));
            return;
        }
    };

    let config = match resolve_config(&args) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("suzume: {e}");
            if let ConfigError::Read { .. } = e {
                eprintln!("(expected a config file at `{}`)", args.config_path);
            }
            std::process::exit(1);
        }
    };

    let app = App::new(config, args.config_path);
    if let Err(e) = run(app) {
        eprintln!("suzume: {e}");
        std::process::exit(1);
    }
}

/// Resolve the connection to open. An argument carrying a `scheme://` prefix is
/// a direct connection string; any other argument is a connection name looked
/// up in the config file. With no argument, the config file is loaded as a
/// whole (single connection auto-connects, multiple show the picker).
fn resolve_config(args: &Args) -> Result<Config, ConfigError> {
    let Some(spec) = &args.connection else {
        return Config::load_or_create(&args.config_path);
    };

    if spec.contains("://") {
        Config::from_connection_string(spec)
    } else {
        Config::load_or_create(&args.config_path)?
            .select(spec)
            .ok_or_else(|| ConfigError::UnknownConnection(spec.clone()))
    }
}

fn run(mut app: App) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| ui::render(frame, app))?;

        // Advance the spinner animation frame.
        app.spinner_frame = app.spinner_frame.wrapping_add(1);

        // Block for input up to one tick so the spinner keeps animating while a
        // query is in flight, without busy-looping.
        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
        {
            input::handle_key(app, key);
        }

        // Apply whatever the worker has produced since the last frame.
        app.drain_worker();
        // Apply the result of an in-flight connection test, if any.
        app.poll_test();
    }
    Ok(())
}

enum ParseOutcome {
    Run(Args),
    Help,
    Version,
}

fn parse_args() -> ParseOutcome {
    let mut config_path = default_config_path();
    let mut connection: Option<String> = None;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return ParseOutcome::Help,
            "-v" | "--version" => return ParseOutcome::Version,
            "-c" | "--config" => {
                if let Some(path) = args.next() {
                    config_path = path;
                } else {
                    eprintln!("suzume: --config requires a path");
                    std::process::exit(2);
                }
            }
            other => connection = Some(other.to_string()),
        }
    }

    ParseOutcome::Run(Args {
        config_path,
        connection,
    })
}

/// The default connections file in the OS config directory, falling back to a
/// file in the working directory if that directory cannot be resolved.
fn default_config_path() -> String {
    Config::default_os_config_path()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| FALLBACK_CONFIG.to_string())
}

fn print_help() {
    let default_path = default_config_path();
    println!(
        r#"suzume-sql — table-first terminal database client

USAGE:
    suzume [connection]
    suzume [--config <path>]

ARGS:
    [connection]   A connection string (identified by its scheme://), or
                   otherwise a connection name from the config file:
        sqlite     sqlite://<path>                       (e.g. sqlite://./demo.db)
        postgres   postgresql://user:pass@host:port/db
        mysql      mysql://user:pass@host:port/db

OPTIONS:
    -c, --config <path>   Path to the TOML config (default: {default_path})
    -h, --help            Show this help

With no argument, the config file is loaded; if it defines more than one
connection, an interactive picker is shown. Connections can also be created,
edited and deleted from inside the picker.
"#
    );
}
