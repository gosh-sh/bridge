//! A QR code as a real image on terminals that can show one: kitty
//! graphics, iTerm2 inline images or sixel. Half blocks scan poorly off
//! small cells and dark themes; a bitmap scans at once.
//!
//! The terminal is asked first and its environment second. A capability
//! probe writes a kitty graphics query, `CSI 16 t` and DA1 to the
//! controlling terminal and reads the answers for a moment: a kitty `OK`
//! proves kitty graphics, DA1 attribute 4 proves sixel. Every terminal
//! answers DA1, so its answer also ends the wait. Variables such as
//! `KITTY_WINDOW_ID` or `TERM_PROGRAM` count only where the probe proves
//! nothing, and inside a multiplexer or an embedded terminal neither is
//! trusted (see [`TerminalEnv::mux`]). Anything short of proof gets the
//! text rendering of [`crate::deposit::qr`]; `--qr-display` forces either
//! way.

use std::{sync::OnceLock, time::Duration};

use base64::Engine as _;

/// How long the probe waits for the terminal's answers. A local terminal
/// answers within milliseconds; the rest pays for an ssh round trip.
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);
/// The most pixels per module, and what a small code keeps.
const MODULE_SCALE_MAX: usize = 8;
/// The fewest pixels per module. Below it a phone camera loses the
/// modules, so a window too small even for this gets an image that
/// overflows rather than one that cannot be scanned.
const MODULE_SCALE_MIN: usize = 2;
/// How much of the window's height the image may take; the rest holds the
/// decoded fields, the URI and the step board.
const WINDOW_HEIGHT_PERCENT: usize = 66;
/// `(width, height)` of the character cell assumed when neither the system
/// nor the terminal reports one: the common default-font size.
const ASSUMED_CELL: (usize, usize) = (8, 16);
/// The light border the QR standard requires, in modules.
const QUIET_ZONE: usize = 4;
/// Printed under an image.
pub const IMAGE_HINT: &str =
    "(QR shown as an image; if it does not scan, run with --qr-display text)";

/// What the terminal answered to the capability probe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeReply {
    /// The terminal answered the kitty graphics query with `OK`.
    pub kitty: bool,
    /// The DA1 attributes, once the terminal answered `CSI c`.
    pub da1: Option<Vec<u16>>,
    /// `(width, height)` of one character cell in pixels, from the answer
    /// to `CSI 16 t`. The system often does not know it: under WSL the
    /// window size comes without pixels.
    pub cell_px: Option<(usize, usize)>,
}

impl ProbeReply {
    /// DA1 attribute 4 advertises sixel.
    pub fn sixel(&self) -> bool {
        self.da1.as_ref().is_some_and(|attrs| attrs.contains(&4))
    }

    /// DA1 is asked last and every terminal answers it: once it is here,
    /// the probe has everything it will get.
    pub fn complete(&self) -> bool {
        self.da1.is_some()
    }
}

/// The answers found in `bytes`, at any position: keys pressed while the
/// probe listens land in the same input, before, between and after them.
pub fn parse_probe_reply(bytes: &[u8]) -> ProbeReply {
    ProbeReply {
        kitty: kitty_confirmed(bytes),
        da1: find_da1(bytes),
        cell_px: find_cell_size(bytes),
    }
}

/// The first position of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The numbers of `CSI <prefix> n;n;... <end>` after `bytes`, for the first
/// such sequence that ends in `end`. A sequence ending in another final
/// byte is a different answer and is skipped.
fn csi_numbers(bytes: &[u8], prefix: &[u8], end: u8) -> Option<Vec<usize>> {
    let mut rest = bytes;
    while let Some(start) = find(rest, prefix) {
        let body = &rest[start + prefix.len()..];
        match body
            .iter()
            .position(|&byte| !(byte.is_ascii_digit() || byte == b';'))
        {
            Some(stop) if body[stop] == end => {
                return Some(
                    body[..stop]
                        .split(|&byte| byte == b';')
                        .filter_map(|n| std::str::from_utf8(n).ok()?.parse().ok())
                        .collect(),
                );
            },
            Some(stop) => rest = &body[stop..],
            // The parameters run off the end: the answer is still arriving.
            None => return None,
        }
    }
    None
}

/// `CSI 6 ; height ; width t` as `(width, height)`. A cursor report,
/// `CSI 6 ; row ; column R`, ends in `R` and is no cell size; a zero either
/// way is unknown.
fn find_cell_size(bytes: &[u8]) -> Option<(usize, usize)> {
    let numbers = csi_numbers(bytes, b"\x1b[6;", b't')?;
    match numbers[..] {
        [height, width, ..] if height > 0 && width > 0 => Some((width, height)),
        _ => None,
    }
}

/// A kitty answer is `ESC _ G <keys> ; <answer> ESC \`, and only `OK` is
/// support: a terminal that refuses the query's format cannot show the
/// image either.
fn kitty_confirmed(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    while let Some(start) = find(rest, b"\x1b_G") {
        let body = &rest[start + 3..];
        // Unterminated: still arriving, and it counts for nothing yet.
        let Some(end) = find(body, b"\x1b\\") else {
            return false;
        };
        if body[..end]
            .split(|&byte| byte == b';')
            .nth(1)
            .is_some_and(|answer| answer.starts_with(b"OK"))
        {
            return true;
        }
        rest = &body[end + 2..];
    }
    false
}

/// DA1 is `ESC [ ? <attr> ; <attr> ... c`.
fn find_da1(bytes: &[u8]) -> Option<Vec<u16>> {
    csi_numbers(bytes, b"\x1b[?", b'c').map(|attrs| {
        attrs
            .into_iter()
            .filter_map(|a| u16::try_from(a).ok())
            .collect()
    })
}

/// How a QR code is put on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QrDisplay {
    /// The kitty graphics protocol.
    Kitty,
    /// The iTerm2 inline image protocol.
    Iterm2,
    /// Sixel graphics.
    Sixel,
    /// Unicode half blocks.
    Text,
}

