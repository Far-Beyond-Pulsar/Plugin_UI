//! UI (`pulsar.ui`): the plugin's root crate, the one place it registers
//! what it contributes.
//!
//! The plugin's other crates hold the code and register nothing: `widget_asset`
//! (the `.widget` file, instances and parameters, layout, drawing commands,
//! hit testing), `widget_text` (runtime text), `helio-ui-api` (the primitive
//! rows), `helio-pass-ui` (the screen and world passes) and `widget_component`
//! (the component, the retained runtime widgets and their interaction).
//!
//! - [`runtime`]: what a game links.
//! - With the `editor` feature: the widget editor (the `.widget` file type and
//!   its WYSIWYG editor) in the `editor` modules. A game links none of it.
//!
//! The asset's modules are re-exported here (`model`, `library`, `resolve`,
//! `layout`, `display`).

#![recursion_limit = "512"]

// The plugin's descriptor (`pulsar-plugin.toml`): linking the crate installs
// the plugin; a project enables it or not (`pulsar_plugins`).
pulsar_plugins::installed!("pulsar-plugin.toml", build);

pub use widget_asset::{display, layout, library, model, resolve};
pub use widget_component;

/// The plugin's example widgets, served as `examples/` in its folder.
mod examples;
mod event_source;
pub mod runtime;

#[cfg(feature = "editor")]
mod editor;
#[cfg(feature = "editor")]
mod editor_plugin;
#[cfg(feature = "editor")]
mod file_types;
#[cfg(feature = "editor")]
pub use editor_plugin::*;

/// The plugin's editor provider, registered by `build` (origin-gated).
#[cfg(all(feature = "editor", feature = "builtin"))]
mod builtin_provider;

#[cfg(all(test, feature = "editor"))]
mod render_tests;

/// Everything the plugin registers (runtime always; the editor provider with
/// `editor` and `builtin`).
pub fn build(p: &mut pulsar_plugins::PluginBuilder) {
    // The icon the editor shows beside this plugin (Extensions window).
    #[cfg(feature = "editor")]
    p.add(pulsar_plugins::PluginIcon {
        origin: p.origin(),
        format: pulsar_plugins::IconFormat::Png,
        bytes: include_bytes!("../icon.png"),
    });
    examples::build(p);
    runtime::build(p);
    #[cfg(all(feature = "editor", feature = "builtin"))]
    builtin_provider::build(p);
}

// The plugin as a library of its own, loaded by the editor from `plugins/editor`
// (`cargo build -p <this package> --release --features dll`).
#[cfg(feature = "dll")]
plugin_editor_api::export_plugin_build!();
