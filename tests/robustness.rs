// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 Buntato
//! Robustness checks for the SFNT parser, glyph outlines, and rasterizer.
//!
//! Inputs come from a seeded generator, so a failure names a case number
//! that reproduces on every run. Each test stops after a short time budget.
//! The parser may reject any of these inputs. It must not panic or hang, and
//! a font it accepts must measure and rasterize every glyph without panicking.

use anvil_ttf::{TtfFont, interpret_type2_charstring, interpret_type2_charstring_with_subrs};

// ---- shared harness (same text in each repo's robustness test) ----

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Wall-clock budget for one robustness test. Cases run until it is spent.
/// Set `ANVIL_ROBUSTNESS_MS` for a longer local soak run.
fn test_budget() -> Duration {
    let millis = std::env::var("ANVIL_ROBUSTNESS_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1500);
    Duration::from_millis(millis)
}

/// One input that runs longer than this counts as a hang.
const CASE_LIMIT: Duration = Duration::from_secs(10);

/// Upper bound on cases per test, so a fast machine does not loop forever.
const MAX_CASES: usize = 200_000;

/// xorshift64 generator. A fixed seed gives the same inputs on every run.
struct Rng(u64);

#[allow(dead_code)] // Not every test file uses every helper.
impl Rng {
    fn new(seed: u64) -> Self {
        Rng((seed ^ 0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Value in `0..n`. Returns 0 when `n` is 0.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    fn byte(&mut self) -> u8 {
        self.next_u64() as u8
    }

    fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    fn bytes(&mut self, max_len: usize) -> Vec<u8> {
        let len = self.below(max_len + 1);
        (0..len).map(|_| self.byte()).collect()
    }
}

/// Apply 1 to 8 random edits: bit flips, byte overwrites, boundary values,
/// inserts, deletes, duplicated runs, and truncation.
fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut out = seed.to_vec();
    let edits = 1 + rng.below(8);
    for _ in 0..edits {
        let len = out.len();
        match rng.below(8) {
            0 if len > 0 => {
                let i = rng.below(len);
                out[i] ^= 1 << rng.below(8);
            }
            1 if len > 0 => {
                let i = rng.below(len);
                out[i] = rng.byte();
            }
            2 if len > 0 => {
                let i = rng.below(len);
                out[i] = rng.pick(&[0x00, 0x01, 0x7F, 0x80, 0xFE, 0xFF]);
            }
            3 if len >= 4 => {
                let i = rng.below(len - 3);
                let v = rng.pick(&[0u32, 1, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFF, 0xFFFF]);
                out[i..i + 4].copy_from_slice(&v.to_be_bytes());
            }
            4 => {
                let i = rng.below(len + 1);
                let extra = rng.bytes(16);
                out.splice(i..i, extra);
            }
            5 if len > 0 => {
                let i = rng.below(len);
                let n = 1 + rng.below((len - i).min(32));
                out.drain(i..i + n);
            }
            6 if len > 0 => {
                let i = rng.below(len);
                let n = 1 + rng.below((len - i).min(64));
                let run = out[i..i + n].to_vec();
                let at = rng.below(len + 1);
                out.splice(at..at, run);
            }
            _ => {
                let keep = rng.below(len + 1);
                out.truncate(keep);
            }
        }
    }
    out
}

fn preview(input: &[u8]) -> String {
    let shown: Vec<String> = input.iter().take(96).map(|b| format!("{b:02x}")).collect();
    let more = if input.len() > 96 { " ..." } else { "" };
    format!("{} bytes: {}{}", input.len(), shown.join(" "), more)
}

/// Run `target` on `input` in its own thread. Fails the test on a panic or
/// when the case runs past [`CASE_LIMIT`]. A hung thread is left behind and
/// ends with the test process.
fn check_case(label: &str, case: usize, input: Vec<u8>, target: fn(&[u8])) {
    let (done_tx, done_rx) = mpsc::channel();
    let shared = std::sync::Arc::new(input);
    let worker_input = shared.clone();
    let handle = thread::Builder::new()
        .name(format!("{label}-{case}"))
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            target(&worker_input);
            let _ = done_tx.send(());
        })
        .expect("spawn case thread");
    match done_rx.recv_timeout(CASE_LIMIT) {
        Ok(()) => {
            let _ = handle.join();
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
            panic!(
                "{label}: case {case} panicked on input {}",
                preview(&shared)
            );
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!(
                "{label}: case {case} did not finish within {:?} on input {}",
                CASE_LIMIT,
                preview(&shared)
            );
        }
    }
}

/// Generate cases from `seed` until the budget is spent and check each one.
fn run_cases(
    label: &str,
    seed: u64,
    mut generate: impl FnMut(&mut Rng) -> Vec<u8>,
    target: fn(&[u8]),
) {
    let mut rng = Rng::new(seed);
    let budget = test_budget();
    let started = Instant::now();
    let mut case = 0;
    while case < MAX_CASES && started.elapsed() < budget {
        let input = generate(&mut rng);
        check_case(label, case, input, target);
        case += 1;
    }
    eprintln!("{label}: {case} cases in {:?}", started.elapsed());
}

fn be16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn be32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Four glyphs: 0 empty, 1 a square with one off-curve point, 2 a composite
/// of glyph 1, and 3 a composite that names itself (it must hit the depth cap).
/// cmap maps 'A'..='D' to glyphs 0..=3. A format 0 kern table pairs 1 and 2.
fn sample_font() -> Vec<u8> {
    let mut head = vec![0u8; 54];
    head[18..20].copy_from_slice(&1024u16.to_be_bytes());
    head[50..52].copy_from_slice(&1u16.to_be_bytes()); // long loca offsets

    let mut hhea = vec![0u8; 36];
    hhea[4..6].copy_from_slice(&800u16.to_be_bytes());
    hhea[6..8].copy_from_slice(&(-200i16 as u16).to_be_bytes());
    hhea[34..36].copy_from_slice(&4u16.to_be_bytes());

    let mut maxp = vec![0u8; 6];
    maxp[0..4].copy_from_slice(&0x0000_5000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&4u16.to_be_bytes());

    let mut hmtx = Vec::new();
    for advance in [500u16, 600, 600, 600] {
        be16(&mut hmtx, advance);
        be16(&mut hmtx, 10);
    }

    let mut glyf = Vec::new();
    let mut loca = vec![0u32, 0];
    // Glyph 1: one contour, five points, the third one off-curve.
    for v in [1i16, 0, 0, 600, 700] {
        be16(&mut glyf, v as u16);
    }
    be16(&mut glyf, 4); // last point index
    be16(&mut glyf, 0); // no instructions
    glyf.extend_from_slice(&[0x01, 0x01, 0x00, 0x01, 0x01]);
    for dx in [0i16, 600, 0, -300, -300] {
        be16(&mut glyf, dx as u16);
    }
    for dy in [0i16, 0, 700, 0, -700] {
        be16(&mut glyf, dy as u16);
    }
    while glyf.len() % 4 != 0 {
        glyf.push(0);
    }
    loca.push(glyf.len() as u32);
    // Glyphs 2 and 3: composites with word arguments.
    for target in [1u16, 3] {
        for v in [-1i16, 0, 0, 600, 700] {
            be16(&mut glyf, v as u16);
        }
        be16(&mut glyf, 0x0001 | 0x0002); // ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES
        be16(&mut glyf, target);
        be16(&mut glyf, 20);
        be16(&mut glyf, 0);
        loca.push(glyf.len() as u32);
    }
    let mut loca_bytes = Vec::new();
    for offset in loca {
        be32(&mut loca_bytes, offset);
    }

    // cmap: one format 4 subtable for platform 3, encoding 1.
    let mut cmap = Vec::new();
    for v in [0u16, 1, 3, 1] {
        be16(&mut cmap, v);
    }
    be32(&mut cmap, 12);
    let seg_count = 2u16;
    for v in [4u16, 16 + 8 * seg_count, 0, seg_count * 2, 2, 0, 2] {
        be16(&mut cmap, v);
    }
    for end in [0x44u16, 0xFFFF] {
        be16(&mut cmap, end);
    }
    be16(&mut cmap, 0);
    for start in [0x41u16, 0xFFFF] {
        be16(&mut cmap, start);
    }
    for delta in [(-0x41i16) as u16, 1] {
        be16(&mut cmap, delta);
    }
    for _ in 0..seg_count {
        be16(&mut cmap, 0);
    }

    let mut kern = Vec::new();
    for v in [0u16, 1, 0, 20, 0x0001, 1, 6, 0, 0, 1, 2, (-40i16) as u16] {
        be16(&mut kern, v);
    }

    let mut name = Vec::new();
    for v in [0u16, 1, 18, 3, 1, 0x409, 1, 10, 0] {
        be16(&mut name, v);
    }
    for unit in "Robust".encode_utf16() {
        be16(&mut name, unit);
    }
    // Patch the name record length to the UTF-16 byte count.
    name[14..16].copy_from_slice(&12u16.to_be_bytes());

    let tables: [(&[u8; 4], Vec<u8>); 9] = [
        (b"cmap", cmap),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"kern", kern),
        (b"loca", loca_bytes),
        (b"maxp", maxp),
        (b"name", name),
    ];
    let mut font = Vec::new();
    be32(&mut font, 0x0001_0000);
    for v in [tables.len() as u16, 128, 3, 16] {
        be16(&mut font, v);
    }
    let mut offset = 12 + 16 * tables.len();
    for (tag, data) in &tables {
        font.extend_from_slice(*tag);
        be32(&mut font, 0);
        be32(&mut font, offset as u32);
        be32(&mut font, data.len() as u32);
        offset += data.len().div_ceil(4) * 4;
    }
    for (_, data) in &tables {
        font.extend_from_slice(data);
        while font.len() % 4 != 0 {
            font.push(0);
        }
    }
    font
}

