// What both UI passes share: the primitive's coverage and colour. Everything
// is analytic: rounded boxes by signed distance, soft shadows by integrating a
// Gaussian over the rounded box, glyphs from the atlas (a coverage raster or a
// multi-channel distance field), images from the scene's texture table.
// Output is premultiplied alpha.

struct UiGlobals {
    // Output size in pixels.
    output: vec2<f32>,
    // 1 when the target is an sRGB format (the shader writes linear values).
    linear_output: u32,
    _pad: u32,
}

@group(0) @binding(0) var<uniform> globals: UiGlobals;
@group(0) @binding(1) var<storage, read> primitives: array<UiPrimitive>;
@group(0) @binding(2) var atlas: texture_2d<f32>;
@group(0) @binding(3) var atlas_sampler: sampler;

@group(1) @binding(0) var scene_sampler: sampler;
@group(1) @binding(1) var scene_textures: binding_array<texture_2d<f32>, 256>;

const KIND_BOX: u32 = 1u;
const KIND_SHADOW: u32 = 2u;
const KIND_IMAGE: u32 = 3u;
const KIND_GLYPH: u32 = 4u;
const KIND_FIELD_GLYPH: u32 = 5u;
const KIND_STROKE: u32 = 6u;
const FLAG_LINEAR_GRADIENT: u32 = 256u;
const FLAG_RADIAL_GRADIENT: u32 = 512u;
const FLAG_MIDDLE_STOP: u32 = 1024u;
const NO_TEXTURE: u32 = 0xffffffffu;
// The distance field's spread in atlas texels (`widget_text::msdf`).
const FIELD_SPREAD: f32 = 4.0;

struct UiVarying {
    @builtin(position) position: vec4<f32>,
    // The point in the primitive's own units, before its transform.
    @location(0) local: vec2<f32>,
    // The point after the transform: what the clip is in.
    @location(1) laid: vec2<f32>,
    @location(2) @interpolate(flat) instance: u32,
}

fn quad_corner(vertex: u32) -> vec2<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    return corners[vertex];
}

// How far the quad reaches beyond the primitive's box.
fn quad_reach(p: UiPrimitive, pixel: f32) -> f32 {
    let kind = p.info.x & 255u;
    if kind == KIND_SHADOW {
        return p.border_softness.y * 1.5 + pixel;
    }
    if kind == KIND_STROKE {
        return p.border_softness.x + pixel;
    }
    if kind == KIND_GLYPH {
        return 0.0;
    }
    return pixel;
}

fn transformed(p: UiPrimitive, local: vec2<f32>) -> vec2<f32> {
    let m = mat2x2<f32>(p.transform.xy, p.transform.zw);
    return m * local + p.translation;
}

fn corner_radius(radii: vec4<f32>, from_centre: vec2<f32>) -> f32 {
    if from_centre.y < 0.0 {
        return select(radii.y, radii.x, from_centre.x < 0.0);
    }
    return select(radii.z, radii.w, from_centre.x < 0.0);
}

// Signed distance to a box with a radius per corner (negative inside).
fn rounded_box(point: vec2<f32>, box_: vec4<f32>, radii: vec4<f32>) -> f32 {
    let half = box_.zw * 0.5;
    let q = point - box_.xy - half;
    let r = min(corner_radius(radii, q), min(half.x, half.y));
    let d = abs(q) - half + vec2<f32>(r);
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0) - r;
}

// Anti-aliased coverage of a distance, `width` being one pixel in its units.
fn coverage(distance: f32, width: f32) -> f32 {
    return clamp(0.5 - distance / max(width, 1e-5), 0.0, 1.0);
}

fn erf2(x: vec2<f32>) -> vec2<f32> {
    let s = sign(x);
    let a = abs(x);
    var r = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    r = r * r;
    return s - s / (r * r);
}

// The blurred rounded box along one horizontal line (Evan Wallace's method).
fn shadow_line(x: f32, y: f32, sigma: f32, corner: f32, half: vec2<f32>) -> f32 {
    let delta = min(half.y - corner - abs(y), 0.0);
    let curved = half.x - corner + sqrt(max(0.0, corner * corner - delta * delta));
    let integral = 0.5 + 0.5 * erf2((x + vec2<f32>(-curved, curved)) * (sqrt(0.5) / sigma));
    return integral.y - integral.x;
}

