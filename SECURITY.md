# Security

Report problems in private:

https://github.com/buntatoes/anvil-ttf/security/advisories/new

Do not put the details in a public issue.

## What is checked

- Font files larger than 320 MiB (`MaxFontBytes`) are rejected immediately.
- SFNT table directories with more than 1,280 table records (`MaxSfntTables`) are rejected before table bodies are parsed.
- Glyphs with more than 2,560 contours (`MaxGlyphContours`) or more than 40,960 control points (`MaxContourPoints`) are rejected to prevent memory exhaustion attacks.
- Composite glyph recursion is strictly capped at depth 64 (`MaxCompositeGlyphDepth`) to eliminate infinite recursion and stack overflows on self-referential fonts.
- Rasterized glyph dimensions are capped at 512x512 pixels (`MaxGlyphBitmapSide`), and coordinates are bounded to within 2^24 pixels of the pen (`MaxGlyphOffset`).
- CFF Type 2 CharString programs enforce operand stack limits (48 numbers) and subroutine call stack limits (16 frames).

Version 0.11.0 (`v.11`) is current.
