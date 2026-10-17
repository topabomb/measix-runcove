//! Bounded image decoding shared by file reads and desktop observations.
use image_codec::{
    codecs::{gif::GifDecoder, jpeg::JpegEncoder, png::PngDecoder, webp::WebPDecoder},
    imageops::FilterType,
    metadata::Orientation,
    AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits,
};
use serde::Serialize;
use std::{
    io::Cursor,
    sync::{Arc, OnceLock},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const SOURCE_LIMIT: usize = 10 * 1024 * 1024;
/// Internal Base64 report, including bounded JSON/path metadata. Independent of
/// the final MCP result budget and of text output limits.
pub const REPORT_LIMIT: usize = SOURCE_LIMIT.div_ceil(3) * 4 + 64 * 1024;
pub const OUTPUT_LIMIT: usize = 4_500_000;
pub const SCREEN_LIMIT: usize = 2 * 1024 * 1024;
const PIXEL_LIMIT: u64 = 16_000_000;

/// Acquire before reading binary input. Move the permit into the actual codec
/// worker, so abandoning a request cannot release capacity while it still runs.
pub async fn acquire() -> anyhow::Result<OwnedSemaphorePermit> {
    static POOL: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Ok(POOL
        .get_or_init(|| Arc::new(Semaphore::new(2)))
        .clone()
        .acquire_owned()
        .await?)
}

#[derive(Debug, Serialize)]
pub struct Picture {
    #[serde(skip)]
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
    pub original_width: u32,
    pub original_height: u32,
    pub width: u32,
    pub height: u32,
    pub resized: bool,
    pub orientation_corrected: bool,
    pub first_frame_only: bool,
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(16_000_000);
    limits.max_image_height = Some(16_000_000);
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits
}
fn dimensions(decoder: &impl ImageDecoder) -> anyhow::Result<(u32, u32)> {
    let (width, height) = decoder.dimensions();
    anyhow::ensure!(width > 0 && height > 0, "INVALID_IMAGE: empty dimensions");
    anyhow::ensure!(
        u64::from(width) * u64::from(height) <= PIXEL_LIMIT
            && decoder.total_bytes() <= 128 * 1024 * 1024,
        "IMAGE_TOO_LARGE: decoded image exceeds 16 million pixels or the decode budget"
    );
    Ok((width, height))
}
fn first<'a>(decoder: impl AnimationDecoder<'a>) -> anyhow::Result<DynamicImage> {
    Ok(DynamicImage::ImageRgba8(
        decoder
            .into_frames()
            .next()
            .ok_or_else(|| anyhow::anyhow!("INVALID_IMAGE: missing first frame"))??
            .into_buffer(),
    ))
}

