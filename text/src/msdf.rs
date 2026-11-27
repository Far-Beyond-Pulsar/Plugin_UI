//! Multi-channel signed distance fields from glyph outlines (Chlumsky's
//! method): edges between a contour's corners are coloured so that two of the
//! three channels cross at every corner, which keeps corners sharp at any
//! magnification, where a single-channel field rounds them. The fourth
//! channel holds the true distance, which outlines and glows read.
//!
//! Curves are flattened to short segments (finer than a texel at the atlas
//! size), so every distance is a point-to-segment distance. The sign comes
//! from the outline's nonzero winding at each texel, which holds for both
//! TrueType and CFF contour directions and also corrects the channels where
//! contours overlap.

/// Atlas texels per em.
pub const EM_TEXELS: f32 = 40.0;
/// The distance in texels mapped to a channel's full swing on each side of the
/// edge.
pub const SPREAD_TEXELS: f32 = 4.0;
/// Segments a curve is flattened into.
const CURVE_STEPS: usize = 12;

const RED: u8 = 1;
const GREEN: u8 = 2;
const BLUE: u8 = 4;
const WHITE: u8 = RED | GREEN | BLUE;

type P = (f32, f32);

#[derive(Clone, Copy)]
enum Curve {
    Line(P, P),
    Quad(P, P, P),
    Cubic(P, P, P, P),
}

impl Curve {
    fn at(&self, t: f32) -> P {
        let lerp = |a: P, b: P| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        match *self {
            Curve::Line(a, b) => lerp(a, b),
            Curve::Quad(a, b, c) => lerp(lerp(a, b), lerp(b, c)),
            Curve::Cubic(a, b, c, d) => {
                let (ab, bc, cd) = (lerp(a, b), lerp(b, c), lerp(c, d));
                lerp(lerp(ab, bc), lerp(bc, cd))
            }
        }
    }

    fn points(&self) -> Vec<P> {
        let steps = match self {
            Curve::Line(..) => 1,
            _ => CURVE_STEPS,
        };
        (0..=steps).map(|i| self.at(i as f32 / steps as f32)).collect()
    }
}

/// One edge of a contour: a curve, flattened, with the channels it feeds.
struct Edge {
    points: Vec<P>,
    channels: u8,
}

/// A glyph's outline, in atlas texels with y pointing down.
#[derive(Default)]
struct Outline {
    scale: f32,
    contours: Vec<Vec<Curve>>,
    current: Vec<Curve>,
    start: P,
    pen: P,
}

impl Outline {
    fn map(&self, x: f32, y: f32) -> P {
        (x * self.scale, -y * self.scale)
    }

    fn push(&mut self, curve: Curve, end: P) {
        let from = self.pen;
        self.pen = end;
        let degenerate = match curve {
            Curve::Line(..) => (end.0 - from.0).abs() < 1e-4 && (end.1 - from.1).abs() < 1e-4,
            _ => false,
        };
        if !degenerate {
            self.current.push(curve);
        }
    }

    fn finish(&mut self) {
        if self.pen != self.start {
            let (pen, start) = (self.pen, self.start);
            self.push(Curve::Line(pen, start), start);
        }
        if !self.current.is_empty() {
            self.contours.push(std::mem::take(&mut self.current));
        }
    }
}

impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish();
        self.start = self.map(x, y);
        self.pen = self.start;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.push(Curve::Line(self.pen, p), p);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, p) = (self.map(x1, y1), self.map(x, y));
        self.push(Curve::Quad(self.pen, c, p), p);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (a, b, p) = (self.map(x1, y1), self.map(x2, y2), self.map(x, y));
        self.push(Curve::Cubic(self.pen, a, b, p), p);
    }
    fn close(&mut self) {
        self.finish();
        self.pen = self.start;
    }
}

fn direction(a: P, b: P) -> P {
    let d = (b.0 - a.0, b.1 - a.1);
    let len = (d.0 * d.0 + d.1 * d.1).sqrt();
    if len > 0.0 {
        (d.0 / len, d.1 / len)
    } else {
        (0.0, 0.0)
    }
}

