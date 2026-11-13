#[cfg(test)]
#[path = "../../../src-tauri/src/extractors/paddle_resize.rs"]
mod paddle_resize;
#[cfg(test)]
mod image_tests {
    use super::paddle_resize::svtr_pre_resize;
    use fast_image_resize::{
        images::{CroppedImageMut, Image},
        pixels::PixelType,
        FilterType, ResizeAlg, ResizeOptions, Resizer,
    };

    #[test]
    fn upstream_wide_line_error_is_reproduced_and_full_line_resize_succeeds() {
        let (w, h, cw, ch) = (1000, 20, 960, 48);
        let mut old = Image::new(cw, ch, PixelType::U8x3);
        let scaled = ((ch as f32 / h as f32) * w as f32).round() as u32;
        let error = CroppedImageMut::new(&mut old, 0, 0, scaled, ch)
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "Size of the crop box is out of the image boundaries"
        );
        let mut pixels = vec![0; w as usize * h as usize * 3];
        for row in 0..h as usize {
            for x in 0..w as usize {
                pixels[(row * w as usize + x) * 3 + if x < w as usize / 2 { 0 } else { 2 }] = 255;
            }
        }
        let src = Image::from_vec_u8(w, h, pixels, PixelType::U8x3).unwrap();
        let (nw, nh) = svtr_pre_resize(w, h, cw, ch).unwrap().unwrap();
        let mut bounded = Image::new(nw, nh, PixelType::U8x3);
        Resizer::new()
            .resize(
                &src,
                &mut bounded,
                &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
            )
            .unwrap();
        // Both ends survive: this is a full-line resize, not a clipped prefix.
        assert_eq!(&bounded.buffer()[..3], &[255, 0, 0]);
        let n = bounded.buffer().len();
        assert_eq!(&bounded.buffer()[n - 3..], &[0, 0, 255]);
        assert!(CroppedImageMut::new(&mut bounded, 0, 0, cw, ch).is_ok());
    }
}
