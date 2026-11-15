use crate::acp::fs_handler::{FileWriteRecord, handle_read_text_file, handle_write_text_file};
use crate::acp::terminal_mgr::{TerminalManager, TerminalSnapshot};
use crate::acp::types::*;
use anyhow::{Context, Result, anyhow};
use chrono::Local;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Debug, Clone)]
pub enum RpcDirection {
    Outgoing,
    Incoming,
    Stderr,
}

#[derive(Debug, Clone)]
pub struct RpcLogEntry {
    pub timestamp: String,
    pub direction: RpcDirection,
    pub summary: String,
    pub payload: String,
}

#[derive(Debug)]
pub struct PendingPermission {
    pub request: RequestPermissionRequest,
    pub responder: oneshot::Sender<RequestPermissionResponse>,
}

#[derive(Debug)]
pub struct PendingElicitation {
    pub request: CreateElicitationRequest,
    pub responder: oneshot::Sender<CreateElicitationResponse>,
}

#[derive(Debug)]
pub enum AcpEvent {
    SessionUpdate(SessionNotification),
    PermissionRequest(PendingPermission),
    ElicitationRequest(PendingElicitation),
    ElicitationComplete { elicitation_id: String },
    TerminalUpdated(TerminalSnapshot),
    FileWritten(FileWriteRecord),
    ExtNotification { method: String, params: Value },
    RpcLog(RpcLogEntry),
    StderrLine(String),
    Disconnected { instance: u64, reason: String },
}

#[derive(Debug, Clone)]
pub struct AgentSpawnSpec {
    /// Connection generation, echoed in `Disconnected` so stale connections can be ignored.
    pub instance: u64,
    pub label: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: PathBuf,
}

#[derive(Clone)]
pub struct AcpClientHandle {
    inner: Arc<AcpClientInner>,
}

struct AcpClientInner {
    spec: AgentSpawnSpec,
    stdin_tx: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<RequestId, oneshot::Sender<Result<Value, RpcError>>>>,
    next_id: AtomicI64,
    terminal_mgr: TerminalManager,
    child: Mutex<Option<Child>>,
    event_tx: mpsc::UnboundedSender<AcpEvent>,
}

fn now_hms() -> String {
    Local::now().format("%H:%M:%S%.3f").to_string()
}

fn truncate_log_payload(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars).collect();
        format!("{head}… ({} bytes)", s.len())
    }
}

