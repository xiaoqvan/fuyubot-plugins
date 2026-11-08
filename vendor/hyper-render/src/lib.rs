//! # hyper-render
//!
//! A Chromium-free HTML rendering engine for generating PNG and PDF outputs.
//!
//! This library provides a simple, high-level API for rendering HTML content to
//! images (PNG) or documents (PDF) without requiring a browser or Chromium dependency.
//! It leverages the [Blitz](https://github.com/DioxusLabs/blitz) rendering engine
//! for HTML/CSS parsing and layout.
//!
//! ## Features
//!
//! - **PNG output**: Render HTML to PNG images using CPU-based rendering
//! - **PDF output**: Render HTML to PDF documents with vector graphics
//! - **No browser required**: Pure Rust implementation, no Chromium/WebKit
//! - **CSS support**: Flexbox, Grid, and common CSS properties via Stylo
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use hyper_render::{render, Config, OutputFormat};
//!
//! let html = r#"
//!     <html>
//!         <body style="font-family: sans-serif; padding: 20px;">
//!             <h1 style="color: navy;">Hello, World!</h1>
//!             <p>Rendered without Chromium.</p>
//!         </body>
//!     </html>
//! "#;
//!
//! // Render to PNG
//! let png_bytes = render(html, Config::default())?;
//! std::fs::write("output.png", png_bytes)?;
//!
//! // Render to PDF
//! let pdf_bytes = render(html, Config::default().format(OutputFormat::Pdf))?;
//! std::fs::write("output.pdf", pdf_bytes)?;
//! # Ok::<(), hyper_render::Error>(())
//! ```
//!
//! ## Configuration
//!
//! Use [`Config`] to customize the rendering:
//!
//! ```rust,no_run
//! use hyper_render::{Config, OutputFormat};
//!
//! let config = Config::new()
//!     .width(1200)
//!     .height(800)
//!     .scale(2.0)  // 2x resolution for retina displays
//!     .format(OutputFormat::Png);
//! ```

mod config;
mod error;
mod net;
mod render;

pub use config::{ColorScheme, Config, OutputFormat, StyleThreading};
pub use error::{Error, Result};

use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_traits::shell::Viewport;
use std::sync::Arc;

/// Render HTML content to the specified output format.
///
/// This is the main entry point for rendering HTML. It parses the HTML,
/// computes styles and layout, and renders to the format specified in the config.
///
/// # Arguments
///
/// * `html` - The HTML content to render
/// * `config` - Rendering configuration (dimensions, format, scale, etc.)
///
/// # Returns
///
/// Returns the rendered output as bytes (PNG image data or PDF document).
///
/// # Errors
///
/// Returns an error if:
/// - Configuration is invalid (zero dimensions, non-positive scale)
/// - HTML parsing fails
/// - Layout computation fails
/// - Rendering fails
/// - The requested output format feature is not enabled
///
/// # Example
///
/// ```rust,no_run
/// use hyper_render::{render, Config, OutputFormat};
///
/// let html = "<h1>Hello</h1>";
///
/// // PNG output (default)
/// let png = render(html, Config::default())?;
///
/// // PDF output
/// let pdf = render(html, Config::default().format(OutputFormat::Pdf))?;
/// # Ok::<(), hyper_render::Error>(())
/// ```
pub fn render(html: &str, config: Config) -> Result<Vec<u8>> {
    // Validate configuration
    config.validate()?;

    // Parse HTML and create document
    let mut document = create_document(html, &config)?;

    // Resolve styles and compute layout
    document.resolve(0.0);

    // Render to the specified format
    match config.format {
        OutputFormat::Png => render::png::render_to_png(&mut document, &config),
        OutputFormat::Pdf => render::pdf::render_to_pdf(&mut document, &config),
    }
}

/// Render HTML content to PNG format.
///
/// Convenience function that renders directly to PNG without needing to specify
/// the format in the config.
///
/// # Example
///
/// ```rust,no_run
/// use hyper_render::{render_to_png, Config};
///
/// let png_bytes = render_to_png("<h1>Hello</h1>", Config::default())?;
/// std::fs::write("output.png", png_bytes)?;
/// # Ok::<(), hyper_render::Error>(())
/// ```
#[cfg(feature = "png")]
pub fn render_to_png(html: &str, config: Config) -> Result<Vec<u8>> {
    render(html, config.format(OutputFormat::Png))
}

/// Render HTML content to PDF format.
///
/// Convenience function that renders directly to PDF without needing to specify
/// the format in the config.
///
/// # Example
///
/// ```rust,no_run
/// use hyper_render::{render_to_pdf, Config};
///
/// let pdf_bytes = render_to_pdf("<h1>Hello</h1>", Config::default())?;
/// std::fs::write("output.pdf", pdf_bytes)?;
/// # Ok::<(), hyper_render::Error>(())
/// ```
#[cfg(feature = "pdf")]
pub fn render_to_pdf(html: &str, config: Config) -> Result<Vec<u8>> {
    render(html, config.format(OutputFormat::Pdf))
}

/// Create and configure a Blitz document from HTML.
fn create_document(html: &str, config: &Config) -> Result<HtmlDocument> {
    let viewport = Viewport::new(
        config.width,
        config.height,
        config.scale,
        config.color_scheme.into(),
    );

    let doc_config = DocumentConfig {
        viewport: Some(viewport),
        net_provider: Some(Arc::new(net::RemoteNetProvider)),
        style_threading: config.style_threading.into(),
        // blitz 默认的 base_url 是 `data:text/css;charset=utf-8;base64,`，
        // 它是 cannot-be-a-base，连 `data:image/...` 这种绝对 URL 都 join 失败，
        // 会在 `resolve_url` 里 panic（PR #1 只加了 NetProvider，没管这里）。
        // 给一个层次化的 http base，让 `data:` / 相对 URL 都能正常 join；
        // 引擎本来就不做网络 I/O，非 data: URL 最终会被忽略。
        base_url: Some("https://hyper-render.invalid/".to_owned()),
        ..Default::default()
    };

    Ok(HtmlDocument::from_html(html, doc_config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builder() {
        let config = Config::new()
            .width(1920)
            .height(1080)
            .scale(2.0)
            .format(OutputFormat::Png);

        assert_eq!(config.width, 1920);
        assert_eq!(config.height, 1080);
        assert_eq!(config.scale, 2.0);
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.width, 800);
        assert_eq!(config.height, 600);
        assert_eq!(config.scale, 1.0);
    }

    #[test]
    fn test_config_validation_zero_width() {
        let config = Config::new().width(0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_zero_height() {
        let config = Config::new().height(0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_zero_scale() {
        let config = Config::new().scale(0.0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_negative_scale() {
        let config = Config::new().scale(-1.0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_valid() {
        let config = Config::new()
            .width(Config::MIN_DIMENSION)
            .height(Config::MIN_DIMENSION)
            .scale(0.1);
        assert!(config.validate().is_ok());
    }
}
