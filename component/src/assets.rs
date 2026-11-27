//! The `.widget` file as a world asset.
//!
//! A component names its widget by path; the world loads it on a worker
//! thread ([`load`]) together with every widget it instances, and binds the
//! result into the component. In the editor the instanced widgets are found
//! by id through the project's widget library; a packaged widget carries the
//! paths of the ones it needs ([`COOKED_DEPENDENCIES`]), which [`cook`]
//! writes along with its image references rewritten to content paths.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value as Json};
use widget_asset::library::Library;
use widget_asset::model::schema::{Kind, ValueClass};
use widget_asset::model::{Asset, AssetPath, Node, WidgetAsset, WidgetId};
use widget_asset::model::validate::WidgetSource;

/// The SceneDB asset kind of widgets.
pub const WIDGET_ASSET_KIND: &str = "widget";
/// The key under which a cooked widget lists the widgets it instances, by id.
pub const COOKED_DEPENDENCIES: &str = "cooked_dependencies";

/// A loaded widget with everything it instances.
#[derive(Debug)]
pub struct LoadedWidget {
    pub asset: Arc<WidgetAsset>,
    /// The widgets reachable through instances, by id.
    pub library: HashMap<WidgetId, Arc<WidgetAsset>>,
    /// Every image path the widget and its instances name, sorted.
    pub images: Vec<String>,
}

fn images_of(node: &Node, out: &mut BTreeSet<String>) {
    node.walk(&mut |n| {
        if n.kind == Kind::Image {
            let source = n.view().text("source");
            if !source.trim().is_empty() {
                out.insert(source);
            }
        }
    });
}

impl LoadedWidget {
    /// A widget with the instanced widgets `library` holds.
    pub fn new(asset: Arc<WidgetAsset>, library: HashMap<WidgetId, Arc<WidgetAsset>>) -> Self {
        let mut images = BTreeSet::new();
        for widget in std::iter::once(&asset).chain(library.values()) {
            images_of(&widget.root, &mut images);
            for param in &widget.parameters {
                if param.class == ValueClass::Image {
                    if let Some(path) = param.default.as_str().filter(|p| !p.trim().is_empty()) {
                        images.insert(path.to_string());
                    }
                }
            }
        }
        Self {
            asset,
            library,
            images: images.into_iter().collect(),
        }
    }
}

fn project_root() -> PathBuf {
    engine_state::get_project_path().map_or_else(PathBuf::new, PathBuf::from)
}

fn read_widget(path: &Path) -> Result<(WidgetAsset, Json), String> {
    let shown = path.display();
    let text = engine_fs::virtual_fs::read_to_string(path)
        .map_err(|error| format!("could not read '{shown}': {error}"))?;
    let (widget, _notes) =
        WidgetAsset::parse(&text).map_err(|error| format!("'{shown}': {error}"))?;
    let raw = serde_json::from_str(&text).unwrap_or(Json::Null);
    Ok((widget, raw))
}

/// The widgets `widget` reaches through instances, from `find`.
fn closure(
    widget: &WidgetAsset,
    find: &mut dyn FnMut(&WidgetId) -> Option<Arc<WidgetAsset>>,
) -> HashMap<WidgetId, Arc<WidgetAsset>> {
    let mut library = HashMap::new();
    let mut pending: Vec<WidgetId> = widget.dependencies().into_iter().collect();
    while let Some(id) = pending.pop() {
        if id == widget.id || library.contains_key(&id) {
            continue;
        }
        let Some(found) = find(&id) else {
            continue;
        };
        pending.extend(found.dependencies());
        library.insert(id, found);
    }
    library
}

/// Load the widget at `source` (a project-relative or absolute path) of the
/// project at `root`.
pub fn load_from(root: &Path, source: &str) -> Result<LoadedWidget, String> {
    let path = helio_component::subsystems::resolve_asset_path(root, source);
    let (widget, raw) = read_widget(&path)?;
    let cooked: Option<BTreeMap<String, String>> = raw
        .get(COOKED_DEPENDENCIES)
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let library = match &cooked {
        Some(paths) => closure(&widget, &mut |id| {
            let file = paths.get(id.as_str())?;
            let file = helio_component::subsystems::resolve_asset_path(root, file);
            read_widget(&file).ok().map(|(w, _)| Arc::new(w))
        }),
        None if widget.dependencies().is_empty() => HashMap::new(),
        None => {
            let scanned = Library::scan(root);
            closure(&widget, &mut |id| scanned.widget(id))
        }
    };
    Ok(LoadedWidget::new(Arc::new(widget), library))
}

/// The asset kind's loader.
pub fn load(source: &str) -> Result<pulsar_scenedb::AssetPayload, String> {
    profiling::profile_scope!("Widget::load");
    let loaded = load_from(&project_root(), source)?;
    Ok(Arc::new(loaded))
}

