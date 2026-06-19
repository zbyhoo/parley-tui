fn main() -> anyhow::Result<()> {
    let dir = std::env::temp_dir();
    let cmd = vec!["sh".to_string(), "-c".to_string(), "exit 7".to_string()];
    let (_handle, child) = parley_tui::headless::proxy::Proxy::spawn(&cmd, &dir)?;
    let code = child.wait();
    assert_eq!(code, 7, "proxy must propagate child exit code");
    println!("proxy_spike OK: exit code {code}");
    Ok(())
}
