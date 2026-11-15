use crate::acp::types::{ContentBlock, EmbeddedResourceContents, PromptCapabilities};
use base64::Engine;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone)]
struct UndoEntry {
    lines: Vec<String>,
    row: usize,
    col: usize,
}

#[derive(Debug, Clone)]
pub struct Composer {
    pub lines: Vec<String>,
    pub row: usize,
    /// Byte offset within `lines[row]`, always at a UTF-8 char boundary.
    pub col: usize,
    undo_stack: Vec<UndoEntry>,
    history: Vec<String>,
    history_index: Option<usize>,
    draft_before_history: Option<String>,
}

impl Composer {
    pub fn new(history: Vec<String>) -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            undo_stack: Vec::new(),
            history,
            history_index: None,
            draft_before_history: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn set_text(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(String::from).collect()
        };
        self.row = self.lines.len().saturating_sub(1);
        self.col = self.lines[self.row].len();
    }

    pub fn take_text(&mut self) -> String {
        let out = self.text();
        self.lines = vec![String::new()];
        self.row = 0;
        self.col = 0;
        self.undo_stack.clear();
        self.history_index = None;
        self.draft_before_history = None;
        out
    }

    pub fn clear(&mut self) {
        if !self.is_empty() {
            self.push_undo();
        }
        self.lines = vec![String::new()];
        self.row = 0;
        self.col = 0;
        self.history_index = None;
        self.draft_before_history = None;
    }

    pub fn record_history(&mut self, submitted: &str) {
        let trimmed = submitted.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.last().map(String::as_str) != Some(trimmed) {
            self.history.push(trimmed.to_string());
        }
        self.history_index = None;
        self.draft_before_history = None;
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    fn push_undo(&mut self) {
        if self.undo_stack.len() >= 80 {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(UndoEntry {
            lines: self.lines.clone(),
            row: self.row,
            col: self.col,
        });
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.lines = prev.lines;
            self.row = prev.row.min(self.lines.len().saturating_sub(1));
            self.col = prev.col.min(self.lines[self.row].len());
        }
    }

    pub fn insert_char(&mut self, ch: char) {
        if ch == '\n' {
            self.insert_newline();
            return;
        }
        self.push_undo();
        self.lines[self.row].insert(self.col, ch);
        self.col += ch.len_utf8();
    }

    pub fn insert_str(&mut self, s: &str) {
        let normalized = s.replace("\r\n", "\n").replace('\r', "\n");
        if normalized.is_empty() {
            return;
        }
        self.push_undo();
        let parts: Vec<&str> = normalized.split('\n').collect();
        if parts.len() == 1 {
            self.lines[self.row].insert_str(self.col, parts[0]);
            self.col += parts[0].len();
            return;
        }

        let tail = self.lines[self.row].split_off(self.col);
        self.lines[self.row].push_str(parts[0]);

        for (i, part) in parts.iter().enumerate().skip(1) {
            self.row += 1;
            if i == parts.len() - 1 {
                let mut last = (*part).to_string();
                self.col = last.len();
                last.push_str(&tail);
                self.lines.insert(self.row, last);
            } else {
                self.lines.insert(self.row, (*part).to_string());
            }
        }
    }

    pub fn insert_newline(&mut self) {
        self.push_undo();
        let tail = self.lines[self.row].split_off(self.col);
        self.row += 1;
        self.col = 0;
        self.lines.insert(self.row, tail);
    }

    /// Backslash + Enter continuation support (`\` at end of line turns Enter into newline).
    pub fn try_backslash_newline(&mut self) -> bool {
        if self.col == self.lines[self.row].len() && self.lines[self.row].ends_with('\\') {
            self.push_undo();
            self.lines[self.row].pop();
            self.col = self.lines[self.row].len();
            let tail = self.lines[self.row].split_off(self.col);
            self.row += 1;
            self.col = 0;
            self.lines.insert(self.row, tail);
            return true;
        }
        false
    }

    pub fn backspace(&mut self) {
        if self.col > 0 {
            self.push_undo();
            let prev = prev_char_boundary(&self.lines[self.row], self.col);
            self.lines[self.row].drain(prev..self.col);
            self.col = prev;
        } else if self.row > 0 {
            self.push_undo();
            let current = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].len();
            self.lines[self.row].push_str(&current);
        }
    }

    pub fn delete_forward(&mut self) {
        let len = self.lines[self.row].len();
        if self.col < len {
            self.push_undo();
            let next = next_char_boundary(&self.lines[self.row], self.col);
            self.lines[self.row].drain(self.col..next);
        } else if self.row + 1 < self.lines.len() {
            self.push_undo();
            let next_line = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next_line);
        }
    }

    pub fn delete_word_back(&mut self) {
        if self.col == 0 {
            self.backspace();
            return;
        }
        self.push_undo();
        let start = word_left_boundary(&self.lines[self.row], self.col);
        self.lines[self.row].drain(start..self.col);
        self.col = start;
    }

    pub fn delete_word_forward(&mut self) {
        if self.col == self.lines[self.row].len() {
            self.delete_forward();
            return;
        }
        self.push_undo();
        let end = word_right_boundary(&self.lines[self.row], self.col);
        self.lines[self.row].drain(self.col..end);
    }

    pub fn kill_to_line_start(&mut self) {
        if self.col > 0 {
            self.push_undo();
            self.lines[self.row].drain(..self.col);
            self.col = 0;
        }
    }

    pub fn kill_to_line_end(&mut self) {
        let len = self.lines[self.row].len();
        if self.col < len {
            self.push_undo();
            self.lines[self.row].truncate(self.col);
        } else if self.row + 1 < self.lines.len() {
            self.delete_forward();
        }
    }

    pub fn move_left(&mut self) {
        if self.col > 0 {
            self.col = prev_char_boundary(&self.lines[self.row], self.col);
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len();
        }
    }

    pub fn move_right(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.col = next_char_boundary(&self.lines[self.row], self.col);
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    pub fn move_word_left(&mut self) {
        if self.col > 0 {
            self.col = word_left_boundary(&self.lines[self.row], self.col);
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len();
        }
    }

    pub fn move_word_right(&mut self) {
        let line = &self.lines[self.row];
        if self.col < line.len() {
            self.col = word_right_boundary(line, self.col);
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    pub fn move_home(&mut self) {
        self.col = 0;
    }

    pub fn move_end(&mut self) {
        self.col = self.lines[self.row].len();
    }

    pub fn move_up_or_history(&mut self) {
        if self.row > 0 {
            let visual_col = UnicodeWidthStr::width(&self.lines[self.row][..self.col]);
            self.row -= 1;
            self.col = byte_col_for_visual_width(&self.lines[self.row], visual_col);
            return;
        }

        // At top row: browse prompt history backward
        if self.history.is_empty() {
            return;
        }
        let next_idx = match self.history_index {
            None => {
                self.draft_before_history = Some(self.text());
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_index = Some(next_idx);
        let entry = self.history[next_idx].clone();
        self.set_text(&entry);
        self.row = 0;
        self.col = self.lines[0].len();
    }

    pub fn move_down_or_history(&mut self) {
        if self.row + 1 < self.lines.len() {
            let visual_col = UnicodeWidthStr::width(&self.lines[self.row][..self.col]);
            self.row += 1;
            self.col = byte_col_for_visual_width(&self.lines[self.row], visual_col);
            return;
        }

        // At bottom row: browse prompt history forward
        if let Some(idx) = self.history_index {
            if idx + 1 < self.history.len() {
                let next = idx + 1;
                self.history_index = Some(next);
                let entry = self.history[next].clone();
                self.set_text(&entry);
            } else {
                self.history_index = None;
                let draft = self.draft_before_history.take().unwrap_or_default();
                self.set_text(&draft);
            }
        }
    }

    /// Returns the active `/command` query when line 0 starts with `/` and cursor is inside the command name.
    pub fn active_slash_query(&self) -> Option<String> {
        if self.row != 0 {
            return None;
        }
        let line = &self.lines[0];
        let trimmed_start = line.trim_start();
        let leading_ws = line.len() - trimmed_start.len();
        if !trimmed_start.starts_with('/') || self.col <= leading_ws {
            return None;
        }
        let after_slash = &line[leading_ws + 1..];
        let cmd_end_rel = after_slash
            .find(char::is_whitespace)
            .unwrap_or(after_slash.len());
        let cmd_end_abs = leading_ws + 1 + cmd_end_rel;
        if self.col <= cmd_end_abs && !line[leading_ws..self.col].contains(char::is_whitespace) {
            Some(line[leading_ws + 1..self.col].to_string())
        } else {
            None
        }
    }

    pub fn complete_slash_command(&mut self, cmd_name: &str, add_trailing_space: bool) {
        let clean = cmd_name.trim_start_matches('/');
        let line = &self.lines[0];
        let trimmed_start = line.trim_start();
        if !trimmed_start.starts_with('/') {
            return;
        }
        let leading_ws = line.len() - trimmed_start.len();
        let after_slash = &line[leading_ws + 1..];
        let cmd_end_rel = after_slash
            .find(char::is_whitespace)
            .unwrap_or(after_slash.len());
        let cmd_end_abs = leading_ws + 1 + cmd_end_rel;
        let line_len = line.len();

        self.push_undo();
        let replacement = if add_trailing_space && cmd_end_abs == line_len {
            format!("/{clean} ")
        } else {
            format!("/{clean}")
        };
        self.lines[0].replace_range(leading_ws..cmd_end_abs, &replacement);
        self.row = 0;
        self.col = leading_ws + replacement.len();
    }

    /// Returns `(command, argument_so_far)` while the cursor is in the argument of a one-line `/command arg`.
    pub fn active_command_arg(&self) -> Option<(String, String)> {
        if self.lines.len() != 1 || self.row != 0 {
            return None;
        }
        let line = &self.lines[0];
        let rest = line.strip_prefix('/')?;
        let (cmd, arg) = rest.split_once(' ')?;
        if cmd.is_empty() || self.col < cmd.len() + 2 {
            return None;
        }
        Some((cmd.to_lowercase(), arg.to_string()))
    }

    pub fn complete_command_arg(&mut self, value: &str) {
        let Some((cmd, _)) = self.active_command_arg() else {
            return;
        };
        self.push_undo();
        self.lines[0] = format!("/{cmd} {value}");
        self.col = self.lines[0].len();
    }

    /// Returns `(start_byte_in_row, query)` if cursor is actively typing an `@file` reference.
    pub fn active_at_mention(&self) -> Option<(usize, String)> {
        let line = &self.lines[self.row];
        let prefix = &line[..self.col];
        let at_pos = prefix.rfind('@')?;
        // `@` must be at start of line or preceded by whitespace
        if at_pos > 0 {
            let prev_ch = prefix[..at_pos].chars().next_back()?;
            if !prev_ch.is_whitespace() {
                return None;
            }
        }
        let query = &prefix[at_pos + 1..];
        if query.contains(char::is_whitespace) {
            return None;
        }
        Some((at_pos, query.to_string()))
    }

    pub fn complete_at_mention(&mut self, rel_path: &str) {
        let Some((at_pos, _)) = self.active_at_mention() else {
            return;
        };
        let line = &self.lines[self.row];
        let tail = &line[self.col..];
        let end_rel = tail.find(char::is_whitespace).unwrap_or(tail.len());
        let end_abs = self.col + end_rel;

        self.push_undo();
        let replacement = format!("@{rel_path} ");
        self.lines[self.row].replace_range(at_pos..end_abs, &replacement);
        self.col = at_pos + replacement.len();
    }
}

fn prev_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.saturating_sub(1);
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = (idx + 1).min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn word_left_boundary(s: &str, idx: usize) -> usize {
    let chars: Vec<(usize, char)> = s[..idx].char_indices().collect();
    let mut k = chars.len();
    while k > 0 && chars[k - 1].1.is_whitespace() {
        k -= 1;
    }
    while k > 0 && !chars[k - 1].1.is_whitespace() {
        k -= 1;
    }
    chars.get(k).map(|(b, _)| *b).unwrap_or(0)
}

fn word_right_boundary(s: &str, idx: usize) -> usize {
    let chars: Vec<(usize, char)> = s[idx..].char_indices().collect();
    let mut k = 0;
    while k < chars.len() && !chars[k].1.is_whitespace() {
        k += 1;
    }
    while k < chars.len() && chars[k].1.is_whitespace() {
        k += 1;
    }
    chars.get(k).map(|(b, _)| idx + *b).unwrap_or(s.len())
}

fn byte_col_for_visual_width(s: &str, target_w: usize) -> usize {
    let mut w = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        let cw = UnicodeWidthStr::width(ch.encode_utf8(&mut [0; 4]));
        if w + cw > target_w {
            return byte_idx;
        }
        w += cw;
    }
    s.len()
}

// ============================================================================
// Fuzzy Scorer & Workspace File Indexer (`@file` Picker)
// ============================================================================

pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let q_lower: Vec<char> = query.to_lowercase().chars().collect();
    let c_lower: Vec<char> = candidate.to_lowercase().chars().collect();

    // Fast exact substring bonus
    let q_str: String = q_lower.iter().collect();
    let c_str: String = c_lower.iter().collect();
    let basename = candidate.rsplit('/').next().unwrap_or(candidate).to_lowercase();

    if basename == q_str {
        return Some(10_000);
    }
    if basename.starts_with(&q_str) {
        return Some(8_000 - basename.len() as i64);
    }
    if let Some(pos) = basename.find(&q_str) {
        return Some(6_000 - (pos as i64 * 10) - candidate.len() as i64);
    }
    if let Some(pos) = c_str.find(&q_str) {
        return Some(4_000 - (pos as i64 * 5) - candidate.len() as i64);
    }

    // Subsequence match with consecutive & path-separator bonuses
    let mut qi = 0usize;
    let mut score = 0i64;
    let mut prev_match_idx: Option<usize> = None;

    for (ci, &ch) in c_lower.iter().enumerate() {
        if qi < q_lower.len() && ch == q_lower[qi] {
            score += 20;
            if prev_match_idx == Some(ci.saturating_sub(1)) {
                score += 25;
            }
            if ci == 0 || matches!(c_lower.get(ci.saturating_sub(1)), Some('/' | '_' | '-' | '.')) {
                score += 30;
            }
            prev_match_idx = Some(ci);
            qi += 1;
        }
    }

    if qi == q_lower.len() {
        Some(score - candidate.len() as i64)
    } else {
        None
    }
}

/// Ranks candidates for a query. Exact, prefix, word-prefix and substring matches come first;
/// loose subsequence matches are used only when nothing matches better.
pub fn rank_candidates<S: AsRef<str>>(query: &str, candidates: &[S]) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..candidates.len()).collect();
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut scored: Vec<(u8, i64, usize)> = Vec::new();
    for (i, c) in candidates.iter().enumerate() {
        let cl = c.as_ref().to_lowercase();
        let tier = if cl == q {
            Some((0, 0))
        } else if cl.starts_with(&q) {
            Some((1, cl.len() as i64))
        } else if let Some(pos) = cl
            .match_indices(|ch: char| !ch.is_alphanumeric())
            .map(|(p, sep)| p + sep.len())
            .find(|&p| cl[p..].starts_with(&q))
        {
            Some((2, pos as i64))
        } else if let Some(pos) = cl.find(&q) {
            Some((3, pos as i64))
        } else if words.len() > 1 && words.iter().all(|w| cl.contains(w)) {
            Some((3, cl.len() as i64))
        } else {
            fuzzy_score(&q, &cl).map(|s| (4, -s))
        };
        if let Some((t, k)) = tier {
            scored.push((t, k, i));
        }
    }
    let has_strong = scored.iter().any(|(t, _, _)| *t < 4);
    if has_strong {
        scored.retain(|(t, _, _)| *t < 4);
    }
    scored.sort();
    scored.into_iter().map(|(_, _, i)| i).collect()
}

