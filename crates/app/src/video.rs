use eframe::egui::{
    self, Align2, Color32, FontId, Rect, Sense, TextureHandle, TextureOptions, pos2, vec2,
};

use crate::decoder::VideoSlot;

/// Shows the latest decoded frame, aspect-fit and centered, with an optional stats overlay.
#[derive(Default)]
pub struct VideoView {
    texture: Option<TextureHandle>,
}

impl VideoView {
    pub fn clear(&mut self) {
        self.texture = None;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, slot: &VideoSlot, overlay: &str) {
        // The decoder thread already built the image: only the upload happens here.
        if let Some(image) = slot.take() {
            match &mut self.texture {
                Some(t) => t.set(image, TextureOptions::LINEAR),
                None => {
                    self.texture = Some(ui.ctx().load_texture(
                        "video",
                        image,
                        TextureOptions::LINEAR,
                    ));
                }
            }
        }

        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0.0, Color32::BLACK);

        if let Some(t) = &self.texture {
            let size = t.size_vec2();
            let scale = (area.width() / size.x).min(area.height() / size.y);
            let rect = Rect::from_center_size(area.center(), size * scale);
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            painter.image(t.id(), rect, uv, Color32::WHITE);
        }

        if !overlay.is_empty() {
            let galley =
                painter.layout_no_wrap(overlay.to_owned(), FontId::monospace(12.0), Color32::WHITE);
            let pos = area.left_top() + vec2(8.0, 8.0);
            let bg = Rect::from_min_size(pos, galley.size()).expand(6.0);
            painter.rect_filled(bg, 4.0, Color32::from_black_alpha(170));
            painter.galley(pos, galley, Color32::WHITE);
        }
    }

    pub fn placeholder(ui: &mut egui::Ui, text: &str) {
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0.0, Color32::from_gray(18));
        painter.text(
            area.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(16.0),
            Color32::from_gray(150),
        );
    }
}
