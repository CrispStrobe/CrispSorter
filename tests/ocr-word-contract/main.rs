//! Thin measurement adapter around the production system OCR implementation.
#![allow(dead_code)]
#[path = "../../src-tauri/src/ocr_word_evidence.rs"]
mod ocr_word_evidence;
use serde_json::json;
use std::{io::Write, path::Path};

fn main() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments == ["capabilities"] {
        println!("{}", serde_json::to_string(&ocr_word_evidence::capabilities()).map_err(|error| error.to_string())?);
        return Ok(());
    }
    if arguments.len() != 3 {
        return Err("Usage: ocr-word-contract IMAGE LANGUAGE OUTPUT.json, or capabilities".into());
    }
    let started = std::time::Instant::now();
    let result = ocr_word_evidence::recognize(Path::new(&arguments[0]), Some(&arguments[1]))?;
    let elapsed = started.elapsed().as_secs_f64();
    for region in &result.regions {
        ocr_word_evidence::validate_word_evidence(&region.word_evidence, &region.text, result.width, result.height)?;
    }
    let regions = result.regions.iter().map(|region| json!({"text":region.text,"x":region.x,"y":region.y,"w":region.w,"h":region.h,
        "word_evidence":region.word_evidence})).collect::<Vec<_>>();
    let payload = json!({"text":result.full_text,"width":result.width,"height":result.height,
        "regions":regions,"diagnostics":result.diagnostics,"warnings":result.warnings,"native_wall_seconds":elapsed});
    let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&arguments[2]).map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(&mut output, &payload).map_err(|error| error.to_string())?;
    output.write_all(b"\n").map_err(|error| error.to_string())?;
    output.sync_all().map_err(|error| error.to_string())?;
    Ok(())
}
