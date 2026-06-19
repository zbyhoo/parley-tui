//! Przezroczysty proxy PTY: agent rysuje wprost na terminal użytkownika; wrapper
//! może wstrzyknąć tekst do stdin agenta (wiadomości od peerów).
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// Clone-able handle used by the poll thread to inject messages into the agent stdin.
#[derive(Clone)]
pub struct ProxyHandle {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl ProxyHandle {
    /// Write `text` into agent stdin, pause 75 ms, then send `\r`.
    pub fn inject(&self, text: &str) {
        {
            let mut w = self.writer.lock().unwrap();
            let _ = w.write_all(text.as_bytes());
            let _ = w.flush();
        }
        std::thread::sleep(Duration::from_millis(75));
        {
            let mut w = self.writer.lock().unwrap();
            let _ = w.write_all(b"\r");
            let _ = w.flush();
        }
    }
}

/// Owned half: holds the child process, master PTY, and original termios.
/// Call `wait(self)` on the main thread to wait for child exit.
pub struct ProxyChild {
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    orig_termios: Option<libc::termios>,
}

impl ProxyChild {
    /// Wait for the child process to exit, restore terminal, return exit code.
    pub fn wait(mut self) -> i32 {
        let code = self.child.wait().map(|s| s.exit_code() as i32).unwrap_or(1);
        if let Some(orig) = self.orig_termios.take() {
            restore_raw(&orig);
        }
        code
    }

    /// Resize the PTY to match the current terminal size.
    pub fn resize_to_current(&self) {
        let (rows, cols) = term_size();
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }
}

/// Entry point: spawns the PTY and returns a (handle, child) pair.
pub struct Proxy;

impl Proxy {
    pub fn spawn(command: &[String], cwd: &Path) -> Result<(ProxyHandle, ProxyChild)> {
        let (rows, cols) = term_size();
        let (program, args) = command.split_first().context("empty command")?;
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| anyhow::anyhow!("openpty: {e}"))?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.cwd(cwd);
        let child = pair.slave.spawn_command(cmd).map_err(|e| anyhow::anyhow!("spawn: {e}"))?;
        drop(pair.slave);

        let mut reader =
            pair.master.try_clone_reader().map_err(|e| anyhow::anyhow!("reader: {e}"))?;
        let writer: Box<dyn Write + Send> =
            pair.master.take_writer().map_err(|e| anyhow::anyhow!("writer: {e}"))?;
        let writer = Arc::new(Mutex::new(writer));

        // master → stdout
        std::thread::spawn(move || {
            let mut out = std::io::stdout();
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out.write_all(&buf[..n]).is_err() {
                            break;
                        }
                        let _ = out.flush();
                    }
                }
            }
        });

        // Enter raw mode before starting stdin thread (returns None when not a tty).
        let orig_termios = enter_raw();

        // stdin → master
        {
            let writer = Arc::clone(&writer);
            std::thread::spawn(move || {
                let mut inp = std::io::stdin();
                let mut buf = [0u8; 4096];
                loop {
                    match inp.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut w = writer.lock().unwrap();
                            if w.write_all(&buf[..n]).is_err() {
                                break;
                            }
                            let _ = w.flush();
                        }
                    }
                }
            });
        }

        let handle = ProxyHandle { writer };
        let child_part = ProxyChild { child, master: pair.master, orig_termios };
        Ok((handle, child_part))
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn term_size() -> (u16, u16) {
    #[cfg(unix)]
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 {
            return (ws.ws_row, ws.ws_col);
        }
    }
    (24, 80)
}

fn enter_raw() -> Option<libc::termios> {
    #[cfg(unix)]
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) != 0 {
            return None;
        }
        let orig = t;
        libc::cfmakeraw(&mut t);
        libc::tcsetattr(0, libc::TCSANOW, &t);
        return Some(orig);
    }
    #[allow(unreachable_code)]
    None
}

fn restore_raw(orig: &libc::termios) {
    #[cfg(unix)]
    unsafe {
        libc::tcsetattr(0, libc::TCSANOW, orig);
    }
}
