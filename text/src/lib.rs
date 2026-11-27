//! Runtime text (`pulsar.ui`): the one owner of how widgets' text is shaped,
//! measured and turned into glyph images.
//!
//! - **Fonts** are embedded in the binary (so a packaged game has them), each
//!   with its licence beside it in `fonts/`: IBM Plex Sans (regular, bold,
//!   italic, bold italic) and JetBrains Mono (regular), both OFL 1.1. No
//!   system font is read, so text is the same on every machine.
//! - **Shaping and line breaking** are `cosmic-text`'s (HarfBuzz-style
//!   shaping, Unicode line breaking and bidirectional text). [`TextSystem`] is
//!   the [`TextMetrics`] layout measures with, and the same shaped result is
//!   drawn, so text never drifts from its box. Shaped text is cached.
//! - **Glyph images** live in one bounded [`atlas::GlyphAtlas`], in two forms.
//!   Small, upright screen text is a hinted coverage raster at its exact pixel
//!   size and horizontal sub-pixel position, as a font rasteriser draws it.
//!   Large, rotated, scaled and world text is a multi-channel distance field
//!   ([`msdf`]), one per glyph whatever its size.

pub mod atlas;
pub mod msdf;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use cosmic_text::{
    fontdb, Attrs, Buffer, CacheKey, CacheKeyFlags, Family, FontSystem, LetterSpacing, Metrics,
    Shaping, Style, SwashCache, SwashContent, Weight, Wrap,
};
use widget_asset::layout::{TextMetrics, TextRequest};
use widget_asset::model::Vec2;

const FONTS: [&[u8]; 5] = [
    include_bytes!("../fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../fonts/IBMPlexSans-Bold.ttf"),
    include_bytes!("../fonts/IBMPlexSans-Italic.ttf"),
    include_bytes!("../fonts/IBMPlexSans-BoldItalic.ttf"),
    include_bytes!("../fonts/JetBrainsMono-Regular.ttf"),
];
pub const SANS: &str = "IBM Plex Sans";
pub const MONO: &str = "JetBrains Mono";

/// The largest pixel size drawn from a hinted raster. Above it hinting no
/// longer shows and a distance field is as good, and holds one image for every
/// size.
pub const MAX_RASTER_PX: f32 = 48.0;

/// Shaped results kept before the least recently used half is dropped.
const SHAPE_CACHE: usize = 2048;

#[derive(Clone, PartialEq, Eq, Hash)]
struct ShapeKey {
    text: String,
    size: u32,
    wrap: Option<u32>,
    line_height: u32,
    spacing: u32,
    bold: bool,
    italic: bool,
    mono: bool,
}

/// A glyph placed by shaping, in the text's own units with the origin at the
/// top-left of its box (before justification).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
    pub font: fontdb::ID,
    pub glyph: u16,
    /// The pen position on the baseline.
    pub x: f32,
    pub y: f32,
    pub weight: Weight,
    pub flags: CacheKeyFlags,
    pub line: u32,
}

/// Shaped and broken text.
#[derive(Debug, Default)]
pub struct ShapedText {
    /// The width of the widest line and the height of all lines.
    pub size: Vec2,
    pub line_widths: Vec<f32>,
    pub glyphs: Vec<ShapedGlyph>,
    /// The font size it was shaped at.
    pub font_size: f32,
}

/// Names one glyph image in the atlas.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum GlyphKey {
    Raster(CacheKey),
    Field(fontdb::ID, u16),
}

struct Shaper {
    fonts: FontSystem,
    swash: SwashCache,
    cache: HashMap<ShapeKey, (Arc<ShapedText>, u64)>,
    clock: u64,
    /// Text shaped since the system was made (diagnostics and tests).
    shaped: u64,
}

/// The shared text system: fonts, the shaping cache and the glyph atlas. One
/// per process is enough; it is behind locks so layout on the game thread and
/// uploads on the render thread share it.
pub struct TextSystem {
    shaper: Mutex<Shaper>,
    atlas: Mutex<atlas::GlyphAtlas<GlyphKey>>,
}

impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A glyph's quad: its atlas texels and where they go, relative to the text
/// box's origin, in output units.
#[derive(Clone, Copy, Debug)]
pub struct GlyphQuad {
    /// `x, y, width, height` of the quad.
    pub rect: [f32; 4],
    /// `u0, v0, u1, v1` in atlas texels.
    pub uv: [f32; 4],
}

