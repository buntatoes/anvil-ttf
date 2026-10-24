# Security

Report problems in private:

https://github.com/buntatoes/anvil-ttf/security/advisories/new

The browser repository takes the same kind of report:

https://github.com/buntatoes/anvil-browser/security/advisories/new

Do not put the details in a public issue.

## What is checked

- Files larger than 32 MiB are rejected.
- Table count, contour count, point count, and compound-glyph depth are capped (128 tables, 256 contours, 4096 points, depth 16).

Version 0.4.0 is current.
