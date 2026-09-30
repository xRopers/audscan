# audscan

Find audio inside any binary file and extract it.

Games usually keep their sounds as standard files packed inside their own archives: Wwise `.wem`, `.bnk` and `.pck`, FMOD sound banks, Ogg, plain WAV. audscan finds them by their headers, works out each one's exact size, and writes them out as ordinary files, without needing a tool for that particular engine.

It's a sibling of [zscan](https://github.com/xRopers/zscan) (compressed streams) and [texscan](https://github.com/xRopers/texscan) (textures), and works the same way: a scan writes a JSON manifest, and later steps work from it.

- **Scan** a file for WAV, Wwise WEM (RIFF and big-endian RIFX), Wwise SoundBanks (`.bnk`) and file packages (`.pck`), FMOD FSB4 and FSB5 banks and Ogg streams, with their codec, channels, sample rate and length, and what's inside each bank or package.
- **Extract** them as `.wav`, `.wem`, `.bnk`, `.pck`, `.fsb` and `.ogg` files, byte for byte. With `--split`, every sound inside a bank or package also comes out as a file of its own: Wwise WEMs named by their ID, FMOD tracks by their name.
- **Convert** WEMs to files any player opens: Wwise Vorbis and Opus to Ogg (rewrapped, not re-encoded, so nothing is lost), PCM and IMA ADPCM to WAV. `audscan convert` for WEM files, or `extract --convert` for everything extracted.

**Status: early.** Scan, extract, splitting and WEM conversion work. More formats, putting edited sounds back, and a desktop app are next.

## Build

Rust 1.89 or later:

```bash
cargo build --release
```

## Usage

```bash
audscan scan game.pak -o manifest.json      # list audio, write a manifest
audscan scan game.pak --tracks              # also list what's inside each bank and package
audscan extract game.pak -m manifest.json -d audio/
audscan extract game.pak -d audio/ --split  # also each sound inside a bank or package
audscan extract game.pak -d audio/ --formats wem   # scan and extract in one go, WEMs only
audscan scan game.pak --formats fmod        # FSB4 and FSB5 only (and wwise: WEM, BNK, PCK)
audscan extract game.pak -d audio/ --split --convert   # every WEM also as .ogg or .wav
audscan convert audio/*.wem -d playable/    # convert WEM files you already have
```

```
      OFFSET        SIZE  FORMAT  CODEC               CH    RATE      LENGTH  NAME
        0xa4        4074  wav     PCM 16-bit           2   44100    0:00.023
      0x10bb        3094  wem BE  Wwise Vorbis         2   48000    0:10.000
      0x1cf0        1602  wem     Wwise Vorbis         1   32000    0:03.000
      0x3296        1450  fsb5    Vorbis               2   44100    0:12.500  3 tracks: music_intro, vo_line_01, amb_odd
      0x3cc7        1856  fsb4    mixed                2   44100    0:00.105  3 tracks: menu_theme, blip, voice_01
      0x4b48        1356  ogg     Vorbis               2   44100    0:02.268
      0x5da1        1994  bnk     mixed                1   48000    0:22.004  3 tracks: 111, 222, 333
      0x6812        1592  pck     Wwise Vorbis         2   48000    0:02.500  4 files (1 bnk, 3 wem): 777, 100, 100, ...
          #0         258  bnk     SoundBank            0       0           -  777 [sfx]
          #1         494  wem     Wwise Vorbis         2   48000    0:01.000  100 [sfx]
          #2         294  wem     Wwise Vorbis         1   48000    0:00.500  100 [english(us)]
          #3         344  wem     Wwise Vorbis         2   44100    0:01.000  4294967297 [sfx]
...
```

The rows starting `#` list what's inside a bank or package (`--tracks`).

Every command takes `--json`. The input is never modified. `extract` refuses a file that no longer matches the manifest (`--force` overrides). `--show-rejected` lists headers that look like audio but can't be used, and why (a WAV with no `fmt ` chunk, a file cut off by the end of the input...).

## Formats

### WAV and Wwise WEM (RIFF, RIFX)