fn shadow_coverage(point: vec2<f32>, box_: vec4<f32>, radii: vec4<f32>, sigma: f32) -> f32 {
    let half = box_.zw * 0.5;
    let q = point - box_.xy - half;
    let corner = min(corner_radius(radii, q), min(half.x, half.y));
    let low = q.y - half.y;
    let high = q.y + half.y;
    let start = clamp(-3.0 * sigma, low, high);
    let end = clamp(3.0 * sigma, low, high);
    let step_ = (end - start) / 4.0;
    var y = start + step_ * 0.5;
    var value = 0.0;
    for (var i = 0; i < 4; i += 1) {
        let g = exp(-(y * y) / (2.0 * sigma * sigma)) / (sqrt(6.283185307) * sigma);
        value += shadow_line(q.x, q.y - y, sigma, corner, half) * g * step_;
        y += step_;
    }
    return value;
}

// Premultiplied, still in sRGB: gradients interpolate premultiplied.
fn premultiplied(packed: u32) -> vec4<f32> {
    let c = unpack4x8unorm(packed);
    return vec4<f32>(c.rgb * c.a, c.a);
}

fn fill_colour(p: UiPrimitive, local: vec2<f32>) -> vec4<f32> {
    let flags = p.info.x;
    let first = premultiplied(p.colors.x);
    if (flags & (FLAG_LINEAR_GRADIENT | FLAG_RADIAL_GRADIENT)) == 0u {
        return first;
    }
    let half = max(p.rect.zw * 0.5, vec2<f32>(1e-4));
    let q = local - p.rect.xy - half;
    var t = 0.0;
    if (flags & FLAG_RADIAL_GRADIENT) != 0u {
        t = length(q / half);
    } else {
        // CSS angles: 0 points up, 90 right; the gradient spans the box.
        let direction = vec2<f32>(sin(p.params.x), -cos(p.params.x));
        let span = abs(direction.x) * p.rect.z + abs(direction.y) * p.rect.w;
        t = dot(q, direction) / max(span, 1e-4) + 0.5;
    }
    t = clamp(t, 0.0, 1.0);
    let last = premultiplied(p.colors.z);
    if (flags & FLAG_MIDDLE_STOP) != 0u {
        let at = clamp(p.params.y, 0.001, 0.999);
        let middle = premultiplied(p.colors.y);
        if t < at {
            return mix(first, middle, t / at);
        }
        return mix(middle, last, (t - at) / (1.0 - at));
    }
    return mix(first, last, t);
}

fn dash_on(p: UiPrimitive, local: vec2<f32>) -> f32 {
    let dash = p.params.z;
    if dash <= 0.0 {
        return 1.0;
    }
    let q = local - p.rect.xy;
    let to_horizontal = min(q.y, p.rect.w - q.y);
    let to_vertical = min(q.x, p.rect.z - q.x);
    let along = select(q.y, q.x, to_horizontal < to_vertical);
    return select(0.0, 1.0, fract(along / (dash * 2.0)) < 0.5);
}

fn median3(v: vec3<f32>) -> f32 {
    return max(min(v.x, v.y), min(max(v.x, v.y), v.z));
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let low = c * 12.92;
    let high = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, c <= vec3<f32>(0.0031308));
}

