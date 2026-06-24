use std::fmt::Display;

use colored::{Color, Colorize};

use super::style::{FrameStyle, HeaderStyle, StringStyle, TextAlignment, TextStyle, with_style};

pub struct Table {
    pub(super) frame_style: FrameStyle,
    pub(super) header_style: HeaderStyle,
    pub(super) text_style: TextStyle,

    pub(super) header: Vec<String>,
    pub(super) rows: Vec<Vec<String>>,

    /// Cap the rendered table at this total width (e.g. the terminal width);
    /// the `flex_column` is shrunk to fit, then either truncated or wrapped.
    pub(super) max_width: Option<usize>,
    /// Index of the column allowed to shrink when `max_width` would be exceeded.
    pub(super) flex_column: Option<usize>,
    /// When set, the `flex_column`'s overflowing cells are wrapped onto multiple
    /// lines instead of truncated with `...`.
    pub(super) wrap: bool,
}

/// Smallest width the flex column is allowed to shrink to, so a truncated cell
/// can still show a few characters plus the `...` marker.
const MIN_FLEX_WIDTH: usize = 8;
const ELLIPSIS: &str = "...";

impl Table {
    pub fn new() -> Self {
        let mut frame_style = FrameStyle::default();
        frame_style.set_color(Color::BrightBlack);
        let mut header_style = HeaderStyle::default();
        header_style.set_color(Color::BrightMagenta);

        Self {
            frame_style,
            header_style,
            text_style: TextStyle::default(),
            header: Vec::new(),
            rows: Vec::new(),
            max_width: None,
            flex_column: None,
            wrap: false,
        }
    }

    /// Constrain the table to `width` columns, truncating `column`'s cells with
    /// `...` when the natural layout would be wider.
    pub fn fit_to_width(&mut self, width: usize, column: usize) -> &mut Self {
        self.max_width = Some(width);
        self.flex_column = Some(column);
        self.wrap = false;
        self
    }

    /// Constrain the table to `width` columns, wrapping `column`'s cells onto
    /// multiple lines (a cell becomes several rows of text) when the natural
    /// layout would be wider — used by `info`, where the full value matters.
    pub fn wrap_to_width(&mut self, width: usize, column: usize) -> &mut Self {
        self.max_width = Some(width);
        self.flex_column = Some(column);
        self.wrap = true;
        self
    }
}

/// row
impl Table {
    pub fn set_header<S, I>(&mut self, header: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.header = header.into_iter().map(Into::into).collect();
        self
    }

    pub fn add_row<S, I>(&mut self, row: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.rows.push(row.into_iter().map(Into::into).collect());
        self
    }

    pub fn add_rows<S, I, R>(&mut self, rows: R) -> &mut Self
    where
        R: IntoIterator<Item = I>,
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for row in rows {
            self.add_row(row);
        }
        self
    }

    pub fn add_row_if<'a, P, R, S, I>(&mut self, predicate: P, row: R) -> &mut Self
    where
        P: FnOnce(usize) -> bool,
        R: FnOnce(usize) -> I,
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let len = self.rows.len();
        if predicate(len) {
            self.rows.push(row(len).into_iter().map(Into::into).collect());
        }
        self
    }
}

/// column
impl Table {
    pub fn column(&self, index: usize) -> Option<Vec<&str>> {
        if self.rows.is_empty() {
            return None;
        }

        let mut column = Vec::new();
        for row in &self.rows {
            column.push(row[index].as_str());
        }

        return Some(column);
    }

    pub fn n_columns(&self) -> Option<usize> {
        if self.rows.is_empty() {
            None
        } else {
            // FIXME: this is acctually not correct
            Some(self.rows[0].len())
        }
    }

    fn column_lens(&self) -> Option<Vec<usize>> {
        if self.rows.is_empty() {
            None
        } else {
            let mut column_lens = Vec::new();
            for i in 0..self.n_columns().unwrap() {
                let column = self.column(i);

                let n = if let Some(column) = column {
                    column
                        .iter()
                        .max_by(|a, b| a.len().cmp(&b.len()))
                        .unwrap_or(&"")
                        .len()
                        .max(self.header.get(i).unwrap_or(&String::new()).len())
                } else {
                    0
                };

                column_lens.push(n);
            }
            self.shrink_flex_column(&mut column_lens);
            Some(column_lens)
        }
    }

