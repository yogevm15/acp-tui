mod acp;
mod app;
mod cli;
mod config;
mod state;
#[cfg(test)]
mod tests;
mod ui;

use crate::app::{App, AppInit, AppMsg, SessionAction};
use crate::cli::{Cli, CliSubcommand};
use crate::config::{AppStateStore, ResolvedAgentProfile, load_agents};
use crate::state::ExternalTerminalCommand;
use crate::ui::theme::Theme;
use anyhow::{Context, Result, bail};
use base64::Engine;
use clap::Parser;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{Stdout, Write};
use std::time::Duration;
use tokio::sync::mpsc;

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn enter_tui(keyboard_enhanced: bool) -> Result<()> {
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;
    if keyboard_enhanced {
        let _ = execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    Ok(())
}

fn leave_tui(keyboard_enhanced: bool) {
    let mut out = std::io::stdout();
    if keyboard_enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen);
    let _ = disable_raw_mode();
    let _ = execute!(out, crossterm::cursor::Show);
}

fn resolve_profile(
    cli: &Cli,
    profiles: &[ResolvedAgentProfile],
    last_agent: Option<&str>,
) -> Result<ResolvedAgentProfile> {
    if let Some(id) = &cli.agent {
        return profiles
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(id))
            .cloned()
            .with_context(|| {
                let ids: Vec<&str> = profiles.iter().map(|p| p.id.as_str()).collect();
                format!("Unknown agent '{id}'. Agents in acp.json: {}", ids.join(", "))
            });
    }
    if let Some(p) = last_agent.and_then(|id| profiles.iter().find(|p| p.id == id)) {
        return Ok(p.clone());
    }
    profiles.first().cloned().context("acp.json has no agents")
}

async fn run_external(cmd: &ExternalTerminalCommand) -> (bool, String) {
    println!(
        "\n\x1b[1m▶ {}\x1b[0m  ({} {})\n",
        cmd.label,
        cmd.command,
        cmd.args.join(" ")
    );
    let status = tokio::process::Command::new(&cmd.command)
        .args(&cmd.args)
        .envs(&cmd.env)
        .current_dir(&cmd.cwd)
        .status()
        .await;
    match status {
        Ok(s) if s.success() => (true, String::new()),
        Ok(s) => (false, format!("exit status {s}")),
        Err(e) => (false, e.to_string()),
    }
}

async fn run_app(mut terminal: Tui, mut app: App, keyboard_enhanced: bool,
    mut acp_rx: mpsc::UnboundedReceiver<acp::client::AcpEvent>,
    mut msg_rx: mpsc::UnboundedReceiver<AppMsg>,
) -> Result<App> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(80));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if app.dirty {
            terminal.draw(|f| ui::render::draw(f, &mut app))?;
            app.dirty = false;
            if let Some(b64) = app.pending_osc52.take() {
                let mut out = std::io::stdout();
                let _ = write!(out, "\x1b]52;c;{b64}\x07");
                let _ = out.flush();
            }
        }
        if app.should_quit {
            break;
        }
        if let Some(ext) = app.pending_external.take() {
            drop(events);
            leave_tui(keyboard_enhanced);
            let (ok, detail) = run_external(&ext).await;
            enter_tui(keyboard_enhanced)?;
            terminal.clear()?;
            events = EventStream::new();
            app.on_external_auth_done(ok, detail);
            continue;
        }

        tokio::select! {
            biased;
            Some(ev) = acp_rx.recv() => app.handle_acp(ev),
            Some(msg) = msg_rx.recv() => app.handle_msg(msg),
            maybe = events.next() => match maybe {
                Some(Ok(Event::Key(k))) => app.handle_key(k),
                Some(Ok(Event::Paste(s))) => app.handle_paste(s),
                Some(Ok(Event::Mouse(m))) => app.handle_mouse(m),
                Some(Ok(Event::Resize(_, _))) => app.dirty = true,
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
            _ = tick.tick() => app.tick(),
        }

        // Drain bursts (streamed chunks) before the next frame.
        for _ in 0..512 {
            let mut progressed = false;
            if let Ok(ev) = acp_rx.try_recv() {
                app.handle_acp(ev);
                progressed = true;
            }
            if let Ok(msg) = msg_rx.try_recv() {
                app.handle_msg(msg);
                progressed = true;
            }
            if !progressed {
                break;
            }
        }
    }
    Ok(app)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = match &cli.cwd {
        Some(p) => std::fs::canonicalize(p).with_context(|| format!("Invalid --cwd {}", p.display()))?,
        None => std::env::current_dir()?,
    };
    let (acp_json_path, profiles) = load_agents(cli.acp_json.as_deref())?;

    if let Some(CliSubcommand::Agents) = &cli.subcommand {
        println!("Agents from {}\n", acp_json_path.display());
        println!("{:<20} {:<10} COMMAND", "NAME", "STATUS");
        for p in &profiles {
            println!(
                "{:<20} {:<10} {}",
                p.id,
                if p.available { "ready" } else { "not found" },
                p.command_line_display()
            );
        }
        return Ok(());
    }

    let store = AppStateStore::new();
    let prefs = store.load_prefs();
    let profile = resolve_profile(&cli, &profiles, prefs.last_agent_id.as_deref())?;
    if !profile.available {
        eprintln!(
            "warning: the command for agent '{}' was not found ({}).",
            profile.id,
            profile.command_line_display()
        );
    }
    let theme_name = cli
        .theme
        .clone()
        .or_else(|| prefs.theme.clone())
        .unwrap_or_else(|| "opencode".to_string());

    let add_dirs: Vec<String> = cli
        .add_dirs
        .iter()
        .map(|d| std::fs::canonicalize(d).unwrap_or_else(|_| d.clone()).to_string_lossy().into_owned())
        .collect();
    let cli_mode = if cli.yolo {
        Some("yolo".to_string())
    } else {
        cli.mode.clone()
    };
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        bail!("acp-tui needs an interactive terminal");
    }

    let (acp_tx, acp_rx) = mpsc::unbounded_channel();
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();
    let initial_action = if let Some(id) = &cli.session {
        SessionAction::Load {
            session_id: id.clone(),
            cwd: cwd.to_string_lossy().into_owned(),
        }
    } else if cli.continue_last {
        SessionAction::ContinueLast
    } else {
        SessionAction::New
    };

    let mut app = App::new(AppInit {
        acp_json_path,
        store,
        prefs,
        theme: Theme::by_name(&theme_name),
        profiles,
        profile,
        cwd,
        add_dirs,
        cli_mode,
        cli_model: cli.model.clone(),
        initial_prompt: cli.prompt.clone(),
        auto_approve: cli.yolo,
        acp_tx,
        msg_tx,
    });
    app.connect(Some(initial_action));

    let keyboard_enhanced = supports_keyboard_enhancement().unwrap_or(false);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave_tui(keyboard_enhanced);
        default_hook(info);
    }));

    enter_tui(keyboard_enhanced)?;
    let terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let result = run_app(terminal, app, keyboard_enhanced, acp_rx, msg_rx).await;
    leave_tui(keyboard_enhanced);

    let app = result?;
    if let Some(client) = app.client.clone() {
        if app.busy
            && let Some(sid) = &app.session_id
        {
            let _ = client.cancel(sid);
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), client.shutdown()).await;
    }
    if let Some(sid) = &app.session_id
        && app.session_has_prompt
    {
        println!(
            "Session {sid}\nResume with: acp-tui --agent {} --session {sid}",
            app.profile.id
        );
    }
    let _ = base64::engine::general_purpose::STANDARD.encode("");
    Ok(())
}
