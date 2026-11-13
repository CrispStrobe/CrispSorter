This isolated workspace tests the production SVTR geometry helper and the pinned
`fast_image_resize` backend without building CrispSorter, usls, ORT or any model.

Run `cargo test --manifest-path tests/paddle-resize/Cargo.toml --locked --jobs 1`.
Its lock is separate from the application lock. Set `CARGO_TARGET_DIR` and
`CARGO_HOME` to volume-backed storage when root disk space is constrained.

The image regression reproduces usls 0.2.0-alpha.3's unbounded height-fit crop
error with an over-wide line. The workaround resizes the complete line into the
model canvas, retaining both image endpoints. It uses the same bilinear
convolution algorithm as usls's default. Narrow and exact-boundary geometry stays
unchanged. This proves preprocessing geometry/pixels, not OCR recognition quality.
The existing ignored live Paddle test remains the end-to-end recognition check.

Geometry alone also runs without Cargo: compile
`tests/rust/paddle_resize_geometry.rs` with `rustc --edition=2021 --test`.
