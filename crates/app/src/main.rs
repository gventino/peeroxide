mod decoder;
mod encoder;
mod stats;
mod ui;
mod video;

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(version, about = "Peer-to-peer LAN screen sharing")]
pub struct Args {
    /// Start broadcasting right away: "test" (test pattern), "monitor" (first monitor),
    /// or part of a window title.
    #[arg(long, value_name = "SOURCE")]
    pub broadcast: Option<String>,
}

fn main() -> eframe::Result {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,p2pss=debug".into()),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("P2P Screen Share")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "P2P Screen Share",
        options,
        Box::new(|_cc| Ok(Box::new(ui::App::new(args)))),
    )
}
