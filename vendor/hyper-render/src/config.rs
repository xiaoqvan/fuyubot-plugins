//! Configuration types for rendering.

use crate::error::{Error, Result};

/// Strategy for Stylo's style traversal during rendering.
///
/// Mirrors [`blitz_dom::StyleThreading`] so hyper-render's public API
/// doesn't leak blitz types. See [`Config::style_threading`] for guidance
/// on which variant to pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StyleThreading {
    /// Use Stylo's parallel traversal via its global rayon thread pool.
    /// Fastest for rendering a single document; concurrent invocation
    /// from multiple OS threads can panic on a worker's thread-local
    /// sharing cache.
    #[default]
    Parallel,
    /// Run style traversal sequentially on the calling thread, bypassing
    /// Stylo's global pool. Safe to invoke concurrently from multiple
    /// OS threads.
    Sequential,
}

impl From<StyleThreading> for blitz_dom::StyleThreading {
    fn from(threading: StyleThreading) -> Self {
        match threading {
            StyleThreading::Parallel => blitz_dom::StyleThreading::Parallel,
            StyleThreading::Sequential => blitz_dom::StyleThreading::Sequential,
        }
    }
}

/// Output format for rendered content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// PNG image format (raster).
    #[default]
    Png,
    /// PDF document format (vector).
    Pdf,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputFormat::Png => write!(f, "png"),
            OutputFormat::Pdf => write!(f, "pdf"),
        }
    }
}

/// Color scheme preference for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorScheme {
    /// Light color scheme.
    #[default]
    Light,
    /// Dark color scheme.
    Dark,
}

impl From<ColorScheme> for blitz_traits::shell::ColorScheme {
    fn from(scheme: ColorScheme) -> Self {
        match scheme {
            ColorScheme::Light => blitz_traits::shell::ColorScheme::Light,
            ColorScheme::Dark => blitz_traits::shell::ColorScheme::Dark,
        }
    }
}

/// Configuration for HTML rendering.
///
/// Use the builder pattern to construct a configuration:
///
/// ```rust
/// use hyper_render::{Config, OutputFormat, ColorScheme};
///
/// let config = Config::new()
///     .width(1200)
///     .height(800)
///     .scale(2.0)
///     .format(OutputFormat::Pdf)
///     .color_scheme(ColorScheme::Dark);
/// ```
#[derive(Debug, Clone)]
pub struct Config {
    /// Width of the viewport in pixels.
    pub width: u32,

    /// Height of the viewport in pixels.
    ///
    /// For PDF output, this may be adjusted based on content length
    /// if `auto_height` is enabled.
    pub height: u32,

    /// Scale factor for rendering (e.g., 2.0 for retina displays).
    pub scale: f32,

    /// Output format (PNG or PDF).
    pub format: OutputFormat,

    /// Color scheme preference (light or dark mode).
    pub color_scheme: ColorScheme,

    /// Whether to automatically adjust height based on content.
    ///
    /// When enabled, the renderer will compute the actual content height
    /// and use that instead of the configured height.
    pub auto_height: bool,

    /// Background color as RGBA (default: white).
    pub background: [u8; 4],

    /// Strategy for Stylo's style traversal during rendering.
    ///
    /// - [`StyleThreading::Parallel`] (default): use Stylo's global rayon
    ///   thread pool for style traversal. Fastest for rendering a single
    ///   large document.
    /// - [`StyleThreading::Sequential`]: run style traversal sequentially
    ///   on the calling thread, bypassing the global pool. Required when
    ///   `render` is invoked concurrently from multiple OS threads (e.g.
    ///   a server rendering documents per request via
    ///   `tokio::task::spawn_blocking`) — two `Parallel` resolves that
    ///   share a rayon worker can panic on the worker's thread-local
    ///   sharing cache (`already mutably borrowed`).
    ///
    pub style_threading: StyleThreading,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: 800,
            height: 600,
            scale: 1.0,
            format: OutputFormat::Png,
            color_scheme: ColorScheme::Light,
            auto_height: false,
            background: [255, 255, 255, 255],           // White
            style_threading: StyleThreading::default(), // Parallel
        }
    }
}

impl Config {
    /// Create a new configuration with default values.
    ///
    /// Defaults:
    /// - Width: 800px
    /// - Height: 600px
    /// - Scale: 1.0
    /// - Format: PNG
    /// - Color scheme: Light
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the viewport width in pixels.
    ///
    /// # Example
    ///
    /// ```rust
    /// use hyper_render::Config;
    ///
    /// let config = Config::new().width(1920);
    /// assert_eq!(config.width, 1920);
    /// ```
    pub fn width(mut self, width: u32) -> Self {
        self.width = width;
        self
    }

