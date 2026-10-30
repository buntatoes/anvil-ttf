// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! TrueType and OpenType SFNT parser, glyph outlines, and an 8-bit coverage rasterizer.
//!
//! Tables read from the font are `name`, `head`, `hhea`, `maxp`, `cmap`, `hmtx`,
//! optional `kern` format 0, optional OpenType `GPOS` Lookup Type 2 (`PairPosFormat1`
//! and `PairPosFormat2`), and optional `loca` plus `glyf`. Multi-byte values
//! and directory offsets are big-endian. Character codes become glyph indices
//! through cmap format 4 (Unicode BMP segments) and format 12 (UCS-4 groups).
//! Other cmap formats are ignored. `head.unitsPerEm` is the design-unit size of
//! one em; pixel metrics divide by that value.
//!
//! Simple `glyf` glyphs decode on-curve points and quadratic off-curve points.
//! Compound glyphs follow component indices, apply an XY offset when the
//! argument flags say the arguments are offsets, and stop past
//! [`MAX_COMPOSITE_GLYPH_DEPTH`]. Scales on compound components are not applied.
//! Off-curve points are flattened to a polygon. The polygon is filled with a
//! nonzero winding rule and 4 by 4 samples per pixel. Each bitmap byte is
//! coverage from 0 (empty) to 255 (solid), row 0 at the top. Font y grows up
//! and is negated before the scan, so increasing the row index moves down.
//!
//! anvil-layout uses the metric helpers to measure text. anvil-raster on Linux
//! calls [`TtfFont::parse`] when a system `.ttf` is found, then
//! [`TtfFont::rasterize_glyph`]. If that fails, the browser draws a built-in
//! bitmap font instead. This crate does not shape complex scripts, hint
//! outlines, decode CFF glyphs, or use a GPU.
//!
//! File size, table count, contour count, and compound-glyph depth are capped.

use std::collections::HashMap;
use std::fmt;

/// Maximum font binary size accepted (`32 MiB`).
pub const MAX_FONT_BYTES: usize = 320 * 1024 * 1024;

/// Maximum number of SFNT directory tables accepted (`128`).
pub const MAX_SFNT_TABLES: usize = 1280;

/// Maximum contours per glyph outline (`256`).
pub const MAX_GLYPH_CONTOURS: usize = 2560;

/// Maximum control points per glyph outline (`4,096`).
pub const MAX_CONTOUR_POINTS: usize = 40960;

/// Maximum compound glyph recursion depth (`16`).
pub const MAX_COMPOSITE_GLYPH_DEPTH: u8 = 64;

/// Failure while reading an SFNT font.
///
/// A short read is [`TtfError::UnexpectedEof`]. Files larger than
/// [`MAX_FONT_BYTES`], and directories with more than [`MAX_SFNT_TABLES`]
/// records, are rejected before table bodies are parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TtfError {
    /// The read ran past the file or the current table.
    UnexpectedEof,
    /// `size` is the file length that exceeded [`MAX_FONT_BYTES`].
    FontTooLarge { size: usize },
    /// `count` is `numTables` from the offset table, above [`MAX_SFNT_TABLES`].
    TooManyTables { count: usize },
    /// The first four bytes were not `0x00010000`, `0x74727565` (`true`), or `0x4F54544F` (`OTTO`).
    InvalidSfntSignature,
    /// A table required for metrics (`head`, `hhea`, `maxp`, `hmtx`, or `cmap`) was absent.
    MissingTable(&'static str),
    /// A required table was present but its length or a field was unusable.
    InvalidTable(&'static str),
}

impl fmt::Display for TtfError {
    /// Formats the error as a short sentence, including the table tag when there is one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "unexpected end of font file"),
            Self::FontTooLarge { size } => {
                write!(
                    f,
                    "font size {size} exceeds security limit ({MAX_FONT_BYTES})"
                )
            }
            Self::TooManyTables { count } => {
                write!(
                    f,
                    "SFNT table count {count} exceeds security limit ({MAX_SFNT_TABLES})"
                )
            }
            Self::InvalidSfntSignature => write!(f, "unrecognized SFNT font signature"),
            Self::MissingTable(tag) => write!(f, "missing required SFNT table '{tag}'"),
            Self::InvalidTable(tag) => write!(f, "invalid SFNT table '{tag}'"),
        }
    }
}

impl std::error::Error for TtfError {}

/// Horizontal metric for one glyph, in font design units.
///
/// Design units are `units_per_em` from `head`. There are that many units in
/// one em at the requested pixel size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HMetric {
    /// Advance width from `hmtx`. The pen moves this far before kerning.
    pub advance_width: u16,
    /// Left side bearing from `hmtx`. May be negative.
    pub left_side_bearing: i16,
}

/// Standard generic font family and style slots loaded by the browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontFamilySlot {
    /// Proportional sans-serif regular (`Segoe UI` / `DejaVu Sans`).
    SansRegular,
    /// Proportional sans-serif bold (`Segoe UI Bold` / `DejaVu Sans Bold`).
    SansBold,
    /// Proportional sans-serif italic (`Segoe UI Italic` / `DejaVu Sans Oblique`).
    SansItalic,
    /// Proportional serif regular (`Georgia` / `Times New Roman` / `DejaVu Serif`).
    SerifRegular,
    /// Proportional serif bold (`Georgia Bold` / `Times New Roman Bold` / `DejaVu Serif Bold`).
    SerifBold,
    /// Fixed-width monospace regular (`Consolas` / `DejaVu Sans Mono`).
    MonospaceRegular,
    /// Fixed-width monospace bold (`Consolas Bold` / `DejaVu Sans Mono Bold`).
    MonospaceBold,
}

/// Curve order for an outline point when flattening quadratic or cubic Bezier contours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveOrder {
    /// On-curve vertex.
    OnCurve,
    /// Off-curve quadratic Bezier control point (`glyf`).
    QuadraticOffCurve,
    /// Off-curve cubic Bezier control point.
    CubicOffCurve,
}

/// One point on a glyph contour, in font design units.
///
/// `y` increases upward, which is the `glyf` convention. The rasterizer negates
/// it when it builds a bitmap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutlinePoint {
    /// X in font units. Positive is to the right of the glyph origin.
    pub x: i16,
    /// Y in font units. Positive is above the baseline.
    pub y: i16,
    /// True when the point lies on the contour. False for a quadratic control point.
    pub on_curve: bool,
}

/// Decoded `glyf` outline for one glyph, in font design units.
///
/// The bounding box is the box stored in the glyph header. It is not recomputed
/// from [`contours`](Self::contours). Each contour is closed. Compound
/// components are already appended and shifted by their XY offsets. Component
/// scales and point-to-point attachment are not applied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlyphOutline {
    /// Header `xMin`, in font units.
    pub x_min: i16,
    /// Header `yMin`, in font units. Font y increases upward.
    pub y_min: i16,
    /// Header `xMax`, in font units.
    pub x_max: i16,
    /// Header `yMax`, in font units.
    pub y_max: i16,
    /// Closed contours. Empty when the glyph has no drawn points.
    pub contours: Vec<Vec<OutlinePoint>>,
}

/// 8-bit coverage bitmap for one rasterized glyph.
///
/// [`coverage`](Self::coverage) is row-major and holds `width * height` bytes.
/// A byte is coverage, not a color: 0 is empty and 255 is fully covered. Row 0
/// is the top of the glyph. Font y increases upward and is multiplied by
/// `-scale` before scan conversion, so a larger row index is further down.
///
/// [`bearing_x`](Self::bearing_x) is the bitmap's left edge relative to the
/// pen. [`bearing_y`](Self::bearing_y) is how many pixels row 0 sits above the
/// baseline. When destination y increases downward, row 0 is drawn at
/// `baseline_y - bearing_y`. When destination y increases upward, row 0 is at
/// `baseline_y + bearing_y` and later rows step down.
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphBitmap {
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels. Row 0 is the top.
    pub height: u32,
    /// Left edge relative to the glyph origin, in pixels. May be negative.
    pub bearing_x: i32,
    /// Distance from the baseline up to row 0, in pixels.
    pub bearing_y: i32,
    /// Horizontal pen advance at this pixel size. Kerning is not included.
    pub advance_px: f32,
    /// Row-major coverage, length `width * height`. 0 is empty, 255 is solid.
    pub coverage: Vec<u8>,
}

/// 3x horizontal RGB subpixel LCD coverage bitmap for one rasterized glyph.
///
/// [`rgb_coverage`](Self::rgb_coverage) is row-major and holds `width * height * 3`
/// bytes (`R`, `G`, `B` coverage in `0..=255` for each pixel) after a 3-tap
/// `[1, 2, 1] / 4` horizontal FIR filter to suppress color fringing.
#[derive(Clone, Debug, PartialEq)]
pub struct SubpixelGlyphBitmap {
    /// Bitmap width in whole pixels.
    pub width: u32,
    /// Bitmap height in pixels. Row 0 is the top.
    pub height: u32,
    /// Left edge relative to the glyph origin, in pixels. May be negative.
    pub bearing_x: i32,
    /// Distance from the baseline up to row 0, in pixels.
    pub bearing_y: i32,
    /// Horizontal pen advance at this pixel size. Kerning is not included.
    pub advance_px: f32,
    /// Row-major RGB subpixel coverage (`width * height * 3` bytes).
    pub rgb_coverage: Vec<u8>,
}

impl OutlinePoint {
    /// Create an on-curve contour vertex.
    pub const fn on_curve(x: i16, y: i16) -> Self {
        Self {
            x,
            y,
            on_curve: true,
        }
    }

    /// Create an off-curve quadratic control point.
    pub const fn quadratic(x: i16, y: i16) -> Self {
        Self {
            x,
            y,
            on_curve: false,
        }
    }

    /// Create a cubic control point tuple `(x, y, CurveOrder::CubicOffCurve)`.
    pub const fn cubic(x: i16, y: i16) -> (i16, i16, CurveOrder) {
        (x, y, CurveOrder::CubicOffCurve)
    }
}

/// Parsed TrueType or OpenType font.
///
/// [`units_per_em`](Self::units_per_em), [`ascender`](Self::ascender),
/// [`descender`](Self::descender), and [`line_gap`](Self::line_gap) are design
/// units from `head` and `hhea`. Pixel sizes divide by `units_per_em`. Glyph
/// indices come from cmap formats 4 and 12. A character with no entry uses
/// glyph 0.
///
/// `kern` format 0 pairs and `glyf` outlines are kept when those tables exist.
/// An `OTTO` font can still supply metrics. Its CFF outlines are not decoded,
/// so glyph bitmaps from such a file are missing.
#[derive(Clone, Debug)]
pub struct TtfFont {
    /// `head.unitsPerEm`. A successful parse never leaves this at zero.
    pub units_per_em: u16,
    /// `hhea.ascender`, in font units. Usually positive.
    pub ascender: i16,
    /// `hhea.descender`, in font units. Usually negative.
    pub descender: i16,
    /// `hhea.lineGap`, in font units. May be zero.
    pub line_gap: i16,
    /// Name ID 1, or name ID 4 if the family name was absent. Empty if neither decoded.
    family_name: String,
    /// Unicode scalar value to glyph index. Glyph index 0 is never stored.
    cmap: HashMap<u32, u16>,
    /// One [`HMetric`] per glyph index, from `hmtx`.
    metrics: Vec<HMetric>,
    /// `(left_glyph, right_glyph)` to a design-unit adjustment from `kern` format 0.
    kern_pairs: HashMap<(u16, u16), i16>,
    /// Parallel to glyph index. [`None`] if `loca`/`glyf` had no outline there.
    outlines: Vec<Option<GlyphOutline>>,
}

impl TtfFont {
    /// Parse an SFNT font from `bytes`.
    ///
    /// The offset table is 12 bytes, then one 16-byte record per table: tag,
    /// checksum, offset, length. Checksums are not verified. `head`, `hhea`,
    /// `maxp`, `hmtx`, and `cmap` are required. `name`, `kern`, `loca`, and
    /// `glyf` are optional. A bad optional table becomes an empty name, an empty
    /// kern map, or missing outlines, and metrics still return. `0x00010000`
    /// and `true` mark TrueType outlines. `OTTO` is accepted for its metrics;
    /// CFF glyph data is not decoded.
    pub fn parse(bytes: &[u8]) -> Result<Self, TtfError> {
        if bytes.len() > MAX_FONT_BYTES {
            return Err(TtfError::FontTooLarge { size: bytes.len() });
        }
        if bytes.len() < 12 {
            return Err(TtfError::UnexpectedEof);
        }
        let sfnt_version = read_u32_be(bytes, 0)?;
        // 0x00010000 and 'true' (0x74727565) are TrueType. 'OTTO' (0x4F54544F) is CFF.
        if sfnt_version != 0x0001_0000 && sfnt_version != 0x7472_7565 && sfnt_version != 0x4F54_544F
        {
            return Err(TtfError::InvalidSfntSignature);
        }

        let num_tables = read_u16_be(bytes, 4)? as usize;
        if num_tables > MAX_SFNT_TABLES {
            return Err(TtfError::TooManyTables { count: num_tables });
        }
        // Directory bytes: 12-byte offset table plus 16 bytes per record.
        if 12 + num_tables * 16 > bytes.len() {
            return Err(TtfError::UnexpectedEof);
        }

        let mut tables: HashMap<[u8; 4], (usize, usize)> = HashMap::new();
        for i in 0..num_tables {
            let rec = 12 + i * 16;
            let tag = [bytes[rec], bytes[rec + 1], bytes[rec + 2], bytes[rec + 3]];
            // Checksum at rec + 4 is skipped. Offset and length are big-endian u32.
            let offset = read_u32_be(bytes, rec + 8)? as usize;
            let length = read_u32_be(bytes, rec + 12)? as usize;
            // saturating_add rejects a length that would wrap past the end of the file.
            if offset.saturating_add(length) > bytes.len() {
                return Err(TtfError::UnexpectedEof);
            }
            tables.insert(tag, (offset, length));
        }

        let head = slice_table(bytes, &tables, b"head", "head")?;
        if head.len() < 54 {
            return Err(TtfError::InvalidTable("head"));
        }
        // unitsPerEm is the u16 at byte 18. Zero cannot scale design units to pixels.
        let units_per_em = read_u16_be(head, 18)?;
        if units_per_em == 0 {
            return Err(TtfError::InvalidTable("head"));
        }
        // indexToLocFormat at byte 50: 0 is short loca (u16 words). Any other value is long (u32 bytes).
        let index_to_loc_format = read_i16_be(head, 50)?;

        let hhea = slice_table(bytes, &tables, b"hhea", "hhea")?;
        if hhea.len() < 36 {
            return Err(TtfError::InvalidTable("hhea"));
        }
        // ascender, descender, and lineGap are i16. numberOfHMetrics is the u16 at byte 34.
        let ascender = read_i16_be(hhea, 4)?;
        let descender = read_i16_be(hhea, 6)?;
        let line_gap = read_i16_be(hhea, 8)?;
        let num_h_metrics = read_u16_be(hhea, 34)? as usize;

        let maxp = slice_table(bytes, &tables, b"maxp", "maxp")?;
        if maxp.len() < 6 {
            return Err(TtfError::InvalidTable("maxp"));
        }
        // numGlyphs follows the 4-byte maxp version.
        let num_glyphs = read_u16_be(maxp, 4)? as usize;

        let hmtx = slice_table(bytes, &tables, b"hmtx", "hmtx")?;
        let metrics = parse_hmtx(hmtx, num_h_metrics, num_glyphs)?;

        let cmap_table = slice_table(bytes, &tables, b"cmap", "cmap")?;
        let cmap = parse_cmap(cmap_table)?;

        // Optional. A missing or unreadable kern table leaves every pair at 0.
        let mut kern_pairs = if let Some(&(off, len)) = tables.get(b"kern") {
            parse_kern(&bytes[off..off + len]).unwrap_or_default()
        } else {
            HashMap::new()
        };
        if let Some(&(off, len)) = tables.get(b"GPOS") {
            parse_gpos(&bytes[off..off + len], &mut kern_pairs);
        }

        // Optional. Failure here is an empty family name, not a parse error.
        let family_name = if let Some(&(off, len)) = tables.get(b"name") {
            parse_name_table(&bytes[off..off + len]).unwrap_or_default()
        } else {
            String::new()
        };

        // Both loca and glyf are required for outlines. A decode error drops every outline.
        let outlines = if let (Some(&(loca_off, loca_len)), Some(&(glyf_off, glyf_len))) =
            (tables.get(b"loca"), tables.get(b"glyf"))
        {
            let loca_slice = &bytes[loca_off..loca_off + loca_len];
            let glyf_slice = &bytes[glyf_off..glyf_off + glyf_len];
            parse_glyf_table(loca_slice, glyf_slice, num_glyphs, index_to_loc_format)
                .unwrap_or_else(|_| vec![None; num_glyphs])
        } else if let Some(&(cff_off, cff_len)) = tables.get(b"CFF ") {
            parse_cff_table(&bytes[cff_off..cff_off + cff_len], num_glyphs)
                .unwrap_or_else(|_| vec![None; num_glyphs])
        } else {
            vec![None; num_glyphs]
        };

        Ok(Self {
            units_per_em,
            ascender,
            descender,
            line_gap,
            family_name,
            cmap,
            metrics,
            kern_pairs,
            outlines,
        })
    }

