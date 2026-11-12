//! Photos from the phone's Camera screen. They're saved like any file, then put on the clipboard and
//! announced with a preview.

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Larger than any phone camera, small enough that a bad file can't take all the memory.
const MAX_SIDE: u32 = 16_384;
const MAX_ALLOC: u64 = 1024 * 1024 * 1024;

/// PNG, as apps paste it more reliably than JPEG. Turned upright, as the clipboard has no EXIF.
pub fn to_png(photo: &[u8]) -> Result<Vec<u8>, String> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);

    let mut reader = ImageReader::new(Cursor::new(photo)).with_guessed_format().map_err(|error| error.to_string())?;
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    let orientation = decoder.orientation().map_err(|error| error.to_string())?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
    image.apply_orientation(orientation);

    let mut png = Vec::new();
    let encoder = PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive);
    image.write_with_encoder(encoder).map_err(|error| error.to_string())?;
    Ok(png)
}

async fn copy(path: &Path) -> Result<(), String> {
    let photo = tokio::fs::read(path).await.map_err(|error| error.to_string())?;
    let png = tokio::task::spawn_blocking(move || to_png(&photo)).await.map_err(|error| error.to_string())??;
    // wl-copy stays behind to serve the clipboard, so it mustn't hold a pipe this waits on
    let mut child = Command::new("wl-copy")
        .args(["--type", "image/png"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("can't run wl-copy: {error}"))?;
    let mut stdin = child.stdin.take().ok_or("wl-copy has no input")?;
    stdin.write_all(&png).await.map_err(|error| error.to_string())?;
    drop(stdin);
    let status = child.wait().await.map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("wl-copy failed ({status})"))
    }
}

fn open(target: &Path) {
    match Command::new("xdg-open").arg(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(mut child) => {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
        Err(error) => eprintln!("lis-daemon: can't open {}: {error}", target.display()),
    }
}

/// Copies it if asked to, then says so with a preview. Clicking opens it.
pub fn deliver(phone: String, path: PathBuf, clipboard: bool) {
    tokio::spawn(async move {
        let copied = if clipboard {
            match copy(&path).await {
                Ok(()) => Some(true),
                Err(error) => {
                    eprintln!("lis-daemon: can't copy the photo: {error}");
                    Some(false)
                }
            }
        } else {
            None
        };

        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let body = match copied {
            Some(true) => format!("{name}, copied to the clipboard"),
            Some(false) => format!("{name}. It couldn't be copied to the clipboard"),
            None => name,
        };
        let shown = path.display().to_string();
        // `--` keeps text from the phone from being read as options
        let answer = Command::new("notify-send")
            .args(["-a", "Lis App", "-h", &format!("string:image-path:{shown}")])
            .args(["-A", "default=Open", "-A", "folder=Show in Folder"])
            .args(["--", &format!("Photo from {phone}"), &body])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await;
        match answer.map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string()).as_deref() {
            Ok("default") => open(&path),
            Ok("folder") => open(path.parent().unwrap_or(&path)),
            Ok(_) => {}
            Err(error) => eprintln!("lis-daemon: can't run notify-send: {error}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::to_png;
    use image::codecs::jpeg::JpegEncoder;
    use image::{ImageEncoder, RgbImage};

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let image = RgbImage::from_fn(width, height, |x, _| if x < width / 2 { [255, 0, 0].into() } else { [0, 0, 255].into() });
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 90)
            .write_image(image.as_raw(), width, height, image::ExtendedColorType::Rgb8)
            .unwrap();
        bytes
    }

    #[test]
    fn a_jpeg_becomes_a_png_of_the_same_size() {
        let png = to_png(&jpeg(64, 48)).unwrap();
        let decoded = image::load_from_memory(&png).unwrap();
        assert_eq!(image::guess_format(&png).unwrap(), image::ImageFormat::Png);
        assert_eq!((decoded.width(), decoded.height()), (64, 48));
    }

    /// A phone that only marks the rotation in EXIF still gives an upright clipboard image.
    #[test]
    fn exif_rotation_is_applied() {
        let mut photo = jpeg(64, 48);
        // APP1 Exif, big-endian TIFF, one IFD entry: Orientation (0x0112) = 6, turn 90° clockwise
        let exif: [u8; 36] = [
            0xFF, 0xE1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0, 0, b'M', b'M', 0, 0x2A, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1,
            0, 6, 0, 0, 0, 0, 0, 0,
        ];
        photo.splice(2..2, exif);
        let decoded = image::load_from_memory(&to_png(&photo).unwrap()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (48, 64));
    }

    #[test]
    fn something_that_is_not_an_image_is_an_error() {
        assert!(to_png(b"not a photo").is_err());
    }
}
