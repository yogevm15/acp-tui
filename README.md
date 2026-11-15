# acp-tui

A terminal UI client for the [Agent Client Protocol (ACP)](https://agentclientprotocol.com), built in Rust with `ratatui` and `tokio`.

`acp-tui` spawns and speaks JSON-RPC over stdio with ACP-compatible coding agents configured in a JetBrains/Zed-style `acp.json`, providing a rich interactive conversation view, inline diffs, live terminal output, file mentions, permission prompts, session management, and a protocol inspector.

## Features

- **Multi-agent support via `acp.json`** — Configure multiple ACP agents and switch between them at runtime (`Ctrl+G` or `/agent`), with global and per-agent MCP server passthrough (`stdio`, `http`, `sse`).
- **Rich streaming timeline** — Markdown rendering (headings, lists, task lists, tables, blockquotes, links) with syntax-highlighted fenced code blocks, collapsible thinking blocks, and expandable tool cards.
- **Inline diffs & terminal execution** — Handles `fs/read_text_file`, `fs/write_text_file`, and `terminal/*` requests (`create`, `output`, `wait_for_exit`, `kill`, `release`), rendering unified line-numbered diffs and live command output.
- **Interactive permissions & elicitations** — Review diffs and shell commands before approving (`1-9`, `y`/`a`/`n`, or `--yolo` / `/yolo` auto-approve), and respond to both `form` and `url` elicitation requests.
- **Composer with autocomplete** — Multi-line editing, readline navigation, undo (`Ctrl+Z`), persistent prompt history (`Ctrl+R`), `/` slash command completion (with argument completion for `/model`, `/mode`, `/thinking`, `/agent`, `/theme`), and fuzzy `@file` / `@file:start-end` attachments (including images when supported by the agent).
- **Sessions & turn steering** — Create, list, load, resume, continue (`-c`), and fork sessions; steer active turns when supported by the agent or queue follow-up prompts.
- **Sidebar & protocol inspector** — Live sidebar showing session details, model/mode/thinking settings, context window usage and cost, todo/plan progress, subagent activity, and modified files (`Ctrl+B`), plus a filterable JSON-RPC traffic log (`F12`).
- **Themes & export** — Six built-in color themes (`opencode`, `claude`, `catppuccin`, `kanagawa`, `nord`, `light`), Markdown conversation export (`/export`), and OSC 52 clipboard copy (`/copy`).

## Installation

Requires a Rust toolchain supporting Rust 2024 edition (Rust 1.85+).

```sh
cargo install --path .
```

Or build a release binary directly:

```sh
cargo build --release
# Binary is at target/release/acp-tui
```

## Configuration (`acp.json`)

`acp-tui` discovers agents from `acp.json` using the following lookup order:

1. `--acp-json <PATH>`
2. `$ACP_TUI_ACP_JSON`
3. `~/.config/acp-tui/acp.json` (or platform config directory)
4. `~/.jetbrains/acp.json`

Example `acp.json`:

```json
{
  "mcp_servers": [],
  "agent_servers": {
    "Cursor": {
      "command": "/usr/local/bin/cursor-acp",
      "args": [],
      "env": {},
      "default_config_options": {
        "mode": "auto-review"
      }
    },
    "Claude": {
      "command": "npx",
      "args": ["-y", "@zed-industries/claude-agent-acp"]
    }
  }
}
```

Each entry under `agent_servers` supports:

- `command` (string, `~/` expanded) and `args` (array of strings, `~/` expanded)
- `env` (object of environment variables)
- `default_config_options` (object), `default_mode`, `default_model`
- `mcp_servers` (array of `stdio`, `http`, or `sse` MCP server definitions, merged with top-level `mcp_servers`)

Verify configured agents and whether their commands are available on `PATH`:

```sh
acp-tui agents
```

## Usage

```sh
# Start with the last used (or first) agent in acp.json
acp-tui

# Choose an agent, working directory, mode, and model
acp-tui --agent Claude --cwd /path/to/repo --mode plan --model claude-sonnet-4-5

# Continue the most recent session in the current directory
acp-tui -c

# Resume a specific session ID
acp-tui --agent Cursor --session <session-id>

# Auto-approve tool permissions and send an initial prompt
acp-tui --yolo -p "Summarize this repository"
```

### CLI Options

| Flag | Env Var | Description |
| --- | --- | --- |
| `-a, --agent <AGENT>` | `ACP_TUI_AGENT` | Agent name from `acp.json` `agent_servers` (case-insensitive) |
| `--acp-json <PATH>` | `ACP_TUI_ACP_JSON` | Path to `acp.json` |
| `-C, --cwd <DIR>` | — | Working directory for the session (defaults to current directory) |
| `--add-dir <DIR>` | — | Additional workspace directory to expose (repeatable) |
| `--mode <MODE>` | — | Initial session mode (e.g. `auto-review`, `plan`, `ask`, `yolo`) |
| `-m, --model <MODEL>` | — | Initial model ID |
| `--yolo` | — | Auto-approve tool permissions and select a `yolo` mode if available |
| `-s, --session <ID>` | — | Resume a specific session ID |
| `-c, --continue` | — | Continue the most recent session in the working directory |
| `--theme <THEME>` | `ACP_TUI_THEME` | Color theme: `opencode`, `claude`, `catppuccin`, `kanagawa`, `nord`, `light` |
| `-p, --prompt <TEXT>` | — | Prompt to send once the session starts |

### Persistent Local Data

Preferences (`prefs.json`), prompt history (`history.json`), and local session records (`sessions.json`) are stored in the platform local data directory under `acp-tui/` (override with `$ACP_TUI_DATA_DIR`).

## Slash Commands & Attachments

Type `/` in the composer to browse both built-in TUI commands and agent-advertised commands:

| Command | Description |
| --- | --- |
| `/new` | Start a new session |
| `/sessions`, `/resume` | Browse and resume earlier sessions |
| `/fork` | Fork the current session |
| `/agent [agent-id]` | Switch the ACP agent |
| `/model [model]` | Select the model |
| `/mode [mode]` | Select the session mode |
| `/thinking` | Change thinking / reasoning level |
| `/config` | Open all session config options |
| `/theme [name]` | Switch color theme |
| `/yolo` | Toggle automatic tool permission approval |
| `/export` | Export conversation to `acp-session-<id>.md` |
| `/copy` | Copy last assistant reply via OSC 52 |
| `/clear` | Clear visible timeline (keeps agent context) |
| `/logs` | Open the ACP JSON-RPC protocol log |
| `/login`, `/logout` | Authenticate or sign out of the agent |
| `/keys`, `/help` | Show keyboard shortcuts |
| `/quit`, `/exit` | Exit `acp-tui` |

Type `@` to fuzzy-search workspace files (respecting `.gitignore`):

- `@path/to/file` attaches a text file (or image for `png`, `jpg`, `jpeg`, `gif`, `webp` when supported by the agent).
- `@path/to/file:10` or `@path/to/file:10-30` attaches a specific line range.

## Keyboard Shortcuts

### Composer

| Shortcut | Action |
| --- | --- |
| `Enter` | Send prompt (steer or queue while agent is busy) |
| `Shift+Enter` · `Alt+Enter` · `Ctrl+J` · `\`+`Enter` | Insert newline |
| `Up` / `Down` | Browse prompt history (or edit last queued message when input is empty) |
| `Ctrl+A` / `Ctrl+E` | Move to start / end of line |
| `Ctrl+Left` / `Ctrl+Right` · `Alt+B` / `Alt+F` | Move by word |
| `Ctrl+W` · `Alt+D` · `Ctrl+U` · `Ctrl+K` | Delete word back · word forward · to line start · to line end |
| `Ctrl+Z` | Undo |
| `Ctrl+R` | Search prompt history |

### Agent, View & App

| Shortcut | Action |
| --- | --- |
| `Esc` | Interrupt running turn (`Esc Esc` clears composer input) |
| `Shift+Tab` | Cycle session mode |
| `Ctrl+P` | Open command palette |
| `Ctrl+S` | Browse and resume sessions (`Ctrl+D` inside modal deletes) |
| `Ctrl+N` | Start a new session |
| `Ctrl+L` | Select model |
| `Ctrl+G` | Switch ACP agent |
| `Ctrl+B` | Toggle sidebar |
| `Ctrl+O` | Expand or collapse all tool outputs |
| `Ctrl+T` | Expand or collapse all thinking blocks |
| `PgUp` / `PgDn` · `Shift+Up` / `Shift+Down` · mouse wheel | Scroll conversation |
| `Ctrl+Home` / `Ctrl+End` | Jump to top / bottom |
| Click timeline step | Expand or collapse a tool call or thinking block |
| `F1` or `?` (empty input) | Show keyboard shortcuts |
| `F12` | Open ACP protocol log |
| `Ctrl+C` | Clear input / interrupt turn / press twice to quit |
| `Ctrl+D` | Quit (when input is empty) |

### Permission Modal

| Shortcut | Action |
| --- | --- |
| `1`–`9` or `Enter` | Select option |
| `y` / `a` / `n` | Allow once / allow always / reject once |
| `Esc` | Reject once |
| `PgUp` / `PgDn` | Scroll diff / command details |

## Development

Run unit and end-to-end tests (uses `python3` with `tests/fixtures/mock_agent.py`):

```sh
cargo test
```

## License

Licensed under the [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) license.
