#!/bin/sh
# Prints the Rust and Zig versions this source tree builds with, as GitHub step outputs, so the
# fork's workflows follow upstream toolchain bumps after a merge.
set -eu
rust="$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)"
zig="$(sed -n 's/.*minimum_zig_version = "\(.*\)".*/\1/p' vendor/libghostty-vt/build.zig.zon)"
if [ -z "$rust" ] || [ -z "$zig" ]; then
    echo "can't read the Rust version from rust-toolchain.toml or the Zig version from vendor/libghostty-vt/build.zig.zon" >&2
    exit 1
fi
echo "rust=$rust"
echo "zig=$zig"