    /// Family name from the `name` table.
    ///
    /// Name ID 1 (font family) wins. Name ID 4 (full name) is used only when no
    /// family string was decoded. A missing table returns an empty string.
    pub fn family_name(&self) -> &str {
        &self.family_name
    }

    /// Load Segoe UI or Consolas from the Windows fonts directory.
    ///
    /// `monospace` selects Consolas (`consola.ttf`, or `consolab.ttf` when
    /// `bold`). Otherwise the file is Segoe UI (`segoeui.ttf`, or `segoeuib.ttf`
    /// when `bold`). The directory is `Fonts` under the `WINDIR` environment
    /// variable. Returns [`None`] when that variable is unset, the file cannot
    /// be read, or [`TtfFont::parse`] fails. anvil-layout calls this for
    /// proportional measurement. Linux glyph drawing does not; it reads a
    /// `.ttf` itself and calls [`TtfFont::parse`].
    pub fn load_system_font(bold: bool, monospace: bool) -> Option<Self> {
        let filename = match (monospace, bold) {
            (true, true) => "consolab.ttf",
            (true, false) => "consola.ttf",
            (false, true) => "segoeuib.ttf",
            (false, false) => "segoeui.ttf",
        };
        let windir = std::env::var_os("WINDIR")?;
        let path = std::path::PathBuf::from(windir)
            .join("Fonts")
            .join(filename);
        let bytes = std::fs::read(path).ok()?;
        Self::parse(&bytes).ok()
    }

    /// Load a specific [`FontFamilySlot`] from the host operating system's standard font directories.
    pub fn load_system_font_slot(slot: FontFamilySlot) -> Option<Self> {
        let win_candidates: &[&str] = match slot {
            FontFamilySlot::SansRegular => &["segoeui.ttf", "arial.ttf"],
            FontFamilySlot::SansBold => &["segoeuib.ttf", "arialbd.ttf", "segoeui.ttf"],
            FontFamilySlot::SansItalic => &["segoeuii.ttf", "ariali.ttf", "segoeui.ttf"],
            FontFamilySlot::SerifRegular => &["georgia.ttf", "times.ttf", "segoeui.ttf"],
            FontFamilySlot::SerifBold => {
                &["georgiab.ttf", "timesbd.ttf", "georgia.ttf", "segoeuib.ttf"]
            }
            FontFamilySlot::MonospaceRegular => &["consola.ttf", "cour.ttf", "segoeui.ttf"],
            FontFamilySlot::MonospaceBold => {
                &["consolab.ttf", "courbd.ttf", "consola.ttf", "segoeuib.ttf"]
            }
        };
        if let Some(windir) = std::env::var_os("WINDIR") {
            let fonts_dir = std::path::PathBuf::from(windir).join("Fonts");
            for name in win_candidates {
                if let Ok(bytes) = std::fs::read(fonts_dir.join(name))
                    && let Ok(font) = Self::parse(&bytes)
                {
                    return Some(font);
                }
            }
        }
        let unix_candidates: &[&str] = match slot {
            FontFamilySlot::SansRegular => &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                "/usr/share/fonts/TTF/DejaVuSans.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            ],
            FontFamilySlot::SansBold => &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
                "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            ],
            FontFamilySlot::SansItalic => &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Oblique.ttf",
                "/usr/share/fonts/TTF/DejaVuSans-Oblique.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationSans-Italic.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            ],
            FontFamilySlot::SerifRegular => &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
                "/usr/share/fonts/TTF/DejaVuSerif.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            ],
            FontFamilySlot::SerifBold => &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSerif-Bold.ttf",
                "/usr/share/fonts/TTF/DejaVuSerif-Bold.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationSerif-Bold.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            ],
            FontFamilySlot::MonospaceRegular => &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
                "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            ],
            FontFamilySlot::MonospaceBold => &[
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
                "/usr/share/fonts/TTF/DejaVuSansMono-Bold.ttf",
                "/usr/share/fonts/truetype/liberation/LiberationMono-Bold.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            ],
        };
        for path in unix_candidates {
            if let Ok(bytes) = std::fs::read(path)
                && let Ok(font) = Self::parse(&bytes)
            {
                return Some(font);
            }
        }
        None
    }

    /// Glyph index for the Unicode scalar value `ch`.
    ///
    /// The index comes from cmap format 4 or 12. A code that is missing, or
    /// that those tables map to glyph 0, returns 0 (`.notdef`). The map stores
    /// the scalar value, not UTF-16 code units.
    pub fn glyph_id(&self, ch: char) -> u16 {
        self.cmap.get(&(ch as u32)).copied().unwrap_or(0)
    }

    /// Horizontal metric for `glyph_id`, in font units.
    ///
    /// An index past the end of `hmtx` uses the last metric. If no metric rows
    /// were stored, the advance is half of [`TtfFont::units_per_em`] and the
    /// side bearing is 0.
    pub fn glyph_metric(&self, glyph_id: u16) -> HMetric {
        self.metrics
            .get(glyph_id as usize)
            .copied()
            .or_else(|| self.metrics.last().copied())
            .unwrap_or(HMetric {
                advance_width: self.units_per_em / 2,
                left_side_bearing: 0,
            })
    }

    /// Kerning adjustment for a glyph pair, in font units.
    ///
    /// The value comes from `kern` format 0 and may be negative. The key order
    /// is `(left_glyph, right_glyph)`. A missing pair, or a font with no usable
    /// `kern` table, returns 0.
    pub fn kerning_units(&self, left_glyph: u16, right_glyph: u16) -> i16 {
        self.kern_pairs
            .get(&(left_glyph, right_glyph))
            .copied()
            .unwrap_or(0)
    }

    /// Pixel advance of `ch` at `font_size_px`, without kerning.
    ///
    /// The width is `advanceWidth * font_size_px / units_per_em`.
    pub fn glyph_advance_px(&self, ch: char, font_size_px: f32) -> f32 {
        let gid = self.glyph_id(ch);
        let m = self.glyph_metric(gid);
        (m.advance_width as f32) * font_size_px / (self.units_per_em as f32)
    }

    /// Width of `text` in pixels at `font_size_px`, including pairwise kerning.
    ///
    /// Each character adds its `hmtx` advance. Between two glyphs the `kern`
    /// value is added first. The design-unit sum is clamped at zero, then
    /// multiplied by `font_size_px / units_per_em`. anvil-layout uses this when
    /// [`TtfFont::load_system_font`] succeeds.
    pub fn measure_text(&self, text: &str, font_size_px: f32) -> f32 {
        let scale = font_size_px / (self.units_per_em as f32);
        let mut total_units: i32 = 0;
        let mut prev_gid: Option<u16> = None;

        for ch in text.chars() {
            let gid = self.glyph_id(ch);
            if let Some(prev) = prev_gid {
                total_units += i32::from(self.kerning_units(prev, gid));
            }
            total_units += i32::from(self.glyph_metric(gid).advance_width);
            prev_gid = Some(gid);
        }
        (total_units.max(0) as f32) * scale
    }

    /// Recommended line height in pixels at `font_size_px`.
    ///
    /// The design-unit height is `ascender - descender + line_gap`. `descender`
    /// is usually negative, so subtracting it adds the descent. The height is
    /// at least one em before it is scaled by `font_size_px / units_per_em`.
    pub fn line_height_px(&self, font_size_px: f32) -> f32 {
        let units = i32::from(self.ascender) - i32::from(self.descender) + i32::from(self.line_gap);
        (units.max(i32::from(self.units_per_em)) as f32) * font_size_px / (self.units_per_em as f32)
    }

    /// Outline for `glyph_id`, if `glyf` stored a non-empty glyph at that index.
    ///
    /// [`None`] means the index is out of range, the `loca` span was empty, or
    /// the glyph failed to decode. Glyph 0 is included when the font defined it.
    pub fn glyph_outline(&self, glyph_id: u16) -> Option<&GlyphOutline> {
        self.outlines
            .get(glyph_id as usize)
            .and_then(|o| o.as_ref())
    }

    /// Rasterize `ch` at `font_size_px` into an 8-bit coverage bitmap.
    ///
    /// Returns [`None`] when the character has no contours, when `font_size_px`
    /// is NaN or infinite, or when the bitmap would be empty, larger than 512
    /// pixels on a side, or more than 2^24 pixels from the pen. The outline is scaled
    /// by `max(font_size_px, 1) / max(units_per_em, 1)` and filled with nonzero
    /// winding at 4 by 4 samples per pixel. See [`GlyphBitmap`] for coverage
    /// and y direction. anvil-raster uses this for Linux glyphs.
    pub fn rasterize_glyph(&self, ch: char, font_size_px: f32) -> Option<GlyphBitmap> {
        if !font_size_px.is_finite() {
            return None;
        }
        let gid = self.glyph_id(ch);
        let outline = self.glyph_outline(gid)?;
        if outline.contours.is_empty() {
            return None;
        }
        let scale = font_size_px.max(1.0) / (self.units_per_em.max(1) as f32);
        let advance_px = (self.glyph_metric(gid).advance_width as f32) * scale;
        rasterize_outline(outline, scale, advance_px)
    }

    /// Rasterize `ch` at `font_size_px` into a 3x horizontal RGB subpixel LCD coverage bitmap.
    ///
    /// Samples 12 by 4 subpixels per pixel (4 by 4 for each R, G, and B subpixel column)
    /// and applies a 3-tap `[1, 2, 1] / 4` horizontal FIR filter across subpixel columns.
    pub fn rasterize_glyph_subpixel_lcd(
        &self,
        ch: char,
        font_size_px: f32,
    ) -> Option<SubpixelGlyphBitmap> {
        let _x_offset_px = 0.0f32;
        let _grid_fit = true;
        if !font_size_px.is_finite() {
            return None;
        }
        let gid = self.glyph_id(ch);
        let outline = self.glyph_outline(gid)?;
        if outline.contours.is_empty() {
            return None;
        }
        let scale = font_size_px.max(1.0) / (self.units_per_em.max(1) as f32);
        let advance_px = (self.glyph_metric(gid).advance_width as f32) * scale;
        rasterize_outline_subpixel_lcd(outline, scale, advance_px)
    }

    /// Rasterize `ch` at `font_size_px` with a horizontal subpixel phase (`0..4`, representing
    /// `0.0`, `0.25`, `0.50`, or `0.75` px fractional X offset) and vertical stem grid fitting.
    pub fn rasterize_glyph_subpixel_positioned(
        &self,
        ch: char,
        font_size_px: f32,
        subpixel_x_phase: u8,
    ) -> Option<GlyphBitmap> {
        if !font_size_px.is_finite() {
            return None;
        }
        let gid = self.glyph_id(ch);
        let outline = self.glyph_outline(gid)?;
        if outline.contours.is_empty() {
            return None;
        }
        let scale = font_size_px.max(1.0) / (self.units_per_em.max(1) as f32);
        let advance_px = (self.glyph_metric(gid).advance_width as f32) * scale;
        let phase_offset = (subpixel_x_phase & 3) as f32 * 0.25;
        rasterize_outline_with_phase(outline, scale, advance_px, phase_offset, true)
    }

    /// Rasterize `ch` at `font_size_px` with horizontal subpixel phase (`0..4`), optional
    /// synthetic stem emboldening (`synthetic_bold`), and optional `0.20` oblique shear (`synthetic_italic`).
    pub fn rasterize_glyph_styled(
        &self,
        ch: char,
        font_size_px: f32,
        subpixel_x_phase: u8,
        synthetic_bold: bool,
        synthetic_italic: bool,
    ) -> Option<GlyphBitmap> {
        if !font_size_px.is_finite() {
            return None;
        }
        let gid = self.glyph_id(ch);
        let outline = self.glyph_outline(gid)?;
        if outline.contours.is_empty() {
            return None;
        }
        let scale = font_size_px.max(1.0) / (self.units_per_em.max(1) as f32);
        let advance_px = (self.glyph_metric(gid).advance_width as f32) * scale;
        let phase_offset = (subpixel_x_phase & 3) as f32 * 0.25;
        let effective_outline: std::borrow::Cow<'_, GlyphOutline> = if synthetic_italic {
            let mut sheared = outline.clone();
            for contour in &mut sheared.contours {
                for pt in contour {
                    let slant = ((pt.y as f32) * 0.20).round() as i16;
                    pt.x = pt.x.saturating_add(slant);
                }
            }
            std::borrow::Cow::Owned(sheared)
        } else {
            std::borrow::Cow::Borrowed(outline)
        };
        let mut bmp = rasterize_outline_with_phase(
            &effective_outline,
            scale,
            advance_px,
            phase_offset,
            true,
        )?;
        if synthetic_bold && bmp.width > 0 && bmp.height > 0 && bmp.width < 511 {
            let old_w = bmp.width as usize;
            let new_w = old_w + 1;
            let h = bmp.height as usize;
            let mut bold_cov = vec![0u8; new_w * h];
            for row in 0..h {
                for col in 0..new_w {
                    let c0 = if col < old_w {
                        bmp.coverage[row * old_w + col]
                    } else {
                        0
                    };
                    let c1 = if col > 0 {
                        ((u16::from(bmp.coverage[row * old_w + (col - 1)]) * 210) / 255) as u8
                    } else {
                        0
                    };
                    bold_cov[row * new_w + col] = c0.max(c1);
                }
            }
            bmp.width = new_w as u32;
            bmp.advance_px += (font_size_px * 0.04).max(0.5);
            bmp.coverage = bold_cov;
        }
        Some(bmp)
    }
}

