#!/usr/bin/env python3
"""
Update Cargo package version and refresh Cargo.lock for the workspace crate.
Intended for CI usage to ensure both manifest and lock step together.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import tomllib  # Python 3.11+

def error(msg: str) -> None:
    print(f"Error: {msg}", file=sys.stderr)
    sys.exit(1)


def load_package_metadata(cargo_toml: Path) -> tuple[str, str]:
    """Return (name, version) from the [package] section."""
    with cargo_toml.open("rb") as f:
        data = tomllib.load(f)

    try:
        package = data["package"]
        return package["name"], package["version"]
    except KeyError:
        error("Cargo.toml is missing [package].name or [package].version")
        raise


def update_manifest_version(cargo_toml: Path, new_version: str) -> str:
    """
    Rewrite only the version line inside the [package] section while preserving
    indentation and any trailing inline comment.
    """
    lines = cargo_toml.read_text().splitlines()

    try:
        package_start = next(i for i, line in enumerate(lines) if line.strip() == "[package]")
    except StopIteration:
        error("Could not find [package] section in Cargo.toml")

    version_line_idx = None
    for i in range(package_start + 1, len(lines)):
        stripped = lines[i].strip()
        if stripped.startswith("[") and not stripped.startswith("[["):  # next section
            break
        if stripped.startswith("#") or not stripped:
            continue
        if stripped.startswith("version"):
            version_line_idx = i
            break

    if version_line_idx is None:
        error("Could not find version field in [package] section of Cargo.toml")

    line = lines[version_line_idx]
    indent = line[: len(line) - len(line.lstrip(" \t"))]
    trailing_comment = ""
    if "#" in line:
        before_comment, _, comment = line.partition("#")
        trailing_comment = f"#{comment}" if comment else ""
        line = before_comment

    new_line = f'{indent}version = "{new_version}"'
    if trailing_comment:
        new_line = f"{new_line} {trailing_comment}"

    lines[version_line_idx] = new_line
    cargo_toml.write_text("\n".join(lines) + "\n")
    return new_version


def update_lockfile(package_name: str, new_version: str) -> None:
    """Ensure Cargo.lock reflects the new package version."""
    subprocess.run(
        ["cargo", "update", "-p", package_name, "--precise", new_version],
        check=True,
    )


def main() -> None:
    if len(sys.argv) != 2:
        error("Usage: bump_version.py <new-version>")

    new_version = sys.argv[1].strip()
    if not new_version:
        error("Version cannot be empty")

    project_root = Path(__file__).resolve().parent.parent
    cargo_toml = project_root / "Cargo.toml"
    cargo_lock = project_root / "Cargo.lock"

    if not cargo_toml.exists():
        error(f"Cargo.toml not found at {cargo_toml}")

    package_name, current_version = load_package_metadata(cargo_toml)
    if new_version == current_version:
        print(f"Version already set to {new_version}, no changes needed.")
        return

    print(f"Updating {package_name} version: {current_version} -> {new_version}")
    update_manifest_version(cargo_toml, new_version)

    if cargo_lock.exists():
        update_lockfile(package_name, new_version)
        print("Updated Cargo.lock")
    else:
        print("Cargo.lock not found; skipping lockfile update")


if __name__ == "__main__":
    main()
