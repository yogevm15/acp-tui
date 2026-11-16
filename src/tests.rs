use crate::acp::types::*;
use crate::app::{App, AppInit, SessionAction};
use crate::config::{AppStateStore, ResolvedAgentProfile, SavedPrefs, load_agents};
use crate::state::{ActiveModal, CompletionPopupKind, TimelineItem, ToolCallCard};
use crate::ui::composer::{Composer, build_prompt_blocks, fuzzy_score, rank_candidates};
use crate::ui::markdown::{render_markdown, render_unified_diff, strip_ansi};
use crate::ui::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[test]
fn parses_session_updates_leniently() {
    let v = serde_json::json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "x",
        "title": "Read a",
        "kind": "read",
        "locations": [{"path": "/a", "line": "12"}],
        "content": [{"type": "diff", "path": "/a", "newText": "n"}, {"type": "future_kind"}]
    });
    match SessionUpdate::from_value(v) {
        SessionUpdate::ToolCall(t) => {
            assert_eq!(t.locations[0].line, Some(12));
            assert_eq!(t.content.len(), 2);
            assert!(matches!(t.content[1], ToolCallContent::Unknown));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        SessionUpdate::from_value(serde_json::json!({"sessionUpdate": "brand_new"})),
        SessionUpdate::Unknown
    ));
}

#[test]
fn parses_grouped_config_options_and_booleans() {
    let opt: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "id": "model", "name": "Model", "type": "select", "currentValue": "b",
        "options": [{"group": "g", "name": "Group", "options": [{"value": "b", "name": "Bee"}]}]
    }))
    .unwrap();
    assert_eq!(opt.display_value(), "Bee");
    assert_eq!(opt.options.flatten()[0].group_name.as_deref(), Some("Group"));
    let b: SessionConfigOption = serde_json::from_value(serde_json::json!({
        "id": "fast", "name": "Fast", "type": "boolean", "currentValue": true
    }))
    .unwrap();
    assert!(b.is_boolean());
    assert_eq!(b.display_value(), "on");
}

#[test]
fn composer_editing_and_completion_queries() {
    let mut c = Composer::new(vec!["older".into()]);
    c.insert_str("héllo wörld");
    c.move_word_left();
    c.delete_word_back();
    assert_eq!(c.text(), "wörld");
    c.undo();
    assert_eq!(c.text(), "héllo wörld");
    c.clear();
    c.insert_str("/mo");
    assert_eq!(c.active_slash_query().as_deref(), Some("mo"));
    c.complete_slash_command("model", true);
    assert_eq!(c.text(), "/model ");
    c.clear();
    c.insert_str("see @src/ma");
    assert_eq!(c.active_at_mention().map(|(_, q)| q).as_deref(), Some("src/ma"));
    c.complete_at_mention("src/main.rs");
    assert_eq!(c.text(), "see @src/main.rs ");
    c.clear();
    c.move_up_or_history();
    assert_eq!(c.text(), "older");
    c.clear();
    c.insert_str("line\\");
    assert!(c.try_backslash_newline());
    assert_eq!(c.lines.len(), 2);
}

#[test]
fn fuzzy_prefers_basename_matches() {
    let a = fuzzy_score("main", "src/main.rs").unwrap();
    let b = fuzzy_score("main", "docs/domain/notes.md").unwrap_or(i64::MIN);
    assert!(a > b);
    assert!(fuzzy_score("zzz", "src/main.rs").is_none());
}

#[test]
fn prompt_blocks_embed_mentioned_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let caps = PromptCapabilities {
        embedded_context: true,
        ..Default::default()
    };
    let p = build_prompt_blocks("look @a.txt:2-3 and @missing.rs", dir.path(), &caps);
    assert_eq!(p.blocks.len(), 2);
    match &p.blocks[1] {
        ContentBlock::Resource { resource } => assert_eq!(resource.text.as_deref(), Some("two\nthree")),
        other => panic!("unexpected {other:?}"),
    }
    let p2 = build_prompt_blocks("look @a.txt", dir.path(), &PromptCapabilities::default());
    assert!(matches!(p2.blocks[1], ContentBlock::ResourceLink { .. }));
}

#[test]
fn markdown_and_diff_render() {
    let t = Theme::opencode_dark();
    let lines = render_markdown("# Title\n\n- item **bold**\n\n```rust\nfn x() {}\n```\n", 60, &t);
    let text: String = lines.iter().map(|l| l.to_string() + "\n").collect();
    assert!(text.contains("Title"));
    assert!(text.contains("• item bold"));
    assert!(text.contains("fn x() {}"));
    let d = render_unified_diff(Some("a\nb\n"), "a\nc\n", 60, None, &t);
    assert_eq!((d.added, d.removed), (1, 1));
    assert_eq!(strip_ansi("\x1b[31mred\x1b[0m\r"), "red");
}

#[test]
fn loads_jetbrains_acp_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("acp.json");
    std::fs::write(
        &path,
        r#"{
          "default_mcp_settings": {"use_idea_mcp": true},
          "agent_servers": {
            "Zeta": {"command": "/bin/sh", "args": ["-c", "x"], "env": {"A": "1"},
                     "default_config_options": {"mode": "plan"}},
            "Alpha": {"command": "definitely-missing-binary",
                      "mcp_servers": [{"name": "fs", "command": "mcp-fs", "env": {"K": "V"}}]}
          }
        }"#,
    )
    .unwrap();
    let (used, agents) = load_agents(Some(&path)).unwrap();
    assert_eq!(used, path);
    let names: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(names, vec!["Zeta", "Alpha"], "file order is kept");
    assert!(agents[0].available);
    assert!(!agents[1].available);
    assert_eq!(agents[0].env.get("A").map(String::as_str), Some("1"));
    assert_eq!(
        agents[0].default_options_json.as_ref().unwrap()["default_config_options"]["mode"],
        "plan"
    );
    assert_eq!(agents[1].mcp_servers.len(), 1);
    assert!(load_agents(Some(&dir.path().join("missing.json"))).is_err());
}

