fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("P2P Screen Share")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_ui_native("P2P Screen Share", options, |ui, _frame| {
        ui.heading("P2P Screen Share");
    })
}
