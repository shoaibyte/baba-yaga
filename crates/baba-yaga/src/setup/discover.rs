//! Reading settings from the environment of a Console-run Switch server
//! (`switch_core.main`): its URL, registration token and gateway admin
//! login. Uses `pgrep` and `ps eww` through `std::process::Command`, with no
//! shell. Works on macOS and Linux for a server run by the same user.
//! Values are returned, never printed.

use std::collections::BTreeMap;
use std::process::Command;

/// Where the server's settings come from. Behind a trait so tests can fake
/// a server process.
pub trait ServerEnv {
    /// The values of `keys` that the server process has, if one is found.
    fn read(&self, keys: &[&str]) -> BTreeMap<String, String>;
}

/// Server URL as Console starts it.
pub const PUBLIC_URL: &str = "GATEWAY_PUBLIC_URL";
/// The registration key `POST /agents` accepts.
pub const REGISTRATION_TOKEN: &str = "AGENT_REGISTRATION_TOKEN";
pub const ADMIN_EMAIL: &str = "GATEWAY_ADMIN_EMAIL";
pub const ADMIN_PASSWORD: &str = "GATEWAY_ADMIN_PASSWORD";

/// The process pattern `pgrep -f` looks for.
const PROCESS: &str = "switch_core.main";

/// The real process table.
pub struct ConsoleProcess;

impl ServerEnv for ConsoleProcess {
    fn read(&self, keys: &[&str]) -> BTreeMap<String, String> {
        let Ok(out) = Command::new("pgrep").args(["-f", PROCESS]).output() else {
            return BTreeMap::new();
        };
        for pid in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            let Ok(ps) = Command::new("ps").args(["eww", "-p", pid]).output() else {
                continue;
            };
            let found = parse_ps(&String::from_utf8_lossy(&ps.stdout), keys);
            if !found.is_empty() {
                return found;
            }
        }
        BTreeMap::new()
    }
}

/// The `KEY=value` words in `ps eww` output for the wanted keys. `ps`
/// separates the environment with spaces, so a value containing a space is
/// cut there; the values this reads (URL, token, email, password) have been
/// single words in every Console server seen.
pub fn parse_ps(output: &str, keys: &[&str]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for word in output.split_whitespace() {
        if let Some((k, v)) = word.split_once('=') {
            if keys.contains(&k) && !v.is_empty() {
                out.entry(k.to_string()).or_insert_with(|| v.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wanted_keys_from_ps_output() {
        let ps = "  PID   TT  STAT      TIME COMMAND\n\
                  46530   ??  S      1:02.33 /venv/bin/python -m switch_core.main \
                  HOME=/Users/x GATEWAY_PUBLIC_URL=http://localhost:51970 \
                  AGENT_REGISTRATION_TOKEN=tok-123 EMPTY= GATEWAY_ADMIN_EMAIL=a@b.c";
        let got = parse_ps(
            ps,
            &[PUBLIC_URL, REGISTRATION_TOKEN, ADMIN_PASSWORD, "EMPTY"],
        );
        assert_eq!(got.len(), 2);
        assert_eq!(got[PUBLIC_URL], "http://localhost:51970");
        assert_eq!(got[REGISTRATION_TOKEN], "tok-123");
        assert!(parse_ps("", &[PUBLIC_URL]).is_empty());
    }
}
