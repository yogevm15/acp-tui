use crate::acp::client::{AcpClientHandle, AcpEvent, PendingElicitation, PendingPermission, RpcLogEntry};
use crate::acp::terminal_mgr::TerminalSnapshot;
use crate::acp::types::*;
use crate::config::{AppStateStore, ResolvedAgentProfile, SavedPrefs};
use crate::state::*;
use crate::ui::composer::{Composer, build_prompt_blocks, fuzzy_score, rank_candidates, scan_workspace_files};
use crate::ui::theme::Theme;
use base64::Engine;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::text::Line;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// TUI-side slash commands: (name, description, argument hint).
pub const LOCAL_COMMANDS: &[(&str, &str, Option<&str>)] = &[
    ("new", "Start a new session", None),
    ("sessions", "Browse and resume earlier sessions", None),
    ("resume", "Browse and resume earlier sessions", None),
    ("fork", "Fork the current session", None),
    ("agent", "Switch the ACP agent", Some("[agent-id]")),
    ("model", "Choose the model", Some("[model]")),
    ("mode", "Choose the session mode", Some("[mode]")),
    ("thinking", "Choose the thinking / reasoning level", None),
    ("config", "Show all session config options", None),
    ("theme", "Change the color theme", Some("[name]")),
    ("export", "Export the conversation to Markdown", None),
    ("copy", "Copy the last reply to the clipboard", None),
    ("clear", "Clear the visible timeline", None),
    ("logs", "Show the ACP protocol log", None),
    ("yolo", "Toggle auto-approval of tool permissions", None),
    ("keys", "Show keyboard shortcuts", None),
    ("help", "Show keyboard shortcuts", None),
    ("login", "Authenticate with the agent", None),
    ("logout", "Sign out of the agent", None),
    ("quit", "Exit ACP TUI", None),
    ("exit", "Exit ACP TUI", None),
];

#[derive(Debug, Clone)]
pub enum SessionAction {
    New,
    Load { session_id: String, cwd: String },
    Resume { session_id: String, cwd: String },
    Fork { session_id: String },
    ContinueLast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenedKind {
    New,
    Loaded,
    Resumed,
    Forked,
}

pub struct SessionOpened {
    pub kind: OpenedKind,
    pub session_id: String,
    pub cwd: String,
    pub modes: Option<SessionModeState>,
    pub models: Option<LegacySessionModels>,
    pub config_options: Vec<SessionConfigOption>,
}

pub enum AppMsg {
    Connected {
        instance: u64,
        client: AcpClientHandle,
        init: InitializeResponse,
    },
    ConnectFailed {
        instance: u64,
        error: String,
    },
    SessionOpened(SessionOpened),
    SessionOpenFailed {
        action: SessionAction,
        error: RpcError,
    },
    PromptDone {
        session_id: String,
        result: Result<PromptResponse, RpcError>,
    },
    SteerResult {
        text: String,
        attachments: Vec<String>,
        result: Result<Value, RpcError>,
    },
    SessionsListed(Vec<SessionInfo>),
    SessionDeleted(String),
    ConfigOptions(Vec<SessionConfigOption>),
    AuthDone(Result<(), RpcError>),
    FileIndex(Vec<String>),
    Info(String),
    Error(String),
}

pub type RenderCacheEntry = (u64, u16, u64, bool, Vec<Line<'static>>, bool);

pub struct App {
    pub theme: Theme,
    pub acp_json_path: PathBuf,
    pub store: AppStateStore,
    pub prefs: SavedPrefs,
    pub profiles: Vec<ResolvedAgentProfile>,
    pub profile: ResolvedAgentProfile,
    pub cwd: PathBuf,
    pub add_dirs: Vec<String>,
    pub cli_mode: Option<String>,
    pub cli_model: Option<String>,
    pub initial_prompt: Option<String>,

    pub client: Option<AcpClientHandle>,
    pub init: Option<InitializeResponse>,
    pub status: ConnectionStatus,
    pub connect_generation: u64,

    pub session_id: Option<String>,
    pub session_title: Option<String>,
    pub opening: bool,
    pub replaying: bool,
    pub pending_updates: Vec<SessionNotification>,
    pub pending_action: Option<SessionAction>,
    pub modes: Option<SessionModeState>,
    pub legacy_models: Option<LegacySessionModels>,
    pub config_options: Vec<SessionConfigOption>,
    pub agent_commands: Vec<SlashCommandDef>,
    pub session_has_prompt: bool,

    pub timeline: Vec<TimelineItem>,
    pub item_revs: Vec<u64>,
    pub item_expanded: Vec<Option<bool>>,
    pub render_cache: Vec<Option<RenderCacheEntry>>,
    pub render_epoch: u64,
    pub timeline_hit: TimelineHitMap,
    pub pending_timeline_anchor: Option<TimelineScrollAnchor>,
    pub tool_index: HashMap<String, usize>,
    pub terminal_owner: HashMap<String, usize>,
    pub terminals: HashMap<String, TerminalSnapshot>,
    pub plan: Vec<PlanEntry>,
    pub plan_markdown: Option<String>,
    pub subagents: Vec<SubagentSummary>,
    /// path -> (change source id -> (added, removed))
    pub file_changes: BTreeMap<String, HashMap<String, (usize, usize)>>,
    fs_write_counter: u64,
    pub usage: SessionUsageState,

    pub busy: bool,
    pub turn_started: Option<Instant>,
    pub last_turn_duration: Option<Duration>,
    pub cancel_requested: bool,
    pub auth_retry_prompt: Option<String>,
    pub queued: VecDeque<String>,
    pub steering_unsupported: bool,

    pub composer: Composer,
    pub completion: Option<CompletionPopup>,
    pub file_index: Option<Vec<String>>,
    pub file_index_loading: bool,

    pub modal: Option<ActiveModal>,
    pub permission_queue: VecDeque<PendingPermission>,
    pub elicitation_queue: VecDeque<PendingElicitation>,

    pub rpc_logs: Vec<RpcLogEntry>,
    pub stderr_tail: VecDeque<String>,
    pub scroll_from_bottom: usize,
    pub last_total_lines: usize,
    pub last_timeline_height: usize,
    pub toasts: Vec<ToastMessage>,
    pub show_sidebar: bool,
    pub show_thoughts: bool,
    pub expand_tools: bool,
    pub auto_approve: bool,
    pub should_quit: bool,
    pub quit_armed_at: Option<Instant>,
    pub esc_armed_at: Option<Instant>,
    pub pending_external: Option<ExternalTerminalCommand>,
    pub pending_osc52: Option<String>,
    pub spinner_frame: usize,
    pub dirty: bool,

    pub acp_tx: UnboundedSender<AcpEvent>,
    pub msg_tx: UnboundedSender<AppMsg>,
}

pub struct AppInit {
    pub acp_json_path: PathBuf,
    pub store: AppStateStore,
    pub prefs: SavedPrefs,
    pub theme: Theme,
    pub profiles: Vec<ResolvedAgentProfile>,
    pub profile: ResolvedAgentProfile,
    pub cwd: PathBuf,
    pub add_dirs: Vec<String>,
    pub cli_mode: Option<String>,
    pub cli_model: Option<String>,
    pub initial_prompt: Option<String>,
    pub auto_approve: bool,
    pub acp_tx: UnboundedSender<AcpEvent>,
    pub msg_tx: UnboundedSender<AppMsg>,
}

fn is_ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

fn count_diff(old: Option<&str>, new: &str) -> (usize, usize) {
    let diff = similar::TextDiff::from_lines(old.unwrap_or(""), new);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (added, removed)
}

#[allow(clippy::too_many_arguments)]
async fn open_session_task(
    client: AcpClientHandle,
    action: SessionAction,
    cwd: String,
    dirs: Vec<String>,
    mcp: Vec<McpServer>,
    meta: Option<Value>,
    caps: AgentCapabilities,
    local_latest: Option<String>,
) -> Result<SessionOpened, RpcError> {
    let dirs = if caps.supports_additional_directories() {
        dirs
    } else {
        Vec::new()
    };
    let mut action = match action {
        SessionAction::Load { session_id, cwd }
            if !caps.load_session && caps.supports_resume_session() =>
        {
            SessionAction::Resume { session_id, cwd }
        }
        other => other,
    };
    if matches!(action, SessionAction::ContinueLast) {
        let mut found: Option<(String, String)> = None;
        if caps.supports_list_sessions()
            && let Ok(list) = client.list_sessions(Some(&cwd), None).await
        {
            let mut sessions = list.sessions;
            sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            found = sessions.into_iter().next().map(|s| (s.session_id, s.cwd));
        }
        if found.is_none() {
            found = local_latest.map(|id| (id, cwd.clone()));
        }
        action = match found {
            Some((session_id, cwd)) if caps.load_session => SessionAction::Load { session_id, cwd },
            Some((session_id, cwd)) if caps.supports_resume_session() => {
                SessionAction::Resume { session_id, cwd }
            }
            _ => SessionAction::New,
        };
    }

    match action {
        SessionAction::New | SessionAction::ContinueLast => {
            let r = client.new_session(&cwd, dirs, mcp, meta).await?;
            Ok(SessionOpened {
                kind: OpenedKind::New,
                session_id: r.session_id,
                cwd,
                modes: r.modes,
                models: r.models,
                config_options: r.config_options.unwrap_or_default(),
            })
        }
        SessionAction::Load { session_id, cwd } => {
            let r = client.load_session(&session_id, &cwd, dirs, mcp).await?;
            Ok(SessionOpened {
                kind: OpenedKind::Loaded,
                session_id,
                cwd,
                modes: r.modes,
                models: r.models,
                config_options: r.config_options.unwrap_or_default(),
            })
        }
        SessionAction::Resume { session_id, cwd } => {
            let r = client.resume_session(&session_id, &cwd, dirs, mcp).await?;
            Ok(SessionOpened {
                kind: OpenedKind::Resumed,
                session_id,
                cwd,
                modes: r.modes,
                models: r.models,
                config_options: r.config_options.unwrap_or_default(),
            })
        }
        SessionAction::Fork { session_id } => {
            let r = client.fork_session(&session_id, &cwd, dirs, mcp).await?;
            Ok(SessionOpened {
                kind: OpenedKind::Forked,
                session_id: r.session_id,
                cwd,
                modes: r.modes,
                models: r.models,
                config_options: r.config_options.unwrap_or_default(),
            })
        }
    }
}

impl App {
    pub fn new(init: AppInit) -> Self {
        let history = init.store.load_history();
        let show_sidebar = init.prefs.show_sidebar.unwrap_or(true);
        let show_thoughts = init.prefs.show_thoughts.unwrap_or(false);
        let expand_tools = init.prefs.expand_tools.unwrap_or(false);
        let auto_approve = init.auto_approve;
        Self {
            theme: init.theme,
            acp_json_path: init.acp_json_path,
            store: init.store,
            prefs: init.prefs,
            profiles: init.profiles,
            profile: init.profile,
            cwd: init.cwd,
            add_dirs: init.add_dirs,
            cli_mode: init.cli_mode,
            cli_model: init.cli_model,
            initial_prompt: init.initial_prompt,
            client: None,
            init: None,
            status: ConnectionStatus::Connecting,
            connect_generation: 0,
            session_id: None,
            session_title: None,
            opening: false,
            replaying: false,
            pending_updates: Vec::new(),
            pending_action: None,
            modes: None,
            legacy_models: None,
            config_options: Vec::new(),
            agent_commands: Vec::new(),
            session_has_prompt: false,
            timeline: Vec::new(),
            item_revs: Vec::new(),
            item_expanded: Vec::new(),
            render_cache: Vec::new(),
            render_epoch: 0,
            timeline_hit: TimelineHitMap::default(),
            pending_timeline_anchor: None,
            tool_index: HashMap::new(),
            terminal_owner: HashMap::new(),
            terminals: HashMap::new(),
            plan: Vec::new(),
            plan_markdown: None,
            subagents: Vec::new(),
            file_changes: BTreeMap::new(),
            fs_write_counter: 0,
            usage: SessionUsageState::default(),
            busy: false,
            turn_started: None,
            last_turn_duration: None,
            cancel_requested: false,
            auth_retry_prompt: None,
            queued: VecDeque::new(),
            steering_unsupported: false,
            composer: Composer::new(history),
            completion: None,
            file_index: None,
            file_index_loading: false,
            modal: None,
            permission_queue: VecDeque::new(),
            elicitation_queue: VecDeque::new(),
            rpc_logs: Vec::new(),
            stderr_tail: VecDeque::new(),
            scroll_from_bottom: 0,
            last_total_lines: 0,
            last_timeline_height: 20,
            toasts: Vec::new(),
            show_sidebar,
            show_thoughts,
            expand_tools,
            auto_approve,
            should_quit: false,
            quit_armed_at: None,
            esc_armed_at: None,
            pending_external: None,
            pending_osc52: None,
            spinner_frame: 0,
            dirty: true,
            acp_tx: init.acp_tx,
            msg_tx: init.msg_tx,
        }
    }

    pub fn spinner(&self) -> &'static str {
        SPINNER[self.spinner_frame % SPINNER.len()]
    }

    pub fn caps(&self) -> AgentCapabilities {
        self.init
            .as_ref()
            .map(|i| i.agent_capabilities.clone())
            .unwrap_or_default()
    }

    /// Agent title from `initialize`, without the version.
    pub fn agent_name(&self) -> String {
        self.init
            .as_ref()
            .and_then(|i| i.agent_info.as_ref())
            .map(|a| a.display_name().to_string())
            .unwrap_or_else(|| self.profile.id.clone())
    }

    pub fn agent_display_name(&self) -> String {
        self.init
            .as_ref()
            .and_then(|i| i.agent_info.as_ref())
            .map(|a| {
                if a.version.is_empty() {
                    a.display_name().to_string()
                } else {
                    format!("{} {}", a.display_name(), a.version)
                }
            })
            .unwrap_or_else(|| self.profile.id.clone())
    }

    pub fn rel_path(&self, p: &str) -> String {
        let path = std::path::Path::new(p);
        path.strip_prefix(&self.cwd)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.to_string())
    }

