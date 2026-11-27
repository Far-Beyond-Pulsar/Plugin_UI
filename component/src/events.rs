//! The events of a widget's buttons: every button of the widget a component
//! shows raises its own events, so a script binds each one separately.
//!
//! Which events a component offers depends on the `.widget` asset it names
//! (the root plugin registers the asset kind's source for the editor, see
//! `ui_editor_plugin`). Their names are
//! `widget::<widget id>.<button>.<phase>`: the widget's id and the button's
//! node ids, so renaming the file, the button or its `action` breaks no
//! binding. They are delivered on the component's channel and its object's,
//! as every component event is. A session declares the events of every
//! widget of the project before scripts link ([`declare_project_events`]).

use std::path::Path;

use pulsar_events::gamma::EventDescriptor;
use pulsar_events::{EventCategory, EventHub};
use pulsar_scenedb::{Entity, World};
use widget_asset::library::Library;
use widget_asset::model::schema::Kind;
use widget_asset::model::validate::WidgetSource;
use widget_asset::model::{NodeId, WidgetAsset, WidgetId};

/// The namespace of button events.
pub const NAMESPACE: &str = "widget";

/// What happens to a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonPhase {
    /// Pressed and released over the button.
    Clicked,
    Pressed,
    /// The press ended, over the button or not.
    Released,
    /// The pointer came over the button.
    Hovered,
    Unhovered,
}

impl ButtonPhase {
    pub const ALL: [ButtonPhase; 5] = [
        Self::Clicked,
        Self::Pressed,
        Self::Released,
        Self::Hovered,
        Self::Unhovered,
    ];

    /// The last part of the event's name.
    pub fn key(self) -> &'static str {
        match self {
            Self::Clicked => "clicked",
            Self::Pressed => "pressed",
            Self::Released => "released",
            Self::Hovered => "hovered",
            Self::Unhovered => "unhovered",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Clicked => "Clicked",
            Self::Pressed => "Pressed",
            Self::Released => "Released",
            Self::Hovered => "Hovered",
            Self::Unhovered => "Unhovered",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|phase| phase.key() == key)
    }
}

/// A button of a widget, instanced widgets' included.
#[derive(Clone, Debug, PartialEq)]
pub struct ButtonDecl {
    /// The chain of node ids from the widget down to the button.
    pub key: Vec<NodeId>,
    pub name: String,
}

/// Every button of `widget`, in tree order; `library` holds the widgets it
/// instances.
pub fn buttons(widget: &WidgetAsset, library: &dyn WidgetSource) -> Vec<ButtonDecl> {
    let mut found = Vec::new();
    widget_asset::resolve::resolve(widget, library).walk(&mut |node| {
        if node.kind == Kind::Button {
            found.push(ButtonDecl {
                key: node.key.clone(),
                name: node.name.clone(),
            });
        }
    });
    found
}

/// A button's stable part of its event names.
pub fn button_id(key: &[NodeId]) -> String {
    key.iter()
        .map(|id| id.0.to_string())
        .collect::<Vec<_>>()
        .join("-")
}

/// The event's name without its namespace.
fn local_name(widget: &WidgetId, key: &[NodeId], phase: ButtonPhase) -> String {
    format!("{}.{}.{}", widget.as_str(), button_id(key), phase.key())
}

/// The name the event is published under.
pub fn event_name(widget: &WidgetId, key: &[NodeId], phase: ButtonPhase) -> String {
    format!("{NAMESPACE}::{}", local_name(widget, key, phase))
}

/// The event's schema. It carries no values: which button and what happened
/// are the event itself.
pub fn descriptor(widget: &WidgetId, key: &[NodeId], phase: ButtonPhase) -> EventDescriptor {
    EventDescriptor::dynamic_in(
        NAMESPACE,
        local_name(widget, key, phase),
        Vec::<(String, pulsar_events::gamma::FieldType)>::new(),
    )
    .expect("widget is a valid namespace")
}

/// Declare the button events of every widget of the project at `root` on
/// `hub` (idempotent), before any script links: a class can handle a button
/// of a widget that has not been shown yet. Returns how many buttons.
pub fn declare_project_events(hub: &EventHub, root: &Path) -> usize {
    profiling::profile_scope!("Widget::declare_project_events");
    let library = Library::scan(root);
    let mut count = 0;
    for entry in library.entries() {
        let widget = &entry.widget;
        for button in buttons(widget, &library) {
            count += 1;
            for phase in ButtonPhase::ALL {
                let declared = hub.register(
                    descriptor(&widget.id, &button.key, phase),
                    EventCategory::Custom("Widgets".into()),
                );
                if let Err(error) = declared {
                    tracing::warn!(%error, button = %button.name, "a button event could not be declared");
                }
            }
        }
    }
    count
}

/// Queue the button event of the widget component `instance`; it is delivered
/// at the next flush.
pub fn queue_button_event(
    world: &mut World,
    instance: Entity,
    widget: &WidgetId,
    key: &[NodeId],
    phase: ButtonPhase,
) {
    pulsar_world_registry::queue_component_event(
        world,
        instance,
        event_name(widget, key, phase),
        Vec::new(),
    );
}
