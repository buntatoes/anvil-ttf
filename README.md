<div align="center">
  <h1>anvil-ttf</h1>
  <p>TrueType and OpenType, drawn to coverage bitmaps.</p>
  <p>
    <img src="https://img.shields.io/badge/release-v.05-e8c37a?style=flat-square" alt="v.05" />
    <img src="https://img.shields.io/badge/status-Proof_of_Concept-d4a359?style=flat-square" alt="Proof of Concept" />
    <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MPL--2.0-blue?style=flat-square" alt="MPL-2.0" /></a>
    <a href="./SECURITY.md"><img src="https://img.shields.io/badge/security-policy-2ea44f?style=flat-square" alt="Security" /></a>
    <img src="https://img.shields.io/badge/rust-2024-orange?style=flat-square" alt="Rust 2024" />
  </p>
</div>

Version 0.5.0.

The browser uses `TtfFont::parse` on Linux when a system `.ttf` is installed. If parsing fails, text falls back to a built-in bitmap font in `anvil-raster`.

## What it does

- `TtfFont::parse` reads an SFNT file: `name`, `head`, `hhea`, `maxp`, `cmap` (formats 4 and 12), `hmtx`, `kern` format 0, OpenType `GPOS` Lookup Type 2 (`PairPosFormat1` and `PairPosFormat2`), `loca`, and `glyf`.
- Simple and compound glyph outlines are decoded. Compound glyphs stop at depth 16.
- `rasterize_glyph` returns an 8-bit coverage bitmap, and `rasterize_glyph_subpixel_lcd` returns a 3x horizontal RGB subpixel coverage bitmap with a 3-tap FIR filter.
- `measure_text` and `glyph_advance_px` return widths, including kerning when the font has it.
- `load_system_font` looks for a font file on the machine.
- `measure_proportional_fallback` estimates width when no font file is available.

## Limits

- Font file size is capped at 32 MiB.
- At most 128 SFNT tables, 256 contours per glyph, and 4096 points per glyph.

## Build

```
cargo test
```

Rust 2024. No crates.io dependencies.

## The other repositories

The browser is [anvil-browser](https://github.com/buntatoes/anvil-browser). These libraries sit next to it. None of them come from crates.io.

| Repository | What it is |
| --- | --- |
| [anvil-cipher](https://github.com/buntatoes/anvil-cipher) | AES-128, AES-256, CTR, AES-256-GCM |
| [anvil-compress](https://github.com/buntatoes/anvil-compress) | DEFLATE, zlib, gzip |
| [anvil-image](https://github.com/buntatoes/anvil-image) | PNG, JPEG, GIF, WebP, ICO, BMP, PPM |
| [anvil-json](https://github.com/buntatoes/anvil-json) | JSON |
| [anvil-ttf](https://github.com/buntatoes/anvil-ttf) | TrueType and OpenType |
| [anvil-gpu](https://github.com/buntatoes/anvil-gpu) | CPU drawing of boxes, shadows, and NV12 |
| [anvil-video](https://github.com/buntatoes/anvil-video) | CPU frames in a small MP4 container |

## License

MPL-2.0. Copyright (c) 2026 Buntos.