#[test]
fn ranking_prefers_prefix_and_drops_loose_matches() {
    let names = ["mode", "model", "migrate-to-skills", "theme"];
    let ranked: Vec<&str> = rank_candidates("mo", &names).iter().map(|&i| names[i]).collect();
    assert_eq!(ranked, vec!["mode", "model"]);
    let models = ["GPT-5 Codex", "Claude Sonnet 4.5", "Composer 2"];
    let ranked: Vec<&str> = rank_candidates("son", &models).iter().map(|&i| models[i]).collect();
    assert_eq!(ranked, vec!["Claude Sonnet 4.5"]);
    let separated = ["Claude · Sonnet", "GPT—Codex"];
    let ranked: Vec<&str> = rank_candidates("son", &separated).iter().map(|&i| separated[i]).collect();
    assert_eq!(ranked, vec!["Claude · Sonnet"]);
    let ranked: Vec<&str> = rank_candidates("cod", &separated).iter().map(|&i| separated[i]).collect();
    assert_eq!(ranked, vec!["GPT—Codex"]);
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Draws the current state at many terminal sizes, including degenerate ones.
fn draw_at_all_sizes(app: &mut App) {
    const WIDTHS: &[u16] = &[0, 1, 2, 3, 4, 5, 8, 10, 13, 17, 20, 23, 30, 40, 60, 80, 99, 100, 101, 120, 233];
    const HEIGHTS: &[u16] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 15, 20, 40];
    for &w in WIDTHS {
        for &h in HEIGHTS {
            let backend = ratatui::backend::TestBackend::new(w, h);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| crate::ui::render::draw(f, app)).unwrap();
        }
    }
}

fn draw_every_view_at_all_sizes(app: &mut App) {
    draw_at_all_sizes(app);
    for _ in 0..5 {
        app.toast("a toast message that is quite long so it needs to be truncated", false);
    }
    app.queued.extend(["queued one".to_string(), "queued two".into(), "queued three".into(), "queued four".into()]);
    app.composer.set_text("line one\nline two\nline three\nline four\nline five\nline six\nline seven\nline eight\nline nine\nline ten\nline eleven");
    app.scroll_from_bottom = 3;
    draw_at_all_sizes(app);
    app.queued.clear();
    app.scroll_from_bottom = 0;

    for text in ["/", "/model ", "@src/"] {
        app.composer.set_text(text);
        app.refresh_completion();
        draw_at_all_sizes(app);
    }
    app.completion = None;
    app.composer.clear();

    let openers = [key(KeyCode::F(1)), key(KeyCode::F(12)), ctrl('p'), ctrl('s'), ctrl('l'), ctrl('g'), ctrl('r')];
    for open in openers {
        app.modal = None;
        app.handle_key(open);
        draw_at_all_sizes(app);
    }
    app.modal = None;
    app.open_config_modal();
    draw_at_all_sizes(app);
    app.modal = None;
    app.open_rewind_modal();
    draw_at_all_sizes(app);
    app.modal = None;
}