impl TextSystem {
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        for data in FONTS {
            db.load_font_data(data.to_vec());
        }
        db.set_sans_serif_family(SANS);
        db.set_monospace_family(MONO);
        Self {
            shaper: Mutex::new(Shaper {
                fonts: FontSystem::new_with_locale_and_db("en-US".into(), db),
                swash: SwashCache::new(),
                cache: HashMap::new(),
                clock: 0,
                shaped: 0,
            }),
            atlas: Mutex::new(atlas::GlyphAtlas::new()),
        }
    }

    /// The atlas, for uploading and inspection.
    pub fn atlas(&self) -> MutexGuard<'_, atlas::GlyphAtlas<GlyphKey>> {
        lock(&self.atlas)
    }

    /// How many distinct texts have been shaped.
    pub fn shaped_count(&self) -> u64 {
        lock(&self.shaper).shaped
    }

    /// Shape and break `request`, cached.
    pub fn shape(&self, request: &TextRequest) -> Arc<ShapedText> {
        let key = ShapeKey {
            text: request.text.clone(),
            size: request.size.to_bits(),
            // A box is often a fraction of a unit narrower than the text
            // measured to size it; half a unit of slack keeps it on one line.
            wrap: request.wrap_width.map(|w| (w + 0.5).to_bits()),
            line_height: request.line_height.to_bits(),
            spacing: request.spacing.to_bits(),
            bold: matches!(request.weight.as_str(), "bold" | "semibold"),
            italic: request.italic,
            mono: request.font == "mono",
        };
        let mut shaper = lock(&self.shaper);
        shaper.clock += 1;
        let clock = shaper.clock;
        if let Some((shaped, used)) = shaper.cache.get_mut(&key) {
            *used = clock;
            return shaped.clone();
        }
        if shaper.cache.len() >= SHAPE_CACHE {
            let mut ages: Vec<u64> = shaper.cache.values().map(|(_, used)| *used).collect();
            ages.sort_unstable();
            let median = ages[ages.len() / 2];
            shaper.cache.retain(|_, (_, used)| *used > median);
        }
        let shaped = Arc::new(shaper.shape(&key));
        shaper.shaped += 1;
        shaper.cache.insert(key, (shaped.clone(), clock));
        shaped
    }

    /// The hinted raster quad of `glyph` drawn at `scale` output pixels per
    /// text unit, with the text box's origin at output position `origin`. The
    /// quad lands on whole pixels, so it is sampled one texel per pixel.
    pub fn raster_glyph(
        &self,
        text: &ShapedText,
        glyph: &ShapedGlyph,
        origin: [f32; 2],
        scale: f32,
    ) -> Option<GlyphQuad> {
        let (key, x, y) = CacheKey::new(
            glyph.font,
            glyph.glyph,
            // Quantised to a sixty-fourth of a pixel, so a size that wobbles
            // does not fill the atlas with near-identical images.
            (text.font_size * scale * 64.0).round() / 64.0,
            (
                origin[0] + glyph.x * scale,
                (origin[1] + glyph.y * scale).round(),
            ),
            glyph.weight,
            glyph.flags,
        );
        let entry = self.atlas().get_or_insert(&GlyphKey::Raster(key), || {
            let mut shaper = lock(&self.shaper);
            let Shaper { fonts, swash, .. } = &mut *shaper;
            let image = swash.get_image_uncached(fonts, key)?;
            if image.placement.width == 0 || image.placement.height == 0 {
                return None;
            }
            // A colour glyph keeps its alpha as coverage; the fonts shipped
            // have none.
            let step = match image.content {
                SwashContent::Color => 4,
                SwashContent::SubpixelMask => 3,
                SwashContent::Mask => 1,
            };
            let texels = image
                .data
                .chunks_exact(step)
                .flat_map(|p| {
                    let a = p[step - 1];
                    [a, a, a, a]
                })
                .collect();
            Some(atlas::Bitmap {
                width: image.placement.width,
                height: image.placement.height,
                texels,
                offset: [image.placement.left as f32, -image.placement.top as f32],
            })
        })?;
        Some(GlyphQuad {
            rect: [
                x as f32 + entry.offset[0],
                y as f32 + entry.offset[1],
                entry.width as f32,
                entry.height as f32,
            ],
            uv: uv_of(&entry),
        })
    }

    /// The distance-field quad of `glyph` at `scale` output units per text
    /// unit, relative to `origin`.
    pub fn field_glyph(
        &self,
        text: &ShapedText,
        glyph: &ShapedGlyph,
        origin: [f32; 2],
        scale: f32,
    ) -> Option<GlyphQuad> {
        let key = GlyphKey::Field(glyph.font, glyph.glyph);
        let entry = self.atlas().get_or_insert(&key, || {
            let shaper = lock(&self.shaper);
            let field = shaper
                .fonts
                .db()
                .with_face_data(glyph.font, |data, index| {
                    msdf::generate(data, index, glyph.glyph)
                })??;
            Some(atlas::Bitmap {
                width: field.width,
                height: field.height,
                texels: field.texels,
                offset: field.offset_em,
            })
        })?;
        let em = text.font_size * scale;
        let texel = em / msdf::EM_TEXELS;
        Some(GlyphQuad {
            rect: [
                origin[0] + glyph.x * scale + entry.offset[0] * em,
                origin[1] + glyph.y * scale + entry.offset[1] * em,
                entry.width as f32 * texel,
                entry.height as f32 * texel,
            ],
            uv: uv_of(&entry),
        })
    }
}

