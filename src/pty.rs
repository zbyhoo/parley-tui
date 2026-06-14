use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

/// Jeden agent CLI w PTY: proces potomny + parser VT + kanały IO.
pub struct AgentProcess {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Option<Box<dyn Write + Send>>,
    child: Box<dyn Child + Send + Sync>,
    master: Option<Box<dyn MasterPty + Send>>,
}

impl AgentProcess {
    /// `command` = [program, arg, ...]; spawn w PTY rows×cols, cwd ustawione.
    pub fn spawn(command: &[String], cwd: &Path, rows: u16, cols: u16) -> Result<Self> {
        let (program, args) = command.split_first().context("empty agent command")?;
        if which(program).is_none() {
            bail!("binary '{program}' not found in PATH — check .parley/config.toml");
        }
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| anyhow::anyhow!("openpty: {e}"))?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.cwd(cwd);
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow::anyhow!("spawn '{program}': {e}"))?;
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 2000)));
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| anyhow::anyhow!("clone reader: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| anyhow::anyhow!("take writer: {e}"))?;
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

        Ok(AgentProcess {
            parser,
            writer: Some(writer),
            child,
            master: Some(pair.master),
        })
    }

    /// Wstrzykuje tekst jako prompt: treść, pauza, Enter (\r) osobnym zapisem.
    /// Pauza oddziela Enter od treści — TUI z detekcją wklejania (np. Codex)
    /// potraktowałyby \r wysłany w jednym strumieniu z tekstem jako nową linię
    /// we wklejce zamiast submitu.
    pub fn write_input(&mut self, text: &str) -> Result<()> {
        self.write_raw(text.as_bytes())?;
        std::thread::sleep(Duration::from_millis(75));
        self.write_raw(b"\r")
    }

    pub fn write_raw(&mut self, bytes: &[u8]) -> Result<()> {
        let w = self.writer.as_mut().context("PTY writer closed")?;
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        // najpierw parser, potem PTY — output wysłany po SIGWINCH ma już trafić
        // do parsera o nowym rozmiarze
        self.parser.lock().unwrap().screen_mut().set_size(rows, cols);
        if let Some(master) = &self.master {
            master
                .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
                .map_err(|e| anyhow::anyhow!("resize: {e}"))?;
        }
        Ok(())
    }

    /// Nieblokująco: Some(kod) jeśli proces się zakończył.
    pub fn try_exit(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.exit_code() as i32),
            _ => None,
        }
    }

    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        let parser = self.parser.lock().unwrap();
        f(parser.screen())
    }

    /// Graceful shutdown: zamknij PTY (CLI dostaje EOF/SIGHUP), poczekaj do 2 s, dobij.
    pub fn shutdown(mut self) {
        self.writer.take();
        self.master.take();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        // poczekaj na reaper (timeout 500 ms żeby uniknąć zombies)
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Sprawdz czy plik ma bit executable (unix: 0o111, windows: zawsze true dla is_file).
fn is_executable(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.is_file() && p.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Minimalny lookup w PATH (bez crate'a `which`).
fn which(program: &str) -> Option<std::path::PathBuf> {
    if program.contains('/') {
        let p = std::path::PathBuf::from(program);
        return is_executable(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(program)).find(|c| is_executable(c))
}