/// What the environment says about the terminal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalEnv {
    /// `--qr-display` names one rendering: it wins over detection.
    pub force: Option<QrDisplay>,
    /// The QR goes to a terminal and stdin is one.
    pub tty: bool,
    /// Inside a multiplexer or an embedded terminal: tmux, screen, zellij,
    /// the terminal of nvim or emacs. Nothing heard through them tells what
    /// the screen shows: tmux advertises sixel in DA1 and takes the image,
    /// yet draws a grid of placeholders instead; and the outer terminal's
    /// variables leak into its panes. Detection gives text here.
    pub mux: bool,
    /// The terminal names itself as one that speaks kitty graphics: kitty,
    /// Ghostty, Konsole from 22.04.
    pub kitty_env: bool,
    /// The terminal names itself as one that speaks iTerm2 images: iTerm2,
    /// also over ssh, or WezTerm.
    pub iterm2_env: bool,
}

impl TerminalEnv {
    /// From an environment lookup; an empty variable counts as unset.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
        tty: bool,
        force: Option<QrDisplay>,
    ) -> TerminalEnv {
        let var = |key: &str| lookup(key).filter(|value| !value.is_empty());
        let term = var("TERM").unwrap_or_default();
        let term_program = var("TERM_PROGRAM").unwrap_or_default();
        TerminalEnv {
            force,
            tty,
            mux: var("TMUX").is_some()
                || var("ZELLIJ").is_some()
                || var("NVIM").is_some()
                || var("INSIDE_EMACS").is_some()
                || term.starts_with("screen")
                || term.starts_with("tmux"),
            kitty_env: var("KITTY_WINDOW_ID").is_some()
                || term == "xterm-kitty"
                || term == "xterm-ghostty"
                || var("GHOSTTY_RESOURCES_DIR").is_some()
                || var("KONSOLE_VERSION")
                    .and_then(|version| version.parse::<u32>().ok())
                    .is_some_and(|version| version >= 220400),
            iterm2_env: term_program == "iTerm.app"
                || term_program == "WezTerm"
                || var("LC_TERMINAL").as_deref() == Some("iTerm2"),
        }
    }
}

/// The rendering for this terminal: the forced one, else what the probe
/// proved, else what the terminal's name promises, else text.
pub fn choose_display(env: &TerminalEnv, probe: &ProbeReply) -> QrDisplay {
    if let Some(forced) = env.force {
        return forced;
    }
    if !env.tty || env.mux {
        return QrDisplay::Text;
    }
    if probe.kitty {
        return QrDisplay::Kitty;
    }
    if probe.sixel() {
        return QrDisplay::Sixel;
    }
    // Every kitty-graphics terminal answers the query: an answered probe
    // without `OK` outranks the name. iTerm2 has no query to decline.
    if env.kitty_env && !probe.complete() {
        return QrDisplay::Kitty;
    }
    if env.iterm2_env {
        return QrDisplay::Iterm2;
    }
    QrDisplay::Text
}

/// Whether the probe's answer can change [`choose_display`]'s: not when a
/// rendering is forced, without a terminal, or behind a multiplexer. Then
/// the probe is not run.
pub fn probe_can_matter(env: &TerminalEnv) -> bool {
    env.force.is_none() && env.tty && !env.mux
}

/// The window, as far as it is known.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalGeometry {
    /// Rows of text.
    pub rows: usize,
    /// Columns of text.
    pub cols: usize,
    /// `(width, height)` of one character cell in pixels.
    pub cell: Option<(usize, usize)>,
}

/// The most whole pixels per module that keep a code `modules` wide, quiet
/// zone included, inside its share of the window. Whole pixels only: a
/// scaled image blurs the module edges, and a phone gives up on a blurred
/// code sooner than on a small one.
pub fn fit_module_scale(modules: usize, geometry: &TerminalGeometry) -> usize {
    if modules == 0 || geometry.rows == 0 || geometry.cols == 0 {
        return MODULE_SCALE_MAX;
    }
    let (cell_width, cell_height) = geometry.cell.unwrap_or(ASSUMED_CELL);
    let height = geometry.rows * WINDOW_HEIGHT_PERCENT / 100 * cell_height;
    let width = geometry.cols * cell_width;
    (height.min(width) / modules).clamp(MODULE_SCALE_MIN, MODULE_SCALE_MAX)
}

/// A QR code as pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrBitmap {
    /// Pixels per row.
    width: usize,
    /// Rows.
    height: usize,
    /// Row by row; `true` is dark.
    dark: Vec<bool>,
}

impl QrBitmap {
    /// `modules`, `modules_wide` per row, `scale` pixels each, inside a
    /// light border `quiet_zone` modules wide.
    pub fn from_modules(
        modules: &[bool],
        modules_wide: usize,
        scale: usize,
        quiet_zone: usize,
    ) -> QrBitmap {
        if modules_wide == 0 || scale == 0 {
            return QrBitmap {
                width: 0,
                height: 0,
                dark: vec![],
            };
        }
        let modules_high = modules.len() / modules_wide;
        let width = (modules_wide + 2 * quiet_zone) * scale;
        let height = (modules_high + 2 * quiet_zone) * scale;
        let mut dark = vec![false; width * height];
        for module_y in 0..modules_high {
            for module_x in 0..modules_wide {
                if !modules[module_y * modules_wide + module_x] {
                    continue;
                }
                for pixel_y in 0..scale {
                    let y = (module_y + quiet_zone) * scale + pixel_y;
                    let row = y * width + (module_x + quiet_zone) * scale;
                    dark[row..row + scale].fill(true);
                }
            }
        }
        QrBitmap {
            width,
            height,
            dark,
        }
    }

    /// The pixel at `(x, y)` is dark.
    fn is_dark(&self, x: usize, y: usize) -> bool {
        self.dark[y * self.width + x]
    }
}

/// One byte per pixel, row by row: 0 dark, 255 light.
fn grayscale_of(bitmap: &QrBitmap) -> Vec<u8> {
    bitmap
        .dark
        .iter()
        .map(|&dark| if dark { 0 } else { 255 })
        .collect()
}