    /// If a `max_width` is set and the natural layout is too wide, shrink the
    /// flex column (never growing it, never below `MIN_FLEX_WIDTH`) so the table
    /// fits. Cells in that column are truncated at render time.
    fn shrink_flex_column(&self, column_lens: &mut [usize]) {
        let (Some(max_width), Some(flex)) = (self.max_width, self.flex_column) else {
            return;
        };
        if flex >= column_lens.len() {
            return;
        }
        // Frame overhead: 2 padding spaces per column plus one separator between
        // every column and the two outer borders (n + 1 vertical bars).
        let n = column_lens.len();
        let overhead = 2 * n + (n + 1);
        let total: usize = column_lens.iter().sum::<usize>() + overhead;
        if total <= max_width {
            return;
        }
        let excess = total - max_width;
        let target = column_lens[flex].saturating_sub(excess).max(MIN_FLEX_WIDTH);
        if target < column_lens[flex] {
            column_lens[flex] = target;
        }
    }
}

/// Split `s` into chunks of at most `width` characters, preserving order, so a
/// long cell can be rendered across multiple lines. Always returns at least one
/// (possibly empty) line.
fn wrap_cell(s: &str, width: usize) -> Vec<String> {
    if s.is_empty() || width == 0 {
        return vec![s.to_string()];
    }
    let chars: Vec<char> = s.chars().collect();
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}

/// Truncate `s` to `width` display columns, ending with `...` when it overflows.
fn fit_cell(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    if width <= ELLIPSIS.len() {
        return s.chars().take(width).collect();
    }
    let mut out: String = s.chars().take(width - ELLIPSIS.len()).collect();
    out.push_str(ELLIPSIS);
    out
}

/// utils for render
impl Table {
    fn print_row(
        &self,
        f: &mut std::fmt::Formatter<'_>,
        column_lens: &Vec<usize>,
        content: &Vec<String>,
        content_style: &dyn StringStyle,
        content_alignment: &TextAlignment,
    ) -> std::fmt::Result {
        // Lay each cell out as one or more physical lines: the wrap column folds
        // onto multiple lines, every other column truncates to a single line. A
        // logical row is then as tall as its tallest cell, with shorter cells
        // padded by blank continuation lines.
        let cells: Vec<Vec<String>> = (0..self.n_columns().unwrap())
            .map(|i| {
                if self.wrap && self.flex_column == Some(i) {
                    wrap_cell(&content[i], column_lens[i])
                } else {
                    vec![fit_cell(&content[i], column_lens[i])]
                }
            })
            .collect();
        let height = cells.iter().map(Vec::len).max().unwrap_or(1);

        let bar = || with_style(self.frame_style.get_frame_line().straight(), &self.frame_style);
        for line in 0..height {
            write!(f, "{}", bar())?;
            for i in 0..self.n_columns().unwrap() {
                if i != 0 {
                    write!(f, "{}", bar())?;
                }
                let max_len = column_lens[i];
                let cell = cells[i].get(line).cloned().unwrap_or_default();
                match content_alignment {
                    TextAlignment::Left => write!(f, " {:<max_len$} ", with_style(cell, content_style))?,
                    TextAlignment::Center => write!(f, " {:^max_len$} ", with_style(cell, content_style))?,
                    TextAlignment::Right => write!(f, " {:>max_len$} ", with_style(cell, content_style))?,
                };
            }
            writeln!(f, "{}", bar())?;
        }

        Ok(())
    }

    fn print_frame(
        &self,
        f: &mut std::fmt::Formatter<'_>,
        column_lens: &Vec<usize>,
        leading: &str,
        separator: &str,
        trailing: &str,
    ) -> std::fmt::Result {
        write!(f, "{}", with_style(leading, &self.frame_style))?;
        for i in 0..self.n_columns().unwrap() {
            if i != 0 {
                write!(f, "{}", with_style(separator, &self.frame_style))?;
            }
            write!(
                f,
                "{}",
                with_style(
                    self.frame_style
                        .get_frame_line()
                        .dash()
                        .repeat(column_lens[i] + 2),
                    &self.frame_style
                )
            )?;
        }
        writeln!(f, "{}", with_style(trailing, &self.frame_style))?;

        Ok(())
    }
}

