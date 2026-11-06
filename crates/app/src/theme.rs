//! The station palettes: the original one and five based on well-known editor themes, each
//! with a dark and a light side. Also the Franklin Gothic faces when Windows has them
//! installed. The font files stay on the machine; nothing is bundled.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use eframe::egui::{self, Color32, Stroke};

use crate::settings::{Palette, ThemeChoice};

static CONDENSED: AtomicBool = AtomicBool::new(false);
/// The palette last applied, as its index in [`Palette::ALL`].
static PALETTE: AtomicU8 = AtomicU8::new(0);

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
    /// Behind the video, and the dark side's text fields. Dark on both sides.
    pub well: Color32,
    pub lamp: Color32,
    pub line: Color32,
    pub line_w: f32,
    pub strong_fill: Color32,
    pub strong_ink: Color32,
    /// Text on the well.
    pub stamp: Color32,
    pub title_bar: Color32,
    pub title_ink: Color32,
    /// Behind selected text, which is drawn in `ink`.
    pub selection: Color32,
}

impl Station {
    pub fn of(palette: Palette, dark: bool) -> Self {
        match (palette, dark) {
            (Palette::Station, true) => STATION_DARK,
            (Palette::Station, false) => STATION_LIGHT,
            (Palette::Gruvbox, true) => GRUVBOX_DARK,
            (Palette::Gruvbox, false) => GRUVBOX_LIGHT,
            (Palette::Kanagawa, true) => KANAGAWA_WAVE,
            (Palette::Kanagawa, false) => KANAGAWA_LOTUS,
            (Palette::Dracula, true) => DRACULA,
            (Palette::Dracula, false) => ALUCARD,
            (Palette::Catppuccin, true) => CATPPUCCIN_MOCHA,
            (Palette::Catppuccin, false) => CATPPUCCIN_LATTE,
            (Palette::Discord, true) => DISCORD_DARK,
            (Palette::Discord, false) => DISCORD_LIGHT,
        }
    }

