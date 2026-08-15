//! A hand-rolled fast path for the Kitty graphics protocol, used instead of
//! `background::Background` when the terminal is Kitty-protocol-native
//! (Kitty, Ghostty, WezTerm — not iTerm2, whose own inline-image protocol
//! has no equivalent of what this module relies on).
//!
//! `ratatui_image`'s own `StatefulKitty` retransmits the *entire* image
//! (raw RGBA, chunked, base64) on every resize — its `resize_encode` is
//! literally commented "If resized then we must transmit again". That's
//! the same expensive-retransmit-on-every-resize problem `background::
//! Background` has for every protocol it supports, and it's why no amount
//! of debouncing or frame-caching in this app's own code could make
//! resizing feel instant — the bottleneck was never our code, it's that a
//! full image round-trips through the pty on every size change.
//!
//! The Kitty protocol itself doesn't require that. An image can be
//! transmitted *once*, given an id, and then displayed at any cell size
//! via a "virtual placement" (`U=1`) plus "Unicode placeholders": a run of
//! `U+10EEEE` characters — one per cell — whose diacritics select which
//! row/column of the placement's *declared* grid that cell shows, with the
//! image id itself carried in the text's foreground color. See:
//! <https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders>
//!
//! That grid size (`c=` columns, `r=` rows) isn't inferred from how many
//! placeholder characters happen to be on screen — it has to be declared
//! explicitly. But re-sending a placement command with the *same* image id
//! and placement id updates that grid size in place ("If you send two
//! placements with the same image id and placement id the second one will
//! replace the first" — same doc) without re-sending any pixel data, so
//! resizing is still just a tiny control-only escape sequence, not a
//! resample-and-retransmit. That's what makes it fast enough to track a
//! live terminal drag: this module transmits the mark's pixels exactly
//! once, at load time, and every `render` call after that only ever sends
//! a cheap placement-size update plus the placeholder characters for
//! whatever the current terminal area calls for.
//!
//! The escape-sequence format and diacritic table below are ported from
//! `ratatui_image::protocol::kitty` (MIT-licensed) — that crate already
//! implements this exact scheme correctly for its non-resizing `Protocol`
//! path, just not for the resizing `StatefulProtocol` path this app needs.

use std::fmt::Write as _;
use std::num::NonZeroU16;

use image::DynamicImage;
use ratatui::Frame;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui_image::{FontSize, Resize};

use crate::cli::background::BG_COLOR;

const ESC: &str = "\x1b";

/// Arbitrary but fixed — this app only ever displays the one mark image,
/// so there's no need for either id to be dynamic.
const IMAGE_ID: u32 = 1;
const PLACEMENT_ID: u32 = 1;

const MARK_BYTES: &[u8] = include_bytes!("../../assets/insula_mark.png");

pub struct KittyMark {
    image: DynamicImage,
    font_size: FontSize,
    id_color: String,
    id_extra: u16,
    /// The transmit-and-virtual-placement escape sequence, prepended to the
    /// very first placement written. `take()`n once it's been written so
    /// every subsequent `render` only ever emits the cheap placement text.
    transmit_seq: Option<String>,
}

impl KittyMark {
    /// Decodes the embedded mark image and prepares (but does not yet
    /// send) the one-time transmit sequence for it.
    pub fn load(font_size: FontSize) -> anyhow::Result<Self> {
        let image = image::load_from_memory(MARK_BYTES)?;

        let [id_extra, id_r, id_g, id_b] = IMAGE_ID.to_be_bytes();
        let id_color = format!("{ESC}[38;2;{id_r};{id_g};{id_b}m");

        Ok(Self {
            transmit_seq: Some(transmit_virtual(&image, IMAGE_ID, PLACEMENT_ID)),
            image,
            font_size,
            id_color,
            id_extra: u16::from(id_extra),
        })
    }

    /// Renders the mark centered in `area`, scaled (grown or shrunk) to
    /// fill it while keeping the source aspect ratio — every single call,
    /// including mid-drag, since unlike `background::Background` there's
    /// no expensive work being deferred here: the pixel data was already
    /// sent once, in `load`, and every call after that is just placement.
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        frame
            .buffer_mut()
            .set_style(area, ratatui::style::Style::default().bg(BG_COLOR));

