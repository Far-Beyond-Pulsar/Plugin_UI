//! A temporary project with widgets, loaded through the world's asset system
//! as in a game.
#![allow(dead_code)] // Each test binary uses its own part.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use pulsar_scenedb::{Entity, World};
use pulsar_world_registry::pulsar_scene_model::Transform;
use serde_json::json;
use widget_asset::model::schema::{Kind, SlotKind, ValueClass};
use widget_asset::model::{Asset, NodeId, WidgetAsset};
use widget_component::paint::Image;
use widget_component::WidgetComponent;

pub fn project() -> &'static PathBuf {
    static PROJECT: OnceLock<tempfile::TempDir> = OnceLock::new();
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = PROJECT.get_or_init(|| tempfile::tempdir().expect("a temp dir"));
        std::fs::write(dir.path().join("Pulsar.toml"), "").expect("project file");
        engine_state::EngineContext::new().set_global();
        engine_state::set_project_path(dir.path().display().to_string());
        pulsar_scenedb::register_loader(
            widget_component::assets::WIDGET_ASSET_KIND,
            widget_component::assets::load,
        );
        dir.path().to_path_buf()
    })
}

pub fn write(name: &str, widget: &WidgetAsset) -> String {
    let path = project().join(name);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(path, widget.to_text()).expect("write");
    name.to_string()
}

/// A canvas child at `offsets` (x, y, width, height).
pub fn add(
    widget: &mut WidgetAsset,
    parent: NodeId,
    kind: Kind,
    name: &str,
    offsets: [f32; 4],
) -> NodeId {
    let mut node = widget.new_node(kind);
    node.name = name.into();
    let id = node.id;
    widget.add(parent, None, node).expect("added");
    widget
        .node_mut(id)
        .expect("node")
        .set_slot(SlotKind::Canvas, "offsets", json!(offsets));
    id
}

pub fn set(widget: &mut WidgetAsset, node: NodeId, prop: &str, value: serde_json::Value) {
    widget.node_mut(node).expect("node").set_prop(prop, value);
}

fn parameter(
    widget: &mut WidgetAsset,
    name: &str,
    class: ValueClass,
    node: NodeId,
    prop: &str,
    default: serde_json::Value,
) {
    let id = widget.add_parameter(name, class);
    widget.link_parameter(id, node, prop).expect("linked");
    widget.adopt_value(id, default);
}

/// A HUD at 1280 by 720: a panel with a title, a score (parameter `score`,
/// text, in a fixed slot), a bar (`health`, number; `bar`, colour) and a
/// button with the action `pause`.
pub fn hud() -> WidgetAsset {
    let mut w = WidgetAsset::new();
    w.design_size = widget_asset::model::Vec2::new(1280.0, 720.0);
    let root = w.root.id;
    let panel = add(&mut w, root, Kind::Border, "Panel", [20.0, 20.0, 300.0, 120.0]);
    set(&mut w, panel, "background", json!("#102030cc"));
    set(&mut w, panel, "corner_radius", json!([12.0, 12.0, 12.0, 12.0]));
    set(&mut w, panel, "shadow_color", json!("#000000aa"));
    set(&mut w, panel, "shadow_blur", json!(16.0));
    set(&mut w, panel, "border_width", json!(2.0));
    let title = add(&mut w, root, Kind::Text, "Title", [36.0, 30.0, 260.0, 24.0]);
    set(&mut w, title, "text", json!("Coin Town"));
    set(&mut w, title, "weight", json!("bold"));
    let score = add(&mut w, root, Kind::Text, "Score", [36.0, 60.0, 260.0, 28.0]);
    parameter(&mut w, "score", ValueClass::Text, score, "text", json!("0"));
    let bar = add(&mut w, root, Kind::ProgressBar, "Bar", [36.0, 100.0, 260.0, 12.0]);
    parameter(&mut w, "health", ValueClass::Float, bar, "percent", json!(0.5));
    parameter(&mut w, "bar", ValueClass::Color, bar, "fill_color", json!("#44cc66"));
    let button = add(&mut w, root, Kind::Button, "PauseButton", [20.0, 160.0, 120.0, 40.0]);
    set(&mut w, button, "action", json!("pause"));
    let mut label = w.new_node(Kind::Text);
    label.set_prop("text", json!("Pause"));
    w.add(button, None, label).expect("label");
    w
}

/// An object at `position` with a widget showing `path`, loaded.
pub fn spawn(world: &mut World, path: &str, position: [f64; 3]) -> Entity {
    let entity = world.spawn();
    world.insert(
        entity,
        Transform {
            position,
            ..Default::default()
        },
    );
    world.insert(
        entity,
        WidgetComponent {
            enabled: true,
            widget_asset: path.into(),
            ..Default::default()
        },
    );
    load(world, entity);
    entity
}

/// Pump the world's assets until the widget on `entity` is bound.
pub fn load(world: &mut World, entity: Entity) {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        world.pump_assets();
        if world
            .get::<WidgetComponent>(entity)
            .is_some_and(|c| c.loaded().is_some())
        {
            return;
        }
        assert!(
            Instant::now() < until,
            "the widget did not load: {:?} {:?}",
            {
                let c = world.get::<WidgetComponent>(entity).expect("component");
                world.asset_state(widget_component::assets::widget_asset_id(&c.widget_asset))
            },
            widget_component::report::titles()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// No GPU: images have a size and no texture.
pub fn no_images(_: &widget_component::component::BoundImages, _: &str) -> Option<Image> {
    None
}
