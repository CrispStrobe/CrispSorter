//! P17.1 — Layout-aware document extraction via CrispEmbed's RT-DETRv2.
//!
//! Detects 17 region types (text, title, table, figure, formula, header,
//! footer, caption, …) on a page image.  Used as a pre-pass before OCR:
//! text/title/caption regions route to OCR, formula regions route to math
//! OCR, figure/table regions are skipped or handled specially.
//!
//! Gated behind `--features crispembed`.  When the feature is off, the
//! public stubs return `Err` so call sites degrade gracefully.

use anyhow::{Context, Result};
use std::path::Path;

// ── Region labels emitted by RT-DETRv2 (17 classes) ─────────────────

/// Semantic region type detected by the layout model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionKind {
    Text,
    Title,
    Figure,
    FigureCaption,
    Table,
    TableCaption,
    Header,
    Footer,
    Reference,
    Formula,
    /// Anything the model returns that isn't in our known set.
    Other(String),
}

impl RegionKind {
    /// Parse the label name string returned by CrispEmbed.
    pub fn from_label(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "text" | "plain text" => Self::Text,
            "title" | "section-header" => Self::Title,
            "figure" | "image" => Self::Figure,
            "figure_caption" | "figure caption" | "caption" => Self::FigureCaption,
            "table" => Self::Table,
            "table_caption" | "table caption" => Self::TableCaption,
            "header" | "page-header" => Self::Header,
            "footer" | "page-footer" | "footnote" => Self::Footer,
            "reference" | "list-item" => Self::Reference,
            "formula" | "equation" | "isolate_formula" | "inline_formula" => Self::Formula,
            other => Self::Other(other.to_string()),
        }
    }

    /// Should this region be routed to text OCR?
    pub fn is_text_bearing(&self) -> bool {
        matches!(
            self,
            Self::Text
                | Self::Title
                | Self::FigureCaption
                | Self::TableCaption
                | Self::Header
                | Self::Footer
                | Self::Reference
        )
    }

    /// Should this region be routed to math OCR?
    pub fn is_formula(&self) -> bool {
        matches!(self, Self::Formula)
    }
}

/// A detected region on a page image.
#[derive(Debug, Clone)]
pub struct LayoutRegion {
    /// Bounding box in pixel coordinates (x1, y1, x2, y2).
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    /// Confidence score [0, 1].
    pub score: f32,
    /// Semantic region type.
    pub kind: RegionKind,
    /// Raw label name from the model.
    pub label_name: String,
}

/// Lazy-loaded layout detector.  Holds an optional `CrispLayout` instance
/// that is created on first use and reused across calls.
#[cfg(feature = "crispembed")]
pub struct LayoutDetector {
    inner: crispembed::CrispLayout,
}

#[cfg(feature = "crispembed")]
impl LayoutDetector {
    /// Load the RT-DETRv2 layout detection model from a GGUF file.
    ///
    /// `model_path` may be:
    /// - An absolute path to a `.gguf` file on disk.
    /// - A registry name that CrispEmbed can auto-resolve.
    ///
    /// `n_threads`: pass 0 for automatic.
    pub fn load(model_path: &str, n_threads: i32) -> Result<Self> {
        let resolved = crispembed::CrispEmbed::resolve_model(model_path, Some(true))
            .unwrap_or_else(|_| model_path.to_string());
        let inner = crispembed::CrispLayout::new(&resolved, n_threads)
            .map_err(|e| anyhow::anyhow!("layout model load failed: {e}"))?;
        Ok(Self { inner })
    }

