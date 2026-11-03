//! Dependency scanning: check out a repository and run osv-scanner on it.

pub mod finding;
pub mod osv;

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::config::Repo;
pub use finding::Finding;

/// Scan one configured repository. A `url` that is a local directory is
/// scanned in place; anything else is shallow-cloned into a temp directory.
/// `call_analysis` asks osv-scanner for Go reachability when `go` is on PATH.
pub fn scan_repo(repo: &Repo, call_analysis: bool) -> Result<Vec<Finding>> {
    let local = Path::new(&repo.url);
    if local.is_dir() {
        return osv::scan_dir(&repo.name, local, call_analysis);
    }
    let checkout = tempfile::tempdir().context("failed to create checkout directory")?;
    clone(repo, checkout.path())?;
    osv::scan_dir(&repo.name, checkout.path(), call_analysis)
}

fn clone(repo: &Repo, dest: &Path) -> Result<()> {
    tracing::info!(repo = %repo.name, url = %repo.url, "cloning");
    let mut cmd = Command::new("git");
    cmd.args(["clone", "--depth", "1", "--quiet"]);
    if let Some(branch) = &repo.branch {
        cmd.args(["--branch", branch]);
    }
    let output = cmd
        .arg(&repo.url)
        .arg(dest)
        .output()
        .context("failed to run git")?;
    if !output.status.success() {
        bail!(
            "git clone of {} failed: {}",
            repo.name,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Keep findings at or above `min`. Findings without a score are kept, so an
/// unscored advisory is never hidden.
pub fn filter_severity(findings: Vec<Finding>, min: Option<f32>) -> Vec<Finding> {
    match min {
        None => findings,
        Some(min) => findings
            .into_iter()
            .filter(|f| f.severity_score.is_none_or(|s| s >= min))
            .collect(),
    }
}
