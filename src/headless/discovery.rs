//! Wykrywanie i zapis adresu brokera (`.parley/broker.json`) + lockfile.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerInfo {
    pub port: u16,
    pub pid: u32,
    pub token: String,
    pub cwd: String,
}

pub fn broker_json_path(state_dir: &Path) -> PathBuf {
    state_dir.join("broker.json")
}

pub fn lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join("broker.lock")
}

pub fn write_atomic(path: &Path, info: &BrokerInfo) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(info)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read(path: &Path) -> Option<BrokerInfo> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 32 hex znaki z /dev/urandom (unix). Fallback: PID+czas (gdy brak urandom).
pub fn random_token() -> String {
    use std::io::Read;
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut buf).is_ok() {
            return buf.iter().map(|b| format!("{b:02x}")).collect();
        }
    }
    let n = std::process::id() as u128
        ^ chrono::Local::now().timestamp_nanos_opt().unwrap_or(0) as u128;
    format!("{n:032x}").chars().take(32).collect()
}

use std::time::{Duration, Instant};

fn health_ok(info: &BrokerInfo, cwd: &Path) -> bool {
    let want = std::fs::canonicalize(cwd)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| cwd.to_string_lossy().into_owned());
    let url = format!("http://127.0.0.1:{}/health?token={}", info.port, info.token);
    match reqwest::blocking::Client::new()
        .get(&url)
        .timeout(Duration::from_millis(500))
        .send()
        .and_then(|r| r.json::<serde_json::Value>())
    {
        Ok(v) => v.get("cwd").and_then(|c| c.as_str()) == Some(want.as_str()),
        Err(_) => false,
    }
}

fn spawn_daemon(self_exe: &Path, cwd: &Path, state_dir: &Path) -> std::io::Result<()> {
    use std::process::Command;
    std::fs::create_dir_all(state_dir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("broker.log"))?;
    let log_err = log.try_clone()?;
    let mut cmd = Command::new(self_exe);
    cmd.arg("__serve")
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()?;
    Ok(())
}

pub fn ensure_broker(
    state_dir: &Path,
    cwd: &Path,
    self_exe: &Path,
) -> anyhow::Result<BrokerInfo> {
    let bj = broker_json_path(state_dir);
    if let Some(info) = read(&bj) {
        if health_ok(&info, cwd) {
            return Ok(info);
        }
    }
    // Atomically take lockfile; stale > 10 s locks are removed and retried.
    let lock = lock_path(state_dir);
    std::fs::create_dir_all(state_dir)?;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => break,
            Err(_) => {
                // Another wrapper may already be starting — poll broker.json.
                if let Some(info) = read(&bj) {
                    if health_ok(&info, cwd) {
                        return Ok(info);
                    }
                }
                let stale = std::fs::metadata(&lock)
                    .and_then(|m| m.modified())
                    .map(|t| t.elapsed().map(|e| e > Duration::from_secs(10)).unwrap_or(true))
                    .unwrap_or(true);
                if stale {
                    let _ = std::fs::remove_file(&lock);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    let _guard = LockGuard(lock.clone());
    spawn_daemon(self_exe, cwd, state_dir)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(info) = read(&bj) {
            if health_ok(&info, cwd) {
                return Ok(info);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("broker did not come up within 5s (see .parley/broker.log)")
}

struct LockGuard(PathBuf);
impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = broker_json_path(dir.path());
        let info = BrokerInfo {
            port: 8765,
            pid: 4242,
            token: "abc".into(),
            cwd: "/tmp/x".into(),
        };
        write_atomic(&path, &info).unwrap();
        assert_eq!(read(&path), Some(info));
    }

    #[test]
    fn read_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(&broker_json_path(dir.path())), None);
    }

    #[test]
    fn read_corrupt_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = broker_json_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(read(&path), None);
    }

    #[test]
    fn token_is_hex_32() {
        let t = random_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t, random_token());
    }
}
