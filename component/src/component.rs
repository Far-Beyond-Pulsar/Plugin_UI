//! The widget component: one widget a game shows, on screen or in the world.
//!
//! The component reference is the handle a script holds: it has liveness,
//! reflected methods, level authoring and the component events every other
//! component has. Parameter values that differ from the asset's defaults are
//! part of the component, so they are saved with the level.

use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::Arc;

use engine_class_derive::{component_events, engine_class, world_component};
use pulsar_reflection::{ReflectError, ReflectResult};
use pulsar_scenedb::{component_methods, AssetId, AssetView, Entity, World};
use serde_json::{json, Value as Json};
use widget_asset::model::geom::Color;
use widget_asset::model::schema::ValueClass;

use crate::assets::{widget_asset_id, LoadedWidget, WIDGET_ASSET_KIND};

/// The class name scripts and level files use.
pub const WIDGET_CLASS_NAME: &str = "WidgetComponent";
/// The component event a clicked button with an action raises.
pub const ON_ACTION_EVENT: &str = "WidgetComponent.on_action";

pulsar_world_registry::asset_path_type! {
    /// A widget file.
    pub WidgetAssetPath, empty = "No widget", extensions = ["widget"]
}

/// Parameter values that differ from the asset's defaults, by parameter name.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct WidgetParams(pub BTreeMap<String, Json>);

impl Deref for WidgetParams {
    type Target = BTreeMap<String, Json>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn params_to_json(params: &WidgetParams) -> ReflectResult<Json> {
    Ok(Json::Object(
        params.0.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    ))
}

fn params_from_json(value: Json) -> ReflectResult<WidgetParams> {
    match value {
        Json::Object(map) => Ok(WidgetParams(map.into_iter().collect())),
        Json::Null => Ok(WidgetParams::default()),
        other => Err(ReflectError::TypeMismatch {
            expected: "an object of parameter values",
            found: format!("{other:?}"),
        }),
    }
}

#[pulsar_reflection::pulsar_type(
    serialize_json_with = params_to_json,
    deserialize_json_with = params_from_json
)]
#[allow(dead_code)]
type RegisteredWidgetParams = WidgetParams;

// The inspector edits the values as one line of JSON, such as
// `{"score": "12", "bar": "#ff0000"}`: the editor knows the property's value,
// not which widget it belongs to.
pulsar_reflection::inventory::submit! {
    ui_types_common::TextEditedType {
        type_id: std::any::TypeId::of::<WidgetParams>(),
        to_text: |value| {
            let params = value.downcast_ref::<WidgetParams>()?;
            serde_json::to_string(&params.0).ok()
        },
        from_text: |line| {
            let line = line.trim();
            if line.is_empty() {
                return Ok(Box::new(WidgetParams::default()));
            }
            serde_json::from_str::<WidgetParams>(line)
                .map(|params| Box::new(params) as Box<dyn std::any::Any + Send>)
                .map_err(|error| format!("not an object of parameter values: {error}"))
        },
    }
}

/// The widget the world loaded for the component's path. Never saved.
#[derive(Clone, Default)]
pub struct BoundWidget(Option<(AssetId, u64, Arc<LoadedWidget>)>);

impl BoundWidget {
    /// The loaded widget and its content version, when it is `expected`.
    pub fn get(&self, expected: AssetId) -> Option<(u64, &Arc<LoadedWidget>)> {
        self.0
            .as_ref()
            .filter(|(id, ..)| *id == expected && !expected.is_zero())
            .map(|(_, version, widget)| (*version, widget))
    }
}

impl std::fmt::Debug for BoundWidget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some((id, version, _)) => write!(f, "BoundWidget({:032x} v{version})", id.0),
            None => f.write_str("BoundWidget(none)"),
        }
    }
}

/// The images the widget shows, as the world loaded them. Never saved.
#[derive(Clone, Debug, Default)]
pub struct BoundImages {
    /// By the path the widget or a parameter names.
    pub images: BTreeMap<String, helio_component::texture_assets::BoundTexture>,
    /// Moves when an image is bound.
    pub version: u64,
}

