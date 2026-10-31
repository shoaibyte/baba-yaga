use serde::{Deserialize, Serialize};

/// One vulnerable package version in one repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub repo: String,
    /// OSV ecosystem name: `Go`, `PyPI`, `npm`, `Packagist`, ...
    pub ecosystem: String,
    pub package: String,
    pub version: String,
    /// OSV id of the representative advisory (GHSA-..., GO-..., PYSEC-...).
    pub id: String,
    /// The other ids osv-scanner grouped with `id`, CVEs included.
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub summary: Option<String>,
    /// Highest CVSS base score osv-scanner reported for the group.
    #[serde(default)]
    pub severity_score: Option<f32>,
    #[serde(default)]
    pub fixed_version: Option<String>,
    /// From osv-scanner call analysis; `None` when it did not run.
    #[serde(default)]
    pub reachable: Option<bool>,
    /// Manifest or lockfile, relative to the repository root.
    pub source_path: String,
}

impl Finding {
    pub fn dedup_key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.repo, self.ecosystem, self.package, self.id
        )
    }
}
