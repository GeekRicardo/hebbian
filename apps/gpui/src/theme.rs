//! Desktop shell 的设计令牌。
//!
//! 这里的每一个字段都对应原 Web 前端 `desktopShell.css` 里的一个 `--dsp-*` 自定义属性，
//! 取值来自「CSS 层叠 + React 内联 style 覆盖」之后的最终结果：内联 style 优先级高于任何
//! 选择器，所以凡是 `hueStyle()` 里出现过的变量以内联值为准，其余才落到样式表最后一遍覆写。
//! 变量名保持与 CSS 一致（`--dsp-card-strong` → `card_strong`），改配色时两边能直接对照。

use gpui::{hsla, rgb, rgba, Hsla};

/// CSS `hsl(H S% L% / A)` 的直译：CSS 用角度 + 百分比，gpui 的 `hsla` 全部取 0..1。
pub fn hsl_deg(h: f32, s_pct: f32, l_pct: f32, a: f32) -> Hsla {
    hsla(h.rem_euclid(360.0) / 360.0, s_pct / 100.0, l_pct / 100.0, a)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePreset {
    Glacier,
    Mist,
    Porcelain,
    Moon,
    Abyss,
}

impl ThemePreset {
    pub const ALL: [ThemePreset; 5] = [
        ThemePreset::Glacier,
        ThemePreset::Mist,
        ThemePreset::Porcelain,
        ThemePreset::Moon,
        ThemePreset::Abyss,
    ];

    pub fn id(self) -> &'static str {
        match self {
            ThemePreset::Glacier => "glacier",
            ThemePreset::Mist => "mist",
            ThemePreset::Porcelain => "porcelain",
            ThemePreset::Moon => "moon",
            ThemePreset::Abyss => "abyss",
        }
    }

    /// 色系选择器里显示的中文名，与 `THEME_PRESETS` 一致。
    pub fn label(self) -> &'static str {
        match self {
            ThemePreset::Glacier => "冰湖蓝绿",
            ThemePreset::Mist => "雾蓝灰",
            ThemePreset::Porcelain => "青瓷灰",
            ThemePreset::Moon => "月白灰",
            ThemePreset::Abyss => "深海墨蓝",
        }
    }

    pub fn hue(self) -> f32 {
        match self {
            ThemePreset::Glacier => 208.0,
            ThemePreset::Mist => 214.0,
            ThemePreset::Porcelain => 190.0,
            ThemePreset::Moon => 204.0,
            ThemePreset::Abyss => 206.0,
        }
    }

    /// 预设色板小圆点的三段渐变色。
    pub fn swatch(self) -> [Hsla; 3] {
        let hexes = match self {
            ThemePreset::Glacier => [0xEAF4FF, 0xEEF0FF, 0xE8FFF5],
            ThemePreset::Mist => [0xEEF4FA, 0xF3F6FA, 0xE8F0F7],
            ThemePreset::Porcelain => [0xEEF8F8, 0xF4F8F6, 0xE7F1F2],
            ThemePreset::Moon => [0xF6F8FA, 0xEEF5F7, 0xF9FAFB],
            ThemePreset::Abyss => [0x07111C, 0x102033, 0x4DB8FF],
        };
        [
            rgb(hexes[0]).into(),
            rgb(hexes[1]).into(),
            rgb(hexes[2]).into(),
        ]
    }

    pub fn is_dark(self) -> bool {
        matches!(self, ThemePreset::Abyss)
    }
}

/// 一份完整的解析后配色。字段与 `--dsp-*` 一一对应。
#[derive(Debug, Clone)]
pub struct Theme {
    pub preset: ThemePreset,
    pub hue: f32,

    pub bg: Hsla,
    pub canvas: Hsla,
    pub sidebar: Hsla,
    pub card: Hsla,
    pub card_strong: Hsla,
    pub line: Hsla,
    pub line_strong: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub accent: Hsla,
    pub accent_2: Hsla,
    pub accent_soft: Hsla,

