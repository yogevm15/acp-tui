use ratatui::style::Color;

#[derive(Debug, Clone)]
pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub is_light: bool,
    pub bg: Color,
    pub surface: Color,
    pub panel: Color,
    pub code_bg: Color,
    pub border: Color,
    pub border_focused: Color,
    pub text: Color,
    pub text_muted: Color,
    pub text_dim: Color,
    pub primary: Color,
    pub secondary: Color,
    pub accent: Color,
    pub user_bar: Color,
    pub assistant_bar: Color,
    pub thought_bar: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub diff_add_fg: Color,
    pub diff_add_bg: Color,
    pub diff_del_fg: Color,
    pub diff_del_bg: Color,
    pub selection_bg: Color,
}

impl Theme {
    pub fn by_name(name: &str) -> Self {
        match name.trim().to_lowercase().as_str() {
            "claude" | "anthropic" | "warm" => Self::claude_warm(),
            "catppuccin" | "mocha" => Self::catppuccin_mocha(),
            "kanagawa" => Self::kanagawa(),
            "nord" => Self::nord(),
            "light" | "solarized-light" => Self::crisp_light(),
            _ => Self::opencode_dark(),
        }
    }

    pub fn all() -> Vec<Self> {
        vec![
            Self::opencode_dark(),
            Self::claude_warm(),
            Self::catppuccin_mocha(),
            Self::kanagawa(),
            Self::nord(),
            Self::crisp_light(),
        ]
    }

    pub fn mode_color(&self, mode_id: &str) -> Color {
        let lower = mode_id.to_lowercase();
        if lower.contains("yolo") || lower.contains("bypass") {
            self.warning
        } else if lower.contains("plan") || lower.contains("architect") {
            self.secondary
        } else if lower.contains("ask") || lower.contains("chat") {
            self.accent
        } else {
            self.primary
        }
    }

    pub fn opencode_dark() -> Self {
        Self {
            id: "opencode",
            name: "OpenCode Dark",
            is_light: false,
            bg: Color::Rgb(19, 22, 31),
            surface: Color::Rgb(26, 30, 43),
            panel: Color::Rgb(22, 26, 37),
            code_bg: Color::Rgb(15, 18, 26),
            border: Color::Rgb(46, 52, 78),
            border_focused: Color::Rgb(122, 162, 247),
            text: Color::Rgb(216, 222, 238),
            text_muted: Color::Rgb(138, 148, 178),
            text_dim: Color::Rgb(92, 102, 132),
            primary: Color::Rgb(122, 162, 247),
            secondary: Color::Rgb(187, 154, 247),
            accent: Color::Rgb(125, 207, 255),
            user_bar: Color::Rgb(122, 162, 247),
            assistant_bar: Color::Rgb(125, 207, 255),
            thought_bar: Color::Rgb(104, 114, 148),
            success: Color::Rgb(158, 206, 106),
            warning: Color::Rgb(224, 175, 104),
            error: Color::Rgb(247, 118, 142),
            diff_add_fg: Color::Rgb(158, 206, 106),
            diff_add_bg: Color::Rgb(26, 48, 34),
            diff_del_fg: Color::Rgb(247, 118, 142),
            diff_del_bg: Color::Rgb(54, 26, 34),
            selection_bg: Color::Rgb(42, 52, 82),
        }
    }

    pub fn claude_warm() -> Self {
        Self {
            id: "claude",
            name: "Claude Code Warm",
            is_light: false,
            bg: Color::Rgb(24, 22, 20),
            surface: Color::Rgb(34, 31, 28),
            panel: Color::Rgb(29, 26, 23),
            code_bg: Color::Rgb(18, 16, 15),
            border: Color::Rgb(62, 56, 50),
            border_focused: Color::Rgb(217, 119, 87),
            text: Color::Rgb(236, 231, 223),
            text_muted: Color::Rgb(164, 155, 143),
            text_dim: Color::Rgb(112, 104, 94),
            primary: Color::Rgb(217, 119, 87),
            secondary: Color::Rgb(182, 146, 226),
            accent: Color::Rgb(118, 184, 198),
            user_bar: Color::Rgb(217, 119, 87),
            assistant_bar: Color::Rgb(228, 178, 118),
            thought_bar: Color::Rgb(120, 110, 98),
            success: Color::Rgb(138, 194, 118),
            warning: Color::Rgb(232, 182, 92),
            error: Color::Rgb(232, 108, 108),
            diff_add_fg: Color::Rgb(146, 208, 126),
            diff_add_bg: Color::Rgb(28, 48, 28),
            diff_del_fg: Color::Rgb(238, 122, 122),
            diff_del_bg: Color::Rgb(56, 26, 26),
            selection_bg: Color::Rgb(62, 46, 38),
        }
    }

