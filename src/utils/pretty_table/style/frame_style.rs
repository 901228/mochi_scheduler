use colored::{Color, Style, Styles};

use super::StringStyle;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameCorner {
    Rounded,
    Normal,
    Double,
    Thick,
}

impl FrameCorner {
    pub fn top_left(&self) -> &'static str {
        match *self {
            FrameCorner::Rounded => "╭",
            FrameCorner::Double => "╔",
            FrameCorner::Normal => "┌",
            FrameCorner::Thick => "┏",
        }
    }

    pub fn top_right(&self) -> &'static str {
        match *self {
            FrameCorner::Rounded => "╮",
            FrameCorner::Double => "╗",
            FrameCorner::Normal => "┐",
            FrameCorner::Thick => "┓",
        }
    }

    pub fn bottom_left(&self) -> &'static str {
        match *self {
            FrameCorner::Rounded => "╰",
            FrameCorner::Double => "╚",
            FrameCorner::Normal => "└",
            FrameCorner::Thick => "┗",
        }
    }

    pub fn bottom_right(&self) -> &'static str {
        match *self {
            FrameCorner::Rounded => "╯",
            FrameCorner::Double => "╝",
            FrameCorner::Normal => "┘",
            FrameCorner::Thick => "┛",
        }
    }
}

impl Default for FrameCorner {
    fn default() -> Self {
        FrameCorner::Rounded
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameLine {
    Normal,
    Double,
    Thick,
}

impl FrameLine {
    pub fn dash(&self) -> &'static str {
        match *self {
            FrameLine::Normal => "─",
            FrameLine::Double => "═",
            FrameLine::Thick => "━",
        }
    }

    pub fn straight(&self) -> &'static str {
        match *self {
            FrameLine::Normal => "│",
            FrameLine::Double => "║",
            FrameLine::Thick => "┃",
        }
    }

    pub fn top_down(&self) -> &'static str {
        match *self {
            FrameLine::Normal => "┬",
            FrameLine::Double => "╤",
            FrameLine::Thick => "┳",
        }
    }

    pub fn bottom_up(&self) -> &'static str {
        match *self {
            FrameLine::Normal => "┴",
            FrameLine::Double => "╧",
            FrameLine::Thick => "┻",
        }
    }

    pub fn left_right(&self) -> &'static str {
        match *self {
            FrameLine::Normal => "├",
            FrameLine::Double => "╠",
            FrameLine::Thick => "┣",
        }
    }

    pub fn right_left(&self) -> &'static str {
        match *self {
            FrameLine::Normal => "┤",
            FrameLine::Double => "╣",
            FrameLine::Thick => "┫",
        }
    }

    pub fn cross(&self) -> &'static str {
        match *self {
            FrameLine::Normal => "┼",
            FrameLine::Double => "╬",
            FrameLine::Thick => "╋",
        }
    }
}

impl Default for FrameLine {
    fn default() -> Self {
        FrameLine::Normal
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FrameStyle {
    /// The color of the text as it will be printed.
    fgcolor: Option<Color>,

    /// The background color (if any). None means that the text will be printed
    /// without a special background.
    bgcolor: Option<Color>,

    /// Any special styling to be applied to the text (see Styles for a list of
    /// available options).
    style: Style,

    /// Corner style for the frame.
    frame_corner: FrameCorner,

    /// Line style for the frame.
    frame_line: FrameLine,
}

impl FrameStyle {
    pub fn set_color(&mut self, color: Color) -> &mut Self {
        self.fgcolor = Some(color);
        self
    }

    pub fn set_bg(&mut self, color: Color) -> &mut Self {
        self.bgcolor = Some(color);
        self
    }

    pub fn set_style(&mut self, style: Style) -> &mut Self {
        self.style = style;
        self
    }

    pub fn add_style(&mut self, style: Styles) -> &mut Self {
        self.style.add(style);
        self
    }

    pub fn set_frame_corner(&mut self, frame_corner: FrameCorner) -> &mut Self {
        self.frame_corner = frame_corner;
        match self.frame_corner {
            FrameCorner::Rounded => self.frame_line = FrameLine::Normal,
            FrameCorner::Normal => self.frame_line = FrameLine::Normal,
            FrameCorner::Double => self.frame_line = FrameLine::Double,
            FrameCorner::Thick => self.frame_line = FrameLine::Thick,
        }
        self
    }

    pub fn set_frame_line(&mut self, frame_line: FrameLine) -> &mut Self {
        self.frame_line = frame_line;
        match self.frame_line {
            FrameLine::Normal => {
                if self.frame_corner != FrameCorner::Rounded && self.frame_corner != FrameCorner::Normal {
                    self.frame_corner = FrameCorner::Rounded;
                }
            }
            FrameLine::Double => self.frame_corner = FrameCorner::Double,
            FrameLine::Thick => self.frame_corner = FrameCorner::Thick,
        }
        self
    }
}

impl StringStyle for FrameStyle {
    fn get_fgcolor(&self) -> Option<Color> {
        self.fgcolor
    }
    fn get_bgcolor(&self) -> Option<Color> {
        self.bgcolor
    }
    fn get_style(&self) -> Style {
        self.style
    }
}

impl FrameStyle {
    pub fn get_frame_corner(&self) -> &FrameCorner {
        &self.frame_corner
    }
    pub fn get_frame_line(&self) -> &FrameLine {
        &self.frame_line
    }
}