    pub fn toast(&mut self, text: impl Into<String>, is_error: bool) {
        let text: String = text.into();
        // A new "X → value" toast replaces the previous one for the same X.
        if let Some((key, _)) = text.split_once(" → ")
            && let Some(last) = self.toasts.last()
            && last.text.starts_with(&format!("{key} → "))
        {
            self.toasts.pop();
        }
        self.toasts.push(ToastMessage {
            text,
            is_error,
            created_at: Instant::now(),
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    // ------------------------------------------------------------------
    // Timeline helpers
    // ------------------------------------------------------------------

    pub fn push_item(&mut self, item: TimelineItem) -> usize {
        self.timeline.push(item);
        self.item_revs.push(0);
        self.item_expanded.push(None);
        self.render_cache.push(None);
        self.timeline.len() - 1
    }

    pub fn touch(&mut self, idx: usize) {
        if let Some(r) = self.item_revs.get_mut(idx) {
            *r = r.wrapping_add(1);
        }
    }

    pub fn is_item_expanded(&self, idx: usize) -> bool {
        if let Some(Some(explicit)) = self.item_expanded.get(idx) {
            return *explicit;
        }
        match self.timeline.get(idx) {
            Some(TimelineItem::AssistantThought { .. }) => self.show_thoughts || self.expand_tools,
            Some(
                TimelineItem::ToolCall(_)
                | TimelineItem::Compaction { .. }
                | TimelineItem::SubagentMessage { .. },
            ) => self.expand_tools,
            _ => false,
        }
    }

    pub fn toggle_item_expanded(&mut self, idx: usize, old_item_start: Option<usize>) {
        if idx >= self.timeline.len() {
            return;
        }
        let next = !self.is_item_expanded(idx);
        if let Some(slot) = self.item_expanded.get_mut(idx) {
            *slot = Some(next);
        }
        self.touch(idx);
        if let Some(old_item_start) = old_item_start {
            self.pending_timeline_anchor = Some(TimelineScrollAnchor {
                item_idx: idx,
                expanding: next,
                old_start: self.timeline_hit.visible_start,
                old_item_start,
            });
        }
        self.dirty = true;
    }

    fn active_step_index(&self) -> Option<usize> {
        if !self.busy {
            return None;
        }
        for (idx, item) in self.timeline.iter().enumerate().rev() {
            match item {
                TimelineItem::ToolCall(c) if c.is_running() => return Some(idx),
                TimelineItem::AssistantThought { .. } => return Some(idx),
                TimelineItem::AssistantMessage { .. } | TimelineItem::UserMessage { .. } => {
                    return None;
                }
                _ => {}
            }
        }
        None
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push_item(TimelineItem::SystemInfo {
            text: text.into(),
            is_error: false,
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push_item(TimelineItem::SystemInfo {
            text: text.into(),
            is_error: true,
        });
    }

    fn clear_timeline(&mut self) {
        self.timeline.clear();
        self.item_revs.clear();
        self.item_expanded.clear();
        self.render_cache.clear();
        self.timeline_hit.spans.clear();
        self.pending_timeline_anchor = None;
        self.tool_index.clear();
        self.terminal_owner.clear();
        self.scroll_from_bottom = 0;
    }

    fn reset_session_state(&mut self, clear_timeline: bool) {
        if clear_timeline {
            self.clear_timeline();
            self.terminals.clear();
            self.file_changes.clear();
        }
        self.session_id = None;
        self.session_title = None;
        self.modes = None;
        self.legacy_models = None;
        self.config_options.clear();
        self.plan.clear();
        self.plan_markdown = None;
        self.subagents.clear();
        self.usage = SessionUsageState::default();
        self.busy = false;
        self.turn_started = None;
        self.last_turn_duration = None;
        self.pending_updates.clear();
        self.session_has_prompt = false;
        self.cancel_all_interactions();
    }

    // ------------------------------------------------------------------
    // Connection & sessions
    // ------------------------------------------------------------------

    fn default_options_meta(&self) -> Option<Value> {
        let mut root = self
            .profile
            .default_options_json
            .clone()
            .unwrap_or_else(|| serde_json::json!({}));
        let mut defaults = root
            .get("default_config_options")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(m) = &self.cli_mode {
            defaults["mode"] = Value::String(m.clone());
        }
        if let Some(m) = &self.cli_model {
            defaults["model"] = Value::String(m.clone());
        }
        if defaults.as_object().is_some_and(|o| o.is_empty()) {
            return None;
        }
        root["default_config_options"] = defaults.clone();
        root["defaultConfigOptions"] = defaults;
        Some(root)
    }

    pub fn connect(&mut self, action: Option<SessionAction>) {
        self.connect_generation += 1;
        let instance = self.connect_generation;
        self.status = ConnectionStatus::Connecting;
        self.pending_action = action;
        self.init = None;
        let spec = self.profile.to_spawn_spec(self.cwd.clone(), instance);
        let meta = self.default_options_meta();
        let acp_tx = self.acp_tx.clone();
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            match AcpClientHandle::spawn(spec, acp_tx).await {
                Ok(client) => match client.initialize(meta).await {
                    Ok(init) => {
                        let _ = tx.send(AppMsg::Connected {
                            instance,
                            client,
                            init,
                        });
                    }
                    Err(e) => {
                        client.shutdown().await;
                        let _ = tx.send(AppMsg::ConnectFailed {
                            instance,
                            error: format!("initialize failed: {e}"),
                        });
                    }
                },
                Err(e) => {
                    let _ = tx.send(AppMsg::ConnectFailed {
                        instance,
                        error: format!("{e:#}"),
                    });
                }
            }
        });
    }

    pub fn reconnect(&mut self) {
        if let Some(c) = self.client.take() {
            tokio::spawn(async move { c.shutdown().await });
        }
        let action = match &self.session_id {
            Some(id) if self.caps().load_session => SessionAction::Load {
                session_id: id.clone(),
                cwd: self.cwd.to_string_lossy().into_owned(),
            },
            Some(id) if self.caps().supports_resume_session() => SessionAction::Resume {
                session_id: id.clone(),
                cwd: self.cwd.to_string_lossy().into_owned(),
            },
            _ => SessionAction::New,
        };
        let keep_timeline = matches!(action, SessionAction::Resume { .. });
        self.reset_session_state(!keep_timeline);
        self.connect(Some(action));
    }

    pub fn switch_agent(&mut self, id: &str) {
        let Some(p) = self.profiles.iter().find(|p| p.id == id).cloned() else {
            self.toast(format!("Unknown agent: {id}"), true);
            return;
        };
        if let Some(c) = self.client.take() {
            tokio::spawn(async move { c.shutdown().await });
        }
        self.reset_session_state(true);
        self.agent_commands.clear();
        self.steering_unsupported = false;
        self.profile = p;
        self.prefs.last_agent_id = Some(id.to_string());
        self.store.save_prefs(&self.prefs);
        self.connect(Some(SessionAction::New));
    }

    pub fn open_session(&mut self, action: SessionAction) {
        let Some(client) = self.client.clone() else {
            self.pending_action = Some(action);
            return;
        };
        if self.busy
            && let Some(sid) = &self.session_id
        {
            let _ = client.cancel(sid);
        }
        let caps = self.caps();
        let forking = matches!(action, SessionAction::Fork { .. });
        if !forking {
            if let (Some(old), true) = (self.session_id.clone(), caps.supports_close_session()) {
                let c = client.clone();
                tokio::spawn(async move {
                    let _ = c.close_session(&old).await;
                });
            }
            self.reset_session_state(true);
        } else {
            self.busy = false;
            self.session_id = None;
        }
        self.opening = true;
        self.pending_updates.clear();
        let local_latest = self
            .store
            .load_session_records()
            .into_iter()
            .find(|r| r.agent_id == self.profile.id && r.cwd == self.cwd.to_string_lossy())
            .map(|r| r.session_id);
        let tx = self.msg_tx.clone();
        let cwd = self.cwd.to_string_lossy().into_owned();
        let dirs = self.add_dirs.clone();
        let mcp = self.profile.mcp_servers.clone();
        let meta = self.default_options_meta();
        tokio::spawn(async move {
            let result = open_session_task(
                client,
                action.clone(),
                cwd,
                dirs,
                mcp,
                meta,
                caps,
                local_latest,
            )
            .await;
            let _ = match result {
                Ok(opened) => tx.send(AppMsg::SessionOpened(opened)),
                Err(error) => tx.send(AppMsg::SessionOpenFailed { action, error }),
            };
        });
    }

    fn on_session_opened(&mut self, opened: SessionOpened) {
        self.opening = false;
        self.status = ConnectionStatus::Connected;
        self.session_id = Some(opened.session_id.clone());
        self.modes = opened.modes;
        self.legacy_models = opened.models;
        if !opened.config_options.is_empty() {
            self.config_options = opened.config_options;
        }

        // Updates that arrived before the response (history replay for `session/load`).
        let buffered = std::mem::take(&mut self.pending_updates);
        self.replaying = true;
        for n in buffered {
            if n.session_id == opened.session_id {
                self.apply_session_update(n);
            } else {
                self.route_foreign_update(n);
            }
        }
        self.replaying = false;

        match opened.kind {
            OpenedKind::New => {}
            OpenedKind::Loaded => {
                self.session_has_prompt = true;
                self.toast("Session loaded", false);
            }
            OpenedKind::Resumed => {
                self.session_has_prompt = true;
                self.info("Session resumed. The agent does not replay the earlier messages.");
            }
            OpenedKind::Forked => {
                self.session_has_prompt = true;
                self.info(format!(
                    "Forked into a new session ({}).",
                    short_id(&opened.session_id)
                ));
            }
        }
        if opened.kind != OpenedKind::New {
            self.store.upsert_session_record(
                &self.profile.id,
                &opened.session_id,
                &opened.cwd,
                self.session_title.as_deref().unwrap_or(""),
            );
        }

        self.apply_cli_overrides();

        if let Some(p) = self.initial_prompt.take() {
            self.send_prompt(p);
        } else if let Some(next) = self.queued.pop_front() {
            self.send_prompt(next);
        }
        self.dirty = true;
    }

    fn apply_cli_overrides(&mut self) {
        if let Some(mode) = self.cli_mode.take() {
            let exists = self.mode_entries().iter().any(|(id, _, _)| *id == mode);
            if exists && self.current_mode_id().as_deref() != Some(mode.as_str()) {
                self.apply_mode(&mode);
            }
        }
        if let Some(model) = self.cli_model.take() {
            let entries = self.model_entries();
            if let Some((id, _, _)) = entries
                .iter()
                .find(|(id, name, _)| *id == model || name.eq_ignore_ascii_case(&model))
                .cloned()
                && self.current_model_id().as_deref() != Some(id.as_str())
            {
                self.apply_model(&id);
            }
        }
    }

    // ------------------------------------------------------------------
    // Config option helpers (mode / model / thinking)
    // ------------------------------------------------------------------

    pub fn config_by_category(&self, cat: &str) -> Option<&SessionConfigOption> {
        self.config_options
            .iter()
            .find(|o| o.category.as_deref() == Some(cat))
            .or_else(|| self.config_options.iter().find(|o| o.id == cat))
    }

    pub fn thinking_option(&self) -> Option<&SessionConfigOption> {
        self.config_options
            .iter()
            .find(|o| o.category.as_deref() == Some("thought_level"))
            .or_else(|| {
                self.config_options.iter().find(|o| {
                    matches!(
                        o.id.as_str(),
                        "thinking" | "reasoning_effort" | "effort" | "thought_level" | "reasoning"
                    )
                })
            })
    }

    pub fn mode_entries(&self) -> Vec<(String, String, Option<String>)> {
        if let Some(opt) = self.config_by_category("mode").filter(|o| !o.is_boolean()) {
            return opt
                .options
                .flatten()
                .into_iter()
                .map(|o| (o.value, o.name, o.description))
                .collect();
        }
        self.modes
            .as_ref()
            .map(|m| {
                m.available_modes
                    .iter()
                    .map(|md| (md.id.clone(), md.name.clone(), md.description.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn current_mode_id(&self) -> Option<String> {
        if let Some(opt) = self.config_by_category("mode").filter(|o| !o.is_boolean()) {
            return Some(opt.current_str());
        }
        self.modes.as_ref().map(|m| m.current_mode_id.clone())
    }

    pub fn current_mode_label(&self) -> Option<String> {
        let id = self.current_mode_id()?;
        Some(
            self.mode_entries()
                .into_iter()
                .find(|(mid, _, _)| *mid == id)
                .map(|(_, name, _)| name)
                .unwrap_or(id),
        )
    }

    pub fn model_entries(&self) -> Vec<(String, String, Option<String>)> {
        if let Some(opt) = self.config_by_category("model").filter(|o| !o.is_boolean()) {
            return opt
                .options
                .flatten()
                .into_iter()
                .map(|o| (o.value, o.name, o.description))
                .collect();
        }
        self.legacy_models
            .as_ref()
            .map(|m| {
                m.available_models
                    .iter()
                    .map(|md| (md.model_id.clone(), md.name.clone(), md.description.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn current_model_id(&self) -> Option<String> {
        if let Some(opt) = self.config_by_category("model").filter(|o| !o.is_boolean()) {
            return Some(opt.current_str());
        }
        self.legacy_models.as_ref().map(|m| m.current_model_id.clone())
    }

    pub fn current_model_label(&self) -> Option<String> {
        let id = self.current_model_id()?;
        Some(
            self.model_entries()
                .into_iter()
                .find(|(mid, _, _)| *mid == id)
                .map(|(_, name, _)| name)
                .unwrap_or(id),
        )
    }

    fn set_local_config_value(&mut self, config_id: &str, value: Value) {
        if let Some(opt) = self.config_options.iter_mut().find(|o| o.id == config_id) {
            opt.current_value = value;
        }
    }

    pub fn set_config_select(&mut self, config_id: String, value: String) {
        let (Some(client), Some(sid)) = (self.client.clone(), self.session_id.clone()) else {
            return;
        };
        self.set_local_config_value(&config_id, Value::String(value.clone()));
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            match client.set_config_option_select(&sid, &config_id, &value).await {
                Ok(r) => {
                    let _ = tx.send(AppMsg::ConfigOptions(r.config_options));
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::Error(format!("Could not set {config_id}: {}", e.message)));
                }
            }
        });
    }

    pub fn set_config_bool(&mut self, config_id: String, value: bool) {
        let (Some(client), Some(sid)) = (self.client.clone(), self.session_id.clone()) else {
            return;
        };
        self.set_local_config_value(&config_id, Value::Bool(value));
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            match client.set_config_option_bool(&sid, &config_id, value).await {
                Ok(r) => {
                    let _ = tx.send(AppMsg::ConfigOptions(r.config_options));
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::Error(format!("Could not set {config_id}: {}", e.message)));
                }
            }
        });
    }

    pub fn apply_mode(&mut self, mode_id: &str) {
        if let Some(opt) = self
            .config_by_category("mode")
            .filter(|o| !o.is_boolean())
            .cloned()
        {
            self.set_config_select(opt.id, mode_id.to_string());
        } else if self.modes.is_some() {
            let (Some(client), Some(sid)) = (self.client.clone(), self.session_id.clone()) else {
                return;
            };
            if let Some(m) = self.modes.as_mut() {
                m.current_mode_id = mode_id.to_string();
            }
            let tx = self.msg_tx.clone();
            let mode = mode_id.to_string();
            tokio::spawn(async move {
                if let Err(e) = client.set_mode(&sid, &mode).await {
                    let _ = tx.send(AppMsg::Error(format!("Could not set mode: {}", e.message)));
                }
            });
        } else {
            self.toast("This agent does not expose modes", true);
            return;
        }
        let name = self
            .mode_entries()
            .into_iter()
            .find(|(id, _, _)| id == mode_id)
            .map(|(_, n, _)| n)
            .unwrap_or_else(|| mode_id.to_string());
        let lower = mode_id.to_lowercase();
        if lower.contains("yolo") || lower.contains("bypass") {
            self.toast(format!("Mode → {name} (tools run without review)"), false);
        } else {
            self.toast(format!("Mode → {name}"), false);
        }
    }

    pub fn apply_model(&mut self, model_id: &str) {
        if let Some(opt) = self
            .config_by_category("model")
            .filter(|o| !o.is_boolean())
            .cloned()
        {
            self.set_config_select(opt.id, model_id.to_string());
        } else if self.legacy_models.is_some() {
            let (Some(client), Some(sid)) = (self.client.clone(), self.session_id.clone()) else {
                return;
            };
            if let Some(m) = self.legacy_models.as_mut() {
                m.current_model_id = model_id.to_string();
            }
            let tx = self.msg_tx.clone();
            let model = model_id.to_string();
            tokio::spawn(async move {
                if let Err(e) = client.set_model_legacy(&sid, &model).await {
                    let _ = tx.send(AppMsg::Error(format!("Could not set model: {}", e.message)));
                }
            });
        } else {
            self.toast("This agent does not expose model selection", true);
            return;
        }
        let name = self
            .model_entries()
            .into_iter()
            .find(|(id, _, _)| id == model_id)
            .map(|(_, n, _)| n)
            .unwrap_or_else(|| model_id.to_string());
        self.toast(format!("Model → {name}"), false);
    }

    pub fn cycle_mode(&mut self) {
        let entries = self.mode_entries();
        if entries.is_empty() {
            self.toast("This agent does not expose modes", true);
            return;
        }
        let cur = self.current_mode_id();
        let idx = entries
            .iter()
            .position(|(id, _, _)| Some(id) == cur.as_ref())
            .map(|i| (i + 1) % entries.len())
            .unwrap_or(0);
        let (id, _, _) = entries[idx].clone();
        self.apply_mode(&id);
    }

    // ------------------------------------------------------------------
    // Messages from async tasks
    // ------------------------------------------------------------------

    pub fn handle_msg(&mut self, msg: AppMsg) {
        self.dirty = true;
        match msg {
            AppMsg::Connected {
                instance,
                client,
                init,
            } => {
                if instance != self.connect_generation {
                    tokio::spawn(async move { client.shutdown().await });
                    return;
                }
                self.client = Some(client);
                self.init = Some(init);
                self.status = ConnectionStatus::Connected;
                let action = self.pending_action.take().unwrap_or(SessionAction::New);
                self.open_session(action);
            }
            AppMsg::ConnectFailed { instance, error } => {
                if instance != self.connect_generation {
                    return;
                }
                self.status = ConnectionStatus::Disconnected;
                self.error(format!(
                    "Could not start agent \"{}\" ({}): {error}",
                    self.profile.id,
                    self.profile.command_line_display()
                ));
                self.append_stderr_hint();
                self.info("Press Ctrl+G to choose another agent, or Ctrl+P → Reconnect.");
            }
            AppMsg::SessionOpened(opened) => self.on_session_opened(opened),
            AppMsg::SessionOpenFailed { action, error } => {
                self.opening = false;
                let buffered = std::mem::take(&mut self.pending_updates);
                drop(buffered);
                if error.is_auth_required() {
                    self.status = ConnectionStatus::AuthRequired;
                    self.pending_action = Some(action);
                    self.error("The agent requires authentication.");
                    self.open_auth_picker();
                    return;
                }
                self.error(format!("Could not open the session: {}", error.message));
                if matches!(
                    action,
                    SessionAction::Load { .. } | SessionAction::Resume { .. } | SessionAction::ContinueLast
                ) {
                    self.info("Starting a new session instead.");
                    self.open_session(SessionAction::New);
                }
            }
            AppMsg::PromptDone { session_id, result } => {
                if self.session_id.as_deref() != Some(session_id.as_str()) {
                    return;
                }
                self.on_prompt_done(result);
            }
            AppMsg::SteerResult {
                text,
                attachments,
                result,
            } => {
                let injected = matches!(
                    &result,
                    Ok(v) if v.get("outcome").and_then(|o| o.as_str()) == Some("injected")
                );
                if injected {
                    self.push_item(TimelineItem::UserMessage {
                        id: uuid::Uuid::new_v4().to_string(),
                        text,
                        attachments,
                        timestamp: chrono::Local::now(),
                        steered: true,
                    });
                    self.scroll_from_bottom = 0;
                } else {
                    if let Err(e) = &result
                        && e.code == ERR_METHOD_NOT_FOUND
                    {
                        self.steering_unsupported = true;
                    }
                    if self.busy {
                        self.queued.push_back(text);
                        self.toast("Queued — sends after the current turn", false);
                    } else {
                        self.send_prompt(text);
                    }
                }
            }
            AppMsg::SessionsListed(sessions) => {
                if let Some(ActiveModal::Sessions(state)) = self.modal.as_mut() {
                    state.sessions = sessions;
                    state.loading = false;
                    state.selected = 0;
                }
            }
            AppMsg::SessionDeleted(id) => {
                self.store.remove_session_record(&self.profile.id, &id);
                if let Some(ActiveModal::Sessions(state)) = self.modal.as_mut() {
                    state.sessions.retain(|s| s.session_id != id);
                    state.selected = state.selected.min(state.sessions.len().saturating_sub(1));
                }
                self.toast("Session deleted", false);
            }
            AppMsg::ConfigOptions(opts) => {
                if !opts.is_empty() {
                    self.config_options = opts;
                }
            }
            AppMsg::AuthDone(result) => match result {
                Ok(()) => {
                    self.toast("Authenticated", false);
                    self.after_auth_success();
                }
                Err(e) => {
                    self.error(format!("Authentication failed: {}", e.message));
                }
            },
            AppMsg::FileIndex(files) => {
                self.file_index = Some(files);
                self.file_index_loading = false;
                self.refresh_completion();
            }
            AppMsg::Info(t) => self.toast(t, false),
            AppMsg::Error(t) => self.toast(t, true),
        }
    }

    fn append_stderr_hint(&mut self) {
        if self.stderr_tail.is_empty() {
            return;
        }
        let tail: Vec<String> = self
            .stderr_tail
            .iter()
            .rev()
            .take(6)
            .rev()
            .cloned()
            .collect();
        self.error(format!("Agent stderr:\n{}", tail.join("\n")));
    }

    fn on_prompt_done(&mut self, result: Result<PromptResponse, RpcError>) {
        self.busy = false;
        self.last_turn_duration = self.turn_started.take().map(|t| t.elapsed());
        let cancelled = match &result {
            Ok(r) => r.stop_reason == "cancelled",
            Err(_) => self.cancel_requested,
        };
        match result {
            Ok(resp) => {
                if let Some(u) = resp.usage {
                    self.usage.cumulative_tokens += u.total_tokens;
                    self.usage.last_turn_usage = Some(u);
                }
                match resp.stop_reason.as_str() {
                    "end_turn" => {}
                    "cancelled" => self.info("Interrupted by user"),
                    "refusal" => self.error("The agent refused to continue."),
                    "max_tokens" => self.error("The reply stopped: maximum token limit reached."),
                    "max_turn_requests" => {
                        self.error("The turn stopped: maximum number of model requests reached.")
                    }
                    other => self.info(format!("Turn ended ({other})")),
                }
            }
            Err(e) => {
                if e.is_auth_required() {
                    self.status = ConnectionStatus::AuthRequired;
                    self.auth_retry_prompt = self.timeline.iter().rev().find_map(|item| match item {
                        TimelineItem::UserMessage { text, .. } => Some(text.clone()),
                        _ => None,
                    });
                    self.error("The agent requires authentication. The message is sent again after login.");
                    self.open_auth_picker();
                } else if !self.cancel_requested {
                    self.error(format!("Prompt failed: {}", e.message));
                } else {
                    self.info("Interrupted by user");
                }
            }
        }
        if cancelled {
            let ids: Vec<usize> = self.tool_index.values().copied().collect();
            for idx in ids {
                if let Some(TimelineItem::ToolCall(card)) = self.timeline.get_mut(idx)
                    && card.is_running()
                {
                    card.status = "cancelled".to_string();
                    self.touch(idx);
                }
            }
        }
        self.cancel_requested = false;
        self.cancel_all_interactions();
        if let Some(next) = self.queued.pop_front() {
            self.send_prompt(next);
        }
    }

    // ------------------------------------------------------------------
    // Events from the ACP connection
    // ------------------------------------------------------------------

    pub fn handle_acp(&mut self, ev: AcpEvent) {
        match ev {
            AcpEvent::RpcLog(entry) => {
                merge_rpc_log(&mut self.rpc_logs, entry);
                if matches!(self.modal, Some(ActiveModal::Logs(_))) {
                    self.dirty = true;
                }
                return;
            }
            AcpEvent::StderrLine(line) => {
                if self.stderr_tail.len() >= 200 {
                    self.stderr_tail.pop_front();
                }
                self.stderr_tail.push_back(line);
                return;
            }
            _ => {}
        }
        self.dirty = true;
        match ev {
            AcpEvent::SessionUpdate(n) => {
                if self.opening {
                    self.pending_updates.push(n);
                } else if self.session_id.as_deref() == Some(n.session_id.as_str()) {
                    self.apply_session_update(n);
                } else {
                    self.route_foreign_update(n);
                }
            }
            AcpEvent::PermissionRequest(p) => self.on_permission_request(p),
            AcpEvent::ElicitationRequest(e) => self.on_elicitation_request(e),
            AcpEvent::ElicitationComplete { elicitation_id } => {
                self.toast(format!("Elicitation {} complete", short_id(&elicitation_id)), false);
            }
            AcpEvent::TerminalUpdated(snap) => {
                let id = snap.terminal_id.clone();
                update_terminal_map(&mut self.terminals, snap);
                if let Some(&idx) = self.terminal_owner.get(&id) {
                    self.touch(idx);
                }
            }
            AcpEvent::FileWritten(rec) => {
                self.fs_write_counter += 1;
                let stats = count_diff(rec.old_text.as_deref(), &rec.new_text);
                let key = format!("fs:{}", self.fs_write_counter);
                self.file_changes
                    .entry(rec.path.clone())
                    .or_default()
                    .insert(key, stats);
            }
            AcpEvent::ExtNotification { method, params } => {
                if method.ends_with("update_todos")
                    && let Some(todos) = params.get("todos").and_then(|t| t.as_array())
                {
                    self.plan = todos
                        .iter()
                        .filter_map(|t| {
                            let content = t
                                .get("content")
                                .or_else(|| t.get("title"))
                                .and_then(|c| c.as_str())?;
                            let status = t
                                .get("status")
                                .and_then(|s| s.as_str())
                                .unwrap_or("pending")
                                .to_lowercase();
                            let status = if status.contains("progress") {
                                "in_progress"
                            } else if status.contains("complete") || status == "done" {
                                "completed"
                            } else {
                                "pending"
                            };
                            Some(PlanEntry {
                                content: content.to_string(),
                                priority: "medium".to_string(),
                                status: status.to_string(),
                            })
                        })
                        .collect();
                }
            }
            AcpEvent::Disconnected { instance, reason } => {
                if instance != self.connect_generation {
                    return;
                }
                self.client = None;
                self.status = ConnectionStatus::Disconnected;
                self.busy = false;
                self.opening = false;
                self.cancel_all_interactions();
                self.error(format!("Agent disconnected: {reason}"));
                self.append_stderr_hint();
                self.info("Press Ctrl+P → Reconnect, or Ctrl+G to choose another agent.");
            }
            AcpEvent::RpcLog(_) | AcpEvent::StderrLine(_) => {}
        }
    }

    /// Updates for sessions other than the current one: subagent children are summarized in the sidebar.
    fn route_foreign_update(&mut self, n: SessionNotification) {
        let Some(sub) = self
            .subagents
            .iter_mut()
            .find(|s| s.session_id == n.session_id)
        else {
            return;
        };
        match n.update {
            SessionUpdate::ToolCall(t) => {
                sub.tool_count += 1;
                sub.activity = Some(t.title);
            }
            SessionUpdate::ToolCallUpdate(t) => {
                if let Some(title) = t.title {
                    sub.activity = Some(title);
                }
            }
            SessionUpdate::AgentThoughtChunk(_) => sub.activity = Some("thinking…".to_string()),
            SessionUpdate::AgentMessageChunk(_) => sub.activity = Some("writing…".to_string()),
            _ => {}
        }
    }

    pub fn apply_session_update(&mut self, n: SessionNotification) {
        match n.update {
            SessionUpdate::UserMessageChunk(c) => {
                if self.busy && !self.replaying {
                    return;
                }
                let text = c.content.as_display_text();
                let last = self.timeline.len().checked_sub(1);
                let mut appended = false;
                if let Some(idx) = last
                    && let TimelineItem::UserMessage { id, text: t, .. } = &mut self.timeline[idx]
                    && c.message_id.as_deref().is_some_and(|m| m == id)
                {
                    t.push_str(&text);
                    appended = true;
                }
                if appended {
                    self.touch(last.unwrap_or(0));
                } else {
                    if self.session_title.is_none() {
                        self.session_title = Some(title_from_prompt(&text));
                    }
                    self.push_item(TimelineItem::UserMessage {
                        id: c
                            .message_id
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        text,
                        attachments: Vec::new(),
                        timestamp: chrono::Local::now(),
                        steered: false,
                    });
                }
            }
            SessionUpdate::AgentMessageChunk(c) => {
                let text = c.content.as_display_text();
                if text.is_empty() {
                    return;
                }
                let last = self.timeline.len().checked_sub(1);
                let mut appended = false;
                if let Some(idx) = last
                    && let TimelineItem::AssistantMessage { id, text: t, .. } = &mut self.timeline[idx]
                    && (c.message_id.is_none() || id.is_none() || *id == c.message_id)
                {
                    t.push_str(&text);
                    appended = true;
                }
                if appended {
                    self.touch(last.unwrap_or(0));
                } else {
                    self.push_item(TimelineItem::AssistantMessage {
                        id: c.message_id,
                        text,
                        timestamp: chrono::Local::now(),
                    });
                }
            }
            SessionUpdate::AgentThoughtChunk(c) => {
                let text = c.content.as_display_text();
                if text.is_empty() {
                    return;
                }
                let last = self.timeline.len().checked_sub(1);
                let mut appended = false;
                if let Some(idx) = last
                    && let TimelineItem::AssistantThought { id, text: t } = &mut self.timeline[idx]
                    && (c.message_id.is_none() || id.is_none() || *id == c.message_id)
                {
                    t.push_str(&text);
                    appended = true;
                }
                if appended {
                    self.touch(last.unwrap_or(0));
                } else {
                    self.push_item(TimelineItem::AssistantThought {
                        id: c.message_id,
                        text,
                    });
                }
            }
            SessionUpdate::ToolCall(ev) => {
                let idx = self.ensure_tool(&ev.tool_call_id);
                self.apply_tool_fields(
                    idx,
                    Some(ev.title),
                    ev.name,
                    ev.kind,
                    ev.status,
                    Some(ev.content),
                    Some(ev.locations),
                    ev.raw_input,
                    ev.raw_output,
                    ev.meta,
                );
            }
            SessionUpdate::ToolCallUpdate(ev) => {
                let idx = self.ensure_tool(&ev.tool_call_id);
                self.apply_tool_fields(
                    idx,
                    ev.title,
                    ev.name,
                    ev.kind,
                    ev.status,
                    ev.content,
                    ev.locations,
                    ev.raw_input,
                    ev.raw_output,
                    ev.meta,
                );
            }
            SessionUpdate::Plan(p) => {
                update_plan_entries(&mut self.plan, p.entries);
                self.plan_markdown = None;
            }
            SessionUpdate::PlanUpdate(p) => match p.plan {
                PlanUpdateContent::Items { entries, .. } => {
                    self.plan = entries;
                    self.plan_markdown = None;
                }
                PlanUpdateContent::Markdown { content, .. } => {
                    self.plan_markdown = Some(content);
                }
                PlanUpdateContent::File { uri, .. } => {
                    self.plan_markdown = Some(format!("Plan file: {uri}"));
                }
                PlanUpdateContent::Unknown => {}
            },
            SessionUpdate::PlanRemoved => {
                self.plan.clear();
                self.plan_markdown = None;
            }
            SessionUpdate::AvailableCommandsUpdate { available_commands } => {
                self.agent_commands = available_commands
                    .iter()
                    .map(SlashCommandDef::from_acp)
                    .collect();
                self.refresh_completion();
            }
            SessionUpdate::CurrentModeUpdate { current_mode_id } => {
                if let Some(m) = self.modes.as_mut() {
                    m.current_mode_id = current_mode_id.clone();
                }
                if let Some(opt) = self
                    .config_options
                    .iter_mut()
                    .find(|o| o.category.as_deref() == Some("mode") || o.id == "mode")
                {
                    opt.current_value = Value::String(current_mode_id);
                }
            }
            SessionUpdate::ConfigOptionUpdate { config_options } => {
                if !config_options.is_empty() {
                    self.config_options = config_options;
                }
            }
            SessionUpdate::SessionInfoUpdate(info) => {
                if let Some(t) = info.title.filter(|t| !t.trim().is_empty()) {
                    self.session_title = Some(t.clone());
                    if let Some(sid) = self.session_id.clone() {
                        self.store.upsert_session_record(
                            &self.profile.id,
                            &sid,
                            &self.cwd.to_string_lossy(),
                            &t,
                        );
                    }
                }
            }
            SessionUpdate::UsageUpdate(u) => {
                self.usage.context_used = u.used;
                self.usage.context_size = u.size;
                if u.cost.is_some() {
                    self.usage.cost = u.cost;
                }
            }
            SessionUpdate::Notice(notice) => {
                if !self.replaying {
                    self.toast(notice.title.clone(), notice.severity == "error");
                }
                self.push_item(TimelineItem::Notice {
                    severity: notice.severity,
                    title: notice.title,
                    description: notice.description,
                });
            }
            SessionUpdate::CompactionUpdate(c) => {
                let summary_text = c.summary.as_ref().map(|blocks| {
                    blocks
                        .iter()
                        .map(ContentBlock::as_display_text)
                        .collect::<Vec<_>>()
                        .join("")
                });
                let found = self.timeline.iter().position(|item| {
                    matches!(item, TimelineItem::Compaction { compaction_id, .. } if *compaction_id == c.compaction_id)
                });
                if let Some(idx) = found {
                    if let TimelineItem::Compaction {
                        status,
                        summary,
                        error,
                        ..
                    } = &mut self.timeline[idx]
                    {
                        *status = c.status;
                        if let Some(s) = summary_text {
                            *summary = s;
                        }
                        if c.error.is_some() {
                            *error = c.error;
                        }
                    }
                    self.touch(idx);
                } else {
                    self.push_item(TimelineItem::Compaction {
                        compaction_id: c.compaction_id,
                        status: c.status,
                        summary: summary_text.unwrap_or_default(),
                        error: c.error,
                    });
                }
            }
            SessionUpdate::CompactionSummaryChunk {
                compaction_id,
                content,
            } => {
                let text = content.as_display_text();
                let found = self.timeline.iter().position(|item| {
                    matches!(item, TimelineItem::Compaction { compaction_id: cid, .. } if *cid == compaction_id)
                });
                if let Some(idx) = found {
                    if let TimelineItem::Compaction { summary, .. } = &mut self.timeline[idx] {
                        summary.push_str(&text);
                    }
                    self.touch(idx);
                } else {
                    self.push_item(TimelineItem::Compaction {
                        compaction_id,
                        status: "in_progress".to_string(),
                        summary: text,
                        error: None,
                    });
                }
            }
            SessionUpdate::SubagentUpdate(s) => {
                let (label, running) = s
                    .state
                    .as_ref()
                    .map(subagent_state_label)
                    .unwrap_or_else(|| ("running".to_string(), true));
                if let Some(existing) = self
                    .subagents
                    .iter_mut()
                    .find(|x| x.session_id == s.session_id)
                {
                    if let Some(t) = s.title {
                        existing.title = t;
                    }
                    if s.description.is_some() {
                        existing.description = s.description;
                    }
                    if s.state.is_some() {
                        existing.state_label = label;
                        existing.is_running = running;
                    }
                } else {
                    self.subagents.push(SubagentSummary {
                        session_id: s.session_id,
                        title: s.title.unwrap_or_else(|| "Subagent".to_string()),
                        description: s.description,
                        state_label: label,
                        is_running: running,
                        activity: None,
                        tool_count: 0,
                    });
                }
            }
            SessionUpdate::SessionMessage(m) => {
                let text = m
                    .content
                    .unwrap_or_default()
                    .iter()
                    .map(ContentBlock::as_display_text)
                    .collect::<Vec<_>>()
                    .join("");
                self.upsert_subagent_message(
                    m.message_id,
                    m.sender_session_id,
                    m.recipient_session_id,
                    text,
                    true,
                );
            }
            SessionUpdate::SessionMessageChunk(m) => {
                let text = m.content.as_display_text();
                self.upsert_subagent_message(
                    m.message_id,
                    m.sender_session_id,
                    m.recipient_session_id,
                    text,
                    false,
                );
            }
            SessionUpdate::Unknown => {}
        }
    }

    fn upsert_subagent_message(
        &mut self,
        message_id: String,
        sender: Option<String>,
        recipient: Option<String>,
        text: String,
        replace: bool,
    ) {
        let found = self.timeline.iter().position(|item| {
            matches!(item, TimelineItem::SubagentMessage { message_id: mid, .. } if *mid == message_id)
        });
        if let Some(idx) = found {
            if let TimelineItem::SubagentMessage { text: t, .. } = &mut self.timeline[idx] {
                if replace {
                    *t = text;
                } else {
                    t.push_str(&text);
                }
            }
            self.touch(idx);
            return;
        }
        let to_subagent = sender.as_deref() == self.session_id.as_deref();
        let other = if to_subagent { recipient } else { sender };
        let title = other
            .as_ref()
            .and_then(|sid| self.subagents.iter().find(|s| s.session_id == *sid))
            .map(|s| s.title.clone())
            .unwrap_or_else(|| "Subagent".to_string());
        self.push_item(TimelineItem::SubagentMessage {
            message_id,
            subagent_title: title,
            text,
            to_subagent,
        });
    }

    fn ensure_tool(&mut self, id: &str) -> usize {
        if let Some(&i) = self.tool_index.get(id) {
            return i;
        }
        let card = ToolCallCard {
            tool_call_id: id.to_string(),
            title: String::new(),
            name: None,
            kind: "other".to_string(),
            status: "pending".to_string(),
            locations: Vec::new(),
            diffs: Vec::new(),
            text_outputs: Vec::new(),
            terminal_ids: Vec::new(),
            raw_input: None,
            raw_output: None,
            started_at: Instant::now(),
            duration_ms: None,
        };
        let idx = self.push_item(TimelineItem::ToolCall(card));
        self.tool_index.insert(id.to_string(), idx);
        idx
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_tool_fields(
        &mut self,
        idx: usize,
        title: Option<String>,
        name: Option<String>,
        kind: Option<String>,
        status: Option<String>,
        content: Option<Vec<ToolCallContent>>,
        locations: Option<Vec<ToolCallLocation>>,
        raw_input: Option<Value>,
        raw_output: Option<Value>,
        meta: Option<Value>,
    ) {
        let mut new_diffs: Vec<(String, Option<String>, String)> = Vec::new();
        let mut new_terminals: Vec<String> = Vec::new();
        let (tool_id, command_hint) = {
            let TimelineItem::ToolCall(card) = &mut self.timeline[idx] else {
                return;
            };
            if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
                card.title = t;
            }
            if name.is_some() {
                card.name = name;
            }
            if let Some(k) = kind {
                card.kind = k;
            }
            if let Some(s) = status {
                let finished = matches!(s.as_str(), "completed" | "failed");
                if finished && card.duration_ms.is_none() {
                    card.duration_ms = Some(card.started_at.elapsed().as_millis());
                }
                card.status = s;
            }
            if let Some(locs) = locations {
                card.locations = locs;
            }
            if raw_input.is_some() {
                card.raw_input = raw_input;
            }
            if raw_output.is_some() {
                card.raw_output = raw_output;
            }
            if let Some(items) = content {
                card.diffs.clear();
                card.text_outputs.clear();
                card.terminal_ids.clear();
                for item in items {
                    match item {
                        ToolCallContent::Content { content } => {
                            let t = content.as_display_text();
                            if !t.trim().is_empty() {
                                card.text_outputs.push(t);
                            }
                        }
                        ToolCallContent::Diff {
                            path,
                            old_text,
                            new_text,
                        } => {
                            new_diffs.push((path.clone(), old_text.clone(), new_text.clone()));
                            card.diffs.push(ToolDiffItem {
                                path,
                                old_text,
                                new_text,
                            });
                        }
                        ToolCallContent::Terminal { terminal_id } => {
                            new_terminals.push(terminal_id.clone());
                            card.terminal_ids.push(terminal_id);
                        }
                        ToolCallContent::Unknown => {}
                    }
                }
            }
            if card.title.is_empty() {
                card.title = card
                    .raw_input
                    .as_ref()
                    .and_then(|r| r.get("command").and_then(|c| c.as_str()))
                    .map(String::from)
                    .or_else(|| card.name.clone())
                    .unwrap_or_else(|| "Tool call".to_string());
            }
            let cmd = card
                .raw_input
                .as_ref()
                .and_then(|r| r.get("command").and_then(|c| c.as_str()))
                .map(String::from)
                .unwrap_or_else(|| card.title.clone());
            (card.tool_call_id.clone(), cmd)
        };

        for tid in new_terminals {
            self.terminal_owner.insert(tid, idx);
        }
        for (path, old, new) in new_diffs {
            let stats = count_diff(old.as_deref(), &new);
            self.file_changes
                .entry(path)
                .or_default()
                .insert(tool_id.clone(), stats);
        }
        if let Some(m) = meta {
            self.apply_terminal_meta(idx, &m, &command_hint);
        }
        self.touch(idx);
    }

    /// Zed-style `_meta.terminal_info` / `terminal_output` / `terminal_exit` tool-call extensions.
    fn apply_terminal_meta(&mut self, idx: usize, meta: &Value, command: &str) {
        let session_id = self.session_id.clone().unwrap_or_default();
        let ensure = |terminals: &mut HashMap<String, TerminalSnapshot>, tid: &str| {
            terminals
                .entry(tid.to_string())
                .or_insert_with(|| TerminalSnapshot {
                    terminal_id: tid.to_string(),
                    session_id: session_id.clone(),
                    command: command.to_string(),
                    cwd: String::new(),
                    output: String::new(),
                    truncated: false,
                    exit_status: None,
                });
        };
        let mut owned: Vec<String> = Vec::new();
        if let Some(info) = meta.get("terminal_info")
            && let Some(tid) = info.get("terminal_id").and_then(|t| t.as_str())
        {
            ensure(&mut self.terminals, tid);
            if let (Some(cwd), Some(t)) = (
                info.get("cwd").and_then(|c| c.as_str()),
                self.terminals.get_mut(tid),
            ) {
                t.cwd = cwd.to_string();
            }
            owned.push(tid.to_string());
        }
        for key in ["terminal_output", "terminal_output_delta"] {
            if let Some(out) = meta.get(key)
                && let Some(tid) = out.get("terminal_id").and_then(|t| t.as_str())
            {
                ensure(&mut self.terminals, tid);
                if let (Some(data), Some(t)) = (
                    out.get("data").and_then(|d| d.as_str()),
                    self.terminals.get_mut(tid),
                ) {
                    t.output.push_str(data);
                }
                owned.push(tid.to_string());
            }
        }
        if let Some(exit) = meta.get("terminal_exit")
            && let Some(tid) = exit.get("terminal_id").and_then(|t| t.as_str())
        {
            ensure(&mut self.terminals, tid);
            if let Some(t) = self.terminals.get_mut(tid) {
                t.exit_status = Some(TerminalExitStatus {
                    exit_code: exit
                        .get("exit_code")
                        .and_then(|c| c.as_i64())
                        .map(|c| c as i32),
                    signal: exit
                        .get("signal")
                        .and_then(|s| s.as_str())
                        .map(String::from),
                });
            }
            owned.push(tid.to_string());
        }
        for tid in owned {
            self.terminal_owner.insert(tid.clone(), idx);
            if let TimelineItem::ToolCall(card) = &mut self.timeline[idx]
                && !card.terminal_ids.contains(&tid)
            {
                card.terminal_ids.push(tid);
            }
        }
    }

    // ------------------------------------------------------------------
    // Permissions & elicitations
    // ------------------------------------------------------------------

    fn on_permission_request(&mut self, p: PendingPermission) {
        let tc = p.request.tool_call.clone();
        let idx = self.ensure_tool(&tc.tool_call_id);
        self.apply_tool_fields(
            idx,
            tc.title,
            tc.name,
            tc.kind,
            None,
            tc.content,
            tc.locations,
            tc.raw_input,
            None,
            None,
        );

        if self.auto_approve {
            let pick = p
                .request
                .options
                .iter()
                .find(|o| o.kind == "allow_always")
                .or_else(|| p.request.options.iter().find(|o| o.kind == "allow_once"))
                .map(|o| o.option_id.clone());
            if let Some(option_id) = pick {
                let _ = p.responder.send(RequestPermissionResponse {
                    outcome: RequestPermissionOutcome::Selected { option_id },
                });
                return;
            }
        }
        self.permission_queue.push_back(p);
        self.show_next_interaction();
    }

    fn on_elicitation_request(&mut self, e: PendingElicitation) {
        self.elicitation_queue.push_back(e);
        self.show_next_interaction();
    }

    fn modal_is_interaction(&self) -> bool {
        matches!(
            self.modal,
            Some(ActiveModal::Permission(_)) | Some(ActiveModal::Elicitation(_))
        )
    }

    fn show_next_interaction(&mut self) {
        if self.modal_is_interaction() {
            return;
        }
        if let Some(p) = self.permission_queue.pop_front() {
            let selected = p
                .request
                .options
                .iter()
                .position(|o| o.kind == "allow_once")
                .unwrap_or(0);
            self.modal = Some(ActiveModal::Permission(PermissionModalState {
                pending: p,
                selected,
                diff_scroll: 0,
            }));
            self.completion = None;
        } else if let Some(e) = self.elicitation_queue.pop_front() {
            let is_url_mode = e.request.mode == "url";
            let fields = build_elicitation_fields(e.request.schema.as_ref());
            self.modal = Some(ActiveModal::Elicitation(ElicitationModalState {
                pending: e,
                is_url_mode,
                fields,
                focused_field: 0,
            }));
            self.completion = None;
        }
    }

    fn cancel_all_interactions(&mut self) {
        let modal = self.modal.take();
        match modal {
            Some(ActiveModal::Permission(state)) => {
                let _ = state.pending.responder.send(RequestPermissionResponse {
                    outcome: RequestPermissionOutcome::Cancelled,
                });
            }
            Some(ActiveModal::Elicitation(state)) => {
                let _ = state.pending.responder.send(CreateElicitationResponse {
                    action: "cancel".to_string(),
                    content: None,
                });
            }
            other => self.modal = other,
        }
        for p in self.permission_queue.drain(..) {
            let _ = p.responder.send(RequestPermissionResponse {
                outcome: RequestPermissionOutcome::Cancelled,
            });
        }
        for e in self.elicitation_queue.drain(..) {
            let _ = e.responder.send(CreateElicitationResponse {
                action: "cancel".to_string(),
                content: None,
            });
        }
    }

    fn respond_permission(&mut self, option_id: Option<String>) {
        if let Some(ActiveModal::Permission(state)) = self.modal.take() {
            let chosen_kind = option_id.as_ref().and_then(|id| {
                state
                    .pending
                    .request
                    .options
                    .iter()
                    .find(|o| o.option_id == *id)
                    .map(|o| o.kind.clone())
            });
            let outcome = match option_id {
                Some(option_id) => RequestPermissionOutcome::Selected { option_id },
                None => RequestPermissionOutcome::Cancelled,
            };
            let _ = state
                .pending
                .responder
                .send(RequestPermissionResponse { outcome });
            if chosen_kind.as_deref() == Some("reject_once")
                || chosen_kind.as_deref() == Some("reject_always")
            {
                self.toast("Rejected — tell the agent what to do differently", false);
            }
        }
        self.show_next_interaction();
    }

    fn respond_elicitation(&mut self, action: &str) {
        let Some(ActiveModal::Elicitation(state)) = self.modal.take() else {
            return;
        };
        if action == "accept" && !state.is_url_mode {
            let mut content = HashMap::new();
            for f in &state.fields {
                if let Some(v) = elicitation_field_value(f) {
                    content.insert(f.key.clone(), v);
                } else if f.required {
                    self.toast(format!("\"{}\" is required", f.label), true);
                    self.modal = Some(ActiveModal::Elicitation(state));
                    return;
                }
            }
            let _ = state.pending.responder.send(CreateElicitationResponse {
                action: "accept".to_string(),
                content: Some(content),
            });
        } else {
            let _ = state.pending.responder.send(CreateElicitationResponse {
                action: action.to_string(),
                content: None,
            });
        }
        self.show_next_interaction();
    }

    // ------------------------------------------------------------------
    // Prompting
    // ------------------------------------------------------------------

    pub fn send_prompt(&mut self, text: String) {
        let (Some(client), Some(sid)) = (self.client.clone(), self.session_id.clone()) else {
            self.queued.push_back(text);
            if self.status == ConnectionStatus::Disconnected {
                self.toast("Not connected — the message waits until the agent reconnects", true);
            }
            return;
        };
        let caps = self.caps().prompt_capabilities;
        let payload = build_prompt_blocks(&text, &self.cwd, &caps);
        if !self.session_has_prompt {
            self.session_has_prompt = true;
            if self.session_title.is_none() {
                self.session_title = Some(title_from_prompt(&text));
            }
            self.store.upsert_session_record(
                &self.profile.id,
                &sid,
                &self.cwd.to_string_lossy(),
                self.session_title.as_deref().unwrap_or(""),
            );
        }
        self.push_item(TimelineItem::UserMessage {
            id: uuid::Uuid::new_v4().to_string(),
            text,
            attachments: payload.attached_labels,
            timestamp: chrono::Local::now(),
            steered: false,
        });
        self.busy = true;
        self.cancel_requested = false;
        self.turn_started = Some(Instant::now());
        self.scroll_from_bottom = 0;
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let result = client.prompt(&sid, payload.blocks).await;
            let _ = tx.send(AppMsg::PromptDone {
                session_id: sid,
                result,
            });
        });
    }

    fn steer_or_queue(&mut self, text: String) {
        let supports = self
            .init
            .as_ref()
            .is_some_and(InitializeResponse::supports_steering)
            && !self.steering_unsupported;
        let (Some(client), Some(sid)) = (self.client.clone(), self.session_id.clone()) else {
            self.queued.push_back(text);
            return;
        };
        if !supports {
            self.queued.push_back(text);
            self.toast("Queued — sends after the current turn", false);
            return;
        }
        let caps = self.caps().prompt_capabilities;
        let payload = build_prompt_blocks(&text, &self.cwd, &caps);
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let result = client.steer(&sid, payload.blocks).await;
            let _ = tx.send(AppMsg::SteerResult {
                text,
                attachments: payload.attached_labels,
                result,
            });
        });
    }

    pub fn cancel_turn(&mut self) {
        if !self.busy {
            return;
        }
        if let (Some(client), Some(sid)) = (&self.client, &self.session_id) {
            let _ = client.cancel(sid);
        }
        self.cancel_requested = true;
        self.cancel_all_interactions();
        self.toast("Interrupting…", false);
    }

    fn submit(&mut self) {
        let text = self.composer.text();
        if text.trim().is_empty() {
            return;
        }
        self.composer.record_history(&text);
        self.store.save_history(self.composer.history());
        if self.try_local_command(text.trim()) {
            self.composer.take_text();
            self.completion = None;
            return;
        }
        self.composer.take_text();
        self.completion = None;
        if self.opening || self.status == ConnectionStatus::Connecting {
            self.queued.push_back(text);
            self.toast("Queued — sends when the session is ready", false);
        } else if self.busy {
            self.steer_or_queue(text);
        } else {
            self.send_prompt(text);
        }
    }

    /// Returns `true` when the TUI handled the slash command itself.
    fn try_local_command(&mut self, text: &str) -> bool {
        let Some(rest) = text.strip_prefix('/') else {
            return false;
        };
        let (name, args) = match rest.split_once(char::is_whitespace) {
            Some((n, a)) => (n.to_lowercase(), a.trim().to_string()),
            None => (rest.to_lowercase(), String::new()),
        };
        if !LOCAL_COMMANDS.iter().any(|(n, _, _)| *n == name) || !self.tui_owns_command(&name) {
            return false;
        }

        match name.as_str() {
            "new" => self.open_session(SessionAction::New),
            "sessions" | "resume" => self.open_sessions_modal(),
            "fork" => self.fork_session(),
            "agent" => {
                let ids: Vec<(String, String, Option<String>)> = self
                    .profiles
                    .iter()
                    .map(|p| (p.id.clone(), p.id.clone(), None))
                    .collect();
                match exact_match(&ids, &args) {
                    Some(id) => self.switch_agent(&id),
                    None => {
                        self.open_agent_picker();
                        self.prefill_picker(&args);
                    }
                }
            }
            "model" => match exact_match(&self.model_entries(), &args) {
                Some(id) => self.apply_model(&id),
                None => {
                    self.open_model_picker();
                    self.prefill_picker(&args);
                }
            },
            "mode" => match exact_match(&self.mode_entries(), &args) {
                Some(id) => self.apply_mode(&id),
                None => {
                    self.open_mode_picker();
                    self.prefill_picker(&args);
                }
            },
            "thinking" => {
                let opt = self.thinking_option().cloned();
                match opt {
                    Some(o) if !args.is_empty() && o.is_boolean() => {
                        let on = matches!(args.to_lowercase().as_str(), "true" | "on" | "yes" | "1");
                        self.set_config_bool(o.id.clone(), on);
                        self.toast(format!("{} → {}", o.name, if on { "on" } else { "off" }), false);
                    }
                    Some(o) if !args.is_empty() => {
                        let entries: Vec<(String, String, Option<String>)> = o
                            .options
                            .flatten()
                            .into_iter()
                            .map(|x| (x.value, x.name, None))
                            .collect();
                        match exact_match(&entries, &args) {
                            Some(v) => {
                                let label = entries.iter().find(|e| e.0 == v).map(|e| e.1.clone()).unwrap_or(v.clone());
                                self.set_config_select(o.id.clone(), v);
                                self.toast(format!("{} → {label}", o.name), false);
                            }
                            None => {
                                self.open_thinking_picker();
                                self.prefill_picker(&args);
                            }
                        }
                    }
                    _ => self.open_thinking_picker(),
                }
            }
            "config" => self.open_config_modal(),
            "theme" => {
                let names: Vec<(String, String, Option<String>)> = Theme::all()
                    .into_iter()
                    .map(|t| (t.id.to_string(), t.name.to_string(), None))
                    .collect();
                match exact_match(&names, &args) {
                    Some(id) => self.set_theme(&id),
                    None => {
                        self.open_theme_picker();
                        self.prefill_picker(&args);
                    }
                }
            }
            "export" => self.export_markdown(),
            "copy" => self.copy_last_reply(),
            "clear" => {
                self.clear_timeline();
                self.toast("Timeline cleared (the agent keeps its context)", false);
            }
            "logs" => {
                self.modal = Some(ActiveModal::Logs(LogsModalState {
                    scroll: 0,
                    filter: String::new(),
                }))
            }
            "yolo" => self.toggle_auto_approve(),
            "keys" | "help" => self.modal = Some(ActiveModal::Help(HelpModalState { scroll: 0 })),
            "login" => self.open_auth_picker(),
            "logout" => self.logout(),
            "quit" | "exit" => self.should_quit = true,
            _ => return false,
        }
        true
    }

    pub fn fork_session(&mut self) {
        if !self.caps().supports_fork_session() {
            self.toast("This agent does not support forking sessions", true);
            return;
        }
        if let Some(id) = self.session_id.clone() {
            self.open_session(SessionAction::Fork { session_id: id });
        }
    }

    pub fn toggle_auto_approve(&mut self) {
        self.auto_approve = !self.auto_approve;
        self.toast(
            if self.auto_approve {
                "Auto-approve ON — tool permissions are granted automatically"
            } else {
                "Auto-approve OFF"
            },
            false,
        );
    }

    fn logout(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.caps().supports_logout() {
            self.toast("This agent does not support logout", true);
            return;
        }
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let _ = match client.logout().await {
                Ok(_) => tx.send(AppMsg::Info("Signed out".to_string())),
                Err(e) => tx.send(AppMsg::Error(format!("Logout failed: {}", e.message))),
            };
        });
    }

    fn set_theme(&mut self, name: &str) {
        self.theme = Theme::by_name(name);
        self.prefs.theme = Some(self.theme.id.to_string());
        self.store.save_prefs(&self.prefs);
        self.render_epoch += 1;
        self.toast(format!("Theme → {}", self.theme.name), false);
    }

    pub fn export_markdown(&mut self) {
        let mut out = String::new();
        out.push_str(&format!(
            "# {}\n\n_Agent: {} · Session: {}_\n\n",
            self.session_title.as_deref().unwrap_or("ACP session"),
            self.agent_display_name(),
            self.session_id.as_deref().unwrap_or("-")
        ));
        for item in &self.timeline {
            match item {
                TimelineItem::UserMessage { text, timestamp, .. } => {
                    out.push_str(&format!("## User · {}\n\n{text}\n\n", timestamp.format("%H:%M")));
                }
                TimelineItem::AssistantMessage { text, timestamp, .. } => {
                    out.push_str(&format!("## Assistant · {}\n\n{text}\n\n", timestamp.format("%H:%M")));
                }
                TimelineItem::AssistantThought { text, .. } => {
                    out.push_str(&format!(
                        "<details><summary>Thinking</summary>\n\n{text}\n\n</details>\n\n"
                    ));
                }
                TimelineItem::ToolCall(card) => {
                    out.push_str(&format!("- **{}** `{}` — {}\n", card.kind_badge(), card.title, card.status));
                }
                TimelineItem::Notice { title, .. } => out.push_str(&format!("> {title}\n\n")),
                TimelineItem::SystemInfo { text, .. } => out.push_str(&format!("> {text}\n\n")),
                _ => {}
            }
        }
        let name = format!(
            "acp-session-{}.md",
            self.session_id
                .as_deref()
                .map(short_id)
                .unwrap_or_else(|| "export".to_string())
        );
        let path = self.cwd.join(&name);
        match std::fs::write(&path, out) {
            Ok(()) => self.toast(format!("Exported to {name}"), false),
            Err(e) => self.toast(format!("Export failed: {e}"), true),
        }
    }

    pub fn copy_last_reply(&mut self) {
        let last = self.timeline.iter().rev().find_map(|item| match item {
            TimelineItem::AssistantMessage { text, .. } => Some(text.clone()),
            _ => None,
        });
        match last {
            Some(t) => {
                self.pending_osc52 = Some(base64::engine::general_purpose::STANDARD.encode(t));
                self.toast("Copied the last reply to the clipboard", false);
            }
            None => self.toast("No reply to copy", true),
        }
    }

    // ------------------------------------------------------------------
    // Modals
    // ------------------------------------------------------------------

    pub fn open_history_picker(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let items: Vec<PickerEntry> = self
            .composer
            .history()
            .iter()
            .rev()
            .filter(|h| seen.insert((*h).clone()))
            .take(300)
            .map(|h| PickerEntry {
                id: h.clone(),
                label: h.replace('\n', " ⏎ "),
                description: String::new(),
                badge: None,
                current: false,
            })
            .collect();
        if items.is_empty() {
            self.toast("No prompt history yet", false);
            return;
        }
        let query = self.composer.text();
        self.modal = Some(ActiveModal::Picker(PickerModalState {
            title: "Search prompt history".to_string(),
            kind: PickerKind::History,
            query: if query.contains('\n') { String::new() } else { query },
            items,
            selected: 0,
        }));
        self.completion = None;
    }

    fn prefill_picker(&mut self, query: &str) {
        if let Some(ActiveModal::Picker(p)) = self.modal.as_mut()
            && !query.trim().is_empty()
        {
            p.query = query.trim().to_string();
            p.selected = 0;
        }
    }

    pub fn open_agent_picker(&mut self) {
        let items = self
            .profiles
            .iter()
            .map(|p| PickerEntry {
                id: p.id.clone(),
                label: p.id.clone(),
                description: p.command_line_display(),
                badge: Some(if p.available { "ready" } else { "not found" }.to_string()),
                current: p.id == self.profile.id,
            })
            .collect::<Vec<_>>();
        let selected = items.iter().position(|i| i.current).unwrap_or(0);
        self.modal = Some(ActiveModal::Picker(PickerModalState {
            title: "Switch agent".to_string(),
            kind: PickerKind::Agent,
            query: String::new(),
            items,
            selected,
        }));
    }

    pub fn open_model_picker(&mut self) {
        let cur = self.current_model_id();
        let items: Vec<PickerEntry> = self
            .model_entries()
            .into_iter()
            .map(|(id, name, desc)| PickerEntry {
                current: Some(&id) == cur.as_ref(),
                description: desc.unwrap_or_default(),
                label: name,
                id,
                badge: None,
            })
            .collect();
        if items.is_empty() {
            self.toast("This agent does not expose models", true);
            return;
        }
        let selected = items.iter().position(|i| i.current).unwrap_or(0);
        self.modal = Some(ActiveModal::Picker(PickerModalState {
            title: "Select model".to_string(),
            kind: PickerKind::Model,
            query: String::new(),
            items,
            selected,
        }));
    }

    pub fn open_mode_picker(&mut self) {
        let cur = self.current_mode_id();
        let items: Vec<PickerEntry> = self
            .mode_entries()
            .into_iter()
            .map(|(id, name, desc)| PickerEntry {
                current: Some(&id) == cur.as_ref(),
                description: desc.unwrap_or_default(),
                label: name,
                id,
                badge: None,
            })
            .collect();
        if items.is_empty() {
            self.toast("This agent does not expose modes", true);
            return;
        }
        let selected = items.iter().position(|i| i.current).unwrap_or(0);
        self.modal = Some(ActiveModal::Picker(PickerModalState {
            title: "Select mode".to_string(),
            kind: PickerKind::Mode,
            query: String::new(),
            items,
            selected,
        }));
    }

    pub fn open_thinking_picker(&mut self) {
        let Some(opt) = self.thinking_option().cloned() else {
            self.toast("The current model has no thinking / reasoning setting", true);
            return;
        };
        if opt.is_boolean() {
            let next = !opt.current_bool().unwrap_or(false);
            self.set_config_bool(opt.id.clone(), next);
            self.toast(format!("{}: {}", opt.name, if next { "on" } else { "off" }), false);
            return;
        }
        self.open_config_select_picker(&opt);
    }

    fn open_config_select_picker(&mut self, opt: &SessionConfigOption) {
        let cur = opt.current_str();
        let items: Vec<PickerEntry> = opt
            .options
            .flatten()
            .into_iter()
            .map(|o| PickerEntry {
                current: o.value == cur,
                id: o.value,
                label: o.name,
                description: o.description.unwrap_or_default(),
                badge: o.group_name,
            })
            .collect();
        let selected = items.iter().position(|i| i.current).unwrap_or(0);
        self.modal = Some(ActiveModal::Picker(PickerModalState {
            title: opt.name.clone(),
            kind: PickerKind::ConfigSelect {
                config_id: opt.id.clone(),
            },
            query: String::new(),
            items,
            selected,
        }));
    }

    pub fn open_theme_picker(&mut self) {
        let items = Theme::all()
            .into_iter()
            .map(|t| PickerEntry {
                current: t.id == self.theme.id,
                id: t.id.to_string(),
                label: t.name.to_string(),
                description: if t.is_light { "light" } else { "dark" }.to_string(),
                badge: None,
            })
            .collect::<Vec<_>>();
        let selected = items.iter().position(|i| i.current).unwrap_or(0);
        self.modal = Some(ActiveModal::Picker(PickerModalState {
            title: "Select theme".to_string(),
            kind: PickerKind::Theme,
            query: String::new(),
            items,
            selected,
        }));
    }

    pub fn open_auth_picker(&mut self) {
        let methods = self
            .init
            .as_ref()
            .map(|i| i.auth_methods.clone())
            .unwrap_or_default();
        if methods.is_empty() {
            self.toast("The agent advertises no authentication methods", true);
            return;
        }
        let items = methods
            .into_iter()
            .map(|m| PickerEntry {
                badge: Some(if m.is_terminal() { "terminal" } else { "agent" }.to_string()),
                id: m.id,
                label: m.name,
                description: m.description.unwrap_or_default(),
                current: false,
            })
            .collect();
        self.modal = Some(ActiveModal::Picker(PickerModalState {
            title: "Authenticate".to_string(),
            kind: PickerKind::AuthMethod,
            query: String::new(),
            items,
            selected: 0,
        }));
    }

    fn start_auth(&mut self, method_id: &str) {
        let Some(method) = self
            .init
            .as_ref()
            .and_then(|i| i.auth_methods.iter().find(|m| m.id == method_id).cloned())
        else {
            return;
        };
        if method.is_terminal() {
            let mut args = self.profile.args.clone();
            args.extend(method.args.clone());
            let mut env = self.profile.env.clone();
            env.extend(method.env.clone());
            self.pending_external = Some(ExternalTerminalCommand {
                label: method.name,
                command: self.profile.command.clone(),
                args,
                env,
                cwd: self.cwd.clone(),
            });
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        self.toast(format!("Authenticating with {}…", method.name), false);
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let result = client.authenticate(&method.id).await.map(|_| ());
            let _ = tx.send(AppMsg::AuthDone(result));
        });
    }

