# Tare desktop release (manual)

> **This path is not part of the offline gate.** `scripts/ci.sh` builds the Rust workspace and
> frontend assets but never runs `tauri build`, signs, notarizes, publishes, or enables an updater.
> Everything below runs only when cutting a release locally or through the tag-triggered workflow.

The committed `tare-tauri/tauri.conf.json` keeps `bundle.active = false`. Release builds layer
`tauri.release.conf.json` on top with `-c` — **never** flip `active` in the default config (that
would make the gate try to bundle/sign).

## 0. Pre-flight: version + secret scan

1. Reconcile versions before signing. The workspace version, Git tag, `tauri.conf.json`, and
   `tauri.release.conf.json` must match. For example, run
   `python3 scripts/check-release-metadata.py --tag v0.1.0`; the tag workflow performs the same
   check and stops before building if any value differs.
2. Secret scan the tree (must all be clean):
   ```sh
   git grep -nE -e 'sk-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|-----BEGIN ([A-Z0-9 ]+ )?PRIVATE KEY-----' -- ':!*.md' ':!tare-core/src/redact.rs' ':!tare-proxy/tests/proxy_integration.rs' || echo clean
   git ls-files | grep -Ei '\.(p12|pfx)$' || echo clean
   ```
   The proxy/provider keys, Apple credentials, signing private keys, and `.p12/.pfx` certs must
   live in the environment/secret store — never in the repo or in CI logs.

## 1. Build the frontend

```sh
npm --prefix web ci
bash scripts/build-ui.sh                # builds once, then syncs browser + desktop embeds
```

The desktop icon master is `assets/app-icon.svg`; generated platform assets are committed under
`tare-tauri/icons/` because every release runner needs them before bundling. After changing the
master, regenerate them with Tauri CLI (`cargo tauri icon assets/app-icon.svg -o tare-tauri/icons`)
and run `scripts/ci.sh`. The gate verifies every path named by `tauri.release.conf.json` is present
and non-empty, plus the conventional `icons/icon.png` loaded by `generate_context!`.

## 2. Per-platform bundle (with the GUI feature + release config)

```sh
(cd tare-tauri && cargo tauri build --features gui -c tauri.release.conf.json)
```

### macOS — codesign → notarize → staple
```sh
codesign --deep --force --options runtime --sign "Developer ID Application: …" Tare.app
xcrun notarytool submit Tare.dmg --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
  --password "$APPLE_APP_PASSWORD" --wait
xcrun stapler staple Tare.dmg
```

### Windows — Authenticode with an RFC-3161 timestamp
```sh
signtool sign /fd sha256 /tr http://timestamp.digicert.com /td sha256 /a Tare.exe
```

## 3. Tag

After the secret scan is clean and artifacts are signed/notarized/stapled, push the version tag.
`release.yml` triggers only on `v*` tags and compile-checks fresh platform bundles; publishing the
credentialed desktop artifacts remains manual. The standalone CLI assets are attached automatically.

## 4. Standalone CLI binaries

Separate from the signed desktop bundle, the workflow's `cli` job publishes a dependency-free
`tare` CLI binary per platform so users can install Tare without a Rust toolchain.

- Targets built natively per matrix: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
  `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (cross-linked). No frontend build is
  needed — the web viewer assets are already committed under `tare-cli/assets/ui`.
- Each asset is `tare-<target>.tar.gz` (one `tare` binary) plus a sibling `.sha256`.
- `scripts/install.sh` detects OS/arch, downloads the matching asset from the release, verifies the
  checksum, and drops the binary on `PATH`. Before advertising it in the README, publish the
  repository and verify that its raw `scripts/install.sh` URL resolves. The script fetches one
  release binary; override its defaults with `TARE_REPO`, `TARE_VERSION`, or `TARE_INSTALL_DIR`.
- `bash scripts/install.test.sh` exercises the verified install, checksum failure, fail-closed
  missing-checksum behavior, explicit unverified override, URL shape, and archive-layout rejection
  without reaching the network. It is part of `scripts/ci.sh`.