    /// The stroke around a selected card: the strong key's color.
    pub fn selected_stroke(&self) -> Stroke {
        Stroke::new(3.0, self.strong_fill)
    }
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

const STATION_DARK: Station = Station {
    dark: true,
    housing: hex(0x0E1210),
    key: hex(0x1A201D),
    key_hover: hex(0x242B27),
    paper: hex(0xE4DCC8),
    ink: hex(0x1C1E1B),
    body: hex(0xE4DCC8),
    muted: hex(0xB7B1A4),
    well: hex(0x080A09),
    lamp: hex(0xE23B2F),
    line: hex(0x3A4440),
    line_w: 0.0,
    strong_fill: hex(0xE4DCC8),
    strong_ink: hex(0x1A1C19),
    stamp: hex(0xD7CFBE),
    title_bar: hex(0x1A222C),
    title_ink: hex(0xC9D0DC),
    selection: hex(0xD7CFBE),
};

const STATION_LIGHT: Station = Station {
    dark: false,
    housing: hex(0xE6E1D6),
    key: hex(0xF7F4EE),
    key_hover: hex(0xEFEBE3),
    paper: hex(0xFBF9F4),
    ink: hex(0x1C1E1B),
    body: hex(0x1C1E1B),
    muted: hex(0x4A463E),
    well: hex(0x080A09),
    lamp: hex(0xE23B2F),
    line: hex(0xC4BDB0),
    line_w: 1.0,
    strong_fill: hex(0x1A1C19),
    strong_ink: hex(0xF7F4EE),
    stamp: hex(0xD7CFBE),
    title_bar: hex(0xEDE9E0),
    title_ink: hex(0x1C1E1B),
    selection: hex(0xD7CFBE),
};

/// Gruvbox dark, medium contrast.
const GRUVBOX_DARK: Station = Station {
    dark: true,
    housing: hex(0x282828),
    key: hex(0x3C3836),
    key_hover: hex(0x504945),
    paper: hex(0x504945),
    ink: hex(0xEBDBB2),
    body: hex(0xEBDBB2),
    muted: hex(0xA89984),
    well: hex(0x1D2021),
    lamp: hex(0xFB4934),
    line: hex(0x665C54),
    line_w: 0.0,
    strong_fill: hex(0xFABD2F),
    strong_ink: hex(0x282828),
    stamp: hex(0xBDAE93),
    title_bar: hex(0x3C3836),
    title_ink: hex(0xEBDBB2),
    selection: hex(0x665C54),
};

/// Gruvbox light, medium contrast.
const GRUVBOX_LIGHT: Station = Station {
    dark: false,
    housing: hex(0xFBF1C7),
    key: hex(0xEBDBB2),
    key_hover: hex(0xD5C4A1),
    paper: hex(0xF9F5D7),
    ink: hex(0x3C3836),
    body: hex(0x3C3836),
    muted: hex(0x7C6F64),
    well: hex(0x1D2021),
    lamp: hex(0x9D0006),
    line: hex(0xBDAE93),
    line_w: 1.0,
    strong_fill: hex(0xAF3A03),
    strong_ink: hex(0xFBF1C7),
    stamp: hex(0xD5C4A1),
    title_bar: hex(0xEBDBB2),
    title_ink: hex(0x3C3836),
    selection: hex(0xD5C4A1),
};

/// Kanagawa Wave.
const KANAGAWA_WAVE: Station = Station {
    dark: true,
    housing: hex(0x1F1F28),
    key: hex(0x2A2A37),
    key_hover: hex(0x363646),
    paper: hex(0x223249),
    ink: hex(0xDCD7BA),
    body: hex(0xDCD7BA),
    muted: hex(0x938AA9),
    well: hex(0x16161D),
    lamp: hex(0xE46876),
    line: hex(0x54546D),
    line_w: 0.0,
    strong_fill: hex(0x7E9CD8),
    strong_ink: hex(0x16161D),
    stamp: hex(0xC8C093),
    title_bar: hex(0x2A2A37),
    title_ink: hex(0xDCD7BA),
    selection: hex(0x2D4F67),
};

/// Kanagawa Lotus.
const KANAGAWA_LOTUS: Station = Station {
    dark: false,
    housing: hex(0xF2ECBC),
    key: hex(0xE5DDB0),
    key_hover: hex(0xDCD5AC),
    paper: hex(0xE7DBA0),
    ink: hex(0x545464),
    body: hex(0x545464),
    muted: hex(0x716E61),
    well: hex(0x16161D),
    lamp: hex(0xC84053),
    line: hex(0xD5CEA3),
    line_w: 1.0,
    strong_fill: hex(0x4D699B),
    strong_ink: hex(0xF2ECBC),
    stamp: hex(0xC8C093),
    title_bar: hex(0xE5DDB0),
    title_ink: hex(0x545464),
    selection: hex(0xC9CBD1),
};

/// Dracula. The comment purple is lightened for the small grey text.
const DRACULA: Station = Station {
    dark: true,
    housing: hex(0x282A36),
    key: hex(0x343746),
    key_hover: hex(0x44475A),
    paper: hex(0x44475A),
    ink: hex(0xF8F8F2),
    body: hex(0xF8F8F2),
    muted: hex(0x9EA8C7),
    well: hex(0x191A21),
    lamp: hex(0xFF5555),
    line: hex(0x44475A),
    line_w: 0.0,
    strong_fill: hex(0xBD93F9),
    strong_ink: hex(0x282A36),
    stamp: hex(0xBFC4DA),
    title_bar: hex(0x343746),
    title_ink: hex(0xF8F8F2),
    selection: hex(0x44475A),
};

/// Alucard, Dracula's light side.
const ALUCARD: Station = Station {
    dark: false,
    housing: hex(0xFFFBEB),
    key: hex(0xF3EFDF),
    key_hover: hex(0xE9E4CF),
    paper: hex(0xFFFFFF),
    ink: hex(0x1F1F1F),
    body: hex(0x1F1F1F),
    muted: hex(0x6C664B),
    well: hex(0x191A21),
    lamp: hex(0xCB3A2A),
    line: hex(0xCFCFDE),
    line_w: 1.0,
    strong_fill: hex(0x644AC9),
    strong_ink: hex(0xFFFBEB),
    stamp: hex(0xBFC4DA),
    title_bar: hex(0xF3EFDF),
    title_ink: hex(0x1F1F1F),
    selection: hex(0xCFCFDE),
};

/// Catppuccin Mocha.
const CATPPUCCIN_MOCHA: Station = Station {
    dark: true,
    housing: hex(0x1E1E2E),
    key: hex(0x313244),
    key_hover: hex(0x45475A),
    paper: hex(0x313244),
    ink: hex(0xCDD6F4),
    body: hex(0xCDD6F4),
    muted: hex(0xA6ADC8),
    well: hex(0x11111B),
    lamp: hex(0xF38BA8),
    line: hex(0x45475A),
    line_w: 0.0,
    strong_fill: hex(0xCBA6F7),
    strong_ink: hex(0x11111B),
    stamp: hex(0xBAC2DE),
    title_bar: hex(0x181825),
    title_ink: hex(0xCDD6F4),
    selection: hex(0x585B70),
};

/// Catppuccin Latte.
const CATPPUCCIN_LATTE: Station = Station {
    dark: false,
    housing: hex(0xEFF1F5),
    key: hex(0xE6E9EF),
    key_hover: hex(0xDCE0E8),
    paper: hex(0xFFFFFF),
    ink: hex(0x4C4F69),
    body: hex(0x4C4F69),
    muted: hex(0x6C6F85),
    well: hex(0x11111B),
    lamp: hex(0xD20F39),
    line: hex(0xBCC0CC),
    line_w: 1.0,
    strong_fill: hex(0x8839EF),
    strong_ink: hex(0xEFF1F5),
    stamp: hex(0xBAC2DE),
    title_bar: hex(0xE6E9EF),
    title_ink: hex(0x4C4F69),
    selection: hex(0xCCD0DA),
};

/// Discord's default dark colors.
const DISCORD_DARK: Station = Station {
    dark: true,
    housing: hex(0x313338),
    key: hex(0x383A40),
    key_hover: hex(0x404249),
    paper: hex(0x404249),
    ink: hex(0xDBDEE1),
    body: hex(0xDBDEE1),
    muted: hex(0x949BA4),
    well: hex(0x1E1F22),
    lamp: hex(0xF23F43),
    line: hex(0x3F4147),
    line_w: 0.0,
    strong_fill: hex(0x5865F2),
    strong_ink: hex(0xFFFFFF),
    stamp: hex(0xB5BAC1),
    title_bar: hex(0x2B2D31),
    title_ink: hex(0xF2F3F5),
    selection: hex(0x4752C4),
};

/// Discord's default light colors.
const DISCORD_LIGHT: Station = Station {
    dark: false,
    housing: hex(0xF2F3F5),
    key: hex(0xE3E5E8),
    key_hover: hex(0xD7D9DC),
    paper: hex(0xFFFFFF),
    ink: hex(0x313338),
    body: hex(0x313338),
    muted: hex(0x5C5E66),
    well: hex(0x1E1F22),
    lamp: hex(0xDA373C),
    line: hex(0xD4D7DC),
    line_w: 1.0,
    strong_fill: hex(0x5865F2),
    strong_ink: hex(0xFFFFFF),
    stamp: hex(0xB5BAC1),
    title_bar: hex(0xE3E5E8),
    title_ink: hex(0x313338),
    selection: hex(0xC9CDFB),
};

/// Points both of egui's themes at the palette's two stations and records the saved preference.
pub fn apply(ctx: &egui::Context, palette: Palette, choice: ThemeChoice) {
    PALETTE.store(palette as u8, Ordering::Relaxed);
    ctx.options_mut(|o| {
        o.theme_preference = match choice {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
            ThemeChoice::Light => egui::ThemePreference::Light,
        };
        o.dark_style = Arc::new(style(Station::of(palette, true)));
        o.light_style = Arc::new(style(Station::of(palette, false)));
    });
}

/// The dark or light station of the palette last applied.
pub fn station(dark: bool) -> Station {
    let palette = Palette::ALL
        .get(usize::from(PALETTE.load(Ordering::Relaxed)))
        .copied()
        .unwrap_or_default();
    Station::of(palette, dark)
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
    // The preview well stays dark on both sides. Typed text uses the body color, so a light
    // station's field has to stay paper or the letters disappear on it.
    v.extreme_bg_color = station.well;
    let field = if station.dark {
        station.well
    } else {
        station.paper
    };
    v.text_edit_bg_color = Some(field);
    v.faint_bg_color = station.key;
    v.widgets.noninteractive.bg_fill = station.housing;
    v.widgets.noninteractive.fg_stroke = body;
    v.widgets.noninteractive.bg_stroke = line;
    // Check boxes, radio buttons and slider rails; the key would vanish on a key-colored window.
    v.widgets.inactive.bg_fill = field;
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
    // Selected text is drawn in the selection stroke, so the fill must carry ink.
    v.selection.bg_fill = station.selection;
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
///
/// Takes effect at the start of the next pass. [`condensed`] is already true once the face
/// is found, so the caller has to install before the pass that draws with it.
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
