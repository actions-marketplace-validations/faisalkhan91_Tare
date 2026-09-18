#!/usr/bin/env python3
"""Validate the version contract shared by source, desktop bundles, and tags."""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--tag",
        help="release tag to validate (must be exactly v<workspace version>)",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    version = workspace["workspace"]["package"]["version"]
    errors: list[str] = []

    for relative in ("tare-tauri/tauri.conf.json", "tare-tauri/tauri.release.conf.json"):
        config = json.loads((ROOT / relative).read_text(encoding="utf-8"))
        if config.get("version") != version:
            errors.append(f"{relative}: version {config.get('version')!r} != {version!r}")

    for manifest_path in sorted(ROOT.glob("tare-*/Cargo.toml")):
        manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
        package_version = manifest["package"].get("version")
        if package_version == {"workspace": True}:
            continue
        if package_version != version:
            relative = manifest_path.relative_to(ROOT)
            errors.append(f"{relative}: package version {package_version!r} != {version!r}")

    expected_tag = f"v{version}"
    if args.tag is not None and args.tag != expected_tag:
        errors.append(f"release tag {args.tag!r} must be exactly {expected_tag!r}")

    if errors:
        for error in errors:
            print(f"release metadata: ERROR: {error}", file=sys.stderr)
        return 1

    tag_suffix = f", tag {args.tag}" if args.tag is not None else ""
    print(f"release metadata: ok (version {version}{tag_suffix})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
