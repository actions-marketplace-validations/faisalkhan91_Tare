#!/bin/sh
# Tare one-line installer — dependency-free, no Rust toolchain required.
#
#   curl -LsSf https://raw.githubusercontent.com/tare-dev/tare/main/scripts/install.sh | sh
#
# Detects your OS/arch, downloads the matching prebuilt `tare` CLI binary from the GitHub
# release, verifies its published SHA-256, and drops it on your PATH. It fetches one binary and
# runs nothing remote — local-first is preserved. Override behavior with env vars:
#
#   TARE_REPO=owner/tare      # which GitHub repo to pull releases from
#   TARE_VERSION=v0.1.0       # a specific tag (default: latest)
#   TARE_INSTALL_DIR=~/.local/bin   # where to drop the binary
#   TARE_ALLOW_UNVERIFIED=1   # proceed even if the SHA-256 can't be verified (NOT recommended)
#
# By default the install FAILS CLOSED: if the release ships no checksum, or no sha256 tool is
# present, it aborts rather than install an unverified binary. Set TARE_ALLOW_UNVERIFIED=1 to
# override (e.g. an old release predating checksums, or a truly minimal environment).
#
# To build from source instead (contributors), see "Build from source" in the README.
set -eu

REPO="${TARE_REPO:-tare-dev/tare}"
VERSION="${TARE_VERSION:-latest}"

say() { printf '%s\n' "$*" >&2; }
err() {
  say "error: $*"
  exit 1
}
need() { command -v "$1" >/dev/null 2>&1 || err "missing required tool: $1"; }

# Prefer ~/.local/bin (no sudo); fall back to /usr/local/bin if the former isn't on PATH and the
# latter is writable. The user can always override with TARE_INSTALL_DIR.
default_install_dir() {
  if [ -n "${TARE_INSTALL_DIR:-}" ]; then
    printf '%s' "$TARE_INSTALL_DIR"
    return
  fi
  case ":${PATH}:" in
    *":${HOME}/.local/bin:"*) printf '%s' "${HOME}/.local/bin" ;;
    *) if [ -w /usr/local/bin ]; then printf '%s' /usr/local/bin; else printf '%s' "${HOME}/.local/bin"; fi ;;
  esac
}

# Map uname output to the Rust target triple the release workflow publishes.
target_triple() {
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Darwin) plat="apple-darwin" ;;
    Linux) plat="unknown-linux-gnu" ;;
    *) err "unsupported OS '$os' — on Windows use the .msi from the GitHub release, or build from source" ;;
  esac
  case "$arch" in
    x86_64 | amd64) cpu="x86_64" ;;
    arm64 | aarch64) cpu="aarch64" ;;
    *) err "unsupported architecture '$arch' — build from source (see README)" ;;
  esac
  printf '%s-%s' "$cpu" "$plat"
}

# Download $1 to $2 using whichever fetcher is present.
fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -LsSf "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$2" "$1"
  else
    err "need curl or wget to download"
  fi
}

verify_sha() {
  # $1 = file, $2 = expected-sha file (format: "<hex>  <name>"). Fails closed: a missing checksum
  # tool aborts the install unless TARE_ALLOW_UNVERIFIED=1 is set.
  expected="$(awk '{print $1}' "$2")"
  [ -n "$expected" ] || err "empty checksum in $2"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$1" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$1" | awk '{print $1}')"
  else
    if [ "${TARE_ALLOW_UNVERIFIED:-0}" = "1" ]; then
      say "warning: no sha256 tool found; skipping checksum verification (TARE_ALLOW_UNVERIFIED=1)"
      return 0
    fi
    err "no sha256 tool (sha256sum/shasum) found to verify the download — install one, or re-run with TARE_ALLOW_UNVERIFIED=1 to bypass (NOT recommended)"
  fi
  [ "$actual" = "$expected" ] || err "checksum mismatch for $(basename "$1") (expected $expected, got $actual)"
}

main() {
  need uname
  triple="$(target_triple)"
  asset="tare-${triple}.tar.gz"
  if [ "$VERSION" = "latest" ]; then
    base="https://github.com/${REPO}/releases/latest/download"
  else
    base="https://github.com/${REPO}/releases/download/${VERSION}"
  fi

  tmp="$(mktemp -d 2>/dev/null || mktemp -d -t tare)"
  trap 'rm -rf "$tmp"' EXIT

  say "downloading ${asset} (${VERSION}) from ${REPO}…"
  fetch "${base}/${asset}" "${tmp}/${asset}" || err "download failed — is ${VERSION} published for ${triple}?"
  # Verification is REQUIRED by default (fail closed). Every current release publishes a .sha256;
  # a missing one means a tampered/incomplete download or a pre-checksum release — abort unless the
  # user explicitly opts out.
  if fetch "${base}/${asset}.sha256" "${tmp}/${asset}.sha256" 2>/dev/null && [ -s "${tmp}/${asset}.sha256" ]; then
    verify_sha "${tmp}/${asset}" "${tmp}/${asset}.sha256"
    say "checksum ok"
  elif [ "${TARE_ALLOW_UNVERIFIED:-0}" = "1" ]; then
    say "warning: no published checksum for ${asset}; continuing without verification (TARE_ALLOW_UNVERIFIED=1)"
  else
    err "no published checksum for ${asset} — refusing to install an unverified binary; re-run with TARE_ALLOW_UNVERIFIED=1 to bypass (NOT recommended)"
  fi

  # Release archives are intentionally a single top-level binary. Validate that shape before
  # extraction so a malformed or compromised archive cannot write outside the temporary directory.
  members="$(tar -tzf "${tmp}/${asset}")" || err "invalid archive"
  [ "$members" = "tare" ] || err "archive must contain exactly one top-level 'tare' binary"
  tar -xzf "${tmp}/${asset}" -C "$tmp" || err "extract failed"
  [ -f "${tmp}/tare" ] && [ ! -L "${tmp}/tare" ] || err "archive did not contain a regular 'tare' binary"
  chmod +x "${tmp}/tare"

  dir="$(default_install_dir)"
  mkdir -p "$dir" || err "could not create install directory ${dir}"
  mv "${tmp}/tare" "${dir}/tare" || err "could not install to ${dir} (set TARE_INSTALL_DIR to a writable path)"
  say "installed tare -> ${dir}/tare"

  case ":${PATH}:" in
    *":${dir}:"*) ;;
    *) say "note: ${dir} is not on your PATH — add it: export PATH=\"${dir}:\$PATH\"" ;;
  esac

  say ""
  say "Next: point any agent or script at Tare (no code change) —"
  say "  tare run -- python agent.py"
  say "  tare report --today"
  say "  tare serve   # then open http://127.0.0.1:8788/__tare/"
}

main "$@"
