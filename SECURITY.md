# Security Policy

## Proof of Concept (PoC) Notice

`flux-ttf` is a standalone, zero-dependency **Proof of Concept (PoC)** TrueType and OpenType (`.ttf` / `.otf`) SFNT font table parser, glyph outline decoder, and anti-aliased scanline rasterizer built from scratch as part of the [**Flux Browser**](https://github.com/buntatoes/flux-browser) engine ecosystem.

---

## Supported Versions

| Version | Supported |
| :--- | :---: |
| `v.03` (Latest) | Yes |
| `< v.03` | No |

---

## Reporting a Vulnerability

Please **do not** report security vulnerabilities through public GitHub issues or pull requests.

- **GitHub Private Vulnerability Reporting (Recommended)**: [`https://github.com/buntatoes/flux-ttf/security/advisories/new`](https://github.com/buntatoes/flux-ttf/security/advisories/new)
- **Central Ecosystem Security Portal**: [`https://github.com/buntatoes/flux-browser/security/advisories/new`](https://github.com/buntatoes/flux-browser/security/advisories/new)
- **Author Attribution**: **Buntos** ([@buntatoes](https://github.com/buntatoes) · `Buntos <324143321+buntatoes@users.noreply.github.com>`)

*(Note: `324143321+buntatoes@users.noreply.github.com` is a GitHub privacy `noreply` commit-attribution address and does not accept direct inbound email; please use GitHub Private Vulnerability Reporting above to contact the maintainer privately.)*

---

## Security Controls in `flux-ttf`

- **SFNT Container & Table Bounds**:
  - Enforces `MAX_FONT_BYTES = 32 MiB` and `MAX_SFNT_TABLES = 128`, validating all table offsets and lengths against buffer bounds prior to parsing.
- **Glyph Contour & Composite Recursion Guards**:
  - Enforces `MAX_GLYPH_CONTOURS = 256`, `MAX_CONTOUR_POINTS = 4096`, and `MAX_COMPOSITE_GLYPH_DEPTH = 16` to prevent malformed or cyclic compound glyphs from causing infinite recursion or excessive memory allocation.