pub fn scan_workspace_files(cwd: &Path, max_files: usize) -> Vec<String> {
    let mut files = Vec::new();
    let walker = WalkBuilder::new(cwd)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                ".git" | "node_modules" | "target" | ".idea" | ".next" | "dist" | "__pycache__"
            )
        })
        .build();

    for entry in walker.flatten() {
        if files.len() >= max_files {
            break;
        }
        if entry.file_type().is_some_and(|ft| ft.is_file())
            && let Ok(rel) = entry.path().strip_prefix(cwd)
        {
            let s = rel.to_string_lossy().into_owned();
            if !s.is_empty() {
                files.push(s);
            }
        }
    }
    files.sort();
    files
}

#[derive(Debug, Clone)]
pub struct ResolvedPromptPayload {
    pub blocks: Vec<ContentBlock>,
    pub attached_labels: Vec<String>,
}

fn image_mime_for_ext(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn parse_mention_token(token: &str) -> (&str, Option<(usize, Option<usize>)>) {
    // Supports `@src/main.rs` or `@src/main.rs:10-30` or `@src/main.rs:10`
    if let Some((path_part, range_part)) = token.rsplit_once(':') {
        if let Some((start_s, end_s)) = range_part.split_once('-') {
            if let (Ok(s), Ok(e)) = (start_s.parse::<usize>(), end_s.parse::<usize>()) {
                return (path_part, Some((s, Some(e))));
            }
        } else if let Ok(s) = range_part.parse::<usize>() {
            return (path_part, Some((s, None)));
        }
    }
    (token, None)
}

/// Build ACP `ContentBlock`s from user prompt text, resolving `@file` and `@image` mentions!
pub fn build_prompt_blocks(
    raw_text: &str,
    cwd: &Path,
    caps: &PromptCapabilities,
) -> ResolvedPromptPayload {
    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut attached_labels: Vec<String> = Vec::new();
    let mut seen_paths: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for word in raw_text.split_whitespace() {
        let Some(after_at) = word.strip_prefix('@') else {
            continue;
        };
        let trimmed = after_at.trim_end_matches(&[',', ';', ')', ']'][..]);
        if trimmed.is_empty() {
            continue;
        }
        let (rel_str, line_range) = parse_mention_token(trimmed);
        let full_path = if Path::new(rel_str).is_absolute() {
            PathBuf::from(rel_str)
        } else {
            cwd.join(rel_str)
        };
        if !full_path.is_file() || !seen_paths.insert(full_path.clone()) {
            continue;
        }

        let uri = format!("file://{}", full_path.display());

        // 1. Image attachment
        if let Some(mime) = image_mime_for_ext(&full_path)
            && caps.image
            && let Ok(bytes) = std::fs::read(&full_path)
        {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            blocks.push(ContentBlock::Image {
                data: b64,
                mime_type: mime.to_string(),
                uri: Some(uri),
            });
            attached_labels.push(format!("🖼 {rel_str}"));
            continue;
        }

        // 2. Text file attachment (Embedded Resource if supported, else ResourceLink)
        if let Ok(content) = std::fs::read_to_string(&full_path) {
            let sliced = if let Some((start, end_opt)) = line_range {
                let all_lines: Vec<&str> = content.lines().collect();
                let s_idx = start.saturating_sub(1).min(all_lines.len());
                let e_idx = end_opt
                    .unwrap_or(start)
                    .min(all_lines.len())
                    .max(s_idx);
                all_lines[s_idx..e_idx].join("\n")
            } else {
                content
            };

            if caps.embedded_context {
                blocks.push(ContentBlock::Resource {
                    resource: EmbeddedResourceContents {
                        uri,
                        mime_type: Some("text/plain".to_string()),
                        text: Some(sliced),
                        blob: None,
                    },
                });
            } else {
                blocks.push(ContentBlock::ResourceLink {
                    uri,
                    name: rel_str.to_string(),
                    title: Some(rel_str.to_string()),
                    description: None,
                    mime_type: Some("text/plain".to_string()),
                });
            }
            attached_labels.push(format!("📄 {trimmed}"));
        }
    }

    blocks.insert(0, ContentBlock::text(raw_text));
    ResolvedPromptPayload {
        blocks,
        attached_labels,
    }
}