/// Drives the real `App` against `tests/fixtures/mock_agent.py`.
#[tokio::test(flavor = "multi_thread")]
async fn end_to_end_with_mock_agent() {
    if crate::config::find_binary_in_path("python3").is_none() {
        return;
    }
    let data_dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("ACP_TUI_DATA_DIR", data_dir.path()) };
    let cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = cwd.join("tests/fixtures/mock_agent.py");
    let profile = ResolvedAgentProfile {
        id: "mock".into(),
        command: "python3".into(),
        args: vec![script.to_string_lossy().into_owned(), format!("{}/", cwd.display())],
        env: HashMap::new(),
        available: true,
        default_options_json: None,
        mcp_servers: Vec::new(),
    };
    let (acp_tx, mut acp_rx) = tokio::sync::mpsc::unbounded_channel();
    let (msg_tx, mut msg_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(AppInit {
        acp_json_path: std::path::PathBuf::from("acp.json"),
        store: AppStateStore::new(),
        prefs: SavedPrefs::default(),
        theme: Theme::opencode_dark(),
        profiles: vec![profile.clone()],
        profile,
        cwd: cwd.clone(),
        add_dirs: Vec::new(),
        cli_mode: Some("plan".into()),
        cli_model: None,
        initial_prompt: None,
        auto_approve: false,
        acp_tx,
        msg_tx,
    });
    draw_every_view_at_all_sizes(&mut app);
    app.connect(Some(SessionAction::New));

    async fn pump(
        app: &mut App,
        acp_rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::acp::client::AcpEvent>,
        msg_rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::app::AppMsg>,
        until: impl Fn(&App) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !until(app) {
            assert!(Instant::now() < deadline, "timed out waiting for app state");
            tokio::select! {
                biased;
                Some(ev) = acp_rx.recv() => app.handle_acp(ev),
                Some(m) = msg_rx.recv() => app.handle_msg(m),
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }

    pump(&mut app, &mut acp_rx, &mut msg_rx, |a| a.session_id.is_some() && !a.agent_commands.is_empty()).await;
    assert_eq!(app.agent_display_name(), "Mock Agent 1.0");
    assert_eq!(app.current_mode_id().as_deref(), Some("plan"), "--mode applied via session/set_mode");
    assert_eq!(app.current_model_label().as_deref(), Some("Model One"));
    assert!(app.all_slash_defs().iter().any(|c| c.name == "review" && !c.is_builtin_ui));

    // Terminals that remap ctrl+arrows to readline `\eb` / `\ef` and Home/End to ctrl+a / ctrl+e.
    let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
    app.composer.set_text("alpha beta gamma");
    app.handle_key(alt('b'));
    assert_eq!(app.composer.col, 11);
    app.handle_key(alt('b'));
    assert_eq!(app.composer.col, 6);
    app.handle_key(alt('f'));
    assert_eq!(app.composer.col, 11);
    app.handle_key(ctrl('a'));
    assert_eq!(app.composer.col, 0);
    app.handle_key(alt('d'));
    assert_eq!(app.composer.text(), "beta gamma");
    app.handle_key(ctrl('e'));
    assert_eq!(app.composer.col, "beta gamma".len());
    app.composer.clear();

    // `/model <partial>` completes model values and Enter applies the choice.
    app.composer.set_text("/model tw");
    app.refresh_completion();
    let popup = app.completion.as_ref().expect("model completion popup");
    assert!(matches!(popup.kind, CompletionPopupKind::CommandArg));
    assert_eq!(popup.candidates.len(), 1);
    assert_eq!(popup.candidates[0].value, "m2");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.composer.is_empty());
    assert_eq!(app.current_model_id().as_deref(), Some("m2"));

    app.composer.set_text("hi there");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.busy);

    pump(&mut app, &mut acp_rx, &mut msg_rx, |a| matches!(a.modal, Some(ActiveModal::Permission(_)))).await;
    draw_at_all_sizes(&mut app);
    app.handle_key(key(KeyCode::Enter));

    pump(&mut app, &mut acp_rx, &mut msg_rx, |a| !a.busy).await;
    let reply = app.timeline.iter().find_map(|i| match i {
        TimelineItem::AssistantMessage { text, .. } => Some(text.clone()),
        _ => None,
    });
    assert_eq!(reply.as_deref(), Some("Echo: hi there | [package] | from-terminal"));
    let statuses: Vec<String> = app
        .timeline
        .iter()
        .filter_map(|i| match i {
            TimelineItem::ToolCall(c) => Some(c.status.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(statuses, vec!["completed", "completed"]);
    assert_eq!(app.plan.len(), 2);
    assert_eq!(app.usage.context_size, 200000);
    assert_eq!(app.modified_files()[0].added, 1);

    // Render a frame to make sure layout code handles the populated state.
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::render::draw(f, &mut app)).unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(screen.contains("Mock Agent"));
    assert!(screen.contains("from-terminal"));

    draw_every_view_at_all_sizes(&mut app);

    if let Some(c) = app.client.clone() {
        c.shutdown().await;
    }
}

#[test]
fn mouse_click_expands_steps_and_live_thinking() {
    let cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let profile = ResolvedAgentProfile {
        id: "mock".into(),
        command: "python3".into(),
        args: Vec::new(),
        env: HashMap::new(),
        available: true,
        default_options_json: None,
        mcp_servers: Vec::new(),
    };
    let (acp_tx, _acp_rx) = tokio::sync::mpsc::unbounded_channel();
    let (msg_tx, _msg_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(AppInit {
        acp_json_path: std::path::PathBuf::from("acp.json"),
        store: AppStateStore::new(),
        prefs: SavedPrefs::default(),
        theme: Theme::opencode_dark(),
        profiles: vec![profile.clone()],
        profile,
        cwd,
        add_dirs: Vec::new(),
        cli_mode: None,
        cli_model: None,
        initial_prompt: None,
        auto_approve: false,
        acp_tx,
        msg_tx,
    });

    app.push_item(TimelineItem::AssistantThought {
        id: Some("t1".into()),
        text: "First finished thought.\nHidden finished detail alpha.".into(),
    });
    app.push_item(TimelineItem::AssistantThought {
        id: Some("t2".into()),
        text: "Second finished thought.\nHidden finished detail beta.".into(),
    });
    app.push_item(TimelineItem::ToolCall(ToolCallCard {
        tool_call_id: "read-1".into(),
        title: "src/cli.rs".into(),
        name: Some("Read".into()),
        kind: "read".into(),
        status: "completed".into(),
        locations: Vec::new(),
        diffs: Vec::new(),
        text_outputs: vec!["```\nread-secret-line-one\nread-secret-line-two\n```".into()],
        terminal_ids: Vec::new(),
        raw_input: None,
        raw_output: None,
        started_at: Instant::now(),
        duration_ms: Some(12),
    }));
    let live_idx = app.push_item(TimelineItem::AssistantThought {
        id: Some("live".into()),
        text: (1..=6)
            .map(|i| format!("live-thought-line-{i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    app.busy = true;

    let render_rows = |app: &mut App| -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::render::draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    };
    let left_click = |app: &mut App, col: u16, row: u16| {
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        });
    };

    // Initial state: finished thoughts, read card, and live thought are all collapsed.
    let rows = render_rows(&mut app);
    let joined = rows.join("\n");
    assert!(!joined.contains("Hidden finished detail alpha"));
    assert!(!joined.contains("Hidden finished detail beta"));
    assert!(joined.contains("Read 2 lines"));
    assert!(!joined.contains("read-secret-line-one"));
    assert!(!joined.contains("live-thought-line-1"));
    assert!(joined.contains("live-thought-line-6"));

    // Click first finished thought: expands only the first thought.
    let t1_row = rows
        .iter()
        .position(|r| r.contains("First finished thought"))
        .unwrap() as u16;
    left_click(&mut app, 6, t1_row);
    let rows = render_rows(&mut app);
    let joined = rows.join("\n");
    assert!(joined.contains("Hidden finished detail alpha"));
    assert!(!joined.contains("Hidden finished detail beta"));

    // Click the READ tool step: expands its file contents.
    let read_row = rows
        .iter()
        .position(|r| r.contains("src/cli.rs"))
        .unwrap() as u16;
    left_click(&mut app, 6, read_row);
    let rows = render_rows(&mut app);
    let joined = rows.join("\n");
    assert!(joined.contains("read-secret-line-one"));
    assert!(joined.contains("read-secret-line-two"));

    // Click the READ tool step again: collapses back to summary.
    let read_row = rows
        .iter()
        .position(|r| r.contains("src/cli.rs"))
        .unwrap() as u16;
    left_click(&mut app, 6, read_row);
    let rows = render_rows(&mut app);
    assert!(!rows.join("\n").contains("read-secret-line-one"));

    // Click the current (live) thinking step: expands all lines while still streaming.
    let live_row = rows
        .iter()
        .position(|r| r.contains("Thinking…") && r.contains("click to expand"))
        .unwrap() as u16;
    left_click(&mut app, 6, live_row);
    let rows = render_rows(&mut app);
    let joined = rows.join("\n");
    assert!(joined.contains("live-thought-line-1"));
    assert!(joined.contains("live-thought-line-6"));

    // Stream another chunk into the live thinking step: stays expanded in full.
    if let TimelineItem::AssistantThought { text, .. } = &mut app.timeline[live_idx] {
        text.push_str("\nlive-thought-line-7");
    }
    app.touch(live_idx);
    let rows = render_rows(&mut app);
    let joined = rows.join("\n");
    assert!(joined.contains("live-thought-line-1"));
    assert!(joined.contains("live-thought-line-7"));

    // Click the live thinking step again to collapse back to the 3-line rolling preview.
    let live_row = rows
        .iter()
        .position(|r| r.contains("click to collapse"))
        .unwrap() as u16;
    left_click(&mut app, 6, live_row);
    let rows = render_rows(&mut app);
    let joined = rows.join("\n");
    assert!(!joined.contains("live-thought-line-1"));
    assert!(joined.contains("live-thought-line-7"));

    // Clicking the busy activity bar also toggles the active thinking step.
    let activity_y = app.timeline_hit.activity_rect.y;
    left_click(&mut app, 4, activity_y);
    let rows = render_rows(&mut app);
    assert!(rows.join("\n").contains("live-thought-line-1"));
}

#[test]
fn help_modal_expands_and_wraps_without_clipping() {
    let cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let profile = ResolvedAgentProfile {
        id: "mock".into(),
        command: "python3".into(),
        args: Vec::new(),
        env: HashMap::new(),
        available: true,
        default_options_json: None,
        mcp_servers: Vec::new(),
    };
    let (acp_tx, _acp_rx) = tokio::sync::mpsc::unbounded_channel();
    let (msg_tx, _msg_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(AppInit {
        acp_json_path: std::path::PathBuf::from("acp.json"),
        store: AppStateStore::new(),
        prefs: SavedPrefs::default(),
        theme: Theme::opencode_dark(),
        profiles: vec![profile.clone()],
        profile,
        cwd,
        add_dirs: Vec::new(),
        cli_mode: None,
        cli_model: None,
        initial_prompt: None,
        auto_approve: false,
        acp_tx,
        msg_tx,
    });

    let render_screen = |app: &mut App, w: u16, h: u16| -> String {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::render::draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    app.handle_key(key(KeyCode::F(1)));

    // Wide terminal: expands so all descriptions and groups fit without clipping or scrolling.
    let wide = render_screen(&mut app, 140, 50);
    assert!(wide.contains("Send the message (steer or queue while the agent works)"));
    assert!(wide.contains("Delete word back · word forward · to line start · to line end"));
    assert!(wide.contains("Slash commands (agent + TUI); /model, /mode, /thinking, /agent, /theme complete values"));
    assert!(wide.contains("Interrupt the running turn (esc esc clears the input)"));
    assert!(wide.contains("Clear input · interrupt · press twice to quit"));
    assert!(wide.contains("Quit (empty input)"));

    // Narrower terminal: wraps descriptions cleanly instead of clipping, and clamps scroll.
    let narrow_top = render_screen(&mut app, 80, 35);
    assert!(narrow_top.contains("complete values"));
    assert!(narrow_top.contains("to line end"));
    assert!(narrow_top.contains("while the agent works)"));

    for _ in 0..10 {
        app.handle_key(key(KeyCode::PageDown));
    }
    let narrow_bottom = render_screen(&mut app, 80, 35);
    assert!(narrow_bottom.contains("Quit (empty input)"));
    let clamped_scroll = match &app.modal {
        Some(ActiveModal::Help(h)) => h.scroll,
        other => panic!("expected help modal, got {other:?}"),
    };
    assert!(clamped_scroll > 0 && clamped_scroll < 50);
}

fn test_app() -> App {
    let cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let profile = ResolvedAgentProfile {
        id: "mock".into(),
        command: "python3".into(),
        args: Vec::new(),
        env: HashMap::new(),
        available: true,
        default_options_json: None,
        mcp_servers: Vec::new(),
    };
    let (acp_tx, _acp_rx) = tokio::sync::mpsc::unbounded_channel();
    let (msg_tx, _msg_rx) = tokio::sync::mpsc::unbounded_channel();
    App::new(AppInit {
        acp_json_path: std::path::PathBuf::from("acp.json"),
        store: AppStateStore::new(),
        prefs: SavedPrefs::default(),
        theme: Theme::opencode_dark(),
        profiles: vec![profile.clone()],
        profile,
        cwd,
        add_dirs: Vec::new(),
        cli_mode: None,
        cli_model: None,
        initial_prompt: None,
        auto_approve: false,
        acp_tx,
        msg_tx,
    })
}

#[test]
fn drag_select_highlights_and_copies_without_collapsing_tools() {
    use base64::Engine;

    let mut app = test_app();
    app.expand_tools = true;
    app.push_item(TimelineItem::AssistantMessage {
        id: Some("a1".into()),
        text: "Alpha line one\n\nBeta line two".into(),
        timestamp: chrono::Local::now(),
    });
    app.push_item(TimelineItem::ToolCall(ToolCallCard {
        tool_call_id: "read-1".into(),
        title: "src/cli.rs".into(),
        name: Some("Read".into()),
        kind: "read".into(),
        status: "completed".into(),
        locations: Vec::new(),
        diffs: Vec::new(),
        text_outputs: vec!["```\nfirst_tool_line\nsecond_tool_line\n```".into()],
        terminal_ids: Vec::new(),
        raw_input: None,
        raw_output: None,
        started_at: Instant::now(),
        duration_ms: Some(5),
    }));

    let draw_buf = |app: &mut App| -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::render::draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    };
    let row_text = |buf: &ratatui::buffer::Buffer, y: u16| -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect()
    };
    let find_col = |buf: &ratatui::buffer::Buffer, y: u16, needle: &str| -> u16 {
        let chars: Vec<char> = needle.chars().collect();
        (0..=buf.area.width.saturating_sub(chars.len() as u16))
            .find(|&x| {
                chars
                    .iter()
                    .enumerate()
                    .all(|(i, c)| buf[(x + i as u16, y)].symbol() == c.encode_utf8(&mut [0; 4]))
            })
            .unwrap()
    };

    let buf = draw_buf(&mut app);
    let alpha_row = (0..buf.area.height)
        .find(|&y| row_text(&buf, y).contains("Alpha line one"))
        .unwrap();
    let alpha_col = find_col(&buf, alpha_row, "Alpha");
    let beta_row = (0..buf.area.height)
        .find(|&y| row_text(&buf, y).contains("Beta line two"))
        .unwrap();
    let beta_end_col = find_col(&buf, beta_row, "Beta") + 3;

    // Drag across AssistantMessage from "Alpha" down to "Beta".
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: alpha_col,
        row: alpha_row,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: beta_end_col,
        row: beta_row,
        modifiers: KeyModifiers::NONE,
    });

    // While dragging, selected cells are highlighted with theme.selection_bg.
    let mid_buf = draw_buf(&mut app);
    assert_eq!(mid_buf[(alpha_col, alpha_row)].bg, app.theme.selection_bg);
    assert_eq!(mid_buf[(beta_end_col, beta_row)].bg, app.theme.selection_bg);

    // Release copies the selected text automatically and keeps the selection highlighted.
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: beta_end_col,
        row: beta_row,
        modifiers: KeyModifiers::NONE,
    });
    assert!(app.selection.is_some());
    let after_up_buf = draw_buf(&mut app);
    assert_eq!(after_up_buf[(alpha_col, alpha_row)].bg, app.theme.selection_bg);
    assert_eq!(after_up_buf[(beta_end_col, beta_row)].bg, app.theme.selection_bg);
    let b64 = app.pending_osc52.take().expect("selection copied via OSC 52");
    let copied = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap();
    assert_eq!(copied, "Alpha line one\n\n  Beta");
    assert!(app.toasts.iter().any(|t| t.text.contains("Copied")));

    // Ctrl+C while selection is active copies the selection and clears it without touching composer.
    app.composer.set_text("keep draft");
    app.handle_key(ctrl('c'));
    assert!(app.selection.is_none());
    assert_eq!(app.composer.text(), "keep draft");
    let b64 = app.pending_osc52.take().expect("ctrl+c copied active selection");
    let copied = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap();
    assert_eq!(copied, "Alpha line one\n\n  Beta");
    app.composer.clear();

    // Drag inside an expanded tool call: copies text WITHOUT collapsing the tool call!
    let buf2 = draw_buf(&mut app);
    let tool_row = (0..buf2.area.height)
        .find(|&y| row_text(&buf2, y).contains("first_tool_line"))
        .unwrap();
    let tool_start = find_col(&buf2, tool_row, "first_tool_line");
    let tool_end = tool_start + "first_tool_line".len() as u16 - 1;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: tool_start,
        row: tool_row,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: tool_end,
        row: tool_row,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: tool_end,
        row: tool_row,
        modifiers: KeyModifiers::NONE,
    });

    let b64 = app.pending_osc52.take().expect("tool text copied");
    let copied = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap();
    assert_eq!(copied, "first_tool_line");

    let buf3 = draw_buf(&mut app);
    assert!(
        (0..buf3.area.height).any(|y| row_text(&buf3, y).contains("second_tool_line")),
        "tool call must remain expanded after drag selection"
    );
}

