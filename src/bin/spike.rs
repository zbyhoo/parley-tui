use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use parley_tui::keys::key_to_bytes;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::widgets::{Block, Borders, Paragraph};
use tui_term::widget::PseudoTerminal;

/// Spike: `cargo run --bin spike -- claude` (albo `codex`).
/// Lewy panel = CLI w PTY (50% szerokości), prawy = instrukcja.
/// Ctrl+Q kończy. Wszystkie pozostałe klawisze idą do CLI.
fn main() -> Result<()> {
    let cmd_name = std::env::args().nth(1).unwrap_or_else(|| "claude".to_string());

    let mut terminal = ratatui::init();
    let size = terminal.size()?;
    let cols = size.width / 2 - 2;
    let rows = size.height - 2;

    let pty = native_pty_system();
    let pair = pty.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
    let mut cmd = CommandBuilder::new(&cmd_name);
    cmd.cwd(std::env::current_dir()?);
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 1000)));
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    {
        let parser = Arc::clone(&parser);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                parser.lock().unwrap().process(&buf[..n]);
            }
        });
    }

    loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(f.area());
            let parser = parser.lock().unwrap();
            let block = Block::default().borders(Borders::ALL).title(cmd_name.clone());
            f.render_widget(PseudoTerminal::new(parser.screen()).block(block), chunks[0]);
            f.render_widget(
                Paragraph::new("SPIKE\nCtrl+Q = quit\nother keys go to the CLI")
                    .block(Block::default().borders(Borders::ALL).title("info")),
                chunks[1],
            );
        })?;

        if event::poll(Duration::from_millis(30))? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    if k.code == KeyCode::Char('q') && k.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }
                    let bytes = key_to_bytes(&k);
                    if !bytes.is_empty() {
                        writer.write_all(&bytes)?;
                        writer.flush()?;
                    }
                }
                _ => {}
            }
        }
        if child.try_wait()?.is_some() {
            break;
        }
    }

    ratatui::restore();
    let _ = child.kill();
    Ok(())
}
