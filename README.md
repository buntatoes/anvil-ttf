# flux-ttf

[![Release](https://img.shields.io/badge/release-v.03-e8c37a)](https://github.com/buntatoes/flux-ttf/releases/tag/v.03)
[![License: MPL-2.0](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](./LICENSE)
[![Edition](https://img.shields.io/badge/rust-2024_edition-orange.svg)](https://github.com/buntatoes/flux-ttf)

`flux-ttf` is a standalone, zero-dependency Rust library that parses **TrueType** and **OpenType** (`.ttf` / `.otf`) SFNT font binaries to compute exact proportional glyph advance widths, horizontal kerning pairs, vertical line metrics, font family metadata (`name`), TrueType `loca`/`glyf` vector contours, and anti-aliased scanline glyph coverage bitmaps.

---

## Features (`v.03`)

- **SFNT Table Directory & Metadata Parser (`TtfFont::parse` / `TtfFont::family_name`)**:
  - Validates TrueType (`0x00010000`, `true`) and OpenType (`OTTO`) SFNT containers under strict resource limits (`MAX_FONT_BYTES = 32 MiB`, `MAX_SFNT_TABLES = 128`).
  - Parses `name` (Font Family & Full Name), `head` (`unitsPerEm`, `indexToLocFormat`), `hhea` (`ascender`, `descender`, `lineGap`, `numberOfHMetrics`), `maxp` (`numGlyphs`), `cmap` (**Format 4** BMP segment mapping and **Format 12** full Unicode segmented coverage), `hmtx` (per-glyph advance widths and left side bearings), `kern` (**Format 0** horizontal kerning pairs), `loca` (short and long glyph location indices), and `glyf` (simple and compound glyph vector contours).
- **Hardened TrueType Outline Decoder & Anti-Aliased Scanline Rasterizer (`TtfFont::glyph_outline` / `TtfFont::rasterize_glyph`)**:
  - Decodes simple glyph contours (bounded by `MAX_GLYPH_CONTOURS = 256` and `MAX_CONTOUR_POINTS = 4096`) and compound glyph components (bounded by `MAX_COMPOSITE_GLYPH_DEPTH = 16`).
  - Flattens quadratic Bézier curves and rasterizes glyphs at arbitrary pixel sizes into 8-bit alpha coverage bitmaps (`GlyphBitmap`) using a 4×4 supersampled non-zero winding scanline rasterizer.
- **Proportional Text Measurement (`TtfFont::measure_text`)**:
  - Computes exact pixel widths for arbitrary UTF-8 strings at any target pixel size, including inter-glyph kerning adjustments.
- **Calibrated Proportional Fallback (`measure_proportional_fallback`)**:
  - Provides proportional character advance ratios when measuring text without an external font binary.

---

## Quick Start

```rust
use flux_ttf::{TtfFont, measure_proportional_fallback};

// Measure proportional text using calibrated fallback ratios
let narrow = measure_proportional_fallback("iiii", 16.0);
let wide = measure_proportional_fallback("MMMM", 16.0);
assert!(wide > narrow * 2.0);

// Or load a system TrueType font and rasterize an anti-aliased glyph
if let Some(font) = TtfFont::load_system_font(false, false) {
    let width_px = font.measure_text("Hello, Flux!", 18.0);
    assert!(width_px > 0.0);
    if let Some(bitmap) = font.rasterize_glyph('A', 24.0) {
        assert!(bitmap.width > 0 && bitmap.height > 0);
    }
}
```

---

## API Overview

| Function / Method | Description |
| :--- | :--- |
| `TtfFont::parse(&[u8]) -> Result<TtfFont, TtfError>` | Parses a `.ttf` or `.otf` binary buffer into a `TtfFont`. |
| `TtfFont::load_system_font(bold, monospace) -> Option<TtfFont>` | Loads a standard system TrueType font if available. |
| `TtfFont::glyph_id(char) -> u16` | Maps a Unicode scalar to its glyph index via `cmap`. |
| `TtfFont::glyph_outline(u16) -> Option<&GlyphOutline>` | Returns the decoded `loca`/`glyf` vector contours for a glyph index. |
| `TtfFont::rasterize_glyph(char, f32) -> Option<GlyphBitmap>` | Rasterizes a glyph outline into an 8-bit anti-aliased alpha coverage bitmap. |
| `TtfFont::glyph_advance_px(char, f32) -> f32` | Returns the scaled horizontal advance width in pixels for a character. |
| `TtfFont::measure_text(&str, f32) -> f32` | Measures the total pixel advance width of a string (including kerning). |
| `TtfFont::line_height_px(f32) -> f32` | Computes recommended line height (`ascender - descender + lineGap`) in pixels. |
| `measure_proportional_fallback(&str, f32) -> f32` | Measures text width using calibrated proportional character ratios. |

---

## Building and Testing

```powershell
cargo test
```

---

## License

Licensed under the Mozilla Public License Version 2.0 ([`MPL-2.0`](./LICENSE)).

## Author

**Buntos**
