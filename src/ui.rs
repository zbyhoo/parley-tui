use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode};
use crate::router::AgentId;
use crate::timeline::Kind;

pub struct Areas {
    pub claude: Rect,
    pub codex: Rect,
    pub timeline: Rect,
    pub input: Rect,
    pub status: Rect,
}

/// Układ B: panele u góry, timeline, input, status.
pub fn areas(area: Rect) -> Areas {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(8),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);
    let panels = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    Areas {
        claude: panels[0],
        codex: panels[1],
        timeline: rows[1],
        input: rows[2],
        status: rows[3],
    }
}

/// Rozmiar PTY wewnątrz panelu z ramką.
pub fn pty_size(panel: Rect) -> (u16, u16) {
    (panel.height.saturating_sub(2), panel.width.saturating_sub(2))
}

pub fn render(f: &mut Frame, app: &App) {
    let a = areas(f.area());
    render_pane(f, app, AgentId::Claude, a.claude);
    render_pane(f, app, AgentId::Codex, a.codex);
    render_timeline(f, app, a.timeline);
    render_input(f, app, a.input);
    render_status(f, app, a.status);
    // Help ma najwyższy priorytet, potem quit, potem pending.
    if app.show_help {
        render_help_popup(f, f.area());
    } else if app.confirm_quit {
        render_quit_popup(f, f.area());
    } else {
        render_pending_popup(f, app, f.area());
    }
}

fn render_pane(f: &mut Frame, app: &App, id: AgentId, area: Rect) {
    let pane = app.pane(id);
    let focused = app.focus == id;
    let mut title = pane.id.label().to_string();
    if pane.exited.is_some() || pane.proc.is_none() {
        title = format!("{title} — {}", pane.status);
    } else if focused && matches!(app.mode, Mode::Passthrough) {
        title = format!("{title} [PASSTHROUGH]");
    }
    let border_style = if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default().borders(Borders::ALL).title(title).border_style(border_style);
    match (&pane.proc, pane.exited) {
        (Some(p), None) => p.with_screen(|screen| {
            f.render_widget(PseudoTerminal::new(screen).block(block), area);
        }),
        _ => f.render_widget(
            Paragraph::new(pane.status.clone())
                .alignment(ratatui::layout::Alignment::Center)
                .block(block),
            area,
        ),
    }
}

fn render_timeline(f: &mut Frame, app: &App, area: Rect) {
    let visible = area.height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .timeline
        .entries
        .iter()
        .rev()
        .take(visible)
        .rev()
        .map(|e| {
            let time = e.ts.get(11..19).unwrap_or("--:--:--");
            let line = match e.kind {
                Kind::Message => format!("[{time}] {} → {}: {}", e.from, e.to, e.text),
                Kind::Event => format!("[{time}] ({} → {}) {}", e.from, e.to, e.text),
            };
            ListItem::new(Line::from(line))
        })
        .collect();
    f.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title("timeline")),
        area,
    );
}

fn render_input(f: &mut Frame, app: &App, area: Rect) {
    let text = format!("> {}", app.input);
    f.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("input (@claude/@codex/@all)")),
        area,
    );
    if matches!(app.mode, Mode::Input) && !app.confirm_quit {
        let x = area.x + 2 + UnicodeWidthStr::width(app.input.as_str()) as u16 + 1;
        f.set_cursor_position((x.min(area.right().saturating_sub(2)), area.y + 1));
    }
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let mode = match app.mode {
        Mode::Input => "INPUT",
        Mode::Passthrough => "PASSTHROUGH",
    };
    let auto = match app.auto {
        Some(n) => format!(" | AUTO ({n} left)"),
        None => String::new(),
    };
    let text = format!(
        " {mode}{auto} | focus: {} | Tab=focus Ctrl+]=mode Ctrl+R=restart Ctrl+C=quit ?=help",
        app.focus.label()
    );
    f.render_widget(Paragraph::new(text).style(Style::default().bg(Color::DarkGray)), area);
}

/// Wycentrowany prostokąt o zadanych wymiarach (przycięty do obszaru).
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

/// Popup oczekującej wiadomości agent→agent (głowa kolejki) ze skrótami moderacji.
fn render_pending_popup(f: &mut Frame, app: &App, area: Rect) {
    let q = app.pending.lock().unwrap();
    let total = q.len();
    let Some(head) = q.front() else { return };
    let more = if total > 1 { format!("    (+{} more)", total - 1) } else { String::new() };
    let body = format!(
        "{} → {}:\n\n{}\n\ny / Enter = approve    n / Esc = reject{}",
        head.from.label(),
        head.to.label(),
        head.text,
        more,
    );
    let rect = centered_rect(64, 9, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" peer message ")
        .border_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    f.render_widget(
        Paragraph::new(body).alignment(ratatui::layout::Alignment::Left).block(block),
        rect,
    );
}

/// Overlay pomocy: skróty klawiszowe + komendy specjalne parley.
fn render_help_popup(f: &mut Frame, area: Rect) {
    let key_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let hdr_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);

    let header = |s: &str| Line::from(Span::styled(format!("  {s}"), hdr_style));
    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<16}"), key_style),
            Span::raw(d.to_string()),
        ])
    };

    let lines = vec![
        header("Keybindings"),
        row("Tab", "switch focus (claude / codex)"),
        row("Ctrl+]", "passthrough mode (keys go straight to agent)"),
        row("Ctrl+R", "restart focused agent"),
        row("Enter", "send input"),
        row("Esc", "abort auto mode / clear input"),
        row("Backspace", "delete last character"),
        row("Ctrl+C / Ctrl+Q", "quit (with confirmation)"),
        row("?", "show this help (when input is empty)"),
        Line::raw(""),
        header("Passthrough mode"),
        row("Ctrl+]", "back to input mode"),
        Line::raw(""),
        header("Peer message popup"),
        row("y / Enter", "approve"),
        row("n / Esc", "reject"),
        Line::raw(""),
        header("Parley commands (type in input)"),
        row("@claude <msg>", "send to claude"),
        row("@codex <msg>", "send to codex"),
        row("@all <msg>", "send to both agents"),
        row("/auto N", "auto-approve next N peer messages"),
        row("/auto off", "disable auto mode"),
        row("/discuss [N] <topic>", "start a peer discussion from focused agent"),
        row("/help  /?", "show this help"),
        Line::raw(""),
        Line::from(Span::styled("  press any key to close", dim)),
    ];

    let rect = centered_rect(72, lines.len() as u16 + 2, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" help ")
        .border_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

fn render_quit_popup(f: &mut Frame, area: Rect) {
    let rect = centered_rect(44, 5, area);
    f.render_widget(Clear, rect);
    let text = "Quit parley?\n\nCtrl+C / y = quit    any other key = stay";
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" quit ")
        .border_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    f.render_widget(
        Paragraph::new(text).alignment(ratatui::layout::Alignment::Center).block(block),
        rect,
    );
}
