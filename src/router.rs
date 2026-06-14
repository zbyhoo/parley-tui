#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentId {
    Claude,
    Codex,
}

impl AgentId {
    pub fn idx(self) -> usize {
        match self {
            AgentId::Claude => 0,
            AgentId::Codex => 1,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentId::Claude => "claude",
            AgentId::Codex => "codex",
        }
    }

    /// Odwrotność `label()` — mapuje wartość nagłówka X-Agent-Id na agenta.
    pub fn from_label(s: &str) -> Option<AgentId> {
        match s {
            "claude" => Some(AgentId::Claude),
            "codex" => Some(AgentId::Codex),
            _ => None,
        }
    }

    pub fn other(self) -> AgentId {
        match self {
            AgentId::Claude => AgentId::Codex,
            AgentId::Codex => AgentId::Claude,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    One(AgentId),
    Both,
}

impl Target {
    pub fn ids(self) -> &'static [AgentId] {
        match self {
            Target::One(AgentId::Claude) => &[AgentId::Claude],
            Target::One(AgentId::Codex) => &[AgentId::Codex],
            Target::Both => &[AgentId::Claude, AgentId::Codex],
        }
    }
}

/// Zdejmuje prefiks adresata tylko gdy po nim koniec stringa lub biały znak —
/// literówka typu "@claudes" nie może być cicho sklasyfikowana jako "@claude".
fn strip_target_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(prefix)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// Rozpoznany adresat (jawny lub domyślny) + treść.
    Message(Target, String),
    /// Pierwszy token zaczyna się od '@', ale nie jest znanym adresatem.
    UnknownTarget(String),
}

/// Parsuje adresata; bez jawnego prefiksu wiadomość idzie do `default`
/// (w praktyce: agent w fokusie).
/// Jeśli input zaczyna się od '@' ale nie pasuje do żadnego adresata — zwraca UnknownTarget.
pub fn parse(input: &str, default: Target) -> Parsed {
    let trimmed = input.trim();
    let (target, rest) = if let Some(r) = strip_target_prefix(trimmed, "@claude") {
        (Target::One(AgentId::Claude), r)
    } else if let Some(r) = strip_target_prefix(trimmed, "@codex") {
        (Target::One(AgentId::Codex), r)
    } else if let Some(r) = strip_target_prefix(trimmed, "@all") {
        (Target::Both, r)
    } else if trimmed.starts_with('@') {
        let tok = trimmed.split_whitespace().next().unwrap_or(trimmed);
        return Parsed::UnknownTarget(tok.to_string());
    } else {
        (default, trimmed)
    };
    Parsed::Message(target, rest.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_label_maps_known_agents() {
        assert_eq!(AgentId::from_label("claude"), Some(AgentId::Claude));
        assert_eq!(AgentId::from_label("codex"), Some(AgentId::Codex));
        assert_eq!(AgentId::from_label("nieznany"), None);
    }

    #[test]
    fn explicit_targets() {
        assert_eq!(
            parse("@claude zrób X", Target::One(AgentId::Codex)),
            Parsed::Message(Target::One(AgentId::Claude), "zrób X".to_string())
        );
        assert_eq!(
            parse("@codex zrób Y", Target::One(AgentId::Claude)),
            Parsed::Message(Target::One(AgentId::Codex), "zrób Y".to_string())
        );
        assert_eq!(
            parse("@all pytanie", Target::One(AgentId::Claude)),
            Parsed::Message(Target::Both, "pytanie".to_string())
        );
    }

    #[test]
    fn no_prefix_goes_to_default() {
        assert_eq!(
            parse("bez adresata", Target::One(AgentId::Codex)),
            Parsed::Message(Target::One(AgentId::Codex), "bez adresata".to_string())
        );
        assert_eq!(
            parse("pytanie", Target::Both),
            Parsed::Message(Target::Both, "pytanie".to_string())
        );
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(
            parse("  @claude   ze spacjami  ", Target::One(AgentId::Codex)),
            Parsed::Message(Target::One(AgentId::Claude), "ze spacjami".to_string())
        );
    }

    #[test]
    fn unknown_target_is_rejected() {
        assert_eq!(
            parse("@obaj zrób X", Target::One(AgentId::Claude)),
            Parsed::UnknownTarget("@obaj".to_string())
        );
        assert_eq!(
            parse("@claudes foo", Target::One(AgentId::Claude)),
            Parsed::UnknownTarget("@claudes".to_string())
        );
    }

    #[test]
    fn bare_prefix_yields_empty_text() {
        // samo "@claude" zwraca pustą treść (App decyduje, że pustych nie doręcza)
        assert_eq!(
            parse("@claude", Target::One(AgentId::Codex)),
            Parsed::Message(Target::One(AgentId::Claude), String::new())
        );
    }
}
