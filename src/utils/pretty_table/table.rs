use std::fmt::Display;

use colored::{Color, Colorize};

use super::style::{FrameStyle, HeaderStyle, StringStyle, TextAlignment, TextStyle, with_style};

pub struct Table {
    pub(super) frame_style: FrameStyle,
    pub(super) header_style: HeaderStyle,
    pub(super) text_style: TextStyle,

    pub(super) header: Vec<String>,
    pub(super) rows: Vec<Vec<String>>,
}

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
        }
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
            Some(column_lens)
        }
    }
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
        write!(
            f,
            "{}",
            with_style(self.frame_style.get_frame_line().straight(), &self.frame_style)
        )?;
        for i in 0..self.n_columns().unwrap() {
            if i != 0 {
                write!(
                    f,
                    "{}",
                    with_style(self.frame_style.get_frame_line().straight(), &self.frame_style)
                )?;
            }
            let max_len = column_lens[i];
            match content_alignment {
                TextAlignment::Left => {
                    write!(f, " {:<max_len$} ", with_style(content[i].clone(), content_style))?
                }
                TextAlignment::Center => {
                    write!(f, " {:^max_len$} ", with_style(content[i].clone(), content_style))?
                }
                TextAlignment::Right => {
                    write!(f, " {:>max_len$} ", with_style(content[i].clone(), content_style))?
                }
            };
        }
        writeln!(
            f,
            "{}",
            with_style(self.frame_style.get_frame_line().straight(), &self.frame_style)
        )?;

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
