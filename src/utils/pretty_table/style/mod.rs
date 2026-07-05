mod frame_style;
mod header_style;
mod text_style;

use colored::{Color, ColoredString, Style};
pub(super) use frame_style::FrameStyle;
pub use frame_style::{FrameCorner, FrameLine};
pub(super) use header_style::HeaderStyle;
pub(super) use text_style::{TextAlignment, TextStyle};

use super::Table;

pub(super) trait StringStyle {
    fn get_fgcolor(&self) -> Option<Color>;
    fn get_bgcolor(&self) -> Option<Color>;
    fn get_style(&self) -> Style;
}

/// change color
impl Table {
    pub fn set_frame_style(&mut self, style: FrameStyle) -> &mut Self {
        self.frame_style = style;
        self
    }
    pub fn set_header_style(&mut self, style: HeaderStyle) -> &mut Self {
        self.header_style = style;
        self
    }
    pub fn set_text_style(&mut self, style: TextStyle) -> &mut Self {
        self.text_style = style;
        self
    }

    pub fn set_frame_corner(&mut self, frame_corner: FrameCorner) -> &mut Self {
        self.frame_style.set_frame_corner(frame_corner);
        self
    }
    pub fn set_frame_line(&mut self, frame_line: FrameLine) -> &mut Self {
        self.frame_style.set_frame_line(frame_line);
        self
    }
}

pub(super) fn with_style<S>(text: S, style: &dyn StringStyle) -> ColoredString
where
    S: Into<String>,
{
    let mut copied = ColoredString::default();
    copied.input = text.into();
    copied.fgcolor = style.get_fgcolor();
    copied.bgcolor = style.get_bgcolor();
    copied.style = style.get_style();
    copied
}