/// Estimate a string width from fixed em ratios when no `.ttf` is loaded.
///
/// Each character adds `font_size_px` times a ratio: space is 0.30, narrow marks
/// and `i` / `l` are 0.28, `I` / `j` / `f` / `t` / `r` and brackets are 0.36,
/// `m` / `w` / `M` / `W` / `@` / `%` are 0.82, other capitals are 0.66, digits
/// are 0.56, and every other scalar is 0.54. There is no kerning. anvil-layout
/// uses this when [`TtfFont::load_system_font`] returns [`None`].
pub fn measure_proportional_fallback(text: &str, font_size_px: f32) -> f32 {
    let mut width = 0.0f32;
    for ch in text.chars() {
        let ratio = match ch {
            ' ' => 0.30,
            'i' | 'l' | '!' | '|' | '.' | ',' | ':' | ';' | '\'' => 0.28,
            'I' | 'j' | 'f' | 't' | 'r' | '(' | ')' | '[' | ']' => 0.36,
            'm' | 'w' | 'M' | 'W' | '@' | '%' => 0.82,
            'A'..='Z' => 0.66,
            '0'..='9' => 0.56,
            _ => 0.54,
        };
        width += font_size_px * ratio;
    }
    width
}

/// Return the byte range recorded for `tag`.
///
/// `tables` maps a 4-byte tag to `(offset, length)` from the SFNT directory.
/// `name` is the label stored in [`TtfError::MissingTable`]. The range was
/// already checked to lie inside `bytes`.
fn slice_table<'a>(
    bytes: &'a [u8],
    tables: &HashMap<[u8; 4], (usize, usize)>,
    tag: &[u8; 4],
    name: &'static str,
) -> Result<&'a [u8], TtfError> {
    let &(offset, length) = tables.get(tag).ok_or(TtfError::MissingTable(name))?;
    Ok(&bytes[offset..offset + length])
}

/// Decode `hmtx` into one [`HMetric`] per glyph.
///
/// The first `num_h_metrics` records are 4 bytes: advance `u16`, then left side
/// bearing `i16`, both big-endian. Every later glyph is a 2-byte side bearing
/// only and reuses the last advance. A missing trailing bearing is stored as 0.
/// `num_h_metrics` of 0, or a table shorter than those long records, is an
/// error. The result length is `max(num_glyphs, num_h_metrics)`.
fn parse_hmtx(
    hmtx: &[u8],
    num_h_metrics: usize,
    num_glyphs: usize,
) -> Result<Vec<HMetric>, TtfError> {
    if num_h_metrics == 0 || hmtx.len() < num_h_metrics * 4 {
        return Err(TtfError::InvalidTable("hmtx"));
    }
    let total = num_glyphs.max(num_h_metrics);
    let mut out = Vec::with_capacity(total);
    for i in 0..num_h_metrics {
        let base = i * 4;
        out.push(HMetric {
            advance_width: read_u16_be(hmtx, base)?,
            left_side_bearing: read_i16_be(hmtx, base + 2)?,
        });
    }
    // Glyphs past numberOfHMetrics repeat this advance and store only an lsb.
    let last_advance = out.last().map(|m| m.advance_width).unwrap_or(0);
    let extra_start = num_h_metrics * 4;
    for i in 0..(total - num_h_metrics) {
        let lsb_off = extra_start + i * 2;
        let lsb = if lsb_off + 2 <= hmtx.len() {
            read_i16_be(hmtx, lsb_off)?
        } else {
            0
        };
        out.push(HMetric {
            advance_width: last_advance,
            left_side_bearing: lsb,
        });
    }
    Ok(out)
}

/// Build a Unicode scalar to glyph-index map from cmap subtables.
///
/// The header is version `u16` and subtable count `u16`. Each encoding record
/// is 8 bytes: platform, encoding, then a big-endian byte offset to the
/// subtable. Platform 0 (Unicode) is accepted. Platform 3 is accepted only for
/// encoding 1 (Windows BMP) and encoding 10 (Windows UCS-4). Other records are
/// skipped. Format 4 and format 12 are read. Any other format is ignored. A
/// broken subtable is skipped so a later one can still fill the map. When two
/// subtables name the same scalar, the later glyph index replaces the earlier
/// one. Glyph index 0 is not stored.
fn parse_cmap(cmap: &[u8]) -> Result<HashMap<u32, u16>, TtfError> {
    if cmap.len() < 4 {
        return Err(TtfError::InvalidTable("cmap"));
    }
    let num_subtables = read_u16_be(cmap, 2)? as usize;
    if 4 + num_subtables * 8 > cmap.len() {
        return Err(TtfError::InvalidTable("cmap"));
    }

    let mut map = HashMap::new();
    for i in 0..num_subtables {
        let rec = 4 + i * 8;
        let platform_id = read_u16_be(cmap, rec)?;
        let encoding_id = read_u16_be(cmap, rec + 2)?;
        let sub_offset = read_u32_be(cmap, rec + 4)? as usize;
        if sub_offset + 4 > cmap.len() {
            continue;
        }
        // Platform 0: Unicode. Platform 3 encoding 1: BMP. Encoding 10: full Unicode.
        if platform_id != 0 && !(platform_id == 3 && (encoding_id == 1 || encoding_id == 10)) {
            continue;
        }
        let sub = &cmap[sub_offset..];
        let format = read_u16_be(sub, 0)?;
        match format {
            4 => {
                let _ = parse_cmap_format4(sub, &mut map);
            }
            12 => {
                let _ = parse_cmap_format12(sub, &mut map);
            }
            _ => {}
        }
    }
    Ok(map)
}

/// Merge a format-4 (segmented BMP) cmap subtable into `out`.
///
/// `segCountX2` at offset 6 is twice the segment count. `endCode` starts at
/// byte 14. A 2-byte reserved pad sits between `endCode` and `startCode`.
/// `idDelta` and `idRangeOffset` follow, each `segCount` big-endian words.
/// The search stops at the `0xFFFF..=0xFFFF` terminator segment.
///
/// An `idRangeOffset` of 0 means the glyph index is the low 16 bits of
/// `code + idDelta`. A non-zero offset is a byte distance from that segment's
/// own `idRangeOffset` word, plus `2 * (code - start)`, to a `u16` glyph id.
/// A stored id of 0 stays 0 and is not inserted. Any other stored id has
/// `idDelta` added, again keeping the low 16 bits.
fn parse_cmap_format4(sub: &[u8], out: &mut HashMap<u32, u16>) -> Result<(), TtfError> {
    if sub.len() < 14 {
        return Err(TtfError::InvalidTable("cmap"));
    }
    // segCountX2 / 2. The +2 below skips the reserved pad after endCode.
    let seg_count = (read_u16_be(sub, 6)? as usize) / 2;
    let end_codes = 14usize;
    let start_codes = end_codes + seg_count * 2 + 2;
    let id_deltas = start_codes + seg_count * 2;
    let id_range_offsets = id_deltas + seg_count * 2;
    if id_range_offsets + seg_count * 2 > sub.len() {
        return Err(TtfError::InvalidTable("cmap"));
    }

    for seg in 0..seg_count {
        let end_code = read_u16_be(sub, end_codes + seg * 2)?;
        let start_code = read_u16_be(sub, start_codes + seg * 2)?;
        let id_delta = read_i16_be(sub, id_deltas + seg * 2)?;
        let id_range_pos = id_range_offsets + seg * 2;
        let id_range_offset = read_u16_be(sub, id_range_pos)? as usize;

        // The last segment is the 0xFFFF sentinel. Later segments are not read.
        if start_code == 0xFFFF && end_code == 0xFFFF {
            break;
        }
        for code in start_code..=end_code {
            let gid = if id_range_offset == 0 {
                // Low 16 bits of code + idDelta. A negative delta still wraps modulo 65536.
                (i32::from(code) + i32::from(id_delta)) as u16
            } else {
                // idRangeOffset is relative to this word, not to the start of the subtable.
                let glyph_off =
                    id_range_pos + id_range_offset + (usize::from(code - start_code) * 2);
                if glyph_off + 2 > sub.len() {
                    continue;
                }
                let raw_gid = read_u16_be(sub, glyph_off)?;
                if raw_gid == 0 {
                    0
                } else {
                    (i32::from(raw_gid) + i32::from(id_delta)) as u16
                }
            };
            if gid != 0 {
                out.insert(u32::from(code), gid);
            }
        }
    }
    Ok(())
}

/// Merge a format-12 (segmented UCS-4) cmap subtable into `out`.
///
/// After the 16-byte header, `numGroups` groups of 12 bytes follow: start code
/// `u32`, end code `u32`, start glyph id `u32`. The index for `code` is
/// `startGlyphId + (code - start)`, cast to `u16`. A group whose end is more
/// than `0xFFFF` past its start is skipped. Glyph index 0 is not inserted.
fn parse_cmap_format12(sub: &[u8], out: &mut HashMap<u32, u16>) -> Result<(), TtfError> {
    if sub.len() < 16 {
        return Err(TtfError::InvalidTable("cmap"));
    }
    let num_groups = read_u32_be(sub, 12)? as usize;
    if 16 + num_groups * 12 > sub.len() {
        return Err(TtfError::InvalidTable("cmap"));
    }
    for i in 0..num_groups {
        let base = 16 + i * 12;
        let start_code = read_u32_be(sub, base)?;
        let end_code = read_u32_be(sub, base + 4)?;
        let start_gid = read_u32_be(sub, base + 8)?;
        // A span longer than a 16-bit glyph run is skipped rather than expanded.
        if end_code.saturating_sub(start_code) > 0xFFFF {
            continue;
        }
        for code in start_code..=end_code {
            // Glyph indices in this crate are u16. The high bits of the sum are dropped.
            let gid = (start_gid + (code - start_code)) as u16;
            if gid != 0 {
                out.insert(code, gid);
            }
        }
    }
    Ok(())
}

/// Read `kern` version 0, format 0 pairs.
///
/// The table header is version `u16` then subtable count `u16`. Any version
/// other than 0 returns an empty map. Each subtable begins with a version
/// `u16`, a byte length `u16`, and a coverage `u16`. The format is the high
/// byte of coverage. Format 0 pairs start 14 bytes into the subtable, after
/// `nPairs` and the three binary-search fields: left glyph `u16`, right glyph
/// `u16`, value `i16` in font units. The horizontal bit in the low byte of
/// coverage is not checked. A subtable length of 0 stops the walk.
fn parse_kern(kern: &[u8]) -> Result<HashMap<(u16, u16), i16>, TtfError> {
    if kern.len() < 4 {
        return Err(TtfError::InvalidTable("kern"));
    }
    let version = read_u16_be(kern, 0)?;
    if version != 0 {
        return Ok(HashMap::new());
    }
    let n_tables = read_u16_be(kern, 2)? as usize;
    let mut pos = 4usize;
    let mut pairs = HashMap::new();

    for _ in 0..n_tables {
        if pos + 6 > kern.len() {
            break;
        }
        let sub_len = read_u16_be(kern, pos + 2)? as usize;
        let coverage = read_u16_be(kern, pos + 4)?;
        // High byte is the format. The low byte (horizontal, cross-stream, override) is ignored.
        let format = coverage >> 8;
        if format == 0 && pos + 14 <= kern.len() {
            let n_pairs = read_u16_be(kern, pos + 6)? as usize;
            // pos + 6 is nPairs. +8, +10, and +12 are searchRange, entrySelector, rangeShift.
            let mut p = pos + 14;
            for _ in 0..n_pairs {
                if p + 6 > kern.len() {
                    break;
                }
                let left = read_u16_be(kern, p)?;
                let right = read_u16_be(kern, p + 2)?;
                let value = read_i16_be(kern, p + 4)?;
                pairs.insert((left, right), value);
                p += 6;
            }
        }
        // The length includes this header. Zero would leave `pos` unchanged.
        if sub_len == 0 {
            break;
        }
        pos += sub_len;
    }
    Ok(pairs)
}

/// Byte size of an OpenType `ValueRecord` given its `ValueFormat` bitmask.
fn value_record_byte_len(format: u16) -> usize {
    (format.count_ones() as usize) * 2
}

/// Byte offset of `XAdvance` (`0x0004`) inside an OpenType `ValueRecord`.
fn x_advance_offset_in_record(format: u16) -> Option<usize> {
    if (format & 0x0004) == 0 {
        return None;
    }
    let earlier_fields = (format & 0x0003).count_ones() as usize;
    Some(earlier_fields * 2)
}

/// Parse an OpenType `Coverage` table (Format 1 glyph array or Format 2 range records)
/// into `(coverage_index, glyph_id)` pairs.
fn parse_gpos_coverage(cov: &[u8]) -> Vec<(usize, u16)> {
    let mut out = Vec::new();
    let Ok(format) = read_u16_be(cov, 0) else {
        return out;
    };
    if format == 1 {
        let Ok(count) = read_u16_be(cov, 2) else {
            return out;
        };
        let n = (count as usize).min(4096);
        if 4 + n * 2 > cov.len() {
            return out;
        }
        for i in 0..n {
            if let Ok(gid) = read_u16_be(cov, 4 + i * 2) {
                out.push((i, gid));
            }
        }
    } else if format == 2 {
        let Ok(range_count) = read_u16_be(cov, 2) else {
            return out;
        };
        let n = (range_count as usize).min(1024);
        if 4 + n * 6 > cov.len() {
            return out;
        }
        for i in 0..n {
            let rec = 4 + i * 6;
            let (Ok(start_gid), Ok(end_gid), Ok(start_cov)) = (
                read_u16_be(cov, rec),
                read_u16_be(cov, rec + 2),
                read_u16_be(cov, rec + 4),
            ) else {
                break;
            };
            if end_gid < start_gid || (end_gid - start_gid) > 2048 {
                continue;
            }
            for gid in start_gid..=end_gid {
                let cov_idx = (start_cov as usize) + usize::from(gid - start_gid);
                out.push((cov_idx, gid));
            }
        }
    }
    out
}

/// Parse an OpenType `ClassDef` table (Format 1 array or Format 2 range records)
/// into `(glyph_id, class_id)` entries.
fn parse_gpos_class_def(cls: &[u8]) -> HashMap<u16, u16> {
    let mut map = HashMap::new();
    let Ok(format) = read_u16_be(cls, 0) else {
        return map;
    };
    if format == 1 {
        let (Ok(start_gid), Ok(count)) = (read_u16_be(cls, 2), read_u16_be(cls, 4)) else {
            return map;
        };
        let n = (count as usize).min(4096);
        if 6 + n * 2 > cls.len() {
            return map;
        }
        for i in 0..n {
            if let Ok(c) = read_u16_be(cls, 6 + i * 2) {
                if c != 0 {
                    map.insert(start_gid.saturating_add(i as u16), c);
                }
            }
        }
    } else if format == 2 {
        let Ok(range_count) = read_u16_be(cls, 2) else {
            return map;
        };
        let n = (range_count as usize).min(1024);
        if 4 + n * 6 > cls.len() {
            return map;
        }
        for i in 0..n {
            let rec = 4 + i * 6;
            let (Ok(start_gid), Ok(end_gid), Ok(class_val)) = (
                read_u16_be(cls, rec),
                read_u16_be(cls, rec + 2),
                read_u16_be(cls, rec + 4),
            ) else {
                break;
            };
            if class_val == 0 || end_gid < start_gid || (end_gid - start_gid) > 2048 {
                continue;
            }
            for gid in start_gid..=end_gid {
                map.insert(gid, class_val);
            }
        }
    }
    map
}

