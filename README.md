<div align="center">
  <h1>Anvil TTF</h1>
  <p>TrueType and OpenType, drawn to coverage bitmaps with vertical grid fitting, multi-family slots, and gamma-correct linear blending.</p>
  <p>
    <img src="https://img.shields.io/badge/release-v.10-e8c37a?style=flat-square" alt="v.10" />
    <img src="https://img.shields.io/badge/status-Proof_of_Concept-d4a359?style=flat-square" alt="Proof of Concept" />
    <a href="./LICENSE"><img src="https://img.shields.io/badge/license-Noncommercial_%28based_on_PolyForm_1.0.0%29_%7C_Paid_Commercial_%28Non--Compete%29-blue?style=flat-square" alt="Noncommercial (based on PolyForm 1.0.0) | Paid Commercial (Non-Compete)" /></a>
    <a href="./SECURITY.md"><img src="https://img.shields.io/badge/security-policy-2ea44f?style=flat-square" alt="Security" /></a>
    <img src="https://img.shields.io/badge/rust-2024-orange?style=flat-square" alt="Rust 2024" />
  </p>
</div>

Version 0.10.0 (`v.10`).

The browser uses `TtfFont::parse` on Linux and Windows when a system `.ttf` is installed. If parsing fails, text falls back to a built-in bitmap font in `anvil-raster`.

## What it does

- `TtfFont::parse` reads an SFNT file: `name`, `head`, `hhea`, `maxp`, `cmap` (formats 4 and 12), `hmtx`, `kern` format 0, `GPOS` pair kerning (Lookup Type 2), `loca` + `glyf`, and `CFF ` outlines.
- Simple, compound, and `CFF ` Type 2 glyph outlines (including `callsubr` and `callgsubr` subroutines) are decoded. Compound glyphs stop at depth 64.
- `FontFamilySlot` (`Sans`, `Serif`, `Mono`, `SansBold`, `SansItalic`), `load_system_font_slot`, and `rasterize_glyph_styled` load family-specific system fonts and synthesize bold horizontal stem dilation and 12-degree italic shear when dedicated faces are unavailable.
- `rasterize_glyph` and `rasterize_glyph_subpixel_positioned` return 8-bit grayscale coverage bitmaps with vertical stem grid fitting (snapping baselines and horizontal stems to integer pixel rows). `rasterize_glyph_subpixel_lcd` returns a 3x horizontal RGB subpixel coverage bitmap.
- `srgb_to_linear_u12`, `linear_u12_to_srgb`, `enhance_stem_contrast`, `blend_glyph_pixel_gamma_correct`, and `blend_subpixel_lcd_gamma_correct` perform luminance-aware stem contrast enhancement and gamma-correct linear-space alpha blending.
- `measure_text` and `glyph_advance_px` return widths, including kerning when the font has it.
- `load_system_font` looks for a font file on the machine.
- `measure_proportional_fallback` estimates width when no font file is available.

## Limits

- Font file size is capped at 320 MiB.
- At most 1,280 SFNT tables, 2,560 contours per glyph, and 40,960 points per glyph.
- Glyph bitmaps are at most 512 pixels on a side. A larger font size, or a NaN or infinite one, draws no bitmap (`rasterize_glyph` returns `None`). Widths from `measure_text` and `glyph_advance_px` are still returned.

## Build

```
cargo test
```

Rust 2024 edition, Rust 1.99.1 or newer. No crates.io dependencies.

## The other repositories

The browser is [anvil-browser](https://github.com/buntatoes/anvil-browser) and the page engine is [anvil-engine](https://github.com/buntatoes/anvil-engine), built alongside the nine companion libraries below. None of them come from crates.io.

| Repository | What it is |
| --- | --- |
| [anvil-cipher](https://github.com/buntatoes/anvil-cipher) | AES-128, AES-256, CTR, AES-256-GCM, SHA-256, HMAC-SHA256, HKDF-SHA256 |
| [anvil-compress](https://github.com/buntatoes/anvil-compress) | DEFLATE, zlib, gzip |
| [anvil-hpack](https://github.com/buntatoes/anvil-hpack) | HPACK header compression for HTTP/2 |
| [anvil-image](https://github.com/buntatoes/anvil-image) | PNG, JPEG, GIF, WebP (`VP8L` + `VP8 `), ICO, BMP, PPM |
| [anvil-json](https://github.com/buntatoes/anvil-json) | JSON |
| [anvil-pqc](https://github.com/buntatoes/anvil-pqc) | ML-KEM-768, X25519, X25519MLKEM768, SHA-3 |
| [anvil-ttf](https://github.com/buntatoes/anvil-ttf) | TrueType and OpenType (`glyf`, `CFF `, `kern`, `GPOS`) |
| [anvil-gpu](https://github.com/buntatoes/anvil-gpu) | Compositor passes for boxes, shadows, gradients, and NV12/I420, on the CPU or a GPU |
| [anvil-video](https://github.com/buntatoes/anvil-video) | CPU frames (AVID/ADCT) and PCM/ADPCM/FLAC/MDCT audio in MP4, fMP4, and WebM containers |

## License

Dual-licensed under **noncommercial terms based on the PolyForm Noncommercial License 1.0.0** and a **Paid Commercial License with a Non-Compete Clause**. Copyright (c) 2026 Buntato. See [LICENSE](./LICENSE).
