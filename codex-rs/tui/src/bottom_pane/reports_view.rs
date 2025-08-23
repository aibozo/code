use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Wrap, WidgetRef};
use ratatui::text::Line;

use super::bottom_pane_view::{BottomPaneView, ConditionalUpdate};
use super::BottomPane;

pub(crate) struct ReportsView {
    lines: Vec<Line<'static>>,
    scroll: u16,
}

impl ReportsView {
    pub fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines, scroll: 0 }
    }
}

impl BottomPaneView<'_> for ReportsView {
    fn desired_height(&self, width: u16) -> u16 {
        // Attempt to fit ~10 rows by default, but this is advisory; the pane layout will clamp.
        let wrap = Paragraph::new(self.lines.clone()).wrap(Wrap { trim: false });
        // Rough estimate based on width to avoid expensive layout; allow 10.
        let _ = width; // unused in simple estimation
        10
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        // Render a scrolled paragraph of the report
        let mut lines = self.lines.clone();
        // Manual scroll by removing top lines (simple; adequate for now)
        let skip = self.scroll as usize;
        if skip < lines.len() {
            lines.drain(0..skip);
        } else {
            lines.clear();
        }
        Paragraph::new(lines).wrap(Wrap { trim: false }).render_ref(area, buf);
    }

    fn handle_key_event(&mut self, _pane: &mut BottomPane<'_>, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyEventKind};
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(5);
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(5);
            }
            _ => {}
        }
    }

    fn update_status_text(&mut self, _text: String) -> ConditionalUpdate {
        ConditionalUpdate::NoRedraw
    }
}

