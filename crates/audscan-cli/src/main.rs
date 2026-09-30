use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, bail};
use audscan_core::{
    AudioEntry, AudioPlan, Container, ConvertError, ExtractOptions, FoundAudio, Manifest, Outcome, PackOptions, Placement,
    Rejected, ScanOptions, SourceInfo, convert_wem, extract_all, input, load_edits, pack, scan,
};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;

#[derive(Parser)]
#[command(name = "audscan", version, about = "Find, extract and put back audio (WAV, Wwise WEM/BNK/PCK, FMOD FSB4/FSB5, Ogg) inside binary files")]
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
    /// Put edited audio from an extract folder back into a copy of the input: a changed
    /// file (.wav, .wem, .bnk, .pck, .fsb, .ogg, replaced by one of the same kind) or a
    /// changed track split out of a bank or package (a .wem or .bnk; for FSB5 a one-track
    /// .fsb of the bank's codec, or a .wav for a PCM bank). Banks are rebuilt around new
    /// tracks. A file that shrinks is padded to keep its place; one that grows only fits
    /// at the end of the input
    Pack {
        file: PathBuf,
        /// Manifest from `audscan scan -o` [default: scan the input again]
        #[arg(short, long)]
        manifest: Option<PathBuf>,
        /// Folder written by `audscan extract` (with `--split` to edit tracks)
        #[arg(short = 'd', long = "dir", value_name = "DIR")]
        dir: PathBuf,
        /// Output file [default: <input stem>.packed.<ext> next to the input]
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Show what would change without writing anything
        #[arg(long)]
        dry_run: bool,
        /// Pack even if the input's size or CRC no longer matches the manifest
        #[arg(long)]
        force: bool,
        /// Filters for the fresh scan (ignored with --manifest): use the ones extract used
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
        Command::Pack { file, manifest, dir, output, dry_run, force, filters } => {
            let data = input::open(&file)?;
            let manifest = load_or_scan(&file, &data, manifest.as_deref(), &filters)?;
            let found = load_edits(&data, &manifest, &dir)?;
            let result = pack(&data, &manifest, &found.edits, &PackOptions { verify_source: !force })?;
            let output = output.unwrap_or_else(|| default_output(&file));
            let write = !dry_run && result.changed() > 0;
            if write {
                if same_file(&output, &file) {
                    bail!("the output would overwrite the input; choose another --output");
                }
                result.write_file(&data, &output)?;
            }
            if cli.json {
                let rows: Vec<_> = result.audio.iter().map(PackJson::from).collect();
                let fields: Vec<_> = result
                    .fields
                    .iter()
                    .map(|f| serde_json::json!({ "offset": f.offset, "big_endian": f.big_endian, "old": f.old, "new": f.new, "measures": f.measures }))
                    .collect();
                let written = write.then(|| output.display().to_string());
                print_json(&serde_json::json!({
                    "audio": rows,
                    "fields": fields,
                    "input_size": result.input_len,
                    "output_size": result.output_len,
                    "unedited_files": found.unchanged,
                    "output": written,
                }))?;
            } else {
                for plan in &result.audio {
                    print_plan(plan, &manifest);
                }
                for f in &result.fields {
                    let order = if f.big_endian { "BE" } else { "LE" };
                    println!("size field at {:#x} ({order}): {} -> {} (it measures {})", f.offset, f.old, f.new, f.measures);
                }
                if result.output_len != result.input_len {
                    println!("the output is {} bytes; the input was {}", result.output_len, result.input_len);
                }
                match (result.changed(), dry_run) {
                    (0, _) => println!("nothing to pack: no edited audio in {} ({} unedited file(s))", dir.display(), found.unchanged),
                    (n, true) => println!("dry run: {n} file(s) would change; nothing written"),
                    (n, false) => println!("{n} file(s) packed into {} (verified)", output.display()),
                }
            }
        }
        Command::Extract { file, manifest, dir, force, split, convert, filters } => {
            let data = input::open(&file)?;
            let manifest = load_or_scan(&file, &data, manifest.as_deref(), &filters)?;
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

/// The manifest at `path`, or one from scanning `data` now.
fn load_or_scan(file: &Path, data: &[u8], path: Option<&Path>, filters: &ScanArgs) -> Result<Manifest> {
    Ok(match path {
        Some(path) => Manifest::load(path)?,
        None => {
            let opts = filters.options();
            let found = scan(data, &opts).audio;
            Manifest::new(SourceInfo::describe(file, data), opts, &found)
        }
    })
}

/// `game.pak` -> `game.packed.pak` next to it.
fn default_output(input: &Path) -> PathBuf {
    let stem = input.file_stem().map_or_else(|| "output".into(), |s| s.to_string_lossy().into_owned());
    let name = match input.extension() {
        Some(ext) => format!("{stem}.packed.{}", ext.to_string_lossy()),
        None => format!("{stem}.packed"),
    };
    input.with_file_name(name)
}

fn print_plan(plan: &AudioPlan, manifest: &Manifest) {
    let entry = manifest.audio.iter().find(|a| a.id == plan.id);
    let label = entry.map_or_else(String::new, AudioEntry::label);
    let what = match &plan.outcome {
        Outcome::Replaced => "replaced".to_string(),
        Outcome::TracksReplaced(tracks) => {
            let names: Vec<String> = tracks
                .iter()
                .map(|&i| match entry.and_then(|e| e.tracks.get(i)).map(|t| t.display_name()) {
                    Some(name) if !name.is_empty() => format!("#{i} {name}"),
                    _ => format!("#{i}"),
                })
                .collect();
            format!("{} track(s) replaced: {}", tracks.len(), names.join(", "))
        }
        Outcome::Unchanged => "unchanged (the edit gives the same bytes)".to_string(),
    };
    println!("{:>#12x}  {label:<6}  {what}", plan.offset);
    if plan.outcome != Outcome::Unchanged {
        let fit = match plan.placement {
            Placement::InPlace => "the same size, written in place".to_string(),
            Placement::Padded { by, inside: true } => format!("{by} bytes smaller, padded inside to keep its place"),
            Placement::Padded { by, inside: false } => format!("{by} bytes smaller, zeros after it keep its place"),
            Placement::Resized => "it ends the input, so the output changes size with it".to_string(),
        };
        println!("{:>12}  {} -> {} bytes: {fit}", "", plan.old_size, plan.new_size);
    }
    for note in &plan.notes {
        println!("{:>12}  note: {note}", "");
    }
}

#[derive(Serialize)]
struct PackJson<'a> {
    id: u32,
    offset: u64,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tracks: Vec<usize>,
    old_size: u64,
    new_size: u64,
    placement: &'static str,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    notes: &'a [String],
}

impl<'a> From<&'a AudioPlan> for PackJson<'a> {
    fn from(p: &'a AudioPlan) -> Self {
        let (outcome, tracks) = match &p.outcome {
            Outcome::Replaced => ("replaced", Vec::new()),
            Outcome::TracksReplaced(t) => ("tracks_replaced", t.clone()),
            Outcome::Unchanged => ("unchanged", Vec::new()),
        };
        let placement = match p.placement {
            Placement::InPlace => "in_place",
            Placement::Padded { inside: true, .. } => "padded_inside",
            Placement::Padded { inside: false, .. } => "padded_after",
            Placement::Resized => "resized",
        };
        Self { id: p.id, offset: p.offset, outcome, tracks, old_size: p.old_size, new_size: p.new_size, placement, notes: &p.notes }
    }
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

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
