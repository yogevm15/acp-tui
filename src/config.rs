use crate::acp::client::AgentSpawnSpec;
use crate::acp::types::{EnvVariable, HttpHeader, McpServer};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One entry of `agent_servers` in a JetBrains / Zed style `acp.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentServerEntry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub default_config_options: serde_json::Map<String, Value>,
    #[serde(default)]
    pub default_mode: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AcpJson {
    #[serde(default)]
    pub agent_servers: serde_json::Map<String, Value>,
    #[serde(default)]
    pub mcp_servers: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ResolvedAgentProfile {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub available: bool,
    pub default_options_json: Option<Value>,
    pub mcp_servers: Vec<McpServer>,
}

impl ResolvedAgentProfile {
    pub fn to_spawn_spec(&self, cwd: PathBuf, instance: u64) -> AgentSpawnSpec {
        AgentSpawnSpec {
            instance,
            label: self.id.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            cwd,
        }
    }

    pub fn command_line_display(&self) -> String {
        if self.args.is_empty() {
            self.command.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
        }
    }
}

pub fn data_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("ACP_TUI_DATA_DIR")
        && !custom.trim().is_empty()
    {
        return PathBuf::from(custom);
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("acp-tui")
}

pub fn find_binary_in_path(bin: &str) -> Option<PathBuf> {
    let p = Path::new(bin);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

/// Candidate `acp.json` locations, in lookup order.
pub fn acp_json_candidates(explicit: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = explicit {
        out.push(p.to_path_buf());
        return out;
    }
    if let Ok(env) = std::env::var("ACP_TUI_ACP_JSON")
        && !env.trim().is_empty()
    {
        out.push(expand_tilde(&env));
    }
    if let Some(cfg) = dirs::config_dir() {
        out.push(cfg.join("acp-tui").join("acp.json"));
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".jetbrains").join("acp.json"));
    }
    out
}

fn env_list(v: Option<&Value>) -> Vec<EnvVariable> {
    match v {
        Some(Value::Object(map)) => map
            .iter()
            .filter_map(|(k, v)| {
                v.as_str().map(|s| EnvVariable {
                    name: k.clone(),
                    value: s.to_string(),
                })
            })
            .collect(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| serde_json::from_value::<EnvVariable>(i.clone()).ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// Accepts ACP wire format, plus `env` / `headers` given as plain objects.
fn parse_mcp_server(v: &Value) -> Option<McpServer> {
    let name = v.get("name")?.as_str()?.to_string();
    let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("stdio");
    if kind == "http" || kind == "sse" {
        let headers = env_list(v.get("headers"))
            .into_iter()
            .map(|e| HttpHeader {
                name: e.name,
                value: e.value,
            })
            .collect();
        return Some(McpServer::Http {
            kind: kind.to_string(),
            name,
            url: v.get("url")?.as_str()?.to_string(),
            headers,
        });
    }
    Some(McpServer::Stdio {
        name,
        command: expand_tilde(v.get("command")?.as_str()?)
            .to_string_lossy()
            .into_owned(),
        args: v
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        env: env_list(v.get("env")),
    })
}

fn default_options_meta(entry: &AgentServerEntry) -> Option<Value> {
    let mut defaults = entry.default_config_options.clone();
    if let Some(m) = &entry.default_mode {
        defaults.entry("mode").or_insert(Value::String(m.clone()));
    }
    if let Some(m) = &entry.default_model {
        defaults.entry("model").or_insert(Value::String(m.clone()));
    }
    if defaults.is_empty() {
        return None;
    }
    let defaults = Value::Object(defaults);
    Some(serde_json::json!({
        "default_config_options": defaults,
        "defaultConfigOptions": defaults,
    }))
}

/// Loads the agents from the first `acp.json` that exists. Returns the path that was used.
pub fn load_agents(explicit: Option<&Path>) -> Result<(PathBuf, Vec<ResolvedAgentProfile>)> {
    let candidates = acp_json_candidates(explicit);
    let Some(path) = candidates.iter().find(|p| p.is_file()).cloned() else {
        let list = candidates
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "No acp.json found. Looked in:\n{list}\n\nCreate one, for example:\n{}",
            EXAMPLE_ACP_JSON
        );
    };
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("Cannot read {}", path.display()))?;
    let parsed: AcpJson = serde_json::from_str(&raw)
        .with_context(|| format!("Invalid JSON in {}", path.display()))?;

    let global_mcp: Vec<McpServer> = parsed.mcp_servers.iter().filter_map(parse_mcp_server).collect();
    let mut agents = Vec::new();
    for (name, value) in &parsed.agent_servers {
        let entry: AgentServerEntry = serde_json::from_value(value.clone()).with_context(|| {
            format!("Invalid agent_servers entry \"{name}\" in {}", path.display())
        })?;
        let command = expand_tilde(&entry.command).to_string_lossy().into_owned();
        let mut mcp = global_mcp.clone();
        mcp.extend(entry.mcp_servers.iter().filter_map(parse_mcp_server));
        agents.push(ResolvedAgentProfile {
            id: name.clone(),
            available: find_binary_in_path(&command).is_some(),
            default_options_json: default_options_meta(&entry),
            args: entry
                .args
                .iter()
                .map(|a| expand_tilde(a).to_string_lossy().into_owned())
                .collect(),
            env: entry.env,
            command,
            mcp_servers: mcp,
        });
    }
    if agents.is_empty() {
        bail!(
            "{} has no agent_servers entries. Example:\n{}",
            path.display(),
            EXAMPLE_ACP_JSON
        );
    }
    Ok((path, agents))
}

pub const EXAMPLE_ACP_JSON: &str = r#"{
  "agent_servers": {
    "Cursor": {
      "command": "/usr/local/bin/cursor-acp",
      "args": [],
      "env": {},
      "default_config_options": { "mode": "auto-review" }
    },
    "Claude": {
      "command": "npx",
      "args": ["-y", "@zed-industries/claude-agent-acp"]
    }
  }
}"#;

// ============================================================================
// Persistent Local State (History, Session Index, UI Preferences)
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedPrefs {
    pub theme: Option<String>,
    pub show_sidebar: Option<bool>,
    pub show_thoughts: Option<bool>,
    pub expand_tools: Option<bool>,
    pub last_agent_id: Option<String>,
    #[serde(default)]
    pub last_mode_by_agent: HashMap<String, String>,
    #[serde(default)]
    pub last_model_by_agent: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSessionRecord {
    pub agent_id: String,
    pub session_id: String,
    pub cwd: String,
    pub title: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct AppStateStore {
    base_dir: PathBuf,
}

impl AppStateStore {
    pub fn new() -> Self {
        let base_dir = data_dir();
        let _ = std::fs::create_dir_all(&base_dir);
        Self { base_dir }
    }

    pub fn load_prefs(&self) -> SavedPrefs {
        let path = self.base_dir.join("prefs.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_prefs(&self, prefs: &SavedPrefs) {
        let path = self.base_dir.join("prefs.json");
        if let Ok(json) = serde_json::to_string_pretty(prefs) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn load_history(&self) -> Vec<String> {
        let path = self.base_dir.join("history.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_history(&self, entries: &[String]) {
        let path = self.base_dir.join("history.json");
        let start = entries.len().saturating_sub(500);
        if let Ok(json) = serde_json::to_string(&entries[start..]) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn load_session_records(&self) -> Vec<LocalSessionRecord> {
        let path = self.base_dir.join("sessions.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn upsert_session_record(&self, agent_id: &str, session_id: &str, cwd: &str, title: &str) {
        let mut records = self.load_session_records();
        let now = Utc::now().to_rfc3339();
        if let Some(existing) = records
            .iter_mut()
            .find(|r| r.agent_id == agent_id && r.session_id == session_id)
        {
            if !title.trim().is_empty() {
                existing.title = title.to_string();
            }
            existing.cwd = cwd.to_string();
            existing.updated_at = now;
        } else {
            records.insert(
                0,
                LocalSessionRecord {
                    agent_id: agent_id.to_string(),
                    session_id: session_id.to_string(),
                    cwd: cwd.to_string(),
                    title: if title.trim().is_empty() {
                        "New Session".to_string()
                    } else {
                        title.to_string()
                    },
                    updated_at: now,
                },
            );
        }
        records.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        records.truncate(200);
        let path = self.base_dir.join("sessions.json");
        if let Ok(json) = serde_json::to_string_pretty(&records) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn remove_session_record(&self, agent_id: &str, session_id: &str) {
        let mut records = self.load_session_records();
        records.retain(|r| !(r.agent_id == agent_id && r.session_id == session_id));
        let path = self.base_dir.join("sessions.json");
        if let Ok(json) = serde_json::to_string_pretty(&records) {
            let _ = std::fs::write(path, json);
        }
    }
}
