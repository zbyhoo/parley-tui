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

/// Wstrzykuje argi MCP do komendy startowej (`args`) i do `resume_command`.
/// Resume to osobny wektor — bez tego injekcja przepadałaby po wznowieniu/Ctrl+R
/// i peer messaging (`send_to_peer`) znikałby z sesji agenta. Argi trafiają po nazwie
/// binarki, przed subkomendą — codex wymaga `-c` przed `resume`.
pub fn inject_mcp_args(cfg: &mut AgentConfig, extra: Vec<String>) {
    cfg.args.extend(extra.iter().cloned());
    if let Some(resume) = cfg.resume_command.as_mut() {
        for (i, arg) in extra.into_iter().enumerate() {
            resume.insert(1 + i, arg);
        }
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

/// Argi codex dla dowolnego id + token (uogólnienie `mcp_extra_args` z Etapu MCP).
pub fn mcp_args_codex(id: &str, port: u16, token: &str) -> Vec<String> {
    vec![
        "-c".into(),
        format!("mcp_servers.parley.url=\"http://127.0.0.1:{port}/mcp\""),
        "-c".into(),
        format!(
            "mcp_servers.parley.http_headers={{ \"X-Agent-Id\" = \"{id}\", \"X-Parley-Token\" = \"{token}\" }}"
        ),
        "-c".into(),
        "mcp_servers.parley.tools.send_to_peer.approval_mode=\"auto\"".into(),
    ]
}

/// Flagi claude wskazujące plik konfiguracji MCP + allowlista narzędzi peerowych.
pub fn mcp_args_claude(_id: &str, config_path: &Path) -> Vec<String> {
    vec![
        "--mcp-config".into(),
        config_path.to_string_lossy().into_owned(),
        "--allowedTools".into(),
        "mcp__parley__send_to_peer,mcp__parley__list_peers".into(),
    ]
}

/// Plik JSON konfiguracji MCP dla claude (dowolne id + token).
pub fn write_mcp_config_json(
    path: &Path,
    id: &str,
    port: u16,
    token: &str,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = format!(
        "{{\n  \"mcpServers\": {{\n    \"parley\": {{\n      \"type\": \"http\",\n      \"url\": \"http://127.0.0.1:{port}/mcp\",\n      \"headers\": {{ \"X-Agent-Id\": \"{id}\", \"X-Parley-Token\": \"{token}\" }}\n    }}\n  }}\n}}\n"
    );
    std::fs::write(path, json)
}

/// Treść configu opencode (do zmiennej `OPENCODE_CONFIG_CONTENT`).
/// Opencode nie ma flagi `--mcp-config`; serwer MCP konfiguruje się plikiem/env.
/// Wstrzykujemy inline JSON per-proces: remote MCP brokera + nagłówki (id + token).
/// Uprawnienia zawężone (jak claude/codex): auto-approve TYLKO narzędzi parley
/// (`parley_*`), a mutujące/egress (`edit`→write/edit/apply_patch, `bash`, `webfetch`)
/// idą przez bramkę zatwierdzenia w TUI. Reszta zostaje na permisywnym defaulcie
/// opencode (read/grep/glob/list). Nie dajemy globalnego `"*":"allow"` — peer może
/// wstrzyknąć prompt, więc destrukcyjne operacje muszą mieć bramkę człowieka.
pub fn opencode_config_content(id: &str, port: u16, token: &str) -> String {
    format!(
        "{{\"$schema\":\"https://opencode.ai/config.json\",\
         \"mcp\":{{\"parley\":{{\"type\":\"remote\",\
         \"url\":\"http://127.0.0.1:{port}/mcp\",\"enabled\":true,\
         \"headers\":{{\"X-Agent-Id\":\"{id}\",\"X-Parley-Token\":\"{token}\"}}}}}},\
         \"permission\":{{\"parley_*\":\"allow\",\"edit\":\"ask\",\"bash\":\"ask\",\"webfetch\":\"ask\"}}}}"
    )
}

/// Dispatch po nazwie binarki: `codex*` → inline `-c`, reszta → flagi claude.
pub fn mcp_args_for(
    binary: &str,
    id: &str,
    port: u16,
    token: &str,
    claude_config_path: &Path,
) -> Vec<String> {
    let base = Path::new(binary).file_name().and_then(|s| s.to_str()).unwrap_or(binary);
    if base.starts_with("codex") {
        mcp_args_codex(id, port, token)
    } else {
        mcp_args_claude(id, claude_config_path)
    }
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
    fn inject_mcp_args_reaches_resume_command() {
        // Regresja: resume gubił injekcję MCP → send_to_peer znikał po Ctrl+R/wznowieniu.
        let mut cfg = default_codex();
        let extra = mcp_extra_args(AgentId::Codex, 8765, Path::new("/unused"));
        inject_mcp_args(&mut cfg, extra);

        // Komenda startowa dostaje argi.
        assert!(cfg.args.iter().any(|a| a.contains("mcp_servers.parley.url")));
        // Resume też je ma...
        let resume = cfg.resume_command.expect("codex ma resume_command");
        assert!(resume.iter().any(|a| a.contains("mcp_servers.parley.url")));
        // ...i `-c` jest przed subkomendą `resume` (codex tego wymaga).
        let first_c = resume.iter().position(|a| a == "-c").unwrap();
        let resume_pos = resume.iter().position(|a| a == "resume").unwrap();
        assert!(first_c < resume_pos, "argi -c muszą poprzedzać `resume`: {resume:?}");
        assert_eq!(resume[0], "codex", "nazwa binarki zostaje pierwsza: {resume:?}");
    }

    #[test]
    fn inject_mcp_args_skips_resume_when_none() {
        let mut cfg = AgentConfig {
            command: "codex".into(),
            args: vec![],
            resume_command: None,
        };
        inject_mcp_args(&mut cfg, mcp_extra_args(AgentId::Codex, 8765, Path::new("/unused")));
        assert!(cfg.args.iter().any(|a| a.contains("mcp_servers.parley.url")));
        assert!(cfg.resume_command.is_none());
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

    #[test]
    fn codex_args_carry_id_and_token() {
        let args = mcp_args_codex("claude-2", 8765, "tok123");
        assert!(args.iter().any(|a| a.contains("mcp_servers.parley.url=\"http://127.0.0.1:8765/mcp\"")));
        assert!(args.iter().any(|a| a.contains("X-Agent-Id") && a.contains("claude-2")));
        assert!(args.iter().any(|a| a.contains("X-Parley-Token") && a.contains("tok123")));
        assert!(args.iter().any(|a| a.contains("approval_mode") && a.contains("auto")));
    }

    #[test]
    fn claude_config_json_has_id_and_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".parley/mcp-reviewer.json");
        write_mcp_config_json(&path, "reviewer", 8765, "tok123").unwrap();
        let c = std::fs::read_to_string(&path).unwrap();
        assert!(c.contains("http://127.0.0.1:8765/mcp"));
        assert!(c.contains("\"X-Agent-Id\": \"reviewer\""));
        assert!(c.contains("\"X-Parley-Token\": \"tok123\""));
    }

    #[test]
    fn opencode_config_has_url_headers_and_permission() {
        let c = opencode_config_content("opencode-2", 8765, "tok123");
        assert!(c.contains("\"type\":\"remote\""));
        assert!(c.contains("http://127.0.0.1:8765/mcp"));
        assert!(c.contains("\"X-Agent-Id\":\"opencode-2\""));
        assert!(c.contains("\"X-Parley-Token\":\"tok123\""));
        // Musi być poprawnym JSON-em (trafia do OPENCODE_CONFIG_CONTENT).
        let v: serde_json::Value = serde_json::from_str(&c).expect("valid JSON");
        assert_eq!(v["mcp"]["parley"]["url"], "http://127.0.0.1:8765/mcp");
        // Auto-approve zawężone do narzędzi parley; mutujące/egress przez bramkę.
        assert_eq!(v["permission"]["parley_*"], "allow");
        assert_eq!(v["permission"]["edit"], "ask");
        assert_eq!(v["permission"]["bash"], "ask");
        assert_eq!(v["permission"]["webfetch"], "ask");
        // Bez globalnego "*":"allow" (regresja bezpieczeństwa — peer prompt injection).
        assert!(v["permission"].get("*").is_none());
    }

    #[test]
    fn args_for_dispatches_by_binary() {
        let p = Path::new("/tmp/c.json");
        assert!(mcp_args_for("codex", "codex", 1, "t", p).contains(&"-c".to_string()));
        assert!(mcp_args_for("claude", "claude", 1, "t", p).contains(&"--mcp-config".to_string()));
    }
}
