//! External tool plugins.
//!
//! A plugin correlates one external tool's state with the compiled packet.
//! Plugins are synchronous subprocess adapters: they observe during an
//! explicitly-invoked command, may contribute session identity, and render one
//! packet section. They hold no state of their own, spawn nothing that
//! outlives the command, and make no network calls. An absent tool disables
//! its plugin silently — plugins are enhancements Toimija detects, never
//! requirements it carries.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;

mod teko;

/// What a plugin is, stated once. `name` keys the plugin's observation in the
/// packet's `session.plugins` map and must never change across releases.
pub struct PluginMeta {
    pub name: &'static str,
    pub description: &'static str,
    /// External command the plugin consumes; its absence disables the plugin.
    pub command: &'static str,
}

/// Session identity a plugin derives from its observation: an authoritative
/// intent and the write scope it implies.
pub struct Identity {
    pub intent: String,
    pub scope: Vec<String>,
}

pub trait Plugin: Sync {
    fn meta(&self) -> PluginMeta;

    /// Observe the tool's state for this repository. `Ok(None)` when the tool
    /// is absent or reports nothing; `Err` only when the tool answered with a
    /// broken contract.
    fn observe(&self, repo_root: &Path) -> Result<Option<Value>>;

    /// Identity derived from an observation, when the tool is authoritative
    /// for what this session is doing.
    fn identity(&self, observation: &Value) -> Option<Identity>;

    /// Markdown section rendered into the agent's entry packet.
    fn render(&self, observation: &Value) -> String;
}

/// Every built-in plugin, in deterministic registry order.
pub fn all() -> &'static [&'static dyn Plugin] {
    static TEKO: teko::TekoPlugin = teko::TekoPlugin;
    static REGISTRY: [&dyn Plugin; 1] = [&TEKO];
    &REGISTRY
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn registry_metadata_is_stable_and_unique() {
        let mut names = BTreeSet::new();
        for plugin in all() {
            let meta = plugin.meta();
            assert!(!meta.name.is_empty());
            assert!(!meta.description.is_empty());
            assert!(!meta.command.is_empty());
            assert!(
                names.insert(meta.name),
                "duplicate plugin name `{}`",
                meta.name
            );
        }
        assert!(names.contains("teko"));
    }
}