/// Parse one GPOS Lookup Type 2 (`PairPosFormat1` or `PairPosFormat2`) subtable.
fn parse_gpos_pair_pos_subtable(sub: &[u8], out: &mut HashMap<(u16, u16), i16>) {
    let Ok(pos_format) = read_u16_be(sub, 0) else {
        return;
    };
    if pos_format == 1 {
        if sub.len() < 10 {
            return;
        }
        let (Ok(cov_off), Ok(vf1), Ok(vf2), Ok(pair_set_count)) = (
            read_u16_be(sub, 2),
            read_u16_be(sub, 4),
            read_u16_be(sub, 6),
            read_u16_be(sub, 8),
        ) else {
            return;
        };
        let Some(x_adv_off) = x_advance_offset_in_record(vf1) else {
            return;
        };
        let cov_off = cov_off as usize;
        let ps_count = (pair_set_count as usize).min(4096);
        if cov_off >= sub.len() || 10 + ps_count * 2 > sub.len() {
            return;
        }
        let rec_len = 2 + value_record_byte_len(vf1) + value_record_byte_len(vf2);
        let coverage = parse_gpos_coverage(&sub[cov_off..]);
        for (cov_idx, left_gid) in coverage {
            if cov_idx >= ps_count {
                continue;
            }
            let Ok(ps_off) = read_u16_be(sub, 10 + cov_idx * 2) else {
                continue;
            };
            let ps_off = ps_off as usize;
            if ps_off + 2 > sub.len() {
                continue;
            }
            let Ok(pv_count) = read_u16_be(sub, ps_off) else {
                continue;
            };
            let pv_count = (pv_count as usize).min(2048);
            let mut p = ps_off + 2;
            for _ in 0..pv_count {
                if p + rec_len > sub.len() {
                    break;
                }
                if let (Ok(right_gid), Ok(x_adv)) =
                    (read_u16_be(sub, p), read_i16_be(sub, p + 2 + x_adv_off))
                {
                    if x_adv != 0 {
                        out.insert((left_gid, right_gid), x_adv);
                    }
                }
                p += rec_len;
            }
        }
    } else if pos_format == 2 {
        if sub.len() < 16 {
            return;
        }
        let (Ok(cov_off), Ok(vf1), Ok(vf2), Ok(cd1_off), Ok(cd2_off), Ok(c1_cnt), Ok(c2_cnt)) = (
            read_u16_be(sub, 2),
            read_u16_be(sub, 4),
            read_u16_be(sub, 6),
            read_u16_be(sub, 8),
            read_u16_be(sub, 10),
            read_u16_be(sub, 12),
            read_u16_be(sub, 14),
        ) else {
            return;
        };
        let Some(x_adv_off) = x_advance_offset_in_record(vf1) else {
            return;
        };
        let cov_off = cov_off as usize;
        let cd1_off = cd1_off as usize;
        let cd2_off = cd2_off as usize;
        if cov_off >= sub.len() || cd1_off >= sub.len() || cd2_off >= sub.len() {
            return;
        }
        let c1_cnt = (c1_cnt as usize).min(512);
        let c2_cnt = (c2_cnt as usize).min(512);
        let cell_len = value_record_byte_len(vf1) + value_record_byte_len(vf2);
        if cell_len == 0 || 16 + c1_cnt * c2_cnt * cell_len > sub.len() {
            return;
        }
        let coverage = parse_gpos_coverage(&sub[cov_off..]);
        let class1 = parse_gpos_class_def(&sub[cd1_off..]);
        let class2 = parse_gpos_class_def(&sub[cd2_off..]);

        let mut class2_by_id: HashMap<u16, Vec<u16>> = HashMap::new();
        for (&gid, &cid) in &class2 {
            class2_by_id.entry(cid).or_default().push(gid);
        }

        for (_, left_gid) in coverage {
            let c1 = class1.get(&left_gid).copied().unwrap_or(0) as usize;
            if c1 >= c1_cnt {
                continue;
            }
            for c2 in 1..c2_cnt {
                let cell_off = 16 + (c1 * c2_cnt + c2) * cell_len;
                let Ok(x_adv) = read_i16_be(sub, cell_off + x_adv_off) else {
                    continue;
                };
                if x_adv == 0 {
                    continue;
                }
                if let Some(right_gids) = class2_by_id.get(&(c2 as u16)) {
                    for &right_gid in right_gids {
                        out.insert((left_gid, right_gid), x_adv);
                    }
                }
            }
        }
    }
}

/// Walk OpenType `GPOS` LookupList for Lookup Type 2 (`PairPosFormat1` and `PairPosFormat2`)
/// and Lookup Type 9 (Extension Positioning wrapping Type 2) kerning adjustments.
fn parse_gpos(gpos: &[u8], out: &mut HashMap<(u16, u16), i16>) {
    if gpos.len() < 10 {
        return;
    }
    let (Ok(major), Ok(minor), Ok(lookup_list_off)) = (
        read_u16_be(gpos, 0),
        read_u16_be(gpos, 2),
        read_u16_be(gpos, 8),
    ) else {
        return;
    };
    if major != 1 || (minor != 0 && minor != 1) {
        return;
    }
    let ll_off = lookup_list_off as usize;
    if ll_off + 2 > gpos.len() {
        return;
    }
    let Ok(lookup_count) = read_u16_be(gpos, ll_off) else {
        return;
    };
    let n_lookups = (lookup_count as usize).min(256);
    if ll_off + 2 + n_lookups * 2 > gpos.len() {
        return;
    }

    for i in 0..n_lookups {
        let Ok(l_rel) = read_u16_be(gpos, ll_off + 2 + i * 2) else {
            continue;
        };
        let l_off = ll_off + (l_rel as usize);
        if l_off + 6 > gpos.len() {
            continue;
        }
        let (Ok(lookup_type), Ok(sub_count)) =
            (read_u16_be(gpos, l_off), read_u16_be(gpos, l_off + 4))
        else {
            continue;
        };
        let n_subs = (sub_count as usize).min(128);
        if l_off + 6 + n_subs * 2 > gpos.len() {
            continue;
        }
        for s in 0..n_subs {
            let Ok(sub_rel) = read_u16_be(gpos, l_off + 6 + s * 2) else {
                continue;
            };
            let sub_off = l_off + (sub_rel as usize);
            if sub_off >= gpos.len() {
                continue;
            }
            if lookup_type == 2 {
                parse_gpos_pair_pos_subtable(&gpos[sub_off..], out);
            } else if lookup_type == 9 && sub_off + 8 <= gpos.len() {
                let (Ok(ext_fmt), Ok(ext_type), Ok(ext_off)) = (
                    read_u16_be(gpos, sub_off),
                    read_u16_be(gpos, sub_off + 2),
                    read_u32_be(gpos, sub_off + 4),
                ) else {
                    continue;
                };
                if ext_fmt == 1 && ext_type == 2 {
                    let target = sub_off.saturating_add(ext_off as usize);
                    if target < gpos.len() {
                        parse_gpos_pair_pos_subtable(&gpos[target..], out);
                    }
                }
            }
        }
    }
}

/// Read a big-endian `u16` at `offset`, or [`TtfError::UnexpectedEof`].
fn read_u16_be(bytes: &[u8], offset: usize) -> Result<u16, TtfError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or(TtfError::UnexpectedEof)?;
    Ok(u16::from_be_bytes([slice[0], slice[1]]))
}

/// Read a big-endian `i16` at `offset`, or [`TtfError::UnexpectedEof`].
fn read_i16_be(bytes: &[u8], offset: usize) -> Result<i16, TtfError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or(TtfError::UnexpectedEof)?;
    Ok(i16::from_be_bytes([slice[0], slice[1]]))
}

/// Read a big-endian `u32` at `offset`, or [`TtfError::UnexpectedEof`].
fn read_u32_be(bytes: &[u8], offset: usize) -> Result<u32, TtfError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(TtfError::UnexpectedEof)?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Decode every glyph outline addressed by `loca`.
///
/// `index_to_loc_format` comes from `head`. Zero selects the short format:
/// each entry is a big-endian `u16` count of 16-bit words, so the byte offset
/// is that value times 2. Any other value selects the long format: each entry
/// is a `u32` byte offset. There are `num_glyphs + 1` entries. Glyph `i`
/// occupies `offsets[i]..offsets[i + 1]`. An empty range, or a range that
/// ends past `glyf`, stores [`None`]. One bad glyph stores [`None`] for that
/// index only. A `loca` that is too short fails the whole table.
fn parse_glyf_table(
    loca: &[u8],
    glyf: &[u8],
    num_glyphs: usize,
    index_to_loc_format: i16,
) -> Result<Vec<Option<GlyphOutline>>, TtfError> {
    let mut offsets = Vec::with_capacity(num_glyphs + 1);
    if index_to_loc_format == 0 {
        if loca.len() < (num_glyphs + 1) * 2 {
            return Err(TtfError::InvalidTable("loca"));
        }
        for i in 0..=num_glyphs {
            // Short loca stores a word offset. The glyf byte offset is twice that.
            offsets.push((read_u16_be(loca, i * 2)? as usize) * 2);
        }
    } else {
        if loca.len() < (num_glyphs + 1) * 4 {
            return Err(TtfError::InvalidTable("loca"));
        }
        for i in 0..=num_glyphs {
            offsets.push(read_u32_be(loca, i * 4)? as usize);
        }
    }

    let mut outlines = Vec::with_capacity(num_glyphs);
    for gid in 0..num_glyphs {
        let start = offsets[gid];
        let end = offsets[gid + 1];
        // start >= end is an empty glyph, not a corrupt one. A range past glyf is dropped too.
        if start >= end || end > glyf.len() {
            outlines.push(None);
            continue;
        }
        let slice = &glyf[start..end];
        outlines.push(parse_single_glyph(slice, glyf, &offsets, 0).ok().flatten());
    }
    Ok(outlines)
}

fn flush_cff_contour(
    current: &mut Vec<(f32, f32, CurveOrder)>,
    contours: &mut Vec<Vec<OutlinePoint>>,
) {
    if current.len() >= 2 && contours.len() < MAX_GLYPH_CONTOURS {
        let poly = flatten_cubic_contour(current);
        if poly.len() >= 3 {
            let mut pts = Vec::with_capacity(poly.len().min(MAX_CONTOUR_POINTS));
            for &(x, y) in poly.iter().take(MAX_CONTOUR_POINTS) {
                pts.push(OutlinePoint::on_curve(
                    x.round().clamp(-32768.0, 32767.0) as i16,
                    y.round().clamp(-32768.0, 32767.0) as i16,
                ));
            }
            contours.push(pts);
        }
    }
    current.clear();
}

/// Interpret a Compact Font Format (`CFF ` / `CFF2`) Type 2 CharString bytecode program into a [`GlyphOutline`].
///
/// Supports Type 2 path operators (`rmoveto`, `hmoveto`, `vmoveto`, `rlineto`, `hlineto`, `vlineto`,
/// `rrcurveto`, `hhcurveto`, `vvcurveto`, `hvcurveto`, `vhcurveto`, `rcurveline`, `rlinecurve`, `endchar`)
/// and stem/hintmask operators (`hstem`, `vstem`, `hstemhm`, `vstemhm`, `hintmask`, `cntrmask`).
fn cff_subr_bias(count: usize) -> i32 {
    if count < 1240 {
        107
    } else if count < 33900 {
        1131
    } else {
        32768
    }
}

/// Interpret a Compact Font Format (`CFF ` / `CFF2`) Type 2 CharString bytecode program without subroutines.
pub fn interpret_type2_charstring(program: &[u8]) -> Option<GlyphOutline> {
    interpret_type2_charstring_with_subrs(program, &[], &[])
}

