//! Parsing room replies into commands.
//!
//! Leading mentions (`@baba-yaga`) and a leading `/` or `!` are ignored, and
//! the verb is case-insensitive.

/// Days a `snooze` lasts when no count is given.
pub const DEFAULT_SNOOZE_DAYS: u32 = 7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Approve,
    Reject,
    Snooze { id: String, days: u32 },
    Status,
    Scan { repo: Option<String> },
    Unknown,
}

pub fn parse(body: &str) -> Command {
    let mut words = body
        .split_whitespace()
        .skip_while(|w| w.starts_with('@'))
        .map(|w| w.trim_end_matches([',', ':']));
    let Some(verb) = words.next() else {
        return Command::Unknown;
    };
    let verb = verb.trim_start_matches(['/', '!']).to_ascii_lowercase();
    let rest: Vec<&str> = words.collect();

    match (verb.as_str(), rest.as_slice()) {
        ("approve", []) => Command::Approve,
        ("reject", []) => Command::Reject,
        ("status", []) => Command::Status,
        ("scan", []) => Command::Scan { repo: None },
        ("scan", [repo]) => Command::Scan {
            repo: Some((*repo).to_string()),
        },
        ("snooze", [id]) => Command::Snooze {
            id: (*id).to_string(),
            days: DEFAULT_SNOOZE_DAYS,
        },
        ("snooze", [id, days]) => match parse_days(days) {
            Some(days) => Command::Snooze {
                id: (*id).to_string(),
                days,
            },
            None => Command::Unknown,
        },
        _ => Command::Unknown,
    }
}

/// `14` or `14d`; zero is rejected.
fn parse_days(s: &str) -> Option<u32> {
    let n: u32 = s.strip_suffix(['d', 'D']).unwrap_or(s).parse().ok()?;
    (n > 0).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_verbs() {
        assert_eq!(parse("approve"), Command::Approve);
        assert_eq!(parse("  Reject "), Command::Reject);
        assert_eq!(parse("/status"), Command::Status);
        assert_eq!(parse("!approve"), Command::Approve);
    }

    #[test]
    fn mentions_are_skipped() {
        assert_eq!(parse("@baba-yaga approve"), Command::Approve);
        assert_eq!(parse("@baba-yaga: @fixer, status"), Command::Status);
    }

    #[test]
    fn scan() {
        assert_eq!(parse("scan"), Command::Scan { repo: None });
        assert_eq!(
            parse("scan web"),
            Command::Scan {
                repo: Some("web".into())
            }
        );
        assert_eq!(parse("scan web api"), Command::Unknown);
    }

    #[test]
    fn snooze() {
        let s = |id: &str, days| Command::Snooze {
            id: id.into(),
            days,
        };
        assert_eq!(parse("snooze GHSA-1"), s("GHSA-1", DEFAULT_SNOOZE_DAYS));
        assert_eq!(parse("snooze GHSA-1 14"), s("GHSA-1", 14));
        assert_eq!(parse("snooze GHSA-1 30d"), s("GHSA-1", 30));
        assert_eq!(parse("snooze GHSA-1 0"), Command::Unknown);
        assert_eq!(parse("snooze GHSA-1 soon"), Command::Unknown);
        assert_eq!(parse("snooze"), Command::Unknown);
    }

    #[test]
    fn unknown() {
        assert_eq!(parse(""), Command::Unknown);
        assert_eq!(parse("@baba-yaga"), Command::Unknown);
        assert_eq!(parse("approve this please"), Command::Unknown);
        assert_eq!(parse("hello"), Command::Unknown);
    }
}
