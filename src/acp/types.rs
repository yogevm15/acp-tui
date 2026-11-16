use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub const PROTOCOL_VERSION: u32 = 1;

pub const ERR_METHOD_NOT_FOUND: i64 = -32601;
pub const ERR_INVALID_PARAMS: i64 = -32602;
pub const ERR_INTERNAL_ERROR: i64 = -32603;
pub const ERR_AUTH_REQUIRED: i64 = -32000;
pub const ERR_RESOURCE_NOT_FOUND: i64 = -32002;

pub fn deserialize_optional_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: Deserializer<'de>,
{
    let val = Option::<Value>::deserialize(deserializer)?;
    match val {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                Ok(Some(u as usize))
            } else if let Some(f) = n.as_f64() {
                if f.is_finite() && f >= 0.0 {
                    Ok(Some(f as usize))
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            }
        }
        Some(Value::String(s)) => Ok(s.trim().parse::<usize>().ok()),
        _ => Ok(None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
    Null,
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::Number(n) => write!(f, "{n}"),
            RequestId::String(s) => write!(f, "{s}"),
            RequestId::Null => write!(f, "null"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

impl RpcError {
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: ERR_METHOD_NOT_FOUND,
            message: format!("\"Method not found\": {method}"),
            data: Some(serde_json::json!({ "method": method })),
        }
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: ERR_INVALID_PARAMS,
            message: msg.into(),
            data: None,
        }
    }

    pub fn internal_error(msg: impl Into<String>) -> Self {
        Self {
            code: ERR_INTERNAL_ERROR,
            message: msg.into(),
            data: None,
        }
    }

    pub fn resource_not_found(uri: &str) -> Self {
        Self {
            code: ERR_RESOURCE_NOT_FOUND,
            message: format!("Resource not found: {uri}"),
            data: Some(serde_json::json!({ "uri": uri })),
        }
    }

    pub fn is_auth_required(&self) -> bool {
        self.code == ERR_AUTH_REQUIRED
            || self.message.to_lowercase().contains("auth_required")
            || self.message.to_lowercase().contains("authentication required")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Implementation {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub version: String,
}

impl Implementation {
    pub fn display_name(&self) -> &str {
        self.title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(&self.name)
    }
}

// ============================================================================
// Initialization & Capabilities
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    pub protocol_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_capabilities: Option<ClientCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_info: Option<Implementation>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fs: Option<FileSystemCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<ClientSessionCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagents: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<ElicitationCapabilities>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

impl ClientCapabilities {
    pub fn full_tui() -> Self {
        Self {
            fs: Some(FileSystemCapabilities {
                read_text_file: Some(true),
                write_text_file: Some(true),
            }),
            terminal: Some(true),
            session: Some(ClientSessionCapabilities {
                compaction: Some(serde_json::json!({})),
                config_options: Some(SessionConfigOptionsCapabilities {
                    boolean: Some(serde_json::json!({})),
                }),
                notices: Some(serde_json::json!({})),
            }),
            subagents: Some(serde_json::json!({})),
            plan: Some(serde_json::json!({})),
            auth: Some(AuthCapabilities {
                terminal: Some(true),
            }),
            elicitation: Some(ElicitationCapabilities {
                form: Some(serde_json::json!({})),
                url: Some(serde_json::json!({})),
            }),
            meta: Some(serde_json::json!({
                "terminal_output": true
            })),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSystemCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_text_file: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_text_file: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSessionCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_options: Option<SessionConfigOptionsCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notices: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigOptionsCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boolean: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    #[serde(default)]
    pub agent_info: Option<Implementation>,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

fn default_protocol_version() -> u32 {
    PROTOCOL_VERSION
}

impl InitializeResponse {
    pub fn supports_steering(&self) -> bool {
        self.meta
            .as_ref()
            .and_then(|m| m.get("steering"))
            .and_then(|s| s.get("supported"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default, alias = "load_session")]
    pub load_session: bool,
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
    #[serde(default)]
    pub mcp_capabilities: McpCapabilities,
    #[serde(default)]
    pub session_capabilities: SessionCapabilities,
    #[serde(default)]
    pub auth: Option<AgentAuthCapabilities>,
    #[serde(default)]
    pub providers: Option<Value>,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

impl AgentCapabilities {
    pub fn supports_list_sessions(&self) -> bool {
        self.session_capabilities.list.is_some()
    }

    pub fn supports_resume_session(&self) -> bool {
        self.session_capabilities.resume.is_some()
    }

    pub fn supports_fork_session(&self) -> bool {
        self.session_capabilities.fork.is_some()
    }

    pub fn supports_delete_session(&self) -> bool {
        self.session_capabilities.delete.is_some()
    }

    pub fn supports_close_session(&self) -> bool {
        self.session_capabilities.close.is_some()
    }

    pub fn supports_additional_directories(&self) -> bool {
        self.session_capabilities.additional_directories.is_some()
    }

    pub fn supports_logout(&self) -> bool {
        self.auth
            .as_ref()
            .and_then(|a| a.logout.as_ref())
            .is_some()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub embedded_context: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCapabilities {
    #[serde(default)]
    pub http: bool,
    #[serde(default)]
    pub sse: bool,
    #[serde(default)]
    pub acp: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    #[serde(default)]
    pub list: Option<Value>,
    #[serde(default)]
    pub delete: Option<Value>,
    #[serde(default)]
    pub additional_directories: Option<Value>,
    #[serde(default)]
    pub system_prompt: Option<Value>,
    #[serde(default)]
    pub fork: Option<Value>,
    #[serde(default)]
    pub resume: Option<Value>,
    #[serde(default)]
    pub close: Option<Value>,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthCapabilities {
    #[serde(default)]
    pub logout: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `"terminal"` for interactive CLI subcommand login, or omitted/`"agent"` for RPC `authenticate`.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl AuthMethod {
    pub fn is_terminal(&self) -> bool {
        self.kind.as_deref() == Some("terminal")
    }
}

// ============================================================================
// MCP Server Config
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVariable {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServer {
    Http {
        #[serde(rename = "type")]
        kind: String, // "http" or "sse"
        name: String,
        url: String,
        #[serde(default)]
        headers: Vec<HttpHeader>,
    },
    Stdio {
        name: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<EnvVariable>,
    },
}

// ============================================================================
// Session Lifecycle, Modes, Models & Config Options
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionRequest {
    pub cwd: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub additional_directories: Vec<String>,
    pub mcp_servers: Vec<McpServer>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: String,
    #[serde(default)]
    pub modes: Option<SessionModeState>,
    #[serde(default)]
    pub models: Option<LegacySessionModels>,
    #[serde(default)]
    pub config_options: Option<Vec<SessionConfigOption>>,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionRequest {
    pub session_id: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub additional_directories: Vec<String>,
    pub mcp_servers: Vec<McpServer>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionResponse {
    #[serde(default)]
    pub modes: Option<SessionModeState>,
    #[serde(default)]
    pub models: Option<LegacySessionModels>,
    #[serde(default)]
    pub config_options: Option<Vec<SessionConfigOption>>,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSessionRequest {
    pub session_id: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub additional_directories: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub mcp_servers: Vec<McpServer>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSessionResponse {
    #[serde(default)]
    pub modes: Option<SessionModeState>,
    #[serde(default)]
    pub models: Option<LegacySessionModels>,
    #[serde(default)]
    pub config_options: Option<Vec<SessionConfigOption>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkSessionRequest {
    pub session_id: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub additional_directories: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub mcp_servers: Vec<McpServer>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkSessionResponse {
    pub session_id: String,
    #[serde(default)]
    pub modes: Option<SessionModeState>,
    #[serde(default)]
    pub models: Option<LegacySessionModels>,
    #[serde(default)]
    pub config_options: Option<Vec<SessionConfigOption>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionsResponse {
    #[serde(default)]
    pub sessions: Vec<SessionInfo>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub cwd: String,
    #[serde(default)]
    pub additional_directories: Vec<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModeState {
    pub current_mode_id: String,
    #[serde(default)]
    pub available_modes: Vec<SessionMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMode {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Pre-ACP-1.4 `models` structure still emitted by many ACP agents (`claude-code-acp`, `codex-acp`, `cursor-acp`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacySessionModels {
    pub current_model_id: String,
    #[serde(default)]
    pub available_models: Vec<LegacyModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyModelInfo {
    pub model_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigSelectOption {
    pub value: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigSelectGroup {
    pub group: String,
    pub name: String,
    #[serde(default)]
    pub options: Vec<SessionConfigSelectOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RawSelectOptions {
    Flat(Vec<SessionConfigSelectOption>),
    Grouped(Vec<SessionConfigSelectGroup>),
}

impl Default for RawSelectOptions {
    fn default() -> Self {
        RawSelectOptions::Flat(Vec::new())
    }
}

impl RawSelectOptions {
    pub fn flatten(&self) -> Vec<SessionConfigSelectOption> {
        match self {
            RawSelectOptions::Flat(items) => items.clone(),
            RawSelectOptions::Grouped(groups) => {
                let mut out = Vec::new();
                for g in groups {
                    for opt in &g.options {
                        let mut cloned = opt.clone();
                        if cloned.group_name.is_none() {
                            cloned.group_name = Some(g.name.clone());
                        }
                        out.push(cloned);
                    }
                }
                out
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigOption {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(rename = "type", default = "default_select_type")]
    pub kind: String, // "select" or "boolean"
    /// String for `"select"`, bool for `"boolean"`.
    #[serde(default)]
    pub current_value: Value,
    #[serde(default)]
    pub options: RawSelectOptions,
}

fn default_select_type() -> String {
    "select".to_string()
}

impl SessionConfigOption {
    pub fn is_boolean(&self) -> bool {
        self.kind == "boolean" || self.current_value.is_boolean()
    }

    pub fn current_bool(&self) -> Option<bool> {
        match &self.current_value {
            Value::Bool(b) => Some(*b),
            Value::String(s) if s == "true" => Some(true),
            Value::String(s) if s == "false" => Some(false),
            _ => None,
        }
    }

    pub fn current_str(&self) -> String {
        match &self.current_value {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            other => other.to_string(),
        }
    }

    pub fn display_value(&self) -> String {
        if self.is_boolean() {
            if self.current_bool().unwrap_or(false) {
                "on".to_string()
            } else {
                "off".to_string()
            }
        } else {
            let cur = self.current_str();
            self.options
                .flatten()
                .into_iter()
                .find(|o| o.value == cur)
                .map(|o| o.name)
                .unwrap_or(cur)
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionConfigOptionResponse {
    #[serde(default)]
    pub config_options: Vec<SessionConfigOption>,
}

// ============================================================================
// Content Blocks & Prompts
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
    },
    Audio {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    ResourceLink {
        uri: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    Resource {
        resource: EmbeddedResourceContents,
    },
    #[serde(other)]
    Unknown,
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text { text: text.into() }
    }

    pub fn as_display_text(&self) -> String {
        match self {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image { mime_type, uri, .. } => {
                if let Some(u) = uri {
                    format!("[Image: {u} ({mime_type})]")
                } else {
                    format!("[Image ({mime_type})]")
                }
            }
            ContentBlock::Audio { mime_type, .. } => format!("[Audio ({mime_type})]"),
            ContentBlock::ResourceLink { uri, name, .. } => format!("[Resource: {name} ({uri})]"),
            ContentBlock::Resource { resource } => {
                if let Some(text) = &resource.text {
                    text.clone()
                } else {
                    format!("[Embedded Resource: {}]", resource.uri)
                }
            }
            ContentBlock::Unknown => String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResourceContents {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    #[serde(default = "default_stop_reason")]
    pub stop_reason: String,
    #[serde(default)]
    pub usage: Option<Usage>,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

fn default_stop_reason() -> String {
    "end_turn".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub thought_tokens: Option<u64>,
    #[serde(default)]
    pub cached_read_tokens: Option<u64>,
    #[serde(default)]
    pub cached_write_tokens: Option<u64>,
}

// ============================================================================
// Session Updates (Notifications from Agent)
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSessionNotification {
    pub session_id: String,
    pub update: Value,
}

#[derive(Debug, Clone)]
pub struct SessionNotification {
    pub session_id: String,
    pub update: SessionUpdate,
}

impl RawSessionNotification {
    pub fn into_typed(self) -> SessionNotification {
        let update = SessionUpdate::from_value(self.update);
        SessionNotification {
            session_id: self.session_id,
            update,
        }
    }
}

#[derive(Debug, Clone)]
pub enum SessionUpdate {
    UserMessageChunk(ContentChunk),
    AgentMessageChunk(ContentChunk),
    AgentThoughtChunk(ContentChunk),
    ToolCall(ToolCallEvent),
    ToolCallUpdate(ToolCallUpdateEvent),
    Plan(PlanEvent),
    PlanUpdate(PlanUpdateEvent),
    PlanRemoved,
    AvailableCommandsUpdate { available_commands: Vec<AvailableCommand> },
    CurrentModeUpdate { current_mode_id: String },
    ConfigOptionUpdate { config_options: Vec<SessionConfigOption> },
    SessionInfoUpdate(SessionInfoUpdateEvent),
    UsageUpdate(UsageUpdateEvent),
    Notice(NoticeEvent),
    CompactionUpdate(CompactionUpdateEvent),
    CompactionSummaryChunk { compaction_id: String, content: ContentBlock },
    SubagentUpdate(SubagentUpdateEvent),
    SessionMessage(SessionMessageEvent),
    SessionMessageChunk(SessionMessageChunkEvent),
    Unknown,
}

impl SessionUpdate {
    pub fn from_value(v: Value) -> Self {
        let kind = v
            .get("sessionUpdate")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string();

        match kind.as_str() {
            "user_message_chunk" => serde_json::from_value::<ContentChunk>(v.clone())
                .map(SessionUpdate::UserMessageChunk)
                .unwrap_or(SessionUpdate::Unknown),
            "agent_message_chunk" => serde_json::from_value::<ContentChunk>(v.clone())
                .map(SessionUpdate::AgentMessageChunk)
                .unwrap_or(SessionUpdate::Unknown),
            "agent_thought_chunk" => serde_json::from_value::<ContentChunk>(v.clone())
                .map(SessionUpdate::AgentThoughtChunk)
                .unwrap_or(SessionUpdate::Unknown),
            "tool_call" => serde_json::from_value::<ToolCallEvent>(v.clone())
                .map(SessionUpdate::ToolCall)
                .unwrap_or(SessionUpdate::Unknown),
            "tool_call_update" => serde_json::from_value::<ToolCallUpdateEvent>(v.clone())
                .map(SessionUpdate::ToolCallUpdate)
                .unwrap_or(SessionUpdate::Unknown),
            "plan" => serde_json::from_value::<PlanEvent>(v.clone())
                .map(SessionUpdate::Plan)
                .unwrap_or(SessionUpdate::Unknown),
            "plan_update" => serde_json::from_value::<PlanUpdateEvent>(v.clone())
                .map(SessionUpdate::PlanUpdate)
                .unwrap_or(SessionUpdate::Unknown),
            "plan_removed" => {
                SessionUpdate::PlanRemoved
            }
            "available_commands_update" => {
                let available_commands = v
                    .get("availableCommands")
                    .cloned()
                    .and_then(|c| serde_json::from_value::<Vec<AvailableCommand>>(c).ok())
                    .unwrap_or_default();
                SessionUpdate::AvailableCommandsUpdate { available_commands }
            }
            "current_mode_update" => {
                let current_mode_id = v
                    .get("currentModeId")
                    .and_then(|m| m.as_str())
                    .unwrap_or_default()
                    .to_string();
                SessionUpdate::CurrentModeUpdate { current_mode_id }
            }
            "config_option_update" => {
                let config_options = v
                    .get("configOptions")
                    .cloned()
                    .and_then(|c| serde_json::from_value::<Vec<SessionConfigOption>>(c).ok())
                    .unwrap_or_default();
                SessionUpdate::ConfigOptionUpdate { config_options }
            }
            "session_info_update" => serde_json::from_value::<SessionInfoUpdateEvent>(v.clone())
                .map(SessionUpdate::SessionInfoUpdate)
                .unwrap_or(SessionUpdate::Unknown),
            "usage_update" => serde_json::from_value::<UsageUpdateEvent>(v.clone())
                .map(SessionUpdate::UsageUpdate)
                .unwrap_or(SessionUpdate::Unknown),
            "notice" => serde_json::from_value::<NoticeEvent>(v.clone())
                .map(SessionUpdate::Notice)
                .unwrap_or(SessionUpdate::Unknown),
            "compaction_update" => serde_json::from_value::<CompactionUpdateEvent>(v.clone())
                .map(SessionUpdate::CompactionUpdate)
                .unwrap_or(SessionUpdate::Unknown),
            "compaction_summary_chunk" => {
                let compaction_id = v
                    .get("compactionId")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default()
                    .to_string();
                let content = v
                    .get("content")
                    .cloned()
                    .and_then(|c| serde_json::from_value::<ContentBlock>(c).ok())
                    .unwrap_or(ContentBlock::Unknown);
                SessionUpdate::CompactionSummaryChunk {
                    compaction_id,
                    content,
                }
            }
            "subagent_update" => serde_json::from_value::<SubagentUpdateEvent>(v.clone())
                .map(SessionUpdate::SubagentUpdate)
                .unwrap_or(SessionUpdate::Unknown),
            "session_message" => serde_json::from_value::<SessionMessageEvent>(v.clone())
                .map(SessionUpdate::SessionMessage)
                .unwrap_or(SessionUpdate::Unknown),
            "session_message_chunk" => serde_json::from_value::<SessionMessageChunkEvent>(v.clone())
                .map(SessionUpdate::SessionMessageChunk)
                .unwrap_or(SessionUpdate::Unknown),
            _ => SessionUpdate::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentChunk {
    pub content: ContentBlock,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallLocation {
    pub path: String,
    #[serde(default, deserialize_with = "deserialize_optional_usize")]
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCallContent {
    Content {
        content: ContentBlock,
    },
    Diff {
        path: String,
        #[serde(rename = "oldText", default)]
        old_text: Option<String>,
        #[serde(rename = "newText", default)]
        new_text: String,
    },
    Terminal {
        #[serde(rename = "terminalId")]
        terminal_id: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEvent {
    pub tool_call_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub content: Vec<ToolCallContent>,
    #[serde(default)]
    pub locations: Vec<ToolCallLocation>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallUpdateEvent {
    pub tool_call_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub content: Option<Vec<ToolCallContent>>,
    #[serde(default)]
    pub locations: Option<Vec<ToolCallLocation>>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub content: String,
    #[serde(default = "default_medium_priority")]
    pub priority: String,
    #[serde(default = "default_pending_status")]
    pub status: String,
}

fn default_medium_priority() -> String {
    "medium".to_string()
}

fn default_pending_status() -> String {
    "pending".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEvent {
    #[serde(default)]
    pub entries: Vec<PlanEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanUpdateEvent {
    pub plan: PlanUpdateContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanUpdateContent {
    Items {
        #[serde(rename = "planId")]
        plan_id: String,
        #[serde(default)]
        entries: Vec<PlanEntry>,
    },
    File {
        #[serde(rename = "planId")]
        plan_id: String,
        uri: String,
    },
    Markdown {
        #[serde(rename = "planId")]
        plan_id: String,
        content: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input: Option<AvailableCommandInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommandInput {
    #[serde(default)]
    pub hint: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoUpdateEvent {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    #[serde(default)]
    pub amount: f64,
    #[serde(default = "default_usd")]
    pub currency: String,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

fn default_usd() -> String {
    "USD".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageUpdateEvent {
    #[serde(default)]
    pub used: u64,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub cost: Option<Cost>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoticeEvent {
    #[serde(default = "default_info_severity")]
    pub severity: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
}

fn default_info_severity() -> String {
    "info".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionUpdateEvent {
    pub compaction_id: String,
    pub status: String,
    #[serde(default)]
    pub summary: Option<Vec<ContentBlock>>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentUpdateEvent {
    pub session_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub state: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageEvent {
    pub message_id: String,
    #[serde(default)]
    pub sender_session_id: Option<String>,
    #[serde(default)]
    pub recipient_session_id: Option<String>,
    #[serde(default)]
    pub content: Option<Vec<ContentBlock>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageChunkEvent {
    pub message_id: String,
    #[serde(default)]
    pub sender_session_id: Option<String>,
    #[serde(default)]
    pub recipient_session_id: Option<String>,
    pub content: ContentBlock,
}

// ============================================================================
// Client-side Requests (Sent by Agent to TUI Client)
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    /// `"allow_once"`, `"allow_always"`, `"reject_once"`, `"reject_always"`
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionRequest {
    pub session_id: String,
    pub tool_call: ToolCallUpdateEvent,
    pub options: Vec<PermissionOption>,
    #[serde(rename = "_meta", default)]
    pub meta: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RequestPermissionOutcome {
    Cancelled,
    Selected {
        #[serde(rename = "optionId")]
        option_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResponse {
    pub outcome: RequestPermissionOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTextFileRequest {
    pub session_id: String,
    pub path: String,
    #[serde(default, deserialize_with = "deserialize_optional_usize")]
    pub line: Option<usize>,
    #[serde(default, deserialize_with = "deserialize_optional_usize")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTextFileResponse {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTextFileRequest {
    pub session_id: String,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTextFileResponse {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalRequest {
    pub session_id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<EnvVariable>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_usize")]
    pub output_byte_limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalResponse {
    pub terminal_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalIdRequest {
    pub session_id: String,
    pub terminal_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalExitStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutputResponse {
    pub output: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<TerminalExitStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateElicitationRequest {
    #[serde(default)]
    pub mode: String, // "form" or "url"
    pub message: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub elicitation_id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, alias = "requestedSchema")]
    pub schema: Option<ElicitationSchema>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationSchema {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub properties: HashMap<String, Value>,
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateElicitationResponse {
    /// `"accept"`, `"decline"`, or `"cancel"`
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<HashMap<String, Value>>,
}