        if area.width == 0 || area.height == 0 {
            return;
        }

        let size = Resize::Scale(None).size_for(&self.image, self.font_size, area.into());
        if size.width == 0 || size.height == 0 {
            return;
        }

        let target = Rect {
            x: area.x + (area.width.saturating_sub(size.width)) / 2,
            y: area.y + (area.height.saturating_sub(size.height)) / 2,
            width: size.width.min(area.width),
            height: size.height.min(area.height),
        };

        place(
            target,
            frame.buffer_mut(),
            &self.id_color,
            self.id_extra,
            self.transmit_seq.take(),
        );
    }
}

/// Writes the Unicode-placeholder placement for `target`, prepending
/// `transmit_seq` (once, if still `Some`) and a placement-size update
/// (every call — see the module docs for why re-sending this with the same
/// image/placement id is cheap) to the very first row.
fn place(
    target: Rect,
    buf: &mut Buffer,
    id_color: &str,
    id_extra: u16,
    mut transmit_seq: Option<String>,
) {
    if target.width == 0 || target.height == 0 {
        return;
    }

    let row_diacritics: String =
        std::iter::repeat_n('\u{10EEEE}', usize::from(target.width) - 1).collect();

    let right = target.width - 1;
    let down = target.height - 1;
    let restore_cursor = format!("{ESC}[u{ESC}[{right}C{ESC}[{down}B");
    let resize_seq = format!(
        "{ESC}_Ga=p,i={IMAGE_ID},p={PLACEMENT_ID},U=1,c={},r={},q=2{ESC}\\",
        target.width, target.height
    );

    // Clamp to the number of distinct row-diacritics Kitty's spec defines;
    // taller placements than that aren't representable and are truncated,
    // same limitation `ratatui_image`'s own implementation has.
    let height = target.height.min(DIACRITICS.len() as u16);

    let mut symbol = String::new();
    for y in 0..height {
        symbol.clear();

        // Only the very first row of the very first `render` call after
        // `load` carries the (large) transmit payload.
        if let Some(seq) = transmit_seq.take() {
            symbol.push_str(&seq);
        }
        // Every row-0 write also (cheaply) redeclares the placement's
        // c=/r= grid to match `target` — this is what actually makes the
        // mark change size on screen; without it Kitty keeps showing
        // whichever grid size the placement was last declared at,
        // regardless of how many placeholder cells are printed.
        if y == 0 {
            symbol.push_str(&resize_seq);
        }

        write!(
            symbol,
            "{ESC}[s{id_color}\u{10EEEE}{}{}{}",
            diacritic(y),
            diacritic(0),
            diacritic(id_extra)
        )
        .unwrap();
        symbol.push_str(&row_diacritics);

        // The whole row is packed into one cell's symbol (below); every
        // other cell in the row must be marked to skip diffing, or Ratatui
        // would try to separately draw/clear cells that the terminal is
        // actually filling as a side effect of the placeholder run above.
        for x in 1..target.width {
            if let Some(cell) = buf.cell_mut((target.x + x, target.y + y)) {
                cell.set_diff_option(CellDiffOption::Skip);
            }
        }

        symbol.push_str(&restore_cursor);

        if let Some(cell) = buf.cell_mut((target.x, target.y + y)) {
            cell.set_symbol(&symbol)
                .set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::new(1).unwrap()));
        }
    }
}

/// Builds the one-time Kitty transmit-and-virtual-placement escape
/// sequence for `image`, chunked to the protocol's 4096-base64-char
/// payload limit per escape sequence. Tags the resulting placement with
/// `placement_id` so the placement-size updates in `place` (same image id
/// + placement id) are recognized as updates to *this* placement.
fn transmit_virtual(image: &DynamicImage, id: u32, placement_id: u32) -> String {
    let (width, height) = (image.width(), image.height());
    let rgba = image.to_rgba8();
    let bytes = rgba.as_raw();

    const CHARS_PER_CHUNK: usize = 4096;
    const CHUNK_SIZE: usize = (CHARS_PER_CHUNK / 4) * 3;
    let chunks = bytes.chunks(CHUNK_SIZE);
    let chunk_count = chunks.len();

    let mut data = String::new();
    for (i, chunk) in chunks.enumerate() {
        write!(data, "{ESC}_Gq=2,").unwrap();
        if i == 0 {
            write!(
                data,
                "i={id},p={placement_id},a=T,U=1,f=32,t=d,s={width},v={height},"
            )
            .unwrap();
        }
        let more = u8::from(chunk_count > i + 1);
        write!(data, "m={more};").unwrap();
        base64_simd::STANDARD.encode_append(chunk, &mut data);
        write!(data, "{ESC}\\").unwrap();
    }
    data
}

