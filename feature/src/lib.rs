//! The UI feature (`pulsar.ui`): the passes that draw widgets.
//!
//! Two nodes at the display slot, after the tone map and the sprites, before
//! the performance overlay: the world widgets (`helio.pass.ui_world`, order
//! 12), then the screen layer over them (`helio.pass.ui_screen`, order 15).
//! Both draw at the output resolution, so a lowered render scale never blurs
//! the UI. What they draw is the [`helio_ui_api::UI_FRAME`] payload.

pub mod heap;
pub mod pass;

pub use pass::{shader_source, SharedAtlas, UiLayerKind, UiPass};

use helio_feature_api::{pass_node, Feature, FeatureRegistrar, NodeBuild};
use helio_frame_api::{FrameNode, Slot};

/// The side of the atlas the passes allocate; the publisher's atlas must have
/// it.
pub const ATLAS_SIDE: u32 = 2048;

/// The widget passes.
pub struct UiFeature;

fn build_pass(build: &mut NodeBuild<'_, '_>, kind: UiLayerKind) -> Result<(), String> {
    let device = build.env.device.clone();
    let queue = build.env.queue.clone();
    let format = build.env.frame.surface_format;
    let atlas = build
        .env
        .shared(|| SharedAtlas::new(&device, ATLAS_SIDE))
        .clone();
    build
        .graph
        .add_pass(Box::new(UiPass::new(&device, &queue, kind, format, atlas)));
    Ok(())
}

impl Feature for UiFeature {
    fn id(&self) -> &'static str {
        "helio.ui"
    }

    fn register(&self, registrar: &mut FeatureRegistrar<'_>) {
        // The passes share the atlas texture: shader reload swaps them
        // together.
        registrar.swap_group(&[helio_core::graph::type_name_of::<UiPass>()]);
        registrar.node(
            pass_node(
                "ui_world",
                "Output: World Widgets",
                "Passes: Output",
                "Widgets placed in the world, at output resolution, depth-tested against the scene.",
                |build| build_pass(build, UiLayerKind::World),
            )
            .with_marker_after()
            .in_slot(Slot::Display, 12),
        );
        registrar.node(
            pass_node(
                "ui_screen",
                "Output: Screen Widgets",
                "Passes: Output",
                "The screen-space widget layer (HUD, menus), at output resolution.",
                |build| build_pass(build, UiLayerKind::Screen),
            )
            .with_marker_after()
            .in_slot(Slot::Display, 15),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(source: &str) {
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
    }

    /// Both shaders, as built for a device with texture binding arrays,
    /// are valid WGSL.
    #[test]
    fn the_shaders_are_valid() {
        let binding = helio_mats::MaterialBindingConfig {
            mode: helio_mats::MaterialBindingMode::BindingArray,
            max_textures: 256,
        };
        validate(&shader_source(UiLayerKind::Screen, binding));
        // The world shader's prelude is expanded by the shader loader.
        let world = shader_source(UiLayerKind::World, binding);
        let world = helio_core::shader::resolve(&world).into_owned();
        validate(&world);
    }
}
