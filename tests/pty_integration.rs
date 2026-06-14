use std::thread;
use std::time::{Duration, Instant};

use parley_tui::pty::AgentProcess;

fn wait_for_screen(agent: &AgentProcess, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if agent.with_screen(|s| s.contents()).contains(needle) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn fake_agent() -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        "echo READY; while IFS= read -r line; do echo \"GOT:$line\"; done".into(),
    ]
}

#[test]
fn spawn_inject_and_read_screen() {
    let cwd = std::env::temp_dir();
    let mut agent = AgentProcess::spawn(&fake_agent(), &cwd, 24, 80).unwrap();
    assert!(wait_for_screen(&agent, "READY", Duration::from_secs(2)));
    agent.write_input("hello world").unwrap();
    assert!(wait_for_screen(&agent, "GOT:hello world", Duration::from_secs(2)));
    assert!(agent.try_exit().is_none());
    agent.shutdown();
}

#[test]
fn detects_exit_code() {
    let cwd = std::env::temp_dir();
    let cmd: Vec<String> = vec!["sh".into(), "-c".into(), "exit 3".into()];
    let mut agent = AgentProcess::spawn(&cmd, &cwd, 24, 80).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut code = None;
    while Instant::now() < deadline {
        code = agent.try_exit();
        if code.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(code, Some(3));
}

#[test]
fn spawn_missing_binary_errors() {
    let cwd = std::env::temp_dir();
    let cmd: Vec<String> = vec!["definitely-not-a-real-binary-xyz".into()];
    assert!(AgentProcess::spawn(&cmd, &cwd, 24, 80).is_err());
}

#[test]
fn resize_updates_screen_size() {
    let cwd = std::env::temp_dir();
    let cmd: Vec<String> = vec!["sh".into(), "-c".into(), "sleep 5".into()];
    let mut agent = AgentProcess::spawn(&cmd, &cwd, 24, 80).unwrap();
    assert_eq!(agent.with_screen(|s| s.size()), (24, 80));
    agent.resize(30, 100).unwrap();
    assert_eq!(agent.with_screen(|s| s.size()), (30, 100));
    agent.shutdown();
}
