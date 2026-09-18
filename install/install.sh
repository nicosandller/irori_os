#!/usr/bin/env bash
# Irori installer: detect the platform, download a prebuilt binary, verify it, and put it on PATH.
#
#   curl -fsSL https://github.com/nicosandller/irori_os/releases/latest/download/install.sh | bash
#
# Options (with `curl | bash`, pass them after `bash -s --`):
#   -v, --version <tag>   install a specific release, e.g. 0.2.0 or v0.2.0
#   -b, --binary <path>   install a local binary instead of downloading
#   --system              install to /usr/local/bin (needs root)
#   --no-modify-path      don't touch shell config files
#   -h, --help            show this help
#
# Everything can be overridden with environment variables: IRORI_REPO, IRORI_HOME, IRORI_VERSION.
set -euo pipefail

APP=irori
REPO="${IRORI_REPO:-nicosandller/irori_os}"
os=""
arch=""
version=""

MUTED='\033[0;2m'
RED='\033[0;31m'
EMBER='\033[38;5;209m'
NC='\033[0m'

usage() {
    cat <<EOF
Irori installer

Usage: install.sh [options]

Options:
    -h, --help            Show this help
    -v, --version <tag>   Install a specific release (e.g. 0.2.0)
    -b, --binary <path>   Install from a local binary instead of downloading
        --system          Install system-wide under /usr/local (needs root)
        --no-modify-path  Don't add the install directory to your shell config

Examples:
    curl -fsSL https://github.com/$REPO/releases/latest/download/install.sh | bash
    curl -fsSL https://github.com/$REPO/releases/latest/download/install.sh | bash -s -- --version 0.2.0
    ./install.sh --binary ./target/release/irori
EOF
}

info() { printf "${MUTED}%s${NC}\n" "$*"; }
warn() { printf "${RED}warning:${NC} %s\n" "$*" >&2; }
die() { printf "${RED}error:${NC} %s\n" "$*" >&2; exit 1; }

requested_version="${IRORI_VERSION:-}"
no_modify_path=false
binary_path=""
system_install=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        -v|--version)
            [[ -n "${2:-}" ]] || die "--version requires a version argument"
            requested_version="$2"
            shift 2
            ;;
        -b|--binary)
            [[ -n "${2:-}" ]] || die "--binary requires a path argument"
            binary_path="$2"
            shift 2
            ;;
        --system)
            system_install=true
            shift
            ;;
        --no-modify-path)
            no_modify_path=true
            shift
            ;;
        *)
            warn "unknown option '$1'"
            shift
            ;;
    esac
done

# Where things go. A system install puts the binary on the system PATH; a user install keeps
# everything under IRORI_HOME.
if [[ "$system_install" == true ]]; then
    [[ "$(id -u)" -eq 0 ]] || die "--system needs root; try: sudo"
    INSTALL_DIR="/usr/local/bin"
else
    IRORI_HOME="${IRORI_HOME:-$HOME/.$APP}"
    INSTALL_DIR="$IRORI_HOME/bin"
fi
mkdir -p "$INSTALL_DIR"

verify_checksum() {
    local file="$1" sums="$2" name="$3"
    [[ -f "$sums" ]] || die "no SHA256SUMS in the release; refusing to install unverified"
    local tool=""
    if command -v sha256sum >/dev/null 2>&1; then
        tool="sha256sum"
    elif command -v shasum >/dev/null 2>&1; then
        tool="shasum -a 256"
    else
        die "neither sha256sum nor shasum found; refusing to install unverified"
    fi
    local expected actual
    expected="$(awk -v n="$name" '$2 == n || $2 == "./" n { print $1 }' "$sums" | head -n1)"
    [[ -n "$expected" ]] || die "$name is not in SHA256SUMS; refusing to install unverified"
    actual="$($tool "$file" | awk '{print $1}')"
    [[ "$expected" == "$actual" ]] || die "checksum mismatch for $name (expected $expected, got $actual)"
    info "checksum verified"
}

install_binary() {
    local src="$1"
    [[ -f "$src" ]] || die "no binary at $src"
    install -m755 "$src" "$INSTALL_DIR/$APP"
    info "installed $INSTALL_DIR/$APP"
}

# A local binary is a curl-less, checksum-less path for people building from source.
if [[ -n "$binary_path" ]]; then
    install_binary "$binary_path"
