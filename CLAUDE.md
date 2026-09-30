# audscan — audio scanner, extractor and (later) packer

## Goal
Find standard audio files (RIFF/RIFX WAVE including Wwise `.wem`, Wwise `.bnk` and `.pck`, FMOD FSB5 banks, Ogg) inside arbitrary binary files — game archives, extracted blobs, memory dumps — extract them, and later reinject edited versions, the way packzip does for zlib. One fast CLI, manifest-driven, with verification; a GUI later on the same core library. A sibling of zscan (`E:\zscan`, github.com/xRopers/zscan) and texscan (`E:\texscan`, github.com/xRopers/texscan): same structure, conventions and style. texscan is the closest model; copy its patterns first.

## Language and key crates
- Rust workspace, edition 2024, MSRV 1.89, license GPL-2.0-or-later (like zscan and texscan). Check licenses against GPL-2.0-or-later before adding a crate (Apache-2.0-only code makes binaries GPL-3; ask first)
- `memchr` (magic search), `memmap2` (large inputs), `rayon`, `clap`, `serde` + `serde_json` (manifest), `crc32fast`, `thiserror`/`anyhow`
- Later, for previews and conversion: decoders (Vorbis, Opus, ADPCM...) and Wwise Vorbis header reconstruction (as ww2ogg/vgmstream do); check licenses first (vgmstream is ISC-style, ww2ogg BSD)

## Layout
```
audscan/
  crates/
    audscan-core/       # library: formats, scan, manifest, extract (later decode, pack)
      src/formats/      # one file per container format: riff.rs, fsb5.rs, ogg.rs, bnk.rs, pck.rs
    audscan-cli/        # clap CLI, binary `audscan`
    audscan-fixtures/   # deterministic fixtures; `gen-fixtures` writes tests/fixtures/
  tests/fixtures/       # generated .bin + .expected.json, checked in
```
The core must never depend on the CLI or GUI.

## CLI
```
audscan scan    <file> [-o manifest.json] [--show-rejected] [--tracks] [--formats riff,fsb5,ogg,bnk,pck]
audscan extract <file> [-m manifest.json] -d out/ [--force] [--split]
```
All commands support `--json`. `--formats` takes aliases (`wav`, `wem`, `rifx` → riff; `fsb`, `fmod` → fsb5; `akpk` → pck).

## How it differs from texscan
- Audio containers mostly state their size outright (RIFF size, FSB5 section sizes); Ogg has none and is walked page by page, checking every page's CRC.
- Banks hold many sounds: FSB5, BNK and PCK are each found as one file with a track list. Wwise tracks are whole files (`Track::extension`), so `extract --split` writes them out; FSB5 tracks are raw codec data and need a header built to stand alone (later).
- Pack is harder than for textures: a new sound is rarely the same size. Plan: same-size-or-smaller replacement with padding first (RIFF allows a `JUNK` chunk, Ogg can't shrink freely), then length fields and relocation (port zscan's `fields.rs`).

## Build order
1. Workspace, `AudioFormat` trait, RIFF/RIFX + FSB5 + Ogg scan, manifest, extract, fixtures. **(done)**
2. More containers: Wwise `.bnk` and `.pck` **(done)**; FSB4 and FSB3; XWB/XSB; AIFF/AIFC; MP3 frames (no magic, opt-in); FSB5 per-track extraction (needs a header per track).
3. Decode for previews and export to WAV: PCM/ADPCM first, then Vorbis/Opus (Wwise Vorbis needs its headers rebuilt).
4. Pack: replace with a same-format file that fits, verify; then length fields and relocation.
5. GUI (egui, like texscan-gui): file strip, table, track list, playback, replace/revert, pack window.
6. Optionally scan inside compressed streams by depending on `zscan-core`.

