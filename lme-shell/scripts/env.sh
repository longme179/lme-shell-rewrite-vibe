# SPDX-License-Identifier: GPL-3.0-or-later
# Build environment for the lme-shell workspace in this sandbox.
# On a real Fedora machine (dnf packages from README) these pkg-config
# shims are unnecessary: system libwayland-client/xkbcommon are found normally.
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/target}"
export CARGO_INCREMENTAL=0
if [ -d /home/z/work/pkgconfig-fake ]; then
  export PKG_CONFIG_PATH="/home/z/work/pkgconfig-fake/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
  export RUSTFLAGS="-L /home/z/work/pkgconfig-fake/lib ${RUSTFLAGS:-}"
fi
