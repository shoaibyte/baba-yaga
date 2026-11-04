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

/// All findings for one package in one repository: the unit that gets a
/// thread in the room.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindingGroup {
    pub repo: String,
    pub ecosystem: String,
    pub package: String,
    /// The installed version. If a package appears at several versions in
    /// one repo (two manifests), this is the first one seen.
    pub version: String,
    pub findings: Vec<Finding>,
    /// The highest `fixed_version` across the findings: upgrading to it
    /// clears all of them, as far as the comparison below can tell.
    pub highest_fixed: Option<String>,
    pub max_severity: Option<f32>,
    /// `Some(true)` if any finding is reachable, `Some(false)` if at least
    /// one was analysed and none is, `None` if none was analysed.
    pub any_reachable: Option<bool>,
}

// Used by state v2, the next commit; remove this allow there.
#[allow(dead_code)]
impl FindingGroup {
    /// `repo:ecosystem:package`, the state key.
    pub fn key(&self) -> String {
        format!("{}:{}:{}", self.repo, self.ecosystem, self.package)
    }

    /// The advisory ids, sorted and without duplicates.
    pub fn vuln_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.findings.iter().map(|f| f.id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

/// Group findings by (repo, ecosystem, package), sorted by that key.
/// Findings inside a group keep their scan order, highest severity first.
pub fn group(findings: Vec<Finding>) -> Vec<FindingGroup> {
    let mut by_key: std::collections::BTreeMap<(String, String, String), Vec<Finding>> =
        std::collections::BTreeMap::new();
    for f in findings {
        by_key
            .entry((f.repo.clone(), f.ecosystem.clone(), f.package.clone()))
            .or_default()
            .push(f);
    }
    by_key
        .into_iter()
        .map(|((repo, ecosystem, package), mut findings)| {
            // Stable sort: equal scores keep scan order. Unscored sort last.
            findings.sort_by(|a, b| {
                b.severity_score
                    .unwrap_or(-1.0)
                    .total_cmp(&a.severity_score.unwrap_or(-1.0))
            });
            let version = findings
                .first()
                .map(|f| f.version.clone())
                .unwrap_or_default();
            let highest_fixed =
                highest_version(findings.iter().filter_map(|f| f.fixed_version.as_deref()));
            let max_severity = findings
                .iter()
                .filter_map(|f| f.severity_score)
                .max_by(f32::total_cmp);
            let any_reachable = if findings.iter().any(|f| f.reachable == Some(true)) {
                Some(true)
            } else if findings.iter().any(|f| f.reachable == Some(false)) {
                Some(false)
            } else {
                None
            };
            FindingGroup {
                repo,
                ecosystem,
                package,
                version,
                findings,
                highest_fixed,
                max_severity,
                any_reachable,
            }
        })
        .collect()
}

/// The highest of `versions` by a semver-ish comparison. Approximate: it
/// splits on '.', compares the leading digits of each part numerically and
/// the rest as text, and ignores a leading `v`. It knows nothing about
/// pre-release tags or ecosystem rules, so `1.0.0-rc1` vs `1.0.0` or PEP 440
/// epochs can come out wrong. Git commit hashes, which OSV lists as fixes
/// for some PyPI packages, are used only when nothing else is available.
pub fn highest_version<'a>(versions: impl Iterator<Item = &'a str>) -> Option<String> {
    let all: Vec<&str> = versions.collect();
    let looks_like_version = |v: &&str| {
        v.trim_start_matches('v')
            .starts_with(|c: char| c.is_ascii_digit())
            && !is_commit_hash(v)
    };
    let candidates: Vec<&str> = if all.iter().any(looks_like_version) {
        all.into_iter().filter(looks_like_version).collect()
    } else {
        all
    };
    candidates
        .into_iter()
        .max_by(|a, b| compare_versions(a, b))
        .map(str::to_string)
}

/// Seven or more hex digits with at least one letter and no dot: an
/// abbreviated or full git commit id, even when it starts with a digit.
fn is_commit_hash(v: &str) -> bool {
    v.len() >= 7
        && v.chars().all(|c| c.is_ascii_hexdigit())
        && v.chars().any(|c| c.is_ascii_alphabetic())
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |v: &str| -> Vec<(u64, String)> {
        v.trim_start_matches('v')
            .split('.')
            .map(|p| {
                let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
                let rest = p[digits.len()..].to_string();
                (digits.parse().unwrap_or(0), rest)
            })
            .collect()
    };
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).cloned().unwrap_or((0, String::new()));
        let y = pb.get(i).cloned().unwrap_or((0, String::new()));
        let ord = x.0.cmp(&y.0).then_with(|| x.1.cmp(&y.1));
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(package: &str, id: &str, score: Option<f32>, fixed: Option<&str>) -> Finding {
        Finding {
            repo: "web".into(),
            ecosystem: "npm".into(),
            package: package.into(),
            version: "1.0.0".into(),
            id: id.into(),
            aliases: vec![],
            summary: None,
            severity_score: score,
            fixed_version: fixed.map(Into::into),
            reachable: None,
            source_path: "package-lock.json".into(),
        }
    }

