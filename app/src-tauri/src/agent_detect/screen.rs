//! What a chat's terminal shows now, as text: a small screen model fed the agent's output, for the rules in
//! [`super::manifest`]. Marv's own (herdr reads Ghostty's screen); not a terminal for people to look at (the page's
//! xterm.js is that): no colours, no scrollback, just the characters where the agent last put them.
//!
//! - The rules read the visible screen, not the output stream: Claude Code, Codex and OpenCode redraw in place
//!   (cursor moves, erases, an alternate screen), so the stream with its escapes stripped holds every spinner frame
//!   and every old copy of a footer. A raw tail would see a prompt that's long gone.
//! - What it understands is what the agents' TUIs use: printing with wrap, CR/LF/BS/TAB, cursor moves and positions,
//!   erases, insert/delete of lines and characters, scroll regions, saved cursors, the alternate screen, and the OSC
//!   title (0, 2) and progress (9;4) the rules may name. Anything else (colours, modes, DCS strings) is skipped.
//! - Wide characters (CJK, most emoji) take two cells, as in the person's terminal, so later columns line up.

use unicode_width::UnicodeWidthChar;

/// The longest OSC string kept (a title, a progress report); longer ones are cut.
const MAX_OSC: usize = 4096;

/// A cell: its character, or the right half of a wide one before it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Cell {
    Char(char),
    WideTail,
}

const BLANK: Cell = Cell::Char(' ');

#[derive(Debug, Clone, Copy, Default)]
struct Cursor {
    row: usize,
    col: usize,
}

/// Where the parser is between two bytes of output.
#[derive(Debug, Default)]
enum Parse {
    #[default]
    Ground,
    Escape,
    /// `ESC (` and kin: one more character names a charset, skipped.
    Charset,
    Csi(String),
    Osc(String),
    /// OSC, an ESC seen: a `\` ends it.
    OscEscape(String),
    /// DCS, SOS, PM, APC: skipped to the string terminator.
    Ignored,
    IgnoredEscape,
}