    /// Set the viewport height in pixels.
    ///
    /// # Example
    ///
    /// ```rust
    /// use hyper_render::Config;
    ///
    /// let config = Config::new().height(1080);
    /// assert_eq!(config.height, 1080);
    /// ```
    pub fn height(mut self, height: u32) -> Self {
        self.height = height;
        self
    }

    /// Set both width and height at once.
    ///
    /// # Example
    ///
    /// ```rust
    /// use hyper_render::Config;
    ///
    /// let config = Config::new().size(1920, 1080);
    /// assert_eq!(config.width, 1920);
    /// assert_eq!(config.height, 1080);
    /// ```
    pub fn size(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Set the scale factor for rendering.
    ///
    /// Use 2.0 for retina/HiDPI displays to get crisp output.
    ///
    /// # Example
    ///
    /// ```rust
    /// use hyper_render::Config;
    ///
    /// let config = Config::new().scale(2.0);
    /// assert_eq!(config.scale, 2.0);
    /// ```
    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    /// Set the output format.
    ///
    /// # Example
    ///
    /// ```rust
    /// use hyper_render::{Config, OutputFormat};
    ///
    /// let config = Config::new().format(OutputFormat::Pdf);
    /// ```
    pub fn format(mut self, format: OutputFormat) -> Self {
        self.format = format;
        self
    }

    /// Set the color scheme preference.
    ///
    /// This affects CSS media queries like `prefers-color-scheme`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use hyper_render::{Config, ColorScheme};
    ///
    /// let config = Config::new().color_scheme(ColorScheme::Dark);
    /// ```
    pub fn color_scheme(mut self, scheme: ColorScheme) -> Self {
        self.color_scheme = scheme;
        self
    }

    /// Enable automatic height detection.
    ///
    /// When enabled, the renderer will compute the actual content height
    /// and use that instead of the configured height. This is useful for
    /// rendering full-page content.
    ///
    /// # Example
    ///
    /// ```rust
    /// use hyper_render::Config;
    ///
    /// let config = Config::new().auto_height(true);
    /// ```
    pub fn auto_height(mut self, auto: bool) -> Self {
        self.auto_height = auto;
        self
    }

    /// Set the background color as RGBA values.
    ///
    /// # Example
    ///
    /// ```rust
    /// use hyper_render::Config;
    ///
    /// // Transparent background
    /// let config = Config::new().background([0, 0, 0, 0]);
    ///
    /// // Light gray background
    /// let config = Config::new().background([240, 240, 240, 255]);
    /// ```
    pub fn background(mut self, rgba: [u8; 4]) -> Self {
        self.background = rgba;
        self
    }

    /// Set a transparent background.
    ///
    /// Shorthand for `.background([0, 0, 0, 0])`.
    pub fn transparent(self) -> Self {
        self.background([0, 0, 0, 0])
    }

    /// Set the Stylo style-traversal strategy.
    ///
    /// Pass [`StyleThreading::Sequential`] when invoking `render` from
    /// multiple OS threads concurrently — see the field docs on
    /// [`Config::style_threading`] for the full rationale.
    ///
    /// # Example
    ///
    /// ```rust
    /// use hyper_render::{Config, StyleThreading};
    ///
    /// // Server context: one render per `spawn_blocking` thread.
    /// let config = Config::new().style_threading(StyleThreading::Sequential);
    /// assert_eq!(config.style_threading, StyleThreading::Sequential);
    /// ```
    pub fn style_threading(mut self, threading: StyleThreading) -> Self {
        self.style_threading = threading;
        self
    }

    /// Minimum supported width/height in pixels.
    ///
    /// Very small dimensions can cause overflow issues in the underlying
    /// rendering engine.
    pub const MIN_DIMENSION: u32 = 16;

    /// Validate the configuration.
    ///
    /// Returns an error if any configuration values are invalid:
    /// - Width must be at least 16
    /// - Height must be at least 16
    /// - Scale must be greater than 0
    ///
    /// This is called automatically by the render functions.
    ///
    /// # Example
    ///
    /// ```rust
    /// use hyper_render::Config;
    ///
    /// let config = Config::new().width(0);
    /// assert!(config.validate().is_err());
    /// ```
    pub fn validate(&self) -> Result<()> {
        if self.width < Self::MIN_DIMENSION {
            return Err(Error::InvalidConfig(format!(
                "width must be at least {} pixels",
                Self::MIN_DIMENSION
            )));
        }
        if self.height < Self::MIN_DIMENSION {
            return Err(Error::InvalidConfig(format!(
                "height must be at least {} pixels",
                Self::MIN_DIMENSION
            )));
        }
        if self.scale <= 0.0 {
            return Err(Error::InvalidConfig(
                "scale must be greater than 0".to_string(),
            ));
        }
        if !self.scale.is_finite() {
            return Err(Error::InvalidConfig(
                "scale must be a finite number".to_string(),
            ));
        }
        Ok(())
    }
}
