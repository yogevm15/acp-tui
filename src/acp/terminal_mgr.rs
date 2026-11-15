use crate::acp::types::{
    CreateTerminalRequest, CreateTerminalResponse, RpcError, TerminalExitStatus, TerminalIdRequest,
    TerminalOutputResponse,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{Mutex, Notify, mpsc};
use uuid::Uuid;

const DEFAULT_OUTPUT_BYTE_LIMIT: usize = 1024 * 1024; // 1 MiB default retention

#[derive(Debug, Clone)]
pub struct TerminalSnapshot {
    pub terminal_id: String,
    #[allow(dead_code)]
    pub session_id: String,
    pub command: String,
    pub cwd: String,
    pub output: String,
    pub truncated: bool,
    pub exit_status: Option<TerminalExitStatus>,
}

struct TerminalState {
    session_id: String,
    command_display: String,
    cwd: String,
    output: String,
    truncated: bool,
    byte_limit: usize,
    exit_status: Option<TerminalExitStatus>,
}

struct ManagedTerminal {
    state: Arc<Mutex<TerminalState>>,
    exit_notify: Arc<Notify>,
    kill_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[derive(Clone)]
pub struct TerminalManager {
    terminals: Arc<Mutex<HashMap<String, Arc<ManagedTerminal>>>>,
    default_cwd: PathBuf,
    snapshot_tx: mpsc::UnboundedSender<TerminalSnapshot>,
}

/// Truncate from the beginning at a valid UTF-8 character boundary so `s.len() <= limit`.
pub fn truncate_utf8_prefix(s: &mut String, limit: usize) -> bool {
    if s.len() <= limit {
        return false;
    }
    let excess = s.len() - limit;
    let mut cut = excess;
    while cut < s.len() && !s.is_char_boundary(cut) {
        cut += 1;
    }
    s.drain(..cut);
    true
}

impl TerminalManager {
    pub fn new(
        default_cwd: PathBuf,
        snapshot_tx: mpsc::UnboundedSender<TerminalSnapshot>,
    ) -> Self {
        Self {
            terminals: Arc::new(Mutex::new(HashMap::new())),
            default_cwd,
            snapshot_tx,
        }
    }

    pub async fn create(
        &self,
        req: CreateTerminalRequest,
    ) -> Result<CreateTerminalResponse, RpcError> {
        let terminal_id = format!("term-{}", Uuid::new_v4().simple());
        let cwd_path = req
            .cwd
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.default_cwd.clone());
        let cwd_display = cwd_path.to_string_lossy().into_owned();

        let command_display = if req.args.is_empty() {
            req.command.clone()
        } else {
            format!("{} {}", req.command, req.args.join(" "))
        };

        let mut cmd = if req.args.is_empty() && !Path::new(&req.command).is_file() {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            let mut c = Command::new(shell);
            c.arg("-c").arg(&req.command);
            c
        } else {
            let mut c = Command::new(&req.command);
            c.args(&req.args);
            c
        };

        cmd.current_dir(&cwd_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for env_var in &req.env {
            cmd.env(&env_var.name, &env_var.value);
        }

        let mut child = cmd.spawn().map_err(|e| {
            RpcError::internal_error(format!("Failed to spawn terminal '{}': {e}", req.command))
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let byte_limit = req
            .output_byte_limit
            .filter(|&b| b > 0)
            .unwrap_or(DEFAULT_OUTPUT_BYTE_LIMIT);

        let state = Arc::new(Mutex::new(TerminalState {
            session_id: req.session_id.clone(),
            command_display: command_display.clone(),
            cwd: cwd_display.clone(),
            output: String::new(),
            truncated: false,
            byte_limit,
            exit_status: None,
        }));
        let exit_notify = Arc::new(Notify::new());
        let (kill_tx, mut kill_rx) = tokio::sync::oneshot::channel::<()>();

        let managed = Arc::new(ManagedTerminal {
            state: state.clone(),
            exit_notify: exit_notify.clone(),
            kill_tx: Mutex::new(Some(kill_tx)),
        });

        self.terminals
            .lock()
            .await
            .insert(terminal_id.clone(), managed);

        let _ = self.snapshot_tx.send(TerminalSnapshot {
            terminal_id: terminal_id.clone(),
            session_id: req.session_id.clone(),
            command: command_display,
            cwd: cwd_display,
            output: String::new(),
            truncated: false,
            exit_status: None,
        });

        let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        if let Some(mut out) = stdout {
            let tx = chunk_tx.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = out.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let _ = tx.send(buf[..n].to_vec());
                }
            });
        }
        if let Some(mut err) = stderr {
            let tx = chunk_tx.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = err.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let _ = tx.send(buf[..n].to_vec());
                }
            });
        }
        drop(chunk_tx);

        let state_reader = state.clone();
        let snap_tx_reader = self.snapshot_tx.clone();
        let term_id_reader = terminal_id.clone();
        let reader_done = tokio::spawn(async move {
            while let Some(bytes) = chunk_rx.recv().await {
                let text = String::from_utf8_lossy(&bytes);
                let snap = {
                    let mut st = state_reader.lock().await;
                    st.output.push_str(&text);
                    let limit = st.byte_limit;
                    if truncate_utf8_prefix(&mut st.output, limit) {
                        st.truncated = true;
                    }
                    TerminalSnapshot {
                        terminal_id: term_id_reader.clone(),
                        session_id: st.session_id.clone(),
                        command: st.command_display.clone(),
                        cwd: st.cwd.clone(),
                        output: st.output.clone(),
                        truncated: st.truncated,
                        exit_status: st.exit_status.clone(),
                    }
                };
                let _ = snap_tx_reader.send(snap);
            }
        });

        let state_waiter = state.clone();
        let snap_tx_waiter = self.snapshot_tx.clone();
        let term_id_waiter = terminal_id.clone();
        tokio::spawn(async move {
            let status_res = tokio::select! {
                res = child.wait() => res,
                _ = &mut kill_rx => {
                    let _ = child.kill().await;
                    child.wait().await
                }
            };
            let _ = reader_done.await;

            let exit_status = match status_res {
                Ok(status) => {
                    #[cfg(unix)]
                    let signal = {
                        use std::os::unix::process::ExitStatusExt;
                        status.signal().map(|sig| format!("SIG{sig}"))
                    };
                    #[cfg(not(unix))]
                    let signal = None;

                    TerminalExitStatus {
                        exit_code: status.code(),
                        signal,
                    }
                }
                Err(_) => TerminalExitStatus {
                    exit_code: Some(1),
                    signal: None,
                },
            };

            let snap = {
                let mut st = state_waiter.lock().await;
                st.exit_status = Some(exit_status);
                TerminalSnapshot {
                    terminal_id: term_id_waiter,
                    session_id: st.session_id.clone(),
                    command: st.command_display.clone(),
                    cwd: st.cwd.clone(),
                    output: st.output.clone(),
                    truncated: st.truncated,
                    exit_status: st.exit_status.clone(),
                }
            };
            let _ = snap_tx_waiter.send(snap);
            exit_notify.notify_waiters();
        });

        Ok(CreateTerminalResponse { terminal_id })
    }

    pub async fn output(&self, req: TerminalIdRequest) -> Result<TerminalOutputResponse, RpcError> {
        let managed = self
            .terminals
            .lock()
            .await
            .get(&req.terminal_id)
            .cloned()
            .ok_or_else(|| RpcError::invalid_params(format!("Unknown terminal: {}", req.terminal_id)))?;

        let st = managed.state.lock().await;
        Ok(TerminalOutputResponse {
            output: st.output.clone(),
            truncated: st.truncated,
            exit_status: st.exit_status.clone(),
        })
    }

    pub async fn wait_for_exit(
        &self,
        req: TerminalIdRequest,
    ) -> Result<TerminalExitStatus, RpcError> {
        let managed = self
            .terminals
            .lock()
            .await
            .get(&req.terminal_id)
            .cloned()
            .ok_or_else(|| RpcError::invalid_params(format!("Unknown terminal: {}", req.terminal_id)))?;

        loop {
            let notified = managed.exit_notify.notified();
            {
                let st = managed.state.lock().await;
                if let Some(exit) = &st.exit_status {
                    return Ok(exit.clone());
                }
            }
            notified.await;
        }
    }

    pub async fn kill(&self, req: TerminalIdRequest) -> Result<Value, RpcError> {
        let managed = self
            .terminals
            .lock()
            .await
            .get(&req.terminal_id)
            .cloned()
            .ok_or_else(|| RpcError::invalid_params(format!("Unknown terminal: {}", req.terminal_id)))?;

        if let Some(kill_tx) = managed.kill_tx.lock().await.take() {
            let _ = kill_tx.send(());
        }
        Ok(serde_json::json!({}))
    }

    pub async fn release(&self, req: TerminalIdRequest) -> Result<Value, RpcError> {
        let removed = self.terminals.lock().await.remove(&req.terminal_id);
        if let Some(managed) = removed
            && let Some(kill_tx) = managed.kill_tx.lock().await.take()
        {
            let _ = kill_tx.send(());
        }
        Ok(serde_json::json!({}))
    }

    pub async fn shutdown_all(&self) {
        let all: Vec<Arc<ManagedTerminal>> = self
            .terminals
            .lock()
            .await
            .drain()
            .map(|(_, v)| v)
            .collect();
        for m in all {
            if let Some(kill_tx) = m.kill_tx.lock().await.take() {
                let _ = kill_tx.send(());
            }
        }
    }
}