    pub fn on_external_auth_done(&mut self, success: bool, detail: String) {
        self.dirty = true;
        if success {
            self.toast("Login finished", false);
            self.after_auth_success();
        } else {
            self.error(format!("Login command failed: {detail}"));
        }
    }

    fn after_auth_success(&mut self) {
        if self.client.is_none() {
            return;
        }
        self.status = ConnectionStatus::Connected;
        if self.session_id.is_none() {
            let action = self.pending_action.take().unwrap_or(SessionAction::New);
            if let Some(p) = self.auth_retry_prompt.take() {
                self.queued.push_front(p);
            }
            self.open_session(action);
        } else if let Some(p) = self.auth_retry_prompt.take()
            && !self.busy
        {
            self.send_prompt(p);
        }
    }

    pub fn open_config_modal(&mut self) {
        if self.config_options.is_empty() {
            self.toast("This agent exposes no session config options", true);
            return;
        }
        self.modal = Some(ActiveModal::Config(ConfigModalState { selected: 0 }));
    }

    pub fn open_sessions_modal(&mut self) {
        let caps = self.caps();
        if caps.supports_list_sessions()
            && let Some(client) = self.client.clone()
        {
            self.modal = Some(ActiveModal::Sessions(SessionPickerState {
                query: String::new(),
                sessions: Vec::new(),
                selected: 0,
                loading: true,
            }));
            let tx = self.msg_tx.clone();
            let cwd = self.cwd.to_string_lossy().into_owned();
            tokio::spawn(async move {
                let mut all = Vec::new();
                let mut cursor: Option<String> = None;
                for _ in 0..10 {
                    match client.list_sessions(Some(&cwd), cursor.as_deref()).await {
                        Ok(r) => {
                            all.extend(r.sessions);
                            cursor = r.next_cursor;
                            if cursor.is_none() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(AppMsg::Error(format!("session/list failed: {}", e.message)));
                            break;
                        }
                    }
                }
                all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                let _ = tx.send(AppMsg::SessionsListed(all));
            });
            return;
        }
        if !caps.load_session && !caps.supports_resume_session() {
            self.toast("This agent cannot load or resume sessions", true);
            return;
        }
        let sessions = self
            .store
            .load_session_records()
            .into_iter()
            .filter(|r| r.agent_id == self.profile.id)
            .map(|r| SessionInfo {
                session_id: r.session_id,
                cwd: r.cwd,
                additional_directories: Vec::new(),
                title: Some(r.title),
                updated_at: Some(r.updated_at),
            })
            .collect();
        self.modal = Some(ActiveModal::Sessions(SessionPickerState {
            query: String::new(),
            sessions,
            selected: 0,
            loading: false,
        }));
    }

