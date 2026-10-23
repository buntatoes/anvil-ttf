//! TrueType/OpenType SFNT font parser, glyph outline decoder, and anti-aliased rasterizer (Proof of Concept).
//!
//! Standalone `MPL-2.0` companion library for the Flux Browser Proof of Concept ecosystem.

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TtfError {
    UnexpectedEof,
    FontTooLarge { size: usize },
    TooManyTables { count: usize },
    InvalidSfntSignature,
    MissingTable(&'static str),
    InvalidTable(&'static str),
}

impl fmt::Display for TtfError {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HMetric {
    pub advance_width: u16,
    pub left_side_bearing: i16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutlinePoint {
    pub x: i16,
    pub y: i16,
    pub on_curve: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlyphOutline {
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
    pub contours: Vec<Vec<OutlinePoint>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance_px: f32,
    pub coverage: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct TtfFont {
    pub units_per_em: u16,
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
    family_name: String,
    cmap: HashMap<u32, u16>,
    metrics: Vec<HMetric>,
    kern_pairs: HashMap<(u16, u16), i16>,
    outlines: Vec<Option<GlyphOutline>>,
}

impl TtfFont {
    /// Parse a TrueType or OpenType font binary into a [`TtfFont`].
    pub fn parse(bytes: &[u8]) -> Result<Self, TtfError> {
        if bytes.len() > MAX_FONT_BYTES {
            return Err(TtfError::FontTooLarge { size: bytes.len() });
        }
        if bytes.len() < 12 {
            return Err(TtfError::UnexpectedEof);
        }
        let sfnt_version = read_u32_be(bytes, 0)?;
        if sfnt_version != 0x0001_0000
            && sfnt_version != 0x7472_7565 // 'true'
            && sfnt_version != 0x4F54_544F
        // 'OTTO'
        {
            return Err(TtfError::InvalidSfntSignature);
        }

        let num_tables = read_u16_be(bytes, 4)? as usize;
        if num_tables > MAX_SFNT_TABLES {
            return Err(TtfError::TooManyTables { count: num_tables });
        }
        if 12 + num_tables * 16 > bytes.len() {
            return Err(TtfError::UnexpectedEof);
        }

        let mut tables: HashMap<[u8; 4], (usize, usize)> = HashMap::new();
        for i in 0..num_tables {
            let rec = 12 + i * 16;
            let tag = [bytes[rec], bytes[rec + 1], bytes[rec + 2], bytes[rec + 3]];
            let offset = read_u32_be(bytes, rec + 8)? as usize;
            let length = read_u32_be(bytes, rec + 12)? as usize;
            if offset.saturating_add(length) > bytes.len() {
                return Err(TtfError::UnexpectedEof);
            }
            tables.insert(tag, (offset, length));
        }

        let head = slice_table(bytes, &tables, b"head", "head")?;
        if head.len() < 54 {
            return Err(TtfError::InvalidTable("head"));
        }
        let units_per_em = read_u16_be(head, 18)?;
        if units_per_em == 0 {
            return Err(TtfError::InvalidTable("head"));
        }
        let index_to_loc_format = read_i16_be(head, 50)?;

        let hhea = slice_table(bytes, &tables, b"hhea", "hhea")?;
        if hhea.len() < 36 {
            return Err(TtfError::InvalidTable("hhea"));
        }
        let ascender = read_i16_be(hhea, 4)?;
        let descender = read_i16_be(hhea, 6)?;
        let line_gap = read_i16_be(hhea, 8)?;
        let num_h_metrics = read_u16_be(hhea, 34)? as usize;

        let maxp = slice_table(bytes, &tables, b"maxp", "maxp")?;
        if maxp.len() < 6 {
            return Err(TtfError::InvalidTable("maxp"));
        }
        let num_glyphs = read_u16_be(maxp, 4)? as usize;

        let hmtx = slice_table(bytes, &tables, b"hmtx", "hmtx")?;
        let metrics = parse_hmtx(hmtx, num_h_metrics, num_glyphs)?;

        let cmap_table = slice_table(bytes, &tables, b"cmap", "cmap")?;
        let cmap = parse_cmap(cmap_table)?;

        let kern_pairs = if let Some(&(off, len)) = tables.get(b"kern") {
            parse_kern(&bytes[off..off + len]).unwrap_or_default()
        } else {
            HashMap::new()
        };

        let family_name = if let Some(&(off, len)) = tables.get(b"name") {
            parse_name_table(&bytes[off..off + len]).unwrap_or_default()
        } else {
            String::new()
        };

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

    /// Return the font family name extracted from the SFNT `name` table, if present.
    pub fn family_name(&self) -> &str {
        &self.family_name
    }

    /// Attempt to load a system TrueType font (`Segoe UI` or `Consolas` on Windows).
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

    pub fn glyph_id(&self, ch: char) -> u16 {
        self.cmap.get(&(ch as u32)).copied().unwrap_or(0)
    }

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

    pub fn kerning_units(&self, left_glyph: u16, right_glyph: u16) -> i16 {
        self.kern_pairs
            .get(&(left_glyph, right_glyph))
            .copied()
            .unwrap_or(0)
    }

    /// Return the pixel advance width of `ch` at `font_size_px`.
    pub fn glyph_advance_px(&self, ch: char, font_size_px: f32) -> f32 {
        let gid = self.glyph_id(ch);
        let m = self.glyph_metric(gid);
        (m.advance_width as f32) * font_size_px / (self.units_per_em as f32)
    }

    /// Measure the horizontal advance width in pixels of `text` at `font_size_px`.
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

    /// Recommended pixel line height (`ascender - descender + line_gap`) at `font_size_px`.
    pub fn line_height_px(&self, font_size_px: f32) -> f32 {
        let units = i32::from(self.ascender) - i32::from(self.descender) + i32::from(self.line_gap);
        (units.max(i32::from(self.units_per_em)) as f32) * font_size_px / (self.units_per_em as f32)
    }

    /// Return the parsed [`GlyphOutline`] for `glyph_id` if present in the `glyf` table.
    pub fn glyph_outline(&self, glyph_id: u16) -> Option<&GlyphOutline> {
        self.outlines
            .get(glyph_id as usize)
            .and_then(|o| o.as_ref())
    }

    /// Rasterize the vector outline of `ch` at `font_size_px` using 4x4 supersampled scanlines.
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

/// Measure text width using calibrated proportional character ratios when no `.ttf` file is loaded.
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

fn slice_table<'a>(
    bytes: &'a [u8],
    tables: &HashMap<[u8; 4], (usize, usize)>,
    tag: &[u8; 4],
    name: &'static str,
) -> Result<&'a [u8], TtfError> {
    let &(offset, length) = tables.get(tag).ok_or(TtfError::MissingTable(name))?;
    Ok(&bytes[offset..offset + length])
}

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
        // Accept Unicode (platform 0) or Windows Unicode BMP/UCS-4 (platform 3, encoding 1 or 10)
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

fn parse_cmap_format4(sub: &[u8], out: &mut HashMap<u32, u16>) -> Result<(), TtfError> {
    if sub.len() < 14 {
        return Err(TtfError::InvalidTable("cmap"));
    }
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

        if start_code == 0xFFFF && end_code == 0xFFFF {
            break;
        }
        for code in start_code..=end_code {
            let gid = if id_range_offset == 0 {
                (i32::from(code) + i32::from(id_delta)) as u16
            } else {
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
        if end_code.saturating_sub(start_code) > 0xFFFF {
            continue;
        }
        for code in start_code..=end_code {
            let gid = (start_gid + (code - start_code)) as u16;
            if gid != 0 {
                out.insert(code, gid);
            }
        }
    }
    Ok(())
}

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
        let format = coverage >> 8;
        if format == 0 && pos + 14 <= kern.len() {
            let n_pairs = read_u16_be(kern, pos + 6)? as usize;
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
        if sub_len == 0 {
            break;
        }
        pos += sub_len;
    }
    Ok(pairs)
}

fn read_u16_be(bytes: &[u8], offset: usize) -> Result<u16, TtfError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or(TtfError::UnexpectedEof)?;
    Ok(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_i16_be(bytes: &[u8], offset: usize) -> Result<i16, TtfError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or(TtfError::UnexpectedEof)?;
    Ok(i16::from_be_bytes([slice[0], slice[1]]))
}

fn read_u32_be(bytes: &[u8], offset: usize) -> Result<u32, TtfError> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(TtfError::UnexpectedEof)?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

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
        if start >= end || end > glyf.len() {
            outlines.push(None);
            continue;
        }
        let slice = &glyf[start..end];
        outlines.push(parse_single_glyph(slice, glyf, &offsets, 0).ok().flatten());
    }
    Ok(outlines)
}

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
        if name_id != 1 && name_id != 4 {
            continue;
        }
        let start = string_offset.saturating_add(offset);
        let end = start.saturating_add(length);
        if end > name_tbl.len() || length == 0 || length > 256 {
            continue;
        }
        let raw = &name_tbl[start..end];
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
    let mut pos = num_contours * 2 + 2 + insn_len;
    if pos > data.len() {
        return Err(TtfError::InvalidTable("glyf"));
    }

    let mut flags = Vec::with_capacity(total_pts);
    while flags.len() < total_pts {
        let f = *data.get(pos).ok_or(TtfError::UnexpectedEof)?;
        pos += 1;
        flags.push(f);
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
        if end >= total_pts || end < start {
            break;
        }
        let mut contour = Vec::with_capacity(end - start + 1);
        for idx in start..=end {
            contour.push(OutlinePoint {
                x: xs[idx],
                y: ys[idx],
                on_curve: (flags[idx] & 0x01) != 0,
            });
        }
        contours.push(contour);
        start = end + 1;
    }
    Ok(contours)
}

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

        let args_words = (flags & 0x0001) != 0;
        let args_xy = (flags & 0x0002) != 0;
        let (dx, dy) = if args_words {
            if pos + 4 > data.len() {
                break;
            }
            let a1 = read_i16_be(data, pos)?;
            let a2 = read_i16_be(data, pos + 2)?;
            pos += 4;
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

        if (flags & 0x0020) == 0 {
            break;
        }
    }
    Ok(all_contours)
}

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
        let cy = -(cur.y as f32) * scale;
        expanded.push((cx, cy, cur.on_curve));
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
                let x = inv * inv * p0.0 + 2.0 * inv * t * p1.0 + t * t * p2.0;
                let y = inv * inv * p0.1 + 2.0 * inv * t * p1.1 + t * t * p2.1;
                poly.push((x, y));
            }
            idx += 2;
        }
    }
    poly
}

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
    const SUB: usize = 4;

    for py in 0..height {
        let mut row_hits = vec![0u8; width as usize];
        for sy in 0..SUB {
            let sample_y = (y0 as f32) + (py as f32) + ((sy as f32) + 0.5) / (SUB as f32);
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

            for px in 0..width {
                for sx in 0..SUB {
                    let sample_x = (x0 as f32) + (px as f32) + ((sx as f32) + 0.5) / (SUB as f32);
                    let mut winding = 0i32;
                    for &(cx, dir) in &crossings {
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
            coverage[(py * width + px) as usize] = ((hits * 255 + 8) / 16).min(255) as u8;
        }
    }

    Some(GlyphBitmap {
        width,
        height,
        bearing_x: x0,
        bearing_y: -y0,
        advance_px,
        coverage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_minimal_synthetic_ttf() -> Vec<u8> {
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&1000u16.to_be_bytes()); // unitsPerEm = 1000
        head[50..52].copy_from_slice(&1i16.to_be_bytes()); // indexToLocFormat = 1 (u32 offsets)

        let mut hhea = vec![0u8; 36];
        hhea[4..6].copy_from_slice(&800i16.to_be_bytes()); // ascender
        hhea[6..8].copy_from_slice(&(-200i16).to_be_bytes()); // descender
        hhea[8..10].copy_from_slice(&100i16.to_be_bytes()); // lineGap
        hhea[34..36].copy_from_slice(&3u16.to_be_bytes()); // numOfLongHorMetrics = 3

        let mut maxp = vec![0u8; 6];
        maxp[4..6].copy_from_slice(&3u16.to_be_bytes()); // numGlyphs = 3

        // Glyph 0 (.notdef): 500, Glyph 1 ('A' = 65): 700, Glyph 2 ('i' = 105): 300
        let mut hmtx = Vec::new();
        for &(adv, lsb) in &[(500u16, 0i16), (700u16, 20i16), (300u16, 10i16)] {
            hmtx.extend_from_slice(&adv.to_be_bytes());
            hmtx.extend_from_slice(&lsb.to_be_bytes());
        }

        // Build simple rectangular contour for Glyph 1 ('A'): 4 on-curve points (0,0)->(600,0)->(600,700)->(0,700)
        let mut glyf = Vec::new();
        glyf.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours = 1
        glyf.extend_from_slice(&0i16.to_be_bytes()); // xMin
        glyf.extend_from_slice(&0i16.to_be_bytes()); // yMin
        glyf.extend_from_slice(&600i16.to_be_bytes()); // xMax
        glyf.extend_from_slice(&700i16.to_be_bytes()); // yMax
        glyf.extend_from_slice(&3u16.to_be_bytes()); // endPtsOfContours[0] = 3
        glyf.extend_from_slice(&0u16.to_be_bytes()); // instructionLength = 0
        // 4 points, all on-curve (0x01), 2-byte signed deltas (0x00)
        for _ in 0..4 {
            glyf.push(0x01);
        }
        // X deltas: +0, +600, +0, -600
        for &dx in &[0i16, 600, 0, -600] {
            glyf.extend_from_slice(&dx.to_be_bytes());
        }
        // Y deltas: +0, +0, +700, -700
        for &dy in &[0i16, 0, 700, -700] {
            glyf.extend_from_slice(&dy.to_be_bytes());
        }
        let glyph1_len = glyf.len() as u32;

        // loca (format 1, 4 u32 entries for 3 glyphs): glyph 0 empty (0..0), glyph 1 (0..glyph1_len), glyph 2 empty
        let mut loca = Vec::new();
        loca.extend_from_slice(&0u32.to_be_bytes());
        loca.extend_from_slice(&0u32.to_be_bytes());
        loca.extend_from_slice(&glyph1_len.to_be_bytes());
        loca.extend_from_slice(&glyph1_len.to_be_bytes());

        // cmap with 1 Format 12 subtable (platform 3, encoding 10)
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes()); // version
        cmap.extend_from_slice(&1u16.to_be_bytes()); // numTables = 1
        cmap.extend_from_slice(&3u16.to_be_bytes()); // platformID = 3
        cmap.extend_from_slice(&10u16.to_be_bytes()); // encodingID = 10
        cmap.extend_from_slice(&12u32.to_be_bytes()); // offset = 12
        // Format 12 header
        cmap.extend_from_slice(&12u16.to_be_bytes()); // format = 12
        cmap.extend_from_slice(&0u16.to_be_bytes()); // reserved
        cmap.extend_from_slice(&40u32.to_be_bytes()); // length
        cmap.extend_from_slice(&0u32.to_be_bytes()); // language
        cmap.extend_from_slice(&2u32.to_be_bytes()); // numGroups = 2
        // Group 1: 'A' (65..=65 -> glyph 1)
        cmap.extend_from_slice(&65u32.to_be_bytes());
        cmap.extend_from_slice(&65u32.to_be_bytes());
        cmap.extend_from_slice(&1u32.to_be_bytes());
        // Group 2: 'i' (105..=105 -> glyph 2)
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
                    0, 0, // format = 0
                    0, 1, // count = 1
                    0, 18, // stringOffset = 18
                    0, 1, 0, 0, 0, 0, // platform 1 (Mac), encoding 0, language 0
                    0, 1, // nameID = 1 (Font Family)
                    0, 9, // length = 9
                    0, 0, // offset = 0
                    b'F', b'l', b'u', b'x', b' ', b'S', b'a', b'n', b's',
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
        assert_eq!(font.family_name(), "Flux Sans");
        assert_eq!(font.units_per_em, 1000);
        assert_eq!(font.glyph_id('A'), 1);
        assert_eq!(font.glyph_id('i'), 2);
        assert!((font.glyph_advance_px('A', 20.0) - 14.0).abs() < 0.001);
        assert!((font.glyph_advance_px('i', 20.0) - 6.0).abs() < 0.001);
        assert!((font.measure_text("Ai", 20.0) - 20.0).abs() < 0.001);

        let mut bad_header = vec![0u8; 16];
        bad_header[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        bad_header[4..6].copy_from_slice(&500u16.to_be_bytes()); // exceeds MAX_SFNT_TABLES
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
