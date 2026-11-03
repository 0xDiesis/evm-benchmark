#!/bin/sh
# Pin the same Solidity compiler for development, CI and release artifacts.
set -eu

version=0.3.0
commit=e7a91fc70f29dc8d0c236d56a2aa7706dc2555cf
bin_dir=${1:-${FOUNDRY_DIR:-$HOME/.foundry}/bin}

if [ -x "$bin_dir/solar" ] && "$bin_dir/solar" --version | grep -qF "Commit SHA: $commit"; then
    "$bin_dir/solar" --version
    exit 0
fi

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)
        target=aarch64-apple-darwin
        expected=6d213a61324b77d55868795b516661c84ec77b41c60befddb4f0cb7a0168d0b1 ;;
    Darwin-x86_64)
        target=x86_64-apple-darwin
        expected=df93b34f1568d4bd7bd2ddf46d5d5056be76166c99d008c4186166c2dbe8238a ;;
    Linux-aarch64|Linux-arm64)
        target=aarch64-unknown-linux-gnu
        expected=8da0507426756ec175d281d421ea4fb44d02e3fb9f46ff8ff79802c5d547e95c ;;
    Linux-x86_64)
        target=x86_64-unknown-linux-gnu
        expected=15a613746dd30ca772664824d97feddb5c5163ec2380aa556a2d270eacedc5fc ;;
    *) echo "Unsupported Solar platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

temp_root=${TMPDIR:-${RUNNER_TEMP:-${XDG_CACHE_HOME:-$HOME/.cache}/solar}}
mkdir -p "$temp_root"
work=$(mktemp -d "$temp_root/solar-install.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
archive="solar-$target.tar.gz"
curl -fsSL --retry 3 "https://github.com/paradigmxyz/solar/releases/download/v$version/$archive" -o "$work/$archive"
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$work/$archive" | cut -d ' ' -f 1)
else
    actual=$(shasum -a 256 "$work/$archive" | cut -d ' ' -f 1)
fi
if [ "$actual" != "$expected" ]; then
    echo "Solar archive checksum mismatch" >&2
    exit 1
fi
tar -xzf "$work/$archive" -C "$work"
binary=$(find "$work" -type f -name solar)
"$binary" --version | grep -qF "Commit SHA: $commit"
mkdir -p "$bin_dir"
install -m 0755 "$binary" "$bin_dir/solar"
"$bin_dir/solar" --version
