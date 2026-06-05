//! xrocmtop — a btop-style terminal UI for AMD ROCm / Vulkan GPUs.
//!
//! This tool is strictly read-only: it observes the GPU via sysfs, `rocm-smi`, and
//! `vulkaninfo`, and never writes to sysfs or changes device state.

mod app;
mod collect;
mod config;
mod history;
mod model;
mod panel;
mod report;
mod settings;
mod theme;
mod thermal;
mod ui;

use std::io::{self, Stdout};
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, ExecutableCommand};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;
use config::Config;

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn main() -> Result<()> {
    let config = Config::parse();

    if config.once {
        return run_once(config);
    }

    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal, App::new(config));
    restore_terminal()?; // always restore, even if `run` failed
    result
}

/// Non-TUI path: collect one snapshot and print it as text or JSON, then exit 0.
fn run_once(config: Config) -> Result<()> {
    let mut app = App::new(config);
    app.tick();
    let report = report::Report::from_app(&app);
    if app.config.json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_text());
    }
    Ok(())
}

/// The main loop: render, then wait up to one interval for input, ticking on timeout.
fn run(terminal: &mut Tui, mut app: App) -> Result<()> {
    let tick_rate = app.config.interval();
    app.tick(); // populate the first frame before drawing
    let mut last_tick = Instant::now();

    while !app.should_quit() {
        terminal.draw(|frame| ui::render(frame, &app))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == event::KeyEventKind::Press {
                    app.on_key(key);
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            if !app.paused() {
                app.tick();
            }
            last_tick = Instant::now();
        }
    }
    app.save_settings(); // persist theme + panel layout on clean exit
    Ok(())
}

/// Enter the alternate screen + raw mode and hand back a ready terminal.
fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    Ok(terminal)
}

/// Undo everything `setup_terminal` did. Safe to call on any exit path.
fn restore_terminal() -> Result<()> {
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    io::stdout().execute(crossterm::cursor::Show)?;
    Ok(())
}

/// Restore the terminal before the default panic handler prints, so a panic never leaves the
/// user stuck in raw mode / the alternate screen.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        original(info);
    }));
}
