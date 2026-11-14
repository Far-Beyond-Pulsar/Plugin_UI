//! The plugin's example widgets: a read-only `examples/` folder in the
//! plugin's virtual folder (`plugins/<plugin>/examples/`), served from files
//! embedded in the binary (`examples_assets/`). Each one is a complete,
//! valid `.widget` asset that teaches one part of the format; open them in
//! the widget editor, or copy one into the project to start from it.

use std::sync::Arc;

use engine_fs::{EmbeddedFile, FileProvider, FolderSource, InGame, MemoryFolder, PluginFolder};

macro_rules! example {
    ($name:literal) => {
        EmbeddedFile {
            path: concat!("examples/", $name),
            bytes: include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/examples_assets/",
                $name
            )),
        }
    };
}

/// Every embedded file, by its path inside the plugin's folder.
pub(crate) static EXAMPLES: &[EmbeddedFile] = &[
    example!("README.md"),
    example!("MainMenu.widget"),
    example!("HealthBar.widget"),
    example!("SettingsPanel.widget"),
    example!("DialogFrame.widget"),
    example!("ConfirmDialog.widget"),
    example!("HudScreen.widget"),
];

fn open(folder: PluginFolder) -> Arc<dyn FolderSource> {
    Arc::new(MemoryFolder::with_files(folder, EXAMPLES))
}

/// Registers the plugin's example files.
pub(crate) fn build(p: &mut pulsar_plugins::PluginBuilder) {
    p.add(FileProvider {
        origin: p.origin(),
        open,
        in_game: InGame::Snapshot,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::validate::WidgetSource;
    use crate::model::widget::WidgetAsset;
    use crate::model::{Asset, Severity, WidgetId};
    use std::collections::HashMap;

    fn widgets() -> Vec<(&'static str, &'static str)> {
        EXAMPLES
            .iter()
            .filter(|file| file.path.ends_with(".widget"))
            .map(|file| (file.path, std::str::from_utf8(file.bytes).expect("utf-8")))
            .collect()
    }

    #[test]
    fn every_example_is_listed_with_its_readme() {
        assert!(EXAMPLES.iter().any(|f| f.path == "examples/README.md"));
        assert!(widgets().len() >= 5, "several varied widgets");
        let readme = std::str::from_utf8(
            EXAMPLES
                .iter()
                .find(|f| f.path == "examples/README.md")
                .unwrap()
                .bytes,
        )
        .unwrap();
        for (path, _) in widgets() {
            let name = path.trim_start_matches("examples/");
            assert!(readme.contains(name), "README lists {name}");
        }
    }

    #[test]
    fn every_example_parses_cleanly_and_has_a_distinct_id() {
        let mut ids = std::collections::HashSet::new();
        for (path, text) in widgets() {
            let (widget, notes) = WidgetAsset::parse(text).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert!(notes.is_empty(), "{path}: the loader repaired it: {notes:?}");
            assert!(!widget.needs_stamp(), "{path} has a valid id");
            assert!(!widget.description.is_empty(), "{path} is described");
            assert!(ids.insert(widget.id.clone()), "{path}: duplicate id");
            assert!(widget.root.count() > 1, "{path} has content");
        }
    }

    #[test]
    fn every_example_validates_with_the_others_as_its_library() {
        let library: HashMap<WidgetId, Arc<WidgetAsset>> = widgets()
            .into_iter()
            .map(|(_, text)| {
                let widget = WidgetAsset::parse(text).unwrap().0;
                (widget.id.clone(), Arc::new(widget))
            })
            .collect();
        assert!(library.widget(&WidgetId::new()).is_none());
        for (path, text) in widgets() {
            let widget = WidgetAsset::parse(text).unwrap().0;
            let errors: Vec<_> = widget
                .validate(&library)
                .into_iter()
                .filter(|issue| issue.severity >= Severity::Warning)
                .collect();
            assert!(errors.is_empty(), "{path}: {errors:?}");
        }
    }

    #[test]
    fn examples_survive_a_save_and_reload() {
        for (path, text) in widgets() {
            let widget = WidgetAsset::parse(text).unwrap().0;
            let again = WidgetAsset::parse(&widget.to_text()).unwrap().0;
            assert_eq!(widget, again, "{path}");
        }
    }
}