    pub chat_wash: Hsla,
    pub chat_bubble_a: Hsla,
    pub chat_bubble_b: Hsla,
    pub chat_bubble_c: Hsla,
    pub chat_bubble_d: Hsla,
    pub chat_panel: Hsla,
    pub chat_panel_end: Hsla,

    pub right_bg_top: Hsla,
    pub right_bg_bottom: Hsla,
    pub right_card: Hsla,

    pub user_bubble_a: Hsla,
    pub user_bubble_b: Hsla,
    pub user_line: Hsla,

    pub green: Hsla,
    pub amber: Hsla,
    pub danger: Hsla,

    pub hue_popover_bg: Hsla,
    pub hue_button_bg: Hsla,
    pub theme_preset_bg: Hsla,
    pub ring_core: Hsla,

    /// 侧栏卡片、浮层的白底（CSS 里写死 rgba(255,255,255,α) 的那些位置，
    /// 深色下必须换成深色底，否则整块发白）。
    pub surface_veil: Hsla,
    /// 侧栏卡片描边（浅色 rgba(32,54,78,0.08)）。
    pub card_line: Hsla,
    /// 会话行标题色（浅色 #5c6a7a）。
    pub session_title: Hsla,
    /// 输入框正文色（浅色 #2f3034）。
    pub input_text: Hsla,
    /// 输入框 placeholder（浅色 #b8c1cb）。
    pub input_placeholder: Hsla,
    /// 「新建对话」按钮的两段底色。
    pub new_chat_bg_a: Hsla,
    pub new_chat_bg_b: Hsla,
    pub new_chat_text: Hsla,
    pub new_chat_line: Hsla,
    /// tabs 选中态。
    pub tab_active_bg: Hsla,
    pub tab_active_text: Hsla,
    pub tab_text: Hsla,
    pub tabs_bg: Hsla,
}

impl Theme {
    pub fn new(preset: ThemePreset, hue: f32) -> Self {
        if preset == ThemePreset::Abyss {
            Self::abyss(hue)
        } else {
            Self::light(preset, hue)
        }
    }

