use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::{AgentConfig, Config};
use crate::pty::AgentProcess;
use crate::router::{self, AgentId, Parsed, Target};
use crate::timeline::{now_ts, Entry, Kind, Timeline};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Piszesz do parley (wspólny input + skróty globalne).
    Input,
    /// Klawisze idą wprost do aktywnego agenta; tylko Ctrl+] wraca do Input.
    Passthrough,
}

pub struct AgentPane {
    pub id: AgentId,
    pub cfg: AgentConfig,
    pub proc: Option<AgentProcess>,
    /// Some(kod) gdy proces się zakończył — panel pokazuje komunikat + skrót restartu.
    pub exited: Option<i32>,
    /// Komunikat statusu (np. błąd startu) pokazywany w panelu.
    pub status: String,
    /// True gdy w bieżącej sesji agenta cokolwiek faktycznie wysłano (write_input Ok lub Enter w passthrough).
    /// CLI nie zapisuje pustych sesji — resume bez tego wznowiłby starszą rozmowę.
    pub submitted: bool,
}

pub struct App {
    pub panes: [AgentPane; 2],
    pub mode: Mode,
    pub focus: AgentId,
    pub input: String,
    pub timeline: Timeline,
    pub confirm_quit: bool,
    pub should_quit: bool,
    /// Overlay pomocy (Option+H) — lista skrótów i komend.
    pub show_help: bool,
    /// Ostatni znany rozmiar PTY paneli (rows, cols) — do restartów, indeks = AgentId::idx().
    pub pty_sizes: [(u16, u16); 2],
    pub cwd: PathBuf,
    /// Kolejka oczekujących wiadomości agent→agent (współdzielona z wątkiem brokera).
    pub pending: crate::pending::PendingQueue,
    /// Tryb auto: Some(remaining) = auto-zatwierdzanie N wiadomości; None = ręczna moderacja.
    pub auto: Option<u32>,
    /// Górny limit dla `/auto N` (z configu).
    pub auto_max: u32,
}

/// Komenda restartu: resume tylko gdy w tej sesji coś wysłano —
/// CLI nie zapisuje pustych sesji, więc resume wznowiłby starszą rozmowę.
pub fn restart_command(cfg: &AgentConfig, submitted: bool) -> (Vec<String>, bool) {
    match (&cfg.resume_command, submitted) {
        (Some(resume), true) => (resume.clone(), true),
        _ => (cfg.full_command(), false),
    }
}

/// Parsuje argumenty `/discuss`: opcjonalne N (pierwszy token jako u32) + temat.
/// Brak N → N = auto_max. Zwraca (capped_n, topic) albo komunikat błędu (usage/zakres).
/// Pierwszy token będący liczbą jest traktowany jako N — temat "3 powody..." trzeba
/// poprzedzić jawnym N (np. "/discuss 6 3 powody...").
fn parse_discuss_args(rest: &str, auto_max: u32) -> Result<(u32, String), &'static str> {
    const USAGE: &str = "discuss: usage /discuss [N] <topic>";
    let rest = rest.trim();
    if rest.is_empty() {
        return Err(USAGE);
    }
    let (n, topic) = match rest.split_once(char::is_whitespace) {
        // pierwszy token to liczba → N + reszta jako temat; inaczej całość to temat
        Some((first, tail)) => match first.parse::<u32>() {
            Ok(n) => (n, tail.trim()),
            Err(_) => (auto_max, rest),
        },
        // jedno słowo: sama liczba = brak tematu; nie-liczba = jednowyrazowy temat
        None => match rest.parse::<u32>() {
            Ok(_) => return Err(USAGE),
            Err(_) => (auto_max, rest),
        },
    };
    if n == 0 {
        return Err("discuss: N must be >= 1");
    }
    if topic.is_empty() {
        return Err(USAGE);
    }
    Ok((n.min(auto_max), topic.to_string()))
}

/// Fizyczny Ctrl+]: terminale legacy raportują bajt 0x1D jako Ctrl+'5',
/// terminale z kitty protocol — jako Ctrl+']'.
fn is_passthrough_toggle(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5'))
}


