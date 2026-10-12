use crate::gfx::color::Color;

#[derive(Clone, Debug)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub muted: Color,
    pub accent: Color,
    pub code_bg: Color,
    pub rule: Color,
    pub quote_bar: Color,
    pub json_key: Color,
    pub json_string: Color,
    pub json_number: Color,
    pub json_literal: Color,
    pub error: Color,
    pub sel: Color,
    pub base_size: f32,
    pub content_max_width: f32,
}

impl Theme {
    pub fn light() -> Self {
        Self {
            bg: Color::hex(0xFFFFFF),
            fg: Color::hex(0x24292F),
            muted: Color::hex(0x6E7781),
            accent: Color::hex(0x0969DA),
            code_bg: Color::hex(0xF6F8FA),
            rule: Color::hex(0xD0D7DE),
            quote_bar: Color::hex(0xD0D7DE),
            json_key: Color::hex(0x0550AE),
            json_string: Color::hex(0x0A3069),
            json_number: Color::hex(0x953800),
            json_literal: Color::hex(0x8250DF),
            error: Color::hex(0xCF222E),
            sel: Color::rgba(9, 105, 218, 72),
            base_size: 16.0,
            content_max_width: 820.0,
        }
    }
    pub fn dark() -> Self {
        Self {
            bg: Color::hex(0x0D1117),
            fg: Color::hex(0xE6EDF3),
            muted: Color::hex(0x8B949E),
            accent: Color::hex(0x58A6FF),
            code_bg: Color::hex(0x161B22),
            rule: Color::hex(0x30363D),
            quote_bar: Color::hex(0x3D444D),
            json_key: Color::hex(0x79C0FF),
            json_string: Color::hex(0xA5D6FF),
            json_number: Color::hex(0xFFA657),
            json_literal: Color::hex(0xD2A8FF),
            error: Color::hex(0xFF7B72),
            sel: Color::rgba(88, 166, 255, 80),
            base_size: 16.0,
            content_max_width: 820.0,
        }
    }
}