pub fn process(bytes: Vec<u8>, screen_budget: Option<usize>) -> anyhow::Result<Picture> {
    anyhow::ensure!(
        bytes.len() <= SOURCE_LIMIT,
        "IMAGE_TOO_LARGE: source exceeds 10 MiB"
    );
    let format = image_codec::guess_format(&bytes)
        .map_err(|_| anyhow::anyhow!("UNSUPPORTED_MEDIA: expected JPEG, PNG, WebP, or GIF"))?;
    anyhow::ensure!(
        matches!(
            format,
            ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP | ImageFormat::Gif
        ),
        "UNSUPPORTED_MEDIA: expected JPEG, PNG, WebP, or GIF"
    );
    let (original_width, original_height, orientation, first_frame_only, mut image) = {
        let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
        reader.limits(limits());
        let mut decoder = reader.into_decoder()?;
        let (original_width, original_height) = dimensions(&decoder)?;
        let orientation = decoder.orientation()?;
        // Inspect only headers before choosing a single-frame decoder. Never collect
        // an animation: its total frame count does not bound resource consumption.
        let mut first_frame_only = false;
        let image = match format {
            ImageFormat::Gif => {
                let mut decoder = GifDecoder::new(Cursor::new(&bytes))?;
                decoder.set_limits(limits())?;
                first_frame_only = true;
                first(decoder)?
            }
            ImageFormat::WebP => {
                let mut decoder = WebPDecoder::new(Cursor::new(&bytes))?;
                decoder.set_limits(limits())?;
                first_frame_only = decoder.has_animation();
                if first_frame_only {
                    first(decoder)?
                } else {
                    DynamicImage::from_decoder(decoder)?
                }
            }
            ImageFormat::Png => {
                let decoder = PngDecoder::with_limits(Cursor::new(&bytes), limits())?;
                first_frame_only = decoder.is_apng()?;
                if first_frame_only {
                    first(decoder.apng()?)?
                } else {
                    DynamicImage::from_decoder(decoder)?
                }
            }
            _ => DynamicImage::from_decoder(decoder)?,
        };
        (
            original_width,
            original_height,
            orientation,
            first_frame_only,
            image,
        )
    };
    let orientation_corrected = orientation != Orientation::NoTransforms;
    image.apply_orientation(orientation);
    let oriented = (image.width(), image.height());
    let mut picture = Picture {
        bytes: vec![],
        mime_type: "image/png",
        original_width,
        original_height,
        width: oriented.0,
        height: oriented.1,
        resized: false,
        orientation_corrected,
        first_frame_only,
    };
    if screen_budget.is_none()
        && !orientation_corrected
        && !first_frame_only
        && oriented.0.max(oriented.1) <= 2000
        && bytes.len() <= OUTPUT_LIMIT
    {
        picture.mime_type = match format {
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::WebP => "image/webp",
            _ => "image/png",
        };
        picture.bytes = bytes;
        return Ok(picture);
    }
    let budget = screen_budget.unwrap_or(OUTPUT_LIMIT);
    let alpha = image.color().has_alpha() && screen_budget.is_none();
    for (edge, quality) in [(2000, 80), (2000, 70), (1600, 70), (1280, 70)] {
        if alpha && edge == 2000 && quality == 70 {
            continue;
        }
        let candidate = if image.width().max(image.height()) > edge {
            image.resize(edge, edge, FilterType::Triangle)
        } else {
            image.clone()
        };
        let mut encoded = vec![];
        if alpha {
            candidate.write_to(&mut Cursor::new(&mut encoded), ImageFormat::Png)?;
        } else {
            JpegEncoder::new_with_quality(&mut encoded, quality)
                .encode_image(&candidate.to_rgb8())?;
        }
        if encoded.len() <= budget {
            picture.width = candidate.width();
            picture.height = candidate.height();
            picture.resized = oriented != (picture.width, picture.height);
            picture.mime_type = if alpha { "image/png" } else { "image/jpeg" };
            picture.bytes = encoded;
            return Ok(picture);
        }
    }
    anyhow::bail!(
        "IMAGE_BUDGET_EXCEEDED: image cannot fit the output budget after bounded compression"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn encode(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Cursor::new(vec![]);
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }
    #[test]
    fn supported_formats_and_transparency() {
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::Gif,
            ImageFormat::WebP,
        ] {
            let source = encode(DynamicImage::new_rgb8(40, 20), format);
            let output = process(source.clone(), None).unwrap();
            assert_eq!((output.width, output.height), (40, 20));
            if format != ImageFormat::Gif {
                assert_eq!(output.bytes, source);
            }
        }
        let output = process(
            encode(DynamicImage::new_rgba8(2100, 10), ImageFormat::Png),
            None,
        )
        .unwrap();
        assert_eq!(output.width, 2000);
        assert!(image_codec::load_from_memory(&output.bytes)
            .unwrap()
            .color()
            .has_alpha());
        assert_eq!(
            image_codec::load_from_memory(&output.bytes)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)[3],
            0
        );
    }
    #[test]
    fn rejects_invalid_and_oversized_images() {
        assert!(process(b"not an image".to_vec(), None).is_err());
        assert!(process(vec![0; SOURCE_LIMIT + 1], None).is_err());
        let source = encode(DynamicImage::new_rgb8(4001, 4000), ImageFormat::Png);
        assert!(process(source, None)
            .unwrap_err()
            .to_string()
            .contains("IMAGE_TOO_LARGE"));
        assert!(process(
            encode(DynamicImage::new_rgb8(40, 20), ImageFormat::Png),
            Some(1)
        )
        .is_err());
    }
    #[test]
    fn corrects_exif_orientation_and_composites_only_the_first_gif_frame() {
        let jpeg = encode(DynamicImage::new_rgb8(40, 20), ImageFormat::Jpeg);
        let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut rotated = jpeg[..2].to_vec();
        rotated.extend_from_slice(&[0xff, 0xe1]);
        rotated.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        rotated.extend_from_slice(exif);
        rotated.extend_from_slice(&jpeg[2..]);
        let output = process(rotated, None).unwrap();
        assert!(output.orientation_corrected);
        assert_eq!((output.original_width, output.original_height), (40, 20));
        assert_eq!((output.width, output.height), (20, 40));
        let mut gif = vec![];
        {
            let mut encoder = image_codec::codecs::gif::GifEncoder::new(&mut gif);
            for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
                encoder
                    .encode_frame(image_codec::Frame::new(image_codec::RgbaImage::from_pixel(
                        10,
                        20,
                        image_codec::Rgba(color),
                    )))
                    .unwrap();
            }
        }
        let output = process(gif, None).unwrap();
        assert!(output.first_frame_only);
        assert_eq!(
            image_codec::load_from_memory(&output.bytes)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)
                .0,
            [255, 0, 0, 255]
        );
    }
    #[tokio::test]
    async fn abandoned_codec_waiter_does_not_release_the_workers_permit() {
        let permit = acquire().await.unwrap();
        let second = acquire().await.unwrap();
        let (release, work) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work.recv().unwrap();
        });
        let waiter = tokio::spawn(async move {
            worker.await.unwrap();
        });
        waiter.abort();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), acquire())
                .await
                .is_err()
        );
        release.send(()).unwrap();
        let recovered = tokio::time::timeout(std::time::Duration::from_secs(2), acquire())
            .await
            .unwrap()
            .unwrap();
        drop((second, recovered));
    }
}
