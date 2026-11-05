//! The two station palettes from the chosen design, and the Franklin Gothic faces when
//! Windows has them installed. The font files stay on the machine; nothing is bundled.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, Stroke};

use crate::settings::ThemeChoice;

static CONDENSED: AtomicBool = AtomicBool::new(false);

/// Colors for one station. Text on the housing is `body`; text on a paper plate is `ink`.
#[derive(Clone, Copy)]
pub struct Station {
    pub dark: bool,
    pub housing: Color32,
    pub key: Color32,
    pub key_hover: Color32,
    pub paper: Color32,
    pub ink: Color32,
    pub body: Color32,
    pub muted: Color32,
    pub well: Color32,
    pub lamp: Color32,
    pub line: Color32,
    pub line_w: f32,
    pub strong_fill: Color32,
    pub strong_ink: Color32,
    pub stamp: Color32,
    pub title_bar: Color32,
    pub title_ink: Color32,
}

impl Station {
    pub fn dark() -> Self {
        Self {
            dark: true,
            housing: rgb(0x0E, 0x12, 0x10),
            key: rgb(0x1A, 0x20, 0x1D),
            key_hover: rgb(0x24, 0x2B, 0x27),
            paper: rgb(0xE4, 0xDC, 0xC8),
            ink: rgb(0x1C, 0x1E, 0x1B),
            body: rgb(0xE4, 0xDC, 0xC8),
            muted: rgb(0xB7, 0xB1, 0xA4),
            well: rgb(0x08, 0x0A, 0x09),
            lamp: rgb(0xE2, 0x3B, 0x2F),
            line: rgb(0x3A, 0x44, 0x40),
            line_w: 0.0,
            strong_fill: rgb(0xE4, 0xDC, 0xC8),
            strong_ink: rgb(0x1A, 0x1C, 0x19),
            stamp: rgb(0xD7, 0xCF, 0xBE),
            title_bar: rgb(0x1A, 0x22, 0x2C),
            title_ink: rgb(0xC9, 0xD0, 0xDC),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            housing: rgb(0xE6, 0xE1, 0xD6),
            key: rgb(0xF7, 0xF4, 0xEE),
            key_hover: rgb(0xEF, 0xEB, 0xE3),
            paper: rgb(0xFB, 0xF9, 0xF4),
            ink: rgb(0x1C, 0x1E, 0x1B),
            body: rgb(0x1C, 0x1E, 0x1B),
            muted: rgb(0x4A, 0x46, 0x3E),
            well: rgb(0x08, 0x0A, 0x09),
            lamp: rgb(0xE2, 0x3B, 0x2F),
            line: rgb(0xC4, 0xBD, 0xB0),
            line_w: 1.0,
            strong_fill: rgb(0x1A, 0x1C, 0x19),
            strong_ink: rgb(0xF7, 0xF4, 0xEE),
            stamp: rgb(0xD7, 0xCF, 0xBE),
            title_bar: rgb(0xED, 0xE9, 0xE0),
            title_ink: rgb(0x1C, 0x1E, 0x1B),
        }
    }

    /// The stroke around a selected thumbnail: paper on the dark station, ink on the light one.
    pub fn selected_stroke(&self) -> Stroke {
        Stroke::new(3.0, if self.dark { self.paper } else { self.ink })
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// Points both of egui's themes at the station palettes and records the saved preference.
pub fn apply(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.options_mut(|o| {
        o.theme_preference = match choice {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
            ThemeChoice::Light => egui::ThemePreference::Light,
        };
        o.dark_style = Arc::new(style(Station::dark()));
        o.light_style = Arc::new(style(Station::light()));
    });
}

fn style(station: Station) -> egui::Style {
    let mut style = egui::Style {
        visuals: if station.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        },
        ..Default::default()
    };
    let body = Stroke::new(1.0, station.body);
    let line = Stroke::new(station.line_w, station.line);
    let v = &mut style.visuals;
    v.dark_mode = station.dark;
    v.window_fill = station.housing;
    v.panel_fill = station.housing;
    // The preview well stays near-black in both themes. Typed text uses the ink color,
    // so a light station's field has to stay paper or the letters disappear on it.
    v.extreme_bg_color = station.well;
    v.text_edit_bg_color = Some(if station.dark {
        station.well
    } else {
        station.paper
    });
    v.faint_bg_color = station.key;
    v.widgets.noninteractive.bg_fill = station.housing;
    v.widgets.noninteractive.fg_stroke = body;
    v.widgets.noninteractive.bg_stroke = line;
    v.widgets.inactive.bg_fill = station.key;
    v.widgets.inactive.weak_bg_fill = station.key;
    v.widgets.inactive.fg_stroke = body;
    v.widgets.inactive.bg_stroke = line;
    v.widgets.hovered.bg_fill = station.key_hover;
    v.widgets.hovered.weak_bg_fill = station.key_hover;
    v.widgets.hovered.fg_stroke = body;
    v.widgets.hovered.bg_stroke = line;
    v.widgets.active.bg_fill = station.strong_fill;
    v.widgets.active.weak_bg_fill = station.strong_fill;
    v.widgets.active.fg_stroke = Stroke::new(1.0, station.strong_ink);
    v.widgets.open.bg_fill = station.paper;
    v.widgets.open.weak_bg_fill = station.paper;
    v.widgets.open.fg_stroke = Stroke::new(1.0, station.ink);
    // Selected text is drawn in the selection stroke, so the fill must carry ink in both themes.
    v.selection.bg_fill = station.stamp;
    v.selection.stroke = Stroke::new(1.0, station.ink);
    v.hyperlink_color = station.body;
    v.error_fg_color = station.lamp;
    v.window_stroke = line;
    let radius = egui::CornerRadius::same(2);
    v.widgets.noninteractive.corner_radius = radius;
    v.widgets.inactive.corner_radius = radius;
    v.widgets.hovered.corner_radius = radius;
    v.widgets.active.corner_radius = radius;
    style
}

/// Uses Franklin Gothic Book for body text, and Medium Condensed for the small labels, when
/// those faces are installed. Otherwise egui's own font stays.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let dir = Path::new(r"C:\Windows\Fonts");
    let Some(book) = read_face(dir, &["FRABK.TTF", "frabk.ttf"]) else {
        return;
    };
    // egui's own faces stay behind Franklin for the glyphs it lacks, such as ⚠ and non-Latin names.
    let fallbacks = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fonts.font_data.insert(
        "FranklinBook".into(),
        Arc::new(egui::FontData::from_owned(book)),
    );
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        family.insert(0, "FranklinBook".into());
    }
    if let Some(cond) = read_face(dir, &["FRAMDCN.TTF", "framdcn.ttf", "FRADMCN.TTF"]) {
        fonts.font_data.insert(
            "FranklinCond".into(),
            Arc::new(egui::FontData::from_owned(cond)),
        );
        let mut condensed = vec!["FranklinCond".to_string()];
        condensed.extend(fallbacks);
        fonts
            .families
            .insert(egui::FontFamily::Name(Arc::from("Condensed")), condensed);
        CONDENSED.store(true, Ordering::Relaxed);
    }
    ctx.set_fonts(fonts);
}

fn read_face(dir: &Path, names: &[&str]) -> Option<Vec<u8>> {
    names
        .iter()
        .find_map(|name| std::fs::read(dir.join(name)).ok())
}

/// Family for the condensed labels. The proportional face when that font is not installed.
pub fn condensed() -> egui::FontFamily {
    if CONDENSED.load(Ordering::Relaxed) {
        egui::FontFamily::Name(Arc::from("Condensed"))
    } else {
        egui::FontFamily::Proportional
    }
}
