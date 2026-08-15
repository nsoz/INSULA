//! The persistent background watermark: the Insula wordmark-in-a-ring,
//! rendered from the actual source image (`assets/insula_mark.png`) via a
//! real terminal graphics protocol (Kitty/iTerm2/Sixel) — pixel-perfect on
//! a terminal that supports one (e.g. iTerm2), falling back to a colored
//! half-block approximation of the same image on terminals that don't
//! (e.g. classic Terminal.app, where the fallback is the visible ceiling —
//! no terminal without a graphics protocol can render a real photo any
//! more crisply than that; it's a limitation of the terminal itself, not
//! of this code). Always low-contrast, always centered, always scaled to
//! fill the current terminal size (grown or shrunk, never left small) —
//! whatever real content the CLI draws on top of it later simply
//! overwrites the cells it occupies, the same way Kali's terminal
//! wallpaper never competes with the shell prompt sitting on top of it.
//!
//! Resizing/encoding the image happens on a background thread
//! (`ratatui_image::thread::ThreadProtocol`) rather than inline in the
//! render call — the crate's own docs warn that a plain [`StatefulProtocol`]
//! blocks the UI thread while it re-encodes, which is what caused the
//! laggy resize behavior in the first version of this file.

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use image::{DynamicImage, Rgba};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Color, Style};
use ratatui_image::errors::Errors;
use ratatui_image::picker::Picker;
use ratatui_image::thread::{ResizeRequest, ResizeResponse, ThreadProtocol};
use ratatui_image::{FilterType, Resize, ResizeEncodeRender};

const BG_RGB: (u8, u8, u8) = (10, 10, 12);
pub const BG_COLOR: Color = Color::Rgb(BG_RGB.0, BG_RGB.1, BG_RGB.2);

const MARK_BYTES: &[u8] = include_bytes!("../../assets/insula_mark.png");

/// Grows or shrinks the mark to fill the terminal. `Resize::Fit` (the more
/// obvious-looking choice) refuses to upscale past the source image's
/// native pixel size, which is exactly why the mark used to stay small in
/// a large terminal.
fn resize_mode() -> Resize {
    Resize::Scale(Some(FilterType::Lanczos3))
}

/// A cheaper resample filter buys next to nothing here: for the iTerm2
/// protocol (`ratatui_image::protocol::iterm2::encode`), every resize
/// re-encodes the *entire* resampled image as PNG and then base64, and that
/// full re-encode — not the resampling step a `FilterType` controls — is
/// what actually costs the time. Re-running it on every one of the dozens
/// of resize events a live terminal drag produces is what reads as lag, no
/// matter how cheap the resample itself is made. So: don't. Only encode
/// once the size has held still for `SETTLE_DELAY`; while a drag is still
/// in progress, keep showing the last encoded frame recentered (free — no
/// re-encode) instead of chasing a target that keeps moving.
const SETTLE_DELAY: Duration = Duration::from_millis(150);

pub struct Background {
    protocol: ThreadProtocol,
    rx_result: Receiver<Result<ResizeResponse, Errors>>,
    last_known_size: Size,
    /// A snapshot of the last cells actually drawn for the mark. `ratatui_image`'s
    /// `ThreadProtocol` hands its encoded image off to the background thread the
    /// instant a resize is needed and renders nothing until the response comes
    /// back; painting this in the meantime is what keeps the mark from vanishing
    /// into a blank/black gap during a live terminal resize.
    last_frame: Option<Buffer>,
    /// The target rect as of the last `render` call, to detect when the
    /// terminal size actually changed (as opposed to just redrawing).
    last_target: Option<Rect>,
    /// When the pending re-encode should fire, once the size has held
    /// steady this long. `None` when the current size is already encoded
    /// (or the encode for it has already been dispatched).
    settle_at: Option<Instant>,
    /// Set if the background thread's last resize/encode attempt failed.
    /// Encoding errors used to be dropped silently, which looked
    /// identical to "still working on it" — an all-black area forever,
    /// with no indication anything had gone wrong.
    pub last_error: Option<String>,
}

