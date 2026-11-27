//! A laid-out widget's drawing commands as UI primitives.
//!
//! Commands are in widget units. A screen widget's primitives are in output
//! pixels: scaled and moved by the widget's [`Mapping`], with box edges and
//! hinted glyphs snapped to whole pixels. A world widget's stay in widget
//! units and draw their text from distance fields.

use helio_ui_api as api;
use helio_ui_api::UiPrimitive;
use widget_asset::display::{fit_image, Cmd, Fill, Justify};
use widget_asset::layout::TextRequest;
use widget_asset::model::geom::{Color, Rect, Vec2};
use widget_text::{TextSystem, MAX_RASTER_PX};

/// How widget units map into the layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mapping {
    /// Layer units per widget unit.
    pub scale: f32,
    /// The widget's origin, in widget units (applied before the scale).
    pub origin: Vec2,
    /// The layer is output pixels: edges snap and small upright text is a
    /// hinted raster. Off for world widgets.
    pub pixels: bool,
    /// The world placement row the primitives name.
    pub placement: u32,
}

/// An image the widget shows, as the scene holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Image {
    /// The scene texture table slot ([`api::NO_TEXTURE`] while it has none).
    pub slot: u32,
    /// Its size in pixels.
    pub size: Vec2,
}

/// The loaded image a path names, if it is loaded.
pub type Images<'a> = &'a mut dyn FnMut(&str) -> Option<Image>;

const NO_CLIP: [f32; 4] = [-1.0e7, -1.0e7, 1.0e7, 1.0e7];

#[derive(Clone, Copy)]
struct Clip {
    bounds: [f32; 4],
    radii: [f32; 4],
}

/// A 2D affine as columns `[a, b, c, d]` and a translation.
#[derive(Clone, Copy, PartialEq)]
struct Turn {
    m: [f32; 4],
    t: [f32; 2],
}

impl Turn {
    const IDENTITY: Turn = Turn {
        m: [1.0, 0.0, 0.0, 1.0],
        t: [0.0, 0.0],
    };

    fn about(pivot: Vec2, degrees: f32) -> Turn {
        let (sin, cos) = degrees.to_radians().sin_cos();
        Turn {
            m: [cos, sin, -sin, cos],
            t: [
                pivot.x - (cos * pivot.x - sin * pivot.y),
                pivot.y - (sin * pivot.x + cos * pivot.y),
            ],
        }
    }

    /// `inner` first, then `self`.
    fn after(&self, inner: &Turn) -> Turn {
        let [a, b, c, d] = self.m;
        let [e, f, g, h] = inner.m;
        let [x, y] = inner.t;
        Turn {
            m: [a * e + c * f, b * e + d * f, a * g + c * h, b * g + d * h],
            t: [a * x + c * y + self.t[0], b * x + d * y + self.t[1]],
        }
    }
}

struct Painter<'a> {
    map: Mapping,
    text: &'a TextSystem,
    clips: Vec<Clip>,
    turns: Vec<Turn>,
    out: &'a mut Vec<UiPrimitive>,
}

fn packed(color: Color) -> u32 {
    api::pack_color([color.r, color.g, color.b, color.a])
}

