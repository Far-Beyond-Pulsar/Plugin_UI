// World widgets: the same primitives, laid out in widget units, placed in the
// frame's camera-relative space. Drawn over the tone-mapped image at output
// resolution without the temporal jitter, and depth-tested here against the
// scene's depth, which is at the internal resolution and jittered.

@group(2) @binding(0) var<storage, read> cameras: array<Camera, 2>;
@group(2) @binding(1) var<storage, read> placements: array<UiPlacement>;
@group(2) @binding(2) var scene_depth: texture_depth_2d;

const PLACE_BILLBOARD: u32 = 1u;
const PLACE_CONSTANT_SIZE: u32 = 2u;
// How much nearer the scene must be, relative to the widget's distance, to
// hide it: a widget lying on a surface stays visible.
const DEPTH_TOLERANCE: f32 = 1.002;

@vertex
fn vs_main(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> UiVarying {
    let p = primitives[instance];
    var out: UiVarying;
    out.instance = instance;
    let camera = cameras[0];
    let place = placements[p.info.z];
    if (p.info.x & 255u) == 0u || !helio_camera_sees_space(camera, u32(place.centre.w)) {
        out.position = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        return out;
    }
    // A margin in widget units that covers the anti-aliased edge up close.
    let reach = quad_reach(p, 2.0);
    let local = p.rect.xy - vec2<f32>(reach) + quad_corner(vertex) * (p.rect.zw + vec2<f32>(reach * 2.0));
    let laid = transformed(p, local);

    let flags = u32(place.right.w);
    var right = place.right.xyz;
    var down = place.down.xyz;
    if (flags & PLACE_BILLBOARD) != 0u {
        // The view matrix's rows are the camera's axes.
        right = vec3<f32>(camera.view[0][0], camera.view[1][0], camera.view[2][0]);
        down = -vec3<f32>(camera.view[0][1], camera.view[1][1], camera.view[2][1]);
    }
    var scale = place.down.w;
    if (flags & PLACE_CONSTANT_SIZE) != 0u {
        let depth = -(camera.view * vec4<f32>(place.centre.xyz, 1.0)).z;
        scale = scale * max(depth, 0.0);
    }
    let offset = (laid - place.size.xy * 0.5) * scale;
    let position = place.centre.xyz + right * offset.x + down * offset.y;

    var clip = camera.view_proj * vec4<f32>(position, 1.0);
    // The image under it is resolved: no jitter.
    clip = vec4<f32>(clip.xy - camera.jitter_frame.xy * clip.w, clip.z, clip.w);
    out.position = clip;
    out.local = local;
    out.laid = laid;
    return out;
}

// How much of this fragment is in front of the scene, over the four depth
// texels around it (a soft edge where geometry cuts the widget).
fn visible_fraction(fragment: vec4<f32>) -> f32 {
    let camera = cameras[0];
    let uv = fragment.xy / globals.output + camera.jitter_frame.xy * vec2<f32>(0.5, -0.5);
    let size = vec2<i32>(textureDimensions(scene_depth));
    let at = uv * vec2<f32>(size) - 0.5;
    let base = vec2<i32>(floor(at));
    let f = at - floor(at);
    var seen = array<f32, 4>();
    for (var i = 0; i < 4; i += 1) {
        let coord = clamp(base + vec2<i32>(i & 1, i >> 1), vec2<i32>(0), size - vec2<i32>(1));
        let depth = textureLoad(scene_depth, coord, 0);
        // Reversed depth: larger is nearer.
        seen[i] = select(0.0, 1.0, depth <= fragment.z * DEPTH_TOLERANCE);
    }
    return mix(mix(seen[0], seen[1], f.x), mix(seen[2], seen[3], f.x), f.y);
}

@fragment
fn fs_main(in: UiVarying) -> @location(0) vec4<f32> {
    let colour = shade(in);
    return colour * visible_fraction(in.position);
}
