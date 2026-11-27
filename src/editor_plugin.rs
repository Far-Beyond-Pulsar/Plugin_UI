//! The editor side of the plugin: the `.widget` file type and its editor
//! (the `editor` feature; a game links none of it).

use std::path::PathBuf;
use std::sync::Arc;

use gpui::*;
use plugin_editor_api::*;
use ui::dock::PanelView;

pub use crate::editor::EDITOR_ID as WIDGET_EDITOR_ID;
pub use crate::file_types::{WIDGET_EXT, WIDGET_ID};
use crate::{editor, file_types};

/// The widget editor plugin.
#[derive(Default)]
pub struct UiEditorPlugin;

impl EditorPlugin for UiEditorPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: PluginId::new("com.pulsar.ui-editor"),
            name: "Widget Editor".into(),
            version: "0.1.0".into(),
            author: "Pulsar Team".into(),
            description: "Widget assets and the visual editor for building user interfaces".into(),
        }
    }

    fn file_types(&self) -> Vec<FileTypeDefinition> {
        file_types::file_types()
    }

    fn editors(&self) -> Vec<EditorMetadata> {
        vec![EditorMetadata {
            id: EditorId::new(editor::EDITOR_ID),
            display_name: "Widget Editor".into(),
            supported_file_types: vec![FileTypeId::new(file_types::WIDGET_ID)],
        }]
    }

    fn on_load(&mut self) {
        tracing::info!("Widget editor plugin loaded");
    }
}

impl EditorPluginEditor for UiEditorPlugin {
    fn register_editors(&'static self, registry: &mut EditorFactoryRegistry) {
        // A dynamically loaded plugin gets the host's undo service through
        // the settings snapshot, so every view of a file shares one history.
        registry.register_fn_with_settings(
            EditorId::new(editor::EDITOR_ID),
            |path, settings, window, cx| {
                Self::create_editor(path, Some(&settings.services().undo), window, cx)
            },
        );
    }
}

impl EditorPluginStatusbar for UiEditorPlugin {}
impl EditorPluginAi for UiEditorPlugin {}
impl EditorPluginComponents for UiEditorPlugin {
    fn component_definitions(&self) -> Vec<ComponentDefinition> {
        Vec::new()
    }
}
impl UiEditorPlugin {
    /// Open `path` in a widget editor (for hosts that link this crate).
    /// `undo` is the host's undo service; with it, every view of a file
    /// shares one history.
    pub fn create_editor(
        path: PathBuf,
        undo: Option<&UndoService>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Arc<dyn PanelView>, PluginError> {
        let panel = cx.new(|cx| editor::WidgetEditor::new(path, undo, window, cx));
        Ok(Arc::new(panel))
    }
}

// Static built-ins are instantiated through the Rust API and must not emit the
// process-global dynamic loader symbols shared by every plugin.
#[cfg(not(feature = "builtin"))]
export_plugin!(UiEditorPlugin);

#[cfg(test)]
mod tests {
    // Explicit imports: `super::*` would pull in gpui's own `test` macro.
    use super::UiEditorPlugin;
    use plugin_editor_api::EditorPlugin;

    #[test]
    fn the_plugin_registers_one_file_type_and_the_editor_for_it() {
        let plugin = UiEditorPlugin;
        let types = plugin.file_types();
        let editors = plugin.editors();
        assert_eq!(editors.len(), 1);
        assert!(editors[0].supported_file_types.contains(&types[0].id));
    }
}