/// Colour a contour's edges: each run of smooth edges between two corners gets
/// two channels, and neighbouring runs share exactly one.
fn color_contour(curves: &[Curve]) -> Vec<Edge> {
    let mut edges: Vec<Edge> = curves
        .iter()
        .map(|c| Edge {
            points: c.points(),
            channels: WHITE,
        })
        .collect();
    let n = edges.len();
    // A corner is where the outline turns by more than about 18 degrees.
    let corners: Vec<usize> = (0..n)
        .filter(|&i| {
            let before = &edges[(i + n - 1) % n].points;
            let after = &edges[i].points;
            let a = direction(before[before.len() - 2], before[before.len() - 1]);
            let b = direction(after[0], after[1]);
            let dot = a.0 * b.0 + a.1 * b.1;
            let cross = a.0 * b.1 - a.1 * b.0;
            dot <= 0.0 || cross.abs() > 0.31
        })
        .collect();
    const PAIRS: [u8; 3] = [RED | BLUE, RED | GREEN, GREEN | BLUE];
    match corners.len() {
        0 => {}
        1 => {
            // One corner (a drop shape): thirds of the contour, the first
            // and last differing, so the corner still has an edge change.
            let start = corners[0];
            let flat: Vec<P> = (0..n)
                .flat_map(|i| {
                    let points = &edges[(start + i) % n].points;
                    points[..points.len() - 1].to_vec()
                })
                .chain(std::iter::once(edges[start].points[0]))
                .collect();
            let segments = flat.len() - 1;
            let mut split = Vec::new();
            for (part, channels) in [RED | BLUE, WHITE, RED | GREEN].into_iter().enumerate() {
                let from = segments * part / 3;
                let to = segments * (part + 1) / 3;
                if to > from {
                    split.push(Edge {
                        points: flat[from..=to].to_vec(),
                        channels,
                    });
                }
            }
            return split;
        }
        count => {
            for (run, &corner) in corners.iter().enumerate() {
                let end = corners[(run + 1) % count];
                let mut channels = PAIRS[run % 3];
                // The last run must differ from the first, which it meets.
                if run + 1 == count && run % 3 == 0 {
                    channels = PAIRS[1];
                }
                let mut i = corner;
                loop {
                    edges[i].channels = channels;
                    i = (i + 1) % n;
                    if i == end {
                        break;
                    }
                }
            }
        }
    }
    edges
}

/// The nearest point of one edge to `p`: squared distance, how orthogonal the
/// approach is (to break ties at shared endpoints) and the signed
/// pseudo-distance, which continues the edge's end tangents past its ends.
fn edge_distance(edge: &Edge, p: P) -> (f32, f32, f32) {
    let mut best = (f32::MAX, 0.0f32, 0.0f32);
    let last = edge.points.len() - 2;
    for (i, pair) in edge.points.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        let ab = (b.0 - a.0, b.1 - a.1);
        let ap = (p.0 - a.0, p.1 - a.1);
        let len2 = ab.0 * ab.0 + ab.1 * ab.1;
        if len2 <= 1e-12 {
            continue;
        }
        let raw = (ap.0 * ab.0 + ap.1 * ab.1) / len2;
        let t = raw.clamp(0.0, 1.0);
        let d = (ap.0 - ab.0 * t, ap.1 - ab.1 * t);
        let dist2 = d.0 * d.0 + d.1 * d.1;
        let len = len2.sqrt();
        let cross = (ab.0 * ap.1 - ab.1 * ap.0) / len;
        // 0 for a perpendicular approach, up to 1 along the segment.
        let along = if dist2 > 1e-12 {
            ((ab.0 * d.0 + ab.1 * d.1) / (len * dist2.sqrt())).abs()
        } else {
            0.0
        };
        if dist2 + 1e-6 < best.0 || (dist2 < best.0 + 1e-6 && along < best.1) {
            let past_end = (i == 0 && raw < 0.0) || (i == last && raw > 1.0);
            let signed = if past_end {
                cross
            } else {
                dist2.sqrt().copysign(cross)
            };
            best = (dist2, along, signed);
        }
    }
    best
}

