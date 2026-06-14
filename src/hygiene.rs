use std::path::Path;

/// Timeline zawiera treść wiadomości użytkownika — pilnujemy, żeby `.parley/`
/// nie trafiło do repo. W repo git: dopisuje `.parley/` do .gitignore korzenia
/// (tworząc plik, jeśli trzeba), wspinając się od `cwd` do korzenia repo.
/// Poza repo: nic nie robi.
pub fn ensure_gitignore(cwd: &Path) -> std::io::Result<()> {
    let root = match cwd.ancestors().find(|d| d.join(".git").exists()) {
        Some(r) => r,
        None => return Ok(()),
    };
    let path = root.join(".gitignore");
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current.lines().any(|l| l.trim() == ".parley/" || l.trim() == ".parley") {
        return Ok(());
    }
    let sep = if current.is_empty() || current.ends_with('\n') { "" } else { "\n" };
    std::fs::write(&path, format!("{current}{sep}.parley/\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_entry_to_existing_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        ensure_gitignore(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains("target/"));
        assert!(content.contains(".parley/"));
    }

    #[test]
    fn creates_gitignore_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        ensure_gitignore(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains(".parley/"));
    }

    #[test]
    fn idempotent_when_already_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), ".parley/\n").unwrap();
        ensure_gitignore(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(content.matches(".parley/").count(), 1);
    }

    #[test]
    fn noop_outside_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        ensure_gitignore(dir.path()).unwrap();
        assert!(!dir.path().join(".gitignore").exists());
    }

    #[test]
    fn walks_up_to_repo_root_from_subdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let sub = dir.path().join("crates/foo");
        std::fs::create_dir_all(&sub).unwrap();
        ensure_gitignore(&sub).unwrap();
        // wpis ląduje w .gitignore KORZENIA repo i ignoruje .parley/ wszędzie
        let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains(".parley/"));
        assert!(!sub.join(".gitignore").exists());
    }
}
