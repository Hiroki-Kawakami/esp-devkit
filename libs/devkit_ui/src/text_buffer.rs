/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;

/// What reaches a focused text input, whichever device it came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextInput {
    Char(char),
    Backspace,
    Delete,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    /// A tap at this byte offset of the text.
    Place(u32),
    /// A `KeyAction::Custom` of the on-screen keyboard.
    Custom(u16),
}

/// Text being edited. The owner keeps it in its state and applies what the
/// input reports.
#[derive(Clone, Default, Debug)]
pub struct TextBuffer {
    text: String,
    cursor: usize,
    composing: Option<(usize, usize)>,
}

impl TextBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Byte offset of the cursor.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.cursor = text.len();
        self.composing = None;
    }

    /// The range an input method has not committed yet, drawn underlined.
    pub fn composing(&self) -> Option<(usize, usize)> {
        self.composing
    }

    pub fn set_composing(&mut self, range: Option<(usize, usize)>) {
        self.composing = range;
    }

    fn previous(&self, from: usize) -> usize {
        self.text[..from].char_indices().next_back().map_or(0, |(i, _)| i)
    }

    fn next(&self, from: usize) -> usize {
        self.text[from..].chars().next().map_or(from, |c| from + c.len_utf8())
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..].find('\n').map_or(self.text.len(), |i| at + i)
    }

    /// Moves along lines by byte column; `Up` and `Down` keep to the logical
    /// lines of the text.
    fn vertical(&mut self, up: bool) {
        let start = self.line_start(self.cursor);
        let column = self.cursor - start;
        let target = if up {
            if start == 0 {
                return;
            }
            self.line_start(start - 1)
        } else {
            let end = self.line_end(self.cursor);
            if end == self.text.len() {
                return;
            }
            end + 1
        };
        let end = self.line_end(target);
        let mut at = (target + column).min(end);
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        self.cursor = at;
    }

    /// `true` when the text changed. `Enter` and `Custom` are for the owner:
    /// it decides whether they insert, submit or do something else.
    pub fn apply(&mut self, input: TextInput) -> bool {
        match input {
            TextInput::Char(c) => {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                true
            }
            TextInput::Backspace if self.cursor > 0 => {
                let start = self.previous(self.cursor);
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
                true
            }
            TextInput::Delete if self.cursor < self.text.len() => {
                let end = self.next(self.cursor);
                self.text.replace_range(self.cursor..end, "");
                true
            }
            TextInput::Left => {
                self.cursor = self.previous(self.cursor);
                false
            }
            TextInput::Right => {
                self.cursor = self.next(self.cursor);
                false
            }
            TextInput::Up | TextInput::Down => {
                self.vertical(input == TextInput::Up);
                false
            }
            TextInput::Home => {
                self.cursor = self.line_start(self.cursor);
                false
            }
            TextInput::End => {
                self.cursor = self.line_end(self.cursor);
                false
            }
            TextInput::Place(byte) => {
                let mut at = (byte as usize).min(self.text.len());
                while !self.text.is_char_boundary(at) {
                    at -= 1;
                }
                self.cursor = at;
                false
            }
            _ => false,
        }
    }
}
