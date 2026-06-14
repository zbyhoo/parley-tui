use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;

use crate::router::AgentId;

#[derive(Debug, Clone, PartialEq)]
pub struct AgentConfig {
    pub command: String,
    pub args: Vec<String>,
    /// Pełna komenda wznowienia sesji po crashu (program + argumenty).
    /// None = wznowienie niedostępne, restart startuje czystą sesję.
    pub resume_command: Option<Vec<String>>,
}

impl AgentConfig {
    pub fn full_command(&self) -> Vec<String> {
        let mut v = vec![self.command.clone()];
        v.extend(self.args.iter().cloned());
        v
    }
}

/// Domyślny górny limit dla `/auto N` (ochrona przed runaway).
pub const DEFAULT_AUTO_MAX: u32 = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub claude: AgentConfig,
    pub codex: AgentConfig,
    /// Katalog stanu (timeline itd.); None = .parley/ w katalogu roboczym.
    pub state_dir: Option<PathBuf>,
    /// Górny limit liczby auto-zatwierdzanych wiadomości (`/auto N` klamrowane do tego).
    pub auto_max: u32,
}

/// Surowa postać pliku — wszystkie pola opcjonalne; brakujące uzupełniamy defaultami.
#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    claude: Option<AgentConfigFile>,
    codex: Option<AgentConfigFile>,
    state_dir: Option<PathBuf>,
    auto_max: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AgentConfigFile {
    command: Option<String>,
    args: Option<Vec<String>>,
    resume_command: Option<Vec<String>>,
}

fn merge(base: AgentConfig, file: Option<AgentConfigFile>) -> AgentConfig {
    match file {
        None => base,
        Some(f) => {
            // Nadpisanie command unieważnia domyślne resume_command —
            // default odnosi się do domyślnego programu, nie do nadpisanego.
            let resume_default = if f.command.is_some() { None } else { base.resume_command };
            AgentConfig {
                command: f.command.unwrap_or(base.command),
                args: f.args.unwrap_or(base.args),
                resume_command: f.resume_command.or(resume_default),
            }
        }
    }
}

fn default_claude() -> AgentConfig {
    AgentConfig {
        command: "claude".into(),
        args: vec![],
        resume_command: Some(vec!["claude".into(), "--continue".into()]),
    }
}

fn default_codex() -> AgentConfig {
    AgentConfig {
        command: "codex".into(),
        args: vec![],
        // Ustalone w spike'u (gate): codex resume --last wznawia ostatnią sesję w cwd.
        resume_command: Some(vec!["codex".into(), "resume".into(), "--last".into()]),
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            claude: default_claude(),
            codex: default_codex(),
            state_dir: None,
            auto_max: DEFAULT_AUTO_MAX,
        }
    }
}

impl Config {
    /// Czyta `<cwd>/.parley/config.toml`; brak pliku => Default.
    /// Sekcje/pola pominięte w pliku dostają wartości domyślne (merge).
    pub fn load(cwd: &Path) -> anyhow::Result<Self> {
        let path = cwd.join(".parley").join("config.toml");
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => {
                return Err(anyhow::Error::new(e)
                    .context(format!("cannot read {}", path.display())))
            }
        };
        let file: ConfigFile = toml::from_str(&raw)
            .with_context(|| format!("invalid TOML in {}", path.display()))?;
        Ok(Config {
            claude: merge(default_claude(), file.claude),
            codex: merge(default_codex(), file.codex),
            state_dir: file.state_dir,
            auto_max: file.auto_max.unwrap_or(DEFAULT_AUTO_MAX),
        })
    }
}

/// Dodatkowe argumenty CLI wstrzykujące konfigurację brokera MCP (wynik spike'a 2026-06-13).
/// Codex: inline `-c` (url + nagłówek X-Agent-Id + auto-approval narzędzia).
/// Claude: `--mcp-config <plik>` (plik pisany przez `write_claude_mcp_config` w `.parley/`)
/// + allowlista narzędzia, bez promptu uprawnień.
pub fn mcp_extra_args(agent: AgentId, port: u16, claude_config_path: &Path) -> Vec<String> {
    match agent {
        AgentId::Codex => vec![
            "-c".into(),
            format!("mcp_servers.parley.url=\"http://127.0.0.1:{port}/mcp\""),
            "-c".into(),
            "mcp_servers.parley.http_headers={ \"X-Agent-Id\" = \"codex\" }".into(),
            "-c".into(),
            "mcp_servers.parley.tools.send_to_peer.approval_mode=\"auto\"".into(),
        ],
        AgentId::Claude => vec![
            "--mcp-config".into(),
            claude_config_path.to_string_lossy().into_owned(),
            "--allowedTools".into(),
            "mcp__parley__send_to_peer".into(),
        ],
    }
}