    fn open_selected_session(&mut self, info: SessionInfo) {
        let caps = self.caps();
        if self.session_id.as_deref() == Some(info.session_id.as_str()) {
            return;
        }
        let title = info.title.clone();
        let action = if caps.load_session {
            SessionAction::Load {
                session_id: info.session_id,
                cwd: info.cwd,
            }
        } else if caps.supports_resume_session() {
            SessionAction::Resume {
                session_id: info.session_id,
                cwd: info.cwd,
            }
        } else {
            self.toast("This agent cannot load or resume sessions", true);
            return;
        };
        self.open_session(action);
        self.session_title = title;
    }

    pub fn palette_items(&self) -> Vec<PaletteItem> {
        let mut items = vec![
            pal("new", "New session", "Start a fresh conversation", Some("Ctrl+N"), "Session", PaletteAction::NewSession),
            pal("sessions", "Sessions", "Browse and resume earlier sessions", Some("Ctrl+S"), "Session", PaletteAction::OpenSessions),
            pal("fork", "Fork session", "Branch the current conversation", None, "Session", PaletteAction::ForkSession),
            pal("agent", "Switch agent", "Choose another ACP agent", Some("Ctrl+G"), "Agent", PaletteAction::OpenAgents),
            pal("model", "Select model", "Change the model", Some("Ctrl+L"), "Agent", PaletteAction::OpenModels),
            pal("mode", "Select mode", "Change the session mode", Some("Shift+Tab"), "Agent", PaletteAction::OpenModes),
            pal("thinking", "Thinking level", "Change the reasoning effort", None, "Agent", PaletteAction::OpenThinking),
            pal("config", "Session config", "All agent config options", None, "Agent", PaletteAction::OpenConfig),
            pal("yolo", "Toggle auto-approve", "Grant tool permissions automatically", None, "Agent", PaletteAction::ToggleAutoApprove),
            pal("login", "Login", "Authenticate with the agent", None, "Agent", PaletteAction::Login),
            pal("logout", "Logout", "Sign out of the agent", None, "Agent", PaletteAction::Logout),
            pal("reconnect", "Reconnect", "Restart the agent process", None, "Agent", PaletteAction::Reconnect),
            pal("theme", "Theme", "Change the color theme", None, "View", PaletteAction::OpenThemes),
            pal("sidebar", "Toggle sidebar", "Show or hide the session sidebar", Some("Ctrl+B"), "View", PaletteAction::ToggleSidebar),
            pal("thoughts", "Toggle thinking", "Expand or collapse finished thoughts", Some("Ctrl+T"), "View", PaletteAction::ToggleThoughts),
            pal("expand", "Toggle tool details", "Expand or collapse tool outputs", Some("Ctrl+O"), "View", PaletteAction::ToggleExpandTools),
            pal("logs", "Protocol log", "Show raw ACP traffic", Some("F12"), "View", PaletteAction::OpenLogs),
            pal("copy", "Copy last reply", "Copy to clipboard (OSC 52)", None, "Edit", PaletteAction::CopyLastReply),
            pal("export", "Export Markdown", "Write the conversation to a file", None, "Edit", PaletteAction::ExportMarkdown),
            pal("clear", "Clear timeline", "Clear the visible messages", None, "Edit", PaletteAction::ClearTimeline),
            pal("help", "Keyboard shortcuts", "Show all keybindings", Some("F1"), "Help", PaletteAction::OpenHelp),
            pal("quit", "Quit", "Exit ACP TUI", Some("Ctrl+D"), "App", PaletteAction::Quit),
        ];
        for c in &self.agent_commands {
            items.push(PaletteItem {
                title: format!("/{}", c.name),
                subtitle: c.description.clone(),
                shortcut: None,
                category: "Agent command".to_string(),
                action: PaletteAction::SlashPrompt(c.name.clone()),
            });
        }
        items
    }