/// Parse and, on success, touch every public query for a spread of characters.
fn exercise_font(input: &[u8]) {
    let Ok(font) = TtfFont::parse(input) else {
        return;
    };
    let _ = font.family_name();
    for ch in [
        'A',
        'B',
        'C',
        'D',
        'E',
        ' ',
        '\u{e9}',
        '\u{FFFF}',
        '\u{1F600}',
    ] {
        let gid = font.glyph_id(ch);
        let _ = font.glyph_metric(gid);
        let _ = font.kerning_units(gid, font.glyph_id('C'));
        let _ = font.glyph_outline(gid);
        for size in [0.0f32, 1.0, 16.0, 97.5, 1e9, f32::MAX, f32::NAN] {
            let _ = font.glyph_advance_px(ch, size);
            let _ = font.rasterize_glyph(ch, size);
            let _ = font.rasterize_glyph_subpixel_lcd(ch, size);
        }
    }
    let _ = font.measure_text("ABCD \u{e9}", 13.0);
    let _ = font.line_height_px(13.0);
    for gid in [0u16, 1, 2, 3, 4, 1000, u16::MAX] {
        let _ = font.glyph_metric(gid);
        let _ = font.glyph_outline(gid);
    }
}

#[test]
fn sample_font_parses() {
    let font = TtfFont::parse(&sample_font()).expect("sample font parses");
    assert_eq!(font.glyph_id('B'), 1);
    assert!(font.glyph_outline(1).is_some());
    assert!(font.rasterize_glyph('B', 24.0).is_some());
    exercise_font(&sample_font());
}