impl Painter<'_> {
    fn point(&self, p: Vec2) -> Vec2 {
        Vec2::new(
            (p.x + self.map.origin.x) * self.map.scale,
            (p.y + self.map.origin.y) * self.map.scale,
        )
    }

    fn upright(&self) -> bool {
        self.turns.last().is_none_or(|t| *t == Turn::IDENTITY)
    }

    /// A widget rectangle in layer units, its edges on whole pixels when the
    /// layer is pixels and nothing is rotated.
    fn rect(&self, r: Rect) -> [f32; 4] {
        let a = self.point(r.origin());
        let b = self.point(Vec2::new(r.right(), r.bottom()));
        if self.map.pixels && self.upright() {
            let (x0, y0) = (a.x.round(), a.y.round());
            // Something drawn at all stays at least a pixel.
            let w = (b.x.round() - x0).max(if r.w > 0.0 { 1.0 } else { 0.0 });
            let h = (b.y.round() - y0).max(if r.h > 0.0 { 1.0 } else { 0.0 });
            [x0, y0, w, h]
        } else {
            [a.x, a.y, b.x - a.x, b.y - a.y]
        }
    }

    fn scaled(&self, v: [f32; 4]) -> [f32; 4] {
        v.map(|x| x * self.map.scale)
    }

    fn new_primitive(&self, kind: u32, rect: [f32; 4]) -> UiPrimitive {
        let clip = self.clips.last().copied().unwrap_or(Clip {
            bounds: NO_CLIP,
            radii: [0.0; 4],
        });
        let turn = self.turns.last().copied().unwrap_or(Turn::IDENTITY);
        UiPrimitive {
            rect,
            clip: clip.bounds,
            clip_radii: clip.radii,
            transform: turn.m,
            translation: turn.t,
            info: [kind, api::NO_TEXTURE, self.map.placement, 0],
            ..Default::default()
        }
    }

    fn push_clip(&mut self, rect: [f32; 4], radii: [f32; 4]) {
        let outer = self.clips.last().copied();
        let mut bounds = [rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3]];
        let mut radii = radii;
        if let Some(outer) = outer {
            let inside = bounds[0] >= outer.bounds[0]
                && bounds[1] >= outer.bounds[1]
                && bounds[2] <= outer.bounds[2]
                && bounds[3] <= outer.bounds[3];
            // One rounded box clips each primitive: the intersection, rounded
            // where the inner box lies inside the outer one, else as the
            // rounded one of the two.
            if !inside && radii == [0.0; 4] {
                radii = outer.radii;
            }
            bounds = [
                bounds[0].max(outer.bounds[0]),
                bounds[1].max(outer.bounds[1]),
                bounds[2].min(outer.bounds[2]),
                bounds[3].min(outer.bounds[3]),
            ];
        }
        bounds[2] = bounds[2].max(bounds[0]);
        bounds[3] = bounds[3].max(bounds[1]);
        self.clips.push(Clip { bounds, radii });
    }

    fn quad(
        &mut self,
        rect: Rect,
        radii: [f32; 4],
        fill: Fill,
        border: Option<(f32, Color)>,
        dash: f32,
    ) {
        let mut p = self.new_primitive(api::KIND_BOX, self.rect(rect));
        p.radii = self.scaled(radii);
        match fill {
            Fill::Solid(color) => p.colors[0] = packed(color),
            Fill::Gradient { angle, from, to } => {
                p.info[0] |= api::FLAG_LINEAR_GRADIENT;
                p.colors[0] = packed(from);
                p.colors[2] = packed(to);
                p.params[0] = angle.to_radians();
            }
            Fill::Ramp {
                radial,
                angle,
                from,
                mid,
                to,
            } => {
                p.info[0] |= if radial {
                    api::FLAG_RADIAL_GRADIENT
                } else {
                    api::FLAG_LINEAR_GRADIENT
                };
                p.colors[0] = packed(from);
                p.colors[2] = packed(to);
                p.params[0] = angle.to_radians();
                if let Some((at, color)) = mid {
                    p.info[0] |= api::FLAG_MIDDLE_STOP;
                    p.colors[1] = packed(color);
                    p.params[1] = at;
                }
            }
        }
        if let Some((width, color)) = border {
            let mut width = width * self.map.scale;
            if self.map.pixels {
                width = width.round().max(1.0);
            }
            p.border_softness[0] = width;
            p.colors[3] = packed(color);
            p.params[2] = dash * self.map.scale;
        }
        self.out.push(p);
    }

    #[allow(clippy::too_many_arguments)]
    fn text_run(
        &mut self,
        rect: Rect,
        request: TextRequest,
        color: Color,
        justify: Justify,
        outline: Option<(f32, Color)>,
    ) {
        let shaped = self.text.shape(&request);
        let scale = self.map.scale;
        // A hinted raster for small upright text on screen; a distance field
        // for the rest, and for outlines, which a raster cannot grow.
        let raster = self.map.pixels
            && self.upright()
            && outline.is_none()
            && shaped.font_size * scale <= MAX_RASTER_PX;
        let origin = self.point(rect.origin());
        for glyph in &shaped.glyphs {
            let line = shaped.line_widths[glyph.line as usize];
            let slack = rect.w - line;
            let shift = match justify {
                Justify::Left => 0.0,
                Justify::Center => slack * 0.5,
                Justify::Right => slack,
            };
            let mut at = [origin.x + shift * scale, origin.y];
            if self.map.pixels && self.upright() {
                // The line starts on a pixel, so centred text keeps the
                // glyph images it has.
                at = [at[0].round(), at[1].round()];
            }
            let quad = if raster {
                self.text.raster_glyph(&shaped, glyph, at, scale)
            } else {
                self.text.field_glyph(&shaped, glyph, at, scale)
            };
            let Some(quad) = quad else {
                continue;
            };
            let kind = if raster {
                api::KIND_GLYPH
            } else {
                api::KIND_FIELD_GLYPH
            };
            let mut p = self.new_primitive(kind, quad.rect);
            p.uv = quad.uv;
            p.colors[0] = packed(color);
            // Layer units per atlas texel, for the outline's width.
            p.params[3] = quad.rect[2] / (quad.uv[2] - quad.uv[0]).max(1.0);
            if let Some((width, outline)) = outline {
                p.border_softness[0] = width * scale;
                p.colors[3] = packed(outline);
            }
            self.out.push(p);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn image(
        &mut self,
        rect: Rect,
        image: Image,
        tint: Color,
        stretch: &str,
        radii: [f32; 4],
        slice: [f32; 4],
        slice_scale: f32,
    ) {
        if slice != [0.0; 4] && image.size.x > 0.0 && image.size.y > 0.0 {
            // Nine-slice: the corners keep their size, the edges stretch one
            // way and the middle both.
            let [l, t, r, b] = slice;
            let xs = [
                rect.x,
                rect.x + (l * slice_scale).min(rect.w),
                (rect.right() - r * slice_scale).max(rect.x),
                rect.right(),
            ];
            let ys = [
                rect.y,
                rect.y + (t * slice_scale).min(rect.h),
                (rect.bottom() - b * slice_scale).max(rect.y),
                rect.bottom(),
            ];
            let us = [0.0, l / image.size.x, 1.0 - r / image.size.x, 1.0];
            let vs = [0.0, t / image.size.y, 1.0 - b / image.size.y, 1.0];
            for row in 0..3 {
                for column in 0..3 {
                    let cell = Rect::new(
                        xs[column],
                        ys[row],
                        xs[column + 1] - xs[column],
                        ys[row + 1] - ys[row],
                    );
                    if cell.w <= 0.0 || cell.h <= 0.0 {
                        continue;
                    }
                    let mut p = self.new_primitive(api::KIND_IMAGE, self.rect(cell));
                    p.uv = [us[column], vs[row], us[column + 1], vs[row + 1]];
                    p.colors[0] = packed(tint);
                    p.info[1] = image.slot;
                    // Only the outer corners are rounded.
                    let corner = |c: usize, wanted: (usize, usize)| {
                        if (column, row) == wanted {
                            radii[c] * self.map.scale
                        } else {
                            0.0
                        }
                    };
                    p.radii = [
                        corner(0, (0, 0)),
                        corner(1, (2, 0)),
                        corner(2, (2, 2)),
                        corner(3, (0, 2)),
                    ];
                    self.out.push(p);
                }
            }
            return;
        }
        let fitted = fit_image(rect, image.size, stretch);
        // What reaches past the box (cover, natural size) is clipped to it,
        // with its corners.
        let spills = fitted.x < rect.x - 0.01 || fitted.y < rect.y - 0.01;
        if spills {
            let bounds = self.rect(rect);
            let radii = self.scaled(radii);
            self.push_clip(bounds, radii);
        }
        let mut p = self.new_primitive(api::KIND_IMAGE, self.rect(fitted));
        p.uv = [0.0, 0.0, 1.0, 1.0];
        p.colors[0] = packed(tint);
        p.info[1] = image.slot;
        if !spills {
            p.radii = self.scaled(radii);
        }
        self.out.push(p);
        if spills {
            self.clips.pop();
        }
    }

    /// A round-capped line between two widget points.
    fn stroke(&mut self, a: Vec2, b: Vec2, width: f32, color: Color) {
        let (a, b) = (self.point(a), self.point(b));
        let width = width * self.map.scale;
        let rect = [
            a.x.min(b.x),
            a.y.min(b.y),
            (a.x - b.x).abs(),
            (a.y - b.y).abs(),
        ];
        let mut p = self.new_primitive(api::KIND_STROKE, rect);
        p.uv = [a.x, a.y, b.x, b.y];
        p.border_softness[0] = width;
        p.colors[0] = packed(color);
        self.out.push(p);
    }
}

/// Append the primitives that draw `commands` to `out`.
pub fn primitives(
    commands: &[Cmd],
    map: Mapping,
    text: &TextSystem,
    images: Images<'_>,
    out: &mut Vec<UiPrimitive>,
) {
    let mut painter = Painter {
        map,
        text,
        clips: Vec::new(),
        turns: Vec::new(),
        out,
    };
    for command in commands {
        match command {
            Cmd::PushClip(rect) => {
                let rect = painter.rect(*rect);
                painter.push_clip(rect, [0.0; 4]);
            }
            Cmd::ClipRadii(radii) => {
                let radii = painter.scaled(*radii);
                if let Some(clip) = painter.clips.last_mut() {
                    clip.radii = radii;
                }
            }
            Cmd::PopClip => {
                painter.clips.pop();
            }
            Cmd::PushRotation { degrees, pivot } => {
                let turn = Turn::about(painter.point(*pivot), *degrees);
                let outer = painter.turns.last().copied().unwrap_or(Turn::IDENTITY);
                painter.turns.push(outer.after(&turn));
            }
            Cmd::PopRotation => {
                painter.turns.pop();
            }
            Cmd::Shadow {
                rect,
                radii,
                blur,
                color,
            } => {
                let mut p = painter.new_primitive(api::KIND_SHADOW, painter.rect(*rect));
                p.radii = painter.scaled(*radii);
                p.border_softness[1] = blur * painter.map.scale;
                p.colors[0] = packed(*color);
                painter.out.push(p);
            }
            Cmd::Quad {
                rect,
                radii,
                fill,
                border,
                dash,
            } => painter.quad(*rect, *radii, *fill, *border, *dash),
            Cmd::Text {
                rect,
                text: content,
                size,
                weight,
                italic,
                color,
                justify,
                wrap,
                line_height,
                font,
                spacing,
                outline,
            } => {
                // The command's sizes are in widget space, as layout measured
                // them (before the mapping).
                let request = TextRequest {
                    text: content.clone(),
                    size: *size,
                    weight: weight.clone(),
                    italic: *italic,
                    wrap_width: wrap.then_some(rect.w),
                    line_height: *line_height,
                    font: font.clone(),
                    spacing: *spacing,
                };
                painter.text_run(*rect, request, *color, *justify, *outline);
            }
            Cmd::Image {
                rect,
                path,
                tint,
                stretch,
                radii,
                slice,
                slice_scale,
                ..
            } => {
                if let Some(image) = images(path) {
                    if image.slot != api::NO_TEXTURE {
                        painter.image(
                            *rect,
                            image,
                            *tint,
                            stretch,
                            *radii,
                            *slice,
                            *slice_scale,
                        );
                    }
                }
            }
            Cmd::Check { rect, color } => {
                let at = |x: f32, y: f32| Vec2::new(rect.x + rect.w * x, rect.y + rect.h * y);
                let width = (rect.w * 0.16).max(1.5);
                painter.stroke(at(0.05, 0.55), at(0.38, 0.88), width, *color);
                painter.stroke(at(0.38, 0.88), at(0.95, 0.15), width, *color);
            }
            Cmd::Chevron {
                center,
                size,
                color,
            } => {
                let width = (size * 0.32).max(1.2);
                let low = Vec2::new(center.x, center.y + size * 0.5);
                for side in [-1.0, 1.0] {
                    let high = Vec2::new(center.x + side * size, center.y - size * 0.5);
                    painter.stroke(high, low, width, *color);
                }
            }
            // Design-time stand-ins are not part of the running widget.
            Cmd::Placeholder { .. } => {}
        }
    }
}
