# audscan

Find audio inside any binary file and extract it.

Games usually keep their sounds as standard files packed inside their own archives: Wwise `.wem`, `.bnk` and `.pck`, FMOD sound banks, Ogg, plain WAV. audscan finds them by their headers, works out each one's exact size, and writes them out as ordinary files, without needing a tool for that particular engine.

It's a sibling of [zscan](https://github.com/xRopers/zscan) (compressed streams) and [texscan](https://github.com/xRopers/texscan) (textures), and works the same way: a scan writes a JSON manifest, and later steps work from it.

- **Scan** a file for WAV, Wwise WEM (RIFF and big-endian RIFX), Wwise SoundBanks (`.bnk`) and file packages (`.pck`), FMOD FSB5 banks and Ogg streams, with their codec, channels, sample rate and length, and what's inside each bank or package.
- **Extract** them as `.wav`, `.wem`, `.bnk`, `.pck`, `.fsb` and `.ogg` files, byte for byte, and with `--split` every WEM inside a Wwise bank or package as a file of its own, named by its Wwise ID.

**Status: early.** Scan and extract work. More formats, splitting FSB5 banks, conversion to WAV, putting edited sounds back, and a desktop app are next.

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
audscan extract game.pak -d audio/ --split  # also each WEM inside a Wwise bank or package
audscan extract game.pak -d audio/ --formats wem   # scan and extract in one go, WEMs only
```

```
      OFFSET        SIZE  FORMAT  CODEC               CH    RATE      LENGTH  NAME
        0x99        4074  wav     PCM 16-bit           2   44100    0:00.023
      0x10b0        3094  wem BE  Wwise Vorbis         2   48000    0:10.000
      0x1ce5        1602  wem     Wwise Vorbis         1   32000    0:03.000
      0x328b        1450  fsb5    Vorbis               2   44100    0:12.500  3 tracks: music_intro, vo_line_01, amb_odd
      0x3cbc        1356  ogg     Vorbis               2   44100    0:02.268
      0x4ee5        1994  bnk     mixed                1   48000    0:22.004  3 tracks: 111, 222, 333
      0x595d        1592  pck     Wwise Vorbis         2   48000    0:02.500  4 files (1 bnk, 3 wem): 777, 100, 100, ...
          #0         258  bnk     SoundBank            0       0           -  777 [sfx]
          #1         494  wem     Wwise Vorbis         2   48000    0:01.000  100 [sfx]
          #2         294  wem     Wwise Vorbis         1   48000    0:00.500  100 [english(us)]
          #3         344  wem     Wwise Vorbis         2   44100    0:01.000  4294967297 [sfx]
...

(The package's contents are listed with `--tracks`.)
```

Every command takes `--json`. The input is never modified. `extract` refuses a file that no longer matches the manifest (`--force` overrides). `--show-rejected` lists headers that look like audio but can't be used, and why (a WAV with no `fmt ` chunk, a file cut off by the end of the input...).

## Formats

### WAV and Wwise WEM (RIFF, RIFX)

- `RIFF` and big-endian `RIFX` files of form `WAVE` (and `XWMA`). Other RIFF forms (AVI, WebP, FMOD Studio `.bank` files) are skipped, so an FSB5 bank inside a `.bank` is still found.
- The codec is named from the `fmt ` chunk: PCM, IEEE float, A-law, mu-law, MS and IMA ADPCM, MP3, WMA, XMA/XMA2, ATRAC3/ATRAC9, `WAVE_FORMAT_EXTENSIBLE`, and Wwise's own (Vorbis, Opus, PTADPCM, IMA ADPCM).
- Wwise audio is extracted as `.wem`: recognised by its codec, an `akd ` chunk, or being RIFX.
- Lengths come from the data size (PCM), the `fact` chunk, or Wwise Vorbis's own sample count.

Checked against Once Human's archives (a Wwise game): 81 GB scanned in 140 seconds, finding 17,748 loose WEMs and 1,215 SoundBanks holding 26,500 more (Wwise Vorbis and PTADPCM), every one sized exactly. All 1,772 `.wav` files on the development machine scan to exactly their length, including 27 whose RIFF header understates it by 4 bytes (a common writer bug; the `data` chunk shows the real end, and a note says so).

### Wwise SoundBanks (BNK) and file packages (PCK)

- A SoundBank (`BKHD`...) is found whole, and its media index (`DIDX`) lists the WEMs in its `DATA` section: ID, codec, channels, rate and length of each. Banks without media (events only) are found too. Big-endian banks from older consoles work.
- A file package (`AKPK`) is found whole, with every SoundBank, streamed WEM and external WEM (64-bit IDs) in its lookup tables, and each one's language from the package's own language map.
- `extract --split` writes each of them as `<ID>.wem` or `<ID>.bnk` in a folder named after the bank or package, with localized files in a folder per language (the same ID is often used once per language). Scan a split-out `.bnk` to list what's inside it.
- Banks often keep just the start of a streamed WEM ("prefetch" media) so it can start playing at once. Those are read from their header (codec, length) and noted as partial; the whole WEM is in the game's streamed files or `.pck`.

On Once Human, every one of the 1,215 banks was found without a problem. Of the WEMs split out of one archive, all 90 complete ones scan to exactly their length. 4 more are prefetch media and 4 banks hold media that aren't WEMs, likely plugin data such as reverb impulse responses (listed as unknown).

### FMOD sound banks (FSB5)

- The whole bank is found and extracted as one `.fsb`, with its track list: names, channels, sample rates, lengths and where each track's data is.
- All FSB5 codecs are named: PCM, GameCube ADPCM, IMA ADPCM, VAG/HEVAG, XMA, MPEG, CELT, ATRAC9, xWMA, Vorbis, FMOD ADPCM, Opus.

### Ogg

- Vorbis, Opus, FLAC and Speex, over any number of pages, each page's CRC checked.
- Multiplexed streams (Theora video with Vorbis audio, say) are one file; chained files (one stream after another) are found as one file per stream.
- A stream with no end-of-stream page is still found, with a note.

## License

audscan is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 2 of the License, or (at your option) any later version (GPL-2.0-or-later). See [LICENSE](LICENSE).
