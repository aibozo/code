use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap, WidgetRef};

use super::bottom_pane_view::{BottomPaneView, ConditionalUpdate};
use super::BottomPane;
use crate::app_event_sender::AppEventSender;
use crate::slash_command::SlashCommand;

pub(crate) struct AgentStatusView {
    lines: Vec<Line<'static>>,
    scroll: u16,
    app_event_tx: AppEventSender,
    complete: bool,
}

impl AgentStatusView {
    pub fn new(lines: Vec<Line<'static>>, app_event_tx: AppEventSender) -> Self {
        Self { lines, scroll: 0, app_event_tx, complete: false }
    }

    fn parse_agent_update_and_build_lines(text: &str) -> Option<Vec<Line<'static>>> {
        if !text.starts_with("[AGENTS]") { return None; }
        let payload = text.trim_start_matches("[AGENTS]");
        let val: serde_json::Value = serde_json::from_str(payload).ok()?;
        let mut out: Vec<Line<'static>> = Vec::new();
        use ratatui::text::Span;
        let header = "Agent Status".to_string();
        out.push(Line::from(header));
        out.push(Line::from(""));
        if let Some(ctx) = val.get("context").and_then(|v| v.as_str()) {
            if !ctx.is_empty() { out.push(Line::from(format!("context: {}", ctx))); }
        }
        if let Some(task) = val.get("task").and_then(|v| v.as_str()) {
            if !task.is_empty() { out.push(Line::from(format!("task: {}", task))); }
        }
        if let Some(arr) = val.get("agents").and_then(|v| v.as_array()) {
            let mut running = 0usize; let mut pending = 0usize; let mut completed = 0usize; let mut failed = 0usize;
            for a in arr { if let Some(s) = a.get("status").and_then(|v| v.as_str()) {
                match s { "running" => running+=1, "pending" => pending+=1, "completed" => completed+=1, "failed" => failed+=1, _=>{} }
            }}
            out.push(Line::from(format!("summary: running {} • pending {} • completed {} • failed {}", running, pending, completed, failed)));
            out.push(Line::from(""));
            for a in arr {
                let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let status = a.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let model = a.get("model").and_then(|v| v.as_str()).unwrap_or("");
                let elapsed = {
                    if let (Some(st), Some(cm)) = (
                        a.get("started_at").and_then(|v| v.as_str()),
                        a.get("completed_at").and_then(|v| v.as_str()),
                    ) {
                        if let (Ok(stdt), Ok(cmdt)) = (chrono::DateTime::parse_from_rfc3339(st), chrono::DateTime::parse_from_rfc3339(cm)) {
                            let secs = (cmdt - stdt).num_seconds().max(0);
                            format!("{}s", secs)
                        } else { String::new() }
                    } else if let Some(st) = a.get("started_at").and_then(|v| v.as_str()) {
                        if let Ok(stdt) = chrono::DateTime::parse_from_rfc3339(st) {
                            let secs = (chrono::Utc::now() - stdt.with_timezone(&chrono::Utc)).num_seconds().max(0);
                            format!("{}s", secs)
                        } else { String::new() }
                    } else { String::new() }
                };
                let mut line = String::new();
                line.push_str(&format!("- [{}] {}", status, name));
                if !model.is_empty() { line.push_str(&format!("  •  model {}", model)); }
                if !elapsed.is_empty() { line.push_str(&format!("  •  elapsed {}", elapsed)); }
                out.push(Line::from(line));

                if let Some(err) = a.get("error").and_then(|v| v.as_str()) {
                    let s = err.trim(); if !s.is_empty() { out.push(Line::from(format!("    error: {}", s))); }
                }
                if let Some(wt) = a.get("worktree_path").and_then(|v| v.as_str()) {
                    let br = a.get("branch_name").and_then(|v| v.as_str()).unwrap_or("");
                    if !wt.is_empty() {
                        let brs = if br.is_empty() { String::new() } else { format!(" ({})", br) };
                        out.push(Line::from(format!("    worktree: {}{}", wt, brs)));
                    }
                }
                if let Some(tail) = a.get("progress_tail").and_then(|v| v.as_array()) {
                    for p in tail { if let Some(t) = p.as_str() { out.push(Line::from(format!("    · {}", t))); } }
                }
            }
        } else {
            out.push(Line::from("no agents"));
        }
        Some(out)
    }
}

impl BottomPaneView<'_> for AgentStatusView {
    fn desired_height(&self, _width: u16) -> u16 { 12 }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 { return; }
        // Reserve 1 row for footer hints
        let content_h = area.height.saturating_sub(1);
        let content_area = Rect { x: area.x, y: area.y, width: area.width, height: content_h };

        // Render a scrolled paragraph of the agent status
        let mut lines = self.lines.clone();
        let skip = self.scroll as usize;
        if skip < lines.len() { lines.drain(0..skip); } else { lines.clear(); }
        Paragraph::new(lines).wrap(Wrap { trim: false }).render_ref(content_area, buf);

        // Footer hint line
        let footer_area = Rect { x: area.x, y: area.y.saturating_add(content_h), width: area.width, height: 1 };
        let hint = "↑/↓/PgUp/PgDn scroll   •   r: reports  •   t: trends  •   e: episodes  •   Esc: close";
        Paragraph::new(Line::from(hint)).wrap(Wrap { trim: false }).render_ref(footer_area, buf);
    }

    fn handle_key_event(&mut self, pane: &mut BottomPane<'_>, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyEventKind};
        if key.kind != KeyEventKind::Press { return; }
        match key.code {
            KeyCode::Up => { self.scroll = self.scroll.saturating_sub(1); }
            KeyCode::Down => { self.scroll = self.scroll.saturating_add(1); }
            KeyCode::PageUp => { self.scroll = self.scroll.saturating_sub(5); }
            KeyCode::PageDown => { self.scroll = self.scroll.saturating_add(5); }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                pane.app_event_tx.send(crate::app_event::AppEvent::DispatchCommand(SlashCommand::Reports, "/reports".to_string()));
            }
            KeyCode::Char('t') | KeyCode::Char('T') => {
                pane.app_event_tx.send(crate::app_event::AppEvent::DispatchCommand(SlashCommand::Reports, "/reports trends 7".to_string()));
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                pane.app_event_tx.send(crate::app_event::AppEvent::DispatchCommand(SlashCommand::Memory, "/memory peek-episodes 20".to_string()));
            }
            KeyCode::Esc => { self.complete = true; }
            _ => {}
        }
    }

    fn update_status_text(&mut self, text: String) -> ConditionalUpdate {
        if let Some(lines) = Self::parse_agent_update_and_build_lines(&text) {
            self.lines = lines;
            ConditionalUpdate::NeedsRedraw
        } else {
            ConditionalUpdate::NoRedraw
        }
    }

    fn is_complete(&self) -> bool { self.complete }
}