    /// 浅色分支：`hueStyle()` 的 else 分支 + 样式表最后一遍覆写里没被内联覆盖的部分。
    fn light(preset: ThemePreset, hue: f32) -> Self {
        let h2 = hue + 42.0;
        Self {
            preset,
            hue,

            // 内联 style 提供：--dsp-bg / --dsp-canvas / --dsp-sidebar / --dsp-line*
            bg: hsl_deg(hue, 42.0, 97.0, 1.0),
            canvas: hsl_deg(hue, 52.0, 99.0, 1.0),
            sidebar: hsl_deg(hue, 48.0, 94.0, 0.86),
            line: hsl_deg(hue, 36.0, 26.0, 0.09),
            line_strong: hsl_deg(hue, 44.0, 28.0, 0.16),

            // 内联未覆盖 → 样式表最后一遍：纯白卡片 + 冷灰文字三档
            card: rgb(0xffffff).into(),
            card_strong: rgb(0xffffff).into(),
            text: rgb(0x2f3034).into(),
            muted: rgb(0x8b99a9).into(),
            faint: rgb(0xb9c2cd).into(),

            accent: hsl_deg(hue, 92.0, 55.0, 1.0),
            accent_2: hsl_deg(hue + 28.0, 92.0, 64.0, 1.0),
            accent_soft: hsl_deg(hue, 92.0, 55.0, 0.12),

            chat_wash: hsl_deg(hue, 72.0, 58.0, 0.045),
            chat_bubble_a: hsl_deg(hue, 80.0, 62.0, 0.12),
            chat_bubble_b: hsl_deg(h2, 76.0, 64.0, 0.10),
            chat_bubble_c: hsl_deg(hue + 148.0, 58.0, 66.0, 0.08),
            chat_bubble_d: hsl_deg(hue + 318.0, 70.0, 68.0, 0.08),
            chat_panel: hsl_deg(hue, 58.0, 99.0, 0.7),
            chat_panel_end: rgba(0xffffff47).into(), // rgba(255,255,255,0.28)

            right_bg_top: hsl_deg(hue, 52.0, 98.0, 0.82),
            right_bg_bottom: hsl_deg(hue, 42.0, 95.0, 0.72),
            right_card: hsl_deg(hue, 44.0, 99.0, 0.88),

            user_bubble_a: hsl_deg(hue, 92.0, 55.0, 0.1),
            user_bubble_b: hsl_deg(hue, 52.0, 99.0, 0.94),
            user_line: hsl_deg(hue, 92.0, 55.0, 0.18),

            green: rgb(0x21a873).into(),
            amber: rgb(0xd89216).into(),
            danger: rgb(0xe05252).into(),

            hue_popover_bg: rgba(0xfafdfff5).into(),
            hue_button_bg: rgba(0xffffff7a).into(),
            theme_preset_bg: rgba(0xffffff8f).into(),
            ring_core: rgba(0xfafdfff5).into(),

            surface_veil: rgba(0xffffffb8).into(), // rgba(255,255,255,0.72) 侧栏卡片底
            card_line: rgba(0x203648_14).into(),   // rgba(32,54,78,0.08)
            session_title: rgb(0x5c6a7a).into(),
            input_text: rgb(0x2f3034).into(),
            input_placeholder: rgb(0xb8c1cb).into(),
            new_chat_bg_a: rgb(0xffffff).into(),
            new_chat_bg_b: rgb(0xedf5fc).into(),
            new_chat_text: rgb(0x47637f).into(),
            new_chat_line: rgba(0x4a93e1_29).into(),
            tab_active_bg: rgb(0xffffff).into(),
            tab_active_text: rgb(0x243041).into(),
            tab_text: rgb(0x7f8b9b).into(),
            tabs_bg: rgba(0xffffffb8).into(),
        }
    }

    /// 深色分支：`hueStyle()` 的 `themeId === "abyss"` 分支。
    fn abyss(hue: f32) -> Self {
        Self {
            preset: ThemePreset::Abyss,
            hue,

            bg: rgb(0x07111c).into(),
            canvas: rgb(0x091522).into(),
            sidebar: rgba(0x070f19_e6).into(), // rgb(7 15 25 / 0.9)
            card: rgba(0x111f30_b8).into(),    // rgb(17 31 48 / 0.72)
            card_strong: rgba(0x101d2d_f0).into(),
            line: rgba(0x8eb4da_24).into(),
            line_strong: rgba(0x97c4ed_3d).into(),
            text: rgb(0xe8f2ff).into(),
            muted: rgb(0x93a8bf).into(),
            faint: rgb(0x61748a).into(),

            accent: hsl_deg(hue, 94.0, 66.0, 1.0),
            accent_2: hsl_deg(hue + 42.0, 86.0, 72.0, 1.0),
            accent_soft: hsl_deg(hue, 94.0, 66.0, 0.18),

            chat_wash: hsl_deg(hue, 92.0, 64.0, 0.08),
            chat_bubble_a: hsl_deg(hue, 90.0, 66.0, 0.16),
            chat_bubble_b: hsl_deg(hue + 42.0, 82.0, 70.0, 0.13),
            chat_bubble_c: hsl_deg(hue + 142.0, 58.0, 64.0, 0.1),
            chat_bubble_d: hsl_deg(hue + 308.0, 72.0, 68.0, 0.1),
            chat_panel: rgba(0x08121d_b8).into(),
            chat_panel_end: rgba(0x060e17_61).into(),

            right_bg_top: rgba(0x091420_eb).into(),
            right_bg_bottom: rgba(0x060e17_d1).into(),
            right_card: rgba(0x0f1d2d_e6).into(),

            user_bubble_a: hsl_deg(hue, 92.0, 62.0, 0.2),
            user_bubble_b: rgba(0x0e1c2c_f2).into(),
            user_line: hsl_deg(hue, 92.0, 66.0, 0.26),

            green: rgb(0x48c78e).into(),
            amber: rgb(0xd6a44c).into(),
            danger: rgb(0xf06f72).into(),

            hue_popover_bg: rgba(0x0a1522_f5).into(),
            hue_button_bg: rgba(0x122132_b8).into(),
            theme_preset_bg: rgba(0x122132_a8).into(),
            ring_core: rgba(0x0a1522_f5).into(),

            surface_veil: rgba(0x0b1725_bd).into(), // rgb(11 23 37 / 0.74)
            card_line: rgba(0x8eb4da_24).into(),
            session_title: rgb(0x93a8bf).into(),
            input_text: rgb(0xe8f2ff).into(),
            input_placeholder: rgb(0x61748a).into(),
            new_chat_bg_a: rgba(0x14263a_d1).into(),
            new_chat_bg_b: rgba(0x0c1a2a_e6).into(),
            new_chat_text: rgb(0xe8f2ff).into(),
            new_chat_line: hsl_deg(hue, 92.0, 66.0, 0.26),
            tab_active_bg: rgba(0x14263a_e0).into(),
            tab_active_text: rgb(0xe8f2ff).into(),
            tab_text: rgb(0x93a8bf).into(),
            tabs_bg: rgba(0x08111d_b8).into(),
        }
    }