/// Zapisuje plik konfiguracji MCP dla Claude (wskazywany przez `--mcp-config`).
/// Trzymany w `.parley/` (już w .gitignore), więc nie zaśmieca projektu.
pub fn write_claude_mcp_config(path: &Path, port: u16) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = format!(
        "{{\n  \"mcpServers\": {{\n    \"parley\": {{\n      \"type\": \"http\",\n      \"url\": \"http://127.0.0.1:{port}/mcp\",\n      \"headers\": {{ \"X-Agent-Id\": \"claude\" }}\n    }}\n  }}\n}}\n"
    );
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_max_default_and_override() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.auto_max, 20);

        std::fs::create_dir_all(dir.path().join(".parley")).unwrap();
        std::fs::write(dir.path().join(".parley/config.toml"), "auto_max = 5\n").unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.auto_max, 5);
    }

    #[test]
    fn codex_mcp_args_inline() {
        let args = mcp_extra_args(AgentId::Codex, 8765, Path::new("/unused"));
        assert!(args.contains(&"-c".to_string()));
        assert!(args
            .iter()
            .any(|a| a.contains("mcp_servers.parley.url=\"http://127.0.0.1:8765/mcp\"")));
        assert!(args.iter().any(|a| a.contains("X-Agent-Id") && a.contains("codex")));
        assert!(args.iter().any(|a| a.contains("approval_mode") && a.contains("auto")));
    }

    #[test]
    fn claude_mcp_args_use_flag() {
        let args = mcp_extra_args(AgentId::Claude, 8765, Path::new("/tmp/.parley/claude-mcp.json"));
        assert!(args.contains(&"--mcp-config".to_string()));
        assert!(args.contains(&"/tmp/.parley/claude-mcp.json".to_string()));
        assert!(args.contains(&"--allowedTools".to_string()));
        assert!(args.contains(&"mcp__parley__send_to_peer".to_string()));
    }

    #[test]
    fn claude_config_file_has_url_and_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".parley/claude-mcp.json");
        write_claude_mcp_config(&path, 8765).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("http://127.0.0.1:8765/mcp"));
        assert!(content.contains("\"X-Agent-Id\": \"claude\""));
    }

    #[test]
    fn default_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.claude.command, "claude");
        assert_eq!(cfg.codex.command, "codex");
        assert_eq!(cfg.claude.resume_command, Some(vec!["claude".into(), "--continue".into()]));
        assert_eq!(cfg.codex.resume_command, Some(vec!["codex".into(), "resume".into(), "--last".into()]));
        assert!(cfg.state_dir.is_none());
    }

    #[test]
    fn parses_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".parley")).unwrap();
        std::fs::write(
            dir.path().join(".parley/config.toml"),
            r#"
state_dir = "/tmp/parley-state"

[claude]
command = "claude"
args = ["--permission-mode", "plan"]

[codex]
command = "codex"
resume_command = ["codex", "resume", "--last"]
"#,
        )
        .unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.claude.args, vec!["--permission-mode", "plan"]);
        assert_eq!(cfg.codex.resume_command, Some(vec!["codex".into(), "resume".into(), "--last".into()]));
        assert_eq!(cfg.state_dir, Some(PathBuf::from("/tmp/parley-state")));
    }

    #[test]
    fn full_command_joins_program_and_args() {
        let ac = AgentConfig {
            command: "claude".into(),
            args: vec!["--foo".into()],
            resume_command: None,
        };
        assert_eq!(ac.full_command(), vec!["claude".to_string(), "--foo".to_string()]);
    }

    #[test]
    fn partial_section_merges_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".parley")).unwrap();
        std::fs::write(
            dir.path().join(".parley/config.toml"),
            "[claude]\nargs = [\"--permission-mode\", \"plan\"]\n",
        )
        .unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.claude.command, "claude"); // z defaultu
        assert_eq!(cfg.claude.args, vec!["--permission-mode", "plan"]); // z pliku
        assert_eq!(cfg.claude.resume_command, Some(vec!["claude".into(), "--continue".into()])); // z defaultu
        assert_eq!(cfg.codex.command, "codex"); // cała sekcja z defaultu
    }

    #[test]
    fn corrupt_toml_returns_err_with_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".parley")).unwrap();
        std::fs::write(dir.path().join(".parley/config.toml"), "[claude\nzepsute").unwrap();
        let err = Config::load(dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("config.toml"), "error should point to file: {err:#}");
    }

    #[test]
    fn overridden_command_does_not_inherit_default_resume() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".parley")).unwrap();
        std::fs::write(
            dir.path().join(".parley/config.toml"),
            "[codex]\ncommand = \"my-custom-agent\"\n",
        )
        .unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.codex.command, "my-custom-agent");
        assert_eq!(cfg.codex.resume_command, None); // nie dziedziczy `codex resume --last`
    }
}
