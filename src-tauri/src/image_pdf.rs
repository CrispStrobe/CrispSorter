//! Model-free image preparation and image-only PDF export for the CLI.
//! Does not initialize the index, OCR engines, model manager, or a Tauri window.

use clap::{Args, ValueEnum};
use image::{DynamicImage, Rgb, RgbImage};
use serde::Serialize;
use std::path::PathBuf;
use std::time::Instant;

const MAX_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_PAGES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Paper {
    A4,
    Letter,
    Source,
}

#[derive(Debug, Clone, Args)]
pub struct ImagePdfArgs {
    /// JPEG, PNG, BMP or TIFF files, in output order. All TIFF frames are kept.
    #[arg(required = true, num_args = 1.., value_name = "IMAGE")]
    pub inputs: Vec<PathBuf>,
    /// Output PDF. Existing files require --force; input images are protected.
    #[arg(long, value_name = "PDF")]
    pub out: PathBuf,
    /// Make paper white with adaptive black-and-white conversion; preserve thin rules.
    #[arg(long)]
    pub black_and_white: bool,
    /// Correct rotation before export.
    #[arg(long)]
    pub deskew: bool,
    /// Also correct ruled-photo perspective (implies deskew, requires black-and-white).
    /// Older native libraries fall back to ordinary consensus deskew.
    #[arg(long, requires = "black_and_white")]
    pub perspective: bool,
    /// Fit without stretching. A4/letter orientation follows each prepared image.
    #[arg(long, value_enum, default_value = "a4")]
    pub paper: Paper,
    /// Fallback image resolution for --paper source, in dots per inch.
    #[arg(long, default_value_t = 300.0)]
    pub dpi: f64,
    /// Replace an existing output PDF after the new document finishes successfully.
    #[arg(long)]
    pub force: bool,
    /// Export PDF/A-2b when the native renderer supports it.
    #[arg(long)]
    pub pdfa: bool,
}

#[derive(Debug, Serialize)]
pub struct ImagePdfTiming {
    pub total_ms: f64,
    pub preparation_ms: f64,
    pub cleanup_ms: f64,
    pub pdf_ms: f64,
}

#[derive(Debug, Serialize)]
pub struct ImagePdfReport {
    pub saved_path: String,
    pub inputs: usize,
    pub pages: usize,
    pub bytes: u64,
    pub black_and_white: bool,
    pub perspective_requested: bool,
    pub timings: ImagePdfTiming,
}

fn elapsed(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn output_path(args: &ImagePdfArgs) -> Result<PathBuf, String> {
    if args.inputs.is_empty() {
        return Err("Select at least one input image.".into());
    }
    if !args.dpi.is_finite() || !(36.0..=2400.0).contains(&args.dpi) {
        return Err("DPI must be a finite number between 36 and 2400.".into());
    }
    if args.perspective && !args.black_and_white {
        return Err("--perspective requires --black-and-white.".into());
    }
    let requested = if args.out.is_absolute() {
        args.out.clone()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Read current directory: {e}"))?
            .join(&args.out)
    };
    let parent = requested
        .parent()
        .ok_or("Choose an output file.")?
        .canonicalize()
        .map_err(|e| format!("Open output directory: {e}"))?;
    let name = requested.file_name().ok_or("Choose an output filename.")?;
    let output = parent.join(name);
    let resolved = output.canonicalize().unwrap_or_else(|_| output.clone());
    for source in &args.inputs {
        if !source.is_file() {
            return Err(format!("Input is not a file: {}", source.display()));
        }
        let source = source
            .canonicalize()
            .map_err(|e| format!("Resolve input: {e}"))?;
        if source == resolved {
            return Err("The output must not replace an input image, even with --force.".into());
        }
    }
    if !args.force && std::fs::symlink_metadata(&output).is_ok() {
        return Err(format!(
            "Output already exists: {}. Use --force to replace it.",
            output.display()
        ));
    }
    Ok(output)
}

fn check_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > 32768
        || height > 32768
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err("Image exceeds the 64 megapixel or 32768-pixel axis limit.".into());
    }
    Ok(())
}

fn white_background(image: DynamicImage) -> RgbImage {
    let rgba = image.into_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let a = u32::from(p[3]);
        Rgb(std::array::from_fn(|c| {
            ((u32::from(p[c]) * a + 255 * (255 - a) + 127) / 255) as u8
        }))
    })
}

