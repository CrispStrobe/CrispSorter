#!/usr/bin/env python3
"""Exercise the actual layout loop with model/image boundaries stubbed, using rustc only.

Run: python3 src-tauri/tests/layout_table_regression.py
The copied production functions are not rewritten; fake regions, crops, and OCR
outputs make the layout routing deterministic without downloading models.
"""
from pathlib import Path
import os
import subprocess
import tempfile

source = (Path(__file__).resolve().parents[1] / 'src/extractors/mod.rs').read_text()
start = source.index('fn ocr_with_layout(')
end = source.index('/// OCR a single image through the smart pipeline', start)
production = source[start:end]

harness = r'''
#![allow(dead_code)]
use std::path::Path;
use std::cell::{Cell, RefCell};
extern crate self as anyhow;
#[macro_export] macro_rules! anyhow { ($($x:tt)*) => { format!($($x)*) }; }
#[macro_export] macro_rules! bail { ($($x:tt)*) => { return Err(format!($($x)*)) }; }
type Result<T> = std::result::Result<T, String>;
trait Context<T> { fn context(self, s: &str) -> Result<T>; fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T>; }
impl<T, E: std::fmt::Display> Context<T> for std::result::Result<T,E> {
 fn context(self, s: &str) -> Result<T> { self.map_err(|e| format!("{s}: {e}")) }
 fn with_context<F: FnOnce() -> String>(self, f: F) -> Result<T> { self.map_err(|e| format!("{}: {e}",f())) }
}
thread_local! { static Y: Cell<u32> = const { Cell::new(0) }; static MODE: Cell<u32> = const { Cell::new(0) }; static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) }; }
#[derive(Default)] struct ExtractedDocument { full_text:String, headings:Vec<String>, ext:String, language:Option<String>, translated_text:Option<String>, translated_to_lang:Option<String>, audio:Option<()>, image_exif:Option<()>, source_url:Option<String>, tags:Vec<String>, audio_pcm:Option<()> }
struct Config { layout_engine:String, layout_model:Option<String>, layout_threshold:f32, drop_headers_footers:bool, rec_model:Option<String> }
struct ExtractOptions { ocr_pipeline:Config }
mod layout {
 #[derive(Debug, PartialEq)] pub enum RegionKind { Text, Table, Formula, Header, Footer, Figure, Other(String) }
 impl RegionKind { pub fn is_formula(&self)->bool { *self == Self::Formula } }
 pub struct LayoutRegion { pub x1:f32, pub y1:f32, pub x2:f32, pub y2:f32, pub kind:RegionKind }
 pub struct Detector;
 impl Detector { pub fn detect(&self, _: &std::path::Path, _: f32)->crate::Result<Vec<LayoutRegion>> { unreachable!() } }
}
fn cached_layout_detector(_: &str)->Result<&'static std::sync::Mutex<layout::Detector>> { unreachable!() }
mod ocr_crispembed {
 use super::*;
 pub fn cc_detect_regions(_: &Path)->Vec<layout::LayoutRegion> {
  [layout::RegionKind::Text,layout::RegionKind::Table,layout::RegionKind::Text].into_iter().enumerate().rev().map(|(i,kind)|layout::LayoutRegion{x1:0.,y1:(i*10) as f32,x2:if i==1 && MODE.with(|m|m.get())==7 {0.} else {10.},y2:(i*10+10) as f32,kind}).collect()
 }
 pub fn table_to_html(_: &Path, model:Option<&str>)->Result<String> {
  CALLS.with(|c| c.borrow_mut().push(format!("table:{}:{}",Y.with(|v|v.get()),model.unwrap_or("default"))));
  match MODE.with(|m|m.get()) {
   0 => Ok("<table><tr><th>Revenue</th><th>42</th></tr></table>".into()),
   1 | 4 | 6 => Err("parser unavailable".into()),
   2 => Ok("<table><tr><td></td></tr></table>".into()),
   3 => Ok("<table><tr><td>broken</tr></table>".into()),
   _ => Ok("<table>\n<tr><th>R&amp;D</th><th>&lt;cost&gt;</th></tr>\n<tr><td>50</td><td>&amp;lt;</td></tr>\n</table>".into())
  }
 }
}
mod image {
 use super::*;
 pub struct Image; pub struct Crop(pub u32);
 pub fn open(_: &Path)->Result<Image> { Ok(Image) }
 impl Image { pub fn to_rgb8(self)->Self {self} pub fn width(&self)->u32{100} pub fn height(&self)->u32{100} }
 impl Crop { pub fn to_image(self)->Self{self} pub fn as_raw(&self)->&[u8]{&[]} pub fn width(&self)->u32{10} pub fn height(&self)->u32{10} }
 pub mod imageops { use super::*; pub fn crop_imm(_: &Image,_:u32,y:u32,_:u32,_:u32)->Crop{Crop(y)} }
 pub enum DynamicImage { ImageRgb8(Crop) }
 impl DynamicImage { pub fn save(self,_:&Path)->Result<()> { let Self::ImageRgb8(c) = self; Y.with(|y|y.set(c.0)); Ok(()) } }
}
mod tempfile { use super::*; pub struct Builder; pub struct Temp;
 impl Builder { pub fn new()->Self{Self} pub fn prefix(self,_:&str)->Self{self} pub fn suffix(self,_:&str)->Self{self} pub fn tempfile(self)->Result<Temp>{Ok(Temp)} }
 impl Temp { pub fn path(&self)->&Path{Path::new("crop.png")} }
}
mod math_ocr { use super::*; pub fn is_math_ocr_available()->bool{false} pub fn recognize_formula_from_pixels(_: &[u8],_:i32,_:i32)->Result<Option<String>>{Ok(None)} }
fn ocr_one_image(_: &Path, _: &ExtractOptions)->Result<ExtractedDocument> {
 let y=Y.with(|v|v.get()); CALLS.with(|c|c.borrow_mut().push(format!("ocr:{y}")));
 if y == 10 && MODE.with(|m|m.get()) == 4 { return Err("crop OCR failed".into()); }
 if y == 10 && MODE.with(|m|m.get()) == 6 { return Ok(ExtractedDocument::default()); }
 Ok(ExtractedDocument{full_text:match y {0=>"before",10=>"fallback table text",20=>"after",_=>panic!("bad crop")}.into(),..Default::default()})
}
fn options()->ExtractOptions {ExtractOptions{ocr_pipeline:Config{layout_engine:"cc".into(),layout_model:None,layout_threshold:0.25,drop_headers_footers:false,rec_model:Some("custom-rec".into())}}}
'''