impl AcpClientHandle {
    pub async fn spawn(
        spec: AgentSpawnSpec,
        event_tx: mpsc::UnboundedSender<AcpEvent>,
    ) -> Result<Self> {
        let mut cmd = Command::new(&spec.command);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "Failed to launch ACP agent '{}' ({} {})",
                spec.label,
                spec.command,
                spec.args.join(" ")
            )
        })?;

        let mut child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to capture agent stdin"))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to capture agent stdout"))?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Failed to capture agent stderr"))?;

        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
        let (term_snap_tx, mut term_snap_rx) = mpsc::unbounded_channel::<TerminalSnapshot>();

        let event_tx_term = event_tx.clone();
        tokio::spawn(async move {
            while let Some(snap) = term_snap_rx.recv().await {
                let _ = event_tx_term.send(AcpEvent::TerminalUpdated(snap));
            }
        });

        let terminal_mgr = TerminalManager::new(spec.cwd.clone(), term_snap_tx);

        let inner = Arc::new(AcpClientInner {
            spec: spec.clone(),
            stdin_tx,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            terminal_mgr,
            child: Mutex::new(Some(child)),
            event_tx: event_tx.clone(),
        });

        // Writer task (NDJSON to agent stdin)
        tokio::spawn(async move {
            while let Some(mut line) = stdin_rx.recv().await {
                if !line.ends_with('\n') {
                    line.push('\n');
                }
                if child_stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if child_stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        // Stderr reader task
        let event_tx_err = event_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(child_stderr);
            let mut buf = Vec::with_capacity(4096);
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                        if !line.is_empty() {
                            let _ = event_tx_err.send(AcpEvent::RpcLog(RpcLogEntry {
                                timestamp: now_hms(),
                                direction: RpcDirection::Stderr,
                                summary: "stderr".to_string(),
                                payload: truncate_log_payload(&line, 600),
                            }));
                            let _ = event_tx_err.send(AcpEvent::StderrLine(line));
                        }
                    }
                }
            }
        });

        // Stdout NDJSON reader & dispatcher task
        let inner_reader = inner.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::with_capacity(64 * 1024, child_stdout);
            let mut buf = Vec::with_capacity(64 * 1024);
            let disconnect_reason = loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => break "Agent closed stdout".to_string(),
                    Err(e) => break format!("Agent stdout read error: {e}"),
                    Ok(_) => {
                        let raw_line = String::from_utf8_lossy(&buf);
                        let trimmed = raw_line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(trimmed) {
                            Ok(Value::Array(batch)) => {
                                for item in batch {
                                    Self::dispatch_incoming(inner_reader.clone(), item).await;
                                }
                            }
                            Ok(msg) => {
                                Self::dispatch_incoming(inner_reader.clone(), msg).await;
                            }
                            Err(_) => {
                                let _ = inner_reader.event_tx.send(AcpEvent::RpcLog(RpcLogEntry {
                                    timestamp: now_hms(),
                                    direction: RpcDirection::Stderr,
                                    summary: "non-json stdout".to_string(),
                                    payload: truncate_log_payload(trimmed, 400),
                                }));
                            }
                        }
                    }
                }
            };

            // Fail any in-flight requests cleanly
            let pending_map = {
                let mut guard = inner_reader.pending.lock().await;
                std::mem::take(&mut *guard)
            };
            for (_, tx) in pending_map {
                let _ = tx.send(Err(RpcError::internal_error(disconnect_reason.clone())));
            }
            let _ = inner_reader.event_tx.send(AcpEvent::Disconnected {
                instance: inner_reader.spec.instance,
                reason: disconnect_reason,
            });
        });

        Ok(Self { inner })
    }

    async fn dispatch_incoming(inner: Arc<AcpClientInner>, msg: Value) {
        let has_id = msg.get("id").is_some();
        let method = msg.get("method").and_then(|m| m.as_str()).map(String::from);

        match (has_id, method) {
            // 1. Response to an outgoing client->agent request
            (true, None) => {
                let id = msg
                    .get("id")
                    .cloned()
                    .and_then(|v| serde_json::from_value::<RequestId>(v).ok())
                    .unwrap_or(RequestId::Null);

                let summary = if msg.get("error").is_some() {
                    format!("resp #{id} (error)")
                } else {
                    format!("resp #{id} (ok)")
                };
                let _ = inner.event_tx.send(AcpEvent::RpcLog(RpcLogEntry {
                    timestamp: now_hms(),
                    direction: RpcDirection::Incoming,
                    summary,
                    payload: truncate_log_payload(&msg.to_string(), 600),
                }));

                let waiter = inner.pending.lock().await.remove(&id);
                if let Some(tx) = waiter {
                    if let Some(err_val) = msg.get("error").cloned() {
                        let err = serde_json::from_value::<RpcError>(err_val).unwrap_or_else(|_| {
                            RpcError::internal_error("Malformed JSON-RPC error response")
                        });
                        let _ = tx.send(Err(err));
                    } else {
                        let result = msg.get("result").cloned().unwrap_or(Value::Null);
                        let _ = tx.send(Ok(result));
                    }
                }
            }

            // 2. Notification from agent->client (no id, has method)
            (false, Some(method)) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let sub = params
                    .get("update")
                    .and_then(|u| u.get("sessionUpdate"))
                    .and_then(|s| s.as_str())
                    .map(|s| format!("{method} ({s})"))
                    .unwrap_or_else(|| method.clone());

                let _ = inner.event_tx.send(AcpEvent::RpcLog(RpcLogEntry {
                    timestamp: now_hms(),
                    direction: RpcDirection::Incoming,
                    summary: sub,
                    payload: truncate_log_payload(&params.to_string(), 600),
                }));

                match method.as_str() {
                    "session/update" => {
                        if let Ok(raw) = serde_json::from_value::<RawSessionNotification>(params) {
                            let _ = inner
                                .event_tx
                                .send(AcpEvent::SessionUpdate(raw.into_typed()));
                        }
                    }
                    "elicitation/complete" => {
                        let elicitation_id = params
                            .get("elicitationId")
                            .and_then(|e| e.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let _ = inner
                            .event_tx
                            .send(AcpEvent::ElicitationComplete { elicitation_id });
                    }
                    _ => {
                        let _ = inner
                            .event_tx
                            .send(AcpEvent::ExtNotification { method, params });
                    }
                }
            }

            // 3. Incoming request from agent->client (has id AND method)
            // Spawned onto its own task so long-running handlers (`request_permission`, `wait_for_exit`)
            // never block subsequent `session/update` notifications or other requests!
            (true, Some(method)) => {
                let id = msg
                    .get("id")
                    .cloned()
                    .and_then(|v| serde_json::from_value::<RequestId>(v).ok())
                    .unwrap_or(RequestId::Null);
                let params = msg.get("params").cloned().unwrap_or(Value::Object(Default::default()));

                let _ = inner.event_tx.send(AcpEvent::RpcLog(RpcLogEntry {
                    timestamp: now_hms(),
                    direction: RpcDirection::Incoming,
                    summary: format!("req #{id} {method}"),
                    payload: truncate_log_payload(&params.to_string(), 600),
                }));

                tokio::spawn(async move {
                    let result = Self::handle_agent_request(&inner, &method, params).await;
                    Self::send_response(&inner, id, result);
                });
            }

            (false, None) => {}
        }
    }

    async fn handle_agent_request(
        inner: &Arc<AcpClientInner>,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        match method {
            "fs/read_text_file" => {
                let req: ReadTextFileRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(format!("Invalid fs/read_text_file: {e}")))?;
                let resp = handle_read_text_file(req, &inner.spec.cwd).await?;
                serde_json::to_value(resp).map_err(|e| RpcError::internal_error(e.to_string()))
            }

            "fs/write_text_file" => {
                let req: WriteTextFileRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(format!("Invalid fs/write_text_file: {e}")))?;
                let (resp, record) = handle_write_text_file(req, &inner.spec.cwd).await?;
                let _ = inner.event_tx.send(AcpEvent::FileWritten(record));
                serde_json::to_value(resp).map_err(|e| RpcError::internal_error(e.to_string()))
            }

            "terminal/create" => {
                let req: CreateTerminalRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(format!("Invalid terminal/create: {e}")))?;
                let resp = inner.terminal_mgr.create(req).await?;
                serde_json::to_value(resp).map_err(|e| RpcError::internal_error(e.to_string()))
            }

            "terminal/output" => {
                let req: TerminalIdRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(format!("Invalid terminal/output: {e}")))?;
                let resp = inner.terminal_mgr.output(req).await?;
                serde_json::to_value(resp).map_err(|e| RpcError::internal_error(e.to_string()))
            }

            "terminal/wait_for_exit" => {
                let req: TerminalIdRequest = serde_json::from_value(params).map_err(|e| {
                    RpcError::invalid_params(format!("Invalid terminal/wait_for_exit: {e}"))
                })?;
                let resp = inner.terminal_mgr.wait_for_exit(req).await?;
                serde_json::to_value(resp).map_err(|e| RpcError::internal_error(e.to_string()))
            }

            "terminal/kill" => {
                let req: TerminalIdRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(format!("Invalid terminal/kill: {e}")))?;
                inner.terminal_mgr.kill(req).await
            }

            "terminal/release" => {
                let req: TerminalIdRequest = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(format!("Invalid terminal/release: {e}")))?;
                inner.terminal_mgr.release(req).await
            }

            "session/request_permission" => {
                let req: RequestPermissionRequest = serde_json::from_value(params).map_err(|e| {
                    RpcError::invalid_params(format!("Invalid session/request_permission: {e}"))
                })?;
                let (tx, rx) = oneshot::channel();
                let _ = inner.event_tx.send(AcpEvent::PermissionRequest(PendingPermission {
                    request: req,
                    responder: tx,
                }));
                let resp = rx.await.unwrap_or(RequestPermissionResponse {
                    outcome: RequestPermissionOutcome::Cancelled,
                });
                serde_json::to_value(resp).map_err(|e| RpcError::internal_error(e.to_string()))
            }

            "elicitation/create" => {
                let req: CreateElicitationRequest = serde_json::from_value(params).map_err(|e| {
                    RpcError::invalid_params(format!("Invalid elicitation/create: {e}"))
                })?;
                let (tx, rx) = oneshot::channel();
                let _ = inner.event_tx.send(AcpEvent::ElicitationRequest(PendingElicitation {
                    request: req,
                    responder: tx,
                }));
                let resp = rx.await.unwrap_or(CreateElicitationResponse {
                    action: "cancel".to_string(),
                    content: None,
                });
                serde_json::to_value(resp).map_err(|e| RpcError::internal_error(e.to_string()))
            }

            // Cursor native ACP extension fallback (`cursor/ask_question`) mapped to elicitation!
            "cursor/ask_question" => {
                let question = params
                    .get("question")
                    .or_else(|| params.get("message"))
                    .and_then(|q| q.as_str())
                    .unwrap_or("Agent requested input")
                    .to_string();
                let mut props = HashMap::new();
                props.insert(
                    "answer".to_string(),
                    serde_json::json!({
                        "type": "string",
                        "title": "Answer"
                    }),
                );
                let req = CreateElicitationRequest {
                    mode: "form".to_string(),
                    message: question,
                    session_id: params
                        .get("sessionId")
                        .and_then(|s| s.as_str())
                        .map(String::from),
                    tool_call_id: None,
                    elicitation_id: None,
                    url: None,
                    schema: Some(ElicitationSchema {
                        title: Some("Question".to_string()),
                        description: None,
                        properties: props,
                        required: vec!["answer".to_string()],
                    }),
                };
                let (tx, rx) = oneshot::channel();
                let _ = inner.event_tx.send(AcpEvent::ElicitationRequest(PendingElicitation {
                    request: req,
                    responder: tx,
                }));
                let resp = rx.await.unwrap_or(CreateElicitationResponse {
                    action: "cancel".to_string(),
                    content: None,
                });
                let answer = resp
                    .content
                    .as_ref()
                    .and_then(|c| c.get("answer"))
                    .cloned()
                    .unwrap_or(Value::Null);
                Ok(serde_json::json!({
                    "action": resp.action,
                    "answer": answer
                }))
            }

            _ => Err(RpcError::method_not_found(method)),
        }
    }

    fn send_response(inner: &Arc<AcpClientInner>, id: RequestId, result: Result<Value, RpcError>) {
        let payload = match result {
            Ok(val) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": val,
            }),
            Err(err) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": err,
            }),
        };
        let serialized = payload.to_string();
        let _ = inner.event_tx.send(AcpEvent::RpcLog(RpcLogEntry {
            timestamp: now_hms(),
            direction: RpcDirection::Outgoing,
            summary: format!("resp #{id}"),
            payload: truncate_log_payload(&serialized, 600),
        }));
        let _ = inner.stdin_tx.send(serialized);
    }

    pub async fn request<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R, RpcError> {
        let val = self.request_raw(method, serde_json::to_value(params).unwrap_or(Value::Null)).await?;
        // Handle `null` or `{}` gracefully for void/empty response types
        let normalized = if val.is_null() {
            serde_json::json!({})
        } else {
            val
        };
        serde_json::from_value::<R>(normalized)
            .map_err(|e| RpcError::internal_error(format!("Failed to decode {method} response: {e}")))
    }

    pub async fn request_raw(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = RequestId::Number(self.inner.next_id.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id.clone(), tx);

        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let serialized = envelope.to_string();
        let _ = self.inner.event_tx.send(AcpEvent::RpcLog(RpcLogEntry {
            timestamp: now_hms(),
            direction: RpcDirection::Outgoing,
            summary: format!("req #{id} {method}"),
            payload: truncate_log_payload(&serialized, 600),
        }));

        if self.inner.stdin_tx.send(serialized).is_err() {
            self.inner.pending.lock().await.remove(&id);
            return Err(RpcError::internal_error("ACP agent connection is closed"));
        }

        rx.await
            .unwrap_or_else(|_| Err(RpcError::internal_error("ACP agent request channel dropped")))
    }

    pub fn notify<P: Serialize>(&self, method: &str, params: &P) -> Result<(), RpcError> {
        let params_val = serde_json::to_value(params).unwrap_or(Value::Null);
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params_val,
        });
        let serialized = envelope.to_string();
        let _ = self.inner.event_tx.send(AcpEvent::RpcLog(RpcLogEntry {
            timestamp: now_hms(),
            direction: RpcDirection::Outgoing,
            summary: format!("notify {method}"),
            payload: truncate_log_payload(&serialized, 600),
        }));
        self.inner
            .stdin_tx
            .send(serialized)
            .map_err(|_| RpcError::internal_error("ACP agent connection is closed"))
    }

    // ========================================================================
    // High-level ACP Agent Operations
    // ========================================================================

    pub async fn initialize(&self, default_options_meta: Option<Value>) -> Result<InitializeResponse, RpcError> {
        let req = InitializeRequest {
            protocol_version: PROTOCOL_VERSION,
            client_capabilities: Some(ClientCapabilities::full_tui()),
            client_info: Some(Implementation {
                name: "acp-tui".to_string(),
                title: Some("ACP TUI".to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }),
            meta: default_options_meta,
        };
        self.request("initialize", &req).await
    }

    pub async fn authenticate(&self, method_id: &str) -> Result<Value, RpcError> {
        self.request_raw(
            "authenticate",
            serde_json::json!({ "methodId": method_id }),
        )
        .await
    }

    pub async fn logout(&self) -> Result<Value, RpcError> {
        self.request_raw("logout", serde_json::json!({})).await
    }

    pub async fn new_session(
        &self,
        cwd: &str,
        additional_directories: Vec<String>,
        mcp_servers: Vec<McpServer>,
        meta: Option<Value>,
    ) -> Result<NewSessionResponse, RpcError> {
        let req = NewSessionRequest {
            cwd: cwd.to_string(),
            additional_directories,
            mcp_servers,
            meta,
        };
        self.request("session/new", &req).await
    }

    pub async fn load_session(
        &self,
        session_id: &str,
        cwd: &str,
        additional_directories: Vec<String>,
        mcp_servers: Vec<McpServer>,
    ) -> Result<LoadSessionResponse, RpcError> {
        let req = LoadSessionRequest {
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
            additional_directories,
            mcp_servers,
            meta: None,
        };
        self.request("session/load", &req).await
    }

    pub async fn resume_session(
        &self,
        session_id: &str,
        cwd: &str,
        additional_directories: Vec<String>,
        mcp_servers: Vec<McpServer>,
    ) -> Result<ResumeSessionResponse, RpcError> {
        let req = ResumeSessionRequest {
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
            additional_directories,
            mcp_servers,
        };
        self.request("session/resume", &req).await
    }

    pub async fn fork_session(
        &self,
        session_id: &str,
        cwd: &str,
        additional_directories: Vec<String>,
        mcp_servers: Vec<McpServer>,
    ) -> Result<ForkSessionResponse, RpcError> {
        let req = ForkSessionRequest {
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
            additional_directories,
            mcp_servers,
        };
        self.request("session/fork", &req).await
    }

    pub async fn list_sessions(
        &self,
        cwd: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<ListSessionsResponse, RpcError> {
        let req = ListSessionsRequest {
            cwd: cwd.map(String::from),
            cursor: cursor.map(String::from),
        };
        self.request("session/list", &req).await
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<Value, RpcError> {
        self.request_raw(
            "session/delete",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
    }

    pub async fn close_session(&self, session_id: &str) -> Result<Value, RpcError> {
        self.request_raw(
            "session/close",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
    }

    pub async fn set_mode(&self, session_id: &str, mode_id: &str) -> Result<Value, RpcError> {
        self.request_raw(
            "session/set_mode",
            serde_json::json!({
                "sessionId": session_id,
                "modeId": mode_id,
            }),
        )
        .await
    }

    pub async fn set_model_legacy(&self, session_id: &str, model_id: &str) -> Result<Value, RpcError> {
        self.request_raw(
            "session/set_model",
            serde_json::json!({
                "sessionId": session_id,
                "modelId": model_id,
            }),
        )
        .await
    }

    pub async fn set_config_option_select(
        &self,
        session_id: &str,
        config_id: &str,
        value: &str,
    ) -> Result<SetSessionConfigOptionResponse, RpcError> {
        let params = serde_json::json!({
            "sessionId": session_id,
            "configId": config_id,
            "value": value,
        });
        let raw = self.request_raw("session/set_config_option", params).await?;
        Ok(serde_json::from_value(raw).unwrap_or_default())
    }

    pub async fn set_config_option_bool(
        &self,
        session_id: &str,
        config_id: &str,
        value: bool,
    ) -> Result<SetSessionConfigOptionResponse, RpcError> {
        let params = serde_json::json!({
            "sessionId": session_id,
            "configId": config_id,
            "type": "boolean",
            "value": value,
        });
        let raw = self.request_raw("session/set_config_option", params).await?;
        Ok(serde_json::from_value(raw).unwrap_or_default())
    }

    pub async fn prompt(
        &self,
        session_id: &str,
        prompt: Vec<ContentBlock>,
    ) -> Result<PromptResponse, RpcError> {
        let req = PromptRequest {
            session_id: session_id.to_string(),
            prompt,
            meta: None,
        };
        self.request("session/prompt", &req).await
    }

    pub async fn steer(
        &self,
        session_id: &str,
        prompt: Vec<ContentBlock>,
    ) -> Result<Value, RpcError> {
        self.request_raw(
            "_session/steering",
            serde_json::json!({
                "sessionId": session_id,
                "prompt": prompt,
                "_meta": {
                    "steering": {
                        "idleBehavior": "promptRequired"
                    }
                }
            }),
        )
        .await
    }

    pub fn cancel(&self, session_id: &str) -> Result<(), RpcError> {
        self.notify(
            "session/cancel",
            &serde_json::json!({ "sessionId": session_id }),
        )
    }

    pub async fn shutdown(&self) {
        self.inner.terminal_mgr.shutdown_all().await;
        if let Some(mut child) = self.inner.child.lock().await.take() {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                // Send SIGTERM first so ACP adapters like cursor-acp can cancel active runs cleanly
                unsafe {
                    libc_kill_sigterm(pid as i32);
                }
            }
            let wait_res = tokio::time::timeout(Duration::from_millis(1500), child.wait()).await;
            if wait_res.is_err() {
                let _ = child.kill().await;
                let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
            }
        }
    }
}

#[cfg(unix)]
unsafe fn libc_kill_sigterm(pid: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const SIGTERM: i32 = 15;
    let _ = unsafe { kill(pid, SIGTERM) };
}
