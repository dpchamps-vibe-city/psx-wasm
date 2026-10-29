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
    /// Decided per game for the whole library (D58): adapters when every
    /// placed object that defines an adapter's function is validated,
    /// otherwise the library's original code over the devices. Never a mix:
    /// a library's adapters keep their state in the runtime, which the
    /// library's original code can't see.
    #[serde(rename = "adapter-or-device")]
    AdapterOrDevice,
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
    /// Chip groups (D59): the libraries and objects that drive each chip.
    #[serde(default)]
    chip: BTreeMap<String, Chip>,
    /// The structs adapters touch, for `psxw layout`.
    pub layout: Option<LayoutSection>,
}

/// A chip group (D59): its `adapter-or-device` members are decided
/// together, native or on the chip's device.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Chip {
    /// `<library>` or `<library>/<member>`.
    pub members: Vec<String>,
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

    /// Libraries with policy `adapter`.
    pub fn adapter_libraries(&self) -> Vec<String> {
        self.library.iter().filter(|(_, e)| e.policy == Policy::Adapter).map(|(l, _)| l.clone()).collect()
    }

    pub fn chips(&self) -> &BTreeMap<String, Chip> {
        &self.chip
    }

    /// The chip group of `<library>/<member>`: one naming the object, else
    /// one naming its library.
    pub fn chip_of(&self, member: &str) -> Option<&str> {
        let lib = member.split('/').next()?;
        let named = |want: &str| self.chip.iter().find(|(_, c)| c.members.iter().any(|m| m == want)).map(|(n, _)| n.as_str());
        named(member).or_else(|| named(lib))
    }

    /// Libraries with policy `adapter-or-device`.
    pub fn adapter_or_device_libraries(&self) -> Vec<String> {
        self.library.iter().filter(|(_, e)| e.policy == Policy::AdapterOrDevice).map(|(l, _)| l.clone()).collect()
    }

    /// The libraries of `members` (`<library>/<member>`) with no policy,
    /// each with how many of its members are placed (M13): a library the
    /// boundary doesn't know must not silently default to adapters.
    pub fn unknown<'a>(&self, members: impl IntoIterator<Item = &'a str>) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for m in members {
            if self.policy(m).is_none() {
                *out.entry(m.split('/').next().unwrap_or(m).to_string()).or_default() += 1;
            }
        }
        out
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

    #[test]
    fn unknown_libraries_and_adapter_or_device() {
        let b: Boundary = toml::from_str(
            r#"
            [library.libcd]
            policy = "adapter-or-device"
            [object."libds/dsread"]
            policy = "device"
            "#,
        )
        .unwrap();
        assert_eq!(b.policy("libcd/bios_1"), Some(Policy::AdapterOrDevice));
        assert_eq!(b.adapter_or_device_libraries(), ["libcd"]);
        let unknown = b.unknown(["libcd/sys", "libds/dsread", "libds/dssys_1", "libds/dsfile", "libsn/op_vnew"]);
        assert_eq!(unknown.into_iter().collect::<Vec<_>>(), [("libds".to_string(), 2), ("libsn".to_string(), 1)]);
    }

    #[test]
    fn chip_groups() {
        let b: Boundary = toml::from_str(
            r#"
            [chip.irq]
            members = ["libetc", "libapi/counter"]
            [chip.cd]
            members = ["libcd", "libds"]
            "#,
        )
        .unwrap();
        assert_eq!(b.chip_of("libapi/counter"), Some("irq"));
        assert_eq!(b.chip_of("libapi/pad"), None);
        assert_eq!(b.chip_of("libds/dsread"), Some("cd"));
        assert_eq!(b.chips().len(), 2);
    }
}
