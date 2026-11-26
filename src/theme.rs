//! macOS-inspired look: Apple system colours, rounded cards, soft shadows,
//! toggle switches and crisp vector icons. Follows the system light/dark
//! appearance.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Pos2, Rect, Response, Sense, Shadow,
    Shape, Stroke, TextStyle, Theme, Ui, Vec2, pos2, vec2,
};

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub sidebar: Color32,
    pub card: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub tertiary: Color32,
    pub separator: Color32,
    pub accent: Color32,
    pub green: Color32,
    pub orange: Color32,
    pub red: Color32,
    pub track: Color32,
    pub shadow: Color32,
    /// Selected sidebar row / tile background.
    pub select: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const LIGHT: Palette = Palette {
    bg: hex(0xFFFFFF),
    sidebar: hex(0xF2F2F4),
    card: hex(0xF6F6F8),
    control: hex(0xEDEDF0),
    control_hover: hex(0xE2E2E6),
    text: hex(0x1D1D1F),
    secondary: hex(0x6E6E73),
    tertiary: hex(0xA1A1A6),
    separator: hex(0xD8D8DD),
    accent: hex(0x007AFF),
    green: hex(0x34C759),
    orange: hex(0xFF9500),
    red: hex(0xFF3B30),
    track: hex(0xD1D1D6),
    shadow: Color32::from_black_alpha(22),
    select: hex(0xE1E1E6),
};

pub const DARK: Palette = Palette {
    bg: hex(0x1E1E1E),
    sidebar: hex(0x2A2A2C),
    card: hex(0x29292B),
    control: hex(0x323234),
    control_hover: hex(0x3E3E41),
    text: hex(0xF5F5F7),
    secondary: hex(0x98989D),
    tertiary: hex(0x6C6C70),
    separator: hex(0x3A3A3C),
    accent: hex(0x0A84FF),
    green: hex(0x30D158),
    orange: hex(0xFF9F0A),
    red: hex(0xFF453A),
    track: hex(0x4A4A4D),
    shadow: Color32::from_black_alpha(90),
    select: hex(0x3A3A3D),
};

pub fn palette(ui: &Ui) -> Palette {
    if ui.visuals().dark_mode { DARK } else { LIGHT }
}

pub fn install(ctx: &egui::Context) {
    // Follows the system appearance; RUSTORRENT_THEME=light|dark overrides it.
    match std::env::var("RUSTORRENT_THEME").as_deref() {
        Ok("light") => ctx.set_theme(egui::ThemePreference::Light),
        Ok("dark") => ctx.set_theme(egui::ThemePreference::Dark),
        _ => {}
    }
    for (theme, p) in [(Theme::Light, LIGHT), (Theme::Dark, DARK)] {
        ctx.style_mut_of(theme, |style| {
            style.text_styles = [
                (
                    TextStyle::Small,
                    FontId::new(11.5, FontFamily::Proportional),
                ),
                (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
                (
                    TextStyle::Button,
                    FontId::new(13.5, FontFamily::Proportional),
                ),
                (
                    TextStyle::Heading,
                    FontId::new(20.0, FontFamily::Proportional),
                ),
                (
                    TextStyle::Monospace,
                    FontId::new(12.5, FontFamily::Monospace),
                ),
            ]
            .into();

            let s = &mut style.spacing;
            s.item_spacing = vec2(8.0, 6.0);
            s.button_padding = vec2(12.0, 5.0);
            s.interact_size.y = 26.0;
            s.window_margin = Margin::same(20);
            s.scroll.bar_width = 6.0;

            let v = &mut style.visuals;
            v.panel_fill = p.bg;
            v.window_fill = p.card;
            v.window_stroke = Stroke::new(1.0, p.separator);
            v.window_corner_radius = CornerRadius::same(14);
            v.window_shadow = Shadow {
                offset: [0, 12],
                blur: 40,
                spread: 0,
                color: p.shadow,
            };
            v.popup_shadow = Shadow {
                offset: [0, 4],
                blur: 16,
                spread: 0,
                color: p.shadow,
            };
            v.menu_corner_radius = CornerRadius::same(10);
            v.extreme_bg_color = p.card;
            v.text_edit_bg_color = Some(if v.dark_mode { hex(0x1C1C1E) } else { p.card });
            v.faint_bg_color = p.control;
            v.hyperlink_color = p.accent;
            v.warn_fg_color = p.orange;
            v.error_fg_color = p.red;
            v.override_text_color = Some(p.text);
            v.weak_text_color = Some(p.secondary);
            v.selection.bg_fill = p.accent.gamma_multiply(0.35);
            v.selection.stroke = Stroke::new(1.0, p.accent);
            v.striped = false;

            let w = &mut v.widgets;
            let r = CornerRadius::same(7);
            w.noninteractive.bg_stroke = Stroke::new(1.0, p.separator);
            w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
            w.noninteractive.corner_radius = r;
            for (wv, fill) in [
                (&mut w.inactive, p.control),
                (&mut w.hovered, p.control_hover),
                (&mut w.active, p.control_hover),
                (&mut w.open, p.control),
            ] {
                wv.bg_fill = fill;
                wv.weak_bg_fill = fill;
                wv.bg_stroke = Stroke::NONE;
                wv.fg_stroke = Stroke::new(1.0, p.text);
                wv.corner_radius = r;
                wv.expansion = 0.0;
            }
            w.hovered.bg_stroke = Stroke::NONE;
            w.active.expansion = 0.0;
        });
    }
}

/// A white-card group, like a macOS settings section.
pub fn card(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.card)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(16, 12))
        .shadow(Shadow {
            offset: [0, 1],
            blur: 6,
            spread: 0,
            color: p.shadow.gamma_multiply(0.6),
        })
}

