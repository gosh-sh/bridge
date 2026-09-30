//! The pairing URI (or an EIP-681 URI) as a QR code: in the terminal,
//! and as an SVG or PNG file for `--qr-out`.

use std::path::Path;

use qrcode::{render::unicode::Dense1x2, EcLevel, QrCode};

/// The code for `uri` at error-correction level M.
fn code(uri: &str) -> anyhow::Result<QrCode> {
    Ok(QrCode::with_error_correction_level(
        uri.as_bytes(),
        EcLevel::M,
    )?)
}

/// The QR version (size class) `uri` needs at level M.
pub fn version(uri: &str) -> anyhow::Result<i16> {
    match code(uri)?.version() {
        qrcode::Version::Normal(v) => Ok(v),
        qrcode::Version::Micro(v) => Ok(v),
    }
}

/// The code as text, two module rows per line, with a quiet zone. `invert`
/// swaps dark and light for a light-on-dark terminal.
pub fn render_terminal(uri: &str, invert: bool) -> anyhow::Result<String> {
    let c = code(uri)?;
    let (dark, light) = if invert {
        (Dense1x2::Light, Dense1x2::Dark)
    } else {
        (Dense1x2::Dark, Dense1x2::Light)
    };
    Ok(c.render::<Dense1x2>()
        .dark_color(dark)
        .light_color(light)
        .quiet_zone(true)
        .build())
}

/// Writes the code to `path`, an `.svg` or a `.png` file.
pub fn write_file(path: &Path, uri: &str) -> anyhow::Result<()> {
    let c = code(uri)?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("svg") => std::fs::write(
            path,
            c.render::<qrcode::render::svg::Color>()
                .min_dimensions(320, 320)
                .build(),
        )?,
        Some("png") => c
            .render::<image::Luma<u8>>()
            .min_dimensions(320, 320)
            .build()
            .save(path)?,
        _ => anyhow::bail!("--qr-out {}: use a .svg or .png file name", path.display()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const URI: &str = "wc:c9c71dfb61298046cee15333ff7bd4431d01ac59814cc2783e1e4ba57c033d13@2?\
                       relay-protocol=irn&\
                       symKey=0f77e74c4faf2feee58fc2c41be0d0f32d5bd32150bdb548ffa65beb2b1ca573&\
                       expiryTimestamp=1700000300";

    #[test]
    fn a_pairing_uri_fits_a_small_code() {
        assert!(
            version(URI).unwrap() <= 10,
            "a phone camera must read it off a terminal"
        );
    }

    #[test]
    fn the_png_decodes_back_to_the_uri() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("qr.png");
        write_file(&p, URI).unwrap();
        let img = image::open(&p).unwrap().to_luma8();
        let mut prepared = rqrr::PreparedImage::prepare(img);
        let grids = prepared.detect_grids();
        let (_, content) = grids[0].decode().unwrap();
        assert_eq!(content, URI);
    }

    #[test]
    fn svg_and_terminal_render() {
        let d = tempfile::tempdir().unwrap();
        write_file(&d.path().join("qr.svg"), URI).unwrap();
        assert!(std::fs::read_to_string(d.path().join("qr.svg"))
            .unwrap()
            .contains("<svg"));
        assert!(write_file(&d.path().join("qr.txt"), URI).is_err());
        let t = render_terminal(URI, false).unwrap();
        assert!(t.lines().count() > 20);
    }
}
