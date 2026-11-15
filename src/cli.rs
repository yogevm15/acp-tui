use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "acp-tui",
    version,
    about = "Terminal UI for Agent Client Protocol (ACP) agents",
    long_about = "Terminal UI for Agent Client Protocol (ACP) agents.\n\nAgents come from acp.json (JetBrains format, `agent_servers`). Lookup order: --acp-json, $ACP_TUI_ACP_JSON, ~/.config/acp-tui/acp.json, ~/.jetbrains/acp.json."
)]
pub struct Cli {
    #[command(subcommand)]
    pub subcommand: Option<CliSubcommand>,

    /// Agent name from acp.json `agent_servers` (case-insensitive)
    #[arg(short = 'a', long = "agent", env = "ACP_TUI_AGENT")]
    pub agent: Option<String>,

    /// Path to the acp.json file
    #[arg(long = "acp-json")]
    pub acp_json: Option<PathBuf>,

    /// Working directory for the session (defaults to current directory)
    #[arg(short = 'C', long = "cwd")]
    pub cwd: Option<PathBuf>,

    /// Additional workspace directories to expose to the ACP session
    #[arg(long = "add-dir")]
    pub add_dirs: Vec<PathBuf>,

    /// Initial session mode (for example auto-review, plan, ask, yolo)
    #[arg(long = "mode")]
    pub mode: Option<String>,

    /// Initial model ID
    #[arg(short = 'm', long = "model")]
    pub model: Option<String>,

    /// Auto-approve tool permissions (and select a "yolo" mode when the agent has one)
    #[arg(long = "yolo")]
    pub yolo: bool,

    /// Resume a specific session ID
    #[arg(short = 's', long = "session")]
    pub session: Option<String>,

    /// Continue the most recent session in the working directory
    #[arg(short = 'c', long = "continue")]
    pub continue_last: bool,

    /// UI color theme (opencode, claude, catppuccin, kanagawa, nord, light)
    #[arg(long = "theme", env = "ACP_TUI_THEME")]
    pub theme: Option<String>,

    /// Prompt to send once the session starts
    #[arg(short = 'p', long = "prompt")]
    pub prompt: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum CliSubcommand {
    /// List the agents in acp.json
    Agents,
}
