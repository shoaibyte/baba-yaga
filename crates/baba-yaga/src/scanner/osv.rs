//! Runs `osv-scanner` and turns its JSON report into findings.
//!
//! Only the fields baba-yaga reads are modelled; everything else in the
//! report is ignored.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::finding::Finding;

/// Run `osv-scanner scan source -r --format json <dir>` and parse the result.
pub fn scan_dir(repo: &str, dir: &Path) -> Result<Vec<Finding>> {
    tracing::info!(repo, dir = %dir.display(), "running osv-scanner");
    let output = Command::new("osv-scanner")
        .args(["scan", "source", "-r", "--format", "json"])
        .arg(dir)
        .output()
        .context("failed to run osv-scanner; is it installed and on PATH?")?;

    match output.status.code() {
        // 0: no vulnerabilities, 1: vulnerabilities found.
        Some(0) | Some(1) => {
            let stdout =
                String::from_utf8(output.stdout).context("osv-scanner output is not UTF-8")?;
            parse(&stdout, repo, dir)
        }
        Some(128) => {
            tracing::info!(repo, "osv-scanner found no packages");
            Ok(Vec::new())
        }
        code => bail!(
            "osv-scanner exited with {}: {}",
            code.map_or_else(|| "a signal".to_string(), |c| c.to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

/// Parse an osv-scanner JSON report. `root` is stripped from source paths.
///
/// osv-scanner groups advisories that describe the same issue (a GHSA, its
/// CVE, a GO- id). Each group becomes one finding, represented by the first
/// of its ids that appears in `vulnerabilities`.
pub fn parse(json: &str, repo: &str, root: &Path) -> Result<Vec<Finding>> {
    let report: Report = serde_json::from_str(json).context("invalid osv-scanner JSON")?;
    let mut findings = Vec::new();

    for result in report.results {
        let source_path = relative_path(&result.source.path, root);
        for pkg in result.packages {
            let vulns: HashMap<&str, &Vulnerability> = pkg
                .vulnerabilities
                .iter()
                .map(|v| (v.id.as_str(), v))
                .collect();
            let mut covered: HashSet<&str> = HashSet::new();

            for group in &pkg.groups {
                let Some(rep) = group.ids.iter().find_map(|id| vulns.get(id.as_str())) else {
                    continue;
                };
                let mut aliases: BTreeSet<String> =
                    group.ids.iter().chain(&group.aliases).cloned().collect();
                aliases.extend(rep.aliases.iter().cloned());
                aliases.remove(&rep.id);
                covered.extend(group.ids.iter().map(String::as_str));

                findings.push(build(
                    repo,
                    &source_path,
                    &pkg.package,
                    rep,
                    aliases.into_iter().collect(),
                    group
                        .max_severity
                        .as_deref()
                        .and_then(|s| s.trim().parse().ok()),
                    group.reachable(),
                ));
            }

            // Vulnerabilities osv-scanner left out of every group.
            for v in &pkg.vulnerabilities {
                if covered.contains(v.id.as_str()) {
                    continue;
                }
                findings.push(build(
                    repo,
                    &source_path,
                    &pkg.package,
                    v,
                    v.aliases.clone(),
                    None,
                    None,
                ));
            }
        }
    }
    Ok(findings)
}

fn build(
    repo: &str,
    source_path: &str,
    pkg: &PackageInfo,
    vuln: &Vulnerability,
    aliases: Vec<String>,
    severity_score: Option<f32>,
    reachable: Option<bool>,
) -> Finding {
    Finding {
        repo: repo.to_string(),
        ecosystem: pkg.ecosystem.clone(),
        package: pkg.name.clone(),
        version: pkg.version.clone(),
        id: vuln.id.clone(),
        aliases,
        summary: vuln.summary.clone().filter(|s| !s.is_empty()),
        severity_score,
        fixed_version: vuln.fixed_version(pkg),
        reachable,
        source_path: source_path.to_string(),
    }
}

fn relative_path(path: &str, root: &Path) -> String {
    Path::new(path)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

#[derive(Debug, Deserialize)]
struct Report {
    #[serde(default)]
    results: Vec<SourceResult>,
}

#[derive(Debug, Deserialize)]
struct SourceResult {
    source: Source,
    #[serde(default)]
    packages: Vec<PackageResult>,
}

#[derive(Debug, Deserialize)]
struct Source {
    path: String,
}

#[derive(Debug, Deserialize)]
struct PackageResult {
    package: PackageInfo,
    #[serde(default)]
    vulnerabilities: Vec<Vulnerability>,
    #[serde(default)]
    groups: Vec<Group>,
}

#[derive(Debug, Deserialize)]
struct PackageInfo {
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    ecosystem: String,
}

#[derive(Debug, Deserialize)]
struct Vulnerability {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    affected: Vec<Affected>,
}

impl Vulnerability {
    /// The first `fixed` event in a range affecting this package.
    fn fixed_version(&self, pkg: &PackageInfo) -> Option<String> {
        self.affected
            .iter()
            .filter(|a| {
                a.package
                    .as_ref()
                    .is_none_or(|p| p.name == pkg.name && p.ecosystem == pkg.ecosystem)
            })
            .flat_map(|a| &a.ranges)
            .flat_map(|r| &r.events)
            .find_map(|e| e.fixed.clone())
    }
}

#[derive(Debug, Deserialize)]
struct Affected {
    #[serde(default)]
    package: Option<AffectedPackage>,
    #[serde(default)]
    ranges: Vec<Range>,
}

#[derive(Debug, Deserialize)]
struct AffectedPackage {
    name: String,
    #[serde(default)]
    ecosystem: String,
}

#[derive(Debug, Deserialize)]
struct Range {
    #[serde(default)]
    events: Vec<Event>,
}

#[derive(Debug, Deserialize)]
struct Event {
    #[serde(default)]
    fixed: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Group {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    max_severity: Option<String>,
    #[serde(default)]
    experimental_analysis: HashMap<String, Analysis>,
}

impl Group {
    /// Reachable if any id in the group is called; unknown without analysis.
    fn reachable(&self) -> Option<bool> {
        let called: Vec<bool> = self
            .experimental_analysis
            .values()
            .filter_map(|a| a.called)
            .collect();
        if called.is_empty() {
            None
        } else {
            Some(called.into_iter().any(|c| c))
        }
    }
}

#[derive(Debug, Deserialize)]
struct Analysis {
    #[serde(default)]
    called: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../../tests/fixtures/osv-sample.json");

    fn findings() -> Vec<Finding> {
        parse(SAMPLE, "web", Path::new("/tmp/scan/web")).unwrap()
    }

    #[test]
    fn one_finding_per_group() {
        let f = findings();
        let ids: Vec<&str> = f.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["GO-2024-2611", "GHSA-jf85-cpcp-j695", "PYSEC-2023-0001"]
        );
    }

    #[test]
    fn go_finding_fields() {
        let f = &findings()[0];
        assert_eq!(f.ecosystem, "Go");
        assert_eq!(f.package, "google.golang.org/protobuf");
        assert_eq!(f.version, "1.32.0");
        assert_eq!(f.aliases, vec!["CVE-2024-24786", "GHSA-8r3f-844c-mc37"]);
        assert_eq!(f.severity_score, Some(5.9));
        assert_eq!(f.fixed_version.as_deref(), Some("1.33.0"));
        assert_eq!(f.reachable, Some(false));
        assert_eq!(f.source_path, "go.mod");
        assert_eq!(
            f.dedup_key(),
            "web:Go:google.golang.org/protobuf:GO-2024-2611"
        );
    }

    #[test]
    fn npm_finding_without_analysis() {
        let f = &findings()[1];
        assert_eq!(f.ecosystem, "npm");
        assert_eq!(f.package, "lodash");
        assert_eq!(f.severity_score, Some(9.1));
        assert_eq!(f.fixed_version.as_deref(), Some("4.17.12"));
        assert_eq!(f.reachable, None);
        assert_eq!(f.source_path, "frontend/package-lock.json");
    }

    #[test]
    fn ungrouped_vulnerability_is_kept() {
        let f = &findings()[2];
        assert_eq!(f.ecosystem, "PyPI");
        assert_eq!(f.severity_score, None);
        assert_eq!(f.summary, None);
        assert_eq!(f.source_path, "/elsewhere/requirements.txt");
    }

    /// Shape osv-scanner 2.7.0 emits with `--call-analysis=go`.
    #[test]
    fn call_analysis_maps_to_reachable() {
        let json = include_str!("../../tests/fixtures/osv-call-analysis.json");
        let f = parse(json, "svc", Path::new("/tmp/scan/svc")).unwrap();
        let got: Vec<(&str, Option<bool>)> =
            f.iter().map(|f| (f.id.as_str(), f.reachable)).collect();
        assert_eq!(
            got,
            vec![
                ("GO-2099-0001", Some(true)),
                ("GO-2099-0002", Some(false)),
                // No Go symbol data for a GHSA-only advisory: unknown.
                ("GHSA-dddd-eeee-ffff", None),
                // Any called id makes the group reachable.
                ("GO-2099-0004", Some(true)),
            ]
        );
        // An empty max_severity is no score, not a parse failure.
        assert_eq!(f[0].severity_score, None);
        assert_eq!(f[1].severity_score, Some(7.5));
    }

    #[test]
    fn empty_report() {
        assert!(parse(r#"{"results": []}"#, "web", Path::new("/"))
            .unwrap()
            .is_empty());
        assert!(parse("{}", "web", Path::new("/")).unwrap().is_empty());
    }
}
