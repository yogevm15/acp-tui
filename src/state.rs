use crate::acp::client::{PendingElicitation, PendingPermission, RpcLogEntry};
use crate::acp::terminal_mgr::TerminalSnapshot;
use crate::acp::types::{
    AvailableCommand, Cost, PlanEntry, SessionInfo, ToolCallLocation, Usage,
};
use chrono::{DateTime, Local};
use ratatui::layout::Rect;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    AuthRequired,
    Disconnected,
}

#[derive(Debug, Clone)]
pub struct ToolDiffItem {
    pub path: String,
    pub old_text: Option<String>,
    pub new_text: String,
}

#[derive(Debug, Clone)]
pub struct ToolCallCard {
    pub tool_call_id: String,
    pub title: String,
    pub name: Option<String>,
    pub kind: String,
    pub status: String,
    pub locations: Vec<ToolCallLocation>,
    pub diffs: Vec<ToolDiffItem>,
    pub text_outputs: Vec<String>,
    pub terminal_ids: Vec<String>,
    pub raw_input: Option<Value>,
    pub raw_output: Option<Value>,
    pub started_at: Instant,
    pub duration_ms: Option<u128>,
}

impl ToolCallCard {
    pub fn is_running(&self) -> bool {
        matches!(self.status.as_str(), "pending" | "in_progress")
    }

    pub fn is_failed(&self) -> bool {
        self.status == "failed"
    }

    pub fn kind_badge(&self) -> &'static str {
        match self.kind.as_str() {
            "read" => "READ",
            "edit" => "EDIT",
            "delete" => "DEL",
            "move" => "MOVE",
            "search" => "SEARCH",
            "execute" => "BASH",
            "think" => "PLAN",
            "fetch" => "FETCH",
            "switch_mode" => "MODE",
            _ => "TOOL",
        }
    }
}

#[derive(Debug, Clone)]
pub enum TimelineItem {
    UserMessage {
        id: String,
        text: String,
        attachments: Vec<String>,
        timestamp: DateTime<Local>,
        steered: bool,
    },
    AssistantThought {
        id: Option<String>,
        text: String,
    },
    AssistantMessage {
        id: Option<String>,
        text: String,
        timestamp: DateTime<Local>,
    },
    ToolCall(ToolCallCard),
    Notice {
        severity: String,
        title: String,
        description: Option<String>,
    },
    Compaction {
        compaction_id: String,
        status: String,
        summary: String,
        error: Option<String>,
    },
    SubagentMessage {
        message_id: String,
        subagent_title: String,
        text: String,
        to_subagent: bool,
    },
    SystemInfo {
        text: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone)]
pub struct SubagentSummary {
    pub session_id: String,
    pub title: String,
    pub description: Option<String>,
    pub state_label: String,
    pub is_running: bool,
    pub activity: Option<String>,
    pub tool_count: usize,
}

#[derive(Debug, Clone)]
pub struct ModifiedFileSummary {
    pub path: String,
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone)]
pub struct ExternalTerminalCommand {
    pub label: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: std::path::PathBuf,
}

#[derive(Debug, Clone)]
pub struct ToastMessage {
    pub text: String,
    pub is_error: bool,
    pub created_at: Instant,
}

// ============================================================================
// Modals & Interactive Overlays
// ============================================================================

#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub title: String,
    pub subtitle: String,
    pub shortcut: Option<String>,
    pub category: String,
    pub action: PaletteAction,
}

#[derive(Debug, Clone)]
pub enum PaletteAction {
    NewSession,
    OpenSessions,
    ForkSession,
    OpenAgents,
    OpenModels,
    OpenModes,
    OpenThinking,
    OpenConfig,
    OpenThemes,
    OpenLogs,
    OpenHelp,
    ToggleSidebar,
    ToggleThoughts,
    ToggleExpandTools,
    ToggleAutoApprove,
    CopyLastReply,
    ExportMarkdown,
    ClearTimeline,
    Login,
    Logout,
    Reconnect,
    Quit,
    SlashPrompt(String),
}

#[derive(Debug, Clone)]
pub struct PickerEntry {
    pub id: String,
    pub label: String,
    pub description: String,
    pub badge: Option<String>,
    pub current: bool,
}

#[derive(Debug, Clone)]
pub enum PickerKind {
    Agent,
    Model,
    Mode,
    ConfigSelect { config_id: String },
    Theme,
    AuthMethod,
    History,
}

#[derive(Debug, Clone)]
pub struct PickerModalState {
    pub title: String,
    pub kind: PickerKind,
    pub query: String,
    pub items: Vec<PickerEntry>,
    pub selected: usize,
}

impl PickerModalState {
    pub fn filtered_indices(&self) -> Vec<usize> {
        let keys: Vec<String> = self
            .items
            .iter()
            .map(|i| format!("{} {}", i.label, i.id))
            .collect();
        crate::ui::composer::rank_candidates(&self.query, &keys)
    }
}