/// The kitty graphics escape: RGB pixels in base64, in the protocol's
/// chunks of 4096 characters.
pub fn encode_kitty(bitmap: &QrBitmap) -> String {
    let rgb: Vec<u8> = grayscale_of(bitmap)
        .into_iter()
        .flat_map(|value| [value, value, value])
        .collect();
    // 3072 bytes encode to exactly 4096 characters without padding, so the
    // chunks put together are the base64 of the whole image.
    let mut chunks: Vec<String> = rgb
        .chunks(3072)
        .map(|chunk| base64::engine::general_purpose::STANDARD.encode(chunk))
        .collect();
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    let last = chunks.len() - 1;
    let mut out = String::new();
    for (index, chunk) in chunks.iter().enumerate() {
        if index == 0 {
            // `q=2`: the terminal does not answer the image, so no answer
            // lands in the input a later question reads.
            out.push_str(&format!(
                "\x1b_Ga=T,q=2,f=24,s={},v={}",
                bitmap.width, bitmap.height
            ));
            if last > 0 {
                out.push_str(",m=1");
            }
        } else {
            out.push_str(&format!("\x1b_Gm={}", u8::from(index != last)));
        }
        out.push(';');
        out.push_str(chunk);
        out.push_str("\x1b\\");
    }
    out
}

/// The iTerm2 inline image escape: a grayscale PNG in `OSC 1337 File`.
pub fn encode_iterm2(bitmap: &QrBitmap) -> anyhow::Result<String> {
    use image::ImageEncoder as _;

    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png).write_image(
        &grayscale_of(bitmap),
        u32::try_from(bitmap.width)?,
        u32::try_from(bitmap.height)?,
        image::ExtendedColorType::L8,
    )?;
    let payload = base64::engine::general_purpose::STANDARD.encode(&png);
    Ok(format!(
        "\x1b]1337;File=inline=1;size={}:{payload}\x07",
        png.len()
    ))
}

/// A black and white sixel image: in every band of six rows, a white pass,
/// a carriage return, a black pass. The white pass is needed: sixel leaves
/// unpainted pixels in the terminal's background colour, and a code on a
/// dark background without its white quiet zone does not scan.
pub fn encode_sixel(bitmap: &QrBitmap) -> String {
    let mut out = format!(
        "\x1bPq\"1;1;{};{}#0;2;100;100;100#1;2;0;0;0",
        bitmap.width, bitmap.height
    );
    for band_top in (0..bitmap.height).step_by(6) {
        if band_top > 0 {
            out.push('-');
        }
        out.push_str("#0");
        sixel_band_pass(&mut out, bitmap, band_top, false);
        out.push_str("$#1");
        sixel_band_pass(&mut out, bitmap, band_top, true);
    }
    out.push_str("\x1b\\");
    out
}

/// One colour of the band at `band_top`, with runs of four and more
/// written as `!<count><sixel>`.
fn sixel_band_pass(out: &mut String, bitmap: &QrBitmap, band_top: usize, dark: bool) {
    let flush = |run: Option<(char, usize)>, out: &mut String| {
        let Some((sixel, count)) = run else {
            return;
        };
        if count >= 4 {
            out.push_str(&format!("!{count}{sixel}"));
        } else {
            out.extend(std::iter::repeat_n(sixel, count));
        }
    };
    let mut run: Option<(char, usize)> = None;
    for x in 0..bitmap.width {
        let mut bits = 0u8;
        for (bit, y) in (band_top..bitmap.height.min(band_top + 6)).enumerate() {
            if bitmap.is_dark(x, y) == dark {
                bits |= 1 << bit;
            }
        }
        let sixel = char::from(63 + bits);
        match &mut run {
            Some((current, count)) if *current == sixel => *count += 1,
            _ => {
                flush(run.take(), out);
                run = Some((sixel, 1));
            },
        }
    }
    flush(run, out);
}

/// The code of `uri` as the image escape of `display`, at the largest scale
/// that fits `geometry`.
pub fn image(uri: &str, display: QrDisplay, geometry: &TerminalGeometry) -> anyhow::Result<String> {
    let code = crate::deposit::qr::code(uri)?;
    let modules: Vec<bool> = code
        .to_colors()
        .iter()
        .map(|color| *color == qrcode::Color::Dark)
        .collect();
    let scale = fit_module_scale(code.width() + 2 * QUIET_ZONE, geometry);
    let bitmap = QrBitmap::from_modules(&modules, code.width(), scale, QUIET_ZONE);
    match display {
        QrDisplay::Kitty => Ok(encode_kitty(&bitmap)),
        QrDisplay::Iterm2 => encode_iterm2(&bitmap),
        QrDisplay::Sixel => Ok(encode_sixel(&bitmap)),
        QrDisplay::Text => anyhow::bail!("the text rendering is not an image"),
    }
}

/// What this process's terminal shows a QR code as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detection {
    /// The rendering.
    pub display: QrDisplay,
    /// The cell size the terminal reported, when it did.
    pub cell_px: Option<(usize, usize)>,
}

/// The probe's answer, asked once per process.
static PROBE: OnceLock<ProbeReply> = OnceLock::new();

/// The rendering of a QR code drawn on a terminal, with `force` from
/// `--qr-display`.
pub fn detect(force: Option<QrDisplay>) -> Detection {
    use std::io::IsTerminal as _;

    // Nobody to answer a question is nobody to scan a code either.
    let tty = std::io::stdin().is_terminal();
    let env = TerminalEnv::from_lookup(|key| std::env::var(key).ok(), tty, force);
    let probe = if probe_can_matter(&env) {
        PROBE.get_or_init(probe::run).clone()
    } else {
        ProbeReply::default()
    };
    let chosen = choose_display(&env, &probe);
    // A picture that does not scan is the one way this goes wrong: keep the
    // evidence behind the choice.
    tracing::debug!(?chosen, ?env, ?probe, "QR display");
    Detection {
        display: chosen,
        cell_px: probe.cell_px,
    }
}

/// The window of the terminal on stdout, or on stderr when stdout is not
/// one. The pixels are often unknown, and then `cell` is `None`.
pub fn window_geometry() -> TerminalGeometry {
    for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCGWINSZ writes one `winsize` into the struct it is
        // given and nothing else.
        let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
        if rc == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            let (rows, cols) = (usize::from(ws.ws_row), usize::from(ws.ws_col));
            return TerminalGeometry {
                rows,
                cols,
                cell: (ws.ws_xpixel > 0 && ws.ws_ypixel > 0).then(|| {
                    (
                        usize::from(ws.ws_xpixel) / cols,
                        usize::from(ws.ws_ypixel) / rows,
                    )
                }),
            };
        }
    }
    TerminalGeometry::default()
}