#[test]
fn drag_select_works_in_sidebar_and_modals() {
    use base64::Engine;

    let mut app = test_app();
    app.show_sidebar = true;

    let draw_buf = |app: &mut App| -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::render::draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    };
    let row_text = |buf: &ratatui::buffer::Buffer, y: u16| -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect()
    };
    let find_col = |buf: &ratatui::buffer::Buffer, y: u16, needle: &str| -> u16 {
        let chars: Vec<char> = needle.chars().collect();
        (0..=buf.area.width.saturating_sub(chars.len() as u16))
            .find(|&x| {
                chars
                    .iter()
                    .enumerate()
                    .all(|(i, c)| buf[(x + i as u16, y)].symbol() == c.encode_utf8(&mut [0; 4]))
            })
            .unwrap()
    };

    // Select inside the sidebar.
    let buf = draw_buf(&mut app);
    let sr = app.timeline_hit.sidebar_rect.unwrap();
    let y = (sr.y..sr.y + sr.height)
        .find(|&y| row_text(&buf, y).contains("SESSION"))
        .unwrap();
    let x0 = find_col(&buf, y, "SESSION");
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x0,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: x0 + 6,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: x0 + 6,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
    let b64 = app.pending_osc52.take().expect("sidebar text copied");
    let copied = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap();
    assert_eq!(copied, "SESSION");

    // Select inside the Help modal.
    app.handle_key(key(KeyCode::F(1)));
    let buf = draw_buf(&mut app);
    let mr = app.timeline_hit.modal_rect.unwrap();
    let y = (mr.y..mr.y + mr.height)
        .find(|&y| row_text(&buf, y).contains("Composer"))
        .unwrap();
    let x0 = find_col(&buf, y, "Composer");
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x0,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: x0 + 7,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: x0 + 7,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
    let b64 = app.pending_osc52.take().expect("modal text copied");
    let copied = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap();
    assert_eq!(copied, "Composer");
}