fn uv_of(entry: &atlas::Entry) -> [f32; 4] {
    [
        entry.x as f32,
        entry.y as f32,
        (entry.x + entry.width) as f32,
        (entry.y + entry.height) as f32,
    ]
}

impl helio_ui_api::AtlasSource for TextSystem {
    fn size(&self) -> u32 {
        atlas::ATLAS_SIZE
    }

    fn changes_since(&self, since: u64, upload: &mut dyn FnMut(u32, u32, u32, &[u8])) -> u64 {
        self.atlas().changes_since(since, upload)
    }
}

impl Shaper {
    fn shape(&mut self, key: &ShapeKey) -> ShapedText {
        let size = f32::from_bits(key.size).max(1.0);
        let line_height = size * f32::from_bits(key.line_height).max(0.1);
        let spacing = f32::from_bits(key.spacing);
        let mut attrs = Attrs::new()
            .family(Family::Name(if key.mono { MONO } else { SANS }))
            .weight(if key.bold && !key.mono {
                Weight::BOLD
            } else {
                Weight::NORMAL
            })
            .style(if key.italic && !key.mono {
                Style::Italic
            } else {
                Style::Normal
            });
        if spacing != 0.0 {
            attrs.letter_spacing_opt = Some(LetterSpacing(spacing / size));
        }
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(size, line_height));
        let wrap = key.wrap.map(f32::from_bits).filter(|w| *w > 0.0);
        buffer.set_wrap(
            &mut self.fonts,
            if wrap.is_some() {
                Wrap::WordOrGlyph
            } else {
                Wrap::None
            },
        );
        buffer.set_size(&mut self.fonts, wrap, None);
        buffer.set_text(&mut self.fonts, &key.text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        let mut out = ShapedText {
            font_size: size,
            ..Default::default()
        };
        let mut lines = 0u32;
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                out.glyphs.push(ShapedGlyph {
                    font: glyph.font_id,
                    glyph: glyph.glyph_id,
                    x: glyph.x + glyph.x_offset * glyph.font_size,
                    y: run.line_y - glyph.y_offset * glyph.font_size + glyph.y,
                    weight: glyph.font_weight,
                    flags: glyph.cache_key_flags,
                    line: lines,
                });
            }
            out.line_widths.push(run.line_w);
            out.size.x = out.size.x.max(run.line_w);
            lines += 1;
        }
        out.size.y = lines.max(1) as f32 * line_height;
        out
    }
}

impl TextMetrics for TextSystem {
    fn measure(&self, request: &TextRequest) -> Vec2 {
        if request.text.is_empty() {
            return Vec2::new(0.0, request.size.max(0.0) * request.line_height);
        }
        let shaped = self.shape(request);
        // Rounded up, so a box sized to the text never wraps it.
        Vec2::new(shaped.size.x.ceil(), shaped.size.y)
    }
}

#[cfg(test)]
mod tests;
