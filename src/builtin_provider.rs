//! The plugin's editor provider: its file types and editors, offered to
//! the editor while the plugin is enabled (`plugin_manager::PluginEditorProvider`).

use gpui::{App, Window};
use plugin_editor_api::*;
use plugin_manager::{BuiltinEditorProvider, EditorContext};
use std::path::PathBuf;
use std::sync::Arc;
use ui::dock::PanelView;

/// Wraps the ui_editor_plugin crate as a built-in provider: the `.widget`
/// file type and the WYSIWYG widget editor.
pub struct WidgetEditorBuiltinProvider;

impl BuiltinEditorProvider for WidgetEditorBuiltinProvider {
    fn provider_id(&self) -> &str {
        "com.pulsar.ui-editor"
    }

    fn file_types(&self) -> Vec<FileTypeDefinition> {
        crate::UiEditorPlugin.file_types()
    }

    fn editors(&self) -> Vec<EditorMetadata> {
        crate::UiEditorPlugin.editors()
    }

    fn can_handle(&self, editor_id: &EditorId) -> bool {
        editor_id.as_str() == crate::WIDGET_EDITOR_ID
    }

    fn create_editor(
        &self,
        file_path: PathBuf,
        editor_context: &EditorContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Arc<dyn PanelView>, PluginError> {
        crate::UiEditorPlugin::create_editor(
            file_path,
            Some(&editor_context.services.undo),
            window,
            cx,
        )
    }
}

plugin_manager::inventory::submit! {
    plugin_manager::PluginEditorProvider {
        origin: pulsar_plugins::origin!(),
        create: || Arc::new(WidgetEditorBuiltinProvider),
    }
}
