//! What the UI plugin registers for a game (and the editor), grouped by the
//! engine seam it plugs into.

use engine_backend::scene::game_hooks::{GameSessionRegistration, PointerClaimRegistration};
use engine_backend::scene::{ClassSpec, ComponentsBuilder};
use engine_backend::subsystems::render::helio_renderer::RenderHostPartRegistration;
use engine_backend::subsystems::render::RenderFeatureRegistration;
use helio_component::asset_component::{AssetLoaderRegistration, AssetReloadRegistration};
use pulsar_world_registry::{systems::COMPONENTS, System};
use widget_component::WidgetComponent;

/// The `.widget` files' kind for hot reload.
const WIDGET_FILES: &str = "pulsar.ui.widget";

/// Takes the pointer events and raises the clicks as `on_action`.
const INTERACTION: System = System::new(
    "pulsar.ui.interaction",
    widget_component::interaction::system,
)
.after(&[COMPONENTS]);

fn is_widget_file(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(widget_asset::library::WIDGET_EXTENSION))
}

pub(crate) fn build(p: &mut pulsar_plugins::PluginBuilder) {
    // ── Component classes ───────────────────────────────────────────────────

    p.components().class::<WidgetComponent>(ClassSpec {
        systems: &[INTERACTION],
        ..ClassSpec::default()
    });

    // What a new component's texture choice starts as: the project's.
    p.add(pulsar_settings::project::ProjectSettingsRegistration {
        origin: p.origin(),
        name: "ui.textures_settings",
        register: widget_component::register_texture_settings,
    });
    p.add(helio_component::asset_component::NewComponentRegistration {
        origin: p.origin(),
        class_name: widget_component::WIDGET_CLASS_NAME,
        seed: widget_component::seed_new,
    });

    // ── Rendering ───────────────────────────────────────────────────────────

    // The screen and world widget passes (display slot, output resolution).
    p.add(RenderFeatureRegistration {
        origin: p.origin(),
        features: widget_component::render_features,
        configure: |config| config,
    });
    // Each renderer lays the widgets out for its output and publishes the
    // frame the passes draw.
    p.add(RenderHostPartRegistration {
        origin: p.origin(),
        name: "ui.widgets",
        new: || Box::new(widget_component::host::UiHostPart::default()),
    });

    // ── Input ───────────────────────────────────────────────────────────────

    // A press on an on-screen button is the widgets', not gameplay's.
    p.add(PointerClaimRegistration {
        origin: p.origin(),
        name: "ui.buttons",
        claims: widget_component::interaction::claims,
    });

    // ── Script events ───────────────────────────────────────────────────────

    // Each button of the widget a component names has its own events: the
    // editor offers them from the asset, and a session declares them before
    // scripts link.
    p.add(crate::event_source::slot(p.origin()));
    p.add(crate::event_source::source(p.origin()));
    p.add(GameSessionRegistration {
        origin: p.origin(),
        name: "ui.button_events",
        begin: Some(|hub, root| {
            widget_component::events::declare_project_events(hub, root);
        }),
        possess: None,
        end: None,
    });

    // ── Assets ──────────────────────────────────────────────────────────────

    p.add(AssetLoaderRegistration {
        origin: p.origin(),
        kind: widget_component::assets::WIDGET_ASSET_KIND,
        load: widget_component::assets::load,
        size: None,
        compose: None,
    });
    // A saved widget reloads in every scene showing it.
    p.add(engine_fs::watchers::AssetChangeKind {
        origin: p.origin(),
        id: "ui.widget_files",
        kind_of: |path| {
            is_widget_file(path).then(|| ui_types_common::AssetKind::Custom(WIDGET_FILES.into()))
        },
    });
    p.add(AssetReloadRegistration {
        origin: p.origin(),
        name: "ui.widgets.reload",
        kind: ui_types_common::AssetKind::Custom(String::from(WIDGET_FILES)),
        sources_for_file: widget_component::assets::sources_for_file,
    });
}

#[cfg(test)]
mod tests {
    use engine_backend::subsystems::render::helio_renderer::render_features;

    #[test]
    fn registers_the_widget_passes() {
        let features = render_features::plugin_features();
        assert!(
            features.ids().contains(&"helio.ui"),
            "features: {:?}",
            features.ids()
        );
    }

    #[test]
    fn registers_under_its_class_name() {
        assert_eq!(
            pulsar_world_registry::component_id_for_class("WidgetComponent"),
            Some(pulsar_scenedb::component_id::<
                widget_component::WidgetComponent,
            >())
        );
    }

    #[test]
    fn every_registration_resolves_to_the_plugin() {
        let ours: Vec<_> = pulsar_plugins::contributions()
            .into_iter()
            .filter(|summary| summary.origin == pulsar_plugins::origin!())
            .collect();
        assert!(ours.iter().any(|s| s.category == "component.class"));
        for summary in &ours {
            assert_eq!(
                pulsar_plugins::plugin_of(summary.origin).map(|plugin| plugin.id.as_str()),
                Some("pulsar.ui"),
                "{summary:?}"
            );
        }
    }
}
