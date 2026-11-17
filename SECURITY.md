# Security policy

## Supported versions

| Version | Supported |
|---------|-----------|
| 1.x     | yes       |

## Reporting a vulnerability

Please do **not** open a public issue for security problems. Use GitHub's
private vulnerability reporting ("Security" tab → "Report a vulnerability")
on https://github.com/TerminalDZ/tarz-text.

Relevant areas include parsing of untrusted `.tzf` and TTF/OTF files
(`FontRegistry::register_tzf`, `TzfFont::decode_limited`), decompression
limits and panics on malformed input. We aim to acknowledge reports within
7 days.