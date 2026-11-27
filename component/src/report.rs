//! Telling a person what is wrong with a widget: each problem once, in the
//! log and in the Problems panel.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use pulsar_findings::{Category, Finding, Severity};

const SOURCE: &str = "Widgets";

fn reported() -> &'static Mutex<BTreeMap<String, Finding>> {
    // Static: "once" is a property of the process; the reporters (script
    // methods, loaders, render hosts) share no context.
    static REPORTED: OnceLock<Mutex<BTreeMap<String, Finding>>> = OnceLock::new();
    REPORTED.get_or_init(Default::default)
}

/// Report `title` about `subject` once per `key`. Returns whether this was the
/// first time.
pub fn once(key: String, subject: &str, title: String, detail: &str) -> bool {
    let mut reported = reported()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if reported.contains_key(&key) {
        return false;
    }
    tracing::warn!("widget {subject}: {title}");
    let finding = Finding::new(key.clone(), Category::Project, Severity::Warning, title)
        .detail(detail)
        .subject(subject);
    reported.insert(key, finding);
    pulsar_findings::publish(SOURCE, reported.values().cloned().collect());
    true
}

/// Forget the report under `key` (what it said is no longer so).
pub fn clear(key: &str) {
    let mut reported = reported()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if reported.remove(key).is_some() {
        pulsar_findings::publish(SOURCE, reported.values().cloned().collect());
    }
}

/// The titles reported so far (tests).
pub fn titles() -> Vec<String> {
    reported()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .map(|f| f.title.clone())
        .collect()
}
