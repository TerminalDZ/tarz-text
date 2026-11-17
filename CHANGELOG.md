# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [1.0.0] - 2026-11-18

First standalone open-source release of the Tarz Studio text engine.

### Added
- Arabic/Latin/Tifinagh shaping (rustybuzz), Unicode bidi, kashida.
- Straight, arc and path layouts; alef / cap-height size rule.
- Embroidery rules: bold, minimum column width, small-part enlargement,
  Arabic join overlap, satin width limits, small-text warnings.
- Outputs: merged outlines and per-cluster regions; pre-digitized stitch
  objects (satin / fill / run) in sew order with joined-letter chaining.
- Skeleton-based stroke decomposition into satin columns and fills.
- `.tzf` embroidery font format (encode, decode, converter from TTF/OTF).
- Hardened runtime font registration and font quality checks.
- 25 bundled SIL OFL 1.1 fonts with their `.tzf` files and licence texts.
- `design`, `geom` and `error` modules (self-contained; previously internal
  Tarz crates).
- Examples `lettering_svg`, `load_tzf`, `build_tzf`.

[1.0.0]: https://github.com/TerminalDZ/tarz-text/releases/tag/v1.0.0