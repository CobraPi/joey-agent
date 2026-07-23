//! Minimal multi-line text editor for the TUI input box.
//!
//! Owns no async I/O; the main loop pumps key events into it. Supports
//! insertion, cursor movement (arrows / Home/End / word jumps), backspace,
//! delete, newline insertion on Alt+Enter, and a visible block cursor.

use ratatui::layout::Rect;

#[derive(Clone, Default)]
pub struct Input {
    /// One string per logical line.
    lines: Vec<String>,
    /// Cursor: (line index, char index within that line).
    cursor_line: usize,
    cursor_col: usize,
    /// Char index in the flattened buffer (cache for rendering offset).
    pub dirty: bool,
}

impl Input {
    pub fn new() -> Self {
        Self { lines: vec![String::new()], cursor_line: 0, cursor_col: 0, dirty: true }
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.dirty = true;
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn cursor_line(&self) -> usize {
        self.cursor_line
    }

    /// Insert a character at the cursor.
    pub fn insert_char(&mut self, c: char) {
        self.lines[self.cursor_line].insert(self.cursor_col, c);
        self.cursor_col += 1;
        self.dirty = true;
    }

    /// Convenience: insert a whole string at the cursor.
    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            self.insert_char(c);
        }
    }

    /// Insert a newline (Alt+Enter).
    pub fn insert_newline(&mut self) {
        let right: String = self.lines[self.cursor_line].split_off(self.cursor_col);
        self.lines.insert(self.cursor_line + 1, right);
        self.cursor_line += 1;
        self.cursor_col = 0;
        self.dirty = true;
    }

    /// Backspace: deletes char before cursor, joining lines at line start.
    pub fn backspace(&mut self) {
        if self.cursor_col == 0 {
            if self.cursor_line > 0 {
                let moved = self.lines.remove(self.cursor_line);
                self.cursor_line -= 1;
                self.cursor_col = self.lines[self.cursor_line].len();
                self.lines[self.cursor_line].push_str(&moved);
            }
        } else {
            self.lines[self.cursor_line].remove(self.cursor_col - 1);
            self.cursor_col -= 1;
        }
        self.dirty = true;
    }

    /// Delete forward.
    pub fn delete(&mut self) {
        if self.cursor_col < self.lines[self.cursor_line].len() {
            self.lines[self.cursor_line].remove(self.cursor_col);
        } else if self.cursor_line + 1 < self.lines.len() {
            let moved = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&moved);
        }
        self.dirty = true;
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor_col < self.lines[self.cursor_line].len() {
            self.cursor_col += 1;
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_line].len());
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_line].len());
        }
    }

    pub fn move_line_start(&mut self) {
        self.cursor_col = 0;
    }

    pub fn move_line_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_line].len();
    }

    /// Move one word left (Ctrl+Left).
    pub fn move_word_left(&mut self) {
        if self.cursor_col == 0 {
            self.move_left();
            return;
        }
        let line = &self.lines[self.cursor_line];
        let mut col = self.cursor_col;
        let chars: Vec<char> = line.chars().collect();
        // skip whitespace
        while col > 0 && chars[col - 1].is_whitespace() {
            col -= 1;
        }
        // skip word
        while col > 0 && !chars[col - 1].is_whitespace() {
            col -= 1;
        }
        self.cursor_col = col;
    }

    /// Move one word right (Ctrl+Right). Lands at the start of the next word.
    pub fn move_word_right(&mut self) {
        let line = &self.lines[self.cursor_line];
        let chars: Vec<char> = line.chars().collect();
        let mut col = self.cursor_col;
        // skip the rest of the current word
        while col < chars.len() && !chars[col].is_whitespace() {
            col += 1;
        }
        // skip whitespace to land at the next word
        while col < chars.len() && chars[col].is_whitespace() {
            col += 1;
        }
        self.cursor_col = col;
    }

    /// (cursor_line, cursor_col) for rendering.
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    /// Compute the visible scroll offset so the cursor stays on screen.
    /// Returns (first_visible_line, cursor_x_in_visible).
    pub fn view_offset(&self, area: Rect) -> (usize, usize) {
        let h = area.height.max(1) as usize;
        // Keep the cursor line within the visible window.
        let first = if self.cursor_line >= h {
            self.cursor_line - h + 1
        } else {
            0
        };
        // Horizontal offset: simple char-based within the line width.
        let w = area.width.saturating_sub(2) as usize; // leave 1 for padding
        let x = if self.cursor_col > w { self.cursor_col - w } else { 0 };
        (first, x)
    }

    /// Iterate visible lines (line index, text).
    pub fn lines(&self) -> &[String] {
        &self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_and_enter() {
        let mut i = Input::new();
        i.insert_str("hello world");
        assert_eq!(i.text(), "hello world");
    }

    #[test]
    fn multiline() {
        let mut i = Input::new();
        i.insert_str("foo");
        i.insert_newline();
        i.insert_str("bar");
        assert_eq!(i.text(), "foo\nbar");
        assert_eq!(i.line_count(), 2);
        i.move_up();
        let (l, _c) = i.cursor();
        assert_eq!(l, 0);
    }

    #[test]
    fn backspace_join() {
        let mut i = Input::new();
        i.insert_str("ab");
        i.insert_newline();
        i.insert_str("cd");
        // cursor on line 1 col 2
        i.backspace(); // delete 'd'
        assert_eq!(i.text(), "ab\nc");
        i.move_line_start();
        i.backspace(); // join
        assert_eq!(i.text(), "abc");
    }

    #[test]
    fn word_motion() {
        let mut i = Input::new();
        i.insert_str("alpha beta gamma");
        i.move_word_left();
        let (l, c) = i.cursor();
        assert_eq!((l, c), (0, 11)); // start of 'gamma'
        i.move_word_left();
        let (_, c) = i.cursor();
        assert_eq!(c, 6); // start of 'beta'
        i.move_word_right();
        let (_, c) = i.cursor();
        assert_eq!(c, 11); // start of 'gamma'
    }
}
