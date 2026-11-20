mod app;
mod config;
mod editor;
mod preview;
mod search;
mod ui;

use app::App;
use config::{Config, clean_canonical, git_root};
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::crossterm::execute;
use std::path::PathBuf;
use std::time::Duration;

const USAGE: &str = "rgg - search text in files and jump to VS Code

usage: rgg [PATH] [-t TYPE]... [--regex]

  PATH          folder to search (default: current directory)
  -t, --type    only search files of this ripgrep type (repeatable), e.g. -t cs
  -r, --regex   start in regex mode
  -V, --version show version and git commit
  -h, --help    show this help

Press F1 inside the app for key bindings.
Config: ~/.rgg/config.toml (%USERPROFILE%\\.rgg\\config.toml)";

/// e.g. `rgg 0.1.0 (0f2f72b)`; `-dirty` marks a build with uncommitted changes.
fn version() -> String {
    format!("rgg {} ({})", env!("CARGO_PKG_VERSION"), env!("RGG_GIT_COMMIT"))
}

struct Args {
    path: PathBuf,
    types: Vec<String>,
    regex: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { path: PathBuf::from("."), types: Vec::new(), regex: false };
    let mut path_set = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("{}", version());
                std::process::exit(0);
            }
            "-r" | "--regex" => args.regex = true,
            "-t" | "--type" => args.types.push(it.next().ok_or("-t needs a type name")?),
            s if s.starts_with("--type=") => args.types.push(s["--type=".len()..].to_string()),
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option: {s}")),
            s if !path_set => {
                args.path = PathBuf::from(s);
                path_set = true;
            }
            s => return Err(format!("unexpected argument: {s}")),
        }
    }
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rgg: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if !args.path.is_dir() {
        eprintln!("rgg: not a directory: {}", args.path.display());
        std::process::exit(2);
    }
    if let Err(e) = search::build_types(&args.types) {
        eprintln!("rgg: {e}");
        std::process::exit(2);
    }
    let search_root = clean_canonical(&args.path);
    let root = git_root(&search_root);
    let (cfg, store, warning) = Config::load_from(Config::default_path());
    preview::warm_up();

    let mut app = App::new(cfg, store, search_root, root, args.types, args.regex);
    app.message = warning;

    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let result = run(&mut terminal, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    if let Err(e) = result {
        eprintln!("rgg: {e}");
        std::process::exit(1);
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        let timeout = if app.preview_pending() {
            Duration::from_millis(2)
        } else if app.is_busy() {
            Duration::from_millis(30)
        } else {
            Duration::from_millis(250)
        };
        if event::poll(timeout)? {
            // Handle everything queued before drawing again (keeps fast key repeat smooth).
            loop {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k),
                    Event::Mouse(m) => app.on_mouse(m),
                    _ => {}
                }
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        app.tick();
        // Syntax highlighting catches up in small slices so input is never blocked for long.
        app.advance_preview(Duration::from_millis(8));
        if app.quit {
            return Ok(());
        }
    }
}
