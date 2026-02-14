#![forbid(unsafe_code)]

use std::{fs, path::PathBuf};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rustdown", about = "Preview markdown from the CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render markdown to a simple plain-text preview and print to stdout.
    Preview {
        /// Path to a markdown file. Use `-` to read from stdin.
        path: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Preview { path } => {
            let source = if path.as_os_str() == "-" {
                use std::io::Read as _;

                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .context("failed to read markdown from stdin")?;
                buf
            } else {
                let meta = fs::metadata(&path)
                    .with_context(|| format!("failed to stat {}", path.display()))?;
                if meta.len() > rustdown_core::MAX_FILE_BYTES {
                    bail!(
                        "refusing to read {} ({} MiB) — too large",
                        path.display(),
                        meta.len() / (1024 * 1024)
                    );
                }
                fs::read_to_string(&path)
                    .with_context(|| format!("failed to read markdown from {}", path.display()))?
            };

            let rendered = rustdown_core::markdown::plain_text(&source);
            print!("{rendered}");
        }
    }

    Ok(())
}
