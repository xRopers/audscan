//! Writes every fixture to a directory (default `tests/fixtures`) as `<name>.bin`
//! plus `<name>.expected.json` listing the audio the scanner should report.

use std::fs;
use std::path::PathBuf;

use serde_json::json;

fn main() -> std::io::Result<()> {
    let dir = std::env::args().nth(1).map_or_else(|| PathBuf::from("tests/fixtures"), PathBuf::from);
    fs::create_dir_all(&dir)?;
    for fixture in audscan_fixtures::all() {
        let audio: Vec<_> = fixture
            .expected
            .iter()
            .map(|e| {
                let tracks: Vec<_> = e
                    .tracks
                    .iter()
                    .map(|(name, channels, rate, samples)| {
                        json!({ "name": name, "channels": channels, "sample_rate": rate, "samples": samples })
                    })
                    .collect();
                json!({
                    "offset": e.offset,
                    "format": e.container,
                    "label": e.label,
                    "size": e.size,
                    "codec": e.codec,
                    "channels": e.channels,
                    "sample_rate": e.sample_rate,
                    "samples": e.samples,
                    "tracks": tracks,
                    "noted": e.noted,
                    "crc32": format!("{:08x}", e.crc32),
                })
            })
            .collect();
        let rejected: Vec<_> =
            fixture.rejected.iter().map(|r| json!({ "offset": r.offset, "reason": r.reason_prefix })).collect();
        let expected = json!({
            "description": fixture.description,
            "size": fixture.data.len(),
            "audio": audio,
            "rejected": rejected,
        });
        fs::write(dir.join(format!("{}.bin", fixture.name)), &fixture.data)?;
        fs::write(
            dir.join(format!("{}.expected.json", fixture.name)),
            serde_json::to_string_pretty(&expected).unwrap() + "\n",
        )?;
        println!("{:<14} {:>8} bytes, {} audio file(s)", fixture.name, fixture.data.len(), fixture.expected.len());
    }
    Ok(())
}