impl App {
    pub fn new(
        config: Config,
        timeline: Timeline,
        cwd: PathBuf,
        pending: crate::pending::PendingQueue,
    ) -> Self {
        let auto_max = config.auto_max;
        App {
            panes: [
                AgentPane {
                    id: AgentId::Claude,
                    cfg: config.claude,
                    proc: None,
                    exited: None,
                    status: "not started".into(),
                    submitted: false,
                },
                AgentPane {
                    id: AgentId::Codex,
                    cfg: config.codex,
                    proc: None,
                    exited: None,
                    status: "not started".into(),
                    submitted: false,
                },
            ],
            mode: Mode::Input,
            focus: AgentId::Claude,
            input: String::new(),
            timeline,
            confirm_quit: false,
            should_quit: false,
            show_help: false,
            pty_sizes: [(24, 80); 2],
            cwd,
            pending,
            auto: None,
            auto_max,
        }
    }

    pub fn pane(&self, id: AgentId) -> &AgentPane {
        &self.panes[id.idx()]
    }

    pub fn pane_mut(&mut self, id: AgentId) -> &mut AgentPane {
        &mut self.panes[id.idx()]
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        // Overlay pomocy jest modalny i informacyjny: dowolny klawisz go zamyka.
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.confirm_quit {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.should_quit = true,
                KeyCode::Char('c') | KeyCode::Char('q') if ctrl => self.should_quit = true,
                _ => self.confirm_quit = false,
            }
            return;
        }
        // Oczekująca wiadomość peer→peer jest modalna w trybie ręcznym: dopóki nie zdecydujesz,
        // klawisze idą do moderacji (bez modyfikatorów — Alt na macOS komponuje znaki).
        // W trybie auto popup się nie pokazuje (kolejka drenowana w tick_auto), więc nie
        // przechwytujemy — Esc ma wtedy przerywać auto, nie odrzucać wiadomość.
        if self.auto.is_none() && !self.pending.lock().unwrap().is_empty() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.moderate_pending(true, false)
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.moderate_pending(false, false)
                }
                _ => {}
            }
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self.mode {
            Mode::Input => {
                if is_passthrough_toggle(&key) {
                    let pane = &self.panes[self.focus.idx()];
                    // passthrough do martwego agenta = klawisze w próżnię; zostań w Input
                    if pane.proc.is_some() && pane.exited.is_none() {
                        self.mode = Mode::Passthrough;
                    }
                    return;
                }
                match (key.code, ctrl) {
                (KeyCode::Char('c'), true) | (KeyCode::Char('q'), true) => self.confirm_quit = true,
                (KeyCode::Char('r'), true) => self.restart_focused(),
                (KeyCode::Tab, _) => self.focus = self.focus.other(),
                (KeyCode::Enter, _) => self.submit_input(),
                (KeyCode::Esc, _) => {
                    if self.auto.is_some() {
                        self.auto = None;
                        let _ = self.timeline.append(Entry {
                            ts: now_ts(),
                            from: "parley".into(),
                            to: "user".into(),
                            kind: Kind::Event,
                            text: "auto mode aborted".into(),
                        });
                    } else {
                        self.input.clear();
                    }
                }
                (KeyCode::Backspace, _) => {
                    self.input.pop();
                }
                // '?' przy pustym wejściu otwiera pomoc; w trakcie pisania to zwykły znak.
                (KeyCode::Char('?'), false) if self.input.is_empty() => self.show_help = true,
                (KeyCode::Char(c), false) => self.input.push(c),
                _ => {}
            }},
            Mode::Passthrough => {
                if is_passthrough_toggle(&key) {
                    self.mode = Mode::Input;
                    return;
                }
                let bytes = crate::keys::key_to_bytes(&key);
                if bytes.is_empty() {
                    return;
                }
                let focus = self.focus;
                let has_enter = bytes.contains(&b'\r');
                let pane = self.pane_mut(focus);
                if let Some(p) = pane.proc.as_mut() {
                    if p.write_raw(&bytes).is_ok() && has_enter {
                        pane.submitted = true;
                    }
                }
            }
        }
    }

    /// Enter w input mode: routing + doręczenie + wpis do timeline.
    fn submit_input(&mut self) {
        let line = std::mem::take(&mut self.input);
        if line.trim().is_empty() {
            return;
        }
        if matches!(line.trim(), "/help" | "/?") {
            self.show_help = true;
            return;
        }
        if let Some(rest) = line.trim().strip_prefix("/auto") {
            self.handle_auto_command(rest.trim());
            return;
        }
        if let Some(rest) = line.trim().strip_prefix("/discuss") {
            self.handle_discuss_command(rest.trim());
            return;
        }
        match router::parse(&line, Target::One(self.focus)) {
            Parsed::UnknownTarget(tok) => {
                let _ = self.timeline.append(Entry {
                    ts: now_ts(),
                    from: "parley".into(),
                    to: "user".into(),
                    kind: Kind::Event,
                    text: format!("unknown target {tok} — use @claude, @codex or @all"),
                });
            }
            Parsed::Message(target, text) => {
                if text.is_empty() {
                    // samo "@claude" bez treści — nic do doręczenia
                    return;
                }
                for &id in target.ids() {
                    let pane = self.pane_mut(id);
                    // proc może jeszcze istnieć choć poll_agents już oznaczył exited — nie piszemy do martwego PTY
                    let (kind, logged) = match pane.proc.as_mut() {
                        Some(p) if pane.exited.is_none() => match p.write_input(&text) {
                            Ok(()) => {
                                pane.submitted = true;
                                (Kind::Message, text.clone())
                            }
                            Err(e) => (Kind::Event, format!("undelivered (write error: {e}): {text}")),
                        },
                        _ => (Kind::Event, format!("undelivered (agent not running): {text}")),
                    };
                    let _ = self.timeline.append(Entry {
                        ts: now_ts(),
                        from: "user".into(),
                        to: id.label().into(),
                        kind,
                        text: logged,
                    });
                }
            }
        }
    }

    /// Rozstrzyga głowę kolejki oczekujących: approve wstrzykuje do peera, reject odrzuca.
    fn moderate_pending(&mut self, approve: bool, auto: bool) {
        let msg = match self.pending.lock().unwrap().pop_front() {
            Some(m) => m,
            None => return,
        };
        if !approve {
            let _ = msg.responder.send(crate::pending::Outcome::Rejected);
            let _ = self.timeline.append(Entry {
                ts: now_ts(),
                from: msg.from.label().into(),
                to: msg.to.label().into(),
                kind: Kind::Event,
                text: format!("peer message rejected: {}", msg.text),
            });
            return;
        }
        let peer = self.pane_mut(msg.to);
        let alive = peer.proc.is_some() && peer.exited.is_none();
        if !alive {
            let _ = msg
                .responder
                .send(crate::pending::Outcome::Error("peer not running".into()));
            let _ = self.timeline.append(Entry {
                ts: now_ts(),
                from: msg.from.label().into(),
                to: msg.to.label().into(),
                kind: Kind::Event,
                text: format!("peer message not delivered (peer not running): {}", msg.text),
            });
            return;
        }
        // Neutralny prefiks — bez tożsamości nadawcy/odbiorcy (LLM nie ma wiedzieć, kto z kim
        // rozmawia). Instrukcja o odpowiedzi jest potrzebna do round-tripu i nie zdradza tożsamości.
        let injected = format!("[incoming message — to reply, call the send_to_peer tool]: {}", msg.text);
        let outcome = match peer.proc.as_mut().unwrap().write_input(&injected) {
            Ok(()) => {
                peer.submitted = true;
                crate::pending::Outcome::Delivered
            }
            Err(e) => crate::pending::Outcome::Error(format!("write failed: {e}")),
        };
        let delivered = matches!(outcome, crate::pending::Outcome::Delivered);
        let _ = msg.responder.send(outcome);
        let _ = self.timeline.append(Entry {
            ts: now_ts(),
            from: if auto {
                format!("{} (auto)", msg.from.label())
            } else {
                msg.from.label().to_string()
            },
            to: msg.to.label().into(),
            kind: if delivered { Kind::Message } else { Kind::Event },
            text: if delivered {
                msg.text
            } else {
                format!("peer message not delivered: {}", msg.text)
            },
        });
    }

    /// Drenuje kolejkę pending w trybie auto: auto-zatwierdza wiadomości aż do
    /// wyczerpania budżetu lub opróżnienia kolejki. Po wyczerpaniu wyłącza auto.
    pub fn tick_auto(&mut self) {
        loop {
            let remaining = match self.auto {
                Some(n) if n > 0 => n,
                _ => return,
            };
            if self.pending.lock().unwrap().is_empty() {
                return;
            }
            self.moderate_pending(true, true);
            if remaining - 1 == 0 {
                self.auto = None;
                let _ = self.timeline.append(Entry {
                    ts: now_ts(),
                    from: "parley".into(),
                    to: "user".into(),
                    kind: Kind::Event,
                    text: "auto mode ended".into(),
                });
                return;
            }
            self.auto = Some(remaining - 1);
        }
    }

    /// Meta-komenda `/auto N` (włącza, klamruje do auto_max) lub `/auto off` (wyłącza).
    fn handle_auto_command(&mut self, arg: &str) {
        let note = if arg == "off" {
            self.auto = None;
            "auto mode off".to_string()
        } else if let Ok(n) = arg.parse::<u32>() {
            if n == 0 {
                "auto: N must be >= 1 (use /auto <number> or /auto off)".to_string()
            } else {
                let capped = n.min(self.auto_max);
                self.auto = Some(capped);
                format!("auto mode on ({capped} messages)")
            }
        } else {
            "auto: usage /auto <number> or /auto off".to_string()
        };
        let _ = self.timeline.append(Entry {
            ts: now_ts(),
            from: "parley".into(),
            to: "user".into(),
            kind: Kind::Event,
            text: note,
        });
    }

    /// Meta-komenda `/discuss [N] <temat>`: wysyła temat TYLKO do sfokusowanego agenta
    /// (inicjatora) i włącza auto na N wiadomości peer↔peer. Dalsza wymiana toczy się
    /// przez send_to_peer → kolejkę moderacji (serializowaną), więc bez przeplatania.
    fn handle_discuss_command(&mut self, rest: &str) {
        let (n, topic) = match parse_discuss_args(rest, self.auto_max) {
            Ok(v) => v,
            Err(msg) => {
                let _ = self.timeline.append(Entry {
                    ts: now_ts(),
                    from: "parley".into(),
                    to: "user".into(),
                    kind: Kind::Event,
                    text: msg.into(),
                });
                return;
            }
        };
        // Kickoff to wiadomość user→agent (write_input) — NIE przechodzi przez kolejkę,
        // więc nie konsumuje licznika auto. Licznik liczy tylko send_to_peer.
        let kickoff = format!(
            "Start a discussion with the other agent about: {topic}. Send your opening message using the send_to_peer tool, then keep the back-and-forth going."
        );
        let id = self.focus;
        let pane = self.pane_mut(id);
        let (kind, logged) = match pane.proc.as_mut() {
            Some(p) if pane.exited.is_none() => match p.write_input(&kickoff) {
                Ok(()) => {
                    pane.submitted = true;
                    (Kind::Message, format!("discuss ({n} messages) — kickoff: {topic}"))
                }
                Err(e) => (Kind::Event, format!("discuss undelivered (write error: {e})")),
            },
            _ => (Kind::Event, "discuss undelivered (agent not running)".to_string()),
        };
        // Auto włączamy tylko gdy kickoff faktycznie poszedł — inaczej zostawiamy moderację ręczną.
        if matches!(kind, Kind::Message) {
            self.auto = Some(n);
        }
        let _ = self.timeline.append(Entry {
            ts: now_ts(),
            from: "user".into(),
            to: id.label().into(),
            kind,
            text: logged,
        });
    }

    /// Rozwiązuje wszystkie oczekujące błędem — odmraża handlery agentów przy wyjściu.
    pub fn resolve_all_pending(&mut self) {
        let mut q = self.pending.lock().unwrap();
        while let Some(msg) = q.pop_front() {
            let _ = msg
                .responder
                .send(crate::pending::Outcome::Error("parley shutting down".into()));
        }
    }

    /// Startuje agenta z pełnej komendy startowej; błąd ląduje w pane.status.
    pub fn spawn_agent(&mut self, id: AgentId) {
        let (rows, cols) = self.pty_sizes[id.idx()];
        let cwd = self.cwd.clone();
        let pane = self.pane_mut(id);
        let command = pane.cfg.full_command();
        match AgentProcess::spawn(&command, &cwd, rows, cols) {
            Ok(p) => {
                pane.proc = Some(p);
                pane.exited = None;
                pane.status = "running".into();
                pane.submitted = false;
            }
            Err(e) => {
                pane.proc = None;
                pane.status = format!("start failed: {e}");
            }
        }
    }

    /// Ctrl+R: restart sfokusowanego agenta po crashu, z resume_command jeśli jest.
    fn restart_focused(&mut self) {
        let id = self.focus;
        let (rows, cols) = self.pty_sizes[id.idx()];
        let pane = self.pane_mut(id);
        if pane.exited.is_none() && pane.proc.is_some() {
            let label = pane.id.label().to_string();
            let _ = self.timeline.append(Entry {
                ts: now_ts(),
                from: "parley".into(),
                to: label,
                kind: Kind::Event,
                text: "restart ignored — agent is running".into(),
            });
            return; // żyje — nic do restartu
        }
        let (command, resumed) = restart_command(&pane.cfg, pane.submitted);
        let cwd = self.cwd.clone();
        let pane = self.pane_mut(id);
        let note = match AgentProcess::spawn(&command, &cwd, rows, cols) {
            Ok(p) => {
                pane.proc = Some(p);
                pane.exited = None;
                pane.status = "running".into();
                pane.submitted = false;
                if resumed { "restarted (session resumed)".to_string() } else { "restarted (fresh session)".to_string() }
            }
            Err(e) => {
                let note = format!("restart failed: {e}");
                pane.status = note.clone();
                note
            }
        };
        let _ = self.timeline.append(Entry {
            ts: now_ts(),
            from: "parley".into(),
            to: id.label().into(),
            kind: Kind::Event,
            text: note,
        });
    }

    /// Tick: wykrywa zakończone procesy, loguje zdarzenie.
    pub fn poll_agents(&mut self) {
        let mut exited_ids: Vec<AgentId> = Vec::new();
        for idx in 0..self.panes.len() {
            let pane = &mut self.panes[idx];
            if pane.exited.is_some() {
                continue;
            }
            if let Some(p) = pane.proc.as_mut() {
                if let Some(code) = p.try_exit() {
                    pane.exited = Some(code);
                    pane.status = format!("process exited (code {code}) — Ctrl+R = restart");
                    let label = pane.id.label().to_string();
                    let _ = self.timeline.append(Entry {
                        ts: now_ts(),
                        from: "parley".into(),
                        to: label,
                        kind: Kind::Event,
                        text: format!("process exited (code {code})"),
                    });
                    exited_ids.push(pane.id);
                }
            }
        }
        // Jeśli tryb Passthrough i sfokusowany agent właśnie umarł — wróć do Input.
        if self.mode == Mode::Passthrough && exited_ids.contains(&self.focus) {
            self.mode = Mode::Input;
        }
    }

    /// Resize paneli: nowy rozmiar PTY per pane.
    pub fn resize_ptys(&mut self, sizes: [(u16, u16); 2]) {
        self.pty_sizes = sizes;
        for pane in &mut self.panes {
            let (rows, cols) = sizes[pane.id.idx()];
            if let Some(p) = pane.proc.as_mut() {
                let _ = p.resize(rows, cols);
            }
        }
    }

    /// Graceful shutdown obu agentów równolegle (każdy czeka do ~2 s).
    pub fn shutdown(&mut self) {
        let handles: Vec<_> = self
            .panes
            .iter_mut()
            .filter_map(|pane| pane.proc.take())
            .map(|p| std::thread::spawn(move || p.shutdown()))
            .collect();
        for h in handles {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let dir = tempfile::tempdir().unwrap();
        let tl = Timeline::open(&dir.path().join("timeline.jsonl")).unwrap();
        let cwd = dir.path().to_path_buf();
        // tempdir musi przeżyć App w testach — leak jest tu akceptowalny
        std::mem::forget(dir);
        App::new(Config::default(), tl, cwd, crate::pending::new_queue())
    }

    fn test_app_with_live_agent() -> App {
        let mut app = test_app();
        app.pane_mut(AgentId::Claude).cfg = AgentConfig {
            command: "sh".into(),
            args: vec!["-c".into(), "sleep 30".into()],
            resume_command: None,
        };
        app.spawn_agent(AgentId::Claude);
        assert!(app.panes[0].proc.is_some());
        app
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn type_line(app: &mut App, line: &str) {
        for c in line.chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    }

    #[test]
    fn auto_command_enables_and_off() {
        let mut app = test_app();
        type_line(&mut app, "/auto 3");
        assert_eq!(app.auto, Some(3));
        assert_eq!(app.input, "");
        type_line(&mut app, "/auto off");
        assert_eq!(app.auto, None);
    }

    #[test]
    fn auto_command_clamps_to_max() {
        let mut app = test_app();
        app.auto_max = 5;
        type_line(&mut app, "/auto 9999");
        assert_eq!(app.auto, Some(5));
    }

    #[test]
    fn auto_command_invalid_ignored() {
        let mut app = test_app();
        for input in ["/auto 0", "/auto x", "/auto"] {
            type_line(&mut app, input);
            assert_eq!(app.auto, None, "input: {input}");
        }
    }

    #[test]
    fn parse_discuss_args_variants() {
        // jawne N + temat
        assert_eq!(parse_discuss_args("4 monorepo", 10), Ok((4, "monorepo".into())));
        // N pominięte → default = auto_max, całość to temat
        assert_eq!(
            parse_discuss_args("monorepo vs polyrepo", 10),
            Ok((10, "monorepo vs polyrepo".into()))
        );
        // jednowyrazowy temat bez N
        assert_eq!(parse_discuss_args("rust", 10), Ok((10, "rust".into())));
        // klamrowanie do auto_max
        assert_eq!(parse_discuss_args("9999 big", 5), Ok((5, "big".into())));
        // błędy
        assert!(parse_discuss_args("", 10).is_err()); // pusto
        assert!(parse_discuss_args("5", 10).is_err()); // sama liczba, brak tematu
        assert!(parse_discuss_args("0 foo", 10).is_err()); // N == 0
    }

    #[test]
    fn discuss_enables_auto_and_delivers_to_focused() {
        let mut app = test_app_with_live_agent(); // fokus = claude, proces żyje
        type_line(&mut app, "/discuss 3 talk about rust");
        assert_eq!(app.auto, Some(3));
        assert_eq!(app.input, "");
        assert!(app.pane(AgentId::Claude).submitted);
        let last = app.timeline.entries.last().unwrap();
        assert_eq!(last.to, "claude");
        assert_eq!(last.kind, Kind::Message);
        assert!(last.text.contains("discuss"));
        app.shutdown();
    }

    #[test]
    fn discuss_without_n_defaults_to_auto_max() {
        let mut app = test_app_with_live_agent();
        app.auto_max = 7;
        type_line(&mut app, "/discuss just chat");
        assert_eq!(app.auto, Some(7));
        app.shutdown();
    }

    #[test]
    fn discuss_not_delivered_keeps_auto_off() {
        let mut app = test_app(); // proc=None — kickoff nie dojdzie
        type_line(&mut app, "/discuss 3 hej");
        assert_eq!(app.auto, None); // auto NIE włączone gdy kickoff nie poszedł
        let last = app.timeline.entries.last().unwrap();
        assert_eq!(last.kind, Kind::Event);
        assert!(last.text.contains("undelivered"));
    }

    #[test]
    fn tab_toggles_focus_in_input_mode() {
        let mut app = test_app();
        assert_eq!(app.focus, AgentId::Claude);
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.focus, AgentId::Codex);
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.focus, AgentId::Claude);
    }

    #[test]
    fn ctrl_bracket_toggles_passthrough() {
        let mut app = test_app_with_live_agent();
        assert!(matches!(app.mode, Mode::Input));
        app.handle_key(key(KeyCode::Char(']'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Passthrough));
        app.handle_key(key(KeyCode::Char(']'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Input));
        app.shutdown();
    }

    #[test]
    fn passthrough_blocked_when_focused_agent_not_running() {
        let mut app = test_app(); // proc=None
        app.handle_key(key(KeyCode::Char(']'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Input)); // brak żywego agenta — zostajemy w Input
    }

    #[test]
    fn ctrl_q_in_passthrough_goes_to_agent_not_confirm() {
        let mut app = test_app_with_live_agent();
        app.handle_key(key(KeyCode::Char(']'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Passthrough));
        app.handle_key(key(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert!(!app.confirm_quit);
        assert!(!app.should_quit);
        app.shutdown();
    }

    #[test]
    fn typing_builds_input_and_esc_clears() {
        let mut app = test_app();
        for c in "abc".chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.input, "abc");
        app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.input, "ab");
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.input, "");
    }

    #[test]
    fn enter_logs_undelivered_when_agent_down() {
        let mut app = test_app(); // proc=None dla obu agentów
        for c in "@codex zrób coś".chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.input, "");
        let last = app.timeline.entries.last().unwrap();
        assert_eq!(last.to, "codex");
        assert_eq!(last.kind, Kind::Event);
        assert!(last.text.contains("undelivered"));
    }

    #[test]
    fn empty_text_after_target_is_not_delivered() {
        let mut app = test_app();
        for c in "@claude".chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        // samo "@claude" bez treści — nie generuje wpisu ani doręczenia
        assert!(app.timeline.entries.is_empty());
    }

    #[test]
    fn ctrl_5_legacy_byte_toggles_passthrough() {
        // crossterm legacy: fizyczny Ctrl+] przychodzi jako Ctrl+'5' (bajt 0x1D)
        let mut app = test_app_with_live_agent();
        app.handle_key(key(KeyCode::Char('5'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Passthrough));
        app.handle_key(key(KeyCode::Char('5'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Input));
        app.shutdown();
    }

    #[test]
    fn unknown_target_logs_event_and_does_not_deliver() {
        let mut app = test_app();
        for c in "@obaj hej".chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        let last = app.timeline.entries.last().unwrap();
        assert_eq!(last.kind, Kind::Event);
        assert!(last.text.contains("unknown target @obaj"));
    }

    #[test]
    fn ctrl_q_asks_for_confirmation() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert!(app.confirm_quit);
        assert!(!app.should_quit);
        app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(!app.confirm_quit);
        app.handle_key(key(KeyCode::Char('q'), KeyModifiers::CONTROL));
        app.handle_key(key(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn double_ctrl_c_quits() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.confirm_quit);
        assert!(!app.should_quit);
        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn other_key_cancels_quit_confirmation() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.confirm_quit);
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.confirm_quit);
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_c_in_passthrough_goes_to_agent() {
        let mut app = test_app_with_live_agent();
        app.handle_key(key(KeyCode::Char(']'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Passthrough));
        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!app.confirm_quit); // Ctrl+C poszedł do agenta, nie do parley
        app.shutdown();
    }

    #[test]
    fn no_prefix_routes_to_focused_agent() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE)); // fokus na codex
        for c in "hej".chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        let last = app.timeline.entries.last().unwrap();
        assert_eq!(last.to, "codex"); // poszło do agenta w fokusie (undelivered, bo proc=None)
    }

    #[test]
    fn ctrl_r_on_running_agent_logs_feedback() {
        let mut app = test_app_with_live_agent(); // fokus = claude, proces żyje
        app.handle_key(key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        let last = app.timeline.entries.last().unwrap();
        assert_eq!(last.kind, Kind::Event);
        assert!(last.text.contains("restart ignored"));
        app.shutdown();
    }

    #[test]
    fn restart_resumes_only_after_submission() {
        let cfg = AgentConfig {
            command: "claude".into(),
            args: vec![],
            resume_command: Some(vec!["claude".into(), "--continue".into()]),
        };
        // pusta sesja → świeży start (CLI nie zapisał sesji, resume wziąłby starszą)
        assert_eq!(restart_command(&cfg, false), (vec!["claude".to_string()], false));
        // po wysłaniu promptu → resume
        assert_eq!(
            restart_command(&cfg, true),
            (vec!["claude".to_string(), "--continue".to_string()], true)
        );
    }

    #[test]
    fn submit_marks_pane_as_submitted() {
        let mut app = test_app_with_live_agent(); // żywy fake-claude (sleep)
        assert!(!app.pane(AgentId::Claude).submitted);
        for c in "@claude hej".chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.pane(AgentId::Claude).submitted);
        app.shutdown();
    }

    #[test]
    fn passthrough_enter_marks_submitted() {
        let mut app = test_app_with_live_agent();
        app.handle_key(key(KeyCode::Char(']'), KeyModifiers::CONTROL));
        app.handle_key(key(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(!app.pane(AgentId::Claude).submitted); // samo pisanie nie liczy się
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.pane(AgentId::Claude).submitted);
        app.shutdown();
    }

    fn pending_msg(
        from: AgentId,
        to: AgentId,
        text: &str,
    ) -> (crate::pending::PendingMessage, tokio::sync::oneshot::Receiver<crate::pending::Outcome>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        (
            crate::pending::PendingMessage { from, to, text: text.into(), responder: tx },
            rx,
        )
    }

    #[test]
    fn tick_auto_approves_and_decrements() {
        let mut app = test_app();
        app.auto = Some(2);
        // peer (codex) nie działa — auto-approve da Error, ale licznik i tak dekrementuje
        let (msg, mut rx) = pending_msg(AgentId::Claude, AgentId::Codex, "x");
        app.pending.lock().unwrap().push_back(msg);
        app.tick_auto();
        assert!(matches!(rx.try_recv(), Ok(crate::pending::Outcome::Error(_))));
        assert_eq!(app.auto, Some(1));
        assert!(app.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn tick_auto_turns_off_at_zero() {
        let mut app = test_app();
        app.auto = Some(1);
        let (msg, _rx) = pending_msg(AgentId::Claude, AgentId::Codex, "x");
        app.pending.lock().unwrap().push_back(msg);
        app.tick_auto();
        assert_eq!(app.auto, None);
        assert!(app.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn tick_auto_idle_when_off() {
        let mut app = test_app(); // auto = None
        let (msg, mut rx) = pending_msg(AgentId::Claude, AgentId::Codex, "x");
        app.pending.lock().unwrap().push_back(msg);
        app.tick_auto();
        assert!(rx.try_recv().is_err()); // nie rozwiązane
        assert_eq!(app.pending.lock().unwrap().len(), 1);
    }

    #[test]
    fn esc_aborts_auto_else_clears_input() {
        let mut app = test_app();
        app.auto = Some(3);
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.auto, None);
        for c in "abc".chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.input, "");
    }

    #[test]
    fn reject_resolves_and_logs_event() {
        let mut app = test_app();
        let (msg, mut rx) = pending_msg(AgentId::Claude, AgentId::Codex, "hej peer");
        app.pending.lock().unwrap().push_back(msg);
        app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE)); // reject = n
        assert!(matches!(rx.try_recv(), Ok(crate::pending::Outcome::Rejected)));
        assert!(app.pending.lock().unwrap().is_empty());
        let last = app.timeline.entries.last().unwrap();
        assert_eq!(last.kind, Kind::Event);
        assert!(last.text.contains("rejected"));
    }

    #[test]
    fn approve_without_running_peer_resolves_error() {
        let mut app = test_app(); // proc=None dla obu
        let (msg, mut rx) = pending_msg(AgentId::Claude, AgentId::Codex, "hej peer");
        app.pending.lock().unwrap().push_back(msg);
        app.handle_key(key(KeyCode::Char('y'), KeyModifiers::NONE)); // approve = y
        match rx.try_recv() {
            Ok(crate::pending::Outcome::Error(_)) => {}
            other => panic!("oczekiwano Error, było {other:?}"),
        }
        assert!(app.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn question_mark_opens_help_when_empty_and_any_key_closes() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(app.show_help);
        assert_eq!(app.input, ""); // '?' nie trafił do inputu
        app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!app.show_help);
        assert_eq!(app.input, ""); // klawisz zamykający też nie trafia do inputu
    }

    #[test]
    fn question_mark_is_literal_when_input_nonempty() {
        let mut app = test_app();
        app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE));
        app.handle_key(key(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(!app.show_help);
        assert_eq!(app.input, "a?"); // w trakcie pisania '?' to zwykły znak
    }

    #[test]
    fn help_commands_open_help() {
        for cmd in ["/help", "/?"] {
            let mut app = test_app();
            type_line(&mut app, cmd);
            assert!(app.show_help, "cmd: {cmd}");
            assert_eq!(app.input, "");
            assert!(app.timeline.entries.is_empty()); // komenda pomocy nic nie loguje
        }
    }

    #[test]
    fn question_mark_in_passthrough_goes_to_agent_not_help() {
        let mut app = test_app_with_live_agent();
        app.handle_key(key(KeyCode::Char(']'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Passthrough));
        app.handle_key(key(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(!app.show_help); // w passthrough '?' idzie do agenta
        app.shutdown();
    }

    #[test]
    fn passthrough_exits_when_focused_agent_dies() {
        let dir = tempfile::tempdir().unwrap();
        let tl = Timeline::open(&dir.path().join("timeline.jsonl")).unwrap();
        let cwd = dir.path().to_path_buf();
        std::mem::forget(dir);
        let mut app = App::new(Config::default(), tl, cwd, crate::pending::new_queue());
        app.pane_mut(AgentId::Claude).cfg = AgentConfig {
            command: "sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
            resume_command: None,
        };
        app.spawn_agent(AgentId::Claude);
        app.handle_key(key(KeyCode::Char(']'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Passthrough));
        // czekaj aż proces umrze i poll go wykryje
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            app.poll_agents();
            if app.pane(AgentId::Claude).exited.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(app.pane(AgentId::Claude).exited.is_some());
        assert!(matches!(app.mode, Mode::Input)); // automatyczny powrót
        app.shutdown();
    }
}
