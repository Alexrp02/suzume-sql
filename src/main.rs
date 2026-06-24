//! normal-sql: a table-first terminal database client.
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

const DEFAULT_CONFIG: &str = "normal-sql.toml";
const TICK: Duration = Duration::from_millis(120);

struct Args {
    config_path: String,
    connection: Option<String>,
}

fn main() {
    let args = match parse_args() {
        ParseOutcome::Run(args) => args,
        ParseOutcome::Help => {
            print_help();
            return;
        }
    };

    let config = match Config::load(&args.config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("normal-sql: {e}");
            eprintln!("(expected a config file at `{}`)", args.config_path);
            std::process::exit(1);
        }
    };

    let app = App::new(config, args.connection);
    if let Err(e) = run(app) {
        eprintln!("normal-sql: {e}");
        std::process::exit(1);
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
    }
    Ok(())
}

enum ParseOutcome {
    Run(Args),
    Help,
}

fn parse_args() -> ParseOutcome {
    let mut config_path = DEFAULT_CONFIG.to_string();
    let mut connection: Option<String> = None;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return ParseOutcome::Help,
            "-c" | "--config" => {
                if let Some(path) = args.next() {
                    config_path = path;
                } else {
                    eprintln!("normal-sql: --config requires a path");
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

fn print_help() {
    println!(
        "normal-sql — table-first terminal database client\n\n\
         USAGE:\n    \
         normal-sql [--config <path>] [connection-name]\n\n\
         OPTIONS:\n    \
         -c, --config <path>   Path to the TOML config (default: {DEFAULT_CONFIG})\n    \
         -h, --help            Show this help\n\n\
         If [connection-name] is omitted and the config has more than one\n\
         connection, an interactive picker is shown.\n"
    );
}
