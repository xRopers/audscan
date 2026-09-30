use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use audscan_core::{
    AudioEntry, Container, ConvertError, ExtractOptions, FoundAudio, Manifest, Rejected, ScanOptions, SourceInfo, convert_wem,
    extract_all, input, scan,
};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;

#[derive(Parser)]
#[command(name = "audscan", version, about = "Find and extract audio (WAV, Wwise WEM/BNK/PCK, FMOD FSB4/FSB5, Ogg) inside binary files")]
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
        /// List every track of an FMOD bank, and every file in a Wwise bank or package
        #[arg(long)]
        tracks: bool,
        #[command(flatten)]
        filters: ScanArgs,
    },
    /// Extract the audio listed in a manifest (or found by a fresh scan if no manifest is
    /// given), each as a file of its own: .wav, .wem, .fsb, .ogg, .bnk or .pck
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
        /// Also split banks and packages into a folder named after each: Wwise WEMs (and a
        /// package's SoundBanks) as <ID>.wem / <ID>.bnk, localized ones in a subfolder
        /// per language; FSB4/FSB5 tracks as <name>.wav (PCM) or one-track <name>.fsb
        #[arg(long)]
        split: bool,
        /// Also convert every WEM written (extracted or split out) to a file any player
        /// opens: Ogg for Vorbis and Opus, WAV for PCM and IMA ADPCM
        #[arg(long)]
        convert: bool,
        /// Filters for the fresh scan (ignored with --manifest)
        #[command(flatten)]
        filters: ScanArgs,
    },
    /// Convert WEM files to Ogg (Wwise Vorbis and Opus, rewrapped, not re-encoded) or WAV
    /// (PCM, IMA ADPCM), written next to each or into --dir
    Convert {
        /// .wem files
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Output directory [default: next to each input]
        #[arg(short = 'd', long = "dir", value_name = "DIR")]
        dir: Option<PathBuf>,
    },
}

#[derive(Args)]
struct ScanArgs {
    /// Formats to look for, comma separated: riff, fsb4, fsb5, ogg, bnk, pck, or the groups
    /// fmod (fsb4, fsb5) and wwise (riff, bnk, pck) [default: all]
    #[arg(long, value_delimiter = ',')]
    formats: Vec<FormatGroup>,
}

/// One `--formats` entry: a format or a group of them.
#[derive(Clone)]
struct FormatGroup(Vec<Container>);

impl std::str::FromStr for FormatGroup {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Container::parse_group(s).map(FormatGroup)
    }
}