tests = r'''
#[test] fn invalid_table_crop_requests_whole_page_fallback() {
 MODE.with(|m|m.set(7)); CALLS.with(|c|c.borrow_mut().clear());
 assert!(ocr_with_layout(Path::new("page.png"),&options()).is_err());
}
#[test] fn table_parser_failure_falls_back_in_reading_order() {
 MODE.with(|m|m.set(1)); CALLS.with(|c|c.borrow_mut().clear());
 let doc=ocr_with_layout(Path::new("page.png"),&options()).unwrap();
 assert_eq!(doc.full_text,"before\n\nfallback table text\n\nafter");
 assert_eq!(CALLS.with(|c|c.borrow().clone()),vec!["ocr:0","table:10:custom-rec","ocr:10","ocr:20"]);
}
#[test] fn empty_table_parser_output_falls_back() {
 MODE.with(|m|m.set(2)); CALLS.with(|c|c.borrow_mut().clear());
 assert_eq!(ocr_with_layout(Path::new("page.png"),&options()).unwrap().full_text,"before\n\nfallback table text\n\nafter");
}
#[test] fn malformed_table_parser_output_falls_back() {
 MODE.with(|m|m.set(3)); CALLS.with(|c|c.borrow_mut().clear());
 assert_eq!(ocr_with_layout(Path::new("page.png"),&options()).unwrap().full_text,"before\n\nfallback table text\n\nafter");
}
#[test] fn table_cells_keep_rows_columns_and_decode_entities_once() {
 MODE.with(|m|m.set(5)); CALLS.with(|c|c.borrow_mut().clear());
 assert_eq!(ocr_with_layout(Path::new("page.png"),&options()).unwrap().full_text,"before\n\nR&D\t<cost>\n50\t&lt;\n\nafter");
}
#[test] fn empty_table_ocr_requests_whole_page_fallback() {
 MODE.with(|m|m.set(6)); CALLS.with(|c|c.borrow_mut().clear());
 assert!(ocr_with_layout(Path::new("page.png"),&options()).is_err());
}
#[test] fn failed_table_ocr_requests_whole_page_fallback() {
 MODE.with(|m|m.set(4)); CALLS.with(|c|c.borrow_mut().clear());
 assert!(ocr_with_layout(Path::new("page.png"),&options()).is_err());
}
#[test] fn table_success_retains_indexable_words_in_reading_order() {
 MODE.with(|m|m.set(0)); CALLS.with(|c|c.borrow_mut().clear());
 let doc=ocr_with_layout(Path::new("page.png"),&options()).unwrap();
 assert_eq!(doc.full_text,"before\n\nRevenue\t42\n\nafter");
 assert_eq!(CALLS.with(|c|c.borrow().clone()),vec!["ocr:0","table:10:custom-rec","ocr:20"]);
}
'''

with tempfile.TemporaryDirectory(prefix='layout-table-', dir=os.environ.get('TMPDIR')) as d:
    rust = Path(d) / 'regression.rs'
    binary = Path(d) / 'regression'
    rust.write_text(harness + '\n' + production + '\n' + tests)
    subprocess.run(['rustc', '--edition=2021', '--test', str(rust), '-o', str(binary)], check=True)
    subprocess.run([str(binary), '--nocapture'], check=True)
