//! Bound SVTR's height-fit preprocessing to its actual model canvas.
//!
//! usls 0.2.0-alpha.3 creates a destination crop using the unbounded scaled
//! width. Wide lines then fail before inference. Paddle's recognizer clamps
//! resized width instead of dropping text.

/// Return a required pre-resize, or leave an already representable crop intact.
/// The full source extent is retained. u64 products cover every u32 dimension.
pub(super) fn svtr_pre_resize(
    width: u32,
    height: u32,
    canvas_width: u32,
    canvas_height: u32,
) -> Result<Option<(u32, u32)>, &'static str> {
    if [width, height, canvas_width, canvas_height].contains(&0) {
        return Err("SVTR requires nonzero image and model dimensions");
    }
    let scaled_numerator = u64::from(width) * u64::from(canvas_height);
    let canvas_numerator = u64::from(canvas_width) * u64::from(height);
    if scaled_numerator > canvas_numerator {
        return Ok(Some((canvas_width, canvas_height)));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::svtr_pre_resize;

    #[test]
    fn wide_text_line_fits_the_model_without_clipping_the_region() {
        assert_eq!(svtr_pre_resize(3200, 80, 960, 48), Ok(Some((960, 48))));
        assert_eq!(svtr_pre_resize(801, 40, 960, 48), Ok(Some((960, 48))));
    }

    #[test]
    fn narrow_and_exact_boundary_lines_keep_existing_preprocessing() {
        for (width, height) in [(100, 40), (800, 40), (960, 48), (1, 96)] {
            assert_eq!(svtr_pre_resize(width, height, 960, 48), Ok(None));
        }
    }

    #[test]
    fn zero_dimensions_are_rejected_before_resizing() {
        for dimensions in [(0, 1, 1, 1), (1, 0, 1, 1), (1, 1, 0, 1), (1, 1, 1, 0)] {
            assert!(
                svtr_pre_resize(dimensions.0, dimensions.1, dimensions.2, dimensions.3).is_err()
            );
        }
    }

    #[test]
    fn extreme_dimensions_do_not_overflow_or_request_out_of_bounds_crops() {
        for width in [1, 2, 97, 800, 3200, u32::MAX] {
            for height in [1, 2, 97, 3200, u32::MAX] {
                for (canvas_width, canvas_height) in [(1, 1), (960, 48), (u32::MAX, u32::MAX)] {
                    match svtr_pre_resize(width, height, canvas_width, canvas_height).unwrap() {
                        Some((w, h)) => assert!(w > 0 && w <= canvas_width && h == canvas_height),
                        None => {
                            let resized = (f64::from(width) * f64::from(canvas_height)
                                / f64::from(height))
                            .round();
                            assert!(resized >= 0.0 && resized <= f64::from(canvas_width));
                        }
                    }
                }
            }
        }
    }
}