// The primitive's colour at this fragment, premultiplied, encoded for the
// target. Derivatives are taken here, before any branch on them, so the
// function must run in uniform control flow.
fn shade(in: UiVarying) -> vec4<f32> {
    let p = primitives[in.instance];
    let kind = p.info.x & 255u;
    let footprint = fwidth(in.local);
    let pixel = max(max(footprint.x, footprint.y), 1e-5);
    let size = vec2<f32>(textureDimensions(atlas));
    let along = (in.local - p.rect.xy) / max(p.rect.zw, vec2<f32>(1e-4));

    // The atlas samples and the image sample, taken for every fragment so
    // their derivatives are defined.
    let texel = mix(p.uv.xy, p.uv.zw, clamp(along, vec2<f32>(0.0), vec2<f32>(1.0)));
    let glyph = textureSampleLevel(atlas, atlas_sampler, texel / size, 0.0);
    let texels_per_pixel = fwidth(mix(p.uv.xy, p.uv.zw, along));
    var image = vec4<f32>(1.0);
    var image_uv = mix(p.uv.xy, p.uv.zw, clamp(along, vec2<f32>(0.0), vec2<f32>(1.0)));
    let image_dx = dpdx(mix(p.uv.xy, p.uv.zw, along));
    let image_dy = dpdy(mix(p.uv.xy, p.uv.zw, along));
    if kind == KIND_IMAGE && p.info.y != NO_TEXTURE {
        image = textureSampleGrad(scene_textures[p.info.y], scene_sampler, image_uv, image_dx, image_dy);
        // The table's colour textures sample as linear light.
        image = vec4<f32>(to_srgb(image.rgb), image.a);
    }

    var colour = vec4<f32>(0.0);
    let box_distance = rounded_box(in.local, p.rect, p.radii);
    if kind == KIND_BOX {
        let outer = coverage(box_distance, pixel);
        let border = p.border_softness.x;
        let fill = fill_colour(p, in.local);
        if border > 0.0 {
            let inner = coverage(box_distance + border, pixel);
            // Between dashes the fill shows to the edge.
            let edge = mix(fill, premultiplied(p.colors.w), dash_on(p, in.local));
            colour = fill * inner + edge * max(outer - inner, 0.0);
        } else {
            colour = fill * outer;
        }
    } else if kind == KIND_SHADOW {
        let sigma = p.border_softness.y * 0.5;
        var amount = coverage(box_distance, pixel);
        if sigma > pixel * 0.35 {
            amount = shadow_coverage(in.local, p.rect, p.radii, sigma);
        }
        colour = premultiplied(p.colors.x) * amount;
    } else if kind == KIND_IMAGE {
        let tint = unpack4x8unorm(p.colors.x);
        let alpha = image.a * tint.a * coverage(box_distance, pixel);
        colour = vec4<f32>(image.rgb * tint.rgb * alpha, alpha);
    } else if kind == KIND_GLYPH {
        colour = premultiplied(p.colors.x) * glyph.a;
    } else if kind == KIND_FIELD_GLYPH {
        // The field's range in screen pixels.
        let range = max(2.0 * FIELD_SPREAD / max(max(texels_per_pixel.x, texels_per_pixel.y), 1e-5), 1.0);
        let inside = clamp((median3(glyph.rgb) - 0.5) * range + 0.5, 0.0, 1.0);
        colour = premultiplied(p.colors.x) * inside;
        if p.border_softness.x > 0.0 {
            let width = min(p.border_softness.x / max(p.params.w, 1e-5), FIELD_SPREAD - 0.5);
            let grown = clamp((glyph.a - 0.5 + width / (2.0 * FIELD_SPREAD)) * range + 0.5, 0.0, 1.0);
            colour += premultiplied(p.colors.w) * max(grown - inside, 0.0);
        }
    } else if kind == KIND_STROKE {
        let a = p.uv.xy;
        let ab = p.uv.zw - a;
        let t = clamp(dot(in.local - a, ab) / max(dot(ab, ab), 1e-5), 0.0, 1.0);
        let d = length(in.local - a - ab * t) - p.border_softness.x * 0.5;
        colour = premultiplied(p.colors.x) * coverage(d, pixel);
    }

    // The clip, with its own rounded corners.
    let clip_box = vec4<f32>(p.clip.xy, p.clip.zw - p.clip.xy);
    let clip_footprint = fwidth(in.laid);
    let clipped = coverage(rounded_box(in.laid, clip_box, p.clip_radii), max(clip_footprint.x, clip_footprint.y));
    colour = colour * clipped;

    if globals.linear_output != 0u && colour.a > 0.0 {
        colour = vec4<f32>(to_linear(colour.rgb / colour.a) * colour.a, colour.a);
    }
    return colour;
}