#[test]
fn paste_collapses_on_first_paste_and_expands_on_second_paste() {
    let mut app = test_app();

    // Short single-line paste inserts directly without collapsing.
    app.handle_paste("hello world".to_string());
    assert_eq!(app.composer.display_text(), "hello world");
    assert_eq!(app.composer.text(), "hello world");
    app.composer.clear();

    // First multiline paste collapses into a compact pill, while `.text()` keeps the full payload.
    let snippet = "line one\nline two\nline three".to_string();
    app.handle_paste("please check ".to_string());
    app.handle_paste(snippet.clone());
    assert_eq!(app.composer.display_text(), "please check [Pasted ~3 lines]");
    assert_eq!(app.composer.text(), "please check line one\nline two\nline three");
    assert_eq!(app.composer.lines.len(), 1);

    // Rendering the composer displays the pill, not the expanded multiline content.
    let backend = ratatui::backend::TestBackend::new(100, 20);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::render::draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = buf.content().iter().map(|c| c.symbol()).collect();
    assert!(screen.contains("[Pasted ~3 lines]"));
    assert!(!screen.contains("line two"));

    // Second paste of the same snippet expands the pill in place!
    app.handle_paste(snippet.clone());
    assert_eq!(app.composer.display_text(), "please check line one\nline two\nline three");
    assert_eq!(app.composer.text(), "please check line one\nline two\nline three");
    assert_eq!(app.composer.lines.len(), 3);

    // Ctrl+Z undoes the expansion back to the collapsed pill.
    app.handle_key(ctrl('z'));
    assert_eq!(app.composer.display_text(), "please check [Pasted ~3 lines]");

    // Pasting a different multiline snippet inserts a second collapsed pill without expanding the first.
    let other = "other a\nother b".to_string();
    app.handle_paste(other.clone());
    assert_eq!(
        app.composer.display_text(),
        "please check [Pasted ~3 lines][Pasted ~2 lines]"
    );
    assert_eq!(
        app.composer.text(),
        "please check line one\nline two\nline threeother a\nother b"
    );

    // Backspace deletes a collapsed pill atomically in one keypress.
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.composer.display_text(), "please check [Pasted ~3 lines]");

    // Long single-line paste (>150 chars) also collapses on first paste and expands on second.
    app.composer.take_text();
    let long_line = "x".repeat(200);
    app.handle_paste(long_line.clone());
    assert_eq!(app.composer.display_text(), "[Pasted ~200 chars]");
    assert_eq!(app.composer.text(), long_line);
    app.handle_paste(long_line.clone());
    assert_eq!(app.composer.display_text(), long_line);
}

