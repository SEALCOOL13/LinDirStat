//! UI flavors (egui visuals) + matching treemap palettes.
use egui::{Color32, Context, Stroke, Theme as EguiTheme, ThemePreference, Visuals};

pub type Rgb = [u8; 3];

pub fn hex(s: &str) -> Rgb {
    let s = s.trim_start_matches('#');
    let v = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0);
    [v(0), v(2), v(4)]
}
fn c(s: &str) -> Color32 {
    let [r, g, b] = hex(s);
    Color32::from_rgb(r, g, b)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    System,
    Light,
    Dark,
    Custom,
}

pub struct Theme {
    pub name: &'static str,
    pub kind: Kind,
    pub bg: Color32,
    pub bg2: Color32,
    pub bg3: Color32,
    pub fg: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub selbg: Color32,
    pub selfg: Color32,
    pub other: Rgb,
    pub tbg: Rgb,
    pub palette: Option<Vec<Rgb>>,
}

fn basic(name: &'static str, kind: Kind, tbg: Rgb) -> Theme {
    Theme {
        name,
        kind,
        bg: Color32::TRANSPARENT,
        bg2: Color32::TRANSPARENT,
        bg3: Color32::TRANSPARENT,
        fg: Color32::WHITE,
        border: Color32::GRAY,
        accent: Color32::from_rgb(70, 130, 220),
        selbg: Color32::from_rgb(40, 90, 160),
        selfg: Color32::WHITE,
        other: [140, 140, 140],
        tbg,
        palette: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn custom(
    name: &'static str,
    bg: &str,
    bg2: &str,
    bg3: &str,
    fg: &str,
    _dim: &str,
    border: &str,
    accent: &str,
    selbg: &str,
    selfg: &str,
    other: &str,
    palette: &str,
) -> Theme {
    Theme {
        name,
        kind: Kind::Custom,
        bg: c(bg),
        bg2: c(bg2),
        bg3: c(bg3),
        fg: c(fg),
        border: c(border),
        accent: c(accent),
        selbg: c(selbg),
        selfg: c(selfg),
        other: hex(other),
        tbg: hex(bg2),
        palette: Some(palette.split_whitespace().map(hex).collect()),
    }
}

pub fn all_themes() -> Vec<Theme> {
    vec![
        basic("System default", Kind::System, [32, 32, 32]),
        basic("Light", Kind::Light, [235, 235, 235]),
        basic("Dark", Kind::Dark, [30, 30, 30]),
        custom("Catppuccin Mocha", "#1e1e2e", "#181825", "#313244", "#cdd6f4", "#a6adc8", "#45475a", "#89b4fa", "#45475a", "#cdd6f4", "#7f849c",
               "89b4fa f38ba8 a6e3a1 f9e2af cba6f7 94e2d5 fab387 f5c2e7 74c7ec b4befe eba0ac 89dceb"),
        custom("Catppuccin Macchiato", "#24273a", "#1e2030", "#363a4f", "#cad3f5", "#a5adcb", "#494d64", "#8aadf4", "#494d64", "#cad3f5", "#8087a2",
               "8aadf4 ed8796 a6da95 eed49f c6a0f6 8bd5ca f5a97f f5bde6 7dc4e4 b7bdf8 ee99a0 91d7e3"),
        custom("Catppuccin Frappé", "#303446", "#292c3c", "#414559", "#c6d0f5", "#a5adce", "#51576d", "#8caaee", "#51576d", "#c6d0f5", "#838ba7",
               "8caaee e78284 a6d189 e5c890 ca9ee6 81c8be ef9f76 f4b8e4 85c1dc babbf1 ea999c 99d1db"),
        custom("Dracula", "#282a36", "#21222c", "#44475a", "#f8f8f2", "#bdc0d0", "#44475a", "#bd93f9", "#44475a", "#f8f8f2", "#6272a4",
               "bd93f9 ff5555 50fa7b f1fa8c 8be9fd ffb86c ff79c6 d6acff ff6e6e 69ff94 a4ffff ffffa5"),
        custom("Nord", "#2e3440", "#272c36", "#3b4252", "#d8dee9", "#a3acbd", "#434c5e", "#88c0d0", "#434c5e", "#eceff4", "#616e88",
               "88c0d0 bf616a a3be8c ebcb8b b48ead 8fbcbb d08770 81a1c1 5e81ac d98891 c0d4ae f3deb0"),
        custom("Gruvbox Dark", "#282828", "#1d2021", "#3c3836", "#ebdbb2", "#a89984", "#504945", "#fabd2f", "#504945", "#ebdbb2", "#928374",
               "83a598 fb4934 b8bb26 fabd2f d3869b 8ec07c fe8019 458588 cc241d 98971a d79921 b16286"),
        custom("Tokyo Night", "#1a1b26", "#16161e", "#292e42", "#c0caf5", "#a9b1d6", "#3b4261", "#7aa2f7", "#33467c", "#c0caf5", "#565f89",
               "7aa2f7 f7768e 9ece6a e0af68 bb9af7 73daca ff9e64 7dcfff 2ac3de b4f9f8 3d59a1 db4b4b"),
        custom("One Dark", "#282c34", "#21252b", "#2c313c", "#abb2bf", "#8b93a1", "#3e4451", "#61afef", "#3e4451", "#d7dae0", "#5c6370",
               "61afef e06c75 98c379 e5c07b c678dd 56b6c2 d19a66 be5046 4d78cc 89ca78 e8d09a a9b1d6"),
        custom("Solarized Dark", "#002b36", "#00212b", "#073642", "#93a1a1", "#839496", "#0f4a59", "#268bd2", "#0f4a59", "#eee8d5", "#586e75",
               "268bd2 dc322f 859900 b58900 6c71c4 2aa198 cb4b16 d33682 5fb0e8 e8645f a4b82a 5ec4bb"),
    ]
}

pub const CLASSIC_BASE: [Rgb; 12] = [
    [0, 0, 255], [255, 0, 0], [0, 255, 0], [0, 255, 255], [255, 0, 255], [255, 255, 0],
    [150, 150, 255], [255, 150, 150], [150, 255, 150], [150, 255, 255], [255, 150, 255], [255, 255, 150],
];

/// WinDirStat's palette normalisation (equal perceived brightness).
pub fn classic_palette() -> Vec<Rgb> {
    CLASSIC_BASE
        .iter()
        .map(|c| {
            let mut r = [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0];
            let total: f32 = r.iter().sum::<f32>().max(1e-6);
            for v in r.iter_mut() {
                *v *= 3.0 * 0.6 / total;
            }
            for _ in 0..3 {
                let over: f32 = r.iter().map(|v| (v - 1.0).max(0.0)).sum();
                if over <= 0.0 {
                    break;
                }
                for v in r.iter_mut() {
                    *v = v.min(1.0);
                }
                let free: Vec<usize> = (0..3).filter(|&i| r[i] < 1.0).collect();
                if free.is_empty() {
                    break;
                }
                for i in &free {
                    r[*i] += over / free.len() as f32;
                }
            }
            [(r[0] * 1.15).min(1.0) * 255.0, (r[1] * 1.15).min(1.0) * 255.0, (r[2] * 1.15).min(1.0) * 255.0].map(|v| v as u8)
        })
        .collect()
}

pub fn apply(ctx: &Context, t: &Theme) {
    match t.kind {
        Kind::System => {
            ctx.set_visuals_of(EguiTheme::Dark, Visuals::dark());
            ctx.set_visuals_of(EguiTheme::Light, Visuals::light());
            ctx.set_theme(ThemePreference::System);
        }
        Kind::Light => {
            ctx.set_visuals_of(EguiTheme::Light, Visuals::light());
            ctx.set_theme(ThemePreference::Light);
        }
        Kind::Dark => {
            ctx.set_visuals_of(EguiTheme::Dark, Visuals::dark());
            ctx.set_theme(ThemePreference::Dark);
        }
        Kind::Custom => {
            let mut v = Visuals::dark();
            v.override_text_color = Some(t.fg);
            v.panel_fill = t.bg;
            v.window_fill = t.bg2;
            v.extreme_bg_color = t.bg2;
            v.faint_bg_color = t.bg3.gamma_multiply(0.45);
            v.window_stroke = Stroke::new(1.0_f32, t.border);
            v.hyperlink_color = t.accent;
            v.widgets.noninteractive.bg_fill = t.bg;
            v.widgets.noninteractive.weak_bg_fill = t.bg;
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, t.border);
            v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, t.fg);
            v.widgets.inactive.bg_fill = t.bg3;
            v.widgets.inactive.weak_bg_fill = t.bg3;
            v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, t.border);
            v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, t.fg);
            v.widgets.hovered.bg_fill = t.selbg;
            v.widgets.hovered.weak_bg_fill = t.selbg;
            v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, t.accent);
            v.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, t.selfg);
            // note: egui also uses widgets.active.fg_stroke for *strong* text, so it must stay readable on bg
            v.widgets.active.bg_fill = t.selbg;
            v.widgets.active.weak_bg_fill = t.selbg;
            v.widgets.active.bg_stroke = Stroke::new(1.0_f32, t.accent);
            v.widgets.active.fg_stroke = Stroke::new(2.0_f32, t.fg);
            v.widgets.open.bg_fill = t.bg3;
            v.widgets.open.weak_bg_fill = t.bg3;
            v.widgets.open.fg_stroke = Stroke::new(1.0_f32, t.fg);
            v.selection.bg_fill = t.selbg;
            v.selection.stroke = Stroke::new(1.0_f32, t.selfg);
            ctx.set_visuals_of(EguiTheme::Dark, v);
            ctx.set_theme(ThemePreference::Dark);
        }
    }
}
