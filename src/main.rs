use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use parabloom::expand::{Options, expand_highlights};
use parabloom::layout::Params;

/// Expand partial PDF highlights to cover the full paragraph they belong to.
#[derive(Parser)]
#[command(name = "parabloom", version)]
struct Cli {
    /// Input PDF file path
    input: PathBuf,
    /// Output PDF file path (can be the same as input)
    output: PathBuf,
    /// X threshold for paragraph indentation detection
    #[arg(long, default_value_t = 80.0)]
    indent_x: f64,
    /// Vertical gap threshold for paragraph breaks
    #[arg(long, default_value_t = 12.0)]
    gap_threshold: f64,
    /// Y threshold below which lines are excluded (footnote zone)
    #[arg(long, default_value_t = 720.0)]
    footnote_y: f64,
    /// Change highlight color to blue on pages with images (the default)
    #[arg(long, default_value_t = true)]
    blue_on_image: bool,
    /// Keep original highlight colors even on pages with images
    #[arg(long)]
    no_blue_on_image: bool,
    /// Print paragraph groupings for debugging
    #[arg(long)]
    debug: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let opts = Options {
        params: Params {
            indent_x: cli.indent_x,
            gap_threshold: cli.gap_threshold,
            footnote_y: cli.footnote_y,
        },
        blue_on_image: cli.blue_on_image && !cli.no_blue_on_image,
        debug: cli.debug,
    };
    match expand_highlights(&cli.input, &cli.output, &opts) {
        Ok(stats) => {
            println!(
                "Done. Expanded {} highlights across {} pages.",
                stats.highlights_expanded, stats.pages_modified
            );
            if stats.highlights_blued > 0 {
                println!(
                    "  {} highlights recolored to blue (image pages).",
                    stats.highlights_blued
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}