#[test]
fn generation_only_tps_excludes_ttft_and_tool_waits_and_renders_per_task() {
    use crate::app::compute_turn_tps;
    use crate::state::TurnGenTracker;

    // Simulate a turn with:
    // - 1.5s TTFT before first token
    // - 0.5s generation burst (thought + message)
    // - 4.0s tool execution & permission wait (paused)
    // - 1.0s final message generation burst
    let t0 = Instant::now();
    let mut tracker = TurnGenTracker::default();
    let first_tok = t0 + Duration::from_millis(1500);
    tracker.on_chunk(40, first_tok);
    tracker.on_chunk(60, first_tok + Duration::from_millis(250));
    tracker.pause(first_tok + Duration::from_millis(500));

    let after_tool = first_tok + Duration::from_millis(4500);
    tracker.on_chunk(80, after_tool);
    tracker.on_chunk(120, after_tool + Duration::from_millis(600));
    let gen_dur = tracker.finish(after_tool + Duration::from_millis(1000));

    assert_eq!(gen_dur, Duration::from_millis(1500));
    let usage = Usage {
        total_tokens: 500,
        input_tokens: 410,
        output_tokens: 90,
        ..Default::default()
    };
    let (tps, estimated) = compute_turn_tps(gen_dur, tracker.chunk_count, tracker.streamed_chars, Some(&usage)).unwrap();
    assert!(!estimated);
    assert!((tps - 60.0).abs() < 0.01);

    // Without output_tokens in Usage, TPS falls back to ~char/4 estimate.
    let (est_tps, estimated) = compute_turn_tps(gen_dur, tracker.chunk_count, tracker.streamed_chars, None).unwrap();
    assert!(estimated);
    // 300 chars / 4 = 75 tokens over 1.5s = 50.0 tok/s
    assert!((est_tps - 50.0).abs() < 0.01);

    // Sub-20ms IPC bursts do not produce bogus huge TPS values.
    assert!(compute_turn_tps(Duration::from_millis(5), 1, 100, Some(&usage)).is_none());

    // End-of-task TurnSummary renders in the timeline and persists across subsequent tasks.
    let mut app = test_app();
    app.status = crate::state::ConnectionStatus::Connected;
    app.push_item(TimelineItem::AssistantMessage {
        id: Some("a1".into()),
        text: "First reply".into(),
        timestamp: chrono::Local::now(),
    });
    app.push_item(TimelineItem::TurnSummary {
        duration_ms: 6500,
        gen_duration_ms: 1500,
        output_tokens: Some(90),
        total_tokens: Some(500),
        tps: Some(60.0),
        tps_estimated: false,
    });
    app.last_turn_duration = Some(Duration::from_millis(6500));
    app.last_turn_tps = Some((60.0, false));
    app.usage.last_turn_usage = Some(usage);

    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::render::draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("✓ Done in 6s · 60.0 tok/s (90 out in 1.5s)"));
    assert!(screen.contains("✓ Done in 6s · 60.0 tok/s · 500 tokens"));
}