    pub fn filtered_palette(query: &str, items: &[PaletteItem]) -> Vec<usize> {
        let keys: Vec<String> = items
            .iter()
            .map(|it| format!("{} {}", it.title, it.subtitle))
            .collect();
        rank_candidates(query, &keys)
    }

    fn run_palette_action(&mut self, action: PaletteAction) {
        match action {
            PaletteAction::NewSession => self.open_session(SessionAction::New),
            PaletteAction::OpenSessions => self.open_sessions_modal(),
            PaletteAction::ForkSession => self.fork_session(),
            PaletteAction::OpenAgents => self.open_agent_picker(),
            PaletteAction::OpenModels => self.open_model_picker(),
            PaletteAction::OpenModes => self.open_mode_picker(),
            PaletteAction::OpenThinking => self.open_thinking_picker(),
            PaletteAction::OpenConfig => self.open_config_modal(),
            PaletteAction::OpenThemes => self.open_theme_picker(),
            PaletteAction::OpenLogs => {
                self.modal = Some(ActiveModal::Logs(LogsModalState {
                    scroll: 0,
                    filter: String::new(),
                }))
            }
            PaletteAction::OpenHelp => self.modal = Some(ActiveModal::Help(HelpModalState { scroll: 0 })),
            PaletteAction::ToggleSidebar => self.toggle_sidebar(),
            PaletteAction::ToggleThoughts => self.toggle_thoughts(),
            PaletteAction::ToggleExpandTools => self.toggle_expand(),
            PaletteAction::ToggleAutoApprove => self.toggle_auto_approve(),
            PaletteAction::CopyLastReply => self.copy_last_reply(),
            PaletteAction::ExportMarkdown => self.export_markdown(),
            PaletteAction::ClearTimeline => self.clear_timeline(),
            PaletteAction::Login => self.open_auth_picker(),
            PaletteAction::Logout => self.logout(),
            PaletteAction::Reconnect => self.reconnect(),
            PaletteAction::Quit => self.should_quit = true,
            PaletteAction::SlashPrompt(name) => {
                let has_hint = self
                    .agent_commands
                    .iter()
                    .find(|c| c.name == name)
                    .is_some_and(|c| c.hint.is_some());
                if has_hint {
                    self.composer.set_text(&format!("/{name} "));
                } else {
                    self.composer.set_text(&format!("/{name}"));
                    self.submit();
                }
            }
        }
    }