fn fit_paper(
    image: RgbImage,
    paper: Paper,
    source_points: Option<(f64, f64)>,
    dpi: f64,
) -> Result<(RgbImage, (f32, f32)), String> {
    if paper == Paper::Source {
        let (w, h) = source_points.unwrap_or((
            image.width() as f64 * 72.0 / dpi,
            image.height() as f64 * 72.0 / dpi,
        ));
        if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 || w > 14400.0 || h > 14400.0 {
            return Err("Physical page dimensions exceed the PDF page limit.".into());
        }
        return Ok((image, (w as f32, h as f32)));
    }
    let (mut pw, mut ph) = if paper == Paper::A4 {
        (595.2756, 841.8898)
    } else {
        (612.0, 792.0)
    };
    if image.width() > image.height() {
        std::mem::swap(&mut pw, &mut ph);
    }
    let ratio = pw / ph;
    let width = image
        .width()
        .max((image.height() as f64 * ratio).ceil() as u32);
    let height = image
        .height()
        .max((image.width() as f64 / ratio).ceil() as u32);
    check_dimensions(width, height)
        .map_err(|_| "Paper padding exceeds the image limit. Use --paper source.".to_string())?;
    let mut canvas = RgbImage::from_pixel(width, height, Rgb([255, 255, 255]));
    image::imageops::overlay(
        &mut canvas,
        &image,
        i64::from((width - image.width()) / 2),
        i64::from((height - image.height()) / 2),
    );
    Ok((canvas, (pw as f32, ph as f32)))
}

