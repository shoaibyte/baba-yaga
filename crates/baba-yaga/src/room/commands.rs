//! Parsing addressed room messages into commands.
//!
//! Mentions (`@baba_yaga`) and punctuation are ignored, the verb is
//! case-insensitive, and the first known verb anywhere in the message wins,
//! so "please approve this" is `approve`. A negation before the verb ("don't
//! approve") makes the message `Unknown` rather than risk acting on it.

/// Days a `snooze` lasts when no duration is given.
pub const DEFAULT_SNOOZE_DAYS: u32 = 7;
/// Longest snooze accepted, so a typo cannot hide a finding for years.
pub const MAX_SNOOZE_DAYS: u32 = 365;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Approve,
    Reject,
    Snooze { days: u32 },
    Status,
    Scan { repo: Option<String> },
    Help,
    Unknown,
}

const NEGATIONS: [&str; 5] = ["not", "dont", "don't", "never", "no"];
/// Words after `scan` that are not a repo name.
const FILLER: [&str; 9] = [
    "please",
    "now",
    "again",
    "it",
    "this",
    "the",
    "all",
    "repos",
    "everything",
];

pub fn parse(body: &str) -> Command {
    let words: Vec<String> = body
        .split_whitespace()
        .filter(|w| !w.starts_with('@'))
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-' && c != '_')
                .trim_start_matches(['/', '!'])
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();

    for (i, word) in words.iter().enumerate() {
        let rest = &words[i + 1..];
        let command = match word.as_str() {
            "approve" | "approved" | "lgtm" => Command::Approve,
            "reject" | "rejected" => Command::Reject,
            "snooze" => match rest.iter().find_map(|w| parse_duration(w)) {
                Some(Ok(days)) => Command::Snooze { days },
                Some(Err(())) => Command::Unknown,
                None => Command::Snooze {
                    days: DEFAULT_SNOOZE_DAYS,
                },
            },
            "status" => Command::Status,
            "scan" => Command::Scan {
                repo: rest.iter().find(|w| !FILLER.contains(&w.as_str())).cloned(),
            },
            "help" | "commands" => Command::Help,
            _ => continue,
        };
        if words[..i].iter().any(|w| NEGATIONS.contains(&w.as_str())) {
            return Command::Unknown;
        }
        return command;
    }
    Command::Unknown
}

/// `30d`, `2w` or a bare `14` (days). `None` when the word is not a
/// duration at all; `Some(Err)` when it is one but out of range.
fn parse_duration(w: &str) -> Option<Result<u32, ()>> {
    let (num, unit) = match w.strip_suffix(['d', 'w']) {
        Some(n) => (n, w.chars().last()),
        None => (w, None),
    };
    let n: u32 = num.parse().ok()?;
    let days = match unit {
        Some('w') => n.checked_mul(7),
        _ => Some(n),
    };
    Some(match days {
        Some(d) if (1..=MAX_SNOOZE_DAYS).contains(&d) => Ok(d),
        _ => Err(()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approve() {
        assert_eq!(parse("@baba_yaga approve"), Command::Approve);
        assert_eq!(parse("approve"), Command::Approve);
        assert_eq!(parse("@baba_yaga please approve this"), Command::Approve);
        assert_eq!(parse("Approve!"), Command::Approve);
        assert_eq!(parse("/approve"), Command::Approve);
    }

    #[test]
    fn reject() {
        assert_eq!(parse("@baba_yaga reject"), Command::Reject);
        assert_eq!(
            parse("@baba_yaga I reject this one, false positive"),
            Command::Reject
        );
    }

    #[test]
    fn negation_is_not_a_command() {
        assert_eq!(parse("@baba_yaga don't approve this"), Command::Unknown);
        assert_eq!(parse("@baba_yaga do not reject"), Command::Unknown);
    }

    #[test]
    fn snooze() {
        assert_eq!(parse("@baba_yaga snooze 30d"), Command::Snooze { days: 30 });
        assert_eq!(
            parse("@baba_yaga snooze 2w please"),
            Command::Snooze { days: 14 }
        );
        assert_eq!(
            parse("@baba_yaga snooze for 10 days"),
            Command::Snooze { days: 10 }
        );
        assert_eq!(
            parse("@baba_yaga snooze"),
            Command::Snooze {
                days: DEFAULT_SNOOZE_DAYS
            }
        );
        assert_eq!(parse("@baba_yaga snooze 0d"), Command::Unknown);
        assert_eq!(parse("@baba_yaga snooze 999w"), Command::Unknown);
    }

    #[test]
    fn status_and_help() {
        assert_eq!(parse("@baba_yaga status"), Command::Status);
        assert_eq!(parse("@baba_yaga what is the status?"), Command::Status);
        assert_eq!(parse("@baba_yaga help"), Command::Help);
    }

    #[test]
    fn scan() {
        assert_eq!(parse("@baba_yaga scan"), Command::Scan { repo: None });
        assert_eq!(
            parse("@baba_yaga scan now please"),
            Command::Scan { repo: None }
        );
        assert_eq!(
            parse("@baba_yaga scan samples"),
            Command::Scan {
                repo: Some("samples".into())
            }
        );
    }

    #[test]
    fn unknown() {
        assert_eq!(parse(""), Command::Unknown);
        assert_eq!(parse("@baba_yaga"), Command::Unknown);
        assert_eq!(parse("@baba_yaga ping"), Command::Unknown);
        assert_eq!(parse("hello there"), Command::Unknown);
    }
}