    fn toggle_sidebar(&mut self) {
        self.show_sidebar = !self.show_sidebar;
        self.prefs.show_sidebar = Some(self.show_sidebar);
        self.store.save_prefs(&self.prefs);
    }

    fn toggle_thoughts(&mut self) {
        self.show_thoughts = !self.show_thoughts;
        for (item, exp) in self.timeline.iter().zip(self.item_expanded.iter_mut()) {
            if matches!(item, TimelineItem::AssistantThought { .. }) {
                *exp = None;
            }
        }
        self.prefs.show_thoughts = Some(self.show_thoughts);
        self.store.save_prefs(&self.prefs);
        self.render_epoch = self.render_epoch.wrapping_add(1);
    }

    fn toggle_expand(&mut self) {
        self.expand_tools = !self.expand_tools;
        for (item, exp) in self.timeline.iter().zip(self.item_expanded.iter_mut()) {
            if matches!(
                item,
                TimelineItem::ToolCall(_)
                    | TimelineItem::Compaction { .. }
                    | TimelineItem::SubagentMessage { .. }
            ) {
                *exp = None;
            }
        }
        self.prefs.expand_tools = Some(self.expand_tools);
        self.store.save_prefs(&self.prefs);
        self.render_epoch = self.render_epoch.wrapping_add(1);
        self.toast(
            if self.expand_tools {
                "Tool details expanded"
            } else {
                "Tool details collapsed"
            },
            false,
        );
    }

    fn apply_picker(&mut self, kind: PickerKind, id: String, label: String) {
        match kind {
            PickerKind::Agent => {
                if id != self.profile.id {
                    self.switch_agent(&id);
                }
            }
            PickerKind::Model => self.apply_model(&id),
            PickerKind::Mode => self.apply_mode(&id),
            PickerKind::ConfigSelect { config_id } => {
                let name = self
                    .config_options
                    .iter()
                    .find(|o| o.id == config_id)
                    .map(|o| o.name.clone())
                    .unwrap_or_else(|| config_id.clone());
                self.set_config_select(config_id, id);
                self.toast(format!("{name} → {label}"), false);
            }
            PickerKind::Theme => self.set_theme(&id),
            PickerKind::AuthMethod => self.start_auth(&id),
            PickerKind::History => {
                self.composer.set_text(&id);
                self.refresh_completion();
            }
        }
    }

    // ------------------------------------------------------------------
    // Input
    // ------------------------------------------------------------------

    pub fn tick(&mut self) {
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
        let before = self.toasts.len();
        self.toasts
            .retain(|t| t.created_at.elapsed() < Duration::from_millis(3500));
        if before != self.toasts.len() {
            self.dirty = true;
        }
        if self.busy || self.opening || self.status == ConnectionStatus::Connecting {
            self.dirty = true;
        }
        if self
            .quit_armed_at
            .is_some_and(|t| t.elapsed() > Duration::from_millis(1500))
        {
            self.quit_armed_at = None;
            self.dirty = true;
        }
    }

