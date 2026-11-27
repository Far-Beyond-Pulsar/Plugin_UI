// The screen layer: primitives in output pixels.

@vertex
fn vs_main(@builtin(vertex_index) vertex: u32, @builtin(instance_index) instance: u32) -> UiVarying {
    let p = primitives[instance];
    var out: UiVarying;
    out.instance = instance;
    if (p.info.x & 255u) == 0u {
        out.position = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        return out;
    }
    let reach = quad_reach(p, 1.0);
    let local = p.rect.xy - vec2<f32>(reach) + quad_corner(vertex) * (p.rect.zw + vec2<f32>(reach * 2.0));
    let laid = transformed(p, local);
    out.local = local;
    out.laid = laid;
    out.position = vec4<f32>(
        laid.x / globals.output.x * 2.0 - 1.0,
        1.0 - laid.y / globals.output.y * 2.0,
        0.0,
        1.0,
    );
    return out;
}

@fragment
fn fs_main(in: UiVarying) -> @location(0) vec4<f32> {
    return shade(in);
}
