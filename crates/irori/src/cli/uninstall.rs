//! `irori uninstall`: what `install.sh` put down. It does not talk to a running server,
//! and it does not delete a config directory unless that path was named this run.

use std::fs;
use std::path::{Path, PathBuf};

use super::args::UninstallArgs;
use super::prompt;

const MARKER: &str = "# irori";
const UNIT: &str = "/etc/systemd/system/irori.service";

/// What a directory holds when Irori made it: the config or the data. An install
/// directory is [`holds_only_the_binary`].
const PURGE_MARKERS: [&str; 4] = ["irori.toml", "irori.db", "cli.url", "packages"];

/// Where a system install's binary sits. Other programs live there too, so it is never
/// Irori's to delete, whatever is in it.
const SHARED: [&str; 3] = ["/usr/local", "/opt", "/opt/homebrew"];

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

/// An install under an `IRORI_HOME` of the person's own: `<somewhere>/bin/irori`, with the
/// `# irori` PATH line install.sh wrote for that directory still in `config`.
pub fn path_line_names(config: &str, exe: &Path) -> bool {
    if exe.file_name().and_then(|name| name.to_str()) != Some("irori") {
        return false;
    }
    let Some(dir) = exe.parent().and_then(Path::to_str) else {
        return false;
    };
    if !dir.ends_with("/bin") {
        return false;
    }
    let mut lines = config.lines();
    while let Some(line) = lines.next() {
        if line == MARKER
            && lines.next().is_some_and(|next| {
                (next.starts_with("export PATH=") || next.starts_with("set -gx PATH"))
                    && next.contains(dir)
            })
        {
            return true;
        }
    }
    false
}

/// [`install_kind`], or a user install when a shell config's `# irori` line names this
/// binary's directory.
fn installed_kind(exe: &Path) -> InstallKind {
    match install_kind(exe) {
        InstallKind::Other
            if path_files_with_marker().iter().any(|file| {
                fs::read_to_string(file).is_ok_and(|text| path_line_names(&text, exe))
            }) =>
        {
            InstallKind::User
        }
        kind => kind,
    }
}

/// `/`, the home directory or anything above it, a missing path, anything that isn't a
/// directory, and a directory with nothing of Irori's in it.
pub fn refuse_purge(path: &Path) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    refuse_purge_from(path, home.as_deref())
}

fn refuse_purge_from(path: &Path, home: Option<&Path>) -> Result<PathBuf, String> {
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
    if SHARED.iter().any(|dir| canonical == Path::new(dir)) {
        return Err(format!(
            "refusing to delete {}: other programs live there",
            canonical.display()
        ));
    }
    // The home directory, and anything it sits inside (`/Users`, `/home`).
    if let Some(home) = home.and_then(|home| fs::canonicalize(home).ok())
        && home.starts_with(&canonical)
    {
        return Err(format!(
            "refusing to delete {}: the home directory {} is {}",
            canonical.display(),
            home.display(),
            if home == canonical {
                "that"
            } else {
                "inside it"
            }
        ));
    }
    // Whatever `HOME` says (under sudo it is root's), a directory Irori never wrote in is
    // not one this command deletes.
    if !PURGE_MARKERS
        .iter()
        .any(|marker| canonical.join(marker).exists())
        && !holds_only_the_binary(&canonical)
    {
        return Err(format!(
            "refusing to delete {}: nothing in it is Irori's (no irori.toml, irori.db, \
             cli.url or packages, and it isn't an install with only bin/irori in it)",
            canonical.display()
        ));
    }
    Ok(canonical)
}