    pub fn handle_paste(&mut self, text: String) {
        self.dirty = true;
        match self.modal.as_mut() {
            None => {
                self.composer.insert_str(&text);
                self.refresh_completion();
            }
            Some(ActiveModal::CommandPalette { query, selected, .. }) => {
                query.push_str(text.trim());
                *selected = 0;
            }
            Some(ActiveModal::Picker(p)) => {
                p.query.push_str(text.trim());
                p.selected = 0;
            }
            Some(ActiveModal::Sessions(s)) => {
                s.query.push_str(text.trim());
                s.selected = 0;
            }
            Some(ActiveModal::Elicitation(e)) => {
                if let Some(f) = e.fields.get_mut(e.focused_field)
                    && matches!(f.kind, ElicitationFieldKind::Text | ElicitationFieldKind::Number { .. })
                {
                    f.text_val.push_str(&text);
                }
            }
            _ => {}
        }
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent) {
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.modal.is_some() {
                    return;
                }
                let contains = |r: Rect| {
                    ev.column >= r.x
                        && ev.column < r.x.saturating_add(r.width)
                        && ev.row >= r.y
                        && ev.row < r.y.saturating_add(r.height)
                };
                if self.timeline_hit.completion_rect.is_some_and(contains)
                    || self.timeline_hit.toast_rects.iter().copied().any(contains)
                {
                    return;
                }
                if self.timeline_hit.jump_pill_rect.is_some_and(contains) {
                    self.scroll_from_bottom = 0;
                    self.dirty = true;
                    return;
                }
                if contains(self.timeline_hit.activity_rect)
                    && let Some(idx) = self.active_step_index()
                {
                    if self.scroll_from_bottom > 0 {
                        self.scroll_from_bottom = 0;
                        self.toggle_item_expanded(idx, None);
                    } else {
                        let start = self
                            .timeline_hit
                            .spans
                            .iter()
                            .find(|s| s.item_idx == idx)
                            .map(|s| s.start_line);
                        self.toggle_item_expanded(idx, start);
                    }
                    return;
                }
                if contains(self.timeline_hit.view_rect)
                    && let Some(row_offset) = ev.row.checked_sub(self.timeline_hit.inner_y)
                {
                    let line_idx = self
                        .timeline_hit
                        .visible_start
                        .saturating_add(row_offset as usize);
                    if let Some(&span) = self
                        .timeline_hit
                        .spans
                        .iter()
                        .find(|s| line_idx >= s.start_line && line_idx < s.end_line)
                        && span.expandable
                    {
                        self.toggle_item_expanded(span.item_idx, Some(span.start_line));
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                self.dirty = true;
                match self.modal.as_mut() {
                    Some(ActiveModal::Logs(l)) => l.scroll = l.scroll.saturating_add(3),
                    Some(ActiveModal::Help(h)) => h.scroll = h.scroll.saturating_sub(3),
                    None => self.scroll_up(3),
                    _ => {}
                }
            }
            MouseEventKind::ScrollDown => {
                self.dirty = true;
                match self.modal.as_mut() {
                    Some(ActiveModal::Logs(l)) => l.scroll = l.scroll.saturating_sub(3),
                    Some(ActiveModal::Help(h)) => h.scroll = h.scroll.saturating_add(3),
                    None => self.scroll_down(3),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    pub fn scroll_up(&mut self, n: usize) {
        let max = self.last_total_lines.saturating_sub(self.last_timeline_height);
        self.scroll_from_bottom = (self.scroll_from_bottom + n).min(max);
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(n);
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.dirty = true;
        if self.modal.is_some() {
            self.handle_modal_key(key);
            return;
        }

        let ctrl = is_ctrl(&key);
        // Global shortcuts
        match key.code {
            KeyCode::Char('c') if ctrl => {
                if self.completion.is_some() {
                    self.completion = None;
                } else if !self.composer.is_empty() {
                    self.composer.clear();
                } else if self.busy {
                    self.cancel_turn();
                } else if self.quit_armed_at.is_some() {
                    self.should_quit = true;
                } else {
                    self.quit_armed_at = Some(Instant::now());
                }
                return;
            }
            KeyCode::Char('d') if ctrl => {
                if self.composer.is_empty() {
                    self.should_quit = true;
                } else {
                    self.composer.delete_forward();
                }
                return;
            }
            KeyCode::Esc => {
                if self.completion.is_some() {
                    self.completion = None;
                } else if self.busy {
                    self.cancel_turn();
                } else if self.scroll_from_bottom > 0 {
                    self.scroll_from_bottom = 0;
                } else if !self.composer.is_empty() {
                    if self
                        .esc_armed_at
                        .is_some_and(|t| t.elapsed() < Duration::from_millis(1200))
                    {
                        self.esc_armed_at = None;
                        self.composer.clear();
                        self.toast("Input cleared (ctrl+z to undo)", false);
                    } else {
                        self.esc_armed_at = Some(Instant::now());
                        self.toast("Press Esc again to clear the input", false);
                    }
                }
                return;
            }
            KeyCode::Char('r') if ctrl => {
                self.open_history_picker();
                return;
            }
            KeyCode::Char('p') if ctrl => {
                self.completion = None;
                let items = self.palette_items();
                self.modal = Some(ActiveModal::CommandPalette {
                    query: String::new(),
                    items,
                    selected: 0,
                });
                return;
            }
            KeyCode::Char('n') if ctrl => {
                self.open_session(SessionAction::New);
                return;
            }
            KeyCode::Char('s') if ctrl => {
                self.open_sessions_modal();
                return;
            }
            KeyCode::Char('l') if ctrl => {
                self.open_model_picker();
                return;
            }
            KeyCode::Char('g') if ctrl => {
                self.open_agent_picker();
                return;
            }
            KeyCode::Char('b') if ctrl => {
                self.toggle_sidebar();
                return;
            }
            KeyCode::Char('t') if ctrl => {
                self.toggle_thoughts();
                return;
            }
            KeyCode::Char('o') if ctrl => {
                self.toggle_expand();
                return;
            }
            KeyCode::BackTab => {
                self.cycle_mode();
                return;
            }
            KeyCode::F(1) => {
                self.modal = Some(ActiveModal::Help(HelpModalState { scroll: 0 }));
                return;
            }
            KeyCode::F(12) => {
                self.modal = Some(ActiveModal::Logs(LogsModalState {
                    scroll: 0,
                    filter: String::new(),
                }));
                return;
            }
            KeyCode::Char('?') if self.composer.is_empty() => {
                self.modal = Some(ActiveModal::Help(HelpModalState { scroll: 0 }));
                return;
            }
            KeyCode::PageUp => {
                self.scroll_up(self.last_timeline_height.max(2) / 2);
                return;
            }
            KeyCode::PageDown => {
                self.scroll_down(self.last_timeline_height.max(2) / 2);
                return;
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll_up(1);
                return;
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll_down(1);
                return;
            }
            KeyCode::Home if ctrl => {
                self.scroll_from_bottom = usize::MAX / 2;
                self.scroll_up(0);
                return;
            }
            KeyCode::End if ctrl => {
                self.scroll_from_bottom = 0;
                return;
            }
            _ => {}
        }

        // Completion popup navigation
        if self.completion.is_some() {
            match key.code {
                KeyCode::Up => {
                    if let Some(c) = self.completion.as_mut() {
                        c.selected = c.selected.checked_sub(1).unwrap_or(c.candidates.len().saturating_sub(1));
                    }
                    return;
                }
                KeyCode::Down => {
                    if let Some(c) = self.completion.as_mut() {
                        c.selected = (c.selected + 1) % c.candidates.len().max(1);
                    }
                    return;
                }
                KeyCode::Tab => {
                    self.accept_completion(false);
                    return;
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    self.accept_completion(true);
                    return;
                }
                _ => {}
            }
        }

        self.handle_composer_key(key);
        self.refresh_completion();
    }

    fn handle_composer_key(&mut self, key: KeyEvent) {
        let ctrl = is_ctrl(&key);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Enter => {
                if shift || alt {
                    self.composer.insert_newline();
                } else if !self.composer.try_backslash_newline() {
                    self.submit();
                }
            }
            KeyCode::Char('j') if ctrl => self.composer.insert_newline(),
            KeyCode::Char('a') if ctrl => self.composer.move_home(),
            KeyCode::Char('e') if ctrl => self.composer.move_end(),
            KeyCode::Char('u') if ctrl => self.composer.kill_to_line_start(),
            KeyCode::Char('k') if ctrl => self.composer.kill_to_line_end(),
            KeyCode::Char('w') if ctrl => self.composer.delete_word_back(),
            KeyCode::Char('b') if alt => self.composer.move_word_left(),
            KeyCode::Char('f') if alt => self.composer.move_word_right(),
            KeyCode::Char('d') if alt => self.composer.delete_word_forward(),
            KeyCode::Char('z') if ctrl => self.composer.undo(),
            KeyCode::Backspace if ctrl || alt => self.composer.delete_word_back(),
            KeyCode::Backspace => self.composer.backspace(),
            KeyCode::Delete => self.composer.delete_forward(),
            KeyCode::Left if ctrl || alt => self.composer.move_word_left(),
            KeyCode::Right if ctrl || alt => self.composer.move_word_right(),
            KeyCode::Left => self.composer.move_left(),
            KeyCode::Right => self.composer.move_right(),
            KeyCode::Home => self.composer.move_home(),
            KeyCode::End => self.composer.move_end(),
            KeyCode::Up => {
                if self.composer.is_empty() && !self.queued.is_empty() {
                    if let Some(last) = self.queued.pop_back() {
                        self.composer.set_text(&last);
                        self.toast("Editing the queued message", false);
                    }
                } else {
                    self.composer.move_up_or_history();
                }
            }
            KeyCode::Down => self.composer.move_down_or_history(),
            KeyCode::Tab => {}
            KeyCode::Char(c) if !ctrl && !alt => self.composer.insert_char(c),
            _ => {}
        }
    }

    /// Whether the TUI answers `/name` itself instead of passing it to the agent.
    fn tui_owns_command(&self, name: &str) -> bool {
        let agent_has = self
            .agent_commands
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(name));
        match name {
            "model" => !self.model_entries().is_empty() || !agent_has,
            "mode" => !self.mode_entries().is_empty() || !agent_has,
            "thinking" => self.thinking_option().is_some() || !agent_has,
            _ => LOCAL_COMMANDS.iter().any(|(n, _, _)| *n == name) && !agent_has,
        }
    }

    pub fn all_slash_defs(&self) -> Vec<SlashCommandDef> {
        let mut out: Vec<SlashCommandDef> = self
            .agent_commands
            .iter()
            .filter(|c| !self.tui_owns_command(&c.name.to_lowercase()))
            .cloned()
            .collect();
        for (name, desc, hint) in LOCAL_COMMANDS {
            if self.tui_owns_command(name) {
                out.push(SlashCommandDef {
                    name: (*name).to_string(),
                    description: (*desc).to_string(),
                    hint: hint.map(String::from),
                    is_builtin_ui: true,
                });
            }
        }
        out
    }

    /// Values for the argument of a TUI command, for `/model <tab>` style completion.
    fn command_arg_candidates(&self, cmd: &str) -> Option<Vec<CompletionCandidate>> {
        if !self.tui_owns_command(cmd) {
            return None;
        }
        let from_entries = |entries: Vec<(String, String, Option<String>)>, current: Option<String>| {
            entries
                .into_iter()
                .map(|(id, name, desc)| CompletionCandidate {
                    tag: (Some(&id) == current.as_ref()).then(|| "current".to_string()),
                    detail: {
                        let d = desc.unwrap_or_default();
                        let d = d.strip_prefix(name.as_str()).unwrap_or(&d).trim().to_string();
                        if !d.is_empty() {
                            d
                        } else if id != name {
                            id.clone()
                        } else {
                            String::new()
                        }
                    },
                    label: name,
                    value: id,
                    has_args: false,
                })
                .collect::<Vec<_>>()
        };
        let list = match cmd {
            "model" => from_entries(self.model_entries(), self.current_model_id()),
            "mode" => from_entries(self.mode_entries(), self.current_mode_id()),
            "thinking" => {
                let opt = self.thinking_option()?;
                let entries = if opt.is_boolean() {
                    vec![
                        ("true".to_string(), "on".to_string(), None),
                        ("false".to_string(), "off".to_string(), None),
                    ]
                } else {
                    opt.options
                        .flatten()
                        .into_iter()
                        .map(|o| (o.value, o.name, o.description))
                        .collect()
                };
                from_entries(entries, Some(opt.current_str()))
            }
            "agent" => self
                .profiles
                .iter()
                .map(|p| CompletionCandidate {
                    value: p.id.clone(),
                    label: p.id.clone(),
                    detail: p.command_line_display(),
                    has_args: false,
                    tag: if p.id == self.profile.id {
                        Some("current".to_string())
                    } else if !p.available {
                        Some("not found".to_string())
                    } else {
                        None
                    },
                })
                .collect(),
            "theme" => Theme::all()
                .into_iter()
                .map(|t| CompletionCandidate {
                    value: t.id.to_string(),
                    label: t.name.to_string(),
                    detail: t.id.to_string(),
                    has_args: false,
                    tag: (t.id == self.theme.id).then(|| "current".to_string()),
                })
                .collect(),
            _ => return None,
        };
        Some(list)
    }

    pub fn refresh_completion(&mut self) {
        let prev_selected = self.completion.as_ref().map(|c| c.selected).unwrap_or(0);
        if let Some(q) = self.composer.active_slash_query() {
            let defs = self.all_slash_defs();
            let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
            let candidates: Vec<CompletionCandidate> = rank_candidates(&q, &names)
                .into_iter()
                .map(|i| {
                    let d = &defs[i];
                    CompletionCandidate {
                        label: format!(
                            "/{}{}",
                            d.name,
                            d.hint.as_ref().map(|h| format!(" {h}")).unwrap_or_default()
                        ),
                        detail: d.description.clone(),
                        has_args: d.hint.is_some(),
                        value: d.name.clone(),
                        tag: Some(if d.is_builtin_ui { "tui" } else { "agent" }.to_string()),
                    }
                })
                .collect();
            self.set_completion(CompletionPopupKind::SlashCommand, candidates, prev_selected);
            return;
        }
        if let Some((cmd, arg)) = self.composer.active_command_arg()
            && let Some(all) = self.command_arg_candidates(&cmd)
        {
            let keys: Vec<String> = all.iter().map(|c| format!("{} {}", c.label, c.value)).collect();
            let candidates: Vec<CompletionCandidate> = rank_candidates(&arg, &keys)
                .into_iter()
                .map(|i| all[i].clone())
                .collect();
            let exact = candidates.len() == 1 && candidates[0].value == arg.trim();
            if exact {
                self.completion = None;
            } else {
                let selected = if self.completion.as_ref().is_some_and(|c| matches!(c.kind, CompletionPopupKind::CommandArg)) {
                    prev_selected
                } else {
                    candidates.iter().position(|c| c.tag.as_deref() == Some("current")).unwrap_or(0)
                };
                self.set_completion(CompletionPopupKind::CommandArg, candidates, selected);
            }
            return;
        }
        if let Some((_, q)) = self.composer.active_at_mention() {
            if self.file_index.is_none() {
                if !self.file_index_loading {
                    self.file_index_loading = true;
                    let tx = self.msg_tx.clone();
                    let cwd = self.cwd.clone();
                    tokio::task::spawn_blocking(move || {
                        let files = scan_workspace_files(&cwd, 30_000);
                        let _ = tx.send(AppMsg::FileIndex(files));
                    });
                }
                self.completion = None;
                return;
            }
            let files = self.file_index.as_deref().unwrap_or(&[]);
            let mut scored: Vec<(i64, &String)> = files
                .iter()
                .filter_map(|f| fuzzy_score(&q, f).map(|s| (s, f)))
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
            let candidates: Vec<CompletionCandidate> = scored
                .into_iter()
                .take(50)
                .map(|(_, f)| {
                    let (dir, base) = match f.rsplit_once('/') {
                        Some((d, b)) => (d.to_string(), b.to_string()),
                        None => (String::new(), f.clone()),
                    };
                    CompletionCandidate {
                        value: f.clone(),
                        label: base,
                        detail: dir,
                        has_args: false,
                        tag: None,
                    }
                })
                .collect();
            self.set_completion(CompletionPopupKind::FileMention, candidates, prev_selected);
            return;
        }
        self.completion = None;
    }

    fn set_completion(&mut self, kind: CompletionPopupKind, candidates: Vec<CompletionCandidate>, selected: usize) {
        self.completion = (!candidates.is_empty()).then(|| CompletionPopup {
            kind,
            selected: selected.min(candidates.len().saturating_sub(1)),
            candidates,
        });
    }

    fn accept_completion(&mut self, submit_if_complete: bool) {
        let Some(popup) = self.completion.take() else {
            return;
        };
        let Some(c) = popup.candidates.get(popup.selected).cloned() else {
            return;
        };
        match popup.kind {
            CompletionPopupKind::SlashCommand => {
                self.composer.complete_slash_command(&c.value, c.has_args || !submit_if_complete);
                if submit_if_complete && !c.has_args {
                    self.submit();
                } else {
                    self.refresh_completion();
                }
            }
            CompletionPopupKind::CommandArg => {
                self.composer.complete_command_arg(&c.value);
                if submit_if_complete {
                    self.submit();
                }
            }
            CompletionPopupKind::FileMention => self.composer.complete_at_mention(&c.value),
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) {
        let ctrl = is_ctrl(&key);
        let Some(modal) = self.modal.take() else {
            return;
        };
        match modal {
            ActiveModal::CommandPalette {
                mut query,
                items,
                mut selected,
            } => {
                let filtered = Self::filtered_palette(&query, &items);
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Char('c') if ctrl => return,
                    KeyCode::Char('p') if ctrl => return,
                    KeyCode::Enter => {
                        if let Some(&idx) = filtered.get(selected) {
                            let action = items[idx].action.clone();
                            self.run_palette_action(action);
                        }
                        return;
                    }
                    KeyCode::Up => selected = selected.checked_sub(1).unwrap_or(filtered.len().saturating_sub(1)),
                    KeyCode::Down => selected = (selected + 1) % filtered.len().max(1),
                    KeyCode::Backspace => {
                        query.pop();
                        selected = 0;
                    }
                    KeyCode::Char(c) if !ctrl => {
                        query.push(c);
                        selected = 0;
                    }
                    _ => {}
                }
                self.modal = Some(ActiveModal::CommandPalette {
                    query,
                    items,
                    selected,
                });
            }
            ActiveModal::Picker(mut p) => {
                let filtered = p.filtered_indices();
                match key.code {
                    KeyCode::Esc => {
                        if matches!(p.kind, PickerKind::AuthMethod) {
                            self.info("Authentication skipped. Use /login to authenticate later.");
                        }
                        return;
                    }
                    KeyCode::Char('c') if ctrl => return,
                    KeyCode::Enter => {
                        if let Some(&idx) = filtered.get(p.selected) {
                            let id = p.items[idx].id.clone();
                            let label = p.items[idx].label.clone();
                            self.apply_picker(p.kind, id, label);
                        }
                        return;
                    }
                    KeyCode::Up => p.selected = p.selected.checked_sub(1).unwrap_or(filtered.len().saturating_sub(1)),
                    KeyCode::Down => p.selected = (p.selected + 1) % filtered.len().max(1),
                    KeyCode::Backspace => {
                        p.query.pop();
                        p.selected = 0;
                    }
                    KeyCode::Char(c) if !ctrl => {
                        p.query.push(c);
                        p.selected = 0;
                    }
                    _ => {}
                }
                self.modal = Some(ActiveModal::Picker(p));
            }
            ActiveModal::Sessions(mut s) => {
                let filtered = s.filtered_indices();
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Char('c') if ctrl => return,
                    KeyCode::Enter => {
                        if let Some(&idx) = filtered.get(s.selected) {
                            let info = s.sessions[idx].clone();
                            self.open_selected_session(info);
                        }
                        return;
                    }
                    KeyCode::Char('d') if ctrl => {
                        if let Some(&idx) = filtered.get(s.selected) {
                            let id = s.sessions[idx].session_id.clone();
                            if self.caps().supports_delete_session() {
                                if let Some(client) = self.client.clone() {
                                    let tx = self.msg_tx.clone();
                                    tokio::spawn(async move {
                                        let _ = match client.delete_session(&id).await {
                                            Ok(_) => tx.send(AppMsg::SessionDeleted(id)),
                                            Err(e) => tx.send(AppMsg::Error(format!(
                                                "Delete failed: {}",
                                                e.message
                                            ))),
                                        };
                                    });
                                }
                            } else {
                                self.store.remove_session_record(&self.profile.id, &id);
                                s.sessions.remove(idx);
                                s.selected = s.selected.min(s.sessions.len().saturating_sub(1));
                                self.toast("Removed from the local list", false);
                            }
                        }
                    }
                    KeyCode::Up => s.selected = s.selected.checked_sub(1).unwrap_or(filtered.len().saturating_sub(1)),
                    KeyCode::Down => s.selected = (s.selected + 1) % filtered.len().max(1),
                    KeyCode::Backspace => {
                        s.query.pop();
                        s.selected = 0;
                    }
                    KeyCode::Char(c) if !ctrl => {
                        s.query.push(c);
                        s.selected = 0;
                    }
                    _ => {}
                }
                self.modal = Some(ActiveModal::Sessions(s));
            }
            ActiveModal::Config(mut c) => {
                let n = self.config_options.len();
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Char('c') if ctrl => return,
                    KeyCode::Up => c.selected = c.selected.checked_sub(1).unwrap_or(n.saturating_sub(1)),
                    KeyCode::Down => c.selected = (c.selected + 1) % n.max(1),
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if let Some(opt) = self.config_options.get(c.selected).cloned() {
                            if opt.is_boolean() {
                                let next = !opt.current_bool().unwrap_or(false);
                                self.set_config_bool(opt.id, next);
                            } else {
                                self.open_config_select_picker(&opt);
                                return;
                            }
                        }
                    }
                    _ => {}
                }
                self.modal = Some(ActiveModal::Config(c));
            }
            ActiveModal::Permission(mut p) => {
                let n = p.pending.request.options.len();
                match key.code {
                    KeyCode::Up | KeyCode::Left | KeyCode::BackTab => {
                        p.selected = p.selected.checked_sub(1).unwrap_or(n.saturating_sub(1))
                    }
                    KeyCode::Down | KeyCode::Right | KeyCode::Tab => p.selected = (p.selected + 1) % n.max(1),
                    KeyCode::PageDown => p.diff_scroll = p.diff_scroll.saturating_add(5),
                    KeyCode::PageUp => p.diff_scroll = p.diff_scroll.saturating_sub(5),
                    KeyCode::Enter => {
                        let id = p.pending.request.options.get(p.selected).map(|o| o.option_id.clone());
                        self.modal = Some(ActiveModal::Permission(p));
                        self.respond_permission(id);
                        return;
                    }
                    KeyCode::Char(d) if d.is_ascii_digit() && d != '0' => {
                        let i = (d as usize) - ('1' as usize);
                        if let Some(o) = p.pending.request.options.get(i) {
                            let id = Some(o.option_id.clone());
                            self.modal = Some(ActiveModal::Permission(p));
                            self.respond_permission(id);
                            return;
                        }
                    }
                    KeyCode::Esc => {
                        let reject = p
                            .pending
                            .request
                            .options
                            .iter()
                            .find(|o| o.kind == "reject_once")
                            .map(|o| o.option_id.clone());
                        self.modal = Some(ActiveModal::Permission(p));
                        self.respond_permission(reject);
                        return;
                    }
                    KeyCode::Char('c') if ctrl => {
                        self.modal = Some(ActiveModal::Permission(p));
                        self.cancel_turn();
                        return;
                    }
                    KeyCode::Char('o') if ctrl => self.toggle_expand(),
                    KeyCode::Char(c @ ('y' | 'a' | 'n')) if !ctrl => {
                        let kind = match c {
                            'y' => "allow_once",
                            'a' => "allow_always",
                            _ => "reject_once",
                        };
                        let id = p
                            .pending
                            .request
                            .options
                            .iter()
                            .find(|o| o.kind == kind)
                            .map(|o| o.option_id.clone());
                        if id.is_some() {
                            self.modal = Some(ActiveModal::Permission(p));
                            self.respond_permission(id);
                            return;
                        }
                    }
                    _ => {}
                }
                self.modal = Some(ActiveModal::Permission(p));
            }
            ActiveModal::Elicitation(mut e) => {
                if e.is_url_mode {
                    match key.code {
                        KeyCode::Enter => {
                            if let Some(url) = e.pending.request.url.clone() {
                                let _ = open::that(url);
                            }
                            self.modal = Some(ActiveModal::Elicitation(e));
                            self.respond_elicitation("accept");
                        }
                        KeyCode::Esc => {
                            self.modal = Some(ActiveModal::Elicitation(e));
                            self.respond_elicitation("decline");
                        }
                        KeyCode::Char('c') if ctrl => {
                            self.modal = Some(ActiveModal::Elicitation(e));
                            self.respond_elicitation("cancel");
                        }
                        _ => self.modal = Some(ActiveModal::Elicitation(e)),
                    }
                    return;
                }
                let nfields = e.fields.len();
                match key.code {
                    KeyCode::Esc => {
                        self.modal = Some(ActiveModal::Elicitation(e));
                        self.respond_elicitation("decline");
                        return;
                    }
                    KeyCode::Char('c') if ctrl => {
                        self.modal = Some(ActiveModal::Elicitation(e));
                        self.respond_elicitation("cancel");
                        return;
                    }
                    KeyCode::Enter => {
                        self.modal = Some(ActiveModal::Elicitation(e));
                        self.respond_elicitation("accept");
                        return;
                    }
                    KeyCode::Tab | KeyCode::Down => {
                        let multi_handled = e.fields.get_mut(e.focused_field).is_some_and(|f| {
                            if let ElicitationFieldKind::MultiSelect { options, cursor, .. } = &mut f.kind
                                && key.code == KeyCode::Down
                                && *cursor + 1 < options.len()
                            {
                                *cursor += 1;
                                return true;
                            }
                            false
                        });
                        if !multi_handled {
                            e.focused_field = (e.focused_field + 1) % nfields.max(1);
                        }
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        let multi_handled = e.fields.get_mut(e.focused_field).is_some_and(|f| {
                            if let ElicitationFieldKind::MultiSelect { cursor, .. } = &mut f.kind
                                && key.code == KeyCode::Up
                                && *cursor > 0
                            {
                                *cursor -= 1;
                                return true;
                            }
                            false
                        });
                        if !multi_handled {
                            e.focused_field = e.focused_field.checked_sub(1).unwrap_or(nfields.saturating_sub(1));
                        }
                    }
                    _ => {
                        if let Some(f) = e.fields.get_mut(e.focused_field) {
                            edit_elicitation_field(f, key);
                        }
                    }
                }
                self.modal = Some(ActiveModal::Elicitation(e));
            }
            ActiveModal::Logs(mut l) => {
                match key.code {
                    KeyCode::Esc | KeyCode::F(12) => return,
                    KeyCode::Char('c') if ctrl => return,
                    KeyCode::Up => l.scroll = l.scroll.saturating_add(1),
                    KeyCode::Down => l.scroll = l.scroll.saturating_sub(1),
                    KeyCode::PageUp => l.scroll = l.scroll.saturating_add(15),
                    KeyCode::PageDown => l.scroll = l.scroll.saturating_sub(15),
                    KeyCode::End => l.scroll = 0,
                    KeyCode::Backspace => {
                        l.filter.pop();
                    }
                    KeyCode::Char(c) if !ctrl => l.filter.push(c),
                    _ => {}
                }
                self.modal = Some(ActiveModal::Logs(l));
            }
            ActiveModal::Help(mut h) => {
                match key.code {
                    KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('q') | KeyCode::Char('?') => return,
                    KeyCode::Char('c') if ctrl => return,
                    KeyCode::Up => h.scroll = h.scroll.saturating_sub(1),
                    KeyCode::Down => h.scroll = h.scroll.saturating_add(1),
                    KeyCode::PageUp => h.scroll = h.scroll.saturating_sub(10),
                    KeyCode::PageDown => h.scroll = h.scroll.saturating_add(10),
                    _ => {}
                }
                self.modal = Some(ActiveModal::Help(h));
            }
        }
    }

    pub fn modified_files(&self) -> Vec<ModifiedFileSummary> {
        self.file_changes
            .iter()
            .map(|(path, changes)| {
                let (added, removed) = changes
                    .values()
                    .fold((0, 0), |acc, (a, r)| (acc.0 + a, acc.1 + r));
                ModifiedFileSummary {
                    path: self.rel_path(path),
                    added,
                    removed,
                }
            })
            .collect()
    }
}

