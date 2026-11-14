/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "devkit.toml";
const LOCAL_FILE: &str = "devkit.local.toml";

#[derive(Serialize, Deserialize, Default)]
struct Local {
    #[serde(default)]
    targets: BTreeMap<String, LocalTarget>,
}

#[derive(Serialize, Deserialize)]
struct LocalTarget {
    device: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    default: Option<String>,
    #[serde(default)]
    targets: BTreeMap<String, Target>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Target {
    /// An ESP-IDF project.
    Idf {
        dir: Option<String>,
        #[serde(rename = "flash-baud")]
        flash_baud: Option<u32>,
        #[serde(rename = "monitor-baud")]
        monitor_baud: Option<u32>,
    },
    /// A simulator build, run under the debug host.
    Sim {
        dir: Option<String>,
        /// CMake target to build instead of the simulator, which is then not run.
        build: Option<String>,
        /// false: the firmware draws its own window (the C bsp), not the host.
        window: Option<bool>,
        /// Run without any window, as `--headless` does.
        headless: Option<bool>,
    },
    /// Shell commands run in the project directory.
    Cmd { build: Option<String>, run: Option<String> },
}

pub struct Project {
    pub root: PathBuf,
    pub default: Option<String>,
    pub targets: BTreeMap<String, Target>,
}

impl Project {
    /// The nearest `devkit.toml` at or above `start`.
    pub fn find(start: &Path) -> Result<Self, String> {
        let start = start.canonicalize().map_err(|e| format!("{}: {e}", start.display()))?;
        let root = start
            .ancestors()
            .find(|d| d.join(FILE).is_file())
            .ok_or_else(|| format!("no {FILE} at or above {}", start.display()))?
            .to_path_buf();
        let text = std::fs::read_to_string(root.join(FILE)).map_err(|e| format!("{FILE}: {e}"))?;
        let file: File = toml::from_str(&text).map_err(|e| format!("{}: {e}", root.join(FILE).display()))?;
        Ok(Self { root, default: file.default, targets: file.targets })
    }

    pub fn name(&self) -> String {
        self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn target(&self, name: Option<&str>) -> Result<(String, Target), String> {
        let name = match name.or(self.default.as_deref()) {
            Some(n) => n.to_string(),
            None if self.targets.len() == 1 => self.targets.keys().next().unwrap().clone(),
            None => return Err(format!("no target given and no default; targets: {}", self.list())),
        };
        let target = self.targets.get(&name).ok_or_else(|| format!("unknown target {name}; targets: {}", self.list()))?;
        Ok((name, target.clone()))
    }

    pub fn dir(&self, name: &str, target: &Target) -> PathBuf {
        let dir = match target {
            Target::Idf { dir, .. } | Target::Sim { dir, .. } => dir.as_deref(),
            Target::Cmd { .. } => None,
        };
        match dir {
            Some(d) => self.root.join(d),
            None if matches!(target, Target::Cmd { .. }) => self.root.clone(),
            None => self.root.join(name),
        }
    }

    fn local(&self) -> Local {
        std::fs::read_to_string(self.root.join(LOCAL_FILE)).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    /// Devices pinned in `devkit.local.toml`, by target.
    pub fn pins(&self) -> BTreeMap<String, String> {
        self.local().targets.into_iter().map(|(k, v)| (k, v.device)).collect()
    }

    pub fn pin(&self, name: &str, device: Option<String>) -> Result<(), String> {
        let mut local = self.local();
        match device {
            Some(device) => local.targets.insert(name.to_string(), LocalTarget { device }),
            None => local.targets.remove(name),
        };
        let path = self.root.join(LOCAL_FILE);
        if local.targets.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", path.display())),
                _ => Ok(()),
            };
        }
        let text = toml::to_string(&local).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn list(&self) -> String {
        self.targets.keys().cloned().collect::<Vec<_>>().join(", ")
    }
}