#[cfg(all(feature = "crispembed", feature = "ocr-reviewed-pdf"))]
pub fn run(
    args: &ImagePdfArgs,
    mut progress: impl FnMut(usize, usize),
) -> Result<ImagePdfReport, String> {
    let start = Instant::now();
    let output = output_path(args)?;
    let caps = crispembed::ocr_render_capabilities();
    if caps & 37 != 37 {
        return Err(
            "Image PDF export requires the matching native image/streaming PDF renderer.".into(),
        );
    }
    if args.pdfa && caps & 8 == 0 {
        return Err("PDF/A-2b is unavailable in this native renderer.".into());
    }
    let temp = tempfile::tempdir().map_err(|e| format!("Stage page images: {e}"))?;
    let cleanup = if args.black_and_white {
        Some(crispembed::CrispScanCleanup::new(None, 1)?)
    } else {
        None
    };
    let prepare_start = Instant::now();
    let mut cleanup_ms = 0.0;
    let mut pages = Vec::new();
    for (input_index, source) in args.inputs.iter().enumerate() {
        let reader = image::ImageReader::open(source)
            .map_err(|e| format!("Open {}: {e}", source.display()))?
            .with_guessed_format()
            .map_err(|e| format!("Inspect image: {e}"))?;
        let format = reader.format().ok_or("Unrecognized input image format.")?;
        if !matches!(
            format,
            image::ImageFormat::Jpeg
                | image::ImageFormat::Png
                | image::ImageFormat::Bmp
                | image::ImageFormat::Tiff
        ) {
            return Err(
                "Input must be JPEG, PNG, BMP or TIFF. Animated image formats are unsupported."
                    .into(),
            );
        }
        if format == image::ImageFormat::Png {
            let file = std::fs::File::open(source).map_err(|e| e.to_string())?;
            let decoder = image::codecs::png::PngDecoder::new(std::io::BufReader::new(file))
                .map_err(|e| e.to_string())?;
            if decoder.is_apng().map_err(|e| e.to_string())? {
                return Err("Animated PNG is unsupported; export its frames as TIFF first.".into());
            }
        }
        // Sniff the format rather than trusting the extension: a renamed TIFF
        // must still retain all its frames.
        let ext = if format == image::ImageFormat::Tiff {
            "tiff"
        } else {
            "raster"
        };
        let images = crate::extractors::page_source::rasterize_pages(source, ext)
            .map_err(|e| format!("Read {}: {e:#}", source.display()))?;
        for (index, path) in images.paths().iter().enumerate() {
            if pages.len() == MAX_PAGES {
                return Err(format!(
                    "Image PDF export supports at most {MAX_PAGES} pages."
                ));
            }
            let (w, h) =
                image::image_dimensions(path).map_err(|e| format!("Read page dimensions: {e}"))?;
            check_dimensions(w, h)?;
            let orientation = images.page_orientation(index);
            let normalized = crate::extractors::page_source::normalized_page(path, orientation)
                .map_err(|e| format!("Orient page: {e:#}"))?;
            let path = normalized.as_ref().map(|file| file.path()).unwrap_or(path);
            let mut image =
                white_background(image::open(path).map_err(|e| format!("Decode page: {e}"))?);
            let before = (image.width(), image.height());
            let mut points = images.page_size_points(index);
            if orientation >= 5 {
                points = points.map(|(w, h)| (h, w));
            }
            if let Some(cleanup) = &cleanup {
                let phase = Instant::now();
                let mut params = unsafe { crispembed_sys::crispembed_scan_cleanup_defaults() };
                params.deskew = i32::from(args.deskew || args.perspective);
                params.deskew_consensus = if args.perspective { 2 } else { 1 };
                params.crop_borders = 0;
                params.whiten_background = 0;
                params.despeckle = 0;
                params.blackfilter = 0;
                params.binarize = 1;
                params.binarize_method = 1;
                let (pixels, width, height) = cleanup.process_with_params(
                    image.as_raw(),
                    image.width() as i32,
                    image.height() as i32,
                    3,
                    params,
                )?;
                if width <= 0 || height <= 0 {
                    return Err("Cleanup returned invalid dimensions.".into());
                }
                check_dimensions(width as u32, height as u32)?;
                image = RgbImage::from_raw(width as u32, height as u32, pixels)
                    .ok_or("Cleanup returned invalid pixels.")?;
                cleanup_ms += elapsed(phase);
            } else if args.deskew {
                let phase = Instant::now();
                let angle = crispembed::CrispScanPhoto::correction_angle(
                    image.as_raw(),
                    image.width() as i32,
                    image.height() as i32,
                    3,
                    15.0,
                )?;
                if angle.abs() >= 0.05 {
                    let (pixels, width, height) = crispembed::CrispScanPhoto::rotate(
                        image.as_raw(),
                        image.width() as i32,
                        image.height() as i32,
                        3,
                        angle,
                    )?;
                    image = RgbImage::from_raw(width as u32, height as u32, pixels)
                        .ok_or("Deskew returned invalid pixels.")?;
                }
                cleanup_ms += elapsed(phase);
            }
            points = points.map(|(w, h)| {
                (
                    w * image.width() as f64 / before.0 as f64,
                    h * image.height() as f64 / before.1 as f64,
                )
            });
            let (image, points) = fit_paper(image, args.paper, points, args.dpi)?;
            let path = temp.path().join(format!("page-{:06}.png", pages.len() + 1));
            image
                .save(&path)
                .map_err(|e| format!("Save prepared page: {e}"))?;
            pages.push((
                path.to_str()
                    .ok_or("Temporary image path is not UTF-8.")?
                    .to_owned(),
                image.width() as i32,
                image.height() as i32,
                points,
            ));
        }
        progress(input_index + 1, args.inputs.len());
    }
    let preparation_ms = elapsed(prepare_start);
    let sizes: Vec<_> = pages.iter().map(|p| p.3).collect();
    let native: Vec<_> = pages
        .iter()
        .map(|p| crispembed::OcrRenderPageInput {
            regions: &[],
            page_width: p.1,
            page_height: p.2,
            image_path: Some(&p.0),
        })
        .collect();
    let mut pdf_ms = 0.0;
    let bytes = crate::ocr_atomic_output::atomic_write_with(&output, args.force, |file| {
        let phase = Instant::now();
        let bytes = crispembed::ocr_render_pdf_to_writer_checked(
            &native,
            args.pdfa,
            Some(&sizes),
            false,
            2 * 1024 * 1024 * 1024,
            file,
        )
        .map_err(|e| format!("Render image PDF: {e}"))?;
        pdf_ms = elapsed(phase);
        Ok(bytes)
    })?;
    Ok(ImagePdfReport {
        saved_path: output.display().to_string(),
        inputs: args.inputs.len(),
        pages: pages.len(),
        bytes,
        black_and_white: args.black_and_white,
        perspective_requested: args.perspective,
        timings: ImagePdfTiming {
            total_ms: elapsed(start),
            preparation_ms,
            cleanup_ms,
            pdf_ms,
        },
    })
}

