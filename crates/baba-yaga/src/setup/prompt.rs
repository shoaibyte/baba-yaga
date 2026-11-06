//! Questions to the person running `bay init`. Behind a trait so the flow
//! can run from a script in tests and fail cleanly with
//! `--non-interactive`.

use std::io::{BufRead, Write};

use anyhow::{bail, Context, Result};

pub trait Prompter {
    /// A visible answer. Empty input takes `default`.
    fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String>;
    /// A hidden answer, for keys and passwords. Never echoed.
    fn secret(&mut self, question: &str) -> Result<String>;
    /// Yes or no. Empty input takes `default`.
    fn confirm(&mut self, question: &str, default: bool) -> Result<bool>;
    /// Wait for Enter after an instruction the person must carry out.
    fn pause(&mut self, instruction: &str) -> Result<()>;
}

/// Questions on the terminal; hidden input through `rpassword`.
pub struct Terminal;

impl Terminal {
    fn line(question: &str) -> Result<String> {
        print!("{question}");
        std::io::stdout().flush()?;
        let mut s = String::new();
        let n = std::io::stdin()
            .lock()
            .read_line(&mut s)
            .context("failed to read the answer")?;
        if n == 0 {
            bail!("input closed; rerun with --non-interactive to use defaults and fail on gaps");
        }
        Ok(s.trim().to_string())
    }
}

impl Prompter for Terminal {
    fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String> {
        loop {
            let shown = match default {
                Some(d) if !d.is_empty() => format!("  {question} [{d}]: "),
                _ => format!("  {question}: "),
            };
            let answer = Self::line(&shown)?;
            match (answer.is_empty(), default) {
                (false, _) => return Ok(answer),
                (true, Some(d)) => return Ok(d.to_string()),
                (true, None) => continue,
            }
        }
    }

    fn secret(&mut self, question: &str) -> Result<String> {
        loop {
            let answer = rpassword::prompt_password(format!("  {question} (hidden): "))
                .context("failed to read hidden input")?;
            if !answer.trim().is_empty() {
                return Ok(answer.trim().to_string());
            }
        }
    }

    fn confirm(&mut self, question: &str, default: bool) -> Result<bool> {
        let hint = if default { "Y/n" } else { "y/N" };
        loop {
            match Self::line(&format!("  {question} [{hint}]: "))?
                .to_lowercase()
                .as_str()
            {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => continue,
            }
        }
    }

    fn pause(&mut self, instruction: &str) -> Result<()> {
        println!("  {instruction}");
        Self::line("  Press Enter when done. ")?;
        Ok(())
    }
}

/// `--non-interactive`: defaults are taken, anything without one fails.
pub struct NonInteractive;

impl Prompter for NonInteractive {
    fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String> {
        match default {
            Some(d) if !d.is_empty() => Ok(d.to_string()),
            _ => bail!("--non-interactive: no value for \"{question}\""),
        }
    }

    fn secret(&mut self, question: &str) -> Result<String> {
        bail!("--non-interactive: no value for \"{question}\"")
    }

    fn confirm(&mut self, _question: &str, default: bool) -> Result<bool> {
        Ok(default)
    }

    fn pause(&mut self, instruction: &str) -> Result<()> {
        bail!("--non-interactive: this step needs a person: {instruction}")
    }
}

/// Answers from a script, for tests. Each call takes the next answer; an
/// empty answer means "take the default". Records every question asked.
#[cfg(test)]
#[derive(Default)]
pub struct Scripted {
    pub answers: std::collections::VecDeque<String>,
    pub asked: Vec<String>,
}

#[cfg(test)]
impl Scripted {
    pub fn new(answers: &[&str]) -> Self {
        Self {
            answers: answers.iter().map(|s| s.to_string()).collect(),
            asked: Vec::new(),
        }
    }

    fn next(&mut self, question: &str) -> Result<String> {
        self.asked.push(question.to_string());
        self.answers
            .pop_front()
            .with_context(|| format!("script ran out at \"{question}\""))
    }
}

#[cfg(test)]
impl Prompter for Scripted {
    fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String> {
        let a = self.next(question)?;
        Ok(if a.is_empty() {
            default.unwrap_or_default().to_string()
        } else {
            a
        })
    }

    fn secret(&mut self, question: &str) -> Result<String> {
        self.next(question)
    }

    fn confirm(&mut self, question: &str, default: bool) -> Result<bool> {
        Ok(match self.next(question)?.as_str() {
            "" => default,
            a => a == "y",
        })
    }

    fn pause(&mut self, instruction: &str) -> Result<()> {
        self.next(instruction).map(|_| ())
    }
}
