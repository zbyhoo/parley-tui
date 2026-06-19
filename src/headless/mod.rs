pub mod cli;
pub mod discovery;
pub mod hub;
pub mod peer;
pub mod proxy;
pub mod wrapper;

use anyhow::Result;
use cli::Invocation;

pub enum Dispatch {
    /// Uruchom istniejące TUI (obsługiwane w main.rs).
    RunTui,
    /// Headless załatwił sprawę.
    Done,
}

pub fn dispatch(args: Vec<String>) -> Result<Dispatch> {
    match cli::parse(&args).map_err(anyhow::Error::msg)? {
        Invocation::Tui => Ok(Dispatch::RunTui),
        Invocation::Serve => {
            let cwd = std::env::current_dir()?;
            hub::serve(cwd)?;
            Ok(Dispatch::Done)
        }
        Invocation::Stop => {
            let cwd = std::env::current_dir()?;
            let state_dir = cwd.join(".parley");
            discovery::stop_broker(&state_dir)?;
            Ok(Dispatch::Done)
        }
        Invocation::Mcp => anyhow::bail!("`parley mcp` not yet implemented"),
        Invocation::Wrapper { as_id, command } => {
            wrapper::run(as_id, command)?;
            Ok(Dispatch::Done)
        }
    }
}
