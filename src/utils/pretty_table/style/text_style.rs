use colored::{Color, Style, Styles};

use super::StringStyle;

pub enum TextAlignment {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TextStyle {
    /// The color of the text as it will be printed.
    fgcolor: Option<Color>,

    /// The background color (if any). None means that the text will be printed
    /// without a special background.
    bgcolor: Option<Color>,

    /// Any special styling to be applied to the text (see Styles for a list of
    /// available options).
    style: Style,
}

impl TextStyle {
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
}

impl StringStyle for TextStyle {
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