    pub fn catppuccin_mocha() -> Self {
        Self {
            id: "catppuccin",
            name: "Catppuccin Mocha",
            is_light: false,
            bg: Color::Rgb(30, 30, 46),
            surface: Color::Rgb(40, 41, 61),
            panel: Color::Rgb(24, 24, 37),
            code_bg: Color::Rgb(17, 17, 27),
            border: Color::Rgb(69, 71, 90),
            border_focused: Color::Rgb(137, 180, 250),
            text: Color::Rgb(205, 214, 244),
            text_muted: Color::Rgb(147, 153, 178),
            text_dim: Color::Rgb(108, 112, 134),
            primary: Color::Rgb(137, 180, 250),
            secondary: Color::Rgb(203, 166, 247),
            accent: Color::Rgb(148, 226, 213),
            user_bar: Color::Rgb(137, 180, 250),
            assistant_bar: Color::Rgb(203, 166, 247),
            thought_bar: Color::Rgb(127, 132, 156),
            success: Color::Rgb(166, 227, 161),
            warning: Color::Rgb(249, 226, 175),
            error: Color::Rgb(243, 139, 168),
            diff_add_fg: Color::Rgb(166, 227, 161),
            diff_add_bg: Color::Rgb(30, 52, 40),
            diff_del_fg: Color::Rgb(243, 139, 168),
            diff_del_bg: Color::Rgb(58, 30, 42),
            selection_bg: Color::Rgb(58, 60, 88),
        }
    }

    pub fn kanagawa() -> Self {
        Self {
            id: "kanagawa",
            name: "Kanagawa Wave",
            is_light: false,
            bg: Color::Rgb(31, 31, 40),
            surface: Color::Rgb(42, 42, 55),
            panel: Color::Rgb(26, 26, 34),
            code_bg: Color::Rgb(22, 22, 29),
            border: Color::Rgb(64, 64, 84),
            border_focused: Color::Rgb(126, 156, 216),
            text: Color::Rgb(220, 215, 186),
            text_muted: Color::Rgb(148, 146, 130),
            text_dim: Color::Rgb(106, 106, 98),
            primary: Color::Rgb(126, 156, 216),
            secondary: Color::Rgb(149, 127, 184),
            accent: Color::Rgb(122, 168, 159),
            user_bar: Color::Rgb(126, 156, 216),
            assistant_bar: Color::Rgb(220, 165, 97),
            thought_bar: Color::Rgb(114, 113, 105),
            success: Color::Rgb(152, 187, 108),
            warning: Color::Rgb(230, 195, 132),
            error: Color::Rgb(228, 104, 118),
            diff_add_fg: Color::Rgb(152, 187, 108),
            diff_add_bg: Color::Rgb(34, 48, 34),
            diff_del_fg: Color::Rgb(228, 104, 118),
            diff_del_bg: Color::Rgb(56, 30, 34),
            selection_bg: Color::Rgb(45, 79, 103),
        }
    }

    pub fn nord() -> Self {
        Self {
            id: "nord",
            name: "Nord",
            is_light: false,
            bg: Color::Rgb(46, 52, 64),
            surface: Color::Rgb(59, 66, 82),
            panel: Color::Rgb(50, 56, 70),
            code_bg: Color::Rgb(38, 43, 54),
            border: Color::Rgb(76, 86, 106),
            border_focused: Color::Rgb(136, 192, 208),
            text: Color::Rgb(236, 239, 244),
            text_muted: Color::Rgb(168, 178, 196),
            text_dim: Color::Rgb(118, 128, 148),
            primary: Color::Rgb(136, 192, 208),
            secondary: Color::Rgb(180, 142, 173),
            accent: Color::Rgb(129, 161, 193),
            user_bar: Color::Rgb(136, 192, 208),
            assistant_bar: Color::Rgb(129, 161, 193),
            thought_bar: Color::Rgb(110, 122, 144),
            success: Color::Rgb(163, 190, 140),
            warning: Color::Rgb(235, 203, 139),
            error: Color::Rgb(191, 97, 106),
            diff_add_fg: Color::Rgb(163, 190, 140),
            diff_add_bg: Color::Rgb(46, 66, 52),
            diff_del_fg: Color::Rgb(216, 122, 130),
            diff_del_bg: Color::Rgb(68, 46, 52),
            selection_bg: Color::Rgb(67, 76, 94),
        }
    }

    pub fn crisp_light() -> Self {
        Self {
            id: "light",
            name: "Crisp Light",
            is_light: true,
            bg: Color::Rgb(250, 250, 252),
            surface: Color::Rgb(238, 240, 246),
            panel: Color::Rgb(244, 245, 249),
            code_bg: Color::Rgb(234, 236, 242),
            border: Color::Rgb(204, 208, 220),
            border_focused: Color::Rgb(42, 96, 196),
            text: Color::Rgb(30, 34, 44),
            text_muted: Color::Rgb(96, 104, 124),
            text_dim: Color::Rgb(136, 144, 162),
            primary: Color::Rgb(42, 96, 196),
            secondary: Color::Rgb(124, 68, 184),
            accent: Color::Rgb(24, 132, 152),
            user_bar: Color::Rgb(42, 96, 196),
            assistant_bar: Color::Rgb(194, 92, 54),
            thought_bar: Color::Rgb(144, 152, 170),
            success: Color::Rgb(36, 138, 68),
            warning: Color::Rgb(184, 118, 22),
            error: Color::Rgb(196, 48, 64),
            diff_add_fg: Color::Rgb(24, 118, 52),
            diff_add_bg: Color::Rgb(220, 246, 226),
            diff_del_fg: Color::Rgb(178, 38, 52),
            diff_del_bg: Color::Rgb(250, 224, 228),
            selection_bg: Color::Rgb(214, 226, 248),
        }
    }
}