#[test]
fn random_bytes_do_not_panic() {
    run_cases(
        "ttf-random",
        31,
        |rng| {
            let mut input = rng.bytes(512);
            if input.len() >= 4 {
                let tag = rng.pick(&[0x0001_0000u32, 0x7472_7565, 0x4F54_544F]);
                input[..4].copy_from_slice(&tag.to_be_bytes());
            }
            input
        },
        exercise_font,
    );
}

#[test]
fn mutated_fonts_do_not_panic() {
    let font = sample_font();
    run_cases(
        "ttf-mutated",
        32,
        move |rng| mutate(rng, &font),
        exercise_font,
    );
}

#[test]
fn mutated_table_directory_does_not_panic() {
    let font = sample_font();
    run_cases(
        "ttf-directory",
        33,
        move |rng| {
            let mut out = font.clone();
            // Rewrite one offset or length field in the table directory.
            let record = 12 + 16 * rng.below(9);
            let field = record + 8 + 4 * rng.below(2);
            let value = rng.pick(&[
                0u32,
                1,
                3,
                0xFFFF,
                0x7FFF_FFFF,
                0xFFFF_FFFF,
                out.len() as u32,
            ]);
            out[field..field + 4].copy_from_slice(&value.to_be_bytes());
            out
        },
        exercise_font,
    );
}

#[test]
fn random_charstrings_do_not_panic() {
    run_cases(
        "ttf-charstring",
        34,
        |rng| {
            let mut program = Vec::new();
            for _ in 0..rng.below(64) {
                if rng.one_in(3) {
                    // Operators 0..=31, including the escape byte 12.
                    program.push(rng.below(32) as u8);
                } else {
                    // Small integer operands in the 32..=246 single-byte form.
                    program.push(32 + rng.below(215) as u8);
                }
            }
            program
        },
        |program| {
            let _ = interpret_type2_charstring(program);
            let subrs: [&[u8]; 2] = [program, &[11]];
            let _ = interpret_type2_charstring_with_subrs(program, &subrs, &subrs);
        },
    );
}

