use crate::acp::client::{PendingElicitation, PendingPermission, RpcLogEntry};
use crate::acp::terminal_mgr::TerminalSnapshot;
use crate::acp::types::{
    AvailableCommand, Cost, PlanEntry, SessionInfo, ToolCallLocation, Usage,
};
use chrono::{DateTime, Local};
use ratatui::layout::Rect;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

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
    TurnSummary {
        duration_ms: u128,
        gen_duration_ms: u128,
        output_tokens: Option<u64>,
        total_tokens: Option<u64>,
        tps: Option<f64>,
        tps_estimated: bool,
    },
}

#[derive(Debug, Clone, Default)]
pub struct TurnGenTracker {
    pub active_burst_start: Option<Instant>,
    pub last_chunk_at: Option<Instant>,
    pub accumulated: Duration,
    pub chunk_count: usize,
    pub streamed_chars: usize,
}

impl TurnGenTracker {
    pub fn on_chunk(&mut self, chars: usize, now: Instant) {
        self.streamed_chars = self.streamed_chars.saturating_add(chars);
        self.chunk_count = self.chunk_count.saturating_add(1);
        self.last_chunk_at = Some(now);
        if self.active_burst_start.is_none() {
            self.active_burst_start = Some(now);
        }
    }

    pub fn pause(&mut self, now: Instant) {
        if let Some(start) = self.active_burst_start.take() {
            self.accumulated = self
                .accumulated
                .saturating_add(now.saturating_duration_since(start));
        }
    }

    pub fn pause_at_last_chunk(&mut self, fallback_now: Instant) {
        let end = self.last_chunk_at.unwrap_or(fallback_now);
        self.pause(end);
    }

    pub fn finish(&mut self, now: Instant) -> Duration {
        self.pause(now);
        self.accumulated
    }
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
    UndoTurn,
    OpenRewind,
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

#[derive(Debug, Clone)]
pub struct RewindEntry {
    pub timeline_idx: usize,
    pub text: String,
    pub timestamp: DateTime<Local>,
    pub files_affected: usize,
    pub turns_removed: usize,
}

#[derive(Debug, Clone)]
pub struct RewindModalState {
    pub query: String,
    pub entries: Vec<RewindEntry>,
    pub selected: usize,
}

impl RewindModalState {
    pub fn filtered_indices(&self) -> Vec<usize> {
        let keys: Vec<String> = self.entries.iter().map(|e| e.text.clone()).collect();
        crate::ui::composer::rank_candidates(&self.query, &keys)
    }
}

#[derive(Debug, Clone)]
pub enum FileRestoreContent {
    /// Exact full-file content before the edit (`None` = file did not exist before).
    Exact(Option<String>),
    /// Fallback when a tool only emitted a diff hunk after execution.
    ReverseDiff {
        old_text: Option<String>,
        new_text: String,
    },
}

#[derive(Debug, Clone)]
pub struct FileEditRecord {
    pub turn_user_idx: usize,
    pub path: String,
    pub restore: FileRestoreContent,
    /// When produced by `fs/write_text_file`, tracks sidebar `file_changes` stats.
    pub fs_stats: Option<(String, (usize, usize))>,
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
    Rewind(RewindModalState),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimelinePos {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone)]
pub enum SelectionState {
    Timeline {
        anchor: TimelinePos,
        focus: TimelinePos,
        moved: bool,
        pending_toggle: Option<(usize, Option<usize>)>,
    },
    Screen {
        region: Rect,
        anchor: (u16, u16),
        focus: (u16, u16),
        moved: bool,
    },
}

#[derive(Debug, Clone, Default)]
pub struct ScreenTextBuffer {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<Option<String>>,
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
    pub inner_rect: Rect,
    pub sidebar_rect: Option<Rect>,
    pub composer_rect: Option<Rect>,
    pub modal_rect: Option<Rect>,
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