#[inline]
fn diacritic(y: u16) -> char {
    *DIACRITICS.get(usize::from(y)).unwrap_or(&DIACRITICS[0])
}

/// From <https://sw.kovidgoyal.net/kitty/_downloads/1792bad15b12979994cd6ecc54c967a6/rowcolumn-diacritics.txt>
/// See <https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders> for further explanation.
static DIACRITICS: [char; 297] = [
    '\u{305}',
    '\u{30D}',
    '\u{30E}',
    '\u{310}',
    '\u{312}',
    '\u{33D}',
    '\u{33E}',
    '\u{33F}',
    '\u{346}',
    '\u{34A}',
    '\u{34B}',
    '\u{34C}',
    '\u{350}',
    '\u{351}',
    '\u{352}',
    '\u{357}',
    '\u{35B}',
    '\u{363}',
    '\u{364}',
    '\u{365}',
    '\u{366}',
    '\u{367}',
    '\u{368}',
    '\u{369}',
    '\u{36A}',
    '\u{36B}',
    '\u{36C}',
    '\u{36D}',
    '\u{36E}',
    '\u{36F}',
    '\u{483}',
    '\u{484}',
    '\u{485}',
    '\u{486}',
    '\u{487}',
    '\u{592}',
    '\u{593}',
    '\u{594}',
    '\u{595}',
    '\u{597}',
    '\u{598}',
    '\u{599}',
    '\u{59C}',
    '\u{59D}',
    '\u{59E}',
    '\u{59F}',
    '\u{5A0}',
    '\u{5A1}',
    '\u{5A8}',
    '\u{5A9}',
    '\u{5AB}',
    '\u{5AC}',
    '\u{5AF}',
    '\u{5C4}',
    '\u{610}',
    '\u{611}',
    '\u{612}',
    '\u{613}',
    '\u{614}',
    '\u{615}',
    '\u{616}',
    '\u{617}',
    '\u{657}',
    '\u{658}',
    '\u{659}',
    '\u{65A}',
    '\u{65B}',
    '\u{65D}',
    '\u{65E}',
    '\u{6D6}',
    '\u{6D7}',
    '\u{6D8}',
    '\u{6D9}',
    '\u{6DA}',
    '\u{6DB}',
    '\u{6DC}',
    '\u{6DF}',
    '\u{6E0}',
    '\u{6E1}',
    '\u{6E2}',
    '\u{6E4}',
    '\u{6E7}',
    '\u{6E8}',
    '\u{6EB}',
    '\u{6EC}',
    '\u{730}',
    '\u{732}',
    '\u{733}',
    '\u{735}',
    '\u{736}',
    '\u{73A}',
    '\u{73D}',
    '\u{73F}',
    '\u{740}',
    '\u{741}',
    '\u{743}',
    '\u{745}',
    '\u{747}',
    '\u{749}',
    '\u{74A}',
    '\u{7EB}',
    '\u{7EC}',
    '\u{7ED}',
    '\u{7EE}',
    '\u{7EF}',
    '\u{7F0}',
    '\u{7F1}',
    '\u{7F3}',
    '\u{816}',
    '\u{817}',
    '\u{818}',
    '\u{819}',
    '\u{81B}',
    '\u{81C}',
    '\u{81D}',
    '\u{81E}',
    '\u{81F}',
    '\u{820}',
    '\u{821}',
    '\u{822}',
    '\u{823}',
    '\u{825}',
    '\u{826}',
    '\u{827}',
    '\u{829}',
    '\u{82A}',
    '\u{82B}',
    '\u{82C}',
    '\u{82D}',
    '\u{951}',
    '\u{953}',
    '\u{954}',
    '\u{F82}',
    '\u{F83}',
    '\u{F86}',
    '\u{F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];
