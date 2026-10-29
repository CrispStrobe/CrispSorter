//! Harness caller for the verbatim production functions in isolated_ocr.py.
use crispsorter_ocr_qualification::extractors::{ocr_crispembed, ocr_presets};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("models") {
        let mut models = Vec::new();
        for name in ["dbnet-det", "trocr-printed", "qwen2vl-3b"] {
            let path = crispembed::CrispEmbed::resolve_model(name, Some(true))?;
            let mut file = std::fs::File::open(path)?;
            let bytes = file.metadata()?.len();
            let mut hash = Sha256::new();
            let mut block = [0; 65536];
            loop {
                let count = file.read(&mut block)?;
                if count == 0 { break; }
                hash.update(&block[..count]);
            }
            models.push(serde_json::json!({"registry_name": name, "bytes": bytes, "sha256": format!("{:x}", hash.finalize())}));
        }
        std::fs::write(&args[2], serde_json::to_vec_pretty(&models)?)?;
        return Ok(());
    }
    if args.len() != 5 { return Err("Expected PRESET IMAGE MAX_TOKENS OUTPUT_JSON".into()); }
    let mut config = ocr_presets::apply(&Default::default(), &args[1])?;
    let cap: i32 = args[3].parse()?;
    for stage in &mut config.stages {
        if stage.vlm_max_tokens > 0 { stage.vlm_max_tokens = cap; }
    }
    let started = Instant::now();
    let regions = ocr_crispembed::ocr_regions_detailed(Path::new(&args[2]), &config)?;
    let text = regions.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("\n");
    let confidence_count: usize = regions.iter().map(|r| r.char_conf.len()).sum();
    let result = serde_json::json!({"text":text,"config":config,"elapsed_seconds":started.elapsed().as_secs_f64(),
        "region_count":regions.len(),"confidence_count":confidence_count,
        "generation_cap":"Pipeline binding does not expose the decoded token count; timeout and length ratio are recorded separately"});
    std::fs::write(&args[4], serde_json::to_vec_pretty(&result)?)?;
    Ok(())
}
