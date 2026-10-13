//! Questions on a terminal. `--json` and a pipe never ask: they refuse without `--yes`.

use std::io::{self, IsTerminal, Write};

/// `true` when the person said yes. A declined question is `Ok(false)`, not an error.
pub fn confirm(question: &str, yes: bool, json: bool) -> Result<bool, String> {
    if yes {
        return Ok(true);
    }
    if json || !io::stdin().is_terminal() {
        return Err(format!(
            "{question} Re-run with --yes to do it without asking."
        ));
    }
    eprint!("{question} [y/N] ");
    let _ = io::stderr().flush();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|error| format!("couldn't read the answer: {error}"))?;
    let line = line.trim();
    Ok(line.eq_ignore_ascii_case("y") || line.eq_ignore_ascii_case("yes"))
}

/// A numbered list. `q` is `Ok(None)`.
pub fn choose(heading: &str, rows: &[String]) -> Result<Option<usize>, String> {
    if !io::stdin().is_terminal() {
        return Err("pass an id, or run this on a terminal to pick from the list".to_owned());
    }
    eprintln!("{heading}");
    eprintln!();
    for (index, row) in rows.iter().enumerate() {
        eprintln!("  {:>2}  {row}", index + 1);
    }
    eprintln!();
    eprint!("Which? [number, or q] ");
    let _ = io::stderr().flush();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|error| format!("couldn't read the answer: {error}"))?;
    let line = line.trim();
    if line.is_empty() || line.eq_ignore_ascii_case("q") {
        return Ok(None);
    }
    let number: usize = line
        .parse()
        .map_err(|_| format!("`{line}` isn't a number on the list"))?;
    if number == 0 || number > rows.len() {
        return Err(format!("`{line}` isn't a number on the list"));
    }
    Ok(Some(number - 1))
}

pub fn password(prompt: &str, from_stdin: bool) -> Result<String, String> {
    let typed = if from_stdin {
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .map_err(|error| format!("couldn't read the password: {error}"))?;
        line.trim_end_matches(['\n', '\r']).to_owned()
    } else {
        if !io::stdin().is_terminal() {
            return Err("pass --password-stdin when stdin isn't a terminal".to_owned());
        }
        rpassword::prompt_password(prompt)
            .map_err(|error| format!("couldn't read the password: {error}"))?
    };
    if typed.is_empty() {
        return Err("the password is empty".to_owned());
    }
    Ok(typed)
}

pub fn line(prompt: &str) -> Result<String, String> {
    eprint!("{prompt}");
    let _ = io::stderr().flush();
    let mut typed = String::new();
    io::stdin()
        .read_line(&mut typed)
        .map_err(|error| format!("couldn't read the answer: {error}"))?;
    Ok(typed.trim().to_owned())
}
