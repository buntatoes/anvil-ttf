<div align="center">
  <h1>flux-ttf</h1>
  <p>TrueType and OpenType, drawn to coverage bitmaps.</p>
  <p>
    <img src="https://img.shields.io/badge/release-v.04-e8c37a?style=flat-square" alt="v.04" />
    <img src="https://img.shields.io/badge/status-Proof_of_Concept-d4a359?style=flat-square" alt="Proof of Concept" />
    <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MPL--2.0-blue?style=flat-square" alt="MPL-2.0" /></a>
    <a href="./SECURITY.md"><img src="https://img.shields.io/badge/security-policy-2ea44f?style=flat-square" alt="Security" /></a>
    <img src="https://img.shields.io/badge/rust-2024-orange?style=flat-square" alt="Rust 2024" />
  </p>
</div>

Version 0.4.0.

The browser uses `TtfFont::parse` on Linux when a system `.ttf` is installed. If parsing fails, text falls back to a built-in bitmap font in `flux-raster`.

## What it does

- `TtfFont::parse` reads an SFNT file: `name`, `head`, `hhea`, `maxp`, `cmap` (formats 4 and 12), `hmtx`, `kern` format 0, `loca`, and `glyf`.
- Simple and compound glyph outlines are decoded. Compound glyphs stop at depth 16.
- `rasterize_glyph` returns an 8-bit coverage bitmap.
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

The browser is [flux-browser](https://github.com/buntatoes/flux-browser). These libraries sit next to it. None of them come from crates.io.

| Repository | What it is |
| --- | --- |
| [flux-cipher](https://github.com/buntatoes/flux-cipher) | AES-128, AES-256, CTR, AES-256-GCM |
| [flux-compress](https://github.com/buntatoes/flux-compress) | DEFLATE, zlib, gzip |
| [flux-image](https://github.com/buntatoes/flux-image) | PNG, JPEG, GIF, WebP, ICO, BMP, PPM |
| [flux-json](https://github.com/buntatoes/flux-json) | JSON |
| [flux-ttf](https://github.com/buntatoes/flux-ttf) | TrueType and OpenType |
| [flux-gpu](https://github.com/buntatoes/flux-gpu) | CPU drawing of boxes, shadows, and NV12 |
| [flux-video](https://github.com/buntatoes/flux-video) | CPU frames in a small MP4 container |

## License

MPL-2.0. Copyright (c) 2026 Buntos.
