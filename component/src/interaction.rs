//! Pointer interaction: which button is under the pointer, how it looks, its
//! own events ([`crate::events`]) and `on_action` when one with an action
//! is clicked.
//!
//! Positions are the engine's input events', in physical pixels of the game's
//! output. A press on a button is **claimed** by the widgets
//! (`engine_backend::scene::game_hooks::PointerClaimRegistration`): the host
//! then publishes it as `PointerClaimedDown`/`Up` instead of the gameplay
//! mouse button events, and does not capture the cursor for it.

use std::sync::{Arc, Mutex};

use glam::{Vec3, Vec4};
use pulsar_events::gamma::{DynEvent, DynValue, Event, SubscribeOptions, SyncSubscription};
use pulsar_events::EventHub;
use pulsar_scenedb::{Entity, World};
use pulsar_world_registry::SystemContext;
use widget_asset::display::{self, PreviewState};
use widget_asset::model::schema::Kind;
use widget_asset::model::{Vec2, WidgetId};
use widget_asset::resolve::ResolvedNode;

use crate::component::{queue_action, WidgetAction};
use crate::events::{queue_button_event, ButtonPhase};
use crate::runtime::{ensure_runtime, runtime_of, NodeKey, PointerCamera, WidgetScene};

/// A button under the pointer.
#[derive(Clone, Debug, PartialEq)]
pub struct ButtonHit {
    pub widget: Entity,
    pub node: NodeKey,
    pub name: String,
    pub action: String,
    pub enabled: bool,
}

/// The innermost button on the path to the node `key`, if there is one.
fn button_over(tree: &ResolvedNode, key: &NodeKey) -> Option<(NodeKey, String, String, bool)> {
    let mut found = None;
    tree.walk(&mut |node| {
        if node.kind == Kind::Button && key.starts_with(&node.key) {
            let view = node.view();
            // A parent is walked before its children: the innermost wins.
            found = Some((
                node.key.clone(),
                node.name.clone(),
                view.text("action"),
                view.bool("enabled"),
            ));
        }
    });
    found
}

/// What is solid under `point` (the tree's layout units): the button it is
/// part of, if any. `None` when nothing of this widget is there.
fn hit_widget(
    widget: Entity,
    tree: &ResolvedNode,
    point: Vec2,
) -> Option<Option<ButtonHit>> {
    let hit = display::hit_test(tree, point, &|_| true)?;
    Some(
        button_over(tree, &hit.key).map(|(node, name, action, enabled)| ButtonHit {
            widget,
            node,
            name,
            action,
            enabled,
        }),
    )
}

/// The point of a world widget's plane under the pointer, in widget units,
/// and its distance along the ray.
fn plane_point(
    scene: &WidgetScene,
    entity: Entity,
    camera: &PointerCamera,
    output: [u32; 2],
    pointer: Vec2,
) -> Option<(f32, Vec2)> {
    let instance = scene.instances.get(&entity)?;
    let pose = instance.pose?;
    let (_, widget) = instance.widget.as_ref()?;
    let design = widget.asset.design_size;
    let row = crate::runtime::placement_row(&instance.placing, &pose, design, camera.origin);
    let centre = Vec3::new(row.centre[0], row.centre[1], row.centre[2]);
    let (mut right, mut down) = (
        Vec3::new(row.right[0], row.right[1], row.right[2]),
        Vec3::new(row.down[0], row.down[1], row.down[2]),
    );
    if instance.placing.face_camera {
        right = camera.view.row(0).truncate();
        down = -camera.view.row(1).truncate();
    }
    let mut per_unit = row.down[3];
    if instance.placing.constant_screen_size {
        per_unit *= (-camera.view.transform_point3(centre).z).max(0.0);
    }
    if per_unit <= 0.0 {
        return None;
    }
    let ndc_x = pointer.x / output[0].max(1) as f32 * 2.0 - 1.0;
    let ndc_y = 1.0 - pointer.y / output[1].max(1) as f32 * 2.0;
    // Reversed depth: 1 is the near plane; a smaller depth is farther.
    let inverse = camera.view_proj.inverse();
    let unproject = |z: f32| {
        let p = inverse * Vec4::new(ndc_x, ndc_y, z, 1.0);
        p.truncate() / p.w
    };
    let near = unproject(1.0);
    let direction = (unproject(0.5) - near).normalize_or_zero();
    let normal = right.cross(down);
    let along = direction.dot(normal);
    if along.abs() < 1e-6 {
        return None;
    }
    let t = (centre - near).dot(normal) / along;
    if t <= 0.0 {
        return None;
    }
    let offset = near + direction * t - centre;
    let local = Vec2::new(
        offset.dot(right) / per_unit + design.x * 0.5,
        offset.dot(down) / per_unit + design.y * 0.5,
    );
    Some((t, local))
}

