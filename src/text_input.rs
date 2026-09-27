const MAX_SIZE: usize = 120;

#[derive(Debug, Clone)]
pub struct TextInput {
    chars: Vec<char>,
    cursor: usize,
}

impl TextInput {
    pub fn new() -> Self {
        Self::from("")
    }

    pub fn from(value: &str) -> Self {
        let chars: Vec<char> = value.chars().take(MAX_SIZE).collect();
        let cursor = chars.len();
        Self { chars, cursor }
    }

    pub fn insert(&mut self, character: char) -> bool {
        if self.chars.len() >= MAX_SIZE || character.is_control() {
            return false;
        }
        self.chars.insert(self.cursor, character);
        self.cursor += 1;
        true
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    pub fn value(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn display(&self, show_cursor: bool) -> String {
        if !show_cursor {
            return self.value();
        }
        let mut display = String::with_capacity(self.chars.len() + 1);
        for (index, character) in self.chars.iter().enumerate() {
            if index == self.cursor {
                display.push('▏');
            }
            display.push(*character);
        }
        if self.cursor == self.chars.len() {
            display.push('▏');
        }
        display
    }
}
