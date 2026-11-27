//! The events a `WidgetComponent` offers because of the widget it names: one
//! set per button of that `.widget`, instanced widgets' buttons included.
//! The scripting editor draws an entry point from each
//! ([`engine_backend::AssetEventSource`]); how they are named, grouped and
//! described is decided here.

use engine_backend::{CatalogEvent, ComponentEventCatalog, EventView, SourceCx};
use widget_asset::library::Library;
use widget_asset::model::{Asset, WidgetAsset};
use widget_component::assets::WIDGET_ASSET_KIND;
use widget_component::events::{self, ButtonPhase};

/// The widget asset kind's source.
pub(crate) fn source(origin: &'static str) -> engine_backend::AssetEventSource {
    engine_backend::AssetEventSource {
        origin,
        kind: WIDGET_ASSET_KIND,
        declare,
        node_title: Some(node_title),
    }
}

/// `WidgetComponent.widget_asset` is a path, so the plugin says what it names.
pub(crate) fn slot(origin: &'static str) -> engine_backend::AssetPathSlot {
    engine_backend::AssetPathSlot {
        origin,
        class_name: "WidgetComponent",
        property: "widget_asset",
        kind: WIDGET_ASSET_KIND,
    }
}

/// The title of a saved button event node, when the widget is not at hand.
fn node_title(event: &str) -> Option<String> {
    let rest = event.strip_prefix("widget::")?;
    let phase = ButtonPhase::from_key(rest.rsplit('.').next()?)?;
    Some(format!("On Button {}", phase.label()))
}

fn declare(cx: &mut SourceCx<'_>, asset: &str) -> ComponentEventCatalog {
    let mut catalog = ComponentEventCatalog::default();
    let Some(path) = cx.resolve(WIDGET_ASSET_KIND, asset) else {
        return catalog;
    };
    let widget = match cx
        .read_to_string(&path)
        .and_then(|text| WidgetAsset::parse(&text).map_err(|error| error.to_string()))
    {
        Ok((widget, _notes)) => widget,
        Err(error) => {
            catalog.diagnostics.push((path, error));
            return catalog;
        }
    };
    // Only a widget that instances others needs the project's widgets, and
    // then any of their files can change its buttons.
    let library = if widget.dependencies().is_empty() {
        Library::empty(cx.root().to_path_buf())
    } else {
        cx.watch_extension(widget_asset::library::WIDGET_EXTENSION);
        Library::scan(cx.root())
    };
    catalog.events = present(&widget, &events::buttons(&widget, &library));
    catalog
}

/// How each button's events are filed and described.
fn present(widget: &WidgetAsset, buttons: &[events::ButtonDecl]) -> Vec<CatalogEvent> {
    let mut presented = Vec::new();
    for button in buttons {
        // Two buttons of one name (an instanced widget used twice) are told
        // apart by their ids.
        let shared = buttons.iter().filter(|b| b.name == button.name).count() > 1;
        let name = if shared {
            format!("{} {}", button.name, events::button_id(&button.key))
        } else {
            button.name.clone()
        };
        for phase in ButtonPhase::ALL {
            let full = events::event_name(&widget.id, &button.key, phase);
            presented.push(CatalogEvent {
                label: format!("{name} {}", phase.label()),
                view: EventView {
                    group: format!("Buttons/{name}"),
                    description: format!(
                        "Runs when the {name} button of this widget is {}.",
                        phase.key()
                    ),
                    documentation: format!(
                        "`{full}`: raised by the widget component that shows the widget, on \
                         the component's and its object's channels. Connect `component_ref` \
                         to choose the component when the class has more than one."
                    ),
                    properties: vec![("widget_button".to_owned(), name.clone())],
                },
                descriptor: events::descriptor(&widget.id, &button.key, phase),
            });
        }
    }
    presented
}

#[cfg(test)]
mod tests {
    use super::*;
    use widget_asset::model::{Kind, Node};

    fn widget_with_buttons() -> WidgetAsset {
        let mut widget = WidgetAsset::new();
        for name in ["Play", "Quit"] {
            let id = widget.alloc_id();
            widget.root.children.push(Node::new(id, name, Kind::Button));
        }
        widget
    }

    #[test]
    fn every_button_of_the_widget_gets_its_own_events() {
        let widget = widget_with_buttons();
        let library = Library::empty(std::path::PathBuf::new());
        let offered = present(&widget, &events::buttons(&widget, &library));
        assert_eq!(offered.len(), 2 * ButtonPhase::ALL.len());
        let play = offered.iter().find(|e| e.label == "Play Clicked").unwrap();
        let quit = offered.iter().find(|e| e.label == "Quit Clicked").unwrap();
        assert_ne!(play.descriptor.name, quit.descriptor.name);
        assert_eq!(play.view.group, "Buttons/Play");
    }

    #[test]
    fn renaming_a_button_keeps_its_events() {
        let mut widget = widget_with_buttons();
        let library = Library::empty(std::path::PathBuf::new());
        let before = events::buttons(&widget, &library);
        widget.root.children[0].name = "Start".into();
        let after = events::buttons(&widget, &library);
        assert_eq!(before[0].key, after[0].key);
    }

    #[test]
    fn a_saved_node_is_titled_by_what_it_listens_to() {
        assert_eq!(
            node_title("widget::abc.1-3.clicked").as_deref(),
            Some("On Button Clicked")
        );
        assert_eq!(node_title("montage::m1.hit"), None);
    }
}