/// render
impl Display for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.rows.is_empty() {
            return Ok(());
        }

        let column_lens = self.column_lens().unwrap();

        self.print_frame(
            f,
            &column_lens,
            self.frame_style.get_frame_corner().top_left(),
            self.frame_style.get_frame_line().top_down(),
            self.frame_style.get_frame_corner().top_right(),
        )?;
        self.print_row(
            f,
            &column_lens,
            &self.header,
            &self.header_style,
            &TextAlignment::Center,
        )?;
        self.print_frame(
            f,
            &column_lens,
            self.frame_style.get_frame_line().left_right(),
            self.frame_style.get_frame_line().cross(),
            self.frame_style.get_frame_line().right_left(),
        )?;
        for row in self.rows.iter() {
            self.print_row(f, &column_lens, row, &self.text_style, &TextAlignment::Left)?;
        }
        self.print_frame(
            f,
            &column_lens,
            self.frame_style.get_frame_line().left_right(),
            self.frame_style.get_frame_line().cross(),
            self.frame_style.get_frame_line().right_left(),
        )?;
        self.print_row(
            f,
            &column_lens,
            &self.header,
            &self.header_style,
            &TextAlignment::Center,
        )?;
        self.print_frame(
            f,
            &column_lens,
            self.frame_style.get_frame_corner().bottom_left(),
            self.frame_style.get_frame_line().bottom_up(),
            self.frame_style.get_frame_corner().bottom_right(),
        )?;

        // term_test();

        Ok(())
    }
}

