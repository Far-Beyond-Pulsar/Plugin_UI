//! Runtime widgets (`pulsar.ui`): widgets a game draws.
//!
//! A widget on screen or in the world is an object with a
//! [`WidgetComponent`]: the component reference is the handle a script holds.
//!
//! | Module | What |
//! |--------|------|
//! | [`component`] | the component: the asset it shows, where, its parameter values, script methods and the `on_action` event |
//! | [`assets`] | the `.widget` file as a world asset (loaded off the frame thread) and its cook handler |
//! | [`runtime`] | the retained widgets of a world: what changed, and the per-view laid-out trees |
//! | [`paint`] | a laid-out widget's drawing commands as UI primitives |
//! | [`host`] | each renderer's part: lays the widgets out for its view and publishes the frame the UI passes draw |
//! | [`interaction`] | hover, press and click, from the engine's input events |
//! | [`events`] | each button's own events (`widget::<widget>.<button>.<phase>`) |
//!
//! This crate registers nothing but its type's attachments; the plugin's root
//! submits the class, the feature, the loader, the host part and the rest.

pub mod assets;
pub mod component;
pub mod events;
pub mod host;
pub mod interaction;
pub mod paint;
pub mod report;
pub mod runtime;

pub use component::{WidgetAction, WidgetAssetPath, WidgetComponent, WidgetParams};

use std::sync::{Arc, OnceLock};

/// The process's runtime text system: fonts, shaping and the glyph atlas.
/// Static: one atlas serves every world and renderer of the process, and the
/// loaders, scripts and render threads that reach it have no context to carry
/// it in.
pub fn text_system() -> &'static Arc<widget_text::TextSystem> {
    static TEXT: OnceLock<Arc<widget_text::TextSystem>> = OnceLock::new();
    TEXT.get_or_init(|| Arc::new(widget_text::TextSystem::new()))
}

/// The UI feature: the screen and world passes.
pub fn render_features() -> helio_feature_api::FeatureSet {
    helio_feature_api::FeatureSet::new().with(helio_pass_ui::UiFeature)
}

