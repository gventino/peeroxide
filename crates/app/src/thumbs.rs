//! One still frame per source, the same capture the picker used to start on open.
//!
//! It starts when the picker opens, off the UI thread, and not before: a capture can ask for
//! permission or flash a border. At most three at a time. A window that has since closed is no
//! longer in the source list; one that opened later is fetched the same way.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use peeroxide_capture::Source;

enum Thumb {
    Pending,
    Ready(ColorImage),
    Missing,
}

pub struct Thumbnails {
    ctx: egui::Context,
    cache: HashMap<String, Thumb>,
    textures: HashMap<String, TextureHandle>,
    queue: Vec<Source>,
    inflight: usize,
    tx: Sender<Message>,
    rx: Receiver<Message>,
}

struct Message {
    id: String,
    image: Result<ColorImage, String>,
}

impl Thumbnails {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        Self {
            ctx,
            cache: HashMap::new(),
            textures: HashMap::new(),
            queue: Vec::new(),
            inflight: 0,
            tx,
            rx,
        }
    }

    /// Keeps stills already taken and fetches only sources that do not have one yet.
    /// A failed capture is tried again. Sources no longer listed are dropped.
    pub fn ensure(&mut self, sources: &[Source]) {
        let wanted: HashSet<String> = sources.iter().map(|source| source.id()).collect();
        self.cache.retain(|id, _| wanted.contains(id));
        self.textures.retain(|id, _| wanted.contains(id));
        self.queue.retain(|source| wanted.contains(&source.id()));
        let queued: HashSet<String> = self.queue.iter().map(|source| source.id()).collect();
        for source in sources {
            let id = source.id();
            if queued.contains(&id)
                || matches!(self.cache.get(&id), Some(Thumb::Ready(_) | Thumb::Pending))
            {
                continue;
            }
            self.cache.remove(&id);
            self.queue.push(source.clone());
        }
        self.pump();
    }

    pub fn poll(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            self.inflight = self.inflight.saturating_sub(1);
            // A source dropped by `ensure` while its still was on the way.
            if !matches!(self.cache.get(&message.id), Some(Thumb::Pending)) {
                continue;
            }
            let thumb = match message.image {
                Ok(image) => Thumb::Ready(image),
                Err(error) => {
                    tracing::debug!(%error, id = %message.id, "thumbnail unavailable");
                    Thumb::Missing
                }
            };
            self.cache.insert(message.id, thumb);
        }
        self.pump();
    }

    /// The still for `id`, once it has arrived. `None` while it is loading or it failed.
    pub fn texture(&mut self, ctx: &egui::Context, id: &str) -> Option<TextureHandle> {
        if let Some(texture) = self.textures.get(id) {
            return Some(texture.clone());
        }
        let Thumb::Ready(image) = self.cache.get(id)? else {
            return None;
        };
        let texture =
            ctx.load_texture(format!("thumb-{id}"), image.clone(), TextureOptions::LINEAR);
        self.textures.insert(id.to_owned(), texture.clone());
        Some(texture)
    }

    fn pump(&mut self) {
        while self.inflight < 3 {
            let Some(source) = self.queue.pop() else {
                return;
            };
            let id = source.id();
            if self.cache.contains_key(&id) {
                continue;
            }
            self.cache.insert(id.clone(), Thumb::Pending);
            self.inflight += 1;
            let tx = self.tx.clone();
            let ctx = self.ctx.clone();
            let message_id = id.clone();
            if std::thread::Builder::new()
                .name("thumb".into())
                .spawn(move || {
                    let image = peeroxide_capture::snapshot(&source)
                        .map(to_image)
                        .map_err(|e| e.to_string());
                    let _ = tx.send(Message {
                        id: message_id,
                        image,
                    });
                    // The channel is only read on a frame.
                    ctx.request_repaint();
                })
                .is_err()
            {
                self.inflight = self.inflight.saturating_sub(1);
                self.cache.insert(id, Thumb::Missing);
            }
        }
    }
}

/// Opaque, as viewers see it: the encoder ignores the capture's alpha, and a window's
/// transparent corners would otherwise show the card through the picture.
fn to_image(frame: peeroxide_capture::Frame) -> ColorImage {
    let mut rgba = frame.data;
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &rgba)
}
