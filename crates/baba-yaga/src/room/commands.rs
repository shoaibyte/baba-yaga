//! Parsing addressed room messages into commands.
//!
//! Strict: once the `@baba_yaga` mention is removed, the rest must be exactly
//! one command with its allowed arguments, case-insensitive, with trailing
//! punctuation allowed. Anything else is `Unknown`, so a message that only
//! starts with or contains a command word ("please approve this", "approve
//! once CI passes") never acts.

/// Longest snooze accepted, so a typo cannot hide a finding for years.
pub const MAX_SNOOZE_DAYS: u32 = 365;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Approve,
    Reject,
    Snooze {
        days: u32,
    },
    Status,
    /// `repo` is the word after `scan` as typed; the caller checks it
    /// against the configured repos.
    Scan {
        repo: Option<String>,
    },
    Help,
    Unknown,
}

/// The mention forms removed before parsing, compared case-insensitively.
const MENTIONS: [&str; 2] = ["@baba_yaga", "@baba-yaga"];

pub fn parse(body: &str) -> Command {
    let words: Vec<&str> = body
        .split_whitespace()
        .filter(|w| {
            let w = w.trim_end_matches([',', ':', '.', '!', '?', ';']);
            !MENTIONS.iter().any(|m| w.eq_ignore_ascii_case(m))
        })
        .collect();
    let Some((last, init)) = words.split_last() else {
        return Command::Unknown;
    };
    let last = last.trim_end_matches(['.', '!', '?', ',', ';', ':']);
    let mut words: Vec<&str> = init.to_vec();
    if !last.is_empty() {
        words.push(last);
    }
    let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    let lower: Vec<&str> = lower.iter().map(String::as_str).collect();
    match lower.as_slice() {
        ["approve"] => Command::Approve,
        ["reject"] => Command::Reject,
        ["status"] => Command::Status,
        ["help"] => Command::Help,
        ["scan"] => Command::Scan { repo: None },
        ["scan", _] => Command::Scan {
            repo: Some(words[1].to_string()),
        },
        ["snooze", d] => match parse_duration(d) {
            Some(days) => Command::Snooze { days },
            None => Command::Unknown,
        },
        _ => Command::Unknown,
    }
}

/// `30d` or `2w`, within `1..=MAX_SNOOZE_DAYS` days.
fn parse_duration(w: &str) -> Option<u32> {
    let unit = w.chars().last()?;
    let n: u32 = w[..w.len() - 1].parse().ok()?;
    let days = match unit {
        'd' => n,
        'w' => n.checked_mul(7)?,
        _ => return None,
    };
    (1..=MAX_SNOOZE_DAYS).contains(&days).then_some(days)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_commands() {
        assert_eq!(parse("@baba_yaga approve"), Command::Approve);
        assert_eq!(parse("approve"), Command::Approve);
        assert_eq!(parse("@Baba_Yaga APPROVE!"), Command::Approve);
        assert_eq!(parse("@baba_yaga: approve."), Command::Approve);
        assert_eq!(parse("approve @baba_yaga"), Command::Approve);
        assert_eq!(parse("@baba_yaga reject"), Command::Reject);
        assert_eq!(parse("@baba_yaga status?"), Command::Status);
        assert_eq!(parse("@baba_yaga help"), Command::Help);
    }

    #[test]
    fn extra_words_are_not_commands() {
        assert_eq!(parse("@baba_yaga please approve this"), Command::Unknown);
        assert_eq!(parse("@baba_yaga approve this"), Command::Unknown);
        assert_eq!(parse("@baba_yaga approve once CI passes"), Command::Unknown);
        assert_eq!(parse("@baba_yaga don't approve"), Command::Unknown);
        assert_eq!(parse("@baba_yaga I reject this one"), Command::Unknown);
        assert_eq!(parse("@baba_yaga what is the status?"), Command::Unknown);
        assert_eq!(parse("@baba_yaga approved"), Command::Unknown);
        assert_eq!(parse("@baba_yaga /approve"), Command::Unknown);
        assert_eq!(parse("@patrol-fixer approve"), Command::Unknown);
    }

    #[test]
    fn snooze() {
        assert_eq!(parse("@baba_yaga snooze 30d"), Command::Snooze { days: 30 });
        assert_eq!(parse("@baba_yaga Snooze 2W."), Command::Snooze { days: 14 });
        assert_eq!(parse("@baba_yaga snooze"), Command::Unknown);
        assert_eq!(parse("@baba_yaga snooze 10"), Command::Unknown);
        assert_eq!(parse("@baba_yaga snooze 2w please"), Command::Unknown);
        assert_eq!(parse("@baba_yaga snooze for 10 days"), Command::Unknown);
        assert_eq!(parse("@baba_yaga snooze 0d"), Command::Unknown);
        assert_eq!(parse("@baba_yaga snooze 999w"), Command::Unknown);
        assert_eq!(parse("@baba_yaga snooze d"), Command::Unknown);
    }

    #[test]
    fn scan() {
        assert_eq!(parse("@baba_yaga scan"), Command::Scan { repo: None });
        assert_eq!(
            parse("@baba_yaga scan Samples!"),
            Command::Scan {
                repo: Some("Samples".into())
            }
        );
        assert_eq!(parse("@baba_yaga scan now please"), Command::Unknown);
    }

    #[test]
    fn unknown() {
        assert_eq!(parse(""), Command::Unknown);
        assert_eq!(parse("@baba_yaga"), Command::Unknown);
        assert_eq!(parse("@baba_yaga ?"), Command::Unknown);
        assert_eq!(parse("@baba_yaga ping"), Command::Unknown);
        assert_eq!(parse("hello there"), Command::Unknown);
    }
}
