//! The SDK boundary (`boundary/<sdk>.toml`, M8): how each library's
//! functions run, with per-object overrides. See the file's own comments.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// Adapters for validated objects; the rest guest code.
    Adapter,
    /// Guest code over the runtime's register-level devices (D40).
    Device,
    /// Guest code only.
    Guest,
}

#[derive(Debug, Clone, Deserialize)]
struct Entry {
    policy: Policy,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Boundary {
    #[serde(default)]
    library: BTreeMap<String, Entry>,
    #[serde(default)]
    object: BTreeMap<String, Entry>,
    /// The structs adapters touch, for `psxw layout`.
    pub layout: Option<LayoutSection>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LayoutSection {
    /// SDK headers, under the game's probe include dir.
    pub headers: Vec<String>,
    pub structs: Vec<String>,
    /// The generated Rust module, relative to the workspace.
    pub output: String,
}

impl Boundary {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// The policy for `<library>/<member>`: its object entry, else its
    /// library's.
    pub fn policy(&self, member: &str) -> Option<Policy> {
        if let Some(e) = self.object.get(member) {
            return Some(e.policy);
        }
        let lib = member.split('/').next()?;
        self.library.get(lib).map(|e| e.policy)
    }

    /// Libraries with policy `device`.
    pub fn device_libraries(&self) -> Vec<String> {
        self.library.iter().filter(|(_, e)| e.policy == Policy::Device).map(|(l, _)| l.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_overrides_library() {
        let b: Boundary = toml::from_str(
            r#"
            [library.libgpu]
            policy = "adapter"
            [library.libpad]
            policy = "device"
            [object."libgpu/font"]
            policy = "guest"
            "#,
        )
        .unwrap();
        assert_eq!(b.policy("libgpu/sys"), Some(Policy::Adapter));
        assert_eq!(b.policy("libgpu/font"), Some(Policy::Guest));
        assert_eq!(b.policy("libsnd/play"), None);
        assert_eq!(b.device_libraries(), ["libpad"]);
    }
}