/// The terminal a QR code is drawn on: what it shows and how big it is.
#[derive(Clone, Copy)]
pub struct QrTerminal {
    /// The rendering; see [`detect`].
    pub detect: fn(Option<QrDisplay>) -> Detection,
    /// The window; see [`window_geometry`].
    pub geometry: fn() -> TerminalGeometry,
}

impl std::fmt::Debug for QrTerminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("QrTerminal")
    }
}

impl QrTerminal {
    /// This process's terminal.
    pub fn process() -> Self {
        Self {
            detect,
            geometry: window_geometry,
        }
    }

    /// A terminal that is never asked and gets text.
    #[cfg(test)]
    pub fn text() -> Self {
        Self {
            detect: |_| Detection {
                display: QrDisplay::Text,
                cell_px: None,
            },
            geometry: TerminalGeometry::default,
        }
    }

    /// The image of `uri` with [`IMAGE_HINT`] under it, when this terminal
    /// shows one; `None` for text. An image that cannot be made is text too,
    /// with a warning in the log.
    pub fn image(&self, uri: &str, force: Option<QrDisplay>) -> Option<String> {
        let detected = (self.detect)(force);
        if detected.display == QrDisplay::Text {
            return None;
        }
        let mut geometry = (self.geometry)();
        // The terminal's own answer wins: the system often reports the
        // window without its pixels.
        if detected.cell_px.is_some() {
            geometry.cell = detected.cell_px;
        }
        match image(uri, detected.display, &geometry) {
            Ok(escape) => Some(format!("{escape}\n{IMAGE_HINT}")),
            Err(e) => {
                tracing::warn!("the QR code could not be drawn as an image, showing text: {e:#}");
                None
            },
        }
    }
}

/// The capability probe.
mod probe {
    use std::{
        fs::File,
        io::{Read as _, Write as _},
        os::unix::io::AsRawFd as _,
        time::{Duration, Instant},
    };

    use super::{parse_probe_reply, ProbeReply, PROBE_TIMEOUT};

    /// Three questions answered in this order: the kitty graphics query
    /// (one dummy pixel, `a=q`), the cell size in pixels, and DA1, which
    /// every terminal answers and which therefore goes last.
    const QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c";

    /// What the terminal answered; every failure is an empty answer.
    pub(super) fn run() -> ProbeReply {
        probe(QUERY, PROBE_TIMEOUT).unwrap_or_default()
    }

    /// The terminal without line editing and echo: answers arrive at once
    /// and are not shown. Restored when dropped, by a panic too. `ISIG`
    /// stays on, so Ctrl-C still interrupts.
    struct RawMode {
        /// The terminal.
        fd: libc::c_int,
        /// Its settings before.
        saved: libc::termios,
    }

