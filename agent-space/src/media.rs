//! Bounded image decoding shared by file reads and desktop observations.
use image_codec::{
    codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder},
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
pub const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
pub const MAX_EDGE: u32 = 1980;
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

pub fn process(bytes: Vec<u8>, output_budget: Option<usize>) -> anyhow::Result<Picture> {
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
        mime_type: "image/webp",
        original_width,
        original_height,
        width: oriented.0,
        height: oriented.1,
        resized: false,
        orientation_corrected,
        first_frame_only,
    };
    // Every image entry point uses one resize and one encoding policy.
    // Do not shrink again or lower quality when the remaining budget is small.
    if oriented.0.max(oriented.1) > MAX_EDGE {
        image = image.resize(MAX_EDGE, MAX_EDGE, FilterType::Lanczos3);
    }
    picture.width = image.width();
    picture.height = image.height();
    picture.resized = oriented != (picture.width, picture.height);
    let rgba = image.to_rgba8();
    let mut config = webp::WebPConfig::new()
        .map_err(|_| anyhow::anyhow!("IMAGE_ENCODING_FAILED: WebP configuration unavailable"))?;
    config.quality = 90.0;
    config.method = 6;
    picture.bytes = webp::Encoder::from_rgba(&rgba, picture.width, picture.height)
        .encode_advanced(&config)
        .map_err(|e| anyhow::anyhow!("IMAGE_ENCODING_FAILED: {e:?}"))?
        .to_vec();
    anyhow::ensure!(
        picture.bytes.len() <= output_budget.unwrap_or(OUTPUT_LIMIT).min(OUTPUT_LIMIT),
        "IMAGE_BUDGET_EXCEEDED: WebP Q90 output exceeds the remaining image budget"
    );
    Ok(picture)
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
            assert_eq!(output.mime_type, "image/webp");
            assert_eq!(
                image_codec::guess_format(&output.bytes).unwrap(),
                ImageFormat::WebP
            );
            assert!(!output.resized);
        }
        let output = process(
            encode(DynamicImage::new_rgba8(2100, 10), ImageFormat::Png),
            None,
        )
        .unwrap();
        assert_eq!(output.width, MAX_EDGE);
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
    fn one_resize_same_quality_for_files_and_sequences() {
        let source = encode(DynamicImage::new_rgb8(1440, 6000), ImageFormat::Png);
        let file = process(source.clone(), None).unwrap();
        assert_eq!((file.width, file.height), (475, 1980));
        assert_eq!((file.original_width, file.original_height), (1440, 6000));
        assert!(file.resized);
        let sequence = process(source.clone(), Some(file.bytes.len())).unwrap();
        assert_eq!(file.bytes, sequence.bytes);
        assert!(process(source, Some(file.bytes.len() - 1))
            .unwrap_err()
            .to_string()
            .contains("IMAGE_BUDGET_EXCEEDED"));
        let portrait = process(
            encode(DynamicImage::new_rgb8(5, 2100), ImageFormat::Png),
            None,
        )
        .unwrap();
        assert_eq!((portrait.width, portrait.height), (5, MAX_EDGE));
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
        let pixel = image_codec::load_from_memory(&output.bytes)
            .unwrap()
            .to_rgba8();
        assert!(pixel.get_pixel(0, 0)[0] > 240);
        assert!(pixel.get_pixel(0, 0)[2] < 20);
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