#[engine_class(category = "UI", clone, debug, serialize, deserialize)]
#[category("Widget", category_color = "#9B7BFF")]
#[category("Screen", category_color = "#38BDF8")]
#[category("World", category_color = "#34D399")]
#[category("Parameters", category_color = "#FBBF24")]
#[serde(default)]
pub struct WidgetComponent {
    /// Whether the widget is shown and takes clicks. Off until something turns
    /// it on, so a widget never covers the editor's viewport: the Blueprint of
    /// the class that holds it enables it (Set Enabled) when it wants it shown.
    #[property(category = "Widget")]
    pub enabled: bool,
    /// The widget file.
    #[property(category = "Widget")]
    pub widget_asset: WidgetAssetPath,
    /// Draw order among screen widgets: higher draws on top.
    #[property(min = -100000, max = 100000, category = "Widget")]
    pub z_order: i32,
    /// Off: on screen. On: in the world, at the object.
    #[property(category = "Widget")]
    pub world_space: bool,
    /// Screen: the point of the viewport (0 to 1) the widget is placed
    /// against, and the same point of the widget.
    #[property(category = "Screen")]
    pub screen_anchor: [f32; 2],
    /// Screen: moved from the anchor by this, in design units.
    #[property(category = "Screen")]
    pub screen_position: [f32; 2],
    /// Screen: the widget's size in design units; zero on an axis fills the
    /// viewport on it.
    #[property(category = "Screen")]
    pub screen_size: [f32; 2],
    /// Screen: a factor on top of the scale that fits the design height to
    /// the viewport.
    #[property(min = 0.05, max = 20.0, step = 0.05, category = "Screen")]
    pub screen_scale: f32,
    /// World: the widget's width in metres (its height follows the design
    /// size's proportions). With a constant screen size, its width one metre
    /// away.
    #[property(min = 0.001, max = 10000.0, step = 0.05, category = "World")]
    pub world_width: f32,
    /// World: turn to face the camera.
    #[property(category = "World")]
    pub face_camera: bool,
    /// World: keep the same size on screen at any distance.
    #[property(category = "World")]
    pub constant_screen_size: bool,
    /// Values for the widget's parameters, where they differ from the asset.
    #[property(category = "Parameters")]
    pub parameters: WidgetParams,
    /// The loaded widget. Never authored.
    #[serde(skip)]
    pub asset: BoundWidget,
    /// The loaded images. Never authored.
    #[serde(skip)]
    pub images: BoundImages,
}

impl Default for WidgetComponent {
    fn default() -> Self {
        Self {
            enabled: false,
            widget_asset: Default::default(),
            z_order: 0,
            world_space: false,
            screen_anchor: [0.0, 0.0],
            screen_position: [0.0, 0.0],
            screen_size: [0.0, 0.0],
            screen_scale: 1.0,
            world_width: 2.0,
            face_camera: true,
            constant_screen_size: false,
            parameters: Default::default(),
            asset: Default::default(),
            images: Default::default(),
        }
    }
}

impl WidgetComponent {
    /// The loaded widget and its version, once the world loaded the path's.
    pub fn loaded(&self) -> Option<(u64, &Arc<LoadedWidget>)> {
        self.asset.get(widget_asset_id(&self.widget_asset))
    }

    /// Why `value` cannot be set as `class` parameter `name` of the loaded
    /// widget, or the value as stored. Unchecked (accepted as given) while
    /// the widget is not loaded; the runtime checks it when it loads.
    pub fn check_parameter(
        &self,
        name: &str,
        class: ValueClass,
        value: &Json,
    ) -> Result<Json, String> {
        let stored = class
            .coerce(value)
            .ok_or_else(|| format!("{value} is not a {} value", class.label()))?;
        let Some((_, widget)) = self.loaded() else {
            return Ok(stored);
        };
        let Some(parameter) = widget.asset.parameter_named(name) else {
            return Err(format!("the widget has no parameter `{name}`"));
        };
        if parameter.class != class {
            return Err(format!(
                "parameter `{name}` is {} and was used as {}",
                parameter.class.label(),
                class.label()
            ));
        }
        Ok(stored)
    }

    fn report(&self, name: &str, problem: &str) {
        let asset: &str = &self.widget_asset;
        crate::report::once(
            format!("parameter|{asset}|{name}|{problem}"),
            asset,
            format!("Widget {asset}, parameter `{name}`: {problem}"),
            "A script or a level sets a parameter the widget cannot take; the value was ignored.",
        );
    }

    /// Set a parameter of the widget on `entity`. Setting the value it
    /// already has writes nothing.
    pub fn set_parameter(
        world: &mut World,
        entity: Entity,
        name: &str,
        class: ValueClass,
        value: Json,
    ) -> Result<(), String> {
        let component = world
            .get::<WidgetComponent>(entity)
            .ok_or("the object has no widget component")?;
        let stored = match component.check_parameter(name, class, &value) {
            Ok(stored) => stored,
            Err(problem) => {
                component.report(name, &problem);
                return Ok(());
            }
        };
        let current = component.parameters.get(name).or_else(|| {
            component
                .loaded()
                .and_then(|(_, w)| w.asset.parameter_named(name))
                .map(|p| &p.default)
        });
        if current == Some(&stored) {
            return Ok(());
        }
        let mut component = world
            .get_mut::<WidgetComponent>(entity)
            .ok_or("the object has no widget component")?;
        component.parameters.0.insert(name.to_string(), stored);
        Ok(())
    }

