use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use audscan_core::{AudioEntry, Container, ExtractOptions, FoundAudio, Manifest, Rejected, ScanOptions, SourceInfo, extract_all, input, scan};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;

#[derive(Parser)]
#[command(name = "audscan", version, about = "Find and extract audio (WAV, Wwise WEM, FMOD FSB5, Ogg) inside binary files")]
struct Cli {
    /// Print machine-readable JSON on stdout instead of text
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Find audio and optionally write a manifest
    Scan {
        file: PathBuf,
        /// Write the manifest here
        #[arg(short, long, value_name = "MANIFEST")]
        output: Option<PathBuf>,
        /// Also list headers that look like audio but can't be used, and why
        #[arg(long)]
        show_rejected: bool,
        /// List every track of an FSB5 bank
        #[arg(long)]
        tracks: bool,
        #[command(flatten)]
        filters: ScanArgs,
    },
    /// Extract the audio listed in a manifest (or found by a fresh scan if no manifest is
    /// given), each as a file of its own: .wav, .wem, .fsb or .ogg
    Extract {
        file: PathBuf,
        /// Manifest from `audscan scan -o`
        #[arg(short, long)]
        manifest: Option<PathBuf>,
        /// Output directory
        #[arg(short = 'd', long = "dir", value_name = "DIR")]
        dir: PathBuf,
        /// Extract even if the input's size or CRC no longer matches the manifest
        #[arg(long)]
        force: bool,
        /// Filters for the fresh scan (ignored with --manifest)
        #[command(flatten)]
        filters: ScanArgs,
    },
}

#[derive(Args)]
struct ScanArgs {
    /// Formats to look for, comma separated [default: all: riff, fsb5, ogg]
    #[arg(long, value_delimiter = ',', default_values_t = Container::ALL.to_vec(), hide_default_value = true)]
    formats: Vec<Container>,
}

impl ScanArgs {
    fn options(&self) -> ScanOptions {
        let mut formats = self.formats.clone();
        formats.sort_unstable();
        formats.dedup();
        ScanOptions { formats }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Scan { file, output, show_rejected, tracks, filters } => {
            let data = input::open(&file)?;
            let opts = filters.options();
            let report = scan(&data, &opts);
            let manifest = Manifest::new(SourceInfo::describe(&file, &data), opts, &report.audio);
            if let Some(path) = &output {
                manifest.save(path)?;
            }
            if cli.json {
                print_json(&ScanJson { audio: &manifest.audio, rejected: &report.rejected })?;
            } else {
                print_audio(&report.audio, tracks);
                println!("{} audio file(s) found", report.audio.len());
                if !report.rejected.is_empty() {
                    if show_rejected {
                        print_rejected(&report.rejected);
                    } else {
                        println!("{} header(s) look like audio but can't be used (--show-rejected lists them)", report.rejected.len());
                    }
                }
                if let Some(path) = &output {
                    println!("manifest written to {}", path.display());
                }
            }
        }
        Command::Extract { file, manifest, dir, force, filters } => {
            let data = input::open(&file)?;
            let manifest = match &manifest {
                Some(path) => Manifest::load(path)?,
                None => {
                    let opts = filters.options();
                    let found = scan(&data, &opts).audio;
                    Manifest::new(SourceInfo::describe(&file, &data), opts, &found)
                }
            };
            if manifest.audio.is_empty() {
                bail!("no audio to extract");
            }
            let files = extract_all(&data, &manifest, &dir, &ExtractOptions { verify_source: !force })?;
            if cli.json {
                let rows: Vec<_> = files
                    .iter()
                    .map(|f| ExtractJson { id: f.id, offset: f.offset, path: f.path.display().to_string(), size: f.size })
                    .collect();
                print_json(&rows)?;
            } else {
                println!("{} audio file(s) extracted to {}", files.len(), dir.display());
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct ScanJson<'a> {
    audio: &'a [AudioEntry],
    rejected: &'a [Rejected],
}

#[derive(Serialize)]
struct ExtractJson {
    id: u32,
    offset: u64,
    path: String,
    size: u64,
}

fn print_json<T: Serialize + ?Sized>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// `1:05.250`, or `-` when unknown.
fn length(seconds: Option<f64>) -> String {
    match seconds {
        Some(s) => {
            let ms = (s * 1000.0).round() as u64;
            format!("{}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
        }
        None => "-".into(),
    }
}

fn print_audio(audio: &[FoundAudio], tracks: bool) {
    if audio.is_empty() {
        return;
    }
    println!(
        "{:>12}  {:>10}  {:<6}  {:<18}  {:>2}  {:>6}  {:>10}  NAME",
        "OFFSET", "SIZE", "FORMAT", "CODEC", "CH", "RATE", "LENGTH"
    );
    for a in audio {
        let i = &a.info;
        let name = match i.tracks.as_slice() {
            [] => String::new(),
            [one] => one.name.clone().unwrap_or_default(),
            many => {
                let named: Vec<_> = many.iter().filter_map(|t| t.name.as_deref()).take(3).collect();
                let more = if many.len() > named.len() && !named.is_empty() { ", ..." } else { "" };
                format!("{} tracks{}{}{more}", many.len(), if named.is_empty() { "" } else { ": " }, named.join(", "))
            }
        };
        println!(
            "{:>#12x}  {:>10}  {:<6}  {:<18}  {:>2}  {:>6}  {:>10}  {name}",
            a.offset,
            i.size,
            a.label(),
            i.codec,
            i.channels,
            i.sample_rate,
            length(i.duration())
        );
        if let Some(note) = &i.note {
            println!("{:>12}  note: {note}", "");
        }
        if tracks && i.tracks.len() > 1 {
            for (n, t) in i.tracks.iter().enumerate() {
                let seconds = (t.sample_rate > 0).then(|| t.samples as f64 / f64::from(t.sample_rate));
                println!(
                    "{:>12}  {:>10}  {:<6}  {:<18}  {:>2}  {:>6}  {:>10}  {}",
                    format!("#{n}"),
                    t.size,
                    "",
                    "",
                    t.channels,
                    t.sample_rate,
                    length(seconds),
                    t.name.as_deref().unwrap_or("")
                );
            }
        }
    }
}

fn print_rejected(rejected: &[Rejected]) {
    println!("Not usable:");
    for r in rejected {
        println!("{:>#12x}  {:<6}  {}", r.offset, r.container, r.reason);
    }
}