/// The button under `pointer` (output pixels) in the view last shown: screen
/// widgets from the top, then the nearest world widget. A widget's solid parts
/// hide what is under them.
pub fn hit(scene: &WidgetScene, pointer: Vec2) -> Option<ButtonHit> {
    let (output, camera) = scene.pointer_view?;
    if let Some(view) = scene.screen_views.get(&output) {
        for batch in view.layer.batches.iter().rev() {
            let Some((entity, _)) = scene.instances.iter().find(|(_, i)| i.key == batch.key) else {
                continue;
            };
            let Some(item) = view.items.get(entity) else {
                continue;
            };
            let m = item.mapping;
            let local = Vec2::new(
                pointer.x / m.scale - m.origin.x,
                pointer.y / m.scale - m.origin.y,
            );
            if let Some(found) = hit_widget(*entity, &item.tree, local) {
                return found;
            }
        }
    }
    let mut nearest: Option<(f32, Option<ButtonHit>)> = None;
    for (entity, item) in &scene.world_view.items {
        if !scene.instances.get(entity).is_some_and(|i| i.is_shown()) {
            continue;
        }
        let Some((t, local)) = plane_point(scene, *entity, &camera, output, pointer) else {
            continue;
        };
        if nearest.as_ref().is_some_and(|(best, _)| *best <= t) {
            continue;
        }
        if let Some(found) = hit_widget(*entity, &item.tree, local) {
            nearest = Some((t, found));
        }
    }
    nearest.and_then(|(_, found)| found)
}

/// Whether a press at `(x, y)` is the widgets'.
pub fn claims(world: &World, x: f64, y: f64) -> bool {
    runtime_of(world).is_some_and(|runtime| {
        hit(&runtime.lock(), Vec2::new(x as f32, y as f32)).is_some()
    })
}

/// A pointer event, in output pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pointer {
    Moved(Vec2),
    Down(Vec2),
    Up(Vec2),
}

fn button_of(key: &Option<ButtonHit>) -> Option<(Entity, NodeKey)> {
    key.as_ref().map(|h| (h.widget, h.node.clone()))
}

fn set_look(scene: &mut WidgetScene, hovered: bool, to: Option<(Entity, NodeKey)>) {
    let mut touched = Vec::new();
    for (entity, instance) in &mut scene.instances {
        let wanted = to
            .as_ref()
            .filter(|(e, _)| e == entity)
            .map(|(_, node)| node.clone());
        let slot = if hovered {
            &mut instance.hovered
        } else {
            &mut instance.pressed
        };
        if *slot != wanted {
            *slot = wanted;
            touched.push(*entity);
        }
    }
    for entity in touched {
        scene.touch(entity);
    }
}

/// Something that happened to a button.
#[derive(Clone, Debug, PartialEq)]
pub struct Raised {
    /// The widget component's instance.
    pub widget: Entity,
    pub node: NodeKey,
    pub phase: ButtonPhase,
    /// For a click on a button that names an action: `on_action`'s value.
    pub action: Option<WidgetAction>,
}

impl Raised {
    fn new(widget: Entity, node: NodeKey, phase: ButtonPhase) -> Self {
        Self {
            widget,
            node,
            phase,
            action: None,
        }
    }
}

/// The button the pointer is over, or the one held down.
fn marked(scene: &WidgetScene, hovered: bool) -> Option<(Entity, NodeKey)> {
    scene.instances.iter().find_map(|(entity, instance)| {
        let slot = if hovered {
            &instance.hovered
        } else {
            &instance.pressed
        };
        slot.clone().map(|node| (*entity, node))
    })
}

/// Move the hover look to `to`, raising what that leaves and enters.
fn hover(scene: &mut WidgetScene, to: Option<(Entity, NodeKey)>, raised: &mut Vec<Raised>) {
    let from = marked(scene, true);
    if from == to {
        return;
    }
    if let Some((widget, node)) = from {
        raised.push(Raised::new(widget, node, ButtonPhase::Unhovered));
    }
    if let Some((widget, node)) = to.clone() {
        raised.push(Raised::new(widget, node, ButtonPhase::Hovered));
    }
    set_look(scene, true, to);
}

/// Take a pointer event: update the buttons' looks; returns what happened to
/// them, in order.
pub fn handle(scene: &mut WidgetScene, event: Pointer) -> Vec<Raised> {
    let mut raised = Vec::new();
    match event {
        Pointer::Moved(at) => {
            let under = hit(scene, at).filter(|h| h.enabled);
            hover(scene, button_of(&under), &mut raised);
        }
        Pointer::Down(at) => {
            let under = hit(scene, at).filter(|h| h.enabled);
            hover(scene, button_of(&under), &mut raised);
            if let Some((widget, node)) = button_of(&under) {
                raised.push(Raised::new(widget, node, ButtonPhase::Pressed));
            }
            set_look(scene, false, button_of(&under));
        }
        Pointer::Up(at) => {
            let under = hit(scene, at).filter(|h| h.enabled);
            let held = marked(scene, false);
            set_look(scene, false, None);
            let Some((widget, node)) = held else {
                return raised;
            };
            raised.push(Raised::new(widget, node.clone(), ButtonPhase::Released));
            let over = under.filter(|h| h.widget == widget && h.node == node);
            if let Some(hit) = over {
                let mut click = Raised::new(widget, node, ButtonPhase::Clicked);
                click.action = (!hit.action.is_empty()).then(|| WidgetAction {
                    action: hit.action,
                    node: hit.name,
                });
                raised.push(click);
            }
        }
    }
    raised
}