fn term_test() {
    println!("─	━	│	┃	┄	┅	┆	┇	┈	┉	┊	┋	┌	┍	┎	┏");
    println!("┐	┑	┒	┓	└	┕	┖	┗	┘	┙	┚	┛	├	┝	┞	┟");
    println!("┠	┡	┢	┣	┤	┥	┦	┧	┨	┩	┪	┫	┬	┭	┮	┯");
    println!("┰	┱	┲	┳	┴	┵	┶	┷	┸	┹	┺	┻	┼	┽	┾	┿");
    println!("╀	╁	╂	╃	╄	╅	╆	╇	╈	╉	╊	╋	╌	╍	╎	╏");
    println!("═	║	╒	╓	╔	╕	╖	╗	╘	╙	╚	╛	╜	╝	╞	╟");
    println!("╠	╡	╢	╣	╤	╥	╦	╧	╨	╩	╪	╫	╬	╭	╮	╯");
    println!("╰	╱	╲	╳	╴	╵	╶	╷	╸	╹	╺	╻	╼	╽	╾	╿");

    println!(
        " {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} ",
        "BLACK".black(),
        "RED".red(),
        "GREEN".green(),
        "YELLOW".yellow(),
        "BLUE".blue(),
        "MAGENTA".magenta(),
        "CYAN".cyan(),
        "WHITE".white()
    );
    println!(
        " {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} ",
        "BRIGHT_BLACK".bright_black(),
        "BRIGHT_RED".bright_red(),
        "BRIGHT_GREEN".bright_green(),
        "BRIGHT_YELLOW".bright_yellow(),
        "BRIGHT_BLUE".bright_blue(),
        "BRIGHT_MAGENTA".bright_magenta(),
        "BRIGHT_CYAN".bright_cyan(),
        "BRIGHT_WHITE".bright_white()
    );
    println!(
        " {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} ",
        "BLACK".black().bold(),
        "RED".red().bold(),
        "GREEN".green().bold(),
        "YELLOW".yellow().bold(),
        "BLUE".blue().bold(),
        "MAGENTA".magenta().bold(),
        "CYAN".cyan().bold(),
        "WHITE".white().bold()
    );
    println!(
        " {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} ",
        "BRIGHT_BLACK".bright_black().bold(),
        "BRIGHT_RED".bright_red().bold(),
        "BRIGHT_GREEN".bright_green().bold(),
        "BRIGHT_YELLOW".bright_yellow().bold(),
        "BRIGHT_BLUE".bright_blue().bold(),
        "BRIGHT_MAGENTA".bright_magenta().bold(),
        "BRIGHT_CYAN".bright_cyan().bold(),
        "BRIGHT_WHITE".bright_white().bold()
    );

    println!(
        " {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} ",
        "BLACK".black().reversed(),
        "RED".red().reversed(),
        "GREEN".green().reversed(),
        "YELLOW".yellow().reversed(),
        "BLUE".blue().reversed(),
        "MAGENTA".magenta().reversed(),
        "CYAN".cyan().reversed(),
        "WHITE".white().reversed()
    );
    println!(
        " {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} {:^14} ",
        "BRIGHT_BLACK".bright_black().reversed(),
        "BRIGHT_RED".bright_red().reversed(),
        "BRIGHT_GREEN".bright_green().reversed(),
        "BRIGHT_YELLOW".bright_yellow().reversed(),
        "BRIGHT_BLUE".bright_blue().reversed(),
        "BRIGHT_MAGENTA".bright_magenta().reversed(),
        "BRIGHT_CYAN".bright_cyan().reversed(),
        "BRIGHT_WHITE".bright_white()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_cell_keeps_short_strings_and_truncates_long_ones() {
        assert_eq!(fit_cell("short", 10), "short");
        assert_eq!(fit_cell("hello world", 8), "hello...");
        assert_eq!(fit_cell("hello world", 8).chars().count(), 8);
        // Widths too small for the ellipsis just hard-cut.
        assert_eq!(fit_cell("hello", 2), "he");
    }

    #[test]
    fn shrink_flex_reduces_only_the_flex_column_to_fit() {
        let mut t = Table::new();
        t.fit_to_width(20, 1);
        // natural: col0=3, col1=40; overhead = 2*2 + 3 = 7
        // target for col1 = 20 - 7 - 3 = 10
        let mut lens = vec![3, 40];
        t.shrink_flex_column(&mut lens);
        assert_eq!(lens, vec![3, 10]);
    }

    #[test]
    fn shrink_flex_floors_at_minimum_and_accepts_overflow() {
        let mut t = Table::new();
        t.fit_to_width(5, 1);
        let mut lens = vec![3, 40];
        t.shrink_flex_column(&mut lens);
        assert_eq!(lens[1], MIN_FLEX_WIDTH);
    }

    #[test]
    fn shrink_flex_noop_when_it_already_fits() {
        let mut t = Table::new();
        t.fit_to_width(100, 1);
        let mut lens = vec![3, 40];
        t.shrink_flex_column(&mut lens);
        assert_eq!(lens, vec![3, 40]);
    }

    #[test]
    fn wrap_cell_chunks_by_width() {
        assert_eq!(wrap_cell("short", 10), vec!["short".to_string()]);
        assert_eq!(
            wrap_cell("abcdefg", 3),
            vec!["abc".to_string(), "def".to_string(), "g".to_string()]
        );
        // Degenerate widths and empty input still yield one line.
        assert_eq!(wrap_cell("", 5), vec!["".to_string()]);
        assert_eq!(wrap_cell("abc", 0), vec!["abc".to_string()]);
    }

    #[test]
    fn wrapped_table_stays_within_width_and_keeps_full_text() {
        colored::control::set_override(false); // deterministic: no ANSI in assertions
        let mut t = Table::new();
        t.set_header(vec!["key", "value"]).add_row(vec![
            "command".to_string(),
            "a-very-long-command-line-that-overflows-the-width".to_string(),
        ]);
        t.wrap_to_width(30, 1);

        let out = format!("{t}");
        for line in out.lines() {
            assert!(
                line.chars().count() <= 30,
                "line too wide ({}): {line:?}",
                line.chars().count()
            );
        }
        // Nothing is dropped: stripping the frame/padding recovers every chunk.
        assert!(!out.contains(ELLIPSIS), "wrap mode must not truncate:\n{out}");
        let joined: String = out
            .lines()
            .filter(|l| l.contains('│'))
            .flat_map(|l| l.split('│'))
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("");
        assert!(
            joined.contains("a-very-long-command-line-that-overflows-the-width"),
            "expected the full value to survive wrapping:\n{out}"
        );
    }

    #[test]
    fn rendered_table_stays_within_max_width() {
        colored::control::set_override(false); // deterministic: no ANSI in assertions
        let mut t = Table::new();
        t.set_header(vec!["id", "command"]).add_row(vec![
            "1".to_string(),
            "a-very-long-command-line-that-overflows".to_string(),
        ]);
        t.fit_to_width(20, 1);

        let out = format!("{t}");
        for line in out.lines() {
            assert!(
                line.chars().count() <= 20,
                "line too wide ({}): {line:?}",
                line.chars().count()
            );
        }
        assert!(out.contains("..."), "expected a truncated cell:\n{out}");
    }
}
