#!/usr/bin/env bash
# Hermetic contract tests for scripts/install.sh. A fake curl serves local release assets, so this
# exercises URL selection, verification, archive validation, and installation without a network.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TEST_ROOT="$(mktemp -d 2>/dev/null || mktemp -d -t tare-install-test)"
trap 'rm -rf "$TEST_ROOT"' EXIT

RELEASE_DIR="$TEST_ROOT/release"
PAYLOAD_DIR="$TEST_ROOT/payload"
MOCK_BIN="$TEST_ROOT/bin"
REQUEST_LOG="$TEST_ROOT/requests.log"
mkdir -p "$RELEASE_DIR" "$PAYLOAD_DIR" "$MOCK_BIN"

case "$(uname -s):$(uname -m)" in
  Darwin:x86_64) TRIPLE="x86_64-apple-darwin" ;;
  Darwin:arm64 | Darwin:aarch64) TRIPLE="aarch64-apple-darwin" ;;
  Linux:x86_64 | Linux:amd64) TRIPLE="x86_64-unknown-linux-gnu" ;;
  Linux:arm64 | Linux:aarch64) TRIPLE="aarch64-unknown-linux-gnu" ;;
  *) printf 'install test: unsupported test host\n' >&2; exit 1 ;;
esac

ASSET_NAME="tare-${TRIPLE}.tar.gz"
ASSET="$RELEASE_DIR/$ASSET_NAME"
CHECKSUM="$ASSET.sha256"

printf '#!/bin/sh\nprintf "tare installer fixture\\n"\n' > "$PAYLOAD_DIR/tare"
chmod +x "$PAYLOAD_DIR/tare"

write_archive() {
  tar -czf "$ASSET" -C "$PAYLOAD_DIR" "$@"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$ASSET" > "$CHECKSUM"
  else
    shasum -a 256 "$ASSET" > "$CHECKSUM"
  fi
}

cat > "$MOCK_BIN/curl" <<'SH'
#!/bin/sh
set -eu
url=""
output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) shift; output="$1" ;;
    http://* | https://*) url="$1" ;;
  esac
  shift
done
[ -n "$url" ] && [ -n "$output" ] || exit 2
printf '%s\n' "$url" >> "$TARE_INSTALL_TEST_REQUEST_LOG"
source_file="$TARE_INSTALL_TEST_RELEASE_DIR/${url##*/}"
[ -f "$source_file" ] || exit 22
cp "$source_file" "$output"
SH
chmod +x "$MOCK_BIN/curl"

run_installer() {
  install_dir="$1"
  shift
  env \
    PATH="$MOCK_BIN:$PATH" \
    TARE_INSTALL_TEST_RELEASE_DIR="$RELEASE_DIR" \
    TARE_INSTALL_TEST_REQUEST_LOG="$REQUEST_LOG" \
    TARE_REPO="example/tare" \
    TARE_VERSION="v9.8.7" \
    TARE_INSTALL_DIR="$install_dir" \
    TARE_ALLOW_UNVERIFIED=0 \
    "$@" \
    sh "$ROOT/scripts/install.sh"
}

fail() {
  printf 'install test: FAIL: %s\n' "$*" >&2
  exit 1
}

# Verified happy path: exact versioned URLs, byte-identical executable payload.
write_archive tare
: > "$REQUEST_LOG"
run_installer "$TEST_ROOT/verified/bin" > "$TEST_ROOT/verified.out" 2>&1
cmp "$PAYLOAD_DIR/tare" "$TEST_ROOT/verified/bin/tare" >/dev/null || fail "installed payload changed"
[ -x "$TEST_ROOT/verified/bin/tare" ] || fail "installed payload is not executable"
grep -Fxq "https://github.com/example/tare/releases/download/v9.8.7/$ASSET_NAME" "$REQUEST_LOG" || fail "asset URL mismatch"
grep -Fxq "https://github.com/example/tare/releases/download/v9.8.7/$ASSET_NAME.sha256" "$REQUEST_LOG" || fail "checksum URL mismatch"
grep -Fq "checksum ok" "$TEST_ROOT/verified.out" || fail "successful verification was not reported"

# A mismatched checksum must fail before writing an installed binary.
printf '%064d  %s\n' 0 "$ASSET_NAME" > "$CHECKSUM"
if run_installer "$TEST_ROOT/mismatch/bin" > "$TEST_ROOT/mismatch.out" 2>&1; then
  fail "checksum mismatch was accepted"
fi
[ ! -e "$TEST_ROOT/mismatch/bin/tare" ] || fail "checksum failure wrote a binary"
grep -Fq "checksum mismatch" "$TEST_ROOT/mismatch.out" || fail "checksum failure was unclear"

# Missing checksums fail closed, but the documented explicit override remains functional.
write_archive tare
mv "$CHECKSUM" "$TEST_ROOT/checksum.saved"
if run_installer "$TEST_ROOT/missing/bin" > "$TEST_ROOT/missing.out" 2>&1; then
  fail "missing checksum was accepted without an override"
fi
[ ! -e "$TEST_ROOT/missing/bin/tare" ] || fail "missing checksum wrote a binary"
grep -Fq "refusing to install an unverified binary" "$TEST_ROOT/missing.out" || fail "missing-checksum failure was unclear"
run_installer "$TEST_ROOT/unverified/bin" TARE_ALLOW_UNVERIFIED=1 > "$TEST_ROOT/unverified.out" 2>&1
[ -x "$TEST_ROOT/unverified/bin/tare" ] || fail "explicit unverified override did not install"
grep -Fq "continuing without verification" "$TEST_ROOT/unverified.out" || fail "override warning was not shown"

# Even a checksum-valid archive is rejected unless it contains only the expected top-level binary.
mv "$TEST_ROOT/checksum.saved" "$CHECKSUM"
printf 'unexpected\n' > "$PAYLOAD_DIR/extra.txt"
write_archive tare extra.txt
if run_installer "$TEST_ROOT/layout/bin" > "$TEST_ROOT/layout.out" 2>&1; then
  fail "multi-file archive was accepted"
fi
[ ! -e "$TEST_ROOT/layout/bin/tare" ] || fail "invalid archive wrote a binary"
grep -Fq "exactly one top-level 'tare' binary" "$TEST_ROOT/layout.out" || fail "archive-layout failure was unclear"

# A single symlink named `tare` also fails; installers must never place a release-controlled link.
rm "$PAYLOAD_DIR/extra.txt"
mv "$PAYLOAD_DIR/tare" "$PAYLOAD_DIR/tare.regular"
ln -s /bin/sh "$PAYLOAD_DIR/tare"
write_archive tare
if run_installer "$TEST_ROOT/symlink/bin" > "$TEST_ROOT/symlink.out" 2>&1; then
  fail "symlink payload was accepted"
fi
[ ! -e "$TEST_ROOT/symlink/bin/tare" ] || fail "symlink archive wrote an installed entry"
grep -Fq "regular 'tare' binary" "$TEST_ROOT/symlink.out" || fail "symlink failure was unclear"

printf 'install test: ok (%s)\n' "$TRIPLE"