fn winding(contours: &[Vec<Edge>], p: P) -> i32 {
    let mut wind = 0;
    for edge in contours.iter().flatten() {
        for pair in edge.points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if (a.1 <= p.1) != (b.1 <= p.1) {
                let x = a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0);
                if x > p.0 {
                    wind += if b.1 > a.1 { 1 } else { -1 };
                }
            }
        }
    }
    wind
}

/// A glyph's distance field: RGBA texels (three edge channels and the true
/// distance), its size, and where its top-left texel sits relative to the pen
/// position on the baseline, in ems with y down.
pub struct Field {
    pub width: u32,
    pub height: u32,
    pub texels: Vec<u8>,
    pub offset_em: [f32; 2],
}

fn encode(distance: f32) -> u8 {
    ((0.5 + distance / (2.0 * SPREAD_TEXELS)).clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The field of glyph `glyph` of the font `data` (face `index`), or `None` for
/// a glyph with no outline (a space).
pub fn generate(data: &[u8], index: u32, glyph: u16) -> Option<Field> {
    let face = ttf_parser::Face::parse(data, index).ok()?;
    let mut outline = Outline {
        scale: EM_TEXELS / f32::from(face.units_per_em()),
        ..Default::default()
    };
    face.outline_glyph(ttf_parser::GlyphId(glyph), &mut outline)?;
    outline.finish();
    let contours: Vec<Vec<Edge>> = outline.contours.iter().map(|c| color_contour(c)).collect();
    let (mut min, mut max) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
    for p in contours.iter().flatten().flat_map(|e| &e.points) {
        min = (min.0.min(p.0), min.1.min(p.1));
        max = (max.0.max(p.0), max.1.max(p.1));
    }
    if min.0 > max.0 {
        return None;
    }
    let pad = SPREAD_TEXELS.ceil();
    let origin = (min.0.floor() - pad, min.1.floor() - pad);
    let width = (max.0.ceil() + pad - origin.0) as u32;
    let height = (max.1.ceil() + pad - origin.1) as u32;

    // Which sign `edge_distance` gives the inside depends on the font's
    // contour direction; find it once from the winding.
    let mut agree = 0i32;
    let mut raw = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let p = (origin.0 + x as f32 + 0.5, origin.1 + y as f32 + 0.5);
            let mut channel = [(f32::MAX, 1.0f32, 0.0f32); 3];
            let mut nearest = f32::MAX;
            let mut nearest_signed = 0.0;
            for edge in contours.iter().flatten() {
                let found = edge_distance(edge, p);
                if found.0 < nearest {
                    nearest = found.0;
                    nearest_signed = found.2;
                }
                for (bit, best) in [RED, GREEN, BLUE].into_iter().zip(&mut channel) {
                    if edge.channels & bit != 0
                        && (found.0 + 1e-6 < best.0
                            || (found.0 < best.0 + 1e-6 && found.1 < best.1))
                    {
                        *best = found;
                    }
                }
            }
            let inside = winding(&contours, p) != 0;
            if nearest > 0.25 {
                agree += if (nearest_signed > 0.0) == inside { 1 } else { -1 };
            }
            raw.push((channel.map(|c| c.2), nearest.sqrt(), inside));
        }
    }
    let flip = if agree < 0 { -1.0 } else { 1.0 };
    let mut texels = Vec::with_capacity(raw.len() * 4);
    for (channels, true_distance, inside) in raw {
        let mut channels = channels.map(|c| c * flip);
        let true_signed = if inside {
            true_distance
        } else {
            -true_distance
        };
        // The median must put the texel on the side the outline does; where it
        // does not (clashing edge colours, overlapping contours) the texel
        // falls back to the true distance.
        let median = channels[0]
            .max(channels[1])
            .min(channels[0].min(channels[1]).max(channels[2]));
        if (median > 0.0) != inside && true_distance > 0.05 {
            channels = [true_signed; 3];
        }
        texels.extend(channels.map(encode));
        texels.push(encode(true_signed));
    }
    Some(Field {
        width,
        height,
        texels,
        offset_em: [origin.0 / EM_TEXELS, origin.1 / EM_TEXELS],
    })
}