    /// The value parameter `name` has: what was set, else the asset's
    /// default. A wrong name or class is reported once and reads as the
    /// class's default.
    pub fn parameter_value(&self, name: &str, class: ValueClass) -> Json {
        let set = self.parameters.get(name);
        if let Some((_, widget)) = self.loaded() {
            match widget.asset.parameter_named(name) {
                Some(p) if p.class == class => {
                    return set
                        .and_then(|v| class.coerce(v))
                        .unwrap_or_else(|| p.default.clone());
                }
                Some(p) => self.report(
                    name,
                    &format!(
                        "it is {} and was read as {}",
                        p.class.label(),
                        class.label()
                    ),
                ),
                None => self.report(name, "the widget has no such parameter"),
            }
            return class.default_value();
        }
        set.and_then(|v| class.coerce(v))
            .unwrap_or_else(|| class.default_value())
    }
}

#[component_methods]
impl WidgetComponent {
    /// Set a number parameter.
    #[world_method(category = "Widget")]
    fn set_number(
        world: &mut World,
        entity: Entity,
        name: String,
        value: f64,
    ) -> Result<(), String> {
        Self::set_parameter(world, entity, &name, ValueClass::Float, json!(value))
    }

    /// Set an integer parameter.
    #[world_method(category = "Widget")]
    fn set_integer(
        world: &mut World,
        entity: Entity,
        name: String,
        value: i64,
    ) -> Result<(), String> {
        Self::set_parameter(world, entity, &name, ValueClass::Int, json!(value))
    }

    /// Set a text parameter.
    #[world_method(category = "Widget")]
    fn set_text(
        world: &mut World,
        entity: Entity,
        name: String,
        value: String,
    ) -> Result<(), String> {
        Self::set_parameter(world, entity, &name, ValueClass::Text, json!(value))
    }

    /// Set a boolean parameter.
    #[world_method(category = "Widget")]
    fn set_bool(
        world: &mut World,
        entity: Entity,
        name: String,
        value: bool,
    ) -> Result<(), String> {
        Self::set_parameter(world, entity, &name, ValueClass::Bool, json!(value))
    }

    /// Set a colour parameter, as `#rrggbb` or `#rrggbbaa`.
    #[world_method(category = "Widget")]
    fn set_color(
        world: &mut World,
        entity: Entity,
        name: String,
        value: String,
    ) -> Result<(), String> {
        Self::set_parameter(world, entity, &name, ValueClass::Color, json!(value))
    }

    /// Set an image parameter to a project-relative image path.
    #[world_method(category = "Widget")]
    fn set_image(
        world: &mut World,
        entity: Entity,
        name: String,
        value: String,
    ) -> Result<(), String> {
        Self::set_parameter(world, entity, &name, ValueClass::Image, json!(value))
    }

    /// Set a two-number parameter.
    #[world_method(category = "Widget")]
    fn set_vec2(
        world: &mut World,
        entity: Entity,
        name: String,
        x: f64,
        y: f64,
    ) -> Result<(), String> {
        Self::set_parameter(world, entity, &name, ValueClass::Vec2, json!([x, y]))
    }

    /// Set a choice parameter to one of its values.
    #[world_method(category = "Widget")]
    fn set_choice(
        world: &mut World,
        entity: Entity,
        name: String,
        value: String,
    ) -> Result<(), String> {
        Self::set_parameter(world, entity, &name, ValueClass::Choice, json!(value))
    }

    #[reflect_method(side_effect_free, category = "Widget")]
    fn get_number(&self, name: String) -> f64 {
        self.parameter_value(&name, ValueClass::Float)
            .as_f64()
            .unwrap_or(0.0)
    }

    #[reflect_method(side_effect_free, category = "Widget")]
    fn get_integer(&self, name: String) -> i64 {
        self.parameter_value(&name, ValueClass::Int)
            .as_i64()
            .unwrap_or(0)
    }

    #[reflect_method(side_effect_free, category = "Widget")]
    fn get_text(&self, name: String) -> String {
        text_of(self.parameter_value(&name, ValueClass::Text))
    }

    #[reflect_method(side_effect_free, category = "Widget")]
    fn get_bool(&self, name: String) -> bool {
        self.parameter_value(&name, ValueClass::Bool)
            .as_bool()
            .unwrap_or(false)
    }

    #[reflect_method(side_effect_free, category = "Widget")]
    fn get_color(&self, name: String) -> String {
        let value = text_of(self.parameter_value(&name, ValueClass::Color));
        Color::from_hex(&value).map_or(value, |c| c.to_hex())
    }

    #[reflect_method(side_effect_free, category = "Widget")]
    fn get_image(&self, name: String) -> String {
        text_of(self.parameter_value(&name, ValueClass::Image))
    }

