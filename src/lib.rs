//! Proprietary TrueType/OpenType SFNT font parser and proportional text metrics engine.

use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TtfError {
    UnexpectedEof,
    InvalidSfntSignature,
    MissingTable(&'static str),
    InvalidTable(&'static str),
}

impl fmt::Display for TtfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "unexpected end of font file"),
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

#[derive(Clone, Debug)]
pub struct TtfFont {
    pub units_per_em: u16,
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
    cmap: HashMap<u32, u16>,
    metrics: Vec<HMetric>,
    kern_pairs: HashMap<(u16, u16), i16>,
}

impl TtfFont {
    /// Parse a TrueType or OpenType font binary into a [`TtfFont`].
    pub fn parse(bytes: &[u8]) -> Result<Self, TtfError> {
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

        Ok(Self {
            units_per_em,
            ascender,
            descender,
            line_gap,
            cmap,
            metrics,
            kern_pairs,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn build_minimal_synthetic_ttf() -> Vec<u8> {
        // Minimal valid SFNT with 5 tables: cmap, head, hhea, hmtx, maxp
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&1000u16.to_be_bytes()); // unitsPerEm = 1000

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

        let tables: [(&[u8; 4], &[u8]); 5] = [
            (b"cmap", &cmap),
            (b"head", &head),
            (b"hhea", &hhea),
            (b"hmtx", &hmtx),
            (b"maxp", &maxp),
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
        assert_eq!(font.units_per_em, 1000);
        assert_eq!(font.glyph_id('A'), 1);
        assert_eq!(font.glyph_id('i'), 2);
        assert!((font.glyph_advance_px('A', 20.0) - 14.0).abs() < 0.001);
        assert!((font.glyph_advance_px('i', 20.0) - 6.0).abs() < 0.001);
        assert!((font.measure_text("Ai", 20.0) - 20.0).abs() < 0.001);
    }

    #[test]
    fn proportional_fallback_distinguishes_narrow_and_wide_glyphs() {
        let narrow = measure_proportional_fallback("iiii", 16.0);
        let wide = measure_proportional_fallback("MMMM", 16.0);
        assert!(wide > narrow * 2.0);
    }
}