#[test]
fn undo_and_rewind_restore_files_truncate_timeline_and_edit_prompt() {
    use crate::acp::client::AcpEvent;
    use crate::acp::fs_handler::FileWriteRecord;
    use crate::acp::types::{
        ContentBlock, ContentChunk, SessionNotification, SessionUpdate, ToolCallContent,
        ToolCallEvent, ToolCallLocation, ToolCallUpdateEvent,
    };

    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing.txt");
    let created = dir.path().join("created.txt");
    std::fs::write(&existing, "v0\n").unwrap();

    let mut app = test_app();
    app.cwd = dir.path().to_path_buf();
    app.status = crate::state::ConnectionStatus::Connected;

    // Turn 0: edits existing.txt via fs/write_text_file
    app.push_item(TimelineItem::UserMessage {
        id: "u1".into(),
        text: "first prompt".into(),
        attachments: Vec::new(),
        timestamp: chrono::Local::now(),
        steered: false,
    });
    std::fs::write(&existing, "v1\n").unwrap();
    app.handle_acp(AcpEvent::FileWritten(FileWriteRecord {
        path: existing.to_string_lossy().into_owned(),
        old_text: Some("v0\n".into()),
        new_text: "v1\n".into(),
    }));
    app.push_item(TimelineItem::AssistantMessage {
        id: Some("a1".into()),
        text: "First reply".into(),
        timestamp: chrono::Local::now(),
    });

    // Turn 1: edits existing.txt via tool call AND creates created.txt
    app.push_item(TimelineItem::UserMessage {
        id: "u2".into(),
        text: "second prompt".into(),
        attachments: Vec::new(),
        timestamp: chrono::Local::now(),
        steered: false,
    });
    app.session_id = Some("s1".into());
    app.apply_session_update(SessionNotification {
        session_id: "s1".into(),
        update: SessionUpdate::ToolCall(ToolCallEvent {
            tool_call_id: "tc-edit".into(),
            title: "Edit existing.txt".into(),
            name: Some("Edit".into()),
            kind: Some("edit".into()),
            status: Some("pending".into()),
            content: Vec::new(),
            locations: vec![ToolCallLocation {
                path: "existing.txt".into(),
                line: Some(1),
            }],
            raw_input: None,
            raw_output: None,
            meta: None,
        }),
    });
    std::fs::write(&existing, "v2\n").unwrap();
    app.apply_session_update(SessionNotification {
        session_id: "s1".into(),
        update: SessionUpdate::ToolCallUpdate(ToolCallUpdateEvent {
            tool_call_id: "tc-edit".into(),
            title: None,
            name: None,
            kind: None,
            status: Some("completed".into()),
            content: Some(vec![ToolCallContent::Diff {
                path: existing.to_string_lossy().into_owned(),
                old_text: Some("v1\n".into()),
                new_text: "v2\n".into(),
            }]),
            locations: None,
            raw_input: None,
            raw_output: None,
            meta: None,
        }),
    });

    std::fs::write(&created, "brand new\n").unwrap();
    app.handle_acp(AcpEvent::FileWritten(FileWriteRecord {
        path: created.to_string_lossy().into_owned(),
        old_text: None,
        new_text: "brand new\n".into(),
    }));
    app.apply_session_update(SessionNotification {
        session_id: "s1".into(),
        update: SessionUpdate::AgentMessageChunk(ContentChunk {
            content: ContentBlock::text("Second reply"),
            message_id: Some("a2".into()),
            meta: None,
        }),
    });

    assert_eq!(app.modified_files().len(), 2);

    // `/undo` reverts Turn 1, restores existing.txt to v1, deletes created.txt, and loads prompt into composer.
    app.composer.set_text("/undo");
    app.handle_key(key(KeyCode::Enter));

    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "v1\n");
    assert!(!created.exists(), "created.txt should be deleted on undo");
    assert_eq!(app.composer.text(), "second prompt");
    assert_eq!(app.timeline.len(), 2);
    assert_eq!(app.modified_files().len(), 1);

    // User edits the undone prompt, sends Turn 1 again, then opens `/rewind`.
    app.composer.insert_str(" edited");
    assert_eq!(app.composer.text(), "second prompt edited");
    let edited_prompt = app.composer.take_text();
    app.push_item(TimelineItem::UserMessage {
        id: "u2b".into(),
        text: edited_prompt,
        attachments: Vec::new(),
        timestamp: chrono::Local::now(),
        steered: false,
    });
    std::fs::write(&existing, "v2b\n").unwrap();
    app.handle_acp(AcpEvent::FileWritten(FileWriteRecord {
        path: existing.to_string_lossy().into_owned(),
        old_text: Some("v1\n".into()),
        new_text: "v2b\n".into(),
    }));
    app.push_item(TimelineItem::AssistantMessage {
        id: Some("a2b".into()),
        text: "Second reply edited".into(),
        timestamp: chrono::Local::now(),
    });

    app.composer.set_text("/rewind");
    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(app.modal, Some(ActiveModal::Rewind(_))));

    // Modal renders both user turns and file counts.
    let backend = ratatui::backend::TestBackend::new(100, 28);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::render::draw(f, &mut app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("Rewind to message"));
    assert!(screen.contains("second prompt edited"));
    assert!(screen.contains("first prompt"));

    // Select the first prompt (2nd row in reverse-chronological list) and press Enter.
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));

    assert!(app.modal.is_none());
    assert!(app.timeline.is_empty());
    assert!(app.modified_files().is_empty());
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "v0\n");
    assert_eq!(app.composer.text(), "first prompt");

    // Verify ForkSessionRequest serializes `_meta.jetbrains.air.fork` properly.
    let req = ForkSessionRequest {
        session_id: "s1".into(),
        cwd: "/tmp".into(),
        additional_directories: Vec::new(),
        mcp_servers: Vec::new(),
        meta: Some(serde_json::json!({
            "jetbrains": { "air": { "fork": { "version": 1, "messageId": "a1" } } }
        })),
    };
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["_meta"]["jetbrains"]["air"]["fork"]["messageId"], "a1");
}

