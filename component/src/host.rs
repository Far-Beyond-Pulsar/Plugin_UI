//! Each scene renderer's part: before the frame renders, it brings the
//! world's widgets up to date for the renderer's output and hands the UI
//! passes the frame. While no widget changed this is one runtime lookup, a few
//! revision compares and an `Arc` clone.

use std::any::Any;
use std::sync::Arc;

use engine_backend::scene::SharedScene;
use engine_backend::subsystems::render::helio_renderer::RenderHostPart;
use helio_ui_api::{NO_TEXTURE, SCREEN_NODE, SCREEN_PASS_NAME, UI_FRAME, WORLD_PASS_NAME};
use pulsar_findings::{Category, Finding, Severity};
use pulsar_scenedb::gpu::GpuMirrorHandle;
use widget_asset::model::Vec2;

use crate::component::BoundImages;
use crate::paint::Image;
use crate::runtime::{ensure_runtime, runtime_of, PointerCamera};
use crate::WidgetComponent;

const PRESENCE_SOURCE: &str = "Widgets: render pipeline";

/// See the module doc.
#[derive(Default)]
pub struct UiHostPart {
    /// The pipeline whose passes were last checked, and what it lacked.
    checked: Option<(Option<String>, bool)>,
}

/// The slot (uploading on first use) and size of a widget's loaded image.
pub fn image_slot(
    images: &BoundImages,
    path: &str,
    mirror: Option<&GpuMirrorHandle>,
) -> Option<Image> {
    let bound = images.images.get(path)?;
    let loaded = bound.image_for(helio_component::texture_assets::texture_asset_id_of(path))?;
    let slot = mirror.map_or(NO_TEXTURE, |mirror| {
        helio_component::texture_assets::texture_slot(bound, path, mirror, "widget")
    });
    Some(Image {
        slot,
        size: Vec2::new(loaded.width as f32, loaded.height as f32),
    })
}

impl UiHostPart {
    /// Say so when widgets exist and the pipeline draws none.
    fn check_pipeline(&mut self, renderer: &helio::Renderer, screen: bool, world: bool) {
        if !renderer.pipeline_status().is_ready() || renderer.pipeline_building().is_some() {
            return;
        }
        let id = renderer.pipeline_id();
        let seen = self
            .checked
            .as_ref()
            .is_some_and(|(checked, _)| checked.as_deref() == id);
        if !seen {
            let members = renderer.graph_members();
            let lacking = !(members.contains(&SCREEN_PASS_NAME)
                && members.contains(&WORLD_PASS_NAME));
            self.checked = Some((id.map(str::to_string), lacking));
        }
        let lacking = self.checked.as_ref().is_some_and(|(_, lacking)| *lacking);
        let findings = if lacking && (screen || world) {
            let pipeline = id.unwrap_or("the active pipeline");
            vec![Finding::new(
                "ui-pass-missing",
                Category::Project,
                Severity::Warning,
                "The render pipeline does not draw widgets",
            )
            .detail(format!(
                "The level has widgets, but {pipeline} has no widget nodes, so they are not \
                 drawn. Add `Output: World Widgets` and `Output: Screen Widgets` ({SCREEN_NODE}) \
                 after the tone map, or use a shipped pipeline."
            ))
            .subject("Widgets")]
        } else {
            Vec::new()
        };
        // Publishing the same set again costs nothing.
        pulsar_findings::publish(PRESENCE_SOURCE, findings);
    }
}

impl RenderHostPart for UiHostPart {
    fn publish_frame(
        &mut self,
        scene: &SharedScene,
        renderer: &mut helio::Renderer,
        camera: Option<&helio::Camera>,
    ) {
        profiling::profile_scope!("Widgets::publish_frame");
        let found = {
            let store = scene.read();
            let found = runtime_of(&store.world);
            if found.is_none() && store.world.query::<&WidgetComponent>().next().is_none() {
                return;
            }
            found
        };
        let runtime = match found {
            Some(runtime) => runtime,
            None => match ensure_runtime(&mut scene.write().world) {
                Some(runtime) => runtime,
                None => return,
            },
        };
        let store = scene.read();
        let world = &store.world;
        let mirror = world.gpu_mirror();
        let size = [renderer.output_width(), renderer.output_height()];
        let origin = renderer.world_origin();
        let mut widgets = runtime.lock();
        widgets.sync(world);
        let frame = widgets.frame(size, origin, &|images, path| {
            image_slot(images, path, mirror)
        });
        if let Some(camera) = camera {
            let view = camera.view_in(origin);
            widgets.pointer_view = Some((
                size,
                PointerCamera {
                    view_proj: camera.proj * view,
                    view,
                    origin,
                },
            ));
        }
        drop(widgets);
        let (screen, in_world) = frame.as_ref().map_or((false, false), |f| {
            (!f.screen.batches.is_empty(), !f.world.batches.is_empty())
        });
        self.check_pipeline(renderer, screen, in_world);
        if let Some(frame) = frame {
            renderer.set_scene_payload::<helio_ui_api::UiFrame>(&UI_FRAME, Arc::clone(&frame));
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