/// The asset id of the widget `path` names, declared so the world can load
/// it; zero for an empty path.
pub fn widget_asset_id(path: &str) -> pulsar_scenedb::AssetId {
    // How a component takes in what it names, registered before the first id
    // is made (SceneDB replaces a repeated registration).
    static BINDINGS: std::sync::Once = std::sync::Once::new();
    BINDINGS.call_once(crate::component::register_bindings);
    helio_component::asset_component::asset_source(path).map_or(
        pulsar_scenedb::AssetId::ZERO,
        |source| {
            helio_component::asset_component::declared_asset_id(WIDGET_ASSET_KIND, &source)
        },
    )
}

/// A changed `.widget` file reloads its asset.
pub fn sources_for_file(path: &Path) -> Vec<String> {
    helio_component::asset_component::asset_source(&path.to_string_lossy())
        .into_iter()
        .collect()
}

// ── Cooking ────────────────────────────────────────────────────────────────

/// The project file a widget's image reference names: a picker id looked up
/// in the project's index, or a path as written.
fn image_file(root: &Path, value: &str) -> Option<String> {
    let reference = AssetPath(value.to_string());
    let Some(id) = reference.image_ref() else {
        return reference.legacy_path().map(str::to_string);
    };
    let found = engine_fs::open_project_file_index(root)
        .ok()
        .flatten()?
        .path_for_asset_id_kind("image", id.as_str())
        .ok()
        .flatten()?;
    let relative = found.strip_prefix(root).unwrap_or(&found);
    Some(relative.to_string_lossy().replace('\\', "/"))
}

/// Where the packaged game holds the project file `relative`: the packer files
/// it under `assets/` (the project's own `assets/` folder flattened), as it
/// rewrites a level's references. A cooked widget names its files that way,
/// because the game reads a reference as written.
fn shipped_path(relative: &str) -> String {
    format!(
        "assets/{}",
        relative.strip_prefix("assets/").unwrap_or(relative)
    )
}

/// Cook a widget: its images become shipped paths, and it records the files
/// of the widgets it instances, so the packaged game needs no project scan.
/// Both are returned as dependencies, so the packer ships them.
///
/// Dependencies are reported as project paths (what the packer reads); the
/// widget itself names their [shipped paths](shipped_path).
pub fn cook(
    context: &plugin_editor_api::CookContext<'_>,
) -> Result<plugin_editor_api::file_types::CookOutput, String> {
    use plugin_editor_api::file_types::{CookOutput, CookedFile};
    let root = context.project_root;
    let (mut widget, raw) = read_widget(context.source_path)?;
    let mut dependencies = BTreeSet::new();

    // Images, in node properties and image parameters' defaults.
    let mut rewrite = |value: &mut Json| {
        let Some(text) = value.as_str().filter(|t| !t.trim().is_empty()) else {
            return;
        };
        match image_file(root, text) {
            Some(file) => {
                *value = json!(shipped_path(&file));
                dependencies.insert(file);
            }
            // Shipped as written; the packer reports it as not found.
            None => {
                dependencies.insert(text.to_string());
            }
        }
    };
    fn walk(node: &mut Node, rewrite: &mut dyn FnMut(&mut Json)) {
        if node.kind == Kind::Image {
            if let Some(source) = node.props.0.get_mut("source") {
                rewrite(source);
            }
        }
        for child in &mut node.children {
            walk(child, rewrite);
        }
    }
    walk(&mut widget.root, &mut rewrite);
    for param in &mut widget.parameters {
        if param.class == ValueClass::Image {
            rewrite(&mut param.default);
        }
    }

    // The instanced widgets, by id.
    let mut cooked = BTreeMap::new();
    if !widget.dependencies().is_empty() {
        let scanned = Library::scan(root);
        for id in widget.dependencies() {
            let Some(path) = scanned.path_of(&id) else {
                return Err(format!(
                    "instances widget {id}, which no file in the project has (or two files claim)"
                ));
            };
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            cooked.insert(id.as_str().to_string(), shipped_path(&relative));
            dependencies.insert(relative);
        }
    }
    let mut value = widget.to_json();
    if let Some(object) = value.as_object_mut() {
        if let Some(stale) = raw.get(COOKED_DEPENDENCIES).filter(|_| cooked.is_empty()) {
            object.insert(COOKED_DEPENDENCIES.into(), stale.clone());
        } else if !cooked.is_empty() {
            object.insert(COOKED_DEPENDENCIES.into(), json!(cooked));
        }
    }
    let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    Ok(CookOutput::new(
        vec![CookedFile {
            path: context.content_path.to_path_buf(),
            bytes,
        }],
        dependencies.into_iter().collect(),
    ))
}
