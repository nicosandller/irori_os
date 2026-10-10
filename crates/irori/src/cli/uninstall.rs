//! `irori uninstall`: what `install.sh` put down. It does not talk to a running server,
//! and it does not delete a config directory unless that path was named this run.

use std::fs;
use std::path::{Component, Path, PathBuf};

use super::args::UninstallArgs;
use super::prompt;

const MARKER: &str = "# irori";
const UNIT: &str = "/etc/systemd/system/irori.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    User,
    System,
    /// A checkout or a copied binary. `--binary` will not delete it.
    Other,
}

pub fn install_kind(exe: &Path) -> InstallKind {
    let text = exe.to_string_lossy();
    if text.ends_with("/.irori/bin/irori") {
        InstallKind::User
    } else if text == "/usr/local/bin/irori" {
        InstallKind::System
    } else {
        InstallKind::Other
    }
}

/// `/`, the home directory, a missing path, or anything that isn't a directory.
pub fn refuse_purge(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("pass the directory to delete with --purge".to_owned());
    }
    if !path.exists() {
        return Err(format!("there's no directory at {}", path.display()));
    }
    if !path.is_dir() {
        return Err(format!("{} isn't a directory", path.display()));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("couldn't confirm {}: {error}", path.display()))?;
    if canonical.components().count() <= 1 || canonical == Path::new("/") {
        return Err("refusing to delete /".to_owned());
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if let Ok(home) = fs::canonicalize(&home)
            && canonical == home
        {
            return Err(format!(
                "refusing to delete the home directory {}",
                home.display()
            ));
        }
    }
    // A path that canonicalize reduced to nothing useful, or that still climbs.
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        // Allowed once it canonicalizes to a real directory that isn't `/` or home.
        // The check above already refused those.
    }
    Ok(canonical)
}

/// Drops the `# irori` line and the PATH line `install.sh` wrote under it.
pub fn strip_irori_path(text: &str) -> String {
    let mut out = String::new();
    let mut skip_next = false;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if skip_next {
            skip_next = false;
            if bare.starts_with("export PATH=") || bare.starts_with("set -gx PATH") {
                continue;
            }
        }
        if bare == MARKER {
            skip_next = true;
            continue;
        }
        out.push_str(line);
    }
    out
}

pub fn run(args: UninstallArgs) -> Result<(), String> {
    let exe =
        std::env::current_exe().map_err(|error| format!("couldn't find this binary: {error}"))?;
    let kind = install_kind(&exe);
    let scripted = args.binary || args.unit || args.purge.is_some();
    let (binary, unit, purge) = if scripted {
        (args.binary, args.unit, args.purge)
    } else {
        menu(&exe, kind)?
    };
    if !binary && !unit && purge.is_none() {
        println!("nothing removed");
        return Ok(());
    }
    if binary {
        remove_binary(&exe, kind, args.yes)?;
    }
    if unit {
        remove_unit(args.yes)?;
    }
    if let Some(dir) = purge {
        let dir = refuse_purge(&dir)?;
        if !prompt::confirm(
            &format!("Delete {} and everything in it?", dir.display()),
            args.yes,
            false,
        )? {
            println!("left {} in place", dir.display());
        } else {
            fs::remove_dir_all(&dir)
                .map_err(|error| format!("couldn't delete {}: {error}", dir.display()))?;
            println!("deleted {}", dir.display());
        }
    }
    Ok(())
}

fn menu(exe: &Path, kind: InstallKind) -> Result<(bool, bool, Option<PathBuf>), String> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(
            "say what to remove: --binary, --unit, or --purge <dir>. On a terminal, this is a menu."
                .to_owned(),
        );
    }
    eprintln!("This is the irori at {}.", exe.display());
    match kind {
        InstallKind::Other => {
            eprintln!(
                "It isn't the copy install.sh puts in ~/.irori/bin or /usr/local/bin, so the binary stays."
            );
        }
        InstallKind::System => eprintln!("That's a system install."),
        InstallKind::User => eprintln!("That's a user install."),
    }
    eprintln!();
    eprintln!("Remove which? Enter the numbers, separated by spaces, or q.");
    eprintln!();
    let mut n = 1;
    if kind != InstallKind::Other {
        eprintln!("  {n}  the binary and the # irori PATH line");
        n += 1;
    }
    eprintln!("  {n}  the systemd unit, when it's installed");
    let unit_n = n;
    n += 1;
    eprintln!("  {n}  a directory you name (nothing is chosen until you do)");
    let purge_n = n;
    eprintln!();
    let answer = prompt::line("Numbers, or q: ")?;
    if answer.is_empty() || answer.eq_ignore_ascii_case("q") {
        return Ok((false, false, None));
    }
    let mut binary = false;
    let mut unit = false;
    let mut purge = false;
    for word in answer.split(|ch: char| ch == ',' || ch.is_whitespace()) {
        if word.is_empty() {
            continue;
        }
        let number: usize = word
            .parse()
            .map_err(|_| format!("`{word}` isn't a number on the list"))?;
        if kind != InstallKind::Other && number == 1 {
            binary = true;
        } else if number == unit_n {
            unit = true;
        } else if number == purge_n {
            purge = true;
        } else {
            return Err(format!("`{word}` isn't a number on the list"));
        }
    }
    let purge = if purge {
        let typed = prompt::line("Directory to delete: ")?;
        if typed.is_empty() {
            return Err("no directory was named".to_owned());
        }
        Some(refuse_purge(Path::new(&typed))?)
    } else {
        None
    };
    Ok((binary, unit, purge))
}