## Status
- Stage 1 done: `audscan scan | extract [--split]`. Wwise BNK and PCK added (stage 2, part).
- `Track` (format.rs): a sound or file inside a bank/package: name (FSB5) or id (Wwise), language (PCK), codec (Wwise, per track), extension (`wem`/`bnk` when it's a whole file that `--split` can write), channels/rate (0 if not audio), samples, offset/size from the container's start, note (prefetch). `display_name()`; `split_filename()` = `{lang}/{id}.{ext}` (no folder for `sfx` or unsafe names). `AudioInfo::duration` of a bank sums its audio tracks, `None` if any is unknown.
- BNK (`formats/bnk.rs`): sections (tag + u32 size) from `BKHD` while the tag is a known one (BKHD DIDX DATA HIRC STID STMG ENVS FXPR PLAT INIT; a second BKHD is the next bank); size = end of the last. Byte order: whichever reads a bank version in 1..=0x1000 at +8; BKHD must be 8..=64 KiB. DIDX entries (id, offset in DATA, size) become tracks, described by `riff::parse_prefix`; zero-size entries are skipped. Codec = the WEMs' shared codec, `mixed`, or `no media`.
- Prefetch media: banks keep the first part of a streamed WEM. `riff::parse_prefix` reads a header whose file is cut short: size = what's there, samples from the declared data size, note `prefetch: the first N of M bytes`. Before BNK support the plain RIFF scan took those at their declared size and swallowed the media after them (Once Human: ~1,900 sounds missed).
- PCK (`formats/pck.rs`): `AKPK`, header size, version 1 (LE or BE by which reads 1), sizes of the language map, bank table, stream table and (if the sizes only add up with a fifth field) external table. Tables: count + entries of 20 bytes (u32 id) or 24 (u64 id), told apart by size: id, block size, file size, start block, language id; offset = start block × block size. Size = furthest file end. Language names UTF-16LE/BE or 8-bit. Bank entries are `bnk` tracks (codec `SoundBank`), not parsed further; streams and externals are WEMs.
- `AudioFormat` (`format.rs`): `magics()` (RIFF has two: `RIFF`, `RIFX`) + `parse(data from offset to EOF) -> Result<AudioInfo, Reject>`. `Reject::NoMatch` is silent (magic in text, non-audio RIFF forms, mid-stream Ogg pages); `Reject::Bad(reason)` is reported (`ScanReport::rejected`, `--show-rejected`). Accepted only if the whole file fits.
- `AudioInfo`: size, codec name, channels, sample rate, samples per channel (when known), `big_endian` (RIFX), `wwise`, `tracks` (FSB5), `note`. Labels: `wav`/`wem`/`fsb5`/`ogg`, ` BE` for RIFX; extracted as `.wav`/`.wem`/`.fsb`/`.ogg`.
- Scan (`scan.rs`): as texscan: `memmem` for each magic, parse in parallel, keep in file order, skip candidates inside found audio. 81 GB of Once Human archives in 96 s (~840 MB/s).
- RIFF (`formats/riff.rs`): form `WAVE` or `XWMA` only (so FMOD's `RIFF....FEV ` bank is skipped and its FSB5 found). Size = 8 + RIFF size; chunks walked with even padding; needs `fmt ` and `data`. `WAVE_FORMAT_EXTENSIBLE` uses the subformat's tag. Wwise = its own tags (0xFFFF Vorbis, 0x3039/0x3040/0x3041 Opus, 0x8311 PTADPCM), an `akd ` chunk, or RIFX at all. Samples: PCM/float/A-law/mu-law from the data size, Wwise Vorbis from `vorb` (its own chunk, or fmt+0x18 in a 0x42-byte fmt), others from `fact`. Not yet: Wwise Opus/PTADPCM/IMA lengths, XMA2 lengths.
- FSB5 (`formats/fsb5.rs`): header 0x3C (0x40 for version 0); size = header + track headers + name table + sample data. Track header u64: bit 0 extra chunks follow, 1–4 rate index, 5–6 channel code (1/2/6/8), 7–33 data offset / 32, 34–63 samples. Extra chunks (u32: bit 0 more, 1–24 size, 25–31 type) override channels (1) and rate (2). A track's data runs to the next track's. Codec numbers 1–17 named; others rejected.
- Ogg (`formats/ogg.rs`): only a BOS page starts a file; walks pages (CRC-checked) until every stream begun in the BOS group has an EOS page. A new BOS after data pages (or a repeated serial) starts the next chain link, found separately. Stops early with a `note` if pages run out. Codec from each stream's first packet (Vorbis, Opus, FLAC, Speex; Theora, Skeleton named); several streams joined with ` + `, details from the first audio stream. Samples = last granule (Opus: minus pre-skip, at 48 kHz).
- Real-file checks: Once Human (NetEase, Wwise): 81 GB of `.npk` archives in 140 s → 17,748 loose WEMs and 1,215 BNKs holding 26,499 WEMs (688 of them prefetch) plus 4 non-WEM media (headerless, 48 kHz: probably convolution-reverb impulse responses), nothing rejected. The gaps between consecutive WEMs are all 0–15 bytes (the archive's 16-byte alignment), so every size is exact; WEMs split out of `res_normal_pack_104.npk`'s banks scan to exactly their length (except the prefetch ones, as expected). No real `.pck` on this machine: PCK is checked by fixtures only. Every `.wav` on this machine (1,772: Windows, KiCad, UE 5.8 samples; PCM 8/16/24/32, float, MS ADPCM) scans as one file of exactly its length. 27 of them (Unreal/Harmonix test WAVs) have a RIFF size 4 bytes short (a 20-byte fmt counted as 16): when the `data` chunk runs past the RIFF size but fits in the input, its end is used and a note says so. These files aren't in the repo.
- Manifest v1 (`manifest.rs`): per file id, offset, size, format, codec, channels, sample_rate, samples, big_endian/wwise (omitted when false), tracks (FSB5), note, crc32, file (`{offset:08x}.{ext}`). Extract checks the input's size and CRC (`--force` skips that) and each file's CRC.
- Fixtures: `audscan-fixtures` writes RIFF, FSB5 and Ogg (with its own bitwise CRC) independently of the core. `audio_archive` covers PCM with odd chunks, RIFX and RIFF Wwise Vorbis (both sample-count locations), an `akd` PCM WEM, MS ADPCM + fact, extensible float, an FSB5 inside a RIFF `FEV ` bank (named tracks, extra chunks), FSB5 v0, Ogg Vorbis/Opus/FLAC, a chained pair, Theora+Vorbis multiplexed, an Ogg without EOS, an Ogg inside a WAV's data (must be skipped), a RIFF size 4 bytes short, BNKs (LE with PCM + Vorbis + prefetch media, BE with a RIFX WEM, no media), PCKs (with externals and two languages sharing an ID; old layout with 2 KiB blocks), and traps (text, AVI, a mid-stream page, no fmt, unknown FSB codec, a DIDX entry past DATA, a PCK file past the end, bad CRC, truncated). `checked_in_fixtures_are_current` fails if `tests/fixtures` is stale: `cargo run -p audscan-fixtures --bin gen-fixtures`.
- cargo is at `%USERPROFILE%\.cargo\bin`, not on the Git Bash PATH; use `~/.cargo/bin/cargo` or PowerShell. `cargo test --workspace`; CI runs clippy with `-D warnings`.

## Working notes
- Prefer direct, plain explanations with concrete next steps.
- Write tests alongside each stage; fixtures should cover each format, false-positive traps, and (later) a pack round trip.
