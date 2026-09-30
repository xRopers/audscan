# audscan

Find audio inside any binary file and extract it.

Games usually keep their sounds as standard files packed inside their own archives: Wwise `.wem`, FMOD sound banks, Ogg, plain WAV. audscan finds them by their headers, works out each one's exact size, and writes them out as ordinary files, without needing a tool for that particular engine.

It's a sibling of [zscan](https://github.com/xRopers/zscan) (compressed streams) and [texscan](https://github.com/xRopers/texscan) (textures), and works the same way: a scan writes a JSON manifest, and later steps work from it.

- **Scan** a file for WAV, Wwise WEM (RIFF and big-endian RIFX), FMOD FSB5 banks and Ogg streams, with their codec, channels, sample rate and length.
- **Extract** them as `.wav`, `.wem`, `.fsb` and `.ogg` files, byte for byte.

**Status: early.** Scan and extract work. Wwise `.bnk`/`.pck` indexes, more formats, conversion to WAV, putting edited sounds back, and a desktop app are next.

## Build

Rust 1.89 or later:

```bash
cargo build --release
```

## Usage

```bash
audscan scan game.pak -o manifest.json      # list audio, write a manifest
audscan scan game.pak --tracks              # also list every sound in an FSB5 bank
audscan extract game.pak -m manifest.json -d audio/
audscan extract game.pak -d audio/ --formats wem   # scan and extract in one go, WEMs only
```

```
      OFFSET        SIZE  FORMAT  CODEC               CH    RATE      LENGTH  NAME
        0x74        4074  wav     PCM 16-bit           2   44100    0:00.023
      0x108b        3094  wem BE  Wwise Vorbis         2   48000    0:10.000
      0x1cc0        1602  wem     Wwise Vorbis         1   32000    0:03.000
      0x3266        1450  fsb5    Vorbis               2   44100    0:12.500  3 tracks: music_intro, vo_line_01, amb_odd
      0x3c97        1356  ogg     Vorbis               2   44100    0:02.268
      0x420d         604  ogg     Opus                 2   48000    0:02.000
...
```

Every command takes `--json`. The input is never modified. `extract` refuses a file that no longer matches the manifest (`--force` overrides). `--show-rejected` lists headers that look like audio but can't be used, and why (a WAV with no `fmt ` chunk, a file cut off by the end of the input...).

## Formats

### WAV and Wwise WEM (RIFF, RIFX)

- `RIFF` and big-endian `RIFX` files of form `WAVE` (and `XWMA`). Other RIFF forms (AVI, WebP, FMOD Studio `.bank` files) are skipped, so an FSB5 bank inside a `.bank` is still found.
- The codec is named from the `fmt ` chunk: PCM, IEEE float, A-law, mu-law, MS and IMA ADPCM, MP3, WMA, XMA/XMA2, ATRAC3/ATRAC9, `WAVE_FORMAT_EXTENSIBLE`, and Wwise's own (Vorbis, Opus, PTADPCM, IMA ADPCM).
- Wwise audio is extracted as `.wem`: recognised by its codec, an `akd ` chunk, or being RIFX.
- Lengths come from the data size (PCM), the `fact` chunk, or Wwise Vorbis's own sample count.

Checked against Once Human's archives (a Wwise game): 81 GB scanned in 96 seconds, finding 42,337 WEMs (Wwise Vorbis and PTADPCM), every one sized exactly. All 1,772 `.wav` files on the development machine scan to exactly their length, including 27 whose RIFF header understates it by 4 bytes (a common writer bug; the `data` chunk shows the real end, and a note says so).

### FMOD sound banks (FSB5)

- The whole bank is found and extracted as one `.fsb`, with its track list: names, channels, sample rates, lengths and where each track's data is.
- All FSB5 codecs are named: PCM, GameCube ADPCM, IMA ADPCM, VAG/HEVAG, XMA, MPEG, CELT, ATRAC9, xWMA, Vorbis, FMOD ADPCM, Opus.

### Ogg

- Vorbis, Opus, FLAC and Speex, over any number of pages, each page's CRC checked.
- Multiplexed streams (Theora video with Vorbis audio, say) are one file; chained files (one stream after another) are found as one file per stream.
- A stream with no end-of-stream page is still found, with a note.

## License

audscan is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 2 of the License, or (at your option) any later version (GPL-2.0-or-later). See [LICENSE](LICENSE).