/// What install.sh leaves under `IRORI_HOME`: a `bin` with `irori` in it and nothing
/// else. A `bin` other programs share (`/usr/local`) is not that.
fn holds_only_the_binary(dir: &Path) -> bool {
    let only = |dir: &Path, name: &str| {
        fs::read_dir(dir).is_ok_and(|entries| {
            let names: Vec<_> = entries.flatten().map(|entry| entry.file_name()).collect();
            names.len() == 1 && names[0] == name
        })
    };
    only(dir, "bin") && only(&dir.join("bin"), "irori")
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
    let kind = installed_kind(&exe);
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
    // Everything that can be refused is refused before anything is removed: the binary is
    // one of the things that marks its directory as Irori's, and a run that stops halfway
    // can't be finished by a binary that is gone.
    if binary && kind == InstallKind::Other {
        return Err(not_installed(&exe));
    }
    let purge = purge.map(|dir| refuse_purge(&dir)).transpose()?;
    if binary {
        remove_binary(&exe, args.yes)?;
    }
    if unit {
        remove_unit(args.yes)?;
    }
    if let Some(dir) = purge {
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

fn not_installed(exe: &Path) -> String {
    format!(
        "this copy of irori isn't one install.sh put down ({}). Delete the file yourself \
         if it should go.",
        exe.display()
    )
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

fn remove_binary(exe: &Path, yes: bool) -> Result<(), String> {
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
    // The shell configs first. If one can't be rewritten the binary is still here to run
    // this again, and a PATH line with no binary behind it is the worse thing to leave.
    for file in files {
        let text = fs::read_to_string(&file)
            .map_err(|error| format!("couldn't read {}: {error}", file.display()))?;
        let next = strip_irori_path(&text);
        if next != text {
            replace_file(&file, &next)
                .map_err(|error| format!("couldn't write {}: {error}", file.display()))?;
            println!("took the PATH line out of {}", file.display());
        }
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
    Ok(())
}

/// Writes beside the file and renames over it, as install.sh does, so a shell config is
/// never left half written. A config that is a symlink keeps being one: its target is
/// what gets replaced.
fn replace_file(file: &Path, text: &str) -> std::io::Result<()> {
    let real = fs::canonicalize(file)?;
    let mut name = real.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".irori.{}", std::process::id()));
    let tmp = real.with_file_name(name);
    let written = fs::write(&tmp, text)
        .and_then(|()| fs::set_permissions(&tmp, fs::metadata(&real)?.permissions()))
        .and_then(|()| fs::rename(&tmp, &real));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

fn remove_unit(yes: bool) -> Result<(), String> {
    let unit = Path::new(UNIT);
    if !unit.is_file() {
        // Nothing to do is not a failure: the menu offers the unit whether it is there
        // or not, and whatever else was chosen still has to happen.
        println!("there is no {UNIT}");
        return Ok(());
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
    let mut files: Vec<PathBuf> = Vec::new();
    for path in candidates {
        // `ZDOTDIR` is usually the home directory, and one file is rewritten once.
        if !files.contains(&path)
            && fs::read_to_string(&path).is_ok_and(|text| text.lines().any(|line| line == MARKER))
        {
            files.push(path);
        }
    }
    files
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
    fn purge_accepts_a_directory_irori_wrote_in() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("irori.toml"), "").unwrap();
        let got = refuse_purge(dir.path()).unwrap();
        assert_eq!(got, fs::canonicalize(dir.path()).unwrap());
    }

    #[test]
    fn purge_refuses_a_directory_with_nothing_of_iroris() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "").unwrap();
        let error = refuse_purge(dir.path()).unwrap_err();
        assert!(error.contains("nothing in it is Irori's"), "{error}");
    }

    #[test]
    fn purge_refuses_a_bin_other_programs_share() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("local");
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::write(prefix.join("bin/irori"), "").unwrap();
        assert!(refuse_purge_from(&prefix, None).is_ok());
        fs::write(prefix.join("bin/other"), "").unwrap();
        assert!(refuse_purge_from(&prefix, None).is_err());
        for shared in ["/usr/local", "/opt"] {
            if Path::new(shared).is_dir() {
                let error = refuse_purge_from(Path::new(shared), None).unwrap_err();
                assert!(error.contains("other programs"), "{shared}: {error}");
            }
        }
    }

    #[test]
    fn purge_refuses_what_the_home_directory_sits_inside() {
        // Marked as Irori's, so only the home check can be what refuses it.
        let above = tempfile::tempdir().unwrap();
        fs::write(above.path().join("irori.toml"), "").unwrap();
        let home = above.path().join("ada");
        fs::create_dir_all(home.join(".irori/bin")).unwrap();
        fs::write(home.join(".irori/bin/irori"), "").unwrap();
        for path in [above.path().to_owned(), home.join(".irori/../..")] {
            let error = refuse_purge_from(&path, Some(&home)).unwrap_err();
            assert!(error.contains("inside it"), "{error}");
        }
        assert!(refuse_purge_from(&home, Some(&home)).is_err());
        assert!(refuse_purge_from(&home.join(".irori"), Some(&home)).is_ok());
    }

    #[test]
    fn a_path_line_for_this_directory_makes_it_an_install() {
        let config = "# irori\nexport PATH='/opt/home/bin':\"$PATH\"\n";
        assert!(path_line_names(config, Path::new("/opt/home/bin/irori")));
        assert!(!path_line_names(config, Path::new("/opt/other/bin/irori")));
        assert!(!path_line_names(config, Path::new("/opt/home/bin/other")));
        assert!(!path_line_names(
            "export PATH='/opt/home/bin'\n",
            Path::new("/opt/home/bin/irori")
        ));
    }

    #[test]
    fn a_rewritten_config_keeps_its_mode_and_its_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("zshrc");
        let link = dir.path().join(".zshrc");
        fs::write(&real, "# irori\nexport PATH=x\nexport A=1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
            std::os::unix::fs::symlink(&real, &link).unwrap();
            replace_file(&link, "export A=1\n").unwrap();
            assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
            assert_eq!(
                fs::metadata(&real).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        #[cfg(not(unix))]
        replace_file(&real, "export A=1\n").unwrap();
        assert_eq!(fs::read_to_string(&real).unwrap(), "export A=1\n");
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            if cfg!(unix) { 2 } else { 1 }
        );
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
