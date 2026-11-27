//! The runtime widgets of a world, end to end without a GPU: loading,
//! parameters and what they dirty, scripts' errors, hit testing and clicks,
//! world widgets, and cooking.

mod common;

use std::sync::Arc;

use common::*;
use glam::{DVec3, Mat4, Vec3};
use pulsar_scenedb::{Entity, World};
use pulsar_world_registry::pulsar_scene_model::Visibility;
use serde_json::json;
use widget_asset::display::PreviewState;
use widget_asset::model::schema::{Kind, ValueClass};
use widget_asset::model::{Asset, Vec2, WidgetAsset};
use widget_component::events::ButtonPhase;
use widget_component::interaction::{self, Pointer};
use widget_component::runtime::{ensure_runtime, PointerCamera, WidgetRuntime};
use widget_component::{report, WidgetComponent};

const VIEW: [u32; 2] = [1280, 720];

fn frame(runtime: &WidgetRuntime, world: &World) -> Option<Arc<helio_ui_api::UiFrame>> {
    let mut scene = runtime.lock();
    scene.sync(world);
    scene.frame(VIEW, DVec3::ZERO, &no_images)
}

fn set_text(world: &mut World, entity: Entity, name: &str, value: &str) {
    WidgetComponent::set_parameter(world, entity, name, ValueClass::Text, json!(value))
        .expect("set");
}

#[test]
fn an_unchanged_widget_costs_nothing_and_a_change_costs_what_it_changes() {
    let mut world = World::new();
    let path = write("hud_dirty.widget", &hud());
    let widgets: Vec<Entity> = (0..20)
        .map(|_| spawn(&mut world, &path, [0.0; 3]))
        .collect();
    let runtime = ensure_runtime(&mut world).expect("widgets");
    let first = frame(&runtime, &world).expect("a frame");
    assert_eq!(first.screen.batches.len(), 20);
    let built = runtime.lock().stats;
    assert_eq!((built.resolves, built.layouts, built.paints), (20, 20, 20));

    // Nothing changed: the same frame, nothing read.
    let again = frame(&runtime, &world).expect("a frame");
    assert!(Arc::ptr_eq(&first, &again));
    assert_eq!(runtime.lock().stats, built);

    // The same value again writes nothing.
    set_text(&mut world, widgets[3], "score", "0");
    let same = frame(&runtime, &world).expect("a frame");
    assert!(Arc::ptr_eq(&first, &same));
    assert_eq!(runtime.lock().stats, built);

    // A colour: one widget repainted, nothing resolved or laid out.
    WidgetComponent::set_parameter(
        &mut world,
        widgets[3],
        "bar",
        ValueClass::Color,
        json!("#ff0000"),
    )
    .expect("set");
    let coloured = frame(&runtime, &world).expect("a frame");
    let stats = runtime.lock().stats;
    assert_eq!(
        (stats.resolves, stats.layouts, stats.paints),
        (20, 20, 21)
    );
    let changed = coloured
        .screen
        .batches
        .iter()
        .zip(&first.screen.batches)
        .filter(|(a, b)| !Arc::ptr_eq(&a.primitives, &b.primitives))
        .count();
    assert_eq!(changed, 1);

    // A number shown as text that now takes more room: that one widget is
    // laid out again.
    set_text(&mut world, widgets[3], "score", "1234567");
    frame(&runtime, &world).expect("a frame");
    let stats = runtime.lock().stats;
    assert_eq!(
        (stats.resolves, stats.layouts, stats.paints),
        (20, 21, 22)
    );

    // Hiding the object removes its widget and touches no other.
    world.insert(
        widgets[0],
        Visibility {
            visible: false,
            locked: false,
        },
    );
    let hidden = frame(&runtime, &world).expect("a frame");
    assert_eq!(hidden.screen.batches.len(), 19);
    assert_eq!(runtime.lock().stats.paints, 22);

    // Despawned widgets leave.
    for entity in widgets {
        world.despawn(entity);
    }
    assert!(frame(&runtime, &world).is_none());
}

