//! Arithmetic for the explicit experimental Ovis2 inference-copy resize.
//! Display/export images and the caller's prepared coordinate space stay intact.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputScale {
    pub prepared: (u32, u32),
    pub inference: (u32, u32),
}

pub fn gate(value: Option<&str>) -> bool {
    value.is_some_and(|s| {
        !s.is_empty()
            && !["0", "false", "off", "no"]
                .iter()
                .any(|v| s.eq_ignore_ascii_case(v))
    })
}

fn round32(v: u32) -> u32 {
    let base = v / 32;
    32 * (base + u32::from(v % 32 > 16 || (v % 32 == 16 && base % 2 == 1)))
}

// Mirror the pinned native Ovis2 dimension rules without allocating pixels.
fn native_dimensions(w: u32, h: u32) -> (u32, u32) {
    let (mut rw, mut rh) = (round32(w), round32(h));
    if u64::from(rw) * u64::from(rh) > 2880 * 2880 {
        let beta = (f64::from(w) * f64::from(h) / (2880.0 * 2880.0)).sqrt();
        rw = ((f64::from(w) / beta / 32.0).floor() as u32 * 32).max(32);
        rh = ((f64::from(h) / beta / 32.0).floor() as u32 * 32).max(32);
    } else if u64::from(rw) * u64::from(rh) < 448 * 448 {
        let beta = ((448.0 * 448.0) / (f64::from(w) * f64::from(h))).sqrt();
        rw = (f64::from(w) * beta / 32.0).ceil() as u32 * 32;
        rh = (f64::from(h) * beta / 32.0).ceil() as u32 * 32;
    }
    (rw, rh)
}

impl InputScale {
    pub fn plan(w: u32, h: u32) -> Result<Self, String> {
        if w == 0
            || h == 0
            || w > 32768
            || h > 32768
            || u64::from(w) * u64::from(h) > 16_777_216
            || f64::from(w.max(h)) / f64::from(w.min(h)) > 200.0
        {
            return Err(
                "Ovis resize input exceeds native dimension, pixel or aspect limits".into(),
            );
        }
        let native = native_dimensions(w, h);
        if native.0 >= 32 && native.1 >= 32 && u64::from(native.0) * u64::from(native.1) <= 262_144
        {
            return Ok(Self {
                prepared: (w, h),
                inference: (w, h),
            });
        }
        let beta = (f64::from(w) * f64::from(h) / 262_144.0).sqrt();
        let iw = ((f64::from(w) / beta / 32.0).floor() as u32 * 32).max(32);
        let ih = ((f64::from(h) / beta / 32.0).floor() as u32 * 32).max(32);
        let area = u64::from(iw) * u64::from(ih);
        if iw > w
            || ih > h
            || !(200_704..=262_144).contains(&area)
            || native_dimensions(iw, ih) != (iw, ih)
        {
            return Err(
                "Ovis resize cannot meet the native minimum and patch bounds without upscaling"
                    .into(),
            );
        }
        Ok(Self {
            prepared: (w, h),
            inference: (iw, ih),
        })
    }

    pub fn changed(self) -> bool {
        self.prepared != self.inference
    }

    pub fn map_box(self, x: f32, y: f32, w: f32, h: f32) -> Result<(f32, f32, f32, f32), String> {
        if ![x, y, w, h].iter().all(|v| v.is_finite() && *v >= 0.0)
            || w == 0.0
            || h == 0.0
            || f64::from(x) + f64::from(w) > f64::from(self.inference.0)
            || f64::from(y) + f64::from(h) > f64::from(self.inference.1)
        {
            return Err("Ovis resize returned invalid inference-input geometry".into());
        }
        let sx = f64::from(self.prepared.0) / f64::from(self.inference.0);
        let sy = f64::from(self.prepared.1) / f64::from(self.inference.1);
        let x1 = (f64::from(x) * sx) as f32;
        let y1 = (f64::from(y) * sy) as f32;
        let x2 = ((f64::from(x) + f64::from(w)) * sx) as f32;
        let y2 = ((f64::from(y) + f64::from(h)) * sy) as f32;
        let mapped = (x1, y1, x2 - x1, y2 - y1);
        if ![mapped.0, mapped.1, mapped.2, mapped.3]
            .iter()
            .all(|v| v.is_finite())
            || mapped.2 <= 0.0
            || mapped.3 <= 0.0
            || f64::from(mapped.0) + f64::from(mapped.2) > f64::from(self.prepared.0)
            || f64::from(mapped.1) + f64::from(mapped.3) > f64::from(self.prepared.1)
        {
            return Err(
                "Ovis resize cannot represent the mapped box safely in prepared-image pixels"
                    .into(),
            );
        }
        Ok(mapped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_rounding_and_patch_boundary() {
        assert_eq!(
            (round32(16), round32(48), round32(80), round32(112)),
            (0, 64, 64, 128)
        );
        assert!(!InputScale::plan(512, 512).unwrap().changed());
        let over = InputScale::plan(544, 512).unwrap();
        assert!(over.changed());
        assert!(u64::from(over.inference.0) * u64::from(over.inference.1) <= 262_144);
        assert!(!InputScale::plan(448, 448).unwrap().changed());
        assert!(!InputScale::plan(1, 1).unwrap().changed()); // native min-pixel resize remains unchanged
    }
    #[test]
    fn real_page_plan_and_geometry_return_to_prepared_pixels() {
        let p = InputScale::plan(1280, 2077).unwrap();
        assert_eq!(p.inference, (384, 640));
        assert_eq!(
            p.map_box(0.0, 0.0, 384.0, 640.0).unwrap(),
            (0.0, 0.0, 1280.0, 2077.0)
        );
        let (x, y, w, h) = p.map_box(19.2, 32.0, 38.4, 64.0).unwrap();
        assert!((x - 64.0).abs() < 0.001 && (y - 103.85).abs() < 0.001);
        assert!((w - 128.0).abs() < 0.001 && (h - 207.7).abs() < 0.001);
    }
    #[test]
    fn refuses_bad_dimensions_and_unmappable_geometry() {
        for (w, h) in [
            (0, 1),
            (1, 0),
            (u32::MAX, u32::MAX),
            (32769, 1),
            (8192, 8192),
            (32768, 1),
            (201, 1),
        ] {
            assert!(InputScale::plan(w, h).is_err(), "{w}x{h}");
        }
        let p = InputScale::plan(1280, 2077).unwrap();
        for b in [
            (f32::NAN, 0., 1., 1.),
            (0., 0., f32::INFINITY, 1.),
            (-1., 0., 1., 1.),
            (384., 0., 1., 1.),
            (0., 640., 1., 1.),
            (0., 0., 0., 1.),
            (0., 0., 1., 0.),
        ] {
            assert!(p.map_box(b.0, b.1, b.2, b.3).is_err());
        }
    }
    #[test]
    fn refuses_positive_boxes_that_collapse_after_f32_mapping() {
        let p = InputScale::plan(1280, 2077).unwrap();
        assert!(p.map_box(383.0, 0.0, 1.0e-10, 1.0).is_err());
        assert!(p.map_box(0.0, 639.0, 1.0, 1.0e-10).is_err());
    }
    #[test]
    fn explicit_gate_keeps_defaults_and_off_spellings() {
        assert!(!gate(None));
        for v in ["", "0", "false", "OFF", "No"] {
            assert!(!gate(Some(v)));
        }
        assert!(gate(Some("1")));
    }
}
