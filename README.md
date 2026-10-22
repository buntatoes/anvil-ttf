# flux-ttf

[![Release](https://img.shields.io/badge/release-v.01-e8c37a)](https://github.com/buntatoes/flux-ttf/releases/tag/v.01)
[![License: MPL-2.0](https://img.shields.io/badge/license-MPL--2.0-blue.svg)](./LICENSE)
[![Edition](https://img.shields.io/badge/rust-2024_edition-orange.svg)](https://github.com/buntatoes/flux-ttf)

`flux-ttf` is a standalone, zero-dependency Rust library that parses **TrueType** and **OpenType** (`.ttf` / `.otf`) SFNT font binaries to compute exact proportional glyph advance widths, horizontal kerning pairs, and vertical line metrics.

---

## Features

- **SFNT Table Directory Parser (`TtfFont::parse`)**:
  - Validates TrueType (`0x00010000`, `true`) and OpenType (`OTTO`) SFNT containers.
  - Parses `head` (`unitsPerEm`), `hhea` (`ascender`, `descender`, `lineGap`, `numberOfHMetrics`), `maxp` (`numGlyphs`), `cmap` (**Format 4** BMP segment mapping and **Format 12** full Unicode segmented coverage), `hmtx` (per-glyph advance widths and left side bearings), and `kern` (**Format 0** horizontal kerning pairs).
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

// Or load a system TrueType font when available
if let Some(font) = TtfFont::load_system_font(false, false) {
    let width_px = font.measure_text("Hello, Flux!", 18.0);
    assert!(width_px > 0.0);
}
```

---

## API Overview

| Function / Method | Description |
| :--- | :--- |
| `TtfFont::parse(&[u8]) -> Result<TtfFont, TtfError>` | Parses a `.ttf` or `.otf` binary buffer into a `TtfFont`. |
| `TtfFont::load_system_font(bold, monospace) -> Option<TtfFont>` | Loads a standard system TrueType font if available. |
| `TtfFont::glyph_id(char) -> u16` | Maps a Unicode scalar to its glyph index via `cmap`. |
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