    impl RawMode {
        fn enter(fd: libc::c_int) -> Option<RawMode> {
            let mut saved = std::mem::MaybeUninit::<libc::termios>::uninit();
            // SAFETY: tcgetattr fills the termios it is given when it
            // succeeds.
            if unsafe { libc::tcgetattr(fd, saved.as_mut_ptr()) } != 0 {
                return None;
            }
            // SAFETY: filled by the successful tcgetattr above.
            let saved = unsafe { saved.assume_init() };
            let mut raw = saved;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            // A read returns after 0.1 s of silence with what arrived.
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 1;
            // SAFETY: a valid termios made from the current one.
            if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
                return None;
            }
            Some(RawMode {
                fd,
                saved,
            })
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            // SAFETY: the termios saved by `enter`. A failure leaves the
            // terminal as it is; there is nothing better to do.
            unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
        }
    }

    /// Writes `query` to the controlling terminal and reads the answers
    /// until DA1 has come and a tenth of a second has passed in silence,
    /// within `timeout` for the answers and as long again for what follows
    /// them. The controlling terminal rather than stdin: a buffered stdin
    /// would keep bytes it read ahead where the next question cannot see
    /// them. Keys pressed during the probe are lost.
    fn probe(query: &[u8], timeout: Duration) -> Option<ProbeReply> {
        let mut tty = File::options()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        let _raw = RawMode::enter(tty.as_raw_fd())?;
        tty.write_all(query).ok()?;
        let deadline = Instant::now() + timeout;
        let hard_stop = deadline + timeout;
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 256];
        loop {
            let read = tty.read(&mut chunk).ok()?;
            buffer.extend_from_slice(&chunk[..read]);
            let reply = parse_probe_reply(&buffer);
            let now = Instant::now();
            if reply.complete() {
                if read == 0 || now >= hard_stop {
                    return Some(reply);
                }
            } else if now >= deadline {
                return Some(reply);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URI: &str = "wc:c9c71dfb61298046cee15333ff7bd4431d01ac59814cc2783e1e4ba57c033d13@2?\
                       relay-protocol=irn&\
                       symKey=0f77e74c4faf2feee58fc2c41be0d0f32d5bd32150bdb548ffa65beb2b1ca573&\
                       expiryTimestamp=1700000300";

    // -- parse_probe_reply ---------------------------------------------------

    #[test]
    fn parse_reply_kitty_ok_and_da1_with_sixel() {
        let reply = parse_probe_reply(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;4;22c");
        assert!(reply.kitty);
        assert_eq!(reply.da1, Some(vec![62, 4, 22]));
        assert!(reply.sixel());
        assert!(reply.complete());
    }

    #[test]
    fn parse_reply_da1_only_without_sixel() {
        let reply = parse_probe_reply(b"\x1b[?1;2c");
        assert!(!reply.kitty);
        assert_eq!(reply.da1, Some(vec![1, 2]));
        assert!(!reply.sixel());
        assert!(reply.complete());
    }

    #[test]
    fn parse_reply_survives_keypress_garbage() {
        // Keys pressed during the probe land before the kitty answer and
        // between it and DA1.
        let reply = parse_probe_reply(b"qw\x1b_Gi=31;OK\x1b\\ab\x1b[?64;4c");
        assert!(reply.kitty);
        assert!(reply.sixel());
        assert!(reply.complete());
    }

    #[test]
    fn parse_reply_kitty_error_is_not_support() {
        // A terminal that answers the query but refuses its format cannot
        // show the image either.
        let reply = parse_probe_reply(b"\x1b_Gi=31;EINVAL:unsupported\x1b\\\x1b[?6c");
        assert!(!reply.kitty);
        assert!(reply.complete());
    }

    #[test]
    fn parse_reply_incomplete_until_da1() {
        // No DA1 yet, and an unterminated kitty answer counts for nothing.
        let reply = parse_probe_reply(b"\x1b_Gi=31;OK");
        assert!(!reply.kitty);
        assert!(!reply.complete());
    }

    #[test]
    fn parse_reply_skips_other_csi_question_answers() {
        // A DECRPM answer ends in `y`, not `c`: skipped, not read as DA1.
        let reply = parse_probe_reply(b"\x1b[?2026;2$y\x1b[?62;4c");
        assert_eq!(reply.da1, Some(vec![62, 4]));
    }

    #[test]
    fn parse_reply_reads_the_cell_size_as_width_by_height() {
        // `CSI 6 ; height ; width t`: height first.
        let reply = parse_probe_reply(b"\x1b[6;17;8t\x1b[?62;4c");
        assert_eq!(reply.cell_px, Some((8, 17)));
    }

    #[test]
    fn parse_reply_cell_size_survives_garbage_and_a_cursor_report() {
        // `CSI 6 ; 1 ; 1 R` is a cursor report, not a cell size.
        let reply = parse_probe_reply(b"x\x1b[6;1;1R\x1b_Gi=31;OK\x1b\\\x1b[6;32;16tz\x1b[?1;2c");
        assert_eq!(reply.cell_px, Some((16, 32)));
        assert!(reply.kitty);
        assert!(reply.complete());
    }

    #[test]
    fn parse_reply_zero_cell_size_is_unknown() {
        assert_eq!(parse_probe_reply(b"\x1b[6;0;8t\x1b[?1c").cell_px, None);
    }

    #[test]
    fn parse_reply_without_a_cell_answer_reports_none() {
        assert_eq!(parse_probe_reply(b"\x1b[?1;2c").cell_px, None);
    }

    // -- choose_display ------------------------------------------------------

    fn plain_tty() -> TerminalEnv {
        TerminalEnv {
            tty: true,
            ..TerminalEnv::default()
        }
    }

    fn probe_with(kitty: bool, da1: &[u16]) -> ProbeReply {
        ProbeReply {
            kitty,
            da1: Some(da1.to_vec()),
            cell_px: None,
        }
    }

    #[test]
    fn forced_text_wins_over_everything() {
        let env = TerminalEnv {
            force: Some(QrDisplay::Text),
            kitty_env: true,
            ..plain_tty()
        };
        assert_eq!(
            choose_display(&env, &probe_with(true, &[4])),
            QrDisplay::Text
        );
    }

    #[test]
    fn a_forced_protocol_wins_over_the_probe_and_the_tty() {
        let env = TerminalEnv {
            force: Some(QrDisplay::Sixel),
            ..TerminalEnv::default()
        };
        assert_eq!(
            choose_display(&env, &probe_with(true, &[])),
            QrDisplay::Sixel
        );
    }

    #[test]
    fn no_tty_is_text() {
        let env = TerminalEnv {
            kitty_env: true,
            ..TerminalEnv::default()
        };
        assert_eq!(
            choose_display(&env, &probe_with(true, &[4])),
            QrDisplay::Text
        );
    }

    #[test]
    fn a_kitty_answer_beats_sixel_and_the_environment() {
        let env = TerminalEnv {
            iterm2_env: true,
            ..plain_tty()
        };
        assert_eq!(
            choose_display(&env, &probe_with(true, &[4])),
            QrDisplay::Kitty
        );
    }

    #[test]
    fn sixel_without_kitty() {
        assert_eq!(
            choose_display(&plain_tty(), &probe_with(false, &[62, 4])),
            QrDisplay::Sixel
        );
    }

    #[test]
    fn kitty_by_name_when_the_probe_got_no_answer() {
        let env = TerminalEnv {
            kitty_env: true,
            ..plain_tty()
        };
        assert_eq!(
            choose_display(&env, &ProbeReply::default()),
            QrDisplay::Kitty
        );
    }

    #[test]
    fn kitty_by_name_is_refuted_by_an_answered_probe() {
        // Every kitty-graphics terminal answers the query: a probe answered
        // without `OK` means the name leaked in from an outer terminal.
        let env = TerminalEnv {
            kitty_env: true,
            ..plain_tty()
        };
        assert_eq!(
            choose_display(&env, &probe_with(false, &[1, 2])),
            QrDisplay::Text
        );
    }

    #[test]
    fn iterm2_by_name_when_the_probe_proves_nothing() {
        // iTerm2 has no query to answer, so its name keeps counting.
        let env = TerminalEnv {
            iterm2_env: true,
            ..plain_tty()
        };
        assert_eq!(
            choose_display(&env, &probe_with(false, &[1, 2])),
            QrDisplay::Iterm2
        );
    }

    #[test]
    fn a_multiplexer_is_text_whatever_is_heard_through_it() {
        let env = TerminalEnv {
            mux: true,
            kitty_env: true,
            iterm2_env: true,
            ..plain_tty()
        };
        assert_eq!(
            choose_display(&env, &probe_with(false, &[1, 2, 4])),
            QrDisplay::Text
        );
        assert_eq!(
            choose_display(&env, &probe_with(true, &[4])),
            QrDisplay::Text
        );
    }

    #[test]
    fn nothing_proven_is_text() {
        assert_eq!(
            choose_display(&plain_tty(), &ProbeReply::default()),
            QrDisplay::Text
        );
    }

    #[test]
    fn the_probe_matters_only_unforced_on_a_tty_outside_a_multiplexer() {
        assert!(probe_can_matter(&plain_tty()));
        assert!(!probe_can_matter(&TerminalEnv {
            force: Some(QrDisplay::Kitty),
            ..plain_tty()
        }));
        assert!(!probe_can_matter(&TerminalEnv::default()));
        assert!(!probe_can_matter(&TerminalEnv {
            mux: true,
            ..plain_tty()
        }));
    }

    // -- TerminalEnv::from_lookup --------------------------------------------

    fn env_of(pairs: &[(&str, &str)]) -> TerminalEnv {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        TerminalEnv::from_lookup(
            move |key: &str| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()),
            true,
            None,
        )
    }

    #[test]
    fn multiplexers_and_embedded_terminals_are_recognised() {
        assert!(env_of(&[("TMUX", "/tmp/tmux-1000/default,42,0")]).mux);
        assert!(env_of(&[("TERM", "screen-256color")]).mux);
        assert!(env_of(&[("TERM", "tmux-256color")]).mux);
        assert!(env_of(&[("ZELLIJ", "0")]).mux);
        assert!(env_of(&[("NVIM", "/run/user/1000/nvim.sock")]).mux);
        assert!(env_of(&[("INSIDE_EMACS", "30.1,vterm")]).mux);
        assert!(!env_of(&[("TERM", "xterm-256color")]).mux);
        assert!(!env_of(&[("TMUX", "")]).mux, "an empty variable is unset");
    }

    #[test]
    fn kitty_names_are_recognised() {
        assert!(env_of(&[("KITTY_WINDOW_ID", "1")]).kitty_env);
        assert!(env_of(&[("TERM", "xterm-kitty")]).kitty_env);
        assert!(env_of(&[("TERM", "xterm-ghostty")]).kitty_env);
        assert!(env_of(&[("GHOSTTY_RESOURCES_DIR", "/usr/share/ghostty")]).kitty_env);
        // Konsole speaks kitty graphics from 22.04.
        assert!(env_of(&[("KONSOLE_VERSION", "230801")]).kitty_env);
        assert!(!env_of(&[("KONSOLE_VERSION", "211203")]).kitty_env);
    }

    #[test]
    fn iterm2_names_are_recognised() {
        assert!(env_of(&[("TERM_PROGRAM", "iTerm.app")]).iterm2_env);
        assert!(env_of(&[("TERM_PROGRAM", "WezTerm")]).iterm2_env);
        // Over ssh iTerm2 passes on LC_TERMINAL, not TERM_PROGRAM.
        assert!(env_of(&[("LC_TERMINAL", "iTerm2")]).iterm2_env);
        assert!(!env_of(&[("TERM_PROGRAM", "Apple_Terminal")]).iterm2_env);
    }

    #[test]
    fn the_lookup_keeps_the_tty_and_the_forced_rendering() {
        let env = TerminalEnv::from_lookup(|_| None, false, Some(QrDisplay::Sixel));
        assert!(!env.tty);
        assert_eq!(env.force, Some(QrDisplay::Sixel));
        assert!(TerminalEnv::from_lookup(|_| None, true, None).tty);
    }

    // -- fit_module_scale ----------------------------------------------------

    /// A window that reports its cell.
    fn window(rows: usize, cols: usize) -> TerminalGeometry {
        TerminalGeometry {
            rows,
            cols,
            cell: Some((8, 16)),
        }
    }

    #[test]
    fn a_small_code_keeps_the_largest_scale() {
        assert_eq!(fit_module_scale(33, &window(45, 190)), 8);
    }

    #[test]
    fn a_large_code_shrinks_to_its_share_of_the_window() {
        // 45 rows * 66% * 16 px = 464 px: 4 px per module (420 px) fits,
        // 5 (525 px) does not.
        assert_eq!(fit_module_scale(105, &window(45, 190)), 4);
    }

    #[test]
    fn a_short_window_stops_at_the_smallest_scale() {
        assert_eq!(fit_module_scale(105, &window(10, 190)), 2);
    }

    #[test]
    fn a_narrow_window_binds_before_its_height() {
        // 40 columns * 8 px = 320 px against 464 px of height.
        assert_eq!(fit_module_scale(105, &window(45, 40)), 3);
    }

    #[test]
    fn an_unreported_cell_is_assumed_8_by_16() {
        let unknown = TerminalGeometry {
            rows: 45,
            cols: 190,
            cell: None,
        };
        assert_eq!(
            fit_module_scale(105, &unknown),
            fit_module_scale(105, &window(45, 190))
        );
    }

    #[test]
    fn a_reported_cell_is_used() {
        let hidpi = TerminalGeometry {
            rows: 45,
            cols: 190,
            cell: Some((16, 32)),
        };
        assert_eq!(fit_module_scale(105, &hidpi), 8);
    }

    #[test]
    fn an_unknown_window_keeps_the_largest_scale() {
        assert_eq!(fit_module_scale(105, &TerminalGeometry::default()), 8);
    }

    // -- QrBitmap ------------------------------------------------------------

    #[test]
    fn modules_are_scaled_inside_the_quiet_zone() {
        // [dark, light, dark], scale 2, quiet zone 1: 10x6, not 6x10.
        let bitmap = QrBitmap::from_modules(&[true, false, true], 3, 2, 1);
        assert_eq!((bitmap.width, bitmap.height), (10, 6));
        for x in 0..10 {
            assert!(!bitmap.is_dark(x, 0) && !bitmap.is_dark(x, 5));
        }
        for y in 0..6 {
            assert!(!bitmap.is_dark(0, y) && !bitmap.is_dark(9, y));
        }
        assert!(bitmap.is_dark(2, 2) && bitmap.is_dark(3, 3));
        assert!(!bitmap.is_dark(4, 2) && !bitmap.is_dark(5, 3));
        assert!(bitmap.is_dark(6, 2) && bitmap.is_dark(7, 3));
    }

    #[test]
    fn no_modules_make_an_empty_bitmap() {
        let empty = QrBitmap::from_modules(&[], 0, 8, 4);
        assert_eq!((empty.width, empty.height), (0, 0));
        assert!(empty.dark.is_empty());
    }

    // -- encoders ------------------------------------------------------------

    /// Each `ESC _G <keys> ; <payload> ESC \` chunk as `(keys, payload)`.
    fn kitty_chunks(encoded: &str) -> Vec<(String, String)> {
        encoded
            .split("\x1b\\")
            .filter(|part| !part.is_empty())
            .map(|part| {
                let body = part.strip_prefix("\x1b_G").expect("a chunk starts ESC _G");
                let (keys, payload) = body.split_once(';').expect("a chunk has a payload");
                (keys.to_string(), payload.to_string())
            })
            .collect()
    }

    #[test]
    fn kitty_one_chunk() {
        // [dark, light]: pixels 000000 ffffff, base64 AAAA////. `q=2` keeps
        // the terminal from answering into the input.
        let bitmap = QrBitmap {
            width: 2,
            height: 1,
            dark: vec![true, false],
        };
        assert_eq!(
            encode_kitty(&bitmap),
            "\x1b_Ga=T,q=2,f=24,s=2,v=1;AAAA////\x1b\\"
        );
    }

    #[test]
    fn kitty_chunks_mark_all_but_the_last() {
        // 64x40 dark: 7680 bytes, 10240 base64 characters, 4096+4096+2048.
        let bitmap = QrBitmap::from_modules(&[true; 640], 32, 2, 0);
        let chunks = kitty_chunks(&encode_kitty(&bitmap));
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].0, "a=T,q=2,f=24,s=64,v=40,m=1");
        assert_eq!(chunks[1].0, "m=1");
        assert_eq!(chunks[2].0, "m=0");
        assert!(chunks.iter().all(|(_, payload)| payload.len() <= 4096));
        let whole: String = chunks.iter().map(|(_, payload)| payload.as_str()).collect();
        let pixels = base64::engine::general_purpose::STANDARD
            .decode(whole)
            .expect("the payloads together are base64");
        assert_eq!(pixels, vec![0u8; 64 * 40 * 3]);
    }

    #[test]
    fn kitty_of_an_empty_bitmap_is_one_empty_chunk() {
        let empty = QrBitmap::from_modules(&[], 0, 8, 4);
        assert_eq!(encode_kitty(&empty), "\x1b_Ga=T,q=2,f=24,s=0,v=0;\x1b\\");
    }

    /// The PNG inside an iTerm2 escape, after checking its declared size.
    fn iterm2_png(encoded: &str) -> Vec<u8> {
        let body = encoded
            .strip_prefix("\x1b]1337;File=inline=1;size=")
            .expect("the iTerm2 prefix")
            .strip_suffix('\x07')
            .expect("BEL at the end");
        let (size, payload) = body.split_once(':').expect("size:payload");
        let png = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("base64");
        assert_eq!(size.parse::<usize>().expect("a number"), png.len());
        png
    }

    #[test]
    fn iterm2_wraps_a_grayscale_png() {
        let bitmap = QrBitmap {
            width: 3,
            height: 2,
            dark: vec![true, false, false, false, false, true],
        };
        let png = iterm2_png(&encode_iterm2(&bitmap).expect("encode"));
        let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("a PNG")
            .to_luma8();
        assert_eq!(img.dimensions(), (3, 2));
        assert_eq!(img.into_raw(), vec![0, 255, 255, 255, 255, 0]);
    }

    #[test]
    fn sixel_paints_white_then_black_in_each_band() {
        //   row 0:  D L L D
        //   row 1:  L D D L
        // Bit 0 is the top row: white pass A@@A, black pass @AA@.
        let bitmap = QrBitmap {
            width: 4,
            height: 2,
            dark: vec![true, false, false, true, false, true, true, false],
        };
        assert_eq!(
            encode_sixel(&bitmap),
            "\x1bPq\"1;1;4;2#0;2;100;100;100#1;2;0;0;0#0A@@A$#1@AA@\x1b\\"
        );
    }

    #[test]
    fn sixel_repeats_four_or_more() {
        let bitmap = QrBitmap {
            width: 6,
            height: 1,
            dark: vec![true; 6],
        };
        assert_eq!(
            encode_sixel(&bitmap),
            "\x1bPq\"1;1;6;1#0;2;100;100;100#1;2;0;0;0#0!6?$#1!6@\x1b\\"
        );
    }

    #[test]
    fn sixel_bands_are_separated_by_graphics_new_lines() {
        let bitmap = QrBitmap {
            width: 1,
            height: 12,
            dark: vec![true; 12],
        };
        assert_eq!(
            encode_sixel(&bitmap),
            "\x1bPq\"1;1;1;12#0;2;100;100;100#1;2;0;0;0#0?$#1~-#0?$#1~\x1b\\"
        );
    }

    // -- the image of a real code scans back -----------------------------------

    /// `uri` read off a grey image by a QR decoder.
    fn scan(img: image::GrayImage) -> String {
        let mut prepared = rqrr::PreparedImage::prepare(img);
        let grids = prepared.detect_grids();
        assert_eq!(grids.len(), 1, "one code in the image");
        grids[0].decode().expect("the code decodes").1
    }

    /// The pixels of a sixel escape this module writes: colour 0 is white
    /// and colour 1 black.
    fn decode_sixel(escape: &str) -> image::GrayImage {
        let body = escape
            .strip_prefix("\x1bPq\"1;1;")
            .and_then(|s| s.strip_suffix("\x1b\\"))
            .expect("a sixel escape");
        let (size, rest) = body.split_once('#').expect("colours follow the size");
        let (w, h) = size.split_once(';').expect("width;height");
        let (w, h): (u32, u32) = (w.parse().expect("width"), h.parse().expect("height"));
        let data = rest
            .strip_prefix("0;2;100;100;100#1;2;0;0;0")
            .expect("white and black");
        // Unpainted pixels stay grey, so a missed one shows.
        let mut img = image::GrayImage::from_pixel(w, h, image::Luma([128]));
        let (mut x, mut band, mut colour) = (0u32, 0u32, 0u8);
        let mut chars = data.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '#' => {
                    colour = chars.next().expect("a colour number") as u8 - b'0';
                },
                '$' => x = 0,
                '-' => {
                    x = 0;
                    band += 6;
                },
                '!' | '?'..='~' => {
                    let (count, sixel) = if c == '!' {
                        let mut n = String::new();
                        while chars.peek().is_some_and(char::is_ascii_digit) {
                            n.push(chars.next().expect("a digit"));
                        }
                        (n.parse().expect("a count"), chars.next().expect("a sixel"))
                    } else {
                        (1, c)
                    };
                    let bits = sixel as u8 - 63;
                    for _ in 0..count {
                        for bit in 0..6 {
                            if bits & (1 << bit) != 0 && band + bit < h {
                                let v = if colour == 1 { 0 } else { 255 };
                                img.put_pixel(x, band + bit, image::Luma([v]));
                            }
                        }
                        x += 1;
                    }
                },
                other => panic!("unexpected {other:?} in the sixel data"),
            }
        }
        assert!(
            img.pixels().all(|p| p.0[0] != 128),
            "every pixel is painted, the quiet zone included"
        );
        img
    }

    /// A window that holds the code at the largest scale.
    fn roomy() -> TerminalGeometry {
        window(200, 400)
    }

    #[test]
    fn the_kitty_image_of_a_pairing_uri_scans_back() {
        let escape = image(URI, QrDisplay::Kitty, &roomy()).expect("encode");
        let chunks = kitty_chunks(&escape);
        let keys = &chunks[0].0;
        let side = |k: &str| -> u32 {
            keys.split(',')
                .find_map(|kv| kv.strip_prefix(k))
                .expect("the size key")
                .parse()
                .expect("a number")
        };
        let (w, h) = (side("s="), side("v="));
        let whole: String = chunks.iter().map(|(_, payload)| payload.as_str()).collect();
        let rgb = base64::engine::general_purpose::STANDARD
            .decode(whole)
            .expect("base64");
        let gray: Vec<u8> = rgb.chunks(3).map(|px| px[0]).collect();
        let img = image::GrayImage::from_raw(w, h, gray).expect("w*h pixels");
        assert_eq!(scan(img), URI);
    }

    #[test]
    fn the_iterm2_image_of_a_pairing_uri_scans_back() {
        let escape = image(URI, QrDisplay::Iterm2, &roomy()).expect("encode");
        let img =
            image::load_from_memory_with_format(&iterm2_png(&escape), image::ImageFormat::Png)
                .expect("a PNG")
                .to_luma8();
        assert_eq!(scan(img), URI);
    }

    #[test]
    fn the_sixel_image_of_a_pairing_uri_scans_back() {
        let escape = image(URI, QrDisplay::Sixel, &roomy()).expect("encode");
        assert_eq!(scan(decode_sixel(&escape)), URI);
    }

    #[test]
    fn the_image_keeps_a_quiet_zone_of_four_modules() {
        // At scale 8 the first 32 pixel rows are white.
        let escape = image(URI, QrDisplay::Iterm2, &roomy()).expect("encode");
        let img =
            image::load_from_memory_with_format(&iterm2_png(&escape), image::ImageFormat::Png)
                .expect("a PNG")
                .to_luma8();
        let code = qrcode::QrCode::with_error_correction_level(URI, qrcode::EcLevel::M)
            .expect("the URI fits");
        assert_eq!(img.width() as usize, (code.width() + 8) * 8);
        assert!((0..32).all(|y| (0..img.width()).all(|x| img.get_pixel(x, y).0[0] == 255)));
        assert!((0..img.width()).any(|x| img.get_pixel(x, 32).0[0] == 0));
    }

    #[test]
    fn text_has_no_image() {
        assert!(image(URI, QrDisplay::Text, &roomy()).is_err());
    }

    // -- QrTerminal::image ---------------------------------------------------

    fn shows_kitty(_: Option<QrDisplay>) -> Detection {
        Detection {
            display: QrDisplay::Kitty,
            cell_px: None,
        }
    }

    fn shows_text(_: Option<QrDisplay>) -> Detection {
        Detection {
            display: QrDisplay::Text,
            cell_px: None,
        }
    }

    /// The forced rendering, text when none is forced.
    fn shows_the_forced_one(force: Option<QrDisplay>) -> Detection {
        Detection {
            display: force.unwrap_or(QrDisplay::Text),
            cell_px: None,
        }
    }

    /// Kitty, on a terminal that reports a 16x32 cell.
    fn shows_kitty_with_big_cells(_: Option<QrDisplay>) -> Detection {
        Detection {
            display: QrDisplay::Kitty,
            cell_px: Some((16, 32)),
        }
    }

    /// 20 rows, no cell size from the system.
    fn short_window() -> TerminalGeometry {
        TerminalGeometry {
            rows: 20,
            cols: 400,
            cell: None,
        }
    }

    /// The width `s=` of a kitty escape.
    fn kitty_width(shown: &str) -> usize {
        kitty_chunks(shown.split_once('\n').expect("escape, then the hint").0)[0]
            .0
            .split(',')
            .find_map(|kv| kv.strip_prefix("s="))
            .expect("s=")
            .parse()
            .expect("a number")
    }

    #[test]
    fn a_terminal_that_shows_images_gets_the_image_and_the_hint_under_it() {
        let terminal = QrTerminal {
            detect: shows_kitty,
            geometry: roomy,
        };
        let shown = terminal.image(URI, None).expect("an image");
        let (escape, hint) = shown.split_once('\n').expect("the hint on its own line");
        assert!(escape.starts_with("\x1b_Ga=T,"), "{escape:?}");
        assert_eq!(hint, IMAGE_HINT);
    }

    #[test]
    fn a_text_terminal_gets_no_image() {
        let terminal = QrTerminal {
            detect: shows_text,
            geometry: roomy,
        };
        assert_eq!(terminal.image(URI, None), None);
    }

    #[test]
    fn detection_hears_the_forced_rendering() {
        let terminal = QrTerminal {
            detect: shows_the_forced_one,
            geometry: roomy,
        };
        let sixel = terminal
            .image(URI, Some(QrDisplay::Sixel))
            .expect("an image");
        assert!(sixel.starts_with("\x1bPq"), "{sixel:?}");
        assert_eq!(terminal.image(URI, Some(QrDisplay::Text)), None);
    }

    #[test]
    fn the_cell_size_the_terminal_reports_is_used_over_the_assumed_one() {
        let assumed = QrTerminal {
            detect: shows_kitty,
            geometry: short_window,
        };
        let reported = QrTerminal {
            detect: shows_kitty_with_big_cells,
            geometry: short_window,
        };
        let small = kitty_width(&assumed.image(URI, None).expect("an image"));
        let large = kitty_width(&reported.image(URI, None).expect("an image"));
        assert!(large > small, "{large} > {small}");
    }
}
