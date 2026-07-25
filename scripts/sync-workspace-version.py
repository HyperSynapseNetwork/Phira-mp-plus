#!/usr/bin/env python3
"""Keep workspace package versions and Cargo.lock entries consistent."""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PACKAGES = {
    "phira-mp-plus-server": ROOT / "phira-mp-plus-server" / "Cargo.toml",
    "phira-plugin-sdk": ROOT / "phira-plugin-sdk" / "Cargo.toml",
}
LOCK_PATH = ROOT / "Cargo.lock"
VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")


def toml_version(path: Path) -> str:
    match = re.search(r'^version\s*=\s*"([^"]+)"', path.read_text(), re.MULTILINE)
    if match is None:
        raise RuntimeError(f"package version not found: {path}")
    return match.group(1)


def lock_version(text: str, package: str) -> str:
    pattern = re.compile(
        rf'\[\[package\]\]\nname = "{re.escape(package)}"\nversion = "([^"]+)"\n'
    )
    matches = pattern.findall(text)
    if len(matches) != 1:
        raise RuntimeError(
            f"Cargo.lock entry must occur exactly once for {package}; found {len(matches)}"
        )
    return matches[0]


def check(expected: str | None = None) -> str:
    versions = {package: toml_version(path) for package, path in PACKAGES.items()}
    unique = set(versions.values())
    if len(unique) != 1:
        raise RuntimeError(f"workspace package versions differ: {versions}")
    version = next(iter(unique))
    if expected is not None and version != expected:
        raise RuntimeError(f"workspace version {version} != expected {expected}")

    lock_text = LOCK_PATH.read_text()
    lock_versions = {package: lock_version(lock_text, package) for package in PACKAGES}
    mismatches = {package: value for package, value in lock_versions.items() if value != version}
    if mismatches:
        raise RuntimeError(
            f"Cargo.lock workspace versions differ from {version}: {mismatches}"
        )
    print(f"workspace version consistent: {version}")
    return version


def update(version: str) -> None:
    if VERSION_RE.fullmatch(version) is None:
        raise RuntimeError(f"invalid semantic version: {version}")

    for package, path in PACKAGES.items():
        text = path.read_text()
        text, count = re.subn(
            r'^version\s*=\s*"[^"]+"',
            f'version = "{version}"',
            text,
            count=1,
            flags=re.MULTILINE,
        )
        if count != 1:
            raise RuntimeError(f"failed to update package version: {package}")
        path.write_text(text)

    lock_text = LOCK_PATH.read_text()
    for package in PACKAGES:
        pattern = re.compile(
            rf'(\[\[package\]\]\nname = "{re.escape(package)}"\nversion = ")[^"]+("\n)'
        )
        lock_text, count = pattern.subn(rf"\g<1>{version}\2", lock_text, count=1)
        if count != 1:
            raise RuntimeError(f"failed to update Cargo.lock entry: {package}")
    LOCK_PATH.write_text(lock_text)
    check(version)


def main(argv: list[str]) -> int:
    try:
        if argv == ["--check"]:
            check()
        elif len(argv) == 2 and argv[0] == "--check":
            check(argv[1])
        elif len(argv) == 1:
            update(argv[0])
        else:
            print(
                "usage: sync-workspace-version.py VERSION | --check [VERSION]",
                file=sys.stderr,
            )
            return 2
    except (OSError, RuntimeError) as error:
        print(f"version synchronization failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
