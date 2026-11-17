# Contributing to tarz-text

Thanks for your interest! Bug reports, fixes, fonts and documentation
improvements are welcome.

## Development

Requirements: Rust 1.88 or newer (stable).

```text
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
cargo doc --no-deps
```

All four must pass before a pull request is merged (CI runs them on Linux,
Windows and macOS).

## Guidelines

- Keep behaviour changes deliberate: the engine output is used to sew real
  garments. Add or update a test for every behaviour change.
- All coordinates are millimetres, y axis down.
- No `unsafe` code (`#![forbid(unsafe_code)]`).
- New fonts must be under an open licence that allows redistribution
  (e.g. SIL OFL 1.1). Add the full licence text to `assets/fonts/licenses/`
  and the font to `assets/fonts/LICENSES.md` and `assets/fonts/fonts.json`.
- Update `CHANGELOG.md` under an "Unreleased" heading.

## Licence

By contributing you agree that your contributions are licensed under the
MIT licence of this repository (fonts keep their own licences).