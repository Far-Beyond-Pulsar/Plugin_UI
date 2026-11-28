//! The retained widgets of a world.
//!
//! A [`WidgetScene`] follows the world's widget components through its own
//! change cursors, so a frame in which nothing changed reads no component,
//! lays nothing out, builds no primitive and allocates nothing. Each view (an
//! output size on screen; one shared view for world widgets, whose layout does
//! not depend on the view) keeps a resolved, laid-out tree per widget. A
//! changed parameter sets exactly the properties it drives in that tree, lays
//! the one widget out again only when its size can have changed, and rebuilds
//! only that widget's primitives.
//!
//! How others do it: Slate's invalidation panels and UI Toolkit's retained
//! visual tree repaint what was invalidated (paint, or layout when a desired
//! size moved) and cache the rest; Unity's uGUI rebuilds a whole canvas when
//! one element changes, which is the shape to avoid.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use glam::{DMat4, DVec3, Vec3};
use helio_ui_api::{UiBatch, UiFrame, UiLayer, UiPlacement, UiPlacements};
use pulsar_scenedb::{ChangeCursor, ChangeRead, ComponentChange, Entity, World};
use pulsar_world_registry::pulsar_scene_model::attachments::owner_of;
use pulsar_world_registry::pulsar_scene_model::hierarchy_transform::world_matrix_of;
use pulsar_world_registry::pulsar_scene_model::{Parent, Transform, Visibility};
use serde_json::Value as Json;
use widget_asset::display::{self, DisplayOptions, PreviewState};
use widget_asset::layout::{layout, text_size, TextMetrics};
use widget_asset::model::schema::Kind;
use widget_asset::model::{NodeId, ParamId, Vec2, WidgetAsset};
use widget_asset::resolve::{drive, resolve_with, ResolvedNode};

use crate::assets::LoadedWidget;
use crate::component::{BoundImages, WidgetComponent, WidgetParams};
use crate::paint::{self, Mapping};

/// A widget's loaded images, with how it wants them held.
fn images_of(component: &WidgetComponent) -> BoundImages {
    BoundImages {
        options: component.texture_options(),
        ..component.images.clone()
    }
}

/// Screen views kept (split viewports have one each).
const MAX_VIEWS: usize = 8;

/// Properties that change how a node looks but not any size, so its widget is
/// not laid out again.
const PAINT_ONLY: &[&str] = &[
    "color",
    "background",
    "gradient_to",
    "gradient_mid",
    "gradient_mid_at",
    "gradient_angle",
    "gradient_kind",
    "border_color",
    "border_style",
    "dash_length",
    "border_width",
    "corner_radius",
    "shadow_color",
    "shadow_offset",
    "shadow_blur",
    "outline_color",
    "outline_width",
    "opacity",
    "tint",
    "percent",
    "value",
    "checked",
    "normal_color",
    "hover_color",
    "pressed_color",
    "disabled_color",
    "fill_color",
    "back_color",
    "track_color",
    "handle_color",
    "check_color",
    "box_color",
    "text_color",
    "angle",
    "enabled",
    "stretch",
    "slice",
    "source",
    "action",
    "tooltip",
    "justify",
];

/// What the runtime did, for tests and measurements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Widgets resolved from their asset.
    pub resolves: u64,
    /// Widgets laid out.
    pub layouts: u64,
    /// Widgets whose primitives were rebuilt.
    pub paints: u64,
    /// Components re-read because they changed.
    pub reads: u64,
}

/// The component's fields that place and show the widget.
#[derive(Clone, PartialEq, Debug)]
pub struct Placing {
    pub visible: bool,
    pub z_order: i32,
    pub world_space: bool,
    pub anchor: [f32; 2],
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub scale: f32,
    pub world_width: f32,
    pub face_camera: bool,
    pub constant_screen_size: bool,
}

impl Placing {
    fn of(c: &WidgetComponent) -> Self {
        Self {
            visible: c.enabled,
            z_order: c.z_order,
            world_space: c.world_space,
            anchor: c.screen_anchor,
            position: c.screen_position,
            size: c.screen_size,
            scale: c.screen_scale,
            world_width: c.world_width,
            face_camera: c.face_camera,
            constant_screen_size: c.constant_screen_size,
        }
    }
}

/// Where a world widget is: its centre and axes in the world.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Pose {
    pub centre: DVec3,
    pub right: Vec3,
    pub down: Vec3,
    /// The object's horizontal scale.
    pub scale: f32,
}

/// One node of a widget, by its chain of ids.
pub type NodeKey = Vec<NodeId>;

/// One widget component of the world.
pub struct Instance {
    /// Names the widget in the layers.
    pub key: u64,
    /// Its placement row, while it is a world widget.
    pub slot: u32,
    pub placing: Placing,
    /// The owner object is shown.
    pub owner_visible: bool,
    pub asset_path: String,
    pub widget: Option<(u64, Arc<LoadedWidget>)>,
    pub parameters: WidgetParams,
    pub images: BoundImages,
    pub pose: Option<Pose>,
    /// Moves whenever what the widget draws can differ.
    pub revision: u64,
    pub hovered: Option<NodeKey>,
    pub pressed: Option<NodeKey>,
}

impl Instance {
    pub fn is_shown(&self) -> bool {
        self.placing.visible && self.owner_visible && self.widget.is_some()
    }

    /// The effective value of every parameter, by id.
    fn values(&self, asset: &WidgetAsset) -> HashMap<ParamId, Json> {
        asset
            .parameters
            .iter()
            .map(|p| {
                let set = self.parameters.get(&p.name).and_then(|v| p.class.coerce(v));
                (p.id, set.unwrap_or_else(|| p.default.clone()))
            })
            .collect()
    }
}

/// A widget as one view shows it.
pub struct ViewItem {
    pub tree: ResolvedNode,
    asset_version: u64,
    values: HashMap<ParamId, Json>,
    /// The size it was laid out in, in widget units.
    pub layout_size: Vec2,
    /// Widget units to the view's layer.
    pub mapping: Mapping,
    built: u64,
    pub revision: u64,
    pub primitives: Arc<[helio_ui_api::UiPrimitive]>,
}

/// The widgets as one output sees them.
#[derive(Default)]
pub struct View {
    pub items: HashMap<Entity, ViewItem>,
    pub layer: Arc<UiLayer>,
    /// The scene revision and atlas epoch the layer was built at.
    built: Option<(u64, u64)>,
    last_used: u64,
}

/// What a frame publishes, cached while nothing changes.
#[derive(Default)]
struct Published {
    frame: Option<Arc<UiFrame>>,
    placements: Arc<UiPlacements>,
    placements_at: Option<(u64, DVec3)>,
}

/// The camera of the view last published, for pointer rays.
#[derive(Clone, Copy, Debug)]
pub struct PointerCamera {
    /// Frame space (camera-relative) to clip.
    pub view_proj: glam::Mat4,
    /// Frame space to view.
    pub view: glam::Mat4,
    pub origin: DVec3,
}

struct Cursors {
    widgets: ChangeCursor,
    visibility: ChangeCursor,
    /// Opened while there are world widgets.
    placed: Option<(ChangeCursor, ChangeCursor)>,
    scratch: Vec<ComponentChange>,
}

/// See the module doc.
#[derive(Default)]
pub struct WidgetScene {
    cursors: Option<Cursors>,
    pub instances: HashMap<Entity, Instance>,
    /// Moves whenever any view's layers can differ.
    pub revision: u64,
    /// Moves when a world widget's placement can differ.
    placement_revision: u64,
    next: u64,
    free_slots: Vec<u32>,
    slots: u32,
    pub screen_views: HashMap<[u32; 2], View>,
    pub world_view: View,
    clock: u64,
    published: HashMap<[u32; 2], Published>,
    /// The output size and camera last published: what pointer positions are
    /// in.
    pub pointer_view: Option<([u32; 2], PointerCamera)>,
    pub stats: Stats,
}

fn owner_shown(world: &World, instance: Entity) -> bool {
    let mut at = Some(owner_of(world, instance).unwrap_or(instance));
    for _ in 0..64 {
        let Some(entity) = at else {
            break;
        };
        if world.get::<Visibility>(entity).is_some_and(|v| !v.visible) {
            return false;
        }
        at = world.get::<Parent>(entity).map(|p| p.0);
    }
    true
}

fn pose_of(world: &World, instance: Entity) -> Option<Pose> {
    let owner = owner_of(world, instance).unwrap_or(instance);
    let matrix: DMat4 = world_matrix_of(world, owner)?;
    let x = matrix.x_axis.truncate().as_vec3();
    let y = matrix.y_axis.truncate().as_vec3();
    Some(Pose {
        centre: matrix.w_axis.truncate(),
        right: x.normalize_or(Vec3::X),
        down: -y.normalize_or(Vec3::Y),
        scale: x.length(),
    })
}

/// The changes since the cursor's last read, or `None` when everything must be
/// read again.
fn changes<'a>(
    world: &World,
    cursor: &mut ChangeCursor,
    scratch: &'a mut Vec<ComponentChange>,
) -> Option<&'a [ComponentChange]> {
    scratch.clear();
    match world.read_changes(cursor, scratch) {
        ChangeRead::Complete => Some(scratch),
        ChangeRead::Overflowed => None,
    }
}

impl WidgetScene {
    fn next(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    /// Whether any world widget exists.
    fn has_world(&self) -> bool {
        self.instances.values().any(|i| i.placing.world_space)
    }

    /// Take in what changed in `world` since the last call.
    pub fn sync(&mut self, world: &World) {
        let first = self.cursors.is_none();
        let mut cursors = self.cursors.take().unwrap_or_else(|| Cursors {
            widgets: world.open_change_cursor::<WidgetComponent>(),
            visibility: world.open_change_cursor::<Visibility>(),
            placed: None,
            scratch: Vec::new(),
        });
        let widgets = if first {
            None
        } else {
            changes(world, &mut cursors.widgets, &mut cursors.scratch)
        };
        match widgets {
            Some(changed) => {
                for change in changed {
                    self.refresh(world, change.entity);
                }
            }
            None => self.rescan(world),
        }

        if !self.instances.is_empty() {
            let hidden = changes(world, &mut cursors.visibility, &mut cursors.scratch);
            if hidden.is_none_or(|c| !c.is_empty()) {
                self.refresh_visibility(world);
            }
        }

        // Transforms are followed only while a widget stands in the world.
        if self.has_world() {
            let moved = match &mut cursors.placed {
                Some((transforms, parents)) => {
                    let a = changes(world, transforms, &mut cursors.scratch)
                        .is_none_or(|c| !c.is_empty());
                    let b =
                        changes(world, parents, &mut cursors.scratch).is_none_or(|c| !c.is_empty());
                    a || b
                }
                None => {
                    cursors.placed = Some((
                        world.open_change_cursor::<Transform>(),
                        world.open_change_cursor::<Parent>(),
                    ));
                    true
                }
            };
            if moved {
                self.refresh_poses(world);
            }
        } else {
            cursors.placed = None;
        }
        self.cursors = Some(cursors);
    }

    fn rescan(&mut self, world: &World) {
        let present: Vec<Entity> = world
            .query::<&WidgetComponent>()
            .map(|(entity, _)| entity)
            .collect();
        let gone: Vec<Entity> = self
            .instances
            .keys()
            .filter(|e| !present.contains(e))
            .copied()
            .collect();
        for entity in present.into_iter().chain(gone) {
            self.refresh(world, entity);
        }
    }

    fn refresh_visibility(&mut self, world: &World) {
        let mut changed = false;
        for (entity, instance) in &mut self.instances {
            let shown = owner_shown(world, *entity);
            if shown != instance.owner_visible {
                instance.owner_visible = shown;
                self.next += 1;
                instance.revision = self.next;
                changed = true;
            }
        }
        if changed {
            self.revision += 1;
            self.placement_revision += 1;
        }
    }

    fn refresh_poses(&mut self, world: &World) {
        for (entity, instance) in &mut self.instances {
            if !instance.placing.world_space {
                continue;
            }
            let pose = pose_of(world, *entity);
            if pose != instance.pose {
                instance.pose = pose;
                self.placement_revision += 1;
            }
        }
    }

    /// Read the component on `entity` again.
    fn refresh(&mut self, world: &World, entity: Entity) {
        let Some(component) = world.get::<WidgetComponent>(entity) else {
            if let Some(gone) = self.instances.remove(&entity) {
                self.free_slots.push(gone.slot);
                self.revision += 1;
                self.placement_revision += 1;
            }
            return;
        };
        self.stats.reads += 1;
        let placing = Placing::of(&component);
        let widget = component.loaded().map(|(v, w)| (v, w.clone()));
        let path: &str = &component.widget_asset;
        let Some(instance) = self.instances.get_mut(&entity) else {
            let slot = self.free_slots.pop().unwrap_or_else(|| {
                self.slots += 1;
                self.slots - 1
            });
            let key = self.next();
            let pose = placing
                .world_space
                .then(|| pose_of(world, entity))
                .flatten();
            let instance = Instance {
                key,
                slot,
                placing,
                owner_visible: owner_shown(world, entity),
                asset_path: path.to_string(),
                widget,
                parameters: component.parameters.clone(),
                images: images_of(component),
                pose,
                revision: key,
                hovered: None,
                pressed: None,
            };
            check_parameters(&instance);
            self.instances.insert(entity, instance);
            self.revision += 1;
            self.placement_revision += 1;
            return;
        };
        let mut changed = false;
        if instance.placing != placing {
            if placing.world_space && !instance.placing.world_space {
                instance.pose = pose_of(world, entity);
            }
            instance.placing = placing;
            self.placement_revision += 1;
            changed = true;
        }
        let version = widget.as_ref().map(|(v, _)| *v);
        let mut check = false;
        if instance.asset_path != path || instance.widget.as_ref().map(|(v, _)| *v) != version {
            instance.asset_path = path.to_string();
            instance.widget = widget;
            self.placement_revision += 1;
            changed = true;
            check = true;
        }
        if instance.parameters != component.parameters {
            instance.parameters = component.parameters.clone();
            changed = true;
            check = true;
        }
        if instance.images.version != component.images.version
            || instance.images.options != component.texture_options()
        {
            instance.images = images_of(component);
            changed = true;
        }
        if check {
            check_parameters(instance);
        }
        if changed {
            self.next += 1;
            instance.revision = self.next;
            self.revision += 1;
        }
    }

    /// Mark a widget's looks as changed (hover and press).
    pub fn touch(&mut self, entity: Entity) {
        let next = self.next();
        if let Some(instance) = self.instances.get_mut(&entity) {
            instance.revision = next;
            self.revision += 1;
        }
    }

    /// Bring the screen layer for an output of `size` pixels up to date.
    pub fn build_screen(&mut self, size: [u32; 2], images: ImageSlots<'_>) -> Arc<UiLayer> {
        self.clock += 1;
        // Nothing changed: no work, no allocation.
        let epoch = crate::text_system().atlas().epoch();
        let revision = self.revision;
        if let Some(view) = self.screen_views.get_mut(&size) {
            if view.built == Some((revision, epoch)) {
                view.last_used = self.clock;
                return view.layer.clone();
            }
        }
        if !self.screen_views.contains_key(&size) && self.screen_views.len() >= MAX_VIEWS {
            if let Some(oldest) = self
                .screen_views
                .iter()
                .min_by_key(|(_, v)| v.last_used)
                .map(|(k, _)| *k)
            {
                self.screen_views.remove(&oldest);
                self.published.remove(&oldest);
            }
        }
        let mut view = self.screen_views.remove(&size).unwrap_or_default();
        view.last_used = self.clock;
        self.build(&mut view, Some(size), images);
        let layer = view.layer.clone();
        self.screen_views.insert(size, view);
        layer
    }

    /// Bring the world layer up to date.
    pub fn build_world(&mut self, images: ImageSlots<'_>) -> Arc<UiLayer> {
        let epoch = crate::text_system().atlas().epoch();
        if self.world_view.built == Some((self.revision, epoch)) {
            return self.world_view.layer.clone();
        }
        let mut view = std::mem::take(&mut self.world_view);
        self.build(&mut view, None, images);
        let layer = view.layer.clone();
        self.world_view = view;
        layer
    }

    /// `size`: the output of a screen view; `None` for the world view.
    fn build(&mut self, view: &mut View, size: Option<[u32; 2]>, images: ImageSlots<'_>) {
        let text = crate::text_system();
        let epoch = text.atlas().epoch();
        if view.built == Some((self.revision, epoch)) {
            return;
        }
        profiling::profile_scope!("Widgets::build_view");
        let evicted = view.built.is_some_and(|(_, e)| e != epoch);
        text.atlas().tick();
        let world = size.is_none();
        let mut shown: Vec<(i32, u64, Entity)> = Vec::new();
        for (entity, instance) in &self.instances {
            if instance.placing.world_space != world || !instance.is_shown() {
                continue;
            }
            update_item(
                view,
                *entity,
                instance,
                size,
                evicted,
                images,
                &mut self.stats,
            );
            shown.push((instance.placing.z_order, instance.key, *entity));
        }
        view.items.retain(|entity, _| {
            self.instances
                .get(entity)
                .is_some_and(|i| i.placing.world_space == world && i.widget.is_some())
        });
        shown.sort_unstable_by_key(|(z, key, _)| (*z, *key));
        let batches: Vec<UiBatch> = shown
            .iter()
            .filter_map(|(_, key, entity)| {
                let item = view.items.get(entity)?;
                Some(UiBatch {
                    key: *key,
                    revision: item.revision,
                    primitives: item.primitives.clone(),
                    placement: self.instances.get(entity)?.slot,
                })
            })
            .collect();
        let same = batches.len() == view.layer.batches.len()
            && batches
                .iter()
                .zip(&view.layer.batches)
                .all(|(a, b)| a.key == b.key && a.revision == b.revision);
        if !same || view.built.is_none() {
            let revision = self.next();
            view.layer = Arc::new(UiLayer { revision, batches });
        }
        // Painting may itself have evicted; the next call then repaints.
        view.built = Some((self.revision, epoch));
    }

    /// The placement rows, relative to `origin`.
    fn placements(&self) -> impl Iterator<Item = (u32, &Instance)> {
        self.instances
            .values()
            .filter(|i| i.placing.world_space)
            .map(|i| (i.slot, i))
    }

    /// The frame to draw on an output of `size` pixels whose frame origin is
    /// `origin`: the same `Arc` while nothing changed. `None` while there is
    /// no widget at all.
    pub fn frame(
        &mut self,
        size: [u32; 2],
        origin: DVec3,
        images: ImageSlots<'_>,
    ) -> Option<Arc<UiFrame>> {
        if self.instances.is_empty() {
            self.published.clear();
            self.screen_views.clear();
            self.world_view = View::default();
            return None;
        }
        let screen = self.build_screen(size, images);
        let world = self.build_world(images);
        let mut published = self.published.remove(&size).unwrap_or_default();
        let at = (self.placement_revision, origin);
        if published.placements_at != Some(at) {
            published.placements_at = Some(at);
            let mut rows = vec![UiPlacement::default(); self.slots as usize];
            for (slot, instance) in self.placements() {
                if let (Some(pose), Some((_, widget))) = (instance.pose, &instance.widget) {
                    rows[slot as usize] =
                        placement_row(&instance.placing, &pose, widget.asset.design_size, origin);
                }
            }
            if rows != published.placements.rows {
                let revision = self.next();
                published.placements = Arc::new(UiPlacements { revision, rows });
            }
        }
        let current = published.frame.as_ref().filter(|f| {
            Arc::ptr_eq(&f.screen, &screen)
                && Arc::ptr_eq(&f.world, &world)
                && Arc::ptr_eq(&f.placements, &published.placements)
        });
        if current.is_none() {
            published.frame = Some(Arc::new(UiFrame {
                screen,
                world,
                placements: published.placements.clone(),
                atlas: crate::text_system().clone(),
            }));
        }
        let frame = published.frame.clone();
        self.published.insert(size, published);
        frame
    }
}

/// A world widget's placement row.
pub fn placement_row(placing: &Placing, pose: &Pose, design: Vec2, origin: DVec3) -> UiPlacement {
    let centre = (pose.centre - origin).as_vec3();
    let per_unit = placing.world_width.max(0.0) * pose.scale / design.x.max(1.0);
    let mut flags = 0;
    if placing.face_camera {
        flags |= helio_ui_api::PLACE_BILLBOARD;
    }
    if placing.constant_screen_size {
        flags |= helio_ui_api::PLACE_CONSTANT_SIZE;
    }
    UiPlacement {
        centre: [centre.x, centre.y, centre.z, 0.0],
        right: [pose.right.x, pose.right.y, pose.right.z, flags as f32],
        down: [pose.down.x, pose.down.y, pose.down.z, per_unit],
        size: [design.x, design.y, 0.0, 0.0],
    }
}

/// Report values the loaded widget cannot take (a level or a script that set
/// them before the widget loaded).
fn check_parameters(instance: &Instance) {
    let Some((_, widget)) = &instance.widget else {
        return;
    };
    let path = &instance.asset_path;
    for (name, value) in instance.parameters.iter() {
        let problem = match widget.asset.parameter_named(name) {
            None => "the widget has no such parameter".to_string(),
            Some(p) if p.class.coerce(value).is_none() => {
                format!("{value} is not a {} value", p.class.label())
            }
            Some(_) => continue,
        };
        crate::report::once(
            format!("parameter|{path}|{name}|{problem}"),
            path,
            format!("Widget {path}, parameter `{name}`: {problem}"),
            "A level or a script sets a parameter the widget cannot take; the asset's default is shown.",
        );
    }
}

/// The scene texture slot and pixel size of a loaded image of a widget.
pub type ImageSlots<'a> = &'a dyn Fn(&BoundImages, &str) -> Option<crate::paint::Image>;

/// What a changed property needs.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Dirty {
    Nothing,
    Paint,
    Layout,
}

/// Set the properties `param` drives under `node`; returns what that needs.
fn apply(node: &mut ResolvedNode, param: ParamId, value: &Json, text: &dyn TextMetrics) -> Dirty {
    let mut dirty = Dirty::Nothing;
    for index in 0..node.drivers.len() {
        if node.drivers[index].1 != param {
            continue;
        }
        let prop = node.drivers[index].0.clone();
        if !drive(node, &prop, value) {
            continue;
        }
        let needs = if PAINT_ONLY.contains(&prop.as_str()) {
            Dirty::Paint
        } else if prop == "text" && node.kind == Kind::Text && !node.view().bool("wrap") {
            // A new text that takes the room the old one did (a counter in a
            // face with tabular figures, a right-aligned number in a fixed
            // slot) moves nothing.
            let size = text_size(node, f32::INFINITY, text);
            if size == node.desired {
                Dirty::Paint
            } else {
                Dirty::Layout
            }
        } else {
            Dirty::Layout
        };
        if needs > dirty {
            dirty = needs;
        }
    }
    for child in &mut node.children {
        let needs = apply(child, param, value, text);
        if needs > dirty {
            dirty = needs;
        }
    }
    dirty
}

/// The layout size and mapping of `instance` in a view.
fn layout_of(instance: &Instance, design: Vec2, size: Option<[u32; 2]>) -> (Vec2, Mapping) {
    let placing = &instance.placing;
    let Some(output) = size else {
        return (
            design,
            Mapping {
                scale: 1.0,
                origin: Vec2::ZERO,
                pixels: false,
                placement: instance.slot,
            },
        );
    };
    // One scale: the design height fits the output's height.
    let scale = (output[1] as f32 / design.y.max(1.0) * placing.scale).max(0.01);
    let viewport = Vec2::new(output[0] as f32 / scale, output[1] as f32 / scale);
    let width = if placing.size[0] > 0.0 {
        placing.size[0]
    } else {
        viewport.x
    };
    let height = if placing.size[1] > 0.0 {
        placing.size[1]
    } else {
        viewport.y
    };
    let origin = Vec2::new(
        placing.anchor[0] * (viewport.x - width) + placing.position[0],
        placing.anchor[1] * (viewport.y - height) + placing.position[1],
    );
    (
        Vec2::new(width, height),
        Mapping {
            scale,
            origin,
            pixels: true,
            placement: 0,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn update_item(
    view: &mut View,
    entity: Entity,
    instance: &Instance,
    size: Option<[u32; 2]>,
    evicted: bool,
    images: ImageSlots<'_>,
    stats: &mut Stats,
) {
    let Some((version, widget)) = &instance.widget else {
        return;
    };
    let text = crate::text_system();
    let design = widget.asset.design_size;
    let (layout_size, mapping) = layout_of(instance, design, size);
    let fresh = view
        .items
        .get(&entity)
        .is_none_or(|item| item.asset_version != *version);
    let mut dirty = Dirty::Nothing;
    if fresh {
        let values = instance.values(&widget.asset);
        let tree = resolve_with(&widget.asset, &values, &widget.library);
        stats.resolves += 1;
        view.items.insert(
            entity,
            ViewItem {
                tree,
                asset_version: *version,
                values,
                layout_size,
                mapping,
                built: 0,
                revision: 0,
                primitives: Arc::from([]),
            },
        );
        dirty = Dirty::Layout;
    }
    let Some(item) = view.items.get_mut(&entity) else {
        return;
    };
    if item.built == instance.revision
        && !evicted
        && dirty == Dirty::Nothing
        && item.mapping == mapping
    {
        return;
    }
    if !fresh {
        for parameter in &widget.asset.parameters {
            let set = instance.parameters.get(&parameter.name);
            let value = set
                .and_then(|v| parameter.class.coerce(v))
                .unwrap_or_else(|| parameter.default.clone());
            if item.values.get(&parameter.id) != Some(&value) {
                let needs = apply(&mut item.tree, parameter.id, &value, text.as_ref());
                if needs > dirty {
                    dirty = needs;
                }
                item.values.insert(parameter.id, value);
            }
        }
        if item.layout_size != layout_size {
            item.layout_size = layout_size;
            dirty = Dirty::Layout;
        }
    }
    item.mapping = mapping;
    if dirty == Dirty::Layout {
        layout(&mut item.tree, layout_size, text.as_ref());
        stats.layouts += 1;
    }
    item.built = instance.revision;

    let mut commands = Vec::new();
    let options = DisplayOptions {
        state: PreviewState::Normal,
        placeholders: false,
        time: 0.0,
    };
    display::build_into(
        &item.tree,
        &options,
        &|node| {
            if node.kind != Kind::Button {
                None
            } else if instance.pressed.as_ref() == Some(&node.key) {
                Some(PreviewState::Pressed)
            } else if instance.hovered.as_ref() == Some(&node.key) {
                Some(PreviewState::Hovered)
            } else {
                None
            }
        },
        &mut commands,
    );
    let mut primitives = Vec::with_capacity(item.primitives.len());
    paint::primitives(
        &commands,
        mapping,
        text,
        &mut |path| images(&instance.images, path),
        &mut primitives,
    );
    stats.paints += 1;
    if fresh || *item.primitives != *primitives {
        item.primitives = primitives.into();
        item.revision += 1;
    }
}

// ── The world's runtime ────────────────────────────────────────────────────

/// The runtime widgets of a world, kept on an entity of its own, so every
/// renderer of the world and its interaction see the same widgets.
#[derive(Clone, Default)]
pub struct WidgetRuntime(Arc<Mutex<WidgetScene>>);

impl WidgetRuntime {
    pub fn lock(&self) -> MutexGuard<'_, WidgetScene> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// `world`'s widget runtime, if one was made.
pub fn runtime_of(world: &World) -> Option<WidgetRuntime> {
    world
        .query::<&WidgetRuntime>()
        .next()
        .map(|(_, runtime)| runtime.clone())
}

/// `world`'s widget runtime, made if it has widgets and none yet.
pub fn ensure_runtime(world: &mut World) -> Option<WidgetRuntime> {
    if let Some(runtime) = runtime_of(world) {
        return Some(runtime);
    }
    world.query::<&WidgetComponent>().next()?;
    let runtime = WidgetRuntime::default();
    let entity = world.spawn();
    world.insert(entity, runtime.clone());
    Some(runtime)
}
