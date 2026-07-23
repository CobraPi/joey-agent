//! Minimal multi-line text editor for the TUI input box.
//!
//! Owns no async I/O; the main loop pumps key events into it. Supports
//! insertion, cursor movement (arrows / Home/End / word jumps), backspace,
//! delete, kill-line ops (Ctrl+U/K/W), newline insertion, and multi-line
//! paste.
//!
//! The cursor column is always a **character** index into the line; byte
//! offsets are derived at the edit site so multibyte input (é, emoji, CJK)
//! can never split a UTF-8 boundary.

/// Byte offset of character index `col` in `line` (end of line if past it).
fn byte_idx(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map(|(i, _)| i).unwrap_or(line.len())
}

fn char_len(line: &str) -> usize {
    line.chars().count()
}

#[derive(Clone)]
pub struct Input {
    /// One string per logical line. Invariant: never empty.
    lines: Vec<String>,
    /// Cursor line index.
    cursor_line: usize,
    /// Cursor column as a CHAR index within that line.
    cursor_col: usize,
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Input {
    pub fn new() -> Self {
        Self { lines: vec![String::new()], cursor_line: 0, cursor_col: 0 }
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_line = 0;
        self.cursor_col = 0;
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
        let at = byte_idx(&self.lines[self.cursor_line], self.cursor_col);
        self.lines[self.cursor_line].insert(at, c);
        self.cursor_col += 1;
    }

    /// Insert a whole string at the cursor. Newlines split lines (so pasted
    /// blocks keep their shape); tabs become spaces (a raw \t breaks cell
    /// width accounting).
    pub fn insert_str(&mut self, s: &str) {
        for c in s.replace("\r\n", "\n").chars() {
            match c {
                '\n' | '\r' => self.insert_newline(),
                '\t' => {
                    for _ in 0..4 {
                        self.insert_char(' ');
                    }
                }
                c if c.is_control() => {}
                c => self.insert_char(c),
            }
        }
    }

    /// Insert a newline at the cursor.
    pub fn insert_newline(&mut self) {
        let at = byte_idx(&self.lines[self.cursor_line], self.cursor_col);
        let right = self.lines[self.cursor_line].split_off(at);
        self.lines.insert(self.cursor_line + 1, right);
        self.cursor_line += 1;
        self.cursor_col = 0;
    }

    /// Backspace: deletes char before cursor, joining lines at line start.
    pub fn backspace(&mut self) {
        if self.cursor_col == 0 {
            if self.cursor_line > 0 {
                let moved = self.lines.remove(self.cursor_line);
                self.cursor_line -= 1;
                self.cursor_col = char_len(&self.lines[self.cursor_line]);
                self.lines[self.cursor_line].push_str(&moved);
            }
        } else {
            let line = &mut self.lines[self.cursor_line];
            let at = byte_idx(line, self.cursor_col - 1);
            line.remove(at);
            self.cursor_col -= 1;
        }
    }

    /// Delete forward.
    pub fn delete(&mut self) {
        let len = char_len(&self.lines[self.cursor_line]);
        if self.cursor_col < len {
            let line = &mut self.lines[self.cursor_line];
            let at = byte_idx(line, self.cursor_col);
            line.remove(at);
        } else if self.cursor_line + 1 < self.lines.len() {
            let moved = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&moved);
        }
    }

