//! Focused executable of the production CLI module, using the real native library.
//! Avoids building the unrelated desktop, indexing and inference stacks.
#![allow(dead_code)]
use clap::{Parser, Subcommand};
#[path = "../../src-tauri/src/extractors/image_input.rs"]
pub mod image_input;
#[path = "../../src-tauri/src/image_pdf.rs"]
mod image_pdf;
#[path = "../../src-tauri/src/ocr_atomic_output.rs"]
mod ocr_atomic_output;
#[path = "../../src-tauri/src/operation_log.rs"]
mod operation_log;
#[path = "../../src-tauri/src/extractors/page_source.rs"]
pub mod page_source;
mod extractors {
    #[allow(unused_imports)]
    pub use crate::{image_input, page_source};
}
fn app_log(level: &str, message: String) {
    eprintln!("{level}: {message}");
}
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Images {
        #[command(subcommand)]
        command: Images,
    },
}
#[derive(Subcommand)]
enum Images {
    Pdf(image_pdf::ImagePdfArgs),
}
fn main() {
    let Cli {
        command: Command::Images {
            command: Images::Pdf(args),
        },
    } = Cli::parse();
    match image_pdf::run(&args, |done, total| {
        eprintln!("Preparing images: {done}/{total}")
    }) {
        Ok(report) => println!("{}", serde_json::to_string(&report).unwrap()),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