    #[reflect_method(side_effect_free, category = "Widget")]
    fn get_vec2(&self, name: String) -> [f32; 2] {
        widget_asset::model::geom::json_array2(&self.parameter_value(&name, ValueClass::Vec2))
            .unwrap_or_default()
    }

    #[reflect_method(side_effect_free, category = "Widget")]
    fn get_choice(&self, name: String) -> String {
        text_of(self.parameter_value(&name, ValueClass::Choice))
    }
}

fn text_of(value: Json) -> String {
    match value {
        Json::String(text) => text,
        _ => String::new(),
    }
}

#[world_component]
impl WidgetComponent {}

/// What `on_action` carries: which button was clicked.
#[engine_class(no_register)]
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WidgetAction {
    /// The button's `action` property.
    #[property]
    pub action: String,
    /// The button node's name.
    #[property]
    pub node: String,
}

#[component_events(class = "WidgetComponent")]
pub trait WidgetEvents {
    #[bp_event]
    fn on_action() -> WidgetAction {}
}

/// Queue `on_action` of the widget on `instance`; it is delivered on the
/// instance's and its object's channels at the next flush.
pub fn queue_action(world: &mut World, instance: Entity, action: WidgetAction) {
    let encoded = pulsar_world_registry::pulsar_script_vm::TypeRegistry::global().encode_event_value(
        &pulsar_world_registry::pulsar_script_vm::Object::new("WidgetAction", action),
    );
    match encoded {
        Ok(bytes) => pulsar_world_registry::queue_component_event(
            world,
            instance,
            ON_ACTION_EVENT,
            vec![pulsar_events::gamma::DynValue::Bytes(bytes)],
        ),
        Err(error) => tracing::warn!(%error, "a widget action could not be encoded"),
    }
}

// ── Assets ─────────────────────────────────────────────────────────────────

/// Every image path the component shows: the loaded widget's, then the image
/// parameters set on it.
pub fn image_paths(component: &WidgetComponent) -> Vec<String> {
    let Some((_, widget)) = component.loaded() else {
        return Vec::new();
    };
    let mut paths = widget.images.clone();
    for parameter in &widget.asset.parameters {
        if parameter.class == ValueClass::Image {
            if let Some(path) = component
                .parameters
                .get(&parameter.name)
                .and_then(Json::as_str)
                .filter(|p| !p.trim().is_empty())
            {
                if !paths.iter().any(|p| p == path) {
                    paths.push(path.to_string());
                }
            }
        }
    }
    paths
}

/// The world counts the widget and the images it shows as the component's
/// references, whatever wrote the value, and binds them when they load.
fn collect_widget_handles(
    value: *const (),
    out: &mut Vec<pulsar_scenedb::handle_ledger::HandleId>,
) {
    // SAFETY: SceneDB reaches this only through `WidgetComponent`'s own
    // `ComponentId`, with a pointer to a live value.
    let component = unsafe { &*(value as *const WidgetComponent) };
    out.push(widget_asset_id(&component.widget_asset).handle());
    for path in image_paths(component) {
        helio_component::texture_assets::collect_texture(&path, out);
    }
}

pulsar_scenedb::pulsar_reflection::inventory::submit! {
    pulsar_scenedb::handle_ledger::HandleLedgerRegistration {
        component_id: pulsar_scenedb::component_id::<WidgetComponent>,
        collect_from_value: collect_widget_handles,
    }
}

fn bind_widget(component: &mut WidgetComponent, view: &AssetView) {
    if view.id.is_zero() || view.id != widget_asset_id(&component.widget_asset) {
        return;
    }
    match view.payload::<LoadedWidget>() {
        Some(widget) => component.asset = BoundWidget(Some((view.id, view.version, widget))),
        None => {
            let path: &str = &component.widget_asset;
            crate::report::once(
                format!("load|{path}"),
                path,
                format!("Widget {path} did not load: {:?}", view.state),
                "The widget file is missing or unreadable.",
            );
            component.asset = BoundWidget(None);
        }
    }
}

fn bind_image(component: &mut WidgetComponent, view: &AssetView) {
    for path in image_paths(component) {
        if helio_component::texture_assets::texture_asset_id_of(&path) != view.id {
            continue;
        }
        let mut bound = component.images.images.remove(&path).unwrap_or_default();
        helio_component::texture_assets::bind_texture(&mut bound, &path, view, "widget");
        component.images.images.insert(path, bound);
        component.images.version += 1;
    }
}

/// Registers how a widget component takes in its loaded widget and images.
pub(crate) fn register_bindings() {
    pulsar_scenedb::register_binding::<WidgetComponent>(WIDGET_ASSET_KIND, bind_widget);
    pulsar_scenedb::register_binding::<WidgetComponent>(
        helio_component::texture_assets::TEXTURE_ASSET_KIND,
        bind_image,
    );
}
