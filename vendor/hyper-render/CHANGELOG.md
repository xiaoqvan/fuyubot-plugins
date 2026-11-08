# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-05-12

### Added

- `DataUrlNetProvider` — an in-process `NetProvider` that resolves `data:`
  URIs synchronously for fonts, images, and stylesheets routed through
  blitz-dom. Lifts the previously documented `@font-face` limitation: HTML
  embedding base64 fonts inline now renders correctly. No real network
  I/O is performed; non-`data:` URLs are still ignored.
- New `Config.style_threading: StyleThreading` field (with builder
  method) that forwards to `DocumentConfig.style_threading` (introduced
  in blitz-dom 0.3.0-alpha.4 via [DioxusLabs/blitz#437]). Defaults to
  `StyleThreading::Parallel`, matching blitz's default — no behavior
  change for existing callers.

  Pass `StyleThreading::Sequential` when invoking `render` concurrently
  from multiple OS threads (e.g. a server rendering per request via
  `tokio::task::spawn_blocking`). Stylo's `STYLE_THREAD_POOL` is
  process-global; two `Parallel` resolves that share a rayon worker can
  panic on the worker's thread-local sharing cache (`already mutably
  borrowed`). `Sequential` bypasses the global pool at the
  `traverse_dom` call site, making concurrent invocation safe at the
  cost of intra-render style parallelism (negligible at typical
  hyper-render document sizes). The `StyleThreading` enum is re-exported
  from the crate root.

### Changed

- Bumped `blitz-*` to `=0.3.0-alpha.4` (was `0.2`). Companion bumps:
  `anyrender 0.10`, `anyrender_vello_cpu 0.12.1`, and the PDF-feature
  deps `krilla 0.7`, `stylo 0.17`, `parley 0.9`. `stylo` MUST match
  blitz's pin: it uses `links = "servo_style_crate"` which enforces a
  single version per dependency graph.
- `render_to_png` and `render_to_pdf` now take `&mut HtmlDocument` (was
  `&HtmlDocument`); `blitz-paint::paint_scene` requires a mutable
  document reference as of `0.3.0-alpha.4`.
- MSRV is now Rust 1.92, required by the new vello stack.

### Fixed

- `paint_scene` API drift in `src/render/png.rs`: added the `x_offset` /
  `y_offset` arguments introduced in `blitz-paint 0.3`.

### Known Limitations

- No JavaScript support (by design)
- External image loading not yet implemented
- HTML parser may emit warnings for non-standard CSS properties

[DioxusLabs/blitz#437]: https://github.com/DioxusLabs/blitz/pull/437

## [0.1.0] - 2024-01-29

### Added

- Initial release of hyper-render
- PNG rendering via Vello CPU rasterizer
- PDF rendering via Krilla with vector graphics
- HTML/CSS parsing using Blitz (html5ever + Stylo)
- Flexbox and Grid layout support via Taffy
- Configuration options:
  - Viewport dimensions (width, height)
  - Scale factor for HiDPI displays
  - Output format selection (PNG/PDF)
  - Color scheme preference (light/dark)
  - Auto-height detection for content
  - Custom background colors with transparency support
- Feature flags for optional PNG and PDF support
- Comprehensive error handling with descriptive messages
- Configuration validation (dimensions, scale)
- Font embedding in PDF output
- Background color rendering for all elements

### Known Limitations

- No JavaScript support (by design)
- System fonts only (`@font-face` not yet supported)
- External image loading not yet implemented
- HTML parser may emit warnings for non-standard CSS properties