- `RIFF` and big-endian `RIFX` files of form `WAVE` (and `XWMA`). Other RIFF forms (AVI, WebP, FMOD Studio `.bank` files) are skipped, so an FSB5 bank inside a `.bank` is still found.
- The codec is named from the `fmt ` chunk: PCM, IEEE float, A-law, mu-law, MS and IMA ADPCM, MP3, WMA, XMA/XMA2, ATRAC3/ATRAC9, `WAVE_FORMAT_EXTENSIBLE`, and Wwise's own (Vorbis, Opus, PTADPCM, IMA ADPCM, PCM).
- Wwise audio is extracted as `.wem`: recognised by its codec, an `akd ` chunk, being RIFX, or Wwise's own short `fmt ` layout for ADPCM and PCM.
- Lengths come from the data size (PCM, IMA ADPCM), the `fact` chunk, or the sample count Wwise stores for Vorbis, Opus and PTADPCM.
- Common writer mistakes are tolerated, each with a note: a RIFF size that's 4 bytes short or counts a padding byte that isn't there, a last chunk a byte short, or a broken metadata chunk after the audio.

Checked against real games:

- **Once Human** (Wwise): 81 GB of archives scanned in 140 seconds, finding 17,748 loose WEMs and 1,215 SoundBanks holding 26,500 more, every one sized exactly.
- **Aniimo** and **BioShock Infinite** (Wwise, 2013 to now): all 637 loose WEMs, 2,022 SoundBanks and both packages scan to exactly their length. The 113,000 sounds inside (Vorbis, Opus, PTADPCM, IMA ADPCM, PCM) all get a codec and a length, and the lengths agree with each file's byte rate. All 10,200 WEMs split out of BioShock's packages are exact, voice lines in an `english(us)` folder.
- **Left 4 Dead 2**: all 26,725 WAVs scan to exactly their length. 7,175 of them needed the writer-mistake handling above.
- All 1,772 other `.wav` files on the development machine (Windows, KiCad, Unreal Engine samples) are exact too.
- Against vgmstream r2117, on 1,944 complete WEMs (loose, and split from banks and packages): channels, rate and length agree for every Vorbis, Opus, IMA ADPCM and PCM file. PTADPCM lengths are the exact count Wwise stores; vgmstream counts whole blocks, which adds up to one block of padding (under 64 samples). A sample of 150 WEMs decodes to exactly its length.

### Wwise SoundBanks (BNK) and file packages (PCK)

- A SoundBank (`BKHD`...) is found whole, and its media index (`DIDX`) lists the WEMs in its `DATA` section: ID, codec, channels, rate and length of each. Banks without media (events only) are found too. Big-endian banks from older consoles work.
- A file package (`AKPK`) is found whole, with every SoundBank, streamed WEM and external WEM (64-bit IDs) in its lookup tables, and each one's language from the package's own language map.
- `extract --split` writes each of them as `<ID>.wem` or `<ID>.bnk` in a folder named after the bank or package, with localized files in a folder per language (the same ID is often used once per language). Scan a split-out `.bnk` to list what's inside it.
- Banks often keep just the start of a streamed WEM ("prefetch" media) so it can start playing at once. Those are read from their header (codec, length) and noted as partial; the whole WEM is in the game's streamed files or `.pck`.

On Once Human, every one of the 1,215 banks was found without a problem. Of the WEMs split out of one archive, all 90 complete ones scan to exactly their length. 4 more are prefetch media and 4 banks hold media that aren't WEMs, likely plugin data such as reverb impulse responses (listed as unknown).

### FMOD sound banks (FSB4, FSB5)

FSB5 is FMOD Studio's format (about 2013 on); FSB4 is FMOD Ex's (about 2006 to 2013).

- The whole bank is found and extracted as one `.fsb`, with its track list: names, channels, sample rates, lengths and where each track's data is.
- FSB4 banks with "basic headers" (only the first track described in full) work, and so does big-endian PCM. FSB4 doesn't record how the tracks' data is aligned; it's worked out from the data size.
- `extract --split` writes each track as a file of its own, in a folder named after the bank:
  - PCM tracks (8-, 16-, 24-, 32-bit, float) as ordinary `.wav` files, converted where WAV needs it (signed 8-bit to unsigned, big-endian to little-endian).
  - Every other codec as a one-track `.fsb` of the same version: the bank's header cut down to that track, with its data copied unchanged. vgmstream, foobar2000 (with vgmstream) and FMOD's tools play these. The raw data alone wouldn't play: Vorbis in FSB5 has no setup headers, and XMA, ADPCM and the rest need their parameters from the track header.
  - Files are named after the track, with characters a file name can't hold replaced by `_`. Tracks with the same name get their number added, and unnamed ones are `track<N>`. Each file is read back and checked against the track before it's written.
