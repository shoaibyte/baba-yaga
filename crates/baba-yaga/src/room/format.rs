//! Plain-text rendering of findings for the room. No emoji, no markup.

use crate::scanner::Finding;

/// CVSS v3 qualitative rating for a base score.
pub fn severity_label(score: Option<f32>) -> &'static str {
    match score {
        None => "UNSCORED",
        Some(s) if s >= 9.0 => "CRITICAL",
        Some(s) if s >= 7.0 => "HIGH",
        Some(s) if s >= 4.0 => "MEDIUM",
        Some(s) if s > 0.0 => "LOW",
        Some(_) => "NONE",
    }
}

/// The first message of a finding's thread.
pub fn thread_opener(f: &Finding) -> String {
    let severity = match f.severity_score {
        Some(s) => format!("{} {:.1}", severity_label(Some(s)), s),
        None => severity_label(None).to_string(),
    };
    let mut lines = vec![
        format!(
            "[{severity}] {} in {} {} ({})",
            f.id, f.package, f.version, f.ecosystem
        ),
        format!("Repo: {}, {}", f.repo, f.source_path),
    ];
    if let Some(summary) = &f.summary {
        lines.push(format!("Summary: {summary}"));
    }
    lines.push(format!(
        "Fixed in: {}",
        f.fixed_version
            .as_deref()
            .unwrap_or("no fixed version published")
    ));
    lines.push(format!(
        "Reachable: {}",
        match f.reachable {
            Some(true) => "yes, the vulnerable code is called",
            Some(false) => "no, the vulnerable code is not called",
            None => "unknown",
        }
    ));
    if !f.aliases.is_empty() {
        lines.push(format!("Aliases: {}", f.aliases.join(", ")));
    }
    lines.push(String::new());
    lines.push(format!(
        "Reply in this thread with: approve, reject, or snooze {} <days>",
        f.id
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding() -> Finding {
        Finding {
            repo: "web".into(),
            ecosystem: "npm".into(),
            package: "lodash".into(),
            version: "4.17.11".into(),
            id: "GHSA-jf85-cpcp-j695".into(),
            aliases: vec!["CVE-2019-10744".into()],
            summary: Some("Prototype Pollution in lodash".into()),
            severity_score: Some(9.1),
            fixed_version: Some("4.17.12".into()),
            reachable: None,
            source_path: "frontend/package-lock.json".into(),
        }
    }

    #[test]
    fn renders_full_finding() {
        assert_eq!(
            thread_opener(&finding()),
            "[CRITICAL 9.1] GHSA-jf85-cpcp-j695 in lodash 4.17.11 (npm)\n\
             Repo: web, frontend/package-lock.json\n\
             Summary: Prototype Pollution in lodash\n\
             Fixed in: 4.17.12\n\
             Reachable: unknown\n\
             Aliases: CVE-2019-10744\n\
             \n\
             Reply in this thread with: approve, reject, or snooze GHSA-jf85-cpcp-j695 <days>"
        );
    }

    #[test]
    fn renders_sparse_finding() {
        let mut f = finding();
        f.summary = None;
        f.severity_score = None;
        f.fixed_version = None;
        f.reachable = Some(false);
        f.aliases.clear();
        let text = thread_opener(&f);
        assert!(text.starts_with("[UNSCORED] GHSA-jf85-cpcp-j695"));
        assert!(text.contains("Fixed in: no fixed version published"));
        assert!(text.contains("Reachable: no,"));
        assert!(!text.contains("Summary:"));
        assert!(!text.contains("Aliases:"));
        assert!(text.is_ascii());
    }

    #[test]
    fn severity_bands() {
        assert_eq!(severity_label(Some(10.0)), "CRITICAL");
        assert_eq!(severity_label(Some(7.0)), "HIGH");
        assert_eq!(severity_label(Some(6.9)), "MEDIUM");
        assert_eq!(severity_label(Some(0.1)), "LOW");
        assert_eq!(severity_label(Some(0.0)), "NONE");
    }
}