/// A widget shows only once something enables it.
#[test]
fn a_new_widget_is_disabled_and_draws_nothing_until_enabled() {
    assert!(!WidgetComponent::default().enabled);
    let mut world = World::new();
    let path = write("hud_disabled.widget", &hud());
    let entity = spawn(&mut world, &path, [0.0; 3]);
    world.get_mut::<WidgetComponent>(entity).expect("component").enabled = false;
    let runtime = ensure_runtime(&mut world).expect("widgets");
    let drawn = |world: &World| {
        frame(&runtime, world).map_or(0, |f| f.screen.batches.len() + f.world.batches.len())
    };
    assert_eq!(drawn(&world), 0);
    world.get_mut::<WidgetComponent>(entity).expect("component").enabled = true;
    assert_eq!(drawn(&world), 1);
}

#[test]
fn getters_return_what_was_set_else_the_default_and_mistakes_are_reported_once() {
    let mut world = World::new();
    let path = write("hud_report.widget", &hud());
    let entity = spawn(&mut world, &path, [0.0; 3]);
    let read = |world: &World, name: &str, class: ValueClass| {
        world
            .get::<WidgetComponent>(entity)
            .expect("component")
            .parameter_value(name, class)
    };
    assert_eq!(read(&world, "score", ValueClass::Text), json!("0"));
    set_text(&mut world, entity, "score", "42");
    assert_eq!(read(&world, "score", ValueClass::Text), json!("42"));

    // A wrong name and a wrong type are each reported once, naming the widget
    // and the parameter, and change nothing.
    for _ in 0..3 {
        set_text(&mut world, entity, "scoer", "1");
        set_text(&mut world, entity, "health", "full");
    }
    let titles = report::titles();
    let about = |needle: &str| {
        titles
            .iter()
            .filter(|t| t.contains("hud_report.widget") && t.contains(needle))
            .count()
    };
    assert_eq!(about("`scoer`"), 1, "{titles:?}");
    assert_eq!(about("`health`"), 1, "{titles:?}");
    let parameters = world
        .get::<WidgetComponent>(entity)
        .expect("component")
        .parameters
        .clone();
    assert_eq!(parameters.len(), 1);
}

#[test]
fn a_button_is_hovered_pressed_and_clicked_and_claims_its_press() {
    let mut world = World::new();
    let path = write("hud_click.widget", &hud());
    let entity = spawn(&mut world, &path, [0.0; 3]);
    let runtime = ensure_runtime(&mut world).expect("widgets");
    // At twice the design height, pointer pixels are twice the design units.
    let size = [2560u32, 1440u32];
    {
        let mut scene = runtime.lock();
        scene.sync(&world);
        scene.frame(size, DVec3::ZERO, &no_images);
        scene.pointer_view = Some((
            size,
            PointerCamera {
                view_proj: Mat4::IDENTITY,
                view: Mat4::IDENTITY,
                origin: DVec3::ZERO,
            },
        ));
    }
    let on_button = Vec2::new(80.0 * 2.0, 180.0 * 2.0);
    let on_panel = Vec2::new(40.0 * 2.0, 40.0 * 2.0);
    let outside = Vec2::new(900.0 * 2.0, 600.0 * 2.0);
    assert!(interaction::claims(&world, f64::from(on_button.x), f64::from(on_button.y)));
    assert!(!interaction::claims(&world, f64::from(on_panel.x), f64::from(on_panel.y)));
    assert!(!interaction::claims(&world, f64::from(outside.x), f64::from(outside.y)));

    let mut scene = runtime.lock();
    let hit = interaction::hit(&scene, on_button).expect("the button");
    assert_eq!((hit.name.as_str(), hit.action.as_str()), ("PauseButton", "pause"));
    interaction::handle(&mut scene, Pointer::Moved(on_button));
    assert_eq!(
        interaction::look(&scene, entity, &hit.node),
        PreviewState::Hovered
    );
    interaction::handle(&mut scene, Pointer::Down(on_button));
    assert_eq!(
        interaction::look(&scene, entity, &hit.node),
        PreviewState::Pressed
    );
    let phases = |raised: &[interaction::Raised]| -> Vec<ButtonPhase> {
        raised.iter().map(|r| r.phase).collect()
    };
    // Released elsewhere: no click.
    let raised = interaction::handle(&mut scene, Pointer::Up(outside));
    assert_eq!(phases(&raised), [ButtonPhase::Released]);
    let raised = interaction::handle(&mut scene, Pointer::Moved(outside));
    assert_eq!(phases(&raised), [ButtonPhase::Unhovered]);
    let raised = interaction::handle(&mut scene, Pointer::Down(on_button));
    assert_eq!(phases(&raised), [ButtonPhase::Hovered, ButtonPhase::Pressed]);
    let mut raised = interaction::handle(&mut scene, Pointer::Up(on_button));
    assert_eq!(phases(&raised), [ButtonPhase::Released, ButtonPhase::Clicked]);
    let click = raised.pop().expect("a click");
    assert_eq!(click.widget, entity);
    let action = click.action.expect("the button names an action");
    assert_eq!(
        (action.action.as_str(), action.node.as_str()),
        ("pause", "PauseButton")
    );
}