/// One terminal screen, as an agent drew it.
pub struct Screen {
    rows: usize,
    cols: usize,
    grid: Vec<Vec<Cell>>,
    /// The main screen while the alternate one shows, with its cursor.
    saved_main: Option<(Vec<Vec<Cell>>, Cursor)>,
    cursor: Cursor,
    saved: Cursor,
    /// At the last column, a character printed: the next one starts the next line (xterm's deferred wrap).
    wrap_pending: bool,
    autowrap: bool,
    /// The scroll region's top and bottom rows (inclusive).
    top: usize,
    bottom: usize,
    last_char: Option<char>,
    parse: Parse,
    /// A UTF-8 character cut between two reads.
    partial: Vec<u8>,
    title: String,
    progress: String,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = (usize::from(cols.max(1)), usize::from(rows.max(1)));
        Screen {
            rows,
            cols,
            grid: vec![vec![BLANK; cols]; rows],
            saved_main: None,
            cursor: Cursor::default(),
            saved: Cursor::default(),
            wrap_pending: false,
            autowrap: true,
            top: 0,
            bottom: rows - 1,
            last_char: None,
            parse: Parse::Ground,
            partial: Vec::new(),
            title: String::new(),
            progress: String::new(),
        }
    }

    /// The window title the agent last set (OSC 0 or 2).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Its last progress report (OSC 9;4…), without the `9;`: `4;1;50`, `4;0;`.
    pub fn progress(&self) -> &str {
        &self.progress
    }

    /// The visible screen as text: each row with its trailing spaces trimmed, trailing blank rows left out, every
    /// line ending in `\n` (herdr's detection text).
    pub fn text(&self) -> String {
        let mut lines: Vec<String> = self
            .grid
            .iter()
            .map(|row| {
                let line: String = row.iter().filter_map(|c| if let Cell::Char(ch) = c { Some(*ch) } else { None }).collect();
                line.trim_end().to_string()
            })
            .collect();
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        lines.iter().map(|l| format!("{l}\n")).collect()
    }

    /// The terminal's new size: rows go from the top when it shrinks (the bottom, where agents draw their prompt,
    /// stays), lines are cut or padded on the right.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (usize::from(cols.max(1)), usize::from(rows.max(1)));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        let fit = |grid: &mut Vec<Vec<Cell>>, cursor: &mut Cursor| {
            if grid.len() > rows {
                let cut = grid.len() - rows;
                grid.drain(..cut);
                cursor.row = cursor.row.saturating_sub(cut);
            }
            grid.resize(rows, vec![BLANK; cols]);
            for row in grid.iter_mut() {
                row.resize(cols, BLANK);
                if row.last() == Some(&Cell::WideTail) || matches!(row.last(), Some(Cell::Char(c)) if c.width() == Some(2)) {
                    row[cols - 1] = BLANK;
                }
            }
            cursor.row = cursor.row.min(rows - 1);
            cursor.col = cursor.col.min(cols - 1);
        };
        fit(&mut self.grid, &mut self.cursor);
        if let Some((grid, cursor)) = &mut self.saved_main {
            fit(grid, cursor);
        }
        (self.cols, self.rows) = (cols, rows);
        (self.top, self.bottom) = (0, rows - 1);
        self.saved.row = self.saved.row.min(rows - 1);
        self.saved.col = self.saved.col.min(cols - 1);
        self.wrap_pending = false;
    }

    /// The agent's next output.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(bytes);
        let mut at = 0;
        while at < data.len() {
            match std::str::from_utf8(&data[at..]) {
                Ok(text) => {
                    text.chars().for_each(|c| self.put(c));
                    break;
                }
                Err(err) => {
                    let good = at + err.valid_up_to();
                    // Checked as UTF-8 just above.
                    std::str::from_utf8(&data[at..good]).unwrap_or_default().chars().for_each(|c| self.put(c));
                    match err.error_len() {
                        Some(bad) => {
                            self.put(char::REPLACEMENT_CHARACTER);
                            at = good + bad;
                        }
                        None => {
                            // A character cut between reads: finished by the next.
                            self.partial = data[good..].to_vec();
                            break;
                        }
                    }
                }
            }
        }
    }

    fn put(&mut self, c: char) {
        match std::mem::take(&mut self.parse) {
            Parse::Ground => self.ground(c),
            Parse::Escape => self.escape(c),
            Parse::Charset => {}
            Parse::Csi(mut params) => {
                if ('@'..='~').contains(&c) {
                    self.csi(&params, c);
                } else if c == '\x1b' {
                    self.parse = Parse::Escape;
                } else if params.len() < 64 {
                    params.push(c);
                    self.parse = Parse::Csi(params);
                } else {
                    self.parse = Parse::Csi(params);
                }
            }
            Parse::Osc(mut text) => match c {
                '\x07' => self.osc(&text),
                '\x1b' => self.parse = Parse::OscEscape(text),
                _ => {
                    if text.len() < MAX_OSC {
                        text.push(c);
                    }
                    self.parse = Parse::Osc(text);
                }
            },
            Parse::OscEscape(text) => {
                self.osc(&text);
                if c != '\\' {
                    self.escape(c);
                }
            }
            Parse::Ignored => {
                self.parse = if c == '\x1b' {
                    Parse::IgnoredEscape
                } else if c == '\x07' {
                    Parse::Ground
                } else {
                    Parse::Ignored
                }
            }
            Parse::IgnoredEscape => self.parse = if c == '\\' { Parse::Ground } else { Parse::Ignored },
        }
    }

    fn ground(&mut self, c: char) {
        match c {
            '\x1b' => self.parse = Parse::Escape,
            '\r' => self.carriage_return(),
            '\n' | '\x0b' | '\x0c' => self.line_feed(),
            '\x08' => {
                self.cursor.col = self.cursor.col.saturating_sub(1);
                self.wrap_pending = false;
            }
            '\t' => {
                self.cursor.col = ((self.cursor.col / 8 + 1) * 8).min(self.cols - 1);
                self.wrap_pending = false;
            }
            c if c.is_control() => {}
            c => self.print(c),
        }
    }

    fn escape(&mut self, c: char) {
        match c {
            '[' => self.parse = Parse::Csi(String::new()),
            ']' => self.parse = Parse::Osc(String::new()),
            'P' | 'X' | '^' | '_' => self.parse = Parse::Ignored,
            '(' | ')' | '*' | '+' | '#' | '%' => self.parse = Parse::Charset,
            '7' => self.saved = self.cursor,
            '8' => self.restore_cursor(),
            'D' => self.line_feed(),
            'E' => {
                self.carriage_return();
                self.line_feed();
            }
            'M' => self.reverse_index(),
            'c' => *self = Screen::new(self.cols as u16, self.rows as u16),
            _ => {}
        }
    }

    fn osc(&mut self, text: &str) {
        let (code, rest) = text.split_once(';').unwrap_or((text, ""));
        match code {
            "0" | "2" => self.title = rest.to_string(),
            "9" if rest.starts_with("4;") => self.progress = rest.to_string(),
            _ => {}
        }
    }

    fn csi(&mut self, params: &str, action: char) {
        let private = params.starts_with(['?', '>', '<', '=']);
        let body = params.trim_start_matches(['?', '>', '<', '=']);
        // Intermediates (` `, `!`, `"`…) change the command: none Marv needs.
        if body.chars().any(|c| (' '..='/').contains(&c)) {
            return;
        }
        let args: Vec<usize> = body.split(';').map(|p| p.split(':').next().unwrap_or("").parse::<usize>().unwrap_or(0)).collect();
        let arg = |i: usize, default: usize| args.get(i).copied().filter(|v| *v != 0).unwrap_or(default);
        if private {
            if action == 'h' || action == 'l' {
                for mode in &args {
                    self.private_mode(*mode, action == 'h');
                }
            }
            return;
        }
        let (rows, cols) = (self.rows, self.cols);
        match action {
            'A' => self.move_to(self.cursor.row.saturating_sub(arg(0, 1)).max(self.top_if_inside()), self.cursor.col),
            'B' | 'e' => self.move_to((self.cursor.row + arg(0, 1)).min(self.bottom_if_inside()), self.cursor.col),
            'C' | 'a' => self.move_to(self.cursor.row, self.cursor.col + arg(0, 1)),
            'D' => self.move_to(self.cursor.row, self.cursor.col.saturating_sub(arg(0, 1))),
            'E' => self.move_to((self.cursor.row + arg(0, 1)).min(self.bottom_if_inside()), 0),
            'F' => self.move_to(self.cursor.row.saturating_sub(arg(0, 1)).max(self.top_if_inside()), 0),
            'G' | '`' => self.move_to(self.cursor.row, arg(0, 1) - 1),
            'd' => self.move_to(arg(0, 1) - 1, self.cursor.col),
            'H' | 'f' => self.move_to(arg(0, 1) - 1, arg(1, 1) - 1),
            'J' => match args.first().copied().unwrap_or(0) {
                0 => {
                    self.erase_line(self.cursor.row, self.cursor.col, cols);
                    (self.cursor.row + 1..rows).for_each(|r| self.erase_line(r, 0, cols));
                }
                1 => {
                    (0..self.cursor.row).for_each(|r| self.erase_line(r, 0, cols));
                    self.erase_line(self.cursor.row, 0, self.cursor.col + 1);
                }
                2 | 3 => (0..rows).for_each(|r| self.erase_line(r, 0, cols)),
                _ => {}
            },
            'K' => match args.first().copied().unwrap_or(0) {
                0 => self.erase_line(self.cursor.row, self.cursor.col, cols),
                1 => self.erase_line(self.cursor.row, 0, self.cursor.col + 1),
                2 => self.erase_line(self.cursor.row, 0, cols),
                _ => {}
            },
            'X' => self.erase_line(self.cursor.row, self.cursor.col, self.cursor.col + arg(0, 1)),
            '@' => {
                let (row, col, n) = (self.cursor.row, self.cursor.col, arg(0, 1).min(cols - self.cursor.col));
                let line = &mut self.grid[row];
                line.splice(col..col, std::iter::repeat_n(BLANK, n));
                line.truncate(cols);
                self.wrap_pending = false;
            }
            'P' => {
                let (row, col, n) = (self.cursor.row, self.cursor.col, arg(0, 1).min(cols - self.cursor.col));
                let line = &mut self.grid[row];
                line.drain(col..col + n);
                line.resize(cols, BLANK);
                self.wrap_pending = false;
            }
            'L' if self.inside_region() => self.insert_lines(self.cursor.row, arg(0, 1)),
            'M' if self.inside_region() => self.delete_lines(self.cursor.row, arg(0, 1)),
            'S' => self.delete_lines(self.top, arg(0, 1)),
            'T' => self.insert_lines(self.top, arg(0, 1)),
            'b' => {
                if let Some(c) = self.last_char {
                    (0..arg(0, 1).min(cols * rows)).for_each(|_| self.print(c));
                }
            }
            'r' => {
                let (top, bottom) = (arg(0, 1) - 1, arg(1, rows).min(rows) - 1);
                if top < bottom {
                    (self.top, self.bottom) = (top, bottom);
                    self.move_to(0, 0);
                }
            }
            's' => self.saved = self.cursor,
            'u' => self.restore_cursor(),
            _ => {}
        }
    }

    fn private_mode(&mut self, mode: usize, on: bool) {
        match mode {
            7 => self.autowrap = on,
            47 | 1047 | 1049 => {
                if on && self.saved_main.is_none() {
                    let blank = vec![vec![BLANK; self.cols]; self.rows];
                    self.saved_main = Some((std::mem::replace(&mut self.grid, blank), self.cursor));
                    if mode == 1049 {
                        self.cursor = Cursor::default();
                    }
                } else if !on && let Some((grid, cursor)) = self.saved_main.take() {
                    self.grid = grid;
                    if mode == 1049 {
                        self.cursor = cursor;
                    }
                }
                self.wrap_pending = false;
            }
            _ => {}
        }
    }

    fn print(&mut self, c: char) {
        let width = match c.width() {
            Some(0) | None => return,
            Some(w) => w.min(2),
        };
        if self.wrap_pending {
            if self.autowrap {
                self.carriage_return();
                self.line_feed();
            }
            self.wrap_pending = false;
        }
        if width == 2 && self.cursor.col + 1 >= self.cols {
            // No room for both halves: it goes to the next line (or, with no wrap, isn't drawn).
            if !self.autowrap || self.cols < 2 {
                return;
            }
            self.erase_line(self.cursor.row, self.cursor.col, self.cols);
            self.carriage_return();
            self.line_feed();
        }
        let (row, col) = (self.cursor.row, self.cursor.col);
        self.clear_wide_at(row, col);
        self.grid[row][col] = Cell::Char(c);
        if width == 2 {
            self.clear_wide_at(row, col + 1);
            self.grid[row][col + 1] = Cell::WideTail;
        }
        self.last_char = Some(c);
        if col + width >= self.cols {
            self.cursor.col = self.cols - 1;
            self.wrap_pending = true;
        } else {
            self.cursor.col = col + width;
        }
    }

    /// Overwriting either half of a wide character blanks the other half.
    fn clear_wide_at(&mut self, row: usize, col: usize) {
        match self.grid[row][col] {
            Cell::WideTail if col > 0 => self.grid[row][col - 1] = BLANK,
            Cell::Char(c) if c.width() == Some(2) && col + 1 < self.cols => self.grid[row][col + 1] = BLANK,
            _ => {}
        }
    }

    fn erase_line(&mut self, row: usize, from: usize, to: usize) {
        let to = to.min(self.cols);
        if from >= to {
            return;
        }
        self.clear_wide_at(row, from);
        self.clear_wide_at(row, to - 1);
        self.grid[row][from..to].fill(BLANK);
        self.wrap_pending = false;
    }

    fn move_to(&mut self, row: usize, col: usize) {
        self.cursor = Cursor { row: row.min(self.rows - 1), col: col.min(self.cols - 1) };
        self.wrap_pending = false;
    }

    fn restore_cursor(&mut self) {
        self.move_to(self.saved.row, self.saved.col);
    }

    fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.wrap_pending = false;
    }

    fn line_feed(&mut self) {
        if self.cursor.row == self.bottom {
            self.delete_lines(self.top, 1);
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
        self.wrap_pending = false;
    }

    fn reverse_index(&mut self) {
        if self.cursor.row == self.top {
            self.insert_lines(self.top, 1);
        } else {
            self.cursor.row = self.cursor.row.saturating_sub(1);
        }
        self.wrap_pending = false;
    }

    fn inside_region(&self) -> bool {
        (self.top..=self.bottom).contains(&self.cursor.row)
    }

    fn top_if_inside(&self) -> usize {
        if self.cursor.row >= self.top { self.top } else { 0 }
    }

    fn bottom_if_inside(&self) -> usize {
        if self.cursor.row <= self.bottom { self.bottom } else { self.rows - 1 }
    }

    /// `n` blank lines at `row`, the region's bottom lines pushed out.
    fn insert_lines(&mut self, row: usize, n: usize) {
        let n = n.min(self.bottom + 1 - row);
        for _ in 0..n {
            self.grid.remove(self.bottom);
            self.grid.insert(row, vec![BLANK; self.cols]);
        }
        self.wrap_pending = false;
    }

    /// `n` lines at `row` gone, blank lines coming in at the region's bottom.
    fn delete_lines(&mut self, row: usize, n: usize) {
        let n = n.min(self.bottom + 1 - row);
        for _ in 0..n {
            self.grid.remove(row);
            self.grid.insert(self.bottom, vec![BLANK; self.cols]);
        }
        self.wrap_pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(cols: u16, rows: u16, output: &str) -> Screen {
        let mut screen = Screen::new(cols, rows);
        screen.feed(output.as_bytes());
        screen
    }

    #[test]
    fn printing_wraps_and_scrolls() {
        assert_eq!(drawn(5, 3, "abcdefg").text(), "abcde\nfg\n");
        assert_eq!(drawn(5, 2, "1\r\n2\r\n3\r\n4").text(), "3\n4\n");
        // At the last column the wrap waits for the next character: a CR LF there is one line break, not two.
        assert_eq!(drawn(5, 3, "abcde\r\nf").text(), "abcde\nf\n");
        assert_eq!(drawn(10, 2, "a\tb").text(), "a       b\n");
    }

    /// An agent redrawing in place (Claude Code's spinner and footer): only what's on screen now is read.
    #[test]
    fn a_redraw_in_place_leaves_only_the_latest_frame() {
        let mut screen = drawn(40, 6, "> hi\r\n\r\n✻ Thinking… (1s)\r\n\r\n❯ \r\n  ? for shortcuts");
        screen.feed("\x1b[3A\r\x1b[2K✻ Thinking… (2s · esc to interrupt)\x1b[3B\r".as_bytes());
        screen.feed("\x1b[3A\r\x1b[J⏺ Done.\r\n\r\n❯ \r\n  ? for shortcuts".as_bytes());
        assert_eq!(screen.text(), "> hi\n\n⏺ Done.\n\n❯\n  ? for shortcuts\n");
    }

    #[test]
    fn positions_erases_and_edits() {
        let screen = drawn(10, 3, "\x1b[2;3Hxy\x1b[1;1Habcdef\x1b[1;3H\x1b[2P\x1b[3;1Hzz\x1b[1G\x1b[1@");
        assert_eq!(screen.text(), "abef\n  xy\n zz\n");
        assert_eq!(drawn(10, 2, "hello\x1b[3D\x1b[K").text(), "he\n");
        assert_eq!(drawn(10, 2, "hello\x1b[1;2H\x1b[2X").text(), "h  lo\n");
        assert_eq!(drawn(10, 2, "ab\x1b[3b").text(), "abbbb\n", "REP repeats the last character");
    }

    #[test]
    fn scroll_regions_and_line_edits_stay_inside_the_region() {
        // Rows 1-3 scroll; row 4 (a footer) stays.
        let mut screen = drawn(10, 4, "\x1b[4;1Hfooter\x1b[1;3r\x1b[1;1Ha\r\nb\r\nc\r\nd");
        assert_eq!(screen.text(), "b\nc\nd\nfooter\n");
        screen.feed(b"\x1b[1;1H\x1b[L");
        assert_eq!(screen.text(), "\nb\nc\nfooter\n");
        screen.feed(b"\x1b[M\x1b[M");
        assert_eq!(screen.text(), "c\n\n\nfooter\n");
        screen.feed(b"\x1bM\x1bMtop");
        assert_eq!(screen.text(), "top\n\nc\nfooter\n");
    }

    #[test]
    fn the_alternate_screen_comes_and_goes() {
        let mut screen = drawn(10, 3, "$ opencode");
        screen.feed(b"\x1b[?1049h\x1b[H\x1b[2Jtui");
        assert_eq!(screen.text(), "tui\n");
        screen.feed(b"\x1b[?1049l");
        assert_eq!(screen.text(), "$ opencode\n");
    }

    #[test]
    fn titles_progress_and_split_utf8_are_read() {
        let mut screen = Screen::new(20, 2);
        screen.feed(b"\x1b]0;\xe2\xa0\x8b Working\x07\x1b]9;4;1;\x1b\\");
        assert_eq!((screen.title(), screen.progress()), ("⠋ Working", "4;1;"));
        screen.feed(b"\x1b]2;w\x1b\\\x1b]9;9;/w\x07");
        assert_eq!((screen.title(), screen.progress()), ("w", "4;1;"), "only OSC 9;4 is progress");
        let euro = "€".as_bytes();
        screen.feed(&euro[..1]);
        screen.feed(&euro[1..]);
        screen.feed(b"\x1b[38;2;255;0;0mred\x1b[0m\x1bP+q544e\x1b\\!");
        assert_eq!(screen.text(), "€red!\n");
    }

    #[test]
    fn wide_characters_take_two_cells() {
        assert_eq!(drawn(6, 2, "日本x").text(), "日本x\n");
        // Overwriting a wide character's tail blanks its head.
        assert_eq!(drawn(6, 2, "日本\x1b[1;2Hy").text(), " y本\n");
        // No room at the end of a line: it wraps whole.
        assert_eq!(drawn(3, 2, "ab日").text(), "ab\n日\n");
    }

    #[test]
    fn resizing_keeps_the_bottom() {
        let mut screen = drawn(10, 4, "1\r\n2\r\n3\r\n4");
        screen.resize(3, 2);
        assert_eq!(screen.text(), "3\n4\n");
        screen.feed(b"!");
        assert_eq!(screen.text(), "3\n4!\n");
        screen.resize(5, 3);
        assert_eq!(screen.text(), "3\n4!\n");
    }
}
