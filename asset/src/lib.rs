//! The `.widget` asset and everything about it that needs no UI toolkit, so
//! the widget editor and a game's runtime widgets share one implementation.
//!
//! | Module | What |
//! |--------|------|
//! | [`model`] | ids, the property schema, nodes, the asset file, editing operations, validation |
//! | [`library`] | the project's widgets by id: scanning, lookup, conflicts, usage |
//! | [`resolve`] | expanding instances and parameters into the tree that is drawn |
//! | [`layout`] | measure and arrange |
//! | [`display`] | the drawing commands, and hit testing |

pub mod display;
pub mod layout;
pub mod library;
pub mod model;
pub mod resolve;