/// Filled accent ("default") button.
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    let p = palette(ui);
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(Color32::WHITE).strong())
            .fill(p.accent)
            .corner_radius(CornerRadius::same(7)),
    )
}

/// Destructive (red) button.
pub fn danger_button(ui: &mut Ui, text: &str) -> Response {
    let p = palette(ui);
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(Color32::WHITE).strong())
            .fill(p.red)
            .corner_radius(CornerRadius::same(7)),
    )
}

/// macOS-style toggle switch.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let p = palette(ui);
    let size = vec2(38.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let bg = lerp_color(p.control, p.green, t);
        ui.painter().rect_filled(rect, CornerRadius::same(11), bg);
        let r = rect.height() / 2.0 - 2.0;
        let x = egui::lerp((rect.left() + r + 2.0)..=(rect.right() - r - 2.0), t);
        let c = pos2(x, rect.center().y);
        ui.painter()
            .circle_filled(c + vec2(0.0, 1.0), r, Color32::from_black_alpha(40));
        ui.painter().circle_filled(c, r, Color32::WHITE);
    }
    response
}

/// Slim rounded progress bar.
pub fn progress_bar(ui: &mut Ui, fraction: f32, color: Color32) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), p.track);
    let f = fraction.clamp(0.0, 1.0);
    if f > 0.0 {
        let mut fill = rect;
        fill.set_width((rect.width() * f).max(6.0));
        painter.rect_filled(fill, CornerRadius::same(3), color);
    }
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Pause,
    Play,
    Folder,
    Close,
    Gear,
    Down,
    Up,
    List,
    Grid,
    Warning,
    Check,
}

