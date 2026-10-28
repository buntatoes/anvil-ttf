# Security

Report problems in private:

https://github.com/buntatoes/anvil-ttf/security/advisories/new

The browser repository takes the same kind of report:

https://github.com/buntatoes/anvil-browser/security/advisories/new

Do not put the details in a public issue.

## What is checked

- Files larger than 320 MiB are rejected.
- Table count, contour count, point count, compound-glyph depth, and CFF Type 2 subroutine depth are capped (1,280 tables, 2,560 contours, 40,960 points, depth 64).
- A glyph bitmap is at most 512 pixels on a side and within 2^24 pixels of the pen. Larger, NaN, or infinite font sizes give no bitmap, and the bounds are checked before any integer conversion.
- Audited `unsafe` lookup table accesses (`srgb_to_linear_u12` and `linear_u12_to_srgb`) index fixed-size `[u16; 256]` and `[u8; 4096]` arrays after clamping indices in bounds.

Version 0.9.0 is current.
