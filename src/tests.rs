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
    let left_click = |col: u16, row: u16| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
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
    app.handle_mouse(left_click(6, t1_row));
    let rows = render_rows(&mut app);
    let joined = rows.join("\n");
    assert!(joined.contains("Hidden finished detail alpha"));
    assert!(!joined.contains("Hidden finished detail beta"));

    // Click the READ tool step: expands its file contents.
    let read_row = rows
        .iter()
        .position(|r| r.contains("src/cli.rs"))
        .unwrap() as u16;
    app.handle_mouse(left_click(6, read_row));
    let rows = render_rows(&mut app);
    let joined = rows.join("\n");
    assert!(joined.contains("read-secret-line-one"));
    assert!(joined.contains("read-secret-line-two"));

    // Click the READ tool step again: collapses back to summary.
    let read_row = rows
        .iter()
        .position(|r| r.contains("src/cli.rs"))
        .unwrap() as u16;
    app.handle_mouse(left_click(6, read_row));
    let rows = render_rows(&mut app);
    assert!(!rows.join("\n").contains("read-secret-line-one"));

    // Click the current (live) thinking step: expands all lines while still streaming.
    let live_row = rows
        .iter()
        .position(|r| r.contains("Thinking…") && r.contains("click to expand"))
        .unwrap() as u16;
    app.handle_mouse(left_click(6, live_row));
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
    app.handle_mouse(left_click(6, live_row));
    let rows = render_rows(&mut app);
    let joined = rows.join("\n");
    assert!(!joined.contains("live-thought-line-1"));
    assert!(joined.contains("live-thought-line-7"));

    // Clicking the busy activity bar also toggles the active thinking step.
    let activity_y = app.timeline_hit.activity_rect.y;
    app.handle_mouse(left_click(4, activity_y));
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