#[derive(Debug, Clone)]
pub struct SessionPickerState {
    pub query: String,
    pub sessions: Vec<SessionInfo>,
    pub selected: usize,
    pub loading: bool,
}

impl SessionPickerState {
    pub fn filtered_indices(&self) -> Vec<usize> {
        let keys: Vec<String> = self
            .sessions
            .iter()
            .map(|s| format!("{} {} {}", s.title.as_deref().unwrap_or(""), s.cwd, s.session_id))
            .collect();
        crate::ui::composer::rank_candidates(&self.query, &keys)
    }
}

#[derive(Debug, Clone)]
pub struct ConfigModalState {
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct LogsModalState {
    pub scroll: usize,
    pub filter: String,
}

#[derive(Debug, Clone)]
pub struct HelpModalState {
    pub scroll: usize,
}

#[derive(Debug)]
pub struct PermissionModalState {
    pub pending: PendingPermission,
    pub selected: usize,
    pub diff_scroll: usize,
}

#[derive(Debug, Clone)]
pub enum ElicitationFieldKind {
    Text,
    Number { integer: bool },
    Boolean,
    SingleSelect { options: Vec<(String, String)> }, // (value, label)
    MultiSelect {
        options: Vec<(String, String)>,
        checked: Vec<bool>,
        cursor: usize,
    },
}

#[derive(Debug, Clone)]
pub struct ElicitationField {
    pub key: String,
    pub label: String,
    pub description: Option<String>,
    pub required: bool,
    pub kind: ElicitationFieldKind,
    pub text_val: String,
    pub bool_val: bool,
    pub select_idx: usize,
}

#[derive(Debug)]
pub struct ElicitationModalState {
    pub pending: PendingElicitation,
    pub is_url_mode: bool,
    pub fields: Vec<ElicitationField>,
    pub focused_field: usize,
}

#[derive(Debug)]
pub enum ActiveModal {
    CommandPalette {
        query: String,
        items: Vec<PaletteItem>,
        selected: usize,
    },
    Picker(PickerModalState),
    Sessions(SessionPickerState),
    Config(ConfigModalState),
    Permission(PermissionModalState),
    Elicitation(ElicitationModalState),
    Logs(LogsModalState),
    Help(HelpModalState),
}

// ============================================================================
// Autocomplete Popup State (Inline above Composer for `/` and `@`)
// ============================================================================

#[derive(Debug, Clone)]
pub enum CompletionPopupKind {
    SlashCommand,
    CommandArg,
    FileMention,
}

#[derive(Debug, Clone)]
pub struct CompletionCandidate {
    pub value: String,
    pub label: String,
    pub detail: String,
    pub has_args: bool,
    pub tag: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompletionPopup {
    pub kind: CompletionPopupKind,
    pub candidates: Vec<CompletionCandidate>,
    pub selected: usize,
}

// ============================================================================
// Session Runtime Snapshot
// ============================================================================

#[derive(Debug, Clone, Default)]
pub struct SessionUsageState {
    pub context_used: u64,
    pub context_size: u64,
    pub last_turn_usage: Option<Usage>,
    pub cumulative_tokens: u64,
    pub cost: Option<Cost>,
}

#[derive(Debug, Clone)]
pub struct SlashCommandDef {
    pub name: String,
    pub description: String,
    pub hint: Option<String>,
    pub is_builtin_ui: bool,
}

impl SlashCommandDef {
    pub fn from_acp(cmd: &AvailableCommand) -> Self {
        Self {
            name: cmd.name.trim_start_matches('/').to_string(),
            description: cmd.description.clone(),
            hint: cmd.input.as_ref().map(|i| i.hint.clone()).filter(|h| !h.is_empty()),
            is_builtin_ui: false,
        }
    }
}

pub fn merge_rpc_log(logs: &mut Vec<RpcLogEntry>, entry: RpcLogEntry) {
    if logs.len() >= 1000 {
        logs.drain(..200);
    }
    logs.push(entry);
}

pub fn update_terminal_map(
    terminals: &mut HashMap<String, TerminalSnapshot>,
    snap: TerminalSnapshot,
) {
    terminals.insert(snap.terminal_id.clone(), snap);
}

pub fn update_plan_entries(target: &mut Vec<PlanEntry>, entries: Vec<PlanEntry>) {
    *target = entries;
}

#[derive(Debug, Clone, Copy)]
pub struct TimelineHitSpan {
    pub start_line: usize,
    pub end_line: usize,
    pub item_idx: usize,
    pub expandable: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TimelineHitMap {
    pub view_rect: Rect,
    pub activity_rect: Rect,
    pub jump_pill_rect: Option<Rect>,
    pub completion_rect: Option<Rect>,
    pub toast_rects: Vec<Rect>,
    pub inner_y: u16,
    pub visible_start: usize,
    pub spans: Vec<TimelineHitSpan>,
}

#[derive(Debug, Clone, Copy)]
pub struct TimelineScrollAnchor {
    pub item_idx: usize,
    pub expanding: bool,
    pub old_start: usize,
    pub old_item_start: usize,
}