else
    raw_os="$(uname -s)"
    os=""
    case "$raw_os" in
        Darwin) os="darwin" ;;
        Linux) os="linux" ;;
        *) die "unsupported operating system: $raw_os" ;;
    esac

    raw_arch="$(uname -m)"
    arch=""
    case "$raw_arch" in
        x86_64|amd64) arch="x64" ;;
        arm64|aarch64) arch="arm64" ;;
        *) die "unsupported architecture: $raw_arch" ;;
    esac

    # Apple's Rosetta reports x86_64 on Apple Silicon; install the native build.
    if [[ "$os" == "darwin" && "$arch" == "x64" ]]; then
        if [[ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" == "1" ]]; then
            arch="arm64"
        fi
    fi

    filename="$APP-$os-$arch.tar.gz"

    command -v curl >/dev/null 2>&1 || die "'curl' is required to install $APP"

    if [[ -z "$requested_version" ]]; then
        info "finding the latest release..."
        tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
            | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
        [[ -n "$tag" ]] || die "couldn't find a release of $REPO; pass --version"
    else
        requested_version="${requested_version#v}"
        tag="v$requested_version"
    fi
    version="${tag#v}"
    url="https://github.com/$REPO/releases/download/$tag/$filename"

    info "downloading $APP $version for $os-$arch..."
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/irori-install.XXXXXX")"
    trap 'rm -rf "$tmp"' EXIT
    if [[ -t 2 ]]; then
        curl -fL --progress-bar -o "$tmp/$filename" "$url" || die "download failed: $url"
    else
        curl -fsSL -o "$tmp/$filename" "$url" || die "download failed: $url"
    fi

    # The release carries checksums; verify before unpacking.
    curl -fsSL -o "$tmp/SHA256SUMS" \
        "https://github.com/$REPO/releases/download/$tag/SHA256SUMS" \
        || die "no SHA256SUMS for $tag; refusing to install unverified"
    verify_checksum "$tmp/$filename" "$tmp/SHA256SUMS" "$filename"

    command -v tar >/dev/null 2>&1 || die "'tar' is required to install $APP"
    tar -xzf "$tmp/$filename" -C "$tmp"

    install_binary "$tmp/$APP"
fi

# How to put the install directory on PATH depends on the shell: fish uses `set -gx`, POSIX
# shells use `export`.
current_shell="$(basename "${SHELL:-sh}")"
if [[ "$current_shell" == "fish" ]]; then
    path_line="set -gx PATH \"$INSTALL_DIR\" \$PATH"
else
    path_line="export PATH=\"$INSTALL_DIR:\$PATH\""
fi

add_to_path() {
    local file="$1"
    if grep -Fqx "# $APP" "$file" 2>/dev/null; then
        info "$file already has an $APP entry; leaving it alone"
    elif [[ -w "$file" ]]; then
        printf '\n# %s\n%s\n' "$APP" "$path_line" >>"$file"
        info "added $APP to \$PATH in $file"
    else
        warn "couldn't write $file; add this yourself:"
        printf '  %s\n' "$path_line"
    fi
}

if [[ "$no_modify_path" == false && "$system_install" == false ]]; then
    XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
    case "$current_shell" in
        fish) config_files="$HOME/.config/fish/config.fish" ;;
        zsh) config_files="${ZDOTDIR:-$HOME}/.zshrc ${ZDOTDIR:-$HOME}/.zshenv $XDG_CONFIG_HOME/zsh/.zshrc" ;;
        bash) config_files="$HOME/.bashrc $HOME/.bash_profile $HOME/.profile $XDG_CONFIG_HOME/bash/.bashrc" ;;
        *) config_files="$HOME/.profile $XDG_CONFIG_HOME/bash/.bashrc" ;;
    esac
    config_file=""
    for file in $config_files; do
        if [[ -f "$file" ]]; then
            config_file="$file"
            break
        fi
    done
    if [[ -n "$config_file" ]]; then
        add_to_path "$config_file"
    else
        warn "no shell config found; add this yourself:"
        printf '  %s\n' "$path_line"
    fi
fi

# GitHub Actions uses this instead of a shell config.
if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
    echo "$INSTALL_DIR" >>"$GITHUB_PATH"
fi

platform=""
if [[ -n "$os" && -n "$arch" ]]; then
    platform=" ($os-$arch)"
fi

printf '\n'
printf "${EMBER}  ██  ${NC}${MUTED}IroriOS${NC}\n"
printf "${MUTED}  ██  installed ${NC}%s%s\n" "${version:-local}" "$platform"
printf '\n'
info "start it with:"
printf '  %s run\n' "$APP"
printf '\n'
info "then open http://127.0.0.1:8480"
printf '\n'