fn pal(
    id: &str,
    title: &str,
    subtitle: &str,
    shortcut: Option<&str>,
    category: &str,
    action: PaletteAction,
) -> PaletteItem {
    let _ = id;
    PaletteItem {
        title: title.to_string(),
        subtitle: subtitle.to_string(),
        shortcut: shortcut.map(String::from),
        category: category.to_string(),
        action,
    }
}

pub fn short_id(id: &str) -> String {
    let clean: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    clean.chars().take(8).collect()
}

fn title_from_prompt(text: &str) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut t: String = first.chars().take(60).collect();
    if first.chars().count() > 60 {
        t.push('…');
    }
    t
}

/// The entry whose id or name equals `query` (case-insensitive), or the only entry that contains it.
fn exact_match(entries: &[(String, String, Option<String>)], query: &str) -> Option<String> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    if let Some((id, _, _)) = entries
        .iter()
        .find(|(id, name, _)| id.eq_ignore_ascii_case(q) || name.eq_ignore_ascii_case(q))
    {
        return Some(id.clone());
    }
    let ql = q.to_lowercase();
    let mut hits = entries
        .iter()
        .filter(|(id, name, _)| id.to_lowercase().contains(&ql) || name.to_lowercase().contains(&ql));
    match (hits.next(), hits.next()) {
        (Some((id, _, _)), None) => Some(id.clone()),
        _ => None,
    }
}

fn subagent_state_label(state: &Value) -> (String, bool) {
    let s = state.get("state").and_then(|s| s.as_str()).unwrap_or("unknown");
    match s {
        "running" => ("running".to_string(), true),
        "idle" => {
            let reason = state
                .get("stopReason")
                .and_then(|r| r.as_str())
                .unwrap_or("end_turn");
            let label = match reason {
                "end_turn" => "done",
                "cancelled" => "cancelled",
                "error" => "failed",
                other => other,
            };
            (label.to_string(), false)
        }
        "requires_action" => ("needs input".to_string(), true),
        other => (other.to_string(), false),
    }
}

fn enum_options_from(schema: &Value) -> Option<Vec<(String, String)>> {
    if let Some(one_of) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(|v| v.as_array())
    {
        let opts: Vec<(String, String)> = one_of
            .iter()
            .filter_map(|o| {
                let v = o.get("const").and_then(|c| c.as_str())?;
                let t = o.get("title").and_then(|t| t.as_str()).unwrap_or(v);
                Some((v.to_string(), t.to_string()))
            })
            .collect();
        if !opts.is_empty() {
            return Some(opts);
        }
    }
    schema.get("enum").and_then(|e| e.as_array()).map(|arr| {
        let names = schema.get("enumNames").and_then(|n| n.as_array());
        arr.iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let val = v.as_str()?;
                let label = names
                    .and_then(|n| n.get(i))
                    .and_then(|n| n.as_str())
                    .unwrap_or(val);
                Some((val.to_string(), label.to_string()))
            })
            .collect()
    })
}

pub fn build_elicitation_fields(schema: Option<&ElicitationSchema>) -> Vec<ElicitationField> {
    let Some(schema) = schema else {
        return Vec::new();
    };
    let mut keys: Vec<&String> = schema.properties.keys().collect();
    keys.sort_by_key(|k| {
        (
            schema.required.iter().position(|r| r == *k).unwrap_or(usize::MAX),
            (*k).clone(),
        )
    });
    keys.into_iter()
        .map(|key| {
            let prop = &schema.properties[key];
            let ty = prop.get("type").and_then(|t| t.as_str()).unwrap_or("string");
            let kind = match ty {
                "boolean" => ElicitationFieldKind::Boolean,
                "number" => ElicitationFieldKind::Number { integer: false },
                "integer" => ElicitationFieldKind::Number { integer: true },
                "array" => {
                    let options = prop
                        .get("items")
                        .and_then(enum_options_from)
                        .unwrap_or_default();
                    let checked = vec![false; options.len()];
                    ElicitationFieldKind::MultiSelect {
                        options,
                        checked,
                        cursor: 0,
                    }
                }
                _ => match enum_options_from(prop) {
                    Some(options) if !options.is_empty() => ElicitationFieldKind::SingleSelect { options },
                    _ => ElicitationFieldKind::Text,
                },
            };
            let default = prop.get("default");
            let text_val = default
                .map(|d| match d {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            let bool_val = default.and_then(|d| d.as_bool()).unwrap_or(false);
            let select_idx = match (&kind, default.and_then(|d| d.as_str())) {
                (ElicitationFieldKind::SingleSelect { options }, Some(d)) => {
                    options.iter().position(|(v, _)| v == d).unwrap_or(0)
                }
                _ => 0,
            };
            ElicitationField {
                key: key.clone(),
                label: prop
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or(key)
                    .to_string(),
                description: prop
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(String::from),
                required: schema.required.contains(key),
                kind,
                text_val,
                bool_val,
                select_idx,
            }
        })
        .collect()
}

fn edit_elicitation_field(f: &mut ElicitationField, key: KeyEvent) {
    match &mut f.kind {
        ElicitationFieldKind::Text | ElicitationFieldKind::Number { .. } => match key.code {
            KeyCode::Backspace => {
                f.text_val.pop();
            }
            KeyCode::Char('u') if is_ctrl(&key) => f.text_val.clear(),
            KeyCode::Char(c) if !is_ctrl(&key) => f.text_val.push(c),
            _ => {}
        },
        ElicitationFieldKind::Boolean => {
            if matches!(key.code, KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right) {
                f.bool_val = !f.bool_val;
            }
        }
        ElicitationFieldKind::SingleSelect { options } => {
            let n = options.len().max(1);
            match key.code {
                KeyCode::Left => f.select_idx = f.select_idx.checked_sub(1).unwrap_or(n - 1),
                KeyCode::Right | KeyCode::Char(' ') => f.select_idx = (f.select_idx + 1) % n,
                _ => {}
            }
        }
        ElicitationFieldKind::MultiSelect { checked, cursor, .. } => {
            if key.code == KeyCode::Char(' ')
                && let Some(c) = checked.get_mut(*cursor)
            {
                *c = !*c;
            }
        }
    }
}

fn elicitation_field_value(f: &ElicitationField) -> Option<Value> {
    match &f.kind {
        ElicitationFieldKind::Text => {
            (!f.text_val.is_empty()).then(|| Value::String(f.text_val.clone()))
        }
        ElicitationFieldKind::Number { integer } => {
            let t = f.text_val.trim();
            if t.is_empty() {
                return None;
            }
            if *integer {
                t.parse::<i64>().ok().map(Value::from)
            } else {
                t.parse::<f64>().ok().map(Value::from)
            }
        }
        ElicitationFieldKind::Boolean => Some(Value::Bool(f.bool_val)),
        ElicitationFieldKind::SingleSelect { options } => options
            .get(f.select_idx)
            .map(|(v, _)| Value::String(v.clone())),
        ElicitationFieldKind::MultiSelect { options, checked, .. } => {
            let vals: Vec<Value> = options
                .iter()
                .zip(checked)
                .filter(|(_, c)| **c)
                .map(|((v, _), _)| Value::String(v.clone()))
                .collect();
            (!vals.is_empty() || !f.required).then_some(Value::Array(vals))
        }
    }
}