#[test]
fn clicks_arrive_as_the_component_event() {
    use pulsar_events::gamma::{Channel, SubscribeOptions};
    let mut world = World::new();
    let path = write("hud_event.widget", &hud());
    let entity = spawn(&mut world, &path, [0.0; 3]);
    let runtime = ensure_runtime(&mut world).expect("widgets");
    {
        let mut scene = runtime.lock();
        scene.sync(&world);
        scene.frame(VIEW, DVec3::ZERO, &no_images);
        scene.pointer_view = Some((
            VIEW,
            PointerCamera {
                view_proj: Mat4::IDENTITY,
                view: Mat4::IDENTITY,
                origin: DVec3::ZERO,
            },
        ));
    }
    let hub = pulsar_events::EventHub::new();
    pulsar_world_registry::register_component_events(&hub).expect("events");
    // The button's own event, as a session declares it.
    let (widget_id, button) = {
        let scene = runtime.lock();
        let (_, loaded) = scene.instances[&entity].widget.as_ref().expect("loaded");
        let buttons = widget_component::events::buttons(&loaded.asset, &loaded.library);
        (loaded.asset.id.clone(), buttons[0].key.clone())
    };
    let own = hub
        .register(
            widget_component::events::descriptor(&widget_id, &button, ButtonPhase::Clicked),
            pulsar_events::EventCategory::Custom("Widgets".into()),
        )
        .expect("declared");
    let own_heard = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = own_heard.clone();
    let _own_subscription = hub.bus().subscribe_dyn(
        own,
        SubscribeOptions {
            channel: Channel::Entity(entity.bits()),
            ..Default::default()
        },
        Box::new(move |_: &pulsar_events::gamma::DynEvent| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }),
    );
    let id = hub
        .descriptor_by_name("WidgetComponent.on_action")
        .expect("declared")
        .id;
    let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = heard.clone();
    let _subscription = hub.bus().subscribe_dyn(
        id,
        SubscribeOptions {
            channel: Channel::Entity(entity.bits()),
            ..Default::default()
        },
        Box::new(move |event: &pulsar_events::gamma::DynEvent| {
            sink.lock().expect("lock").push(event.fields.len())
        }),
    );
    let mut state = pulsar_world_registry::ComponentRuntimeState::default();
    let mut tick = |world: &mut World| {
        interaction::system(&mut pulsar_world_registry::SystemContext {
            world,
            events: &hub,
            delta_seconds: 0.016,
            runtime: &mut state,
        });
        pulsar_world_registry::flush_component_events(world, &hub);
        hub.flush(pulsar_events::FlushPoint::AfterSystems);
    };
    tick(&mut world);
    let at = (80.0, 180.0);
    hub.publish(
        Channel::Global,
        pulsar_events::builtin::PointerClaimedDown {
            button: 0,
            x: at.0,
            y: at.1,
        },
    );
    hub.publish(
        Channel::Global,
        pulsar_events::builtin::PointerClaimedUp {
            button: 0,
            x: at.0,
            y: at.1,
        },
    );
    hub.flush(pulsar_events::FlushPoint::AfterInput);
    tick(&mut world);
    tick(&mut world);
    assert_eq!(heard.lock().expect("lock").len(), 1);
    assert_eq!(own_heard.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_world_widget_stands_at_its_object_and_is_clicked_through_the_camera() {
    let mut world = World::new();
    let mut sign = WidgetAsset::new();
    sign.design_size = Vec2::new(200.0, 100.0);
    let root = sign.root.id;
    let button = add(&mut sign, root, Kind::Button, "Go", [0.0, 0.0, 200.0, 100.0]);
    set(&mut sign, button, "action", json!("go"));
    let path = write("sign.widget", &sign);
    let entity = world.spawn();
    world.insert(
        entity,
        pulsar_world_registry::pulsar_scene_model::Transform {
            position: [1_000_000.0, 0.0, -5.0],
            ..Default::default()
        },
    );
    world.insert(
        entity,
        WidgetComponent {
            enabled: true,
            widget_asset: path.as_str().into(),
            world_space: true,
            face_camera: false,
            world_width: 2.0,
            ..Default::default()
        },
    );
    load(&mut world, entity);
    let runtime = ensure_runtime(&mut world).expect("widgets");
    // The frame's origin is at the camera, a million metres from the
    // world's: the placement is exact all the same.
    let origin = DVec3::new(1_000_000.0, 0.0, 0.0);
    let mut scene = runtime.lock();
    scene.sync(&world);
    let first = scene.frame(VIEW, origin, &no_images).expect("a frame");
    assert!(first.screen.batches.is_empty());
    assert_eq!(first.world.batches.len(), 1);
    let row = first.placements.rows[first.world.batches[0].placement as usize];
    assert_eq!(row.centre[..3], [0.0, 0.0, -5.0]);
    assert_eq!(row.down[3], 0.01, "2 m over 200 units");

    let view = glam::camera::rh::view::look_to_mat4(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y);
    let proj = helio_core::depth::perspective(1.0, 16.0 / 9.0, 0.1);
    scene.pointer_view = Some((
        VIEW,
        PointerCamera {
            view_proj: proj * view,
            view,
            origin,
        },
    ));
    assert!(interaction::hit(&scene, Vec2::new(640.0, 360.0)).is_some());
    assert!(interaction::hit(&scene, Vec2::new(20.0, 20.0)).is_none());
    drop(scene);

    // Moving the object moves the placement and rebuilds no primitive.
    let paints = runtime.lock().stats.paints;
    world
        .get_mut::<pulsar_world_registry::pulsar_scene_model::Transform>(entity)
        .expect("transform")
        .position[0] += 1.5;
    let mut scene = runtime.lock();
    scene.sync(&world);
    let moved = scene.frame(VIEW, origin, &no_images).expect("a frame");
    assert!(Arc::ptr_eq(&moved.world, &first.world));
    assert_eq!(scene.stats.paints, paints);
    let row = moved.placements.rows[moved.world.batches[0].placement as usize];
    assert_eq!(row.centre[0], 1.5);
}

#[test]
fn small_screen_text_is_a_pixel_raster_and_world_text_a_distance_field() {
    let mut world = World::new();
    let path = write("hud_glyphs.widget", &hud());
    spawn(&mut world, &path, [0.0; 3]);
    let runtime = ensure_runtime(&mut world).expect("widgets");
    let screen = frame(&runtime, &world).expect("a frame");
    let kinds: Vec<u32> = screen.screen.batches[0]
        .primitives
        .iter()
        .map(|p| p.info[0] & helio_ui_api::KIND_MASK)
        .collect();
    assert!(kinds.contains(&helio_ui_api::KIND_GLYPH));
    assert!(!kinds.contains(&helio_ui_api::KIND_FIELD_GLYPH));
    assert!(kinds.contains(&helio_ui_api::KIND_SHADOW));
    assert!(kinds.contains(&helio_ui_api::KIND_BOX));
    // Boxes sit on whole pixels.
    for p in screen.screen.batches[0]
        .primitives
        .iter()
        .filter(|p| p.info[0] & helio_ui_api::KIND_MASK == helio_ui_api::KIND_BOX)
    {
        assert!(p.rect.iter().all(|v| v.fract() == 0.0), "{:?}", p.rect);
    }
}

#[test]
fn a_widget_that_instances_another_loads_it_and_cooks_with_its_files() {
    let mut inner = WidgetAsset::new();
    let root = inner.root.id;
    let image = add(&mut inner, root, Kind::Image, "Icon", [0.0, 0.0, 32.0, 32.0]);
    set(&mut inner, image, "source", json!("ui/coin.png"));
    write("ui/icon.widget", &inner);
    let mut outer = WidgetAsset::new();
    let instance = outer.new_instance(inner.id.clone(), "Icon");
    let root = outer.root.id;
    outer.add(root, None, instance).expect("instance");
    let name = write("ui/outer.widget", &outer);
    std::fs::write(project().join("ui/coin.png"), b"not really").expect("image");

    let loaded = widget_component::assets::load_from(project(), &name).expect("loads");
    assert_eq!(loaded.library.len(), 1);
    assert_eq!(loaded.images, vec!["ui/coin.png".to_string()]);

    let source = project().join(&name);
    let cooked = widget_component::assets::cook(&plugin_editor_api::CookContext {
        project_root: project(),
        source_path: &source,
        content_path: std::path::Path::new("assets/ui/outer.widget"),
        optimization: Default::default(),
    })
    .expect("cooks");
    assert_eq!(cooked.dependencies, vec!["ui/icon.widget".to_string()]);
    let text = String::from_utf8(cooked.files[0].bytes.clone()).expect("utf-8");
    assert!(text.contains("cooked_dependencies"), "{text}");

    // A packaged game: only the shipped files, where the packer puts them
    // (under `assets/`), and no library to scan. Every reference the cooked
    // widget holds must name a file that is there as written.
    let shipped = project().join("shipped");
    std::fs::create_dir_all(shipped.join("assets/ui")).expect("dirs");
    std::fs::write(shipped.join("assets/ui/outer.widget"), &text).expect("write");
    std::fs::copy(
        project().join("ui/icon.widget"),
        shipped.join("assets/ui/icon.widget"),
    )
    .expect("copy");
    let packaged =
        widget_component::assets::load_from(&shipped, "assets/ui/outer.widget").expect("loads");
    assert_eq!(packaged.library.len(), 1);
    assert!(WidgetAsset::parse(&text).is_ok());

    // The widget that holds the image names it where it ships.
    let source = project().join("ui/icon.widget");
    let cooked = widget_component::assets::cook(&plugin_editor_api::CookContext {
        project_root: project(),
        source_path: &source,
        content_path: std::path::Path::new("assets/ui/icon.widget"),
        optimization: Default::default(),
    })
    .expect("cooks");
    assert_eq!(cooked.dependencies, vec!["ui/coin.png".to_string()]);
    let text = String::from_utf8(cooked.files[0].bytes.clone()).expect("utf-8");
    assert!(text.contains("\"assets/ui/coin.png\""), "{text}");
}
