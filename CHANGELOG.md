# Changelog

All notable changes to Parabloom are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). See `RELEASING.md` for how a
release is cut.

## [Unreleased]

### Added

- `parabloom` command that expands partial PDF highlights to cover each whole paragraph, based on the `expand_highlights.py` reference script.
- In-place updates when the input and output paths match, using an incremental save with a staged, permission-preserving full-rewrite fallback.
- Preservation of annotation notes, authors, dates and reply threads when highlights are expanded or merged.
- Column-aware paragraph grouping, matching of every selected quad, and rotation-safe text extraction.
- Prebuilt binaries for Linux and macOS (x86_64 and arm64), a vendored source archive and SHA-256 checksums on each GitHub release.
