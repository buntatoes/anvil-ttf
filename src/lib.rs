//! TrueType and OpenType SFNT parser, glyph outlines, and an 8-bit coverage rasterizer.
//!
//! Tables read from the font are `name`, `head`, `hhea`, `maxp`, `cmap`, `hmtx`,
//! optional `kern` format 0, and optional `loca` plus `glyf`. Multi-byte values
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
//! root-layout uses the metric helpers to measure text. root-raster on Linux
//! calls [`TtfFont::parse`] when a system `.ttf` is found, then
//! [`TtfFont::rasterize_glyph`]. If that fails, the browser draws a built-in
//! bitmap font instead. This crate does not shape complex scripts, hint
//! outlines, decode CFF glyphs, or use a GPU.
//!
//! File size, table count, contour count, and compound-glyph depth are capped.

use std::collections::HashMap;
use std::fmt;

/// Maximum font binary size accepted (`32 MiB`).
pub const MAX_FONT_BYTES: usize = 32 * 1024 * 1024;

/// Maximum number of SFNT directory tables accepted (`128`).
pub const MAX_SFNT_TABLES: usize = 128;

/// Maximum contours per glyph outline (`256`).
pub const MAX_GLYPH_CONTOURS: usize = 256;

/// Maximum control points per glyph outline (`4,096`).
pub const MAX_CONTOUR_POINTS: usize = 4096;

/// Maximum compound glyph recursion depth (`16`).
pub const MAX_COMPOSITE_GLYPH_DEPTH: u8 = 16;

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
        let kern_pairs = if let Some(&(off, len)) = tables.get(b"kern") {
            parse_kern(&bytes[off..off + len]).unwrap_or_default()
        } else {
            HashMap::new()
        };

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
    /// be read, or [`TtfFont::parse`] fails. root-layout calls this for
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
    /// multiplied by `font_size_px / units_per_em`. root-layout uses this when
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
    /// Returns [`None`] when the character has no contours, or when the bitmap
    /// would be empty or larger than 512 pixels on a side. The outline is scaled
    /// by `max(font_size_px, 1) / max(units_per_em, 1)` and filled with nonzero
    /// winding at 4 by 4 samples per pixel. See [`GlyphBitmap`] for coverage
    /// and y direction. root-raster uses this for Linux glyphs.
    pub fn rasterize_glyph(&self, ch: char, font_size_px: f32) -> Option<GlyphBitmap> {
        let gid = self.glyph_id(ch);
        let outline = self.glyph_outline(gid)?;
        if outline.contours.is_empty() {
            return None;
        }
        let scale = font_size_px.max(1.0) / (self.units_per_em.max(1) as f32);
        let advance_px = (self.glyph_metric(gid).advance_width as f32) * scale;
        rasterize_outline(outline, scale, advance_px)
    }
}

/// Estimate a string width from fixed em ratios when no `.ttf` is loaded.
///
/// Each character adds `font_size_px` times a ratio: space is 0.30, narrow marks
/// and `i` / `l` are 0.28, `I` / `j` / `f` / `t` / `r` and brackets are 0.36,
/// `m` / `w` / `M` / `W` / `@` / `%` are 0.82, other capitals are 0.66, digits
/// are 0.56, and every other scalar is 0.54. There is no kerning. root-layout
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
fn flatten_contour(points: &[OutlinePoint], scale: f32) -> Vec<(f32, f32)> {
    if points.len() < 2 {
        return Vec::new();
    }
    let mut expanded: Vec<(f32, f32, bool)> = Vec::new();
    let n = points.len();
    for i in 0..n {
        let cur = points[i];
        let next = points[(i + 1) % n];
        let cx = (cur.x as f32) * scale;
        // Negate font y so row 0 of the bitmap is the top of the glyph.
        let cy = -(cur.y as f32) * scale;
        expanded.push((cx, cy, cur.on_curve));
        // Consecutive off-curve points imply an on-curve point halfway between them.
        if !cur.on_curve && !next.on_curve {
            let nx = (next.x as f32) * scale;
            let ny = -(next.y as f32) * scale;
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
    let mut segments: Vec<((f32, f32), (f32, f32))> = Vec::new();
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;

    for contour in &outline.contours {
        let poly = flatten_contour(contour, scale);
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
    if segments.is_empty() || !min_x.is_finite() {
        return None;
    }

    let x0 = min_x.floor() as i32;
    let y0 = min_y.floor() as i32;
    let x1 = max_x.ceil() as i32;
    let y1 = max_y.ceil() as i32;
    let width = (x1 - x0).max(1) as u32;
    let height = (y1 - y0).max(1) as u32;
    if width > 512 || height > 512 {
        return None;
    }

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
                    0, 0, 0, 1, 0, 18, 0, 1, 0, 0, 0, 0, 0, 1, 0, 9, 0, 0, b'F', b'l', b'u', b'x',
                    b' ', b'S', b'a', b'n', b's',
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
        assert_eq!(font.family_name(), "Root Sans");
        assert_eq!(font.units_per_em, 1000);
        assert_eq!(font.glyph_id('A'), 1);
        assert_eq!(font.glyph_id('i'), 2);
        assert!((font.glyph_advance_px('A', 20.0) - 14.0).abs() < 0.001);
        assert!((font.glyph_advance_px('i', 20.0) - 6.0).abs() < 0.001);
        assert!((font.measure_text("AAi", 20.0) - 34.0).abs() < 0.001);

        let mut bad_header = vec![0u8; 16];
        bad_header[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        bad_header[4..6].copy_from_slice(&500u16.to_be_bytes());
        assert_eq!(
            TtfFont::parse(&bad_header).unwrap_err(),
            TtfError::TooManyTables { count: 500 }
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
}