    /// Detect layout regions on a page image.
    ///
    /// - `image_path`: path to the page image (PNG/JPG/TIFF).
    /// - `threshold`: confidence threshold (0.0–1.0); 0.25 is a good default.
    ///
    /// Returns regions sorted top-to-bottom, left-to-right (reading order).
    pub fn detect(&self, image_path: &Path, threshold: f32) -> Result<Vec<LayoutRegion>> {
        let path_str = image_path
            .to_str()
            .context("image path is not valid UTF-8")?;
        let raw = self.inner.detect(path_str, threshold);
        let mut regions: Vec<LayoutRegion> = raw
            .into_iter()
            .map(|r| LayoutRegion {
                x1: r.x1,
                y1: r.y1,
                x2: r.x2,
                y2: r.y2,
                score: r.score,
                kind: RegionKind::from_label(&r.label_name),
                label_name: r.label_name,
            })
            .collect();
        // Sort in reading order: top-to-bottom, then left-to-right.
        regions.sort_by(|a, b| {
            a.y1.partial_cmp(&b.y1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    a.x1.partial_cmp(&b.x1)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        Ok(regions)
    }

    /// Convenience: return only text-bearing regions (for OCR routing).
    pub fn text_regions(&self, image_path: &Path, threshold: f32) -> Result<Vec<LayoutRegion>> {
        Ok(self
            .detect(image_path, threshold)?
            .into_iter()
            .filter(|r| r.kind.is_text_bearing())
            .collect())
    }

    /// Convenience: return only formula regions (for math OCR routing).
    pub fn formula_regions(&self, image_path: &Path, threshold: f32) -> Result<Vec<LayoutRegion>> {
        Ok(self
            .detect(image_path, threshold)?
            .into_iter()
            .filter(|r| r.kind.is_formula())
            .collect())
    }
}

// ── Stub when crispembed is not compiled ────────────────────────────

#[cfg(not(feature = "crispembed"))]
#[derive(Debug)]
pub struct LayoutDetector;

#[cfg(not(feature = "crispembed"))]
impl LayoutDetector {
    pub fn load(_model_path: &str, _n_threads: i32) -> Result<Self> {
        Err(anyhow::anyhow!(
            "layout detection requires --features crispembed"
        ))
    }

    pub fn detect(&self, _image_path: &Path, _threshold: f32) -> Result<Vec<LayoutRegion>> {
        Err(anyhow::anyhow!(
            "layout detection requires --features crispembed"
        ))
    }

    pub fn text_regions(
        &self,
        _image_path: &Path,
        _threshold: f32,
    ) -> Result<Vec<LayoutRegion>> {
        Err(anyhow::anyhow!(
            "layout detection requires --features crispembed"
        ))
    }

    pub fn formula_regions(
        &self,
        _image_path: &Path,
        _threshold: f32,
    ) -> Result<Vec<LayoutRegion>> {
        Err(anyhow::anyhow!(
            "layout detection requires --features crispembed"
        ))
    }
}

/// Check if layout detection is available at runtime.
pub fn is_layout_available() -> bool {
    cfg!(feature = "crispembed")
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_kind_parses_known_labels() {
        assert_eq!(RegionKind::from_label("text"), RegionKind::Text);
        assert_eq!(RegionKind::from_label("Title"), RegionKind::Title);
        assert_eq!(RegionKind::from_label("FIGURE"), RegionKind::Figure);
        assert_eq!(RegionKind::from_label("formula"), RegionKind::Formula);
        assert_eq!(RegionKind::from_label("table"), RegionKind::Table);
        assert_eq!(
            RegionKind::from_label("figure_caption"),
            RegionKind::FigureCaption
        );
        assert_eq!(
            RegionKind::from_label("table caption"),
            RegionKind::TableCaption
        );
        assert_eq!(RegionKind::from_label("header"), RegionKind::Header);
        assert_eq!(RegionKind::from_label("footer"), RegionKind::Footer);
        assert_eq!(RegionKind::from_label("page-footer"), RegionKind::Footer);
        assert_eq!(RegionKind::from_label("reference"), RegionKind::Reference);
        assert_eq!(
            RegionKind::from_label("isolate_formula"),
            RegionKind::Formula
        );
        assert_eq!(
            RegionKind::from_label("inline_formula"),
            RegionKind::Formula
        );
    }

    #[test]
    fn region_kind_unknown_label_is_other() {
        match RegionKind::from_label("watermark") {
            RegionKind::Other(s) => assert_eq!(s, "watermark"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn text_bearing_regions() {
        assert!(RegionKind::Text.is_text_bearing());
        assert!(RegionKind::Title.is_text_bearing());
        assert!(RegionKind::FigureCaption.is_text_bearing());
        assert!(RegionKind::Reference.is_text_bearing());
        assert!(!RegionKind::Figure.is_text_bearing());
        assert!(!RegionKind::Table.is_text_bearing());
        assert!(!RegionKind::Formula.is_text_bearing());
    }

    #[test]
    fn formula_regions_identified() {
        assert!(RegionKind::Formula.is_formula());
        assert!(!RegionKind::Text.is_formula());
        assert!(!RegionKind::Table.is_formula());
    }

    #[test]
    fn is_layout_available_matches_feature() {
        // When tests run without the feature, this should be false.
        // When tests run with --features crispembed, this should be true.
        // Either way the test passes — it just documents the runtime value.
        let available = is_layout_available();
        if cfg!(feature = "crispembed") {
            assert!(available);
        } else {
            assert!(!available);
        }
    }

    #[test]
    fn layout_region_sorting() {
        // Verify reading-order sort: top-to-bottom, then left-to-right.
        let mut regions = vec![
            LayoutRegion {
                x1: 100.0, y1: 200.0, x2: 300.0, y2: 400.0,
                score: 0.9, kind: RegionKind::Text,
                label_name: "text".into(),
            },
            LayoutRegion {
                x1: 50.0, y1: 50.0, x2: 200.0, y2: 100.0,
                score: 0.8, kind: RegionKind::Title,
                label_name: "title".into(),
            },
            LayoutRegion {
                x1: 250.0, y1: 50.0, x2: 400.0, y2: 100.0,
                score: 0.7, kind: RegionKind::Formula,
                label_name: "formula".into(),
            },
        ];
        regions.sort_by(|a, b| {
            a.y1.partial_cmp(&b.y1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    a.x1.partial_cmp(&b.x1)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        assert_eq!(regions[0].kind, RegionKind::Title);   // y=50, x=50
        assert_eq!(regions[1].kind, RegionKind::Formula);  // y=50, x=250
        assert_eq!(regions[2].kind, RegionKind::Text);     // y=200
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_load_errors_without_feature() {
        let err = LayoutDetector::load("any-model", 0);
        assert!(err.is_err());
        assert!(
            err.unwrap_err()
                .to_string()
                .contains("requires --features crispembed")
        );
    }

    // ── Live test (requires model on disk) ──────────────────────────

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // Run with: cargo test --features crispembed layout_detect_live -- --ignored
    fn layout_detect_live() {
        // Requires a layout model GGUF in the CrispEmbed cache.
        // Use `crispembed --list-models | grep layout` to find the name.
        let det = LayoutDetector::load("rt-detrv2-layout", 0)
            .expect("layout model should load");
        // Create a simple test image (white 200×200 PNG).
        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("test.png");
        let img = image::RgbImage::new(200, 200);
        img.save(&img_path).unwrap();
        let regions = det.detect(&img_path, 0.25).unwrap();
        // A blank white image should produce zero or very few detections.
        // The important thing is that the pipeline doesn't crash.
        println!("detected {} regions on blank image", regions.len());
    }
}