/// Interpret a Compact Font Format (`CFF ` / `CFF2`) Type 2 CharString program with local and global subroutines (`callsubr` / `callgsubr`).
pub fn interpret_type2_charstring_with_subrs<'a>(
    mut program: &'a [u8],
    local_subrs: &[&'a [u8]],
    global_subrs: &[&'a [u8]],
) -> Option<GlyphOutline> {
    let mut stack: Vec<f32> = Vec::with_capacity(48);
    let mut contours: Vec<Vec<OutlinePoint>> = Vec::new();
    let mut current: Vec<(f32, f32, CurveOrder)> = Vec::new();
    let mut cx = 0.0f32;
    let mut cy = 0.0f32;
    let mut stem_hints = 0usize;
    let mut i = 0usize;

    let mut call_stack: Vec<(&'a [u8], usize)> = Vec::new();
    let local_bias = cff_subr_bias(local_subrs.len());
    let global_bias = cff_subr_bias(global_subrs.len());

    loop {
        if i >= program.len() {
            if let Some((caller_prog, caller_i)) = call_stack.pop() {
                program = caller_prog;
                i = caller_i;
                continue;
            }
            break;
        }
        let b0 = program[i];
        i += 1;
        match b0 {
            28 => {
                if i + 2 > program.len() {
                    break;
                }
                let val = i16::from_be_bytes([program[i], program[i + 1]]) as f32;
                i += 2;
                if stack.len() < 48 {
                    stack.push(val);
                }
            }
            32..=246 => {
                if stack.len() < 48 {
                    stack.push((i32::from(b0) - 139) as f32);
                }
            }
            247..=250 => {
                if i >= program.len() {
                    break;
                }
                let b1 = program[i];
                i += 1;
                let val = ((i32::from(b0) - 247) * 256 + i32::from(b1) + 108) as f32;
                if stack.len() < 48 {
                    stack.push(val);
                }
            }
            251..=254 => {
                if i >= program.len() {
                    break;
                }
                let b1 = program[i];
                i += 1;
                let val = -((i32::from(b0) - 251) * 256 + i32::from(b1) + 108) as f32;
                if stack.len() < 48 {
                    stack.push(val);
                }
            }
            255 => {
                if i + 4 > program.len() {
                    break;
                }
                let raw = i32::from_be_bytes([
                    program[i],
                    program[i + 1],
                    program[i + 2],
                    program[i + 3],
                ]);
                i += 4;
                if stack.len() < 48 {
                    stack.push((raw as f32) / 65536.0);
                }
            }
            1 | 3 | 18 | 23 => {
                stem_hints += stack.len() / 2;
                stack.clear();
            }
            19 | 20 => {
                stem_hints += stack.len() / 2;
                stack.clear();
                let mask_bytes = (stem_hints + 7) / 8;
                i = i.saturating_add(mask_bytes).min(program.len());
            }
            21 => {
                // rmoveto
                flush_cff_contour(&mut current, &mut contours);
                if stack.len() >= 2 {
                    let dy = stack[stack.len() - 1];
                    let dx = stack[stack.len() - 2];
                    cx += dx;
                    cy += dy;
                    current.push((cx, cy, CurveOrder::OnCurve));
                }
                stack.clear();
            }
            22 => {
                // hmoveto
                flush_cff_contour(&mut current, &mut contours);
                if let Some(&dx) = stack.last() {
                    cx += dx;
                    current.push((cx, cy, CurveOrder::OnCurve));
                }
                stack.clear();
            }
            4 => {
                // vmoveto
                flush_cff_contour(&mut current, &mut contours);
                if let Some(&dy) = stack.last() {
                    cy += dy;
                    current.push((cx, cy, CurveOrder::OnCurve));
                }
                stack.clear();
            }
            5 => {
                // rlineto
                for pair in stack.chunks_exact(2) {
                    cx += pair[0];
                    cy += pair[1];
                    current.push((cx, cy, CurveOrder::OnCurve));
                }
                stack.clear();
            }
            6 | 7 => {
                // hlineto (6) or vlineto (7)
                let mut horiz = b0 == 6;
                for &delta in &stack {
                    if horiz {
                        cx += delta;
                    } else {
                        cy += delta;
                    }
                    current.push((cx, cy, CurveOrder::OnCurve));
                    horiz = !horiz;
                }
                stack.clear();
            }
            8 => {
                // rrcurveto
                for seg in stack.chunks_exact(6) {
                    let x1 = cx + seg[0];
                    let y1 = cy + seg[1];
                    let x2 = x1 + seg[2];
                    let y2 = y1 + seg[3];
                    cx = x2 + seg[4];
                    cy = y2 + seg[5];
                    current.push((x1, y1, CurveOrder::CubicOffCurve));
                    current.push((x2, y2, CurveOrder::CubicOffCurve));
                    current.push((cx, cy, CurveOrder::OnCurve));
                }
                stack.clear();
            }
            24 => {
                // rcurveline
                if stack.len() >= 8 {
                    let curve_end = stack.len() - 2;
                    for seg in stack[..curve_end].chunks_exact(6) {
                        let x1 = cx + seg[0];
                        let y1 = cy + seg[1];
                        let x2 = x1 + seg[2];
                        let y2 = y1 + seg[3];
                        cx = x2 + seg[4];
                        cy = y2 + seg[5];
                        current.push((x1, y1, CurveOrder::CubicOffCurve));
                        current.push((x2, y2, CurveOrder::CubicOffCurve));
                        current.push((cx, cy, CurveOrder::OnCurve));
                    }
                    cx += stack[curve_end];
                    cy += stack[curve_end + 1];
                    current.push((cx, cy, CurveOrder::OnCurve));
                }
                stack.clear();
            }
            25 => {
                // rlinecurve
                if stack.len() >= 8 {
                    let line_end = stack.len() - 6;
                    for pair in stack[..line_end].chunks_exact(2) {
                        cx += pair[0];
                        cy += pair[1];
                        current.push((cx, cy, CurveOrder::OnCurve));
                    }
                    let seg = &stack[line_end..];
                    let x1 = cx + seg[0];
                    let y1 = cy + seg[1];
                    let x2 = x1 + seg[2];
                    let y2 = y1 + seg[3];
                    cx = x2 + seg[4];
                    cy = y2 + seg[5];
                    current.push((x1, y1, CurveOrder::CubicOffCurve));
                    current.push((x2, y2, CurveOrder::CubicOffCurve));
                    current.push((cx, cy, CurveOrder::OnCurve));
                }
                stack.clear();
            }
            26 => {
                // vvcurveto
                let mut idx = 0usize;
                let mut dx1 = 0.0f32;
                if stack.len() % 4 == 1 {
                    dx1 = stack[0];
                    idx = 1;
                }
                while idx + 4 <= stack.len() {
                    let x1 = cx + dx1;
                    let y1 = cy + stack[idx];
                    let x2 = x1 + stack[idx + 1];
                    let y2 = y1 + stack[idx + 2];
                    cx = x2;
                    cy = y2 + stack[idx + 3];
                    current.push((x1, y1, CurveOrder::CubicOffCurve));
                    current.push((x2, y2, CurveOrder::CubicOffCurve));
                    current.push((cx, cy, CurveOrder::OnCurve));
                    dx1 = 0.0;
                    idx += 4;
                }
                stack.clear();
            }
            27 => {
                // hhcurveto
                let mut idx = 0usize;
                let mut dy1 = 0.0f32;
                if stack.len() % 4 == 1 {
                    dy1 = stack[0];
                    idx = 1;
                }
                while idx + 4 <= stack.len() {
                    let x1 = cx + stack[idx];
                    let y1 = cy + dy1;
                    let x2 = x1 + stack[idx + 1];
                    let y2 = y1 + stack[idx + 2];
                    cx = x2 + stack[idx + 3];
                    cy = y2;
                    current.push((x1, y1, CurveOrder::CubicOffCurve));
                    current.push((x2, y2, CurveOrder::CubicOffCurve));
                    current.push((cx, cy, CurveOrder::OnCurve));
                    dy1 = 0.0;
                    idx += 4;
                }
                stack.clear();
            }
            30 | 31 => {
                // vhcurveto (30) or hvcurveto (31)
                let mut vertical_first = b0 == 30;
                let mut idx = 0usize;
                while idx + 4 <= stack.len() {
                    let trailing = if idx + 5 == stack.len() {
                        stack[idx + 4]
                    } else {
                        0.0
                    };
                    if vertical_first {
                        let x1 = cx;
                        let y1 = cy + stack[idx];
                        let x2 = x1 + stack[idx + 1];
                        let y2 = y1 + stack[idx + 2];
                        cx = x2 + stack[idx + 3];
                        cy = y2 + trailing;
                        current.push((x1, y1, CurveOrder::CubicOffCurve));
                        current.push((x2, y2, CurveOrder::CubicOffCurve));
                        current.push((cx, cy, CurveOrder::OnCurve));
                    } else {
                        let x1 = cx + stack[idx];
                        let y1 = cy;
                        let x2 = x1 + stack[idx + 1];
                        let y2 = y1 + stack[idx + 2];
                        cx = x2 + trailing;
                        cy = y2 + stack[idx + 3];
                        current.push((x1, y1, CurveOrder::CubicOffCurve));
                        current.push((x2, y2, CurveOrder::CubicOffCurve));
                        current.push((cx, cy, CurveOrder::OnCurve));
                    }
                    vertical_first = !vertical_first;
                    idx += if idx + 5 == stack.len() { 5 } else { 4 };
                }
                stack.clear();
            }
            12 => {
                if i < program.len() {
                    i += 1;
                }
                stack.clear();
            }
            10 | 29 => {
                // callsubr (10) or callgsubr (29)
                if let Some(biased) = stack.pop() {
                    let (subrs, bias) = if b0 == 10 {
                        (local_subrs, local_bias)
                    } else {
                        (global_subrs, global_bias)
                    };
                    let idx = (biased.round() as i32) + bias;
                    if idx >= 0 && call_stack.len() < 16 {
                        if let Some(&subr_bytes) = subrs.get(idx as usize) {
                            call_stack.push((program, i));
                            program = subr_bytes;
                            i = 0;
                        }
                    }
                }
            }
            11 => {
                // return from subroutine
                i = program.len();
            }
            14 => {
                // endchar
                flush_cff_contour(&mut current, &mut contours);
                break;
            }
            _ => {
                stack.clear();
            }
        }
    }
    flush_cff_contour(&mut current, &mut contours);
    if contours.is_empty() {
        return None;
    }

    let mut x_min = i16::MAX;
    let mut y_min = i16::MAX;
    let mut x_max = i16::MIN;
    let mut y_max = i16::MIN;
    for contour in &contours {
        for pt in contour {
            x_min = x_min.min(pt.x);
            y_min = y_min.min(pt.y);
            x_max = x_max.max(pt.x);
            y_max = y_max.max(pt.y);
        }
    }
    Some(GlyphOutline {
        x_min,
        y_min,
        x_max,
        y_max,
        contours,
    })
}

fn read_cff_offset(bytes: &[u8], pos: usize, off_size: usize) -> Option<usize> {
    if off_size == 0 || off_size > 4 || pos + off_size > bytes.len() {
        return None;
    }
    let mut v = 0usize;
    for &b in &bytes[pos..pos + off_size] {
        v = (v << 8) | usize::from(b);
    }
    Some(v)
}

/// Parse a CFF 1.0 `INDEX` structure starting at `start`, returning the item slices and the byte offset after the INDEX.
fn parse_cff_index<'a>(cff: &'a [u8], start: usize) -> Option<(Vec<&'a [u8]>, usize)> {
    if start + 2 > cff.len() {
        return None;
    }
    let count = u16::from_be_bytes([cff[start], cff[start + 1]]) as usize;
    if count == 0 {
        return Some((Vec::new(), start + 2));
    }
    if start + 3 > cff.len() || count > 65535 {
        return None;
    }
    let off_size = usize::from(cff[start + 2]);
    let off_array_start = start + 3;
    let off_array_len = (count + 1).checked_mul(off_size)?;
    let data_base = off_array_start.checked_add(off_array_len)?.checked_sub(1)?;
    if off_array_start + off_array_len > cff.len() {
        return None;
    }
    let mut offsets = Vec::with_capacity(count + 1);
    for idx in 0..=count {
        let off = read_cff_offset(cff, off_array_start + idx * off_size, off_size)?;
        if off == 0 {
            return None;
        }
        offsets.push(off);
    }
    let end_pos = data_base.checked_add(*offsets.last()?)?;
    if end_pos > cff.len() {
        return None;
    }
    let mut items = Vec::with_capacity(count.min(8192));
    for idx in 0..count.min(8192) {
        let s = data_base.checked_add(offsets[idx])?;
        let e = data_base.checked_add(offsets[idx + 1])?;
        if s > e || e > cff.len() {
            return None;
        }
        items.push(&cff[s..e]);
    }
    Some((items, end_pos))
}

/// Extract the `CharStrings` table offset (operator `17`) from a CFF Top DICT byte slice.
fn parse_cff_dict_operands(dict: &[u8], target_op: u8) -> Option<Vec<i32>> {
    let mut stack: Vec<i32> = Vec::with_capacity(16);
    let mut i = 0usize;
    while i < dict.len() {
        let b0 = dict[i];
        i += 1;
        match b0 {
            28 => {
                if i + 2 > dict.len() {
                    break;
                }
                let v = i16::from_be_bytes([dict[i], dict[i + 1]]) as i32;
                i += 2;
                stack.push(v);
            }
            29 => {
                if i + 4 > dict.len() {
                    break;
                }
                let v = i32::from_be_bytes([dict[i], dict[i + 1], dict[i + 2], dict[i + 3]]);
                i += 4;
                stack.push(v);
            }
            30 => {
                while i < dict.len() {
                    let nib_byte = dict[i];
                    i += 1;
                    if (nib_byte & 0xF0) == 0xF0 || (nib_byte & 0x0F) == 0x0F {
                        break;
                    }
                }
                stack.push(0);
            }
            32..=246 => stack.push(i32::from(b0) - 139),
            247..=250 => {
                if i >= dict.len() {
                    break;
                }
                let b1 = dict[i];
                i += 1;
                stack.push((i32::from(b0) - 247) * 256 + i32::from(b1) + 108);
            }
            251..=254 => {
                if i >= dict.len() {
                    break;
                }
                let b1 = dict[i];
                i += 1;
                stack.push(-((i32::from(b0) - 251) * 256 + i32::from(b1) + 108));
            }
            12 => {
                if i < dict.len() {
                    i += 1;
                }
                stack.clear();
            }
            op if op == target_op => {
                return (!stack.is_empty()).then_some(stack);
            }
            255 => {
                stack.clear();
            }
            _ => {
                stack.clear();
            }
        }
    }
    None
}

/// Parse an OpenType `CFF ` table and decode each glyph's Type 2 CharString into a [`GlyphOutline`].
fn parse_cff_table(cff: &[u8], num_glyphs: usize) -> Result<Vec<Option<GlyphOutline>>, TtfError> {
    if cff.len() < 4 || cff[0] != 1 {
        return Err(TtfError::InvalidTable("CFF "));
    }
    let hdr_size = usize::from(cff[2]);
    let (_, after_name_idx) =
        parse_cff_index(cff, hdr_size).ok_or(TtfError::InvalidTable("CFF "))?;
    let (top_dicts, after_top_idx) =
        parse_cff_index(cff, after_name_idx).ok_or(TtfError::InvalidTable("CFF "))?;
    let top_dict = top_dicts
        .first()
        .copied()
        .ok_or(TtfError::InvalidTable("CFF "))?;
    let charstrings_off = parse_cff_dict_operands(top_dict, 17)
        .and_then(|ops| ops.last().copied())
        .filter(|&off| off > 0)
        .map(|off| off as usize)
        .ok_or(TtfError::InvalidTable("CFF "))?;
    let (charstrings, _) =
        parse_cff_index(cff, charstrings_off).ok_or(TtfError::InvalidTable("CFF "))?;

    let global_subrs = parse_cff_index(cff, after_top_idx)
        .and_then(|(_, after_str_idx)| parse_cff_index(cff, after_str_idx))
        .map(|(v, _)| v)
        .unwrap_or_default();
    let local_subrs = parse_cff_dict_operands(top_dict, 18)
        .filter(|ops| ops.len() >= 2)
        .and_then(|ops| {
            let priv_size = ops[ops.len() - 2].max(0) as usize;
            let priv_off = ops[ops.len() - 1].max(0) as usize;
            let priv_end = priv_off.checked_add(priv_size)?;
            if priv_end > cff.len() {
                return None;
            }
            let subrs_rel =
                *parse_cff_dict_operands(&cff[priv_off..priv_end], 19)?.last()? as usize;
            let subrs_abs = priv_off.checked_add(subrs_rel)?;
            parse_cff_index(cff, subrs_abs).map(|(v, _)| v)
        })
        .unwrap_or_default();

    let mut outlines = Vec::with_capacity(num_glyphs);
    for gid in 0..num_glyphs {
        let outline = charstrings.get(gid).and_then(|prog| {
            interpret_type2_charstring_with_subrs(prog, &local_subrs, &global_subrs)
        });
        outlines.push(outline);
    }
    Ok(outlines)
}

/// Decode a family name from the `name` table, if one is usable.
///
/// The header is format `u16`, record count `u16`, and `stringOffset` `u16`.
/// Each record is 12 bytes. The characters sit at `stringOffset + offset`.
/// Name ID 1 (family) is returned as soon as a non-empty string is decoded.
/// Name ID 4 (full name) is kept only as a fallback. Platform 0 and platform 3
/// strings are big-endian UTF-16. Other platforms keep bytes in the printable
/// ASCII range and drop the rest. More than 512 records, a record array that
/// does not fit, or a `stringOffset` past the table returns [`None`]. A string
/// longer than 256 bytes is skipped, not fatal.
fn parse_name_table(name_tbl: &[u8]) -> Option<String> {
    if name_tbl.len() < 6 {
        return None;
    }
    let count = read_u16_be(name_tbl, 2).ok()? as usize;
    let string_offset = read_u16_be(name_tbl, 4).ok()? as usize;
    if count > 512 || 6 + count * 12 > name_tbl.len() || string_offset > name_tbl.len() {
        return None;
    }
    let mut fallback: Option<String> = None;
    for i in 0..count {
        let rec = 6 + i * 12;
        let platform_id = read_u16_be(name_tbl, rec).ok()?;
        let name_id = read_u16_be(name_tbl, rec + 6).ok()?;
        let length = read_u16_be(name_tbl, rec + 8).ok()? as usize;
        let offset = read_u16_be(name_tbl, rec + 10).ok()? as usize;
        // 1 is the font family. 4 is the full name, used only when no family string exists.
        if name_id != 1 && name_id != 4 {
            continue;
        }
        let start = string_offset.saturating_add(offset);
        let end = start.saturating_add(length);
        if end > name_tbl.len() || length == 0 || length > 256 {
            continue;
        }
        let raw = &name_tbl[start..end];
        // Platforms 0 and 3 store big-endian UTF-16. An odd trailing byte is dropped.
        let decoded = if platform_id == 0 || platform_id == 3 {
            let mut u16s = Vec::with_capacity(raw.len() / 2);
            for chunk in raw.chunks_exact(2) {
                u16s.push(u16::from_be_bytes([chunk[0], chunk[1]]));
            }
            String::from_utf16_lossy(&u16s)
        } else {
            raw.iter()
                .filter(|&&b| (32..=126).contains(&b))
                .map(|&b| b as char)
                .collect()
        };
        let trimmed = decoded.trim().to_string();
        if !trimmed.is_empty() {
            if name_id == 1 {
                return Some(trimmed);
            }
            fallback = Some(trimmed);
        }
    }
    fallback
}

/// Decode one `glyf` record, including a compound glyph's components.
///
/// The 10-byte header is `numberOfContours` `i16`, then `xMin`, `yMin`,
/// `xMax`, and `yMax`. A count of 0 is an empty glyph ([`None`]). A positive
/// count is a simple glyph and must be at most [`MAX_GLYPH_CONTOURS`]. A
/// negative count is a compound glyph; the usual stored value is -1. `depth`
/// counts nested compounds and refuses a value past
/// [`MAX_COMPOSITE_GLYPH_DEPTH`]. The bounding box is copied from the header
/// and is not recomputed from the points.
fn parse_single_glyph(
    slice: &[u8],
    glyf: &[u8],
    offsets: &[usize],
    depth: u8,
) -> Result<Option<GlyphOutline>, TtfError> {
    if depth > MAX_COMPOSITE_GLYPH_DEPTH || slice.len() < 10 {
        return Ok(None);
    }
    let num_contours = read_i16_be(slice, 0)?;
    let x_min = read_i16_be(slice, 2)?;
    let y_min = read_i16_be(slice, 4)?;
    let x_max = read_i16_be(slice, 6)?;
    let y_max = read_i16_be(slice, 8)?;

    if num_contours == 0 {
        return Ok(None);
    }
    // Positive: simple contours. Negative (usually -1): compound components.
    if num_contours > 0 {
        if (num_contours as usize) > MAX_GLYPH_CONTOURS {
            return Err(TtfError::InvalidTable("glyf"));
        }
        let contours = parse_simple_contours(&slice[10..], num_contours as usize)?;
        Ok(Some(GlyphOutline {
            x_min,
            y_min,
            x_max,
            y_max,
            contours,
        }))
    } else {
        let contours = parse_compound_contours(&slice[10..], glyf, offsets, depth + 1)?;
        Ok(Some(GlyphOutline {
            x_min,
            y_min,
            x_max,
            y_max,
            contours,
        }))
    }
}

/// Decode the simple-glyph body that follows a 10-byte `glyf` header.
///
/// `endPtsOfContours` is `num_contours` big-endian `u16` end indices. The next
/// `u16` is the instruction length; those bytes are skipped and not hinted.
/// Flags follow, then every X delta, then every Y delta. Coordinates are
/// deltas from the previous point, starting at 0, and wrap on overflow.
/// Flag bit 0 is on-curve. Bit 3 repeats the flag. Bits 1 and 4 pack X.
/// Bits 2 and 5 pack Y.
fn parse_simple_contours(
    data: &[u8],
    num_contours: usize,
) -> Result<Vec<Vec<OutlinePoint>>, TtfError> {
    if num_contours > MAX_GLYPH_CONTOURS || data.len() < num_contours * 2 + 2 {
        return Err(TtfError::InvalidTable("glyf"));
    }
    let mut end_pts = Vec::with_capacity(num_contours);
    for i in 0..num_contours {
        end_pts.push(read_u16_be(data, i * 2)? as usize);
    }
    let total_pts = end_pts.last().copied().map(|p| p + 1).unwrap_or(0);
    if total_pts == 0 {
        return Ok(Vec::new());
    }
    if total_pts > MAX_CONTOUR_POINTS {
        return Err(TtfError::InvalidTable("glyf"));
    }

    let insn_len = read_u16_be(data, num_contours * 2)? as usize;
    // Instructions sit between the end-point list and the flags. They are not executed.
    let mut pos = num_contours * 2 + 2 + insn_len;
    if pos > data.len() {
        return Err(TtfError::InvalidTable("glyf"));
    }

    let mut flags = Vec::with_capacity(total_pts);
    while flags.len() < total_pts {
        let f = *data.get(pos).ok_or(TtfError::UnexpectedEof)?;
        pos += 1;
        flags.push(f);
        // Bit 3 (REPEAT_FLAG): the next byte is how many extra copies to emit.
        // Copies stop once `flags` already holds every point.
        if (f & 0x08) != 0 {
            let repeat = *data.get(pos).ok_or(TtfError::UnexpectedEof)? as usize;
            pos += 1;
            for _ in 0..repeat {
                if flags.len() < total_pts {
                    flags.push(f);
                }
            }
        }
    }

    let mut xs = Vec::with_capacity(total_pts);
    let mut cur_x: i16 = 0;
    for &f in &flags {
        // Bit 1 (X_SHORT_VECTOR): one unsigned byte. Bit 4 set means +byte, clear means -byte.
        // Bit 1 clear and bit 4 set: dx is 0 and no bytes are consumed.
        // Bit 1 clear and bit 4 clear: dx is a signed i16.
        let short = (f & 0x02) != 0;
        let same_or_pos = (f & 0x10) != 0;
        let dx = if short {
            let b = i16::from(*data.get(pos).ok_or(TtfError::UnexpectedEof)?);
            pos += 1;
            if same_or_pos { b } else { -b }
        } else if same_or_pos {
            0
        } else {
            let v = read_i16_be(data, pos)?;
            pos += 2;
            v
        };
        cur_x = cur_x.wrapping_add(dx);
        xs.push(cur_x);
    }

    let mut ys = Vec::with_capacity(total_pts);
    let mut cur_y: i16 = 0;
    for &f in &flags {
        // Bit 2 (Y_SHORT_VECTOR) and bit 5 match the X packing above.
        let short = (f & 0x04) != 0;
        let same_or_pos = (f & 0x20) != 0;
        let dy = if short {
            let b = i16::from(*data.get(pos).ok_or(TtfError::UnexpectedEof)?);
            pos += 1;
            if same_or_pos { b } else { -b }
        } else if same_or_pos {
            0
        } else {
            let v = read_i16_be(data, pos)?;
            pos += 2;
            v
        };
        cur_y = cur_y.wrapping_add(dy);
        ys.push(cur_y);
    }

    let mut contours = Vec::with_capacity(num_contours);
    let mut start = 0usize;
    for &end in &end_pts {
        // A decreasing end index stops the contour walk. Later contours are dropped.
        if end >= total_pts || end < start {
            break;
        }
        let mut contour = Vec::with_capacity(end - start + 1);
        for idx in start..=end {
            contour.push(OutlinePoint {
                x: xs[idx],
                y: ys[idx],
                // Bit 0 (ON_CURVE_POINT). Clear means a quadratic control, not a corner.
                on_curve: (flags[idx] & 0x01) != 0,
            });
        }
        contours.push(contour);
        start = end + 1;
    }
    Ok(contours)
}

/// Append component outlines for a compound glyph.
///
/// Each component is a flag `u16`, a glyph index `u16`, then arguments and an
/// optional scale. Flag `0x0001` stores the two arguments as signed 16-bit
/// words; otherwise they are signed bytes. Flag `0x0002` means those arguments
/// are XY offsets in font units. When it is clear, the arguments are point
/// indices and the offset applied here is `(0, 0)`. Flags `0x0008`, `0x0040`,
/// and `0x0080` mark a uniform scale, separate X and Y scales, or a 2-by-2
/// transform. The matching words are skipped. The scale is not applied. Flag
/// `0x0020` means another component follows. Components are translated with
/// wrapping addition and decoded at `depth`.
fn parse_compound_contours(
    data: &[u8],
    glyf: &[u8],
    offsets: &[usize],
    depth: u8,
) -> Result<Vec<Vec<OutlinePoint>>, TtfError> {
    let mut pos = 0usize;
    let mut all_contours = Vec::new();
    loop {
        if pos + 4 > data.len() {
            break;
        }
        let flags = read_u16_be(data, pos)?;
        let comp_gid = read_u16_be(data, pos + 2)? as usize;
        pos += 4;

        // 0x0001: ARG_1_AND_2_ARE_WORDS. 0x0002: ARGS_ARE_XY_VALUES.
        let args_words = (flags & 0x0001) != 0;
        let args_xy = (flags & 0x0002) != 0;
        let (dx, dy) = if args_words {
            if pos + 4 > data.len() {
                break;
            }
            let a1 = read_i16_be(data, pos)?;
            let a2 = read_i16_be(data, pos + 2)?;
            pos += 4;
            // Point-index arguments are not resolved. Only an XY offset moves the component.
            if args_xy { (a1, a2) } else { (0, 0) }
        } else {
            if pos + 2 > data.len() {
                break;
            }
            let a1 = i16::from(data[pos] as i8);
            let a2 = i16::from(data[pos + 1] as i8);
            pos += 2;
            if args_xy { (a1, a2) } else { (0, 0) }
        };

        // 0x0008: one F2.14 scale (2 bytes). 0x0040: X and Y scales (4). 0x0080: 2x2 (8).
        // The first matching flag wins. The words are skipped, not multiplied in.
        if (flags & 0x0008) != 0 {
            pos += 2;
        } else if (flags & 0x0040) != 0 {
            pos += 4;
        } else if (flags & 0x0080) != 0 {
            pos += 8;
        }

        if comp_gid + 1 < offsets.len() {
            let c_start = offsets[comp_gid];
            let c_end = offsets[comp_gid + 1];
            if c_start < c_end && c_end <= glyf.len() {
                if let Ok(Some(sub)) =
                    parse_single_glyph(&glyf[c_start..c_end], glyf, offsets, depth)
                {
                    for contour in sub.contours {
                        let shifted = contour
                            .into_iter()
                            .map(|pt| OutlinePoint {
                                x: pt.x.wrapping_add(dx),
                                y: pt.y.wrapping_add(dy),
                                on_curve: pt.on_curve,
                            })
                            .collect();
                        all_contours.push(shifted);
                    }
                }
            }
        }

        // 0x0020 (MORE_COMPONENTS). Clear means this was the last component.
        if (flags & 0x0020) == 0 {
            break;
        }
    }
    Ok(all_contours)
}

/// Turn one closed contour into a polygon in pixel space, y increasing downward.
///
/// X is `point.x * scale`. Y is `-point.y * scale`, so font y-up becomes
/// raster y-down. The last point is adjacent to the first. Two off-curve
/// points in a row insert the on-curve midpoint TrueType implies there. The
/// walk starts at the first on-curve point. An on-curve neighbor is a straight
/// edge. An off-curve neighbor is a quadratic through that control and the
/// following point, sampled at six steps. The sample at `t = 1` is left for
/// the next vertex.
/// Snap horizontal stems and the baseline (`y == 0`) to integer pixel rows at UI/body sizes.
#[inline]
fn snap_vertical_grid_y(
    cur: OutlinePoint,
    prev: OutlinePoint,
    next: OutlinePoint,
    scale: f32,
    grid_fit: bool,
) -> f32 {
    let raw_y = -(cur.y as f32) * scale;
    if !grid_fit {
        return raw_y;
    }
    let is_baseline = cur.y == 0;
    let dy_prev = (i32::from(cur.y) - i32::from(prev.y)).abs();
    let dx_prev = (i32::from(cur.x) - i32::from(prev.x)).abs();
    let dy_next = (i32::from(cur.y) - i32::from(next.y)).abs();
    let dx_next = (i32::from(cur.x) - i32::from(next.x)).abs();
    let is_horizontal_stem = cur.on_curve
        && ((prev.on_curve && dy_prev <= 2 && dx_prev >= 8)
            || (next.on_curve && dy_next <= 2 && dx_next >= 8));
    if is_baseline || is_horizontal_stem {
        raw_y.round()
    } else {
        raw_y
    }
}

fn flatten_contour_opts(
    points: &[OutlinePoint],
    scale: f32,
    x_offset_px: f32,
    grid_fit: bool,
) -> Vec<(f32, f32)> {
    if points.len() < 2 {
        return Vec::new();
    }
    let mut expanded: Vec<(f32, f32, bool)> = Vec::new();
    let n = points.len();
    for i in 0..n {
        let cur = points[i];
        let next = points[(i + 1) % n];
        let prev = points[(i + n - 1) % n];
        let cx = (cur.x as f32) * scale + x_offset_px;
        // Negate font y so row 0 of the bitmap is the top of the glyph.
        let cy = snap_vertical_grid_y(cur, prev, next, scale, grid_fit);
        expanded.push((cx, cy, cur.on_curve));
        // Consecutive off-curve points imply an on-curve point halfway between them.
        if !cur.on_curve && !next.on_curve {
            let after = points[(i + 2) % n];
            let nx = (next.x as f32) * scale + x_offset_px;
            let ny = snap_vertical_grid_y(next, cur, after, scale, grid_fit);
            expanded.push(((cx + nx) * 0.5, (cy + ny) * 0.5, true));
        }
    }

    let first_on = expanded.iter().position(|p| p.2).unwrap_or(0);
    let mut poly = Vec::new();
    let len = expanded.len();
    let mut idx = 0usize;
    while idx < len {
        let p0 = expanded[(first_on + idx) % len];
        let p1 = expanded[(first_on + idx + 1) % len];
        if p1.2 {
            poly.push((p0.0, p0.1));
            idx += 1;
        } else {
            let p2 = expanded[(first_on + idx + 2) % len];
            let steps = 6usize;
            for s in 0..steps {
                let t = (s as f32) / (steps as f32);
                let inv = 1.0 - t;
                // Quadratic Bezier. t = 0 is included. t = 1 is the next vertex.
                let x = inv * inv * p0.0 + 2.0 * inv * t * p1.0 + t * t * p2.0;
                let y = inv * inv * p0.1 + 2.0 * inv * t * p1.1 + t * t * p2.1;
                poly.push((x, y));
            }
            idx += 2;
        }
    }
    poly
}

/// Subdivide a quadratic Bezier segment `(p0, c, p2)` into `steps` linear vertices (`t = 0` included, `t = 1` omitted).
pub fn flatten_quad_bezier(
    p0: (f32, f32),
    c: (f32, f32),
    p2: (f32, f32),
    steps: usize,
    out: &mut Vec<(f32, f32)>,
) {
    let steps = steps.max(1);
    for s in 0..steps {
        let t = (s as f32) / (steps as f32);
        let inv = 1.0 - t;
        let x = inv * inv * p0.0 + 2.0 * inv * t * c.0 + t * t * p2.0;
        let y = inv * inv * p0.1 + 2.0 * inv * t * c.1 + t * t * p2.1;
        out.push((x, y));
    }
}

/// Subdivide a cubic Bezier segment `(p0, c1, c2, p3)` into `steps` linear vertices (`t = 0` included, `t = 1` omitted).
pub fn flatten_cubic_bezier(
    p0: (f32, f32),
    c1: (f32, f32),
    c2: (f32, f32),
    p3: (f32, f32),
    steps: usize,
    out: &mut Vec<(f32, f32)>,
) {
    let steps = steps.max(1);
    for s in 0..steps {
        let t = (s as f32) / (steps as f32);
        let inv = 1.0 - t;
        let b0 = inv * inv * inv;
        let b1 = 3.0 * inv * inv * t;
        let b2 = 3.0 * inv * t * t;
        let b3 = t * t * t;
        let x = b0 * p0.0 + b1 * c1.0 + b2 * c2.0 + b3 * p3.0;
        let y = b0 * p0.1 + b1 * c1.1 + b2 * c2.1 + b3 * p3.1;
        out.push((x, y));
    }
}

/// Flatten a mixed contour containing `OnCurve`, `QuadraticOffCurve`, and `CubicOffCurve` control points.
pub fn flatten_cubic_contour(points: &[(f32, f32, CurveOrder)]) -> Vec<(f32, f32)> {
    if points.len() < 2 {
        return Vec::new();
    }
    let first_on = points
        .iter()
        .position(|p| p.2 == CurveOrder::OnCurve)
        .unwrap_or(0);
    let mut poly = Vec::new();
    let len = points.len();
    let mut idx = 0usize;
    while idx < len {
        let p0 = points[(first_on + idx) % len];
        let p1 = points[(first_on + idx + 1) % len];
        match p1.2 {
            CurveOrder::OnCurve => {
                poly.push((p0.0, p0.1));
                idx += 1;
            }
            CurveOrder::QuadraticOffCurve => {
                let p2 = points[(first_on + idx + 2) % len];
                flatten_quad_bezier((p0.0, p0.1), (p1.0, p1.1), (p2.0, p2.1), 6, &mut poly);
                idx += 2;
            }
            CurveOrder::CubicOffCurve => {
                let c2 = points[(first_on + idx + 2) % len];
                let p3 = points[(first_on + idx + 3) % len];
                flatten_cubic_bezier(
                    (p0.0, p0.1),
                    (p1.0, p1.1),
                    (c2.0, c2.1),
                    (p3.0, p3.1),
                    8,
                    &mut poly,
                );
                idx += 3;
            }
        }
    }
    poly
}

/// Largest bitmap side, in pixels, either rasterizer returns.
const MAX_GLYPH_BITMAP_SIDE: f32 = 512.0;

/// Largest distance, in pixels, of a glyph bitmap's corner from the pen.
/// Well inside `i32`, and small enough that every whole pixel position
/// near it is exact in `f32`.
const MAX_GLYPH_OFFSET: f32 = 16_777_216.0;

/// Pixel box `(x0, y0, width, height)` around scaled outline bounds.
///
/// Returns [`None`] when a bound is not finite, when a side is larger than
/// 512 pixels, or when the box sits more than 2^24 pixels from the pen. The
/// sides are measured in `f32` before any integer conversion, so outlines
/// scaled far past the bitmap limit can't overflow or wrap.
fn pixel_bounds(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Option<(i32, i32, u32, u32)> {
    let (left, top, right, bottom) = (min_x.floor(), min_y.floor(), max_x.ceil(), max_y.ceil());
    if ![left, top, right, bottom].iter().all(|v| v.is_finite()) {
        return None;
    }
    let (w, h) = (right - left, bottom - top);
    // `!(a <= b)` is also true for NaN.
    if !(w <= MAX_GLYPH_BITMAP_SIDE && h <= MAX_GLYPH_BITMAP_SIDE) {
        return None;
    }
    if !(left.abs() <= MAX_GLYPH_OFFSET && top.abs() <= MAX_GLYPH_OFFSET) {
        return None;
    }
    Some((
        left as i32,
        top as i32,
        (w as u32).max(1),
        (h as u32).max(1),
    ))
}

/// Fill `outline` with nonzero winding and 4 by 4 coverage samples.
///
/// Contours are flattened, then closed. An edge whose Y delta is at most
/// `1e-5` is dropped, so a horizontal edge is not a crossing and does not
/// divide by zero. A sample counts a crossing when `ymin <= sample_y < ymax`,
/// which gives a shared vertex to the edge that continues below it. Direction
/// is `+1` when the edge moves toward increasing raster y and `-1` otherwise.
/// A nonzero sum is inside, so overlapping contours accumulate instead of
/// toggling even-odd. Sixteen hits become coverage `(hits * 255 + 8) / 16`,
/// which rounds to the nearest byte. A side longer than 512 pixels returns
/// [`None`]. `bearing_y` is the negation of the top edge, so row 0 sits that
/// many pixels above the baseline.
fn rasterize_outline(outline: &GlyphOutline, scale: f32, advance_px: f32) -> Option<GlyphBitmap> {
    rasterize_outline_with_phase(outline, scale, advance_px, 0.0, true)
}

fn rasterize_outline_with_phase(
    outline: &GlyphOutline,
    scale: f32,
    advance_px: f32,
    x_offset_px: f32,
    grid_fit: bool,
) -> Option<GlyphBitmap> {
    let mut segments: Vec<((f32, f32), (f32, f32))> = Vec::new();
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;

    for contour in &outline.contours {
        let poly = flatten_contour_opts(contour, scale, x_offset_px, grid_fit);
        if poly.len() < 2 {
            continue;
        }
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            min_x = min_x.min(a.0).min(b.0);
            min_y = min_y.min(a.1).min(b.1);
            max_x = max_x.max(a.0).max(b.0);
            max_y = max_y.max(a.1).max(b.1);
            // Drop near-horizontal edges. They have no reliable crossing and a zero Y delta.
            if (a.1 - b.1).abs() > 1e-5 {
                segments.push((a, b));
            }
        }
    }
    if segments.is_empty() {
        return None;
    }
    let (x0, y0, width, height) = pixel_bounds(min_x, min_y, max_x, max_y)?;

    let mut coverage = vec![0u8; (width * height) as usize];
    // 4 by 4 subpixels. The coverage divisor below is SUB * SUB, which is 16.
    const SUB: usize = 4;

    for py in 0..height {
        let mut row_hits = vec![0u8; width as usize];
        for sy in 0..SUB {
            let sample_y = (y0 as f32) + (py as f32) + ((sy as f32) + 0.5) / (SUB as f32);
            let mut crossings: Vec<(f32, i32)> = Vec::new();
            for &(a, b) in &segments {
                // dir +1 follows increasing raster y (down the bitmap). The test is half-open.
                let (ymin, ymax, dir) = if a.1 < b.1 {
                    (a.1, b.1, 1)
                } else {
                    (b.1, a.1, -1)
                };
                if sample_y >= ymin && sample_y < ymax {
                    let t = (sample_y - a.1) / (b.1 - a.1);
                    let ix = a.0 + t * (b.0 - a.0);
                    crossings.push((ix, dir));
                }
            }
            if crossings.is_empty() {
                continue;
            }
            crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

            for px in 0..width {
                for sx in 0..SUB {
                    let sample_x = (x0 as f32) + (px as f32) + ((sx as f32) + 0.5) / (SUB as f32);
                    let mut winding = 0i32;
                    for &(cx, dir) in &crossings {
                        // Nonzero winding, not even-odd. `cx == sample_x` still counts.
                        // Crossings are sorted, so the first one to the right ends the scan.
                        if cx <= sample_x {
                            winding += dir;
                        } else {
                            break;
                        }
                    }
                    if winding != 0 {
                        row_hits[px as usize] += 1;
                    }
                }
            }
        }
        for px in 0..width {
            let hits = u32::from(row_hits[px as usize]);
            // +8 rounds to nearest over 16 samples. hits is at most 16, so the byte stays in 0..=255.
            coverage[(py * width + px) as usize] = ((hits * 255 + 8) / 16).min(255) as u8;
        }
    }

    Some(GlyphBitmap {
        width,
        height,
        bearing_x: x0,
        // y0 is the top in y-down space, so -y0 is how far row 0 sits above the baseline.
        bearing_y: -y0,
        advance_px,
        coverage,
    })
}

/// Fill `outline` at 3x horizontal RGB subpixel resolution (12 by 4 samples per pixel)
/// and apply a `[1, 2, 1] / 4` horizontal FIR filter across subpixel columns.
fn rasterize_outline_subpixel_lcd(
    outline: &GlyphOutline,
    scale: f32,
    advance_px: f32,
) -> Option<SubpixelGlyphBitmap> {
    let x_offset_px = 0.0f32;
    let grid_fit = true;
    let mut segments: Vec<((f32, f32), (f32, f32))> = Vec::new();
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;

    for contour in &outline.contours {
        let poly = flatten_contour_opts(contour, scale, x_offset_px, grid_fit);
        if poly.len() < 2 {
            continue;
        }
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            min_x = min_x.min(a.0).min(b.0);
            min_y = min_y.min(a.1).min(b.1);
            max_x = max_x.max(a.0).max(b.0);
            max_y = max_y.max(a.1).max(b.1);
            if (a.1 - b.1).abs() > 1e-5 {
                segments.push((a, b));
            }
        }
    }
    if segments.is_empty() {
        return None;
    }
    let (x0, y0, width, height) = pixel_bounds(min_x, min_y, max_x, max_y)?;

    let sub_cols = (width as usize) * 3;
    let mut rgb_coverage = vec![0u8; sub_cols * (height as usize)];
    const SUB_X_PER_CHANNEL: usize = 4;
    const SUB_Y: usize = 4;

    for py in 0..height {
        let mut raw_sub = vec![0u8; sub_cols];
        for sy in 0..SUB_Y {
            let sample_y = (y0 as f32) + (py as f32) + ((sy as f32) + 0.5) / (SUB_Y as f32);
            let mut crossings: Vec<(f32, i32)> = Vec::new();
            for &(a, b) in &segments {
                let (ymin, ymax, dir) = if a.1 < b.1 {
                    (a.1, b.1, 1)
                } else {
                    (b.1, a.1, -1)
                };
                if sample_y >= ymin && sample_y < ymax {
                    let t = (sample_y - a.1) / (b.1 - a.1);
                    let ix = a.0 + t * (b.0 - a.0);
                    crossings.push((ix, dir));
                }
            }
            if crossings.is_empty() {
                continue;
            }
            crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

            for col in 0..sub_cols {
                for sx in 0..SUB_X_PER_CHANNEL {
                    let sub_idx = col * SUB_X_PER_CHANNEL + sx;
                    let sample_x =
                        (x0 as f32) + ((sub_idx as f32) + 0.5) / ((3 * SUB_X_PER_CHANNEL) as f32);
                    let mut winding = 0i32;
                    for &(cx, dir) in &crossings {
                        if cx <= sample_x {
                            winding += dir;
                        } else {
                            break;
                        }
                    }
                    if winding != 0 {
                        raw_sub[col] += 1;
                    }
                }
            }
        }

        let mut unfiltered = vec![0u32; sub_cols];
        for col in 0..sub_cols {
            let hits = u32::from(raw_sub[col]);
            unfiltered[col] = ((hits * 255 + 8) / 16).min(255);
        }

        // 3-tap [1, 2, 1] / 4 horizontal FIR filter across subpixel columns.
        let row_base = (py as usize) * sub_cols;
        for col in 0..sub_cols {
            let left = if col > 0 {
                unfiltered[col - 1]
            } else {
                unfiltered[col]
            };
            let center = unfiltered[col];
            let right = if col + 1 < sub_cols {
                unfiltered[col + 1]
            } else {
                unfiltered[col]
            };
            rgb_coverage[row_base + col] = ((left + center * 2 + right + 2) / 4).min(255) as u8;
        }
    }

    Some(SubpixelGlyphBitmap {
        width,
        height,
        bearing_x: x0,
        bearing_y: -y0,
        advance_px,
        rgb_coverage,
    })
}