#[cfg(not(all(feature = "crispembed", feature = "ocr-reviewed-pdf")))]
pub fn run(
    _args: &ImagePdfArgs,
    _progress: impl FnMut(usize, usize),
) -> Result<ImagePdfReport, String> {
    Err("Image PDF export requires a CrispSorter build with crispembed and ocr-reviewed-pdf features.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::path::Path;
    #[derive(Parser)]
    struct Options {
        #[command(flatten)]
        options: ImagePdfArgs,
    }
    fn args(input: &Path, output: &Path) -> ImagePdfArgs {
        ImagePdfArgs {
            inputs: vec![input.to_owned()],
            out: output.to_owned(),
            black_and_white: false,
            deskew: false,
            perspective: false,
            paper: Paper::A4,
            dpi: 300.0,
            force: false,
            pdfa: false,
        }
    }
    #[test]
    fn parser_retains_order_and_requires_binary_for_perspective() {
        let parsed = Options::try_parse_from([
            "cli",
            "z.jpg",
            "a.jpg",
            "--out",
            "x.pdf",
            "--black-and-white",
            "--perspective",
        ])
        .unwrap();
        assert_eq!(
            parsed.options.inputs,
            vec![PathBuf::from("z.jpg"), PathBuf::from("a.jpg")]
        );
        assert!(
            Options::try_parse_from(["cli", "a.jpg", "--out", "x.pdf", "--perspective"]).is_err()
        );
    }
    #[test]
    fn white_alpha_and_paper_do_not_stretch_pixels() {
        let mut rgba = image::RgbaImage::from_pixel(20, 10, image::Rgba([0, 0, 0, 0]));
        rgba.put_pixel(3, 4, image::Rgba([40, 80, 120, 255]));
        let rgb = white_background(DynamicImage::ImageRgba8(rgba));
        assert_eq!(rgb.get_pixel(0, 0).0, [255; 3]);
        let (canvas, points) = fit_paper(rgb, Paper::A4, None, 300.0).unwrap();
        assert!(points.0 > points.1);
        assert!(canvas.pixels().any(|p| p.0 == [40, 80, 120]));
        assert_eq!(canvas.pixels().filter(|p| p.0 == [40, 80, 120]).count(), 1);
    }
    #[test]
    fn protect_input_existing_output_and_invalid_dpi() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.png");
        std::fs::write(&input, b"original").unwrap();
        let mut options = args(&input, &input);
        options.force = true;
        assert!(output_path(&options).unwrap_err().contains("input image"));
        options.out = temp.path().join("out.pdf");
        std::fs::write(&options.out, b"existing").unwrap();
        options.force = false;
        assert!(output_path(&options).is_err());
        options.force = true;
        assert!(output_path(&options).is_ok());
        options.dpi = f64::NAN;
        assert!(output_path(&options).is_err());
    }
    #[cfg(all(feature = "crispembed", feature = "ocr-reviewed-pdf"))]
    #[test]
    fn native_export_retains_tiff_frames_and_failure_keeps_destination() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("renamed.bin");
        let mut encoder =
            tiff::encoder::TiffEncoder::new(std::fs::File::create(&input).unwrap()).unwrap();
        for value in [0, 255] {
            encoder
                .write_image::<tiff::encoder::colortype::RGB8>(32, 40, &vec![value; 32 * 40 * 3])
                .unwrap();
        }
        drop(encoder);
        let output = temp.path().join("out.pdf");
        let mut options = args(&input, &output);
        options.black_and_white = true;
        let report = run(&options, |_, _| {}).unwrap();
        assert_eq!(report.pages, 2);
        assert!(std::fs::read(&output).unwrap().starts_with(b"%PDF-"));
        let before = std::fs::read(&output).unwrap();
        options.force = true;
        let corrupt = temp.path().join("corrupt.jpg");
        std::fs::write(&corrupt, [0xff, 0xd8, 0xff]).unwrap();
        options.inputs.push(corrupt);
        assert!(run(&options, |_, _| {}).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), before);
    }
}
