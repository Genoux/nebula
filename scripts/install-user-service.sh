#!/usr/bin/env bash
set -euo pipefail

project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
staging_dir=''
binary_dir=''
while (($#)); do
    case "$1" in
        --destdir) staging_dir=${2:?missing destination}; shift 2 ;;
        --bin-dir) binary_dir=${2:?missing binary directory}; shift 2 ;;
        --help)
            printf '%s\n' 'usage: install-user-service.sh [--bin-dir PATH] [--destdir PATH]'
            printf '%s\n' 'installs files and reloads user units; never starts or enables the service'
            exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
if [[ $(id -u) == 0 ]]; then
    printf '%s\n' 'run as your normal user, without sudo' >&2
    exit 1
fi
if [[ -z "$binary_dir" ]]; then
    binary_dir="$project_dir/.work/release-build/release"
    cargo build --manifest-path "$project_dir/Cargo.toml" --workspace --release --locked --target-dir "$project_dir/.work/release-build"
fi
for binary in nebula nebula-ui; do
    [[ -x "$binary_dir/$binary" ]] || { printf 'missing binary: %s\n' "$binary_dir/$binary" >&2; exit 1; }
done
if [[ -n "$staging_dir" ]]; then
    [[ "$staging_dir" == /* ]] || { printf '%s\n' '--destdir must be absolute' >&2; exit 2; }
    install_home="$staging_dir"
else
    install_home="$HOME"
fi
install -d -m 755 "$install_home/.local/bin" "$install_home/.config/systemd/user"
install -d -m 700 "$install_home/.config/nebula"
install -m 755 "$binary_dir/nebula" "$install_home/.local/bin/nebula"
install -m 755 "$binary_dir/nebula-ui" "$install_home/.local/bin/nebula-ui"
install -m 644 "$project_dir/systemd/nebula.service" "$install_home/.config/systemd/user/nebula.service"
if [[ ! -e "$install_home/.config/nebula/config.toml" ]]; then
    install -m 600 "$project_dir/config/nebula.example.toml" "$install_home/.config/nebula/config.toml"
fi
if [[ -z "$staging_dir" ]]; then
    systemctl --user daemon-reload
fi
printf '%s\n' 'installed; service remains disabled unless you previously enabled it'
printf '%s\n' 'next: configure the key, train the wake phrase, and run nebula --check'