    pub fn is_dark(&self) -> bool {
        self.preset.is_dark()
    }

    /// 会话行 hover 底色：CSS `color-mix(in srgb, var(--dsp-accent) 5%, transparent)`。
    pub fn session_hover(&self) -> Hsla {
        let mut c = self.accent;
        c.a = 0.05;
        c
    }

    /// 会话行选中底色：`color-mix(... 7%, white)`——与 hover 不同，它混的是白而非透明。
    pub fn session_active(&self) -> Hsla {
        mix_with(self.accent, if self.is_dark() { self.card } else { gpui::white() }, 0.07)
    }

    pub fn session_active_ring(&self) -> Hsla {
        let mut c = self.accent;
        c.a = 0.12;
        c
    }
}

/// CSS `color-mix(in srgb, a p%, b)`：按比例混两色（alpha 一并混）。
pub fn mix_with(a: Hsla, b: Hsla, p: f32) -> Hsla {
    let ra = gpui::Rgba::from(a);
    let rb = gpui::Rgba::from(b);
    gpui::Rgba {
        r: rb.r + (ra.r - rb.r) * p,
        g: rb.g + (ra.g - rb.g) * p,
        b: rb.b + (ra.b - rb.b) * p,
        a: rb.a + (ra.a - rb.a) * p,
    }
    .into()
}

/// 给定 alpha 的同色变体，等价 CSS 里的 `hsl(... / α)` 写法。
pub fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    let mut c = color;
    c.a = alpha;
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    /// hueStyle(208, glacier) 的 accent 必须落在 #2393f6 —— 这是原 UI 里
    /// 所有蓝色（新建按钮、选中态、状态点）的来源，算错整套配色就偏了。
    #[test]
    fn glacier_accent_matches_css() {
        let theme = Theme::new(ThemePreset::Glacier, 208.0);
        let rgba = gpui::Rgba::from(theme.accent);
        assert_eq!((rgba.r * 255.0).round() as u8, 35);
        assert_eq!((rgba.g * 255.0).round() as u8, 147);
        assert_eq!((rgba.b * 255.0).round() as u8, 246);
    }

    #[test]
    fn hue_wraps_past_360() {
        // chat_bubble_d 用 hue+318，208+318=526 → 必须回绕到 166 而不是溢出。
        let theme = Theme::new(ThemePreset::Glacier, 208.0);
        assert!((theme.chat_bubble_d.h - 166.0 / 360.0).abs() < 1e-4);
    }
}