/// Queue the component events of what happened.
fn publish(world: &mut World, scene_ids: &[(Entity, WidgetId)], raised: Vec<Raised>) {
    for event in raised {
        if let Some((_, id)) = scene_ids.iter().find(|(e, _)| *e == event.widget) {
            queue_button_event(world, event.widget, id, &event.node, event.phase);
        }
        if let Some(action) = event.action {
            queue_action(world, event.widget, action);
        }
    }
}

#[derive(Default)]
struct Inbox(Arc<Mutex<Vec<Pointer>>>);

/// The session's subscription to the pointer events.
struct Listening {
    hub: EventHub,
    inbox: Inbox,
    _subscriptions: Vec<SyncSubscription>,
}

fn number(event: &DynEvent, field: usize) -> f64 {
    match event.fields.get(field) {
        Some(DynValue::F64(v)) => *v,
        Some(DynValue::I64(v)) => *v as f64,
        _ => 0.0,
    }
}

impl Listening {
    fn to(hub: &EventHub) -> Self {
        use pulsar_events::builtin::{MouseMoved, PointerClaimedDown, PointerClaimedUp};
        let inbox = Inbox::default();
        let mut subscriptions = Vec::new();
        let mut route = |id: u64, make: fn(&DynEvent) -> Option<Pointer>| {
            let queue = inbox.0.clone();
            subscriptions.push(hub.bus().subscribe_dyn(
                id,
                SubscribeOptions::default(),
                Box::new(move |event: &DynEvent| {
                    if let Some(pointer) = make(event) {
                        queue
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(pointer);
                    }
                }),
            ));
        };
        route(MouseMoved::stable_type_id(), |e| {
            Some(Pointer::Moved(Vec2::new(
                number(e, 0) as f32,
                number(e, 1) as f32,
            )))
        });
        // The primary button only (0).
        route(PointerClaimedDown::stable_type_id(), |e| {
            (number(e, 0) == 0.0)
                .then(|| Pointer::Down(Vec2::new(number(e, 1) as f32, number(e, 2) as f32)))
        });
        route(PointerClaimedUp::stable_type_id(), |e| {
            (number(e, 0) == 0.0)
                .then(|| Pointer::Up(Vec2::new(number(e, 1) as f32, number(e, 2) as f32)))
        });
        Self {
            hub: hub.clone(),
            inbox,
            _subscriptions: subscriptions,
        }
    }
}

/// The interaction system's state, a session resource.
#[derive(Default)]
struct Session {
    listening: Option<Listening>,
    taken: Vec<Pointer>,
}

/// The per-frame system of a playing session: takes the pointer events since
/// the last frame and raises the clicks as component events.
pub fn system(context: &mut SystemContext<'_>) {
    let Some(runtime) = ensure_runtime(context.world) else {
        return;
    };
    let session = context
        .runtime
        .scope_mut()
        .get_or_insert_with(Session::default);
    if !session
        .listening
        .as_ref()
        .is_some_and(|l| l.hub.ptr_eq(context.events))
    {
        session.listening = Some(Listening::to(context.events));
    }
    session.taken.clear();
    if let Some(listening) = &session.listening {
        session.taken.append(
            &mut listening
                .inbox
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
    if session.taken.is_empty() {
        return;
    }
    let mut raised = Vec::new();
    // The id of each widget something happened to.
    let mut ids: Vec<(Entity, WidgetId)> = Vec::new();
    {
        let mut scene = runtime.lock();
        scene.sync(context.world);
        for event in &session.taken {
            raised.extend(handle(&mut scene, *event));
        }
        for event in &raised {
            if ids.iter().any(|(e, _)| *e == event.widget) {
                continue;
            }
            let shown = scene.instances.get(&event.widget);
            if let Some((_, loaded)) = shown.and_then(|i| i.widget.as_ref()) {
                ids.push((event.widget, loaded.asset.id.clone()));
            }
        }
    }
    publish(context.world, &ids, raised);
}

/// What a button looks like now (tests).
pub fn look(scene: &WidgetScene, widget: Entity, node: &NodeKey) -> PreviewState {
    match scene.instances.get(&widget) {
        Some(i) if i.pressed.as_ref() == Some(node) => PreviewState::Pressed,
        Some(i) if i.hovered.as_ref() == Some(node) => PreviewState::Hovered,
        _ => PreviewState::Normal,
    }
}