impl ScanArgs {
    fn options(&self) -> ScanOptions {
        let mut formats: Vec<Container> = self.formats.iter().flat_map(|g| g.0.iter().copied()).collect();
        if formats.is_empty() {
            formats = Container::ALL.to_vec();
        }
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
        Command::Convert { files, dir } => {
            use rayon::prelude::*;
            let results: Vec<_> = files
                .par_iter()
                .map(|input| {
                    let result = (|| -> Result<(PathBuf, String, Option<String>)> {
                        let data = std::fs::read(input)?;
                        let c = convert_wem(&data)?;
                        let name = input.with_extension(c.extension);
                        let out = match &dir {
                            Some(d) => d.join(name.file_name().unwrap()),
                            None => name,
                        };
                        if same_file(&out, input) {
                            bail!("the output would overwrite the input");
                        }
                        if let Some(d) = &dir {
                            std::fs::create_dir_all(d)?;
                        }
                        std::fs::write(&out, &c.bytes)?;
                        Ok((out, c.codec, c.note))
                    })();
                    (input, result)
                })
                .collect();
            let failed = results.iter().filter(|(_, r)| r.is_err()).count();
            if cli.json {
                let rows: Vec<_> = results
                    .iter()
                    .map(|(input, r)| match r {
                        Ok((out, codec, note)) => serde_json::json!({
                            "input": input.display().to_string(),
                            "output": out.display().to_string(),
                            "codec": codec,
                            "note": note,
                        }),
                        Err(e) => serde_json::json!({ "input": input.display().to_string(), "error": format!("{e:#}") }),
                    })
                    .collect();
                print_json(&rows)?;
            } else {
                for (input, r) in &results {
                    match r {
                        Ok((out, codec, note)) => {
                            println!("{} -> {} ({codec})", input.display(), out.display());
                            if let Some(note) = note {
                                println!("    note: {note}");
                            }
                        }
                        Err(e) => println!("{}: {e:#}", input.display()),
                    }
                }
                println!("{} converted, {failed} not", results.len() - failed);
            }
            if failed > 0 {
                bail!("{failed} file(s) couldn't be converted");
            }
        }
        Command::Extract { file, manifest, dir, force, split, convert, filters } => {
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
            let files = extract_all(&data, &manifest, &dir, &ExtractOptions { verify_source: !force, split, convert })?;
            if cli.json {
                let rows: Vec<_> = files
                    .iter()
                    .map(|f| ExtractJson {
                        id: f.id,
                        offset: f.offset,
                        path: f.path.display().to_string(),
                        size: f.size,
                        split: f.split.iter().map(|p| p.display().to_string()).collect(),
                        converted: f.converted.iter().map(|p| p.display().to_string()).collect(),
                        not_converted: f.not_converted.iter().map(|(p, e)| (p.display().to_string(), e.to_string())).collect(),
                        convert_notes: f.convert_notes.iter().map(|(p, n)| (p.display().to_string(), n.clone())).collect(),
                    })
                    .collect();
                print_json(&rows)?;
            } else {
                println!("{} audio file(s) extracted to {}", files.len(), dir.display());
                let split_out: usize = files.iter().map(|f| f.split.len()).sum();
                if split {
                    println!("{split_out} file(s) split out of banks and packages");
                }
                if convert {
                    print_conversions(files.iter().flat_map(|f| &f.not_converted), files.iter().map(|f| f.converted.len()).sum());
                    for (path, note) in files.iter().flat_map(|f| &f.convert_notes) {
                        println!("{}: {note}", path.display());
                    }
                }
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    split: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    converted: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    not_converted: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    convert_notes: Vec<(String, String)>,
}

/// Summarise conversions: how many were written, and why the rest weren't, grouped (a bank
/// can hold hundreds of prefetch fragments).
fn print_conversions<'a>(failed: impl Iterator<Item = &'a (PathBuf, ConvertError)>, converted: usize) {
    let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
    for (_, e) in failed {
        let reason = match e {
            ConvertError::Partial { .. } => "only part of the WEM is here (prefetch media)".to_string(),
            e => e.to_string(),
        };
        *reasons.entry(reason).or_default() += 1;
    }
    println!("{converted} WEM(s) converted to Ogg/WAV");
    for (reason, n) in reasons {
        println!("{n:>8} not converted: {reason}");
    }
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
            [one] => one.display_name(),
            many => {
                let named: Vec<_> = many.iter().map(|t| t.display_name()).filter(|n| !n.is_empty()).take(3).collect();
                let more = if many.len() > named.len() && !named.is_empty() { ", ..." } else { "" };
                let banks = many.iter().filter(|t| t.extension.as_deref() == Some("bnk")).count();
                let what = if banks > 0 { format!("{} files ({banks} bnk, {} wem)", many.len(), many.len() - banks) } else { format!("{} tracks", many.len()) };
                format!("{what}{}{}{more}", if named.is_empty() { "" } else { ": " }, named.join(", "))
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
        if tracks && (i.tracks.len() > 1 || i.tracks.iter().any(|t| t.extension.is_some())) {
            for (n, t) in i.tracks.iter().enumerate() {
                let seconds = t.samples.filter(|_| t.sample_rate > 0).map(|s| s as f64 / f64::from(t.sample_rate));
                let name = match &t.language {
                    Some(lang) => format!("{} [{lang}]", t.display_name()),
                    None => t.display_name(),
                };
                println!(
                    "{:>12}  {:>10}  {:<6}  {:<18}  {:>2}  {:>6}  {:>10}  {name}",
                    format!("#{n}"),
                    t.size,
                    t.extension.as_deref().unwrap_or(""),
                    t.codec.as_deref().unwrap_or(""),
                    t.channels,
                    t.sample_rate,
                    length(seconds),
                );
                if let Some(note) = &t.note {
                    println!("{:>12}  note: {note}", "");
                }
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

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