    /// Kill from the cursor to the end of the line (Ctrl+K).
    pub fn kill_to_end(&mut self) {
        let line = &mut self.lines[self.cursor_line];
        let at = byte_idx(line, self.cursor_col);
        if at < line.len() {
            line.truncate(at);
        } else if self.cursor_line + 1 < self.lines.len() {
            // At line end: kill the newline (join with the next line).
            let moved = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&moved);
        }
    }

    /// Kill from the start of the line to the cursor (Ctrl+U).
    pub fn kill_to_start(&mut self) {
        let line = &mut self.lines[self.cursor_line];
        let at = byte_idx(line, self.cursor_col);
        line.drain(..at);
        self.cursor_col = 0;
    }

    /// Delete the word before the cursor (Ctrl+W / Alt+Backspace).
    pub fn delete_word_back(&mut self) {
        if self.cursor_col == 0 {
            self.backspace();
            return;
        }
        let target = self.word_left_col();
        let line = &mut self.lines[self.cursor_line];
        let from = byte_idx(line, target);
        let to = byte_idx(line, self.cursor_col);
        line.drain(from..to);
        self.cursor_col = target;
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = char_len(&self.lines[self.cursor_line]);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor_col < char_len(&self.lines[self.cursor_line]) {
            self.cursor_col += 1;
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.cursor_col.min(char_len(&self.lines[self.cursor_line]));
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = self.cursor_col.min(char_len(&self.lines[self.cursor_line]));
        }
    }

    pub fn move_line_start(&mut self) {
        self.cursor_col = 0;
    }

    pub fn move_line_end(&mut self) {
        self.cursor_col = char_len(&self.lines[self.cursor_line]);
    }

    /// The column one word to the left of the cursor (whitespace-delimited).
    fn word_left_col(&self) -> usize {
        let chars: Vec<char> = self.lines[self.cursor_line].chars().collect();
        let mut col = self.cursor_col.min(chars.len());
        while col > 0 && chars[col - 1].is_whitespace() {
            col -= 1;
        }
        while col > 0 && !chars[col - 1].is_whitespace() {
            col -= 1;
        }
        col
    }

    /// Move one word left (Ctrl+Left).
    pub fn move_word_left(&mut self) {
        if self.cursor_col == 0 {
            self.move_left();
            return;
        }
        self.cursor_col = self.word_left_col();
    }

    /// Move one word right (Ctrl+Right). Lands at the start of the next word.
    pub fn move_word_right(&mut self) {
        let chars: Vec<char> = self.lines[self.cursor_line].chars().collect();
        let mut col = self.cursor_col.min(chars.len());
        while col < chars.len() && !chars[col].is_whitespace() {
            col += 1;
        }
        while col < chars.len() && chars[col].is_whitespace() {
            col += 1;
        }
        self.cursor_col = col;
    }

    /// (cursor_line, cursor_col) for rendering. `cursor_col` is a char index.
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    /// Compute the visible scroll offset so the cursor stays on screen given
    /// a viewport of `height` rows × `width` content columns.
    /// Returns (first_visible_line, first_visible_char_col).
    pub fn view_offset(&self, height: usize, width: usize) -> (usize, usize) {
        let h = height.max(1);
        let first = self.cursor_line.saturating_sub(h - 1);
        let w = width.max(1);
        let x = if self.cursor_col >= w { self.cursor_col + 1 - w } else { 0 };
        (first, x)
    }

    /// All logical lines.
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

    #[test]
    fn multibyte_editing_never_splits_utf8() {
        let mut i = Input::new();
        // Two consecutive multibyte chars — the old byte-indexed cursor
        // panicked on the second insert.
        i.insert_char('é');
        i.insert_char('ß');
        i.insert_char('!');
        assert_eq!(i.text(), "éß!");
        i.move_left();
        i.insert_char('日');
        assert_eq!(i.text(), "éß日!");
        i.backspace();
        assert_eq!(i.text(), "éß!");
        i.move_left();
        i.delete();
        assert_eq!(i.text(), "é!");
        i.move_line_end();
        assert_eq!(i.cursor(), (0, 2));
    }

    #[test]
    fn kill_ops() {
        let mut i = Input::new();
        i.insert_str("one two three");
        i.delete_word_back();
        assert_eq!(i.text(), "one two ");
        i.kill_to_start();
        assert_eq!(i.text(), "");
        i.insert_str("abcdef");
        i.move_line_start();
        i.move_right();
        i.move_right();
        i.kill_to_end();
        assert_eq!(i.text(), "ab");
    }

    #[test]
    fn paste_multiline_keeps_shape() {
        let mut i = Input::new();
        i.insert_str("fn main() {\r\n\tprintln!(\"hi\");\r\n}");
        assert_eq!(i.line_count(), 3);
        assert_eq!(i.text(), "fn main() {\n    println!(\"hi\");\n}");
    }
}