#[tokio::test(flavor = "multi_thread")]
async fn rewind_preserves_mode_and_model_and_prefs_persist_across_restarts() {
    if crate::config::find_binary_in_path("python3").is_none() {
        return;
    }
    let data_dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("ACP_TUI_DATA_DIR", data_dir.path()) };
    let cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = cwd.join("tests/fixtures/mock_agent.py");
    let profile = ResolvedAgentProfile {
        id: "mock".into(),
        command: "python3".into(),
        args: vec![script.to_string_lossy().into_owned(), format!("{}/", cwd.display())],
        env: HashMap::new(),
        available: true,
        default_options_json: None,
        mcp_servers: Vec::new(),
    };

    async fn pump(
        app: &mut App,
        acp_rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::acp::client::AcpEvent>,
        msg_rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::app::AppMsg>,
        until: impl Fn(&App) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !until(app) {
            assert!(Instant::now() < deadline, "timed out waiting for app state");
            tokio::select! {
                biased;
                Some(ev) = acp_rx.recv() => app.handle_acp(ev),
                Some(m) = msg_rx.recv() => app.handle_msg(m),
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }

    // 1. First run: start with default mode ("code") and model ("m1"), switch to "plan" and "m2".
    let (acp_tx, mut acp_rx) = tokio::sync::mpsc::unbounded_channel();
    let (msg_tx, mut msg_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app1 = App::new(AppInit {
        acp_json_path: std::path::PathBuf::from("acp.json"),
        store: AppStateStore::new(),
        prefs: SavedPrefs::default(),
        theme: Theme::opencode_dark(),
        profiles: vec![profile.clone()],
        profile: profile.clone(),
        cwd: cwd.clone(),
        add_dirs: Vec::new(),
        cli_mode: None,
        cli_model: None,
        initial_prompt: None,
        auto_approve: false,
        acp_tx,
        msg_tx,
    });
    app1.connect(Some(SessionAction::New));
    pump(&mut app1, &mut acp_rx, &mut msg_rx, |a| a.session_id.is_some()).await;
    assert_eq!(app1.current_mode_id().as_deref(), Some("code"));
    assert_eq!(app1.current_model_id().as_deref(), Some("m1"));

    app1.apply_mode("plan");
    app1.apply_model("m2");
    assert_eq!(app1.current_mode_id().as_deref(), Some("plan"));
    assert_eq!(app1.current_model_id().as_deref(), Some("m2"));

    // Send a prompt, then rewind to it via `/rewind`: mode and model must stay "plan" and "m2"!
    app1.push_item(TimelineItem::UserMessage {
        id: "u1".into(),
        text: "turn one".into(),
        attachments: Vec::new(),
        timestamp: chrono::Local::now(),
        steered: false,
    });
    app1.push_item(TimelineItem::AssistantMessage {
        id: Some("a1".into()),
        text: "reply one".into(),
        timestamp: chrono::Local::now(),
    });
    app1.session_has_prompt = true;

    app1.composer.set_text("/rewind");
    app1.handle_key(key(KeyCode::Enter));
    assert!(matches!(app1.modal, Some(ActiveModal::Rewind(_))));
    app1.handle_key(key(KeyCode::Enter));
    pump(&mut app1, &mut acp_rx, &mut msg_rx, |a| !a.opening && a.session_id.is_some()).await;
    assert_eq!(app1.current_mode_id().as_deref(), Some("plan"), "/rewind must preserve mode");
    assert_eq!(app1.current_model_id().as_deref(), Some("m2"), "/rewind must preserve model");

    // Also simulate RewindFork returning `modes: None, models: None` — must not clear mode/model.
    app1.pending_session_overrides = Some(crate::app::SessionOverrides {
        mode: Some("plan".into()),
        model: Some("m2".into()),
        ..Default::default()
    });
    app1.handle_msg(crate::app::AppMsg::SessionOpened(crate::app::SessionOpened {
        kind: crate::app::OpenedKind::Rewound,
        session_id: "s-fork".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        modes: None,
        models: None,
        config_options: Vec::new(),
        fallback_context: None,
    }));
    assert_eq!(app1.current_mode_id().as_deref(), Some("plan"));
    assert_eq!(app1.current_model_id().as_deref(), Some("m2"));

    if let Some(c) = app1.client.clone() {
        c.shutdown().await;
    }

    // 2. Restart CLI with no `--mode`/`--model`: loads saved prefs and keeps "plan" and "m2".
    let store2 = AppStateStore::new();
    let prefs2 = store2.load_prefs();
    assert_eq!(prefs2.last_mode_by_agent.get("mock").map(String::as_str), Some("plan"));
    assert_eq!(prefs2.last_model_by_agent.get("mock").map(String::as_str), Some("m2"));

    let (acp_tx2, mut acp_rx2) = tokio::sync::mpsc::unbounded_channel();
    let (msg_tx2, mut msg_rx2) = tokio::sync::mpsc::unbounded_channel();
    let mut app2 = App::new(AppInit {
        acp_json_path: std::path::PathBuf::from("acp.json"),
        store: store2,
        prefs: prefs2,
        theme: Theme::opencode_dark(),
        profiles: vec![profile.clone()],
        profile: profile.clone(),
        cwd: cwd.clone(),
        add_dirs: Vec::new(),
        cli_mode: None,
        cli_model: None,
        initial_prompt: None,
        auto_approve: false,
        acp_tx: acp_tx2,
        msg_tx: msg_tx2,
    });
    app2.connect(Some(SessionAction::New));
    pump(&mut app2, &mut acp_rx2, &mut msg_rx2, |a| a.session_id.is_some()).await;
    assert_eq!(app2.current_mode_id().as_deref(), Some("plan"), "restarted CLI keeps last mode");
    assert_eq!(app2.current_model_id().as_deref(), Some("m2"), "restarted CLI keeps last model");

    if let Some(c) = app2.client.clone() {
        c.shutdown().await;
    }

    // 3. Explicit `--mode` still overrides saved prefs.
    let store3 = AppStateStore::new();
    let prefs3 = store3.load_prefs();
    let (acp_tx3, mut acp_rx3) = tokio::sync::mpsc::unbounded_channel();
    let (msg_tx3, mut msg_rx3) = tokio::sync::mpsc::unbounded_channel();
    let mut app3 = App::new(AppInit {
        acp_json_path: std::path::PathBuf::from("acp.json"),
        store: store3,
        prefs: prefs3,
        theme: Theme::opencode_dark(),
        profiles: vec![profile.clone()],
        profile,
        cwd,
        add_dirs: Vec::new(),
        cli_mode: Some("code".into()),
        cli_model: None,
        initial_prompt: None,
        auto_approve: false,
        acp_tx: acp_tx3,
        msg_tx: msg_tx3,
    });
    app3.connect(Some(SessionAction::New));
    pump(&mut app3, &mut acp_rx3, &mut msg_rx3, |a| a.session_id.is_some()).await;
    assert_eq!(app3.current_mode_id().as_deref(), Some("code"));
    assert_eq!(app3.current_model_id().as_deref(), Some("m2"));

    if let Some(c) = app3.client.clone() {
        c.shutdown().await;
    }
}