    #[test]
    fn groups_three_findings_into_two_packages() {
        let groups = group(vec![
            f("lodash", "GHSA-a", Some(5.0), Some("4.17.19")),
            f("axios", "GHSA-b", Some(7.5), Some("0.21.2")),
            f("lodash", "GHSA-c", Some(9.1), Some("4.17.21")),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].package, "axios");
        let lodash = &groups[1];
        assert_eq!(lodash.key(), "web:npm:lodash");
        assert_eq!(lodash.vuln_ids(), vec!["GHSA-a", "GHSA-c"]);
        assert_eq!(lodash.findings[0].id, "GHSA-c", "highest severity first");
        assert_eq!(lodash.highest_fixed.as_deref(), Some("4.17.21"));
        assert_eq!(lodash.max_severity, Some(9.1));
        assert_eq!(lodash.any_reachable, None);
    }

    #[test]
    fn reachability_rolls_up() {
        let mut a = f("x", "1", None, None);
        let mut b = f("x", "2", None, None);
        a.reachable = Some(false);
        b.reachable = Some(false);
        assert_eq!(
            group(vec![a.clone(), b.clone()])[0].any_reachable,
            Some(false)
        );
        b.reachable = Some(true);
        assert_eq!(group(vec![a, b])[0].any_reachable, Some(true));
    }

    #[test]
    fn highest_fixed_ordering() {
        let h = |v: &[&str]| highest_version(v.iter().copied());
        // Numeric, not lexical: 0.62.0 > 0.7.0 > 0.13.0 is wrong lexically.
        assert_eq!(h(&["0.7.0", "0.62.0", "0.13.0"]).as_deref(), Some("0.62.0"));
        assert_eq!(
            h(&["4.17.19", "4.17.21", "4.18.0"]).as_deref(),
            Some("4.18.0")
        );
        // Go pseudo-versions lose to real releases.
        assert_eq!(
            h(&["0.0.0-20220906165146-f3363e06e74c", "0.4.0"]).as_deref(),
            Some("0.4.0")
        );
        // Leading v ignored; mixed v and no v compare by number.
        assert_eq!(
            h(&["v5.4.46", "6.4.46", "5.4.50"]).as_deref(),
            Some("6.4.46")
        );
        // Commit hashes lose to any version...
        assert_eq!(
            h(&[
                "bd8153872e9c6fc98f4023df9c2deaffea2fa463",
                "2023.7.22",
                "2022.12.7"
            ])
            .as_deref(),
            Some("2023.7.22")
        );
        // ...including one that starts with a digit (seen for requests).
        assert_eq!(
            h(&[
                "2.32.0",
                "74ea7cf7a6a27a4eeb2ae24e162bcc942a6706d5",
                "2.33.0"
            ])
            .as_deref(),
            Some("2.33.0")
        );
        // ...but are kept when there is nothing else.
        assert_eq!(
            h(&["1d365e17e10d72d0b7876316fc7b9ca0eebdd38d"]).as_deref(),
            Some("1d365e17e10d72d0b7876316fc7b9ca0eebdd38d")
        );
        assert_eq!(h(&[]), None);
    }
}