- All FSB5 codecs are named: PCM, GameCube ADPCM, IMA ADPCM, VAG/HEVAG, XMA, MPEG, CELT, ATRAC9, xWMA, Vorbis, FMOD ADPCM, Opus. FSB4 names each track's own (a bank can mix them): PCM, MPEG, IMA ADPCM, VAG, XMA, GameCube ADPCM, CELT.

FSB5 is checked against real banks. **Slay the Spire 2** keeps FMOD Studio `.bank` files inside its Godot package: all 11 FSB5 banks are found (2,509 named Vorbis tracks, about 7 hours of audio), each ending exactly where its `.bank` does, and every split track is a one-track bank of exactly its length. So are the two in **VTube Studio**'s Unity `.resource` file (Unity stores AudioClips as FSB5).

Split tracks play. Checked with [vgmstream](https://github.com/vgmstream/vgmstream) r2117: each of the 2,511 split tracks has the same length, rate, channels, codec and name as the same subsong of its original bank. 120 of them (226 MB of audio) decode to exactly the same samples either way. FSB4 is checked by test files only. Scanning 81 GB of a non-FMOD game's archives found no false FSB4 or FSB5 banks.

### Ogg

- Vorbis, Opus, FLAC and Speex, over any number of pages, each page's CRC checked.
- Multiplexed streams (Theora video with Vorbis audio, say) are one file; chained files (one stream after another) are found as one file per stream.
- A stream with no end-of-stream page is still found, with a note.

## Converting WEMs

| WEM codec | Becomes | How |
|---|---|---|
| Wwise Vorbis | `.ogg` | Vorbis headers rebuilt, packets rewrapped: no re-encoding |
| Wwise Opus | `.ogg` | Opus packets rewrapped in Ogg Opus: no re-encoding |
| PCM | `.wav` | copied |
| Wwise IMA ADPCM | `.wav` | decoded to 16-bit PCM |
| Wwise PTADPCM | not yet | |

- Wwise leaves out most of what a Vorbis decoder needs. audscan rebuilds the three Vorbis headers, unpacking the codebooks from Wwise's standard table (Wwise 2011.2 and later), and puts back the bits Wwise strips from each audio packet (as [ww2ogg](https://github.com/hcs64/ww2ogg) does). It also works out every page's granule position and trims the end to the exact sample count, so no separate `revorb` pass is needed.
- Prefetch media (the first seconds of a streamed sound that a bank keeps) can't be converted on their own: the rest of the sound is in the game's streamed files or `.pck`. Split and convert the package to get the whole sound.
- Vorbis with more than 2 channels keeps Wwise's channel order (as in WAV: L R C LFE...), which players read in Vorbis's order (L C R...). The channels can't be reordered without re-encoding, so the conversion notes it. Mono and stereo, nearly all game audio, are unaffected.

Checked with [vgmstream](https://github.com/vgmstream/vgmstream) r2117 on 1,690 real WEMs from Aniimo and BioShock Infinite: every converted file decodes to exactly the same samples as the WEM (520 Vorbis, 195 Opus, 275 IMA ADPCM including 40 stereo, and PCM). The one exception is a 5.1 Vorbis track, whose samples are the same with the channels in Wwise's order, as above. The tests also check the rebuilt Vorbis headers with an independent decoder ([lewton](https://github.com/RustAudio/lewton)).

## License

audscan is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 2 of the License, or (at your option) any later version (GPL-2.0-or-later). See [LICENSE](LICENSE).

The Wwise Vorbis codebook table (`crates/audscan-core/data/packed_codebooks_aoTuV_603.bin`) comes from ww2ogg by Adam Gashlin, under the BSD 3-clause license in `crates/audscan-core/data/COPYING-ww2ogg`, which is compatible with the GPL.
