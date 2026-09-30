# Wwise Vorbis codebooks

`packed_codebooks_aoTuV_603.bin` is the table of Vorbis codebooks that Wwise (2011.2 and later, aoTuV 6.03 encoder) refers to by number instead of storing them in each `.wem`. audscan needs it to rebuild a standard Vorbis setup header when converting WEMs to Ogg.

It comes from [ww2ogg](https://github.com/hcs64/ww2ogg) by Adam Gashlin (file `packed_codebooks_aoTuV_603.bin`, 74,387 bytes, SHA-256 `00a93eab267d281401b1efd54e888a2e183299b9e6c446c48d09f701a89d9d27`), under the BSD 3-clause license in `COPYING-ww2ogg`, which is compatible with audscan's GPL. The codebooks themselves derive from Xiph.org's Vorbis encoder.

Format: the packed codebooks back to back, then a little-endian u32 offset for each, and last the offset of that offset table.
