<div align="center">
  <h1>Anvil TTF</h1>
  <p>Clean-room TrueType and OpenType font parser, outline decoder, and coverage rasterizer in Go.</p>
  <p>
    <img src="https://img.shields.io/badge/release-v.11-e8c37a?style=flat-square" alt="v.11" />
    <img src="https://img.shields.io/badge/status-Proof_of_Concept-d4a359?style=flat-square" alt="Proof of Concept" />
    <img src="https://img.shields.io/badge/archive-Frozen_in_Time-8b949e?style=flat-square" alt="Archived and Frozen in Time" />
    <a href="./LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square" alt="Apache-2.0" /></a>
    <a href="./SECURITY.md"><img src="https://img.shields.io/badge/security-policy-2ea44f?style=flat-square" alt="Security" /></a>
    <img src="https://img.shields.io/badge/go-1.22%2B-00ADD8?style=flat-square" alt="Go 1.22+" />
  </p>
</div>

> [!IMPORTANT]
> **Archived & Frozen in Time**: This standalone repository is archived and frozen in time as a historical snapshot. Active development now lives in-tree inside [buntatoes/anvil-browser](https://github.com/buntatoes/anvil-browser) under `engines/go/anvil-ttf`.

Version 0.11.0 (`v.11`).

The browser uses this for text measurement and glyph rasterization. Written 100% from scratch in pure Go (`github.com/buntatoes/anvil-ttf`) with zero external dependencies and zero Cgo.

## What it does

- **SFNT Table Directory Parsing**: Validates TrueType (`0x00010000`, `true`) and OpenType CFF (`OTTO`) signatures.
- **Table Parsers**:
  - `head`: `unitsPerEm`, `indexToLocFormat` (short and long formats).
  - `hhea`: `ascender`, `descender`, `lineGap`, `numberOfHMetrics`.
  - `maxp`: `numGlyphs`.
  - `hmtx`: long metric records (`advanceWidth`, `leftSideBearing`) and truncated trailing bearing arrays.
  - `cmap`: Format 4 (segmented BMP) and Format 12 (segmented UCS-4 coverage).
  - `name`: Family name (nameID = 1) and full name fallback (nameID = 4) in big-endian UTF-16 and ASCII.
  - `kern`: Format 0 pairwise design-unit kerning adjustments.
  - `GPOS`: OpenType Glyph Positioning Lookup Type 2 (`PairPosFormat1` pair sets and `PairPosFormat2` class-based kerning) and Lookup Type 9 (Extension Positioning).
  - `loca` & `glyf`: TrueType outlines with simple contour decoding and compound composite glyphs with 2x2 affine matrix transformations.
  - `CFF `: Compact Font Format v1 container, Top DICT, Private DICT, Local/Global Subroutines (biases 107, 1131, 32768), and Type 2 CharString bytecode interpreter with all curve operators, stem hints, and hintmasks.
- **Geometry & Curve Flattening**:
  - Quadratic Bezier subdivision into linear polygon vertices.
  - Cubic Bezier subdivision into linear polygon vertices.
  - Contour flattening with implicit midpoint synthesis between consecutive off-curve points.
- **Rasterization & Styling**:
  - 4x4 supersampled coverage rasterizer (16 samples per pixel) with non-zero winding rule.
  - 12x4 (3x horizontal) LCD subpixel rasterizer with horizontal 3-tap FIR filter `[1, 2, 1] / 4` to suppress color fringing.
  - Vertical stem grid fitting (`snapVerticalGridY`) to snap baselines and horizontal crossbars to whole pixel rows at small sizes.
  - Horizontal subpixel phase positioning (4 quarter-pixel bins: 0.00px, 0.25px, 0.50px, 0.75px).
  - Synthetic bold (1px horizontal dilation + pen advance expansion) and synthetic italic (0.20 oblique shear).
  - Gamma-correct sRGB <-> 12-bit linear light LUT conversions and luminance-aware stem contrast enhancement.
- **CLI & Worker**:
  - Persistent binary IPC worker over standard I/O (`cmd/anvil-ttf --worker`) with 1-byte opcodes.
  - Inspection mode (`--inspect <font.ttf>`) and Netpbm PGM rendering (`--render <font> <char> <size> <out.pgm>`).
  - Internal self-test validation (`--self-test`).

## What it does not do

- It does not call FreeType, HarfBuzz, Go's `golang.org/x/image/font`, or any external font library.
- It does not execute TrueType bytecode hinting instructions or use a GPU.

## Security Bounds

| Limit | Value | Description |
|---|---|---|
| `MaxFontBytes` | `320 MiB` | Maximum raw SFNT binary file size |
| `MaxSfntTables` | `1,280` | Maximum SFNT directory table records |
| `MaxGlyphContours` | `2,560` | Maximum contours per glyph outline |
| `MaxContourPoints` | `40,960` | Maximum control points per glyph outline |
| `MaxCompositeGlyphDepth` | `64` | Maximum compound glyph recursion depth |
| `MaxGlyphBitmapSide` | `512 px` | Maximum rasterized bitmap width or height |
| `MaxGlyphOffset` | `2^24 px` | Maximum distance of bitmap corner from pen |

## Build

```
go vet ./...
go test -v ./...
go build ./...
```

Target: Go 1.22 or newer (`CGO_ENABLED=0`). Zero external dependencies in `go.mod`.

## The other repositories

The browser is [anvil-browser](https://github.com/buntatoes/anvil-browser) and the page engine is [anvil-forge](https://github.com/buntatoes/anvil-forge), built alongside the nine companion libraries below.

| Repository | What it is |
| --- | --- |
| [anvil-cipher](https://github.com/buntatoes/anvil-cipher) | AES-128, AES-256, CTR, AES-256-GCM, SHA-256, HMAC-SHA256, HKDF-SHA256 |
| [anvil-compress](https://github.com/buntatoes/anvil-compress) | DEFLATE, zlib, gzip (Go) |
| [anvil-hpack](https://github.com/buntatoes/anvil-hpack) | HPACK header compression for HTTP/2 (Go) |
| [anvil-image](https://github.com/buntatoes/anvil-image) | PNG, JPEG, GIF, WebP (`VP8L` + `VP8 `), ICO, BMP, PPM |
| [anvil-json](https://github.com/buntatoes/anvil-json) | JSON & RFC 6901 JSON Pointer (Go) |
| [anvil-pqc](https://github.com/buntatoes/anvil-pqc) | ML-KEM-768, X25519, X25519MLKEM768, SHA-3 |
| [anvil-ttf](https://github.com/buntatoes/anvil-ttf) | TrueType and OpenType (`glyf`, `CFF `, `kern`, `GPOS`) |
| [anvil-gpu](https://github.com/buntatoes/anvil-gpu) | Compositor passes for boxes, shadows, gradients, and NV12/I420, on the CPU or a GPU |
| [anvil-video](https://github.com/buntatoes/anvil-video) | CPU frames (AVID/ADCT) and PCM/ADPCM/FLAC/MDCT audio in MP4, fMP4, and WebM containers |

## License

Licensed under the **Apache License, Version 2.0** (`Apache-2.0`). Copyright (c) 2026 Buntato. Pursuant to Section 6 of the Apache License 2.0, the names **"Anvil"**, **"Anvil Browser"**, **"Anvil Forge"** (**"Forge"**), **"Cinder"**, **"Prism"**, and the Anvil brand logo/emblem are copyrighted and trademarked by **Buntato** (*All Rights Reserved*) and may not be used to brand derivative works without written permission. See [LICENSE](./LICENSE).
