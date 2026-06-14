use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Wiadomość użytkownika doręczona do agenta.
    Message,
    /// Zdarzenie sesji: crash, restart, błąd doręczenia itp.
    Event,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub ts: String,
    pub from: String,
    pub to: String,
    pub kind: Kind,
    pub text: String,
}

pub fn now_ts() -> String {
    chrono::Local::now().to_rfc3339()
}

pub struct Timeline {
    file: File,
    pub entries: Vec<Entry>,
}

impl Timeline {
    /// Otwiera (tworząc katalogi) plik JSONL w trybie append; wczytuje istniejące wpisy.
    /// Na unixach wymusza 0600 — timeline zawiera treść wiadomości użytkownika.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut opts = OpenOptions::new();
        opts.create(true).append(true).read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600); // skuteczne tylko przy tworzeniu pliku
        }
        let file = opts.open(path)?;
        #[cfg(unix)]
        {
            // egzekwuj 0600 także dla plików istniejących przed tą wersją
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        let mut entries = Vec::new();
        // Osobny uchwyt do odczytu: `file` jest w trybie append (kursor na końcu),
        // więc czytanie przez niego zwróciłoby pustkę.
        for line in BufReader::new(File::open(path)?).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Entry>(&line) {
                Ok(e) => entries.push(e),
                Err(_) => continue, // uszkodzona linia nie blokuje sesji
            }
        }
        Ok(Timeline { file, entries })
    }

    pub fn append(&mut self, entry: Entry) -> io::Result<()> {
        let line = serde_json::to_string(&entry)?;
        writeln!(self.file, "{line}")?;
        self.file.flush()?;
        self.entries.push(entry);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> Entry {
        Entry {
            ts: now_ts(),
            from: "user".into(),
            to: "claude".into(),
            kind: Kind::Message,
            text: text.into(),
        }
    }

    #[test]
    fn append_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1/timeline.jsonl");
        {
            let mut tl = Timeline::open(&path).unwrap();
            tl.append(entry("hello")).unwrap();
            tl.append(entry("world")).unwrap();
            assert_eq!(tl.entries.len(), 2);
        }
        let tl = Timeline::open(&path).unwrap();
        assert_eq!(tl.entries.len(), 2);
        assert_eq!(tl.entries[1].text, "world");
        assert_eq!(tl.entries[0].kind, Kind::Message);
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timeline.jsonl");
        let _tl = Timeline::open(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn corrupted_line_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timeline.jsonl");
        {
            let mut tl = Timeline::open(&path).unwrap();
            tl.append(entry("ok")).unwrap();
        }
        // dopisz śmieci (symulacja niepełnego zapisu po crashu)
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{\"ts\": \"niepełny json").unwrap();
        let tl = Timeline::open(&path).unwrap();
        assert_eq!(tl.entries.len(), 1);
        assert_eq!(tl.entries[0].text, "ok");
    }
}