/// Draw an icon centred in `rect`.
pub fn paint_icon(ui: &Ui, rect: Rect, icon: Icon, color: Color32) {
    let painter = ui.painter();
    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.5; // half-size
    let stroke = Stroke::new((s * 0.16).max(1.4), color);
    match icon {
        Icon::Pause => {
            let w = s * 0.28;
            let h = s * 0.95;
            for dx in [-s * 0.3, s * 0.3] {
                painter.rect_filled(
                    Rect::from_center_size(c + vec2(dx, 0.0), vec2(w, h * 2.0 * 0.5 * 1.6)),
                    CornerRadius::same(1),
                    color,
                );
            }
        }
        Icon::Play => {
            let h = s * 0.8;
            painter.add(Shape::convex_polygon(
                vec![
                    c + vec2(-h * 0.55, -h),
                    c + vec2(h * 0.95, 0.0),
                    c + vec2(-h * 0.55, h),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Folder => {
            let w = s * 1.5;
            let h = s * 1.05;
            let body = Rect::from_center_size(c + vec2(0.0, s * 0.1), vec2(w, h));
            let tab = Rect::from_min_size(
                body.left_top() + vec2(0.0, -s * 0.25),
                vec2(w * 0.42, s * 0.4),
            );
            painter.rect_filled(tab, CornerRadius::same(2), color);
            painter.rect_filled(body, CornerRadius::same(2), color);
        }
        Icon::Close => {
            let d = s * 0.55;
            painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], stroke);
            painter.line_segment([c + vec2(d, -d), c + vec2(-d, d)], stroke);
        }
        Icon::Gear => {
            let teeth = 8;
            for i in 0..teeth {
                let a = i as f32 / teeth as f32 * std::f32::consts::TAU;
                let dir = vec2(a.cos(), a.sin());
                painter.line_segment([c + dir * s * 0.55, c + dir * s * 0.9], stroke);
            }
            painter.circle_stroke(c, s * 0.55, stroke);
            painter.circle_filled(c, s * 0.2, color);
        }
        Icon::Down | Icon::Up => {
            let sign = if icon == Icon::Down { 1.0 } else { -1.0 };
            let tip = c + vec2(0.0, sign * s * 0.75);
            painter.line_segment([c + vec2(0.0, -sign * s * 0.75), tip], stroke);
            painter.line_segment([tip, tip + vec2(-s * 0.5, -sign * s * 0.5)], stroke);
            painter.line_segment([tip, tip + vec2(s * 0.5, -sign * s * 0.5)], stroke);
        }
        Icon::List => {
            for dy in [-0.5, 0.0, 0.5] {
                let y = c.y + dy * s * 1.2;
                painter.circle_filled(pos2(c.x - s * 0.65, y), stroke.width * 0.7, color);
                painter.line_segment([pos2(c.x - s * 0.3, y), pos2(c.x + s * 0.75, y)], stroke);
            }
        }
        Icon::Grid => {
            let g = s * 0.36;
            for dx in [-1.0, 1.0] {
                for dy in [-1.0, 1.0] {
                    painter.rect_filled(
                        Rect::from_center_size(
                            c + vec2(dx * g * 1.25, dy * g * 1.25),
                            Vec2::splat(g * 1.6),
                        ),
                        CornerRadius::same(2),
                        color,
                    );
                }
            }
        }
        Icon::Check => {
            painter.add(Shape::line(
                vec![
                    c + vec2(-s * 0.6, 0.0),
                    c + vec2(-s * 0.15, s * 0.5),
                    c + vec2(s * 0.65, -s * 0.5),
                ],
                Stroke::new(stroke.width * 1.3, color),
            ));
        }
        Icon::Warning => {
            painter.circle_stroke(c, s * 0.8, stroke);
            painter.line_segment([c + vec2(0.0, -s * 0.4), c + vec2(0.0, s * 0.1)], stroke);
            painter.circle_filled(c + vec2(0.0, s * 0.38), stroke.width * 0.7, color);
        }
    }
}

/// Small circular icon button that only shows a background on hover.
pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str) -> Response {
    let p = palette(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    if ui.is_rect_visible(rect) {
        if response.hovered() {
            ui.painter()
                .circle_filled(rect.center(), 14.0, p.control_hover);
        }
        let color = if response.hovered() {
            p.text
        } else {
            p.secondary
        };
        paint_icon(ui, rect.shrink(6.5), icon, color);
    }
    response.on_hover_text(tooltip)
}

/// Tiny area chart of recent speeds.
pub fn sparkline(ui: &mut Ui, values: &[f64], color: Color32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    if values.len() < 2 {
        return;
    }
    let max = values.iter().cloned().fold(1.0, f64::max);
    let n = values.len() - 1;
    let pts: Vec<Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            pos2(
                rect.left() + rect.width() * i as f32 / n as f32,
                rect.bottom() - rect.height() * (*v / max) as f32,
            )
        })
        .collect();
    let painter = ui.painter();
    // Filled area under the line, one quad per segment.
    let fill = color.gamma_multiply(0.18);
    for w in pts.windows(2) {
        painter.add(Shape::convex_polygon(
            vec![
                w[0],
                w[1],
                pos2(w[1].x, rect.bottom()),
                pos2(w[0].x, rect.bottom()),
            ],
            fill,
            Stroke::NONE,
        ));
    }
    painter.add(Shape::line(pts, Stroke::new(1.5, color)));
}

/// Rounded "pill" behind a group of toolbar buttons, like macOS toolbars.
pub fn capsule<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.control)
        .corner_radius(CornerRadius::same(17))
        .stroke(Stroke::new(1.0, p.separator.gamma_multiply(0.6)))
        .inner_margin(Margin::symmetric(4, 2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.horizontal(add).inner
        })
        .inner
}

/// Icon button for use inside a capsule; `active` draws a selected state.
pub fn capsule_button(ui: &mut Ui, icon: Icon, tooltip: &str, active: bool) -> Response {
    let p = palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(34.0, 28.0), Sense::click());
    if ui.is_rect_visible(rect) {
        if active || response.hovered() {
            let fill = if active { p.select } else { p.control_hover };
            ui.painter()
                .rect_filled(rect.shrink(1.0), CornerRadius::same(13), fill);
        }
        let color = if active || response.hovered() {
            p.text
        } else {
            p.secondary
        };
        paint_icon(
            ui,
            Rect::from_center_size(rect.center(), Vec2::splat(15.0)),
            icon,
            color,
        );
    }
    response.on_hover_text(tooltip)
}

/// Rounded-square "app icon" tile with a glyph, used for torrents.
pub fn app_icon(ui: &Ui, rect: Rect, color: Color32, glyph: Icon) {
    let painter = ui.painter();
    let r = CornerRadius::same((rect.width() * 0.23) as u8);
    painter.rect_filled(
        rect.translate(vec2(0.0, 2.0)),
        r,
        Color32::from_black_alpha(50),
    );
    painter.rect_filled(rect, r, color);
    // Soft top sheen for depth.
    let mut sheen = rect;
    sheen.set_height(rect.height() * 0.5);
    painter.rect_filled(
        sheen,
        CornerRadius {
            nw: r.nw,
            ne: r.ne,
            sw: 0,
            se: 0,
        },
        Color32::from_white_alpha(28),
    );
    paint_icon(ui, rect.shrink(rect.width() * 0.28), glyph, Color32::WHITE);
}