/// Convert an 8-bit sRGB channel (`0..=255`) to 12-bit linear light (`0..=4095`).
#[inline]
pub fn srgb_to_linear_u12(srgb: u8) -> u16 {
    static LUT: std::sync::OnceLock<[u16; 256]> = std::sync::OnceLock::new();
    let table = LUT.get_or_init(|| {
        let mut t = [0u16; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let s = (i as f32) / 255.0;
            let lin = if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            };
            *slot = (lin * 4095.0).round().clamp(0.0, 4095.0) as u16;
        }
        t
    });
    // SAFETY: `srgb as usize` is always in `0..256`.
    unsafe { *table.get_unchecked(srgb as usize) }
}

/// Convert a 12-bit linear light value (`0..=4095`) back to 8-bit sRGB (`0..=255`).
#[inline]
pub fn linear_u12_to_srgb(linear: u16) -> u8 {
    static LUT: std::sync::OnceLock<[u8; 4096]> = std::sync::OnceLock::new();
    let table = LUT.get_or_init(|| {
        let mut t = [0u8; 4096];
        for (i, slot) in t.iter_mut().enumerate() {
            let lin = (i as f32) / 4095.0;
            let s = if lin <= 0.0031308 {
                lin * 12.92
            } else {
                1.055 * lin.powf(1.0 / 2.4) - 0.055
            };
            *slot = (s * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        t
    });
    let idx = (linear as usize).min(4095);
    // SAFETY: `idx` is clamped to `0..=4095`.
    unsafe { *table.get_unchecked(idx) }
}

/// Luminance-aware stem contrast curve (`coverage` in `0..=255`, `fg_luma` in `0..=255`).
///
/// Boosts mid-tone coverage on dark-on-light text so thin 1px stems remain crisp and solid,
/// while gently moderating light-on-dark stems to prevent linear-space haloing.
#[inline]
pub fn enhance_stem_contrast(coverage: u8, fg_luma: u8) -> u8 {
    if coverage == 0 || coverage == 255 {
        return coverage;
    }
    let a = u32::from(coverage);
    let inv = 255 - a;
    let boost = (a * inv) / 255;
    let dark_weight = u32::from(255 - fg_luma);
    let delta = (boost * (dark_weight + 64)) / 512;
    (a + delta).min(255) as u8
}

/// Blend a foreground RGB text pixel over a destination RGB pixel in gamma-correct linear light space
/// with luminance-aware stem contrast enhancement.
#[inline]
pub fn blend_glyph_pixel_gamma_correct(
    dst_rgb: (u8, u8, u8),
    fg_rgb: (u8, u8, u8),
    coverage: u8,
) -> (u8, u8, u8) {
    if coverage == 0 {
        return dst_rgb;
    }
    if coverage == 255 {
        return fg_rgb;
    }
    let fg_luma =
        ((u32::from(fg_rgb.0) * 54 + u32::from(fg_rgb.1) * 183 + u32::from(fg_rgb.2) * 19) >> 8)
            as u8;
    let alpha = u32::from(enhance_stem_contrast(coverage, fg_luma));
    let inv_alpha = 255 - alpha;

    let dr = u32::from(srgb_to_linear_u12(dst_rgb.0));
    let dg = u32::from(srgb_to_linear_u12(dst_rgb.1));
    let db = u32::from(srgb_to_linear_u12(dst_rgb.2));

    let sr = u32::from(srgb_to_linear_u12(fg_rgb.0));
    let sg = u32::from(srgb_to_linear_u12(fg_rgb.1));
    let sb = u32::from(srgb_to_linear_u12(fg_rgb.2));

    let or = ((sr * alpha + dr * inv_alpha + 127) / 255) as u16;
    let og = ((sg * alpha + dg * inv_alpha + 127) / 255) as u16;
    let ob = ((sb * alpha + db * inv_alpha + 127) / 255) as u16;

    (
        linear_u12_to_srgb(or),
        linear_u12_to_srgb(og),
        linear_u12_to_srgb(ob),
    )
}

/// Blend a 3-channel RGB subpixel LCD coverage triple over `dst_rgb` in gamma-correct linear space.
#[inline]
pub fn blend_subpixel_lcd_gamma_correct(
    dst_rgb: (u8, u8, u8),
    fg_rgb: (u8, u8, u8),
    cov_rgb: (u8, u8, u8),
) -> (u8, u8, u8) {
    if cov_rgb == (0, 0, 0) {
        return dst_rgb;
    }
    if cov_rgb == (255, 255, 255) {
        return fg_rgb;
    }
    let fg_luma =
        ((u32::from(fg_rgb.0) * 54 + u32::from(fg_rgb.1) * 183 + u32::from(fg_rgb.2) * 19) >> 8)
            as u8;
    let ar = u32::from(enhance_stem_contrast(cov_rgb.0, fg_luma));
    let ag = u32::from(enhance_stem_contrast(cov_rgb.1, fg_luma));
    let ab = u32::from(enhance_stem_contrast(cov_rgb.2, fg_luma));

    let dr = u32::from(srgb_to_linear_u12(dst_rgb.0));
    let dg = u32::from(srgb_to_linear_u12(dst_rgb.1));
    let db = u32::from(srgb_to_linear_u12(dst_rgb.2));

    let sr = u32::from(srgb_to_linear_u12(fg_rgb.0));
    let sg = u32::from(srgb_to_linear_u12(fg_rgb.1));
    let sb = u32::from(srgb_to_linear_u12(fg_rgb.2));

    let or = ((sr * ar + dr * (255 - ar) + 127) / 255) as u16;
    let og = ((sg * ag + dg * (255 - ag) + 127) / 255) as u16;
    let ob = ((sb * ab + db * (255 - ab) + 127) / 255) as u16;

    (
        linear_u12_to_srgb(or),
        linear_u12_to_srgb(og),
        linear_u12_to_srgb(ob),
    )
}

#[cfg(test)]
mod tests {
    //! Synthetic SFNT bytes for table parsing, advances, and the coverage rasterizer.

    use super::*;

    fn build_minimal_synthetic_ttf() -> Vec<u8> {
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        head[50..52].copy_from_slice(&1i16.to_be_bytes());

        let mut hhea = vec![0u8; 36];
        hhea[4..6].copy_from_slice(&800i16.to_be_bytes());
        hhea[6..8].copy_from_slice(&(-200i16).to_be_bytes());
        hhea[8..10].copy_from_slice(&100i16.to_be_bytes());
        hhea[34..36].copy_from_slice(&3u16.to_be_bytes());

        let mut maxp = vec![0u8; 6];
        maxp[4..6].copy_from_slice(&3u16.to_be_bytes());

        let mut hmtx = Vec::new();
        for &(adv, lsb) in &[(500u16, 0i16), (700u16, 20i16), (300u16, 10i16)] {
            hmtx.extend_from_slice(&adv.to_be_bytes());
            hmtx.extend_from_slice(&lsb.to_be_bytes());
        }

        let mut glyf = Vec::new();
        glyf.extend_from_slice(&1i16.to_be_bytes());
        glyf.extend_from_slice(&0i16.to_be_bytes());
        glyf.extend_from_slice(&0i16.to_be_bytes());
        glyf.extend_from_slice(&600i16.to_be_bytes());
        glyf.extend_from_slice(&700i16.to_be_bytes());
        glyf.extend_from_slice(&3u16.to_be_bytes());
        glyf.extend_from_slice(&0u16.to_be_bytes());
        for _ in 0..4 {
            glyf.push(0x01);
        }
        for &dx in &[0i16, 600, 0, -600] {
            glyf.extend_from_slice(&dx.to_be_bytes());
        }
        for &dy in &[0i16, 0, 700, -700] {
            glyf.extend_from_slice(&dy.to_be_bytes());
        }
        let glyph1_len = glyf.len() as u32;

        let mut loca = Vec::new();
        loca.extend_from_slice(&0u32.to_be_bytes());
        loca.extend_from_slice(&0u32.to_be_bytes());
        loca.extend_from_slice(&glyph1_len.to_be_bytes());
        loca.extend_from_slice(&glyph1_len.to_be_bytes());

        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&1u16.to_be_bytes());
        cmap.extend_from_slice(&3u16.to_be_bytes());
        cmap.extend_from_slice(&10u16.to_be_bytes());
        cmap.extend_from_slice(&12u32.to_be_bytes());
        cmap.extend_from_slice(&12u16.to_be_bytes());
        cmap.extend_from_slice(&0u16.to_be_bytes());
        cmap.extend_from_slice(&40u32.to_be_bytes());
        cmap.extend_from_slice(&0u32.to_be_bytes());
        cmap.extend_from_slice(&2u32.to_be_bytes());
        cmap.extend_from_slice(&65u32.to_be_bytes());
        cmap.extend_from_slice(&65u32.to_be_bytes());
        cmap.extend_from_slice(&1u32.to_be_bytes());
        cmap.extend_from_slice(&105u32.to_be_bytes());
        cmap.extend_from_slice(&105u32.to_be_bytes());
        cmap.extend_from_slice(&2u32.to_be_bytes());

        let tables: [(&[u8; 4], &[u8]); 8] = [
            (b"cmap", &cmap),
            (b"glyf", &glyf),
            (b"head", &head),
            (b"hhea", &hhea),
            (b"hmtx", &hmtx),
            (b"loca", &loca),
            (b"maxp", &maxp),
            (
                b"name",
                &[
                    0, 0, 0, 1, 0, 18, 0, 1, 0, 0, 0, 0, 0, 1, 0, 10, 0, 0, b'A', b'n', b'v', b'i',
                    b'l', b' ', b'S', b'a', b'n', b's',
                ],
            ),
        ];

        let mut sfnt = Vec::new();
        sfnt.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        sfnt.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        sfnt.extend_from_slice(&64u16.to_be_bytes());
        sfnt.extend_from_slice(&2u16.to_be_bytes());
        sfnt.extend_from_slice(&16u16.to_be_bytes());

        let mut offset = 12 + tables.len() * 16;
        for &(tag, data) in &tables {
            sfnt.extend_from_slice(tag);
            sfnt.extend_from_slice(&0u32.to_be_bytes());
            sfnt.extend_from_slice(&(offset as u32).to_be_bytes());
            sfnt.extend_from_slice(&(data.len() as u32).to_be_bytes());
            offset += data.len();
        }
        for &(_, data) in &tables {
            sfnt.extend_from_slice(data);
        }
        sfnt
    }

    #[test]
    fn parses_sfnt_tables_and_computes_proportional_advances() {
        let bytes = build_minimal_synthetic_ttf();
        let font = TtfFont::parse(&bytes).unwrap();
        assert_eq!(font.family_name(), "Anvil Sans");
        assert_eq!(font.units_per_em, 1000);
        assert_eq!(font.glyph_id('A'), 1);
        assert_eq!(font.glyph_id('i'), 2);
        assert!((font.glyph_advance_px('A', 20.0) - 14.0).abs() < 0.001);
        assert!((font.glyph_advance_px('i', 20.0) - 6.0).abs() < 0.001);
        assert!((font.measure_text("AAi", 20.0) - 34.0).abs() < 0.001);

        let mut bad_header = vec![0u8; 16];
        bad_header[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        bad_header[4..6].copy_from_slice(&1500u16.to_be_bytes());
        assert_eq!(
            TtfFont::parse(&bad_header).unwrap_err(),
            TtfError::TooManyTables { count: 1500 }
        );
    }

    #[test]
    fn proportional_fallback_distinguishes_narrow_and_wide_glyphs() {
        let narrow = measure_proportional_fallback("iiii", 16.0);
        let wide = measure_proportional_fallback("MMMM", 16.0);
        assert!(wide > narrow * 2.0);
    }

    #[test]
    fn parses_glyf_outlines_and_rasterizes_coverage_bitmap() {
        let bytes = build_minimal_synthetic_ttf();
        let font = TtfFont::parse(&bytes).unwrap();
        let outline = font.glyph_outline(1).expect("glyph 1 outline");
        assert_eq!(outline.contours.len(), 1);
        assert_eq!(outline.contours[0].len(), 4);

        let bitmap = font.rasterize_glyph('A', 20.0).expect("rasterized A");
        assert!(bitmap.width >= 10 && bitmap.height >= 12);
        assert!(bitmap.coverage.iter().any(|&alpha| alpha == 255));
    }

    #[test]
    fn parses_gpos_pair_pos_kerning_cubic_contours_and_subpixel_lcd_glyphs() {
        let mut gpos = Vec::new();
        // GPOS v1.0 header (10 bytes): major=1, minor=0, scriptList=0, featureList=0, lookupList=10
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&10u16.to_be_bytes());
        // LookupList at offset 10: lookupCount=1, lookupOffsets[0]=4 (offset 14)
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&4u16.to_be_bytes());
        // Lookup at offset 14: lookupType=2 (PairPos), lookupFlag=0, subTableCount=1, subtableOffsets[0]=8 (offset 22)
        gpos.extend_from_slice(&2u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&8u16.to_be_bytes());
        // PairPosFormat1 at offset 22:
        // posFormat=1, coverageOffset=12 (offset 34), valueFormat1=0x0004 (XAdvance), valueFormat2=0, pairSetCount=1, pairSetOffset[0]=18 (offset 40)
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&12u16.to_be_bytes());
        gpos.extend_from_slice(&0x0004u16.to_be_bytes());
        gpos.extend_from_slice(&0u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&18u16.to_be_bytes());
        // Coverage Format 1 at offset 34 (6 bytes): coverageFormat=1, glyphCount=1, glyphArray[0]=1 ('A')
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&1u16.to_be_bytes());
        // PairSet at offset 40 (6 bytes): pairValueCount=1, secondGlyph=2 ('i'), value1.xAdvance=-80
        gpos.extend_from_slice(&1u16.to_be_bytes());
        gpos.extend_from_slice(&2u16.to_be_bytes());
        gpos.extend_from_slice(&(-80i16).to_be_bytes());

        let mut pairs = HashMap::new();
        parse_gpos(&gpos, &mut pairs);
        assert_eq!(pairs.get(&(1, 2)), Some(&-80));

        // Cubic contour flattening
        let cubic_pts = [
            (0.0, 0.0, CurveOrder::OnCurve),
            (4.0, 10.0, CurveOrder::CubicOffCurve),
            (8.0, 10.0, CurveOrder::CubicOffCurve),
            (12.0, 0.0, CurveOrder::OnCurve),
        ];
        let poly = flatten_cubic_contour(&cubic_pts);
        assert_eq!(poly.len(), 9);

        // 3x horizontal subpixel LCD coverage rasterization
        let bytes = build_minimal_synthetic_ttf();
        let font = TtfFont::parse(&bytes).unwrap();
        let lcd = font
            .rasterize_glyph_subpixel_lcd('A', 20.0)
            .expect("subpixel LCD A");
        assert_eq!(
            lcd.rgb_coverage.len(),
            (lcd.width * lcd.height * 3) as usize
        );
        assert!(lcd.rgb_coverage.iter().any(|&v| v == 255));

        // CFF Type 2 CharString interpreter & CFF 1.0 table parser:
        // Encode numbers in Type 2 single-byte form (v + 139):
        // rmoveto(50, 0), rrcurveto(0, 60, 40, 40, 60, 0), vlineto(-100), hlineto(-100), endchar
        let prog = [
            189u8, 139, 21, // 50, 0, rmoveto
            139, 199, 179, 179, 199, 139, 8, // 0, 60, 40, 40, 60, 0, rrcurveto
            39, 7, // -100, vlineto
            39, 6,  // -100, hlineto
            14, // endchar
        ];
        let cff_outline = interpret_type2_charstring(&prog).expect("CFF Type 2 outline");
        assert_eq!(cff_outline.contours.len(), 1);
        assert!(cff_outline.contours[0].len() >= 10);
        assert_eq!(cff_outline.x_min, 50);
        assert_eq!(cff_outline.x_max, 150);

        // Build a synthetic CFF 1.0 table with Name INDEX, Top DICT INDEX (op 17 -> offset 17), and CharStrings INDEX
        let mut cff = Vec::new();
        cff.extend_from_slice(&[1, 0, 4, 1]); // CFF header (4 bytes)
        cff.extend_from_slice(&0u16.to_be_bytes()); // Name INDEX (empty, 2 bytes -> offset 6)
        // Top DICT INDEX at offset 6: count=1 (2B), offSize=1 (1B), offsets=[1, 3] (2B), data=[156 (17+139), 17 (op CharStrings)] (2B) -> ends at offset 13
        cff.extend_from_slice(&1u16.to_be_bytes());
        cff.push(1);
        cff.extend_from_slice(&[1, 3]);
        cff.extend_from_slice(&[13 + 139, 17]);
        // CharStrings INDEX at offset 13: count=2 (.notdef + glyph 1)
        cff.extend_from_slice(&2u16.to_be_bytes());
        cff.push(1);
        let notdef_prog = [14u8];
        let g1_end = 1 + notdef_prog.len() + prog.len();
        cff.extend_from_slice(&[1, (1 + notdef_prog.len()) as u8, g1_end as u8]);
        cff.extend_from_slice(&notdef_prog);
        cff.extend_from_slice(&prog);

        let cff_outlines = parse_cff_table(&cff, 2).expect("parse CFF table");
        assert!(cff_outlines[0].is_none());
        assert!(cff_outlines[1].is_some());

        // Subroutine calls: bias for < 1240 subrs is 107, so index 0 is biased -107 (encoded as -107 + 139 = 32)
        let local_sub0: &[u8] = &[189, 139, 21, 189, 6, 11]; // rmoveto(50, 0), hlineto(50), return
        let global_sub0: &[u8] = &[189, 7, 89, 6, 14]; // vlineto(50), hlineto(-50), endchar
        let main_with_subrs: &[u8] = &[32, 10, 32, 29]; // callsubr(0), callgsubr(0)
        let subr_outline =
            interpret_type2_charstring_with_subrs(main_with_subrs, &[local_sub0], &[global_sub0])
                .expect("subr outline");
        assert_eq!(subr_outline.contours.len(), 1);
        assert_eq!(subr_outline.x_min, 50);
        assert_eq!(subr_outline.x_max, 100);

        let bytes = build_minimal_synthetic_ttf();
        let font = TtfFont::parse(&bytes).unwrap();
        let p0 = font
            .rasterize_glyph_subpixel_positioned('A', 20.0, 0)
            .expect("phase 0");
        let p2 = font
            .rasterize_glyph_subpixel_positioned('A', 20.0, 2)
            .expect("phase 2");
        assert_eq!(p0.height, p2.height);
        assert!(enhance_stem_contrast(128, 16) > 128);
        let blended = blend_glyph_pixel_gamma_correct((255, 255, 255), (0, 0, 0), 128);
        assert!(blended.0 > 0 && blended.0 < 255);
        let lcd_blended =
            blend_subpixel_lcd_gamma_correct((20, 20, 24), (240, 240, 245), (120, 180, 120));
        assert!(lcd_blended.1 >= lcd_blended.0);

        let bold_bmp = font
            .rasterize_glyph_styled('A', 20.0, 0, true, false)
            .expect("synthetic bold");
        assert_eq!(bold_bmp.width, p0.width + 1);
        assert!(bold_bmp.advance_px > p0.advance_px);
        let italic_bmp = font
            .rasterize_glyph_styled('A', 20.0, 0, false, true)
            .expect("synthetic italic");
        assert!(italic_bmp.width >= p0.width);
        let _ = TtfFont::load_system_font_slot(FontFamilySlot::SansRegular);
    }
}
