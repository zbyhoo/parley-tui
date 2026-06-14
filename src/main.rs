use std::time::Duration;

use anyhow::Result;
use parley_tui::app::App;
use parley_tui::config::Config;
use parley_tui::router::AgentId;
use parley_tui::timeline::Timeline;
use parley_tui::ui;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::layout::Rect;

fn main() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config = Config::load(&cwd)?;
    let hygiene_warning = parley_tui::hygiene::ensure_gitignore(&cwd)
        .err()
        .map(|e| format!("warning: failed to update .gitignore: {e}"));
    let state_dir = config.state_dir.clone().unwrap_or_else(|| cwd.join(".parley"));
    let session = format!("session-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let timeline = Timeline::open(&state_dir.join(&session).join("timeline.jsonl"))?;

    let pending = parley_tui::pending::new_queue();
    // Broker MCP (komunikacja agent→agent). Brak brokera = degradacja do Etapu 1.
    let broker = match parley_tui::broker::start(pending.clone()) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("warning: broker failed to start: {e} — agent↔agent messaging disabled");
            None
        }
    };
    let claude_mcp_path = state_dir.join("claude-mcp.json");

    let mut terminal = ratatui::init();
    let mut app = App::new(config, timeline, cwd, pending);

    // Wstrzyknięcie konfiguracji MCP do komend agentów PRZED spawnem (spawn czyta args).
    if let Some(h) = &broker {
        let _ = parley_tui::config::write_claude_mcp_config(&claude_mcp_path, h.port);
        for id in [AgentId::Claude, AgentId::Codex] {
            let extra = parley_tui::config::mcp_extra_args(id, h.port, &claude_mcp_path);
            app.pane_mut(id).cfg.args.extend(extra);
        }
    }

    // Rozmiar PTY z faktycznego layoutu, zanim wystartują agenci.
    // terminal.size() zwraca Result<Size> (ratatui 0.30), budujemy Rect z width/height.
    let sz = terminal.size()?;
    let screen = Rect::new(0, 0, sz.width, sz.height);
    let a = ui::areas(screen);
    app.pty_sizes = [ui::pty_size(a.claude), ui::pty_size(a.codex)];
    app.spawn_agent(AgentId::Claude);
    app.spawn_agent(AgentId::Codex);

    let run_result = run(&mut terminal, &mut app);
    let _ = terminal.draw(|f| {
        let area = f.area();
        f.render_widget(ratatui::widgets::Clear, area);
        let msg = ratatui::widgets::Paragraph::new("parley is shutting down — stopping agents…")
            .alignment(ratatui::layout::Alignment::Center);
        let y = area.height / 2;
        let line = ratatui::layout::Rect::new(area.x, area.y + y, area.width, 1);
        f.render_widget(msg, line);
    });
    // Odmroź zablokowane handlery agentów, zatrzymaj broker, potem zamknij PTY.
    app.resolve_all_pending();
    if let Some(h) = broker {
        h.shutdown();
    }
    app.shutdown();
    ratatui::restore();
    if let Some(w) = hygiene_warning {
        eprintln!("{w}");
    }

    // Jeśli żaden agent nie wystartował lub wszystkie zakończyły się — pokaż błędy po wyjściu z TUI.
    if app.panes.iter().all(|p| p.proc.is_none() || p.exited.is_some()) {
        for pane in &app.panes {
            eprintln!("{}: {}", pane.id.label(), pane.status);
        }
        eprintln!("hint: configure agent commands in .parley/config.toml");
    }
    run_result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        app.poll_agents();
        app.tick_auto();
        terminal.draw(|f| ui::render(f, app))?;
        if event::poll(Duration::from_millis(33))? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => app.handle_key(k),
                Event::Resize(w, h) => {
                    let a = ui::areas(Rect::new(0, 0, w, h));
                    app.resize_ptys([ui::pty_size(a.claude), ui::pty_size(a.codex)]);
                }
                _ => {}
            }
        }
        if app.should_quit {
            return Ok(());
        }
    }
}