impl Background {
    /// Decodes the embedded mark image, spawns the background resize/encode
    /// worker, and prepares it for rendering through whichever protocol
    /// `picker` detected for this terminal.
    pub fn load(picker: &mut Picker) -> anyhow::Result<Self> {
        let image: DynamicImage = image::load_from_memory(MARK_BYTES)?;

        // Without this, any pixels the resizer pads on to round the image up to
        // a whole number of terminal cells default to transparent, which most
        // graphics protocols paint as solid black instead of blending with the
        // surrounding watermark background — visible as a seam along whichever
        // edge doesn't divide evenly into cells. Matching it to `BG_COLOR` makes
        // that padding (and any transparent border already in the source image)
        // disappear into the background instead.
        picker.set_background_color(Some(Rgba([BG_RGB.0, BG_RGB.1, BG_RGB.2, 255])));

        let protocol = picker.new_resize_protocol(image);

        let (tx_worker, rx_worker) = mpsc::channel::<ResizeRequest>();
        let (tx_result, rx_result) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(request) = rx_worker.recv() {
                if tx_result.send(request.resize_encode()).is_err() {
                    return;
                }
            }
        });

        Ok(Self {
            protocol: ThreadProtocol::new(tx_worker, Some(protocol)),
            rx_result,
            last_known_size: Size::default(),
            last_frame: None,
            last_target: None,
            settle_at: None,
            last_error: None,
        })
    }

    /// Picks up any resize/encode result that finished on the background
    /// thread since the last call. Cheap and non-blocking — call once per
    /// frame before `render`.
    pub fn poll_resize_result(&mut self) {
        while let Ok(result) = self.rx_result.try_recv() {
            match result {
                Ok(response) => {
                    self.last_error = None;
                    self.protocol.update_resized_protocol(response);
                }
                Err(err) => self.last_error = Some(err.to_string()),
            }
        }
    }

    /// Renders the mark centered in `area`, scaled (grown or shrunk) to
    /// fill it while keeping the source aspect ratio.
    ///
    /// A real graphics protocol has no notion of "the same image, just
    /// bigger" — every size change means resending a fully re-rendered
    /// raster image and having the terminal redecode it, there's no way
    /// around that for any terminal program. So while the target size is
    /// actively changing (a live terminal drag), this doesn't attempt a
    /// re-encode at all — see `SETTLE_DELAY` for why one per resize event
    /// is actively counterproductive — and it doesn't try to reposition the
    /// last frame either (see `paint_cached`, that retransmits just as much
    /// as a real re-encode would). It leaves the last successfully rendered
    /// frame exactly where it is, which costs nothing, so the mark holds
    /// perfectly still with no flicker while the terminal resizes around
    /// it. Once the size has held still for `SETTLE_DELAY`, exactly one
    /// re-encode is dispatched to the background thread and swapped in,
    /// correctly centered, when it completes.
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        frame
            .buffer_mut()
            .set_style(area, Style::default().bg(BG_COLOR));

        if area.width == 0 || area.height == 0 {
            return;
        }

        let ready = self.protocol.size_for(resize_mode(), area.into());
        let size = ready.unwrap_or(self.last_known_size);
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.last_known_size = size;

        let target = Rect {
            x: area.x + (area.width.saturating_sub(size.width)) / 2,
            y: area.y + (area.height.saturating_sub(size.height)) / 2,
            width: size.width.min(area.width),
            height: size.height.min(area.height),
        };

        if self.last_target != Some(target) {
            self.last_target = Some(target);
            self.settle_at = Some(Instant::now() + SETTLE_DELAY);
        }
        let settled = self.settle_at.is_none_or(|at| Instant::now() >= at);

        // `ready` tells us whether the protocol currently holds an encoded
        // image at all (it's briefly `None` right after being handed off to
        // the worker thread). `needs_resize` then tells us whether that
        // encoded image already matches `target` — the case where a plain
        // render would draw nothing.
        let mismatched =
            ready.and_then(|_| self.protocol.needs_resize(&resize_mode(), target.into()));

        if let Some(rect) = mismatched {
            if settled {
                self.settle_at = None;
                self.protocol.resize_encode(&resize_mode(), rect);
            }
            self.paint_cached(frame.buffer_mut());
        } else if ready.is_some() {
            self.protocol.render(target, frame.buffer_mut());
            self.cache_frame(frame.buffer_mut(), target);
        } else {
            self.paint_cached(frame.buffer_mut());
        }
    }

    /// Snapshots exactly the cells that were just drawn for the mark so they
    /// can stand in for it while the next resize is being computed.
    fn cache_frame(&mut self, buf: &Buffer, area: Rect) {
        let mut snapshot = Buffer::empty(area);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let (Some(cell), Some(slot)) = (buf.cell((x, y)), snapshot.cell_mut((x, y))) {
                    *slot = cell.clone();
                }
            }
        }
        self.last_frame = Some(snapshot);
    }

    /// Paints the last cached frame back at the *exact* screen position it
    /// was captured at — deliberately not recentered to track the current
    /// (possibly still-changing) area.
    ///
    /// For a real graphics protocol (iTerm2/Kitty), a cell holding part of
    /// the image doesn't hold a character — it holds the *entire* encoded
    /// payload (base64 PNG, easily several hundred KB to a few MB) as that
    /// cell's symbol; every other cell it covers is just marked to be
    /// skipped. Ratatui only ever writes a cell to the terminal if it
    /// differs from what was there last frame. So: redraw at a shifted
    /// position (e.g. recentered for a new size) and every one of those
    /// cells reads as "changed", and the whole multi-megabyte payload gets
    /// retransmitted and redecoded by the terminal — on *every single
    /// frame* of a live drag. That retransmit-and-redecode latency is what
    /// was showing up as the mark visibly disappearing and popping back in
    /// while resizing. Redrawing at the unchanged position instead makes
    /// every one of those cells compare equal to last frame, so Ratatui
    /// emits nothing for them at all: the mark just sits there, untouched,
    /// for zero cost, until the real resize lands and replaces it in one
    /// shot at `cache_frame` time.
    fn paint_cached(&self, buf: &mut Buffer) {
        let Some(snapshot) = &self.last_frame else {
            return;
        };
        for y in snapshot.area.top()..snapshot.area.bottom() {
            for x in snapshot.area.left()..snapshot.area.right() {
                if let (Some(cell), Some(slot)) = (snapshot.cell((x, y)), buf.cell_mut((x, y))) {
                    *slot = cell.clone();
                }
            }
        }
    }
}
