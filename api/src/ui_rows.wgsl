// One UI primitive (`helio_ui_api::UiPrimitive`) and a world widget's
// placement (`UiPlacement`).

struct UiPrimitive {
    rect: vec4<f32>,
    radii: vec4<f32>,
    uv: vec4<f32>,
    clip: vec4<f32>,
    clip_radii: vec4<f32>,
    transform: vec4<f32>,
    translation: vec2<f32>,
    border_softness: vec2<f32>,
    colors: vec4<u32>,
    params: vec4<f32>,
    info: vec4<u32>,
}

struct UiPlacement {
    centre: vec4<f32>,
    right: vec4<f32>,
    down: vec4<f32>,
    size: vec4<f32>,
}