fn remove_binary(exe: &Path, kind: InstallKind, yes: bool) -> Result<(), String> {
    if kind == InstallKind::Other {
        return Err(format!(
            "this copy of irori isn't one install.sh put down ({})",
            exe.display()
        ));
    }
    let files = path_files_with_marker();
    let listed = if files.is_empty() {
        "no shell config has the # irori PATH line".to_owned()
    } else {
        files
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !prompt::confirm(
        &format!("Remove {} and the PATH line in {listed}?", exe.display()),
        yes,
        false,
    )? {
        println!("left the binary in place");
        return Ok(());
    }
    match fs::remove_file(exe) {
        Ok(()) => println!("removed {}", exe.display()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(format!(
                "needs root to remove {}. Run: sudo irori uninstall --binary --yes",
                exe.display()
            ));
        }
        Err(error) => return Err(format!("couldn't remove {}: {error}", exe.display())),
    }
    for file in files {
        let text = fs::read_to_string(&file)
            .map_err(|error| format!("couldn't read {}: {error}", file.display()))?;
        let next = strip_irori_path(&text);
        if next != text {
            fs::write(&file, next)
                .map_err(|error| format!("couldn't write {}: {error}", file.display()))?;
            println!("took the PATH line out of {}", file.display());
        }
    }
    Ok(())
}

fn remove_unit(yes: bool) -> Result<(), String> {
    let unit = Path::new(UNIT);
    if !unit.is_file() {
        return Err(format!("there is no {UNIT}"));
    }
    if !prompt::confirm(&format!("Disable and remove {UNIT}?"), yes, false)? {
        println!("left the systemd unit in place");
        return Ok(());
    }
    let disabled = std::process::Command::new("systemctl")
        .args(["disable", "--now", "irori"])
        .status();
    match disabled {
        Ok(status) if status.success() => {}
        Ok(_) | Err(_) => {
            return Err(format!(
                "couldn't disable the unit. Run:\n  sudo systemctl disable --now irori\n  sudo rm {UNIT}"
            ));
        }
    }
    match fs::remove_file(unit) {
        Ok(()) => println!("removed {UNIT}"),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(format!(
                "the unit is disabled, and removing the file needs root: sudo rm {UNIT}"
            ));
        }
        Err(error) => return Err(format!("couldn't remove {UNIT}: {error}")),
    }
    Ok(())
}

fn path_files_with_marker() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let home = PathBuf::from(home);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let zdot = std::env::var_os("ZDOTDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.clone());
    let candidates = [
        zdot.join(".zshrc"),
        zdot.join(".zshenv"),
        home.join(".zshrc"),
        home.join(".bashrc"),
        home.join(".bash_profile"),
        home.join(".bash_login"),
        home.join(".profile"),
        xdg.join("fish/config.fish"),
    ];
    candidates
        .into_iter()
        .filter(|path| {
            fs::read_to_string(path)
                .map(|text| text.lines().any(|line| line == MARKER))
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn purge_refuses_root_and_home() {
        assert!(refuse_purge(Path::new("/")).is_err());
        if let Some(home) = std::env::var_os("HOME") {
            let error = refuse_purge(Path::new(&home)).unwrap_err();
            assert!(error.contains("home"), "{error}");
        }
    }

    #[test]
    fn purge_accepts_a_directory_that_is_neither() {
        let dir = std::env::temp_dir().join(format!("irori-purge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let got = refuse_purge(&dir).unwrap();
        assert_eq!(got, fs::canonicalize(&dir).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_path_block_goes_and_the_neighbours_stay() {
        let text =
            "export PATH=foo\n# irori\nexport PATH=\"$HOME/.irori/bin\":\"$PATH\"\nexport A=1\n";
        let out = strip_irori_path(text);
        assert!(!out.contains(".irori"), "{out}");
        assert!(out.contains("export PATH=foo"), "{out}");
        assert!(out.contains("export A=1"), "{out}");
    }

    #[test]
    fn a_fish_path_line_goes_too() {
        let text = "# irori\nset -gx PATH \"$HOME/.irori/bin\" \"$PATH\"\n";
        assert_eq!(strip_irori_path(text), "");
    }

    #[test]
    fn only_the_install_sh_locations_count() {
        assert_eq!(
            install_kind(Path::new("/Users/a/.irori/bin/irori")),
            InstallKind::User
        );
        assert_eq!(
            install_kind(Path::new("/usr/local/bin/irori")),
            InstallKind::System
        );
        assert_eq!(install_kind(Path::new("/tmp/irori")), InstallKind::Other);
    }
}