/// Font sizes that used to overflow the glyph bounds: zero, negative, NaN,
/// infinities, and sizes far past the 512 pixel bitmap limit.
const EXTREME_SIZES: [f32; 16] = [
    0.0,
    -0.0,
    -1.0,
    -1e30,
    f32::NAN,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::MIN_POSITIVE,
    512.0,
    1e5,
    1e6,
    1e9,
    2_147_483_647.0,
    1e30,
    f32::MAX,
    f32::MIN,
];

fn check_bitmap_sizes(font: &TtfFont, ch: char, size: f32) {
    if let Some(bmp) = font.rasterize_glyph(ch, size) {
        assert!(bmp.width <= 512 && bmp.height <= 512, "{ch:?} at {size}");
        assert_eq!(bmp.coverage.len(), (bmp.width * bmp.height) as usize);
        assert!(bmp.advance_px.is_finite(), "{ch:?} at {size}");
    }
    if let Some(bmp) = font.rasterize_glyph_subpixel_lcd(ch, size) {
        assert!(bmp.width <= 512 && bmp.height <= 512, "{ch:?} at {size}");
        assert!(bmp.advance_px.is_finite(), "{ch:?} at {size}");
    }
}

#[test]
fn extreme_font_sizes_do_not_overflow() {
    let font = TtfFont::parse(&sample_font()).expect("sample font parses");
    for size in EXTREME_SIZES {
        for ch in ['A', 'B', 'C', 'D'] {
            check_bitmap_sizes(&font, ch, size);
            let _ = font.glyph_advance_px(ch, size);
        }
        let _ = font.measure_text("ABCD", size);
        let _ = font.line_height_px(size);
    }
    // A size the bitmap can hold still draws.
    assert!(font.rasterize_glyph('B', 300.0).is_some());
    assert!(
        font.rasterize_glyph('C', 300.0).is_some(),
        "composite glyph"
    );
}

/// First four bytes: an `f32` font size. The rest: a font (the sample font
/// when empty). Every glyph is measured and drawn at that size.
fn exercise_size(input: &[u8]) {
    let Some((size, rest)) = input.split_first_chunk::<4>() else {
        return;
    };
    let size = f32::from_le_bytes(*size);
    let owned;
    let bytes = if rest.is_empty() {
        owned = sample_font();
        &owned[..]
    } else {
        rest
    };
    let Ok(font) = TtfFont::parse(bytes) else {
        return;
    };
    for ch in ['A', 'B', 'C', 'D', 'E'] {
        check_bitmap_sizes(&font, ch, size);
        let _ = font.glyph_advance_px(ch, size);
    }
    let _ = font.measure_text("ABCD", size);
    let _ = font.line_height_px(size);
}

/// A size near the bitmap limit, a fixed extreme, any bit pattern, or a
/// power of ten up to 1e38, with either sign.
fn random_size(rng: &mut Rng) -> f32 {
    let size = match rng.below(4) {
        0 => rng.pick(&EXTREME_SIZES),
        1 => f32::from_bits(rng.next_u64() as u32),
        2 => 10f32.powi(rng.below(39) as i32) * (1.0 + rng.below(1000) as f32 / 1000.0),
        _ => 400.0 + rng.below(400) as f32 + rng.below(1000) as f32 / 1000.0,
    };
    if rng.one_in(4) { -size } else { size }
}

#[test]
fn random_font_sizes_do_not_overflow() {
    run_cases(
        "ttf-sizes",
        0x5349_5a45,
        |rng| random_size(rng).to_le_bytes().to_vec(),
        exercise_size,
    );
}

#[test]
fn mutated_fonts_at_extreme_sizes_do_not_overflow() {
    let font = sample_font();
    run_cases(
        "ttf-mutated-sizes",
        0x4d55_5453,
        move |rng| {
            let mut input = random_size(rng).to_le_bytes().to_vec();
            input.extend(mutate(rng, &font));
            input
        },
        exercise_size,
    );
}

#[test]
fn system_font_glyphs_at_extreme_sizes_do_not_overflow() {
    // Real outlines when the machine has a font. Skipped quietly otherwise.
    for (bold, mono) in [(false, false), (true, false), (false, true)] {
        let Some(font) = TtfFont::load_system_font(bold, mono) else {
            continue;
        };
        for size in EXTREME_SIZES {
            for ch in ['H', 'g', '@', '\u{e9}', '\u{c5}', 'W'] {
                check_bitmap_sizes(&font, ch, size);
            }
        }
    }
}
