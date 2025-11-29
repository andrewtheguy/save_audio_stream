#!/usr/bin/env python3
"""
Release script that:
1. Validates git state (clean, on main, synced with remote)
2. Calculates the next patch version
3. Triggers GitHub Actions workflow which bumps version and builds

Requires Python 3.11+ (for tomllib)
"""

import subprocess
import sys

if sys.version_info < (3, 11):
    print("Error: Python 3.11+ is required (for tomllib)", file=sys.stderr)
    sys.exit(1)

import tomllib
from pathlib import Path


def run_cmd(cmd: list[str], check: bool = True, capture: bool = True) -> subprocess.CompletedProcess:
    """Run a command and return the result."""
    return subprocess.run(cmd, check=check, capture_output=capture, text=True)


def error_exit(msg: str) -> None:
    """Print error message and exit."""
    print(f"Error: {msg}", file=sys.stderr)
    sys.exit(1)


def check_git_clean() -> None:
    """Check that working directory is clean."""
    print("Checking git status...")
    result = run_cmd(["git", "status", "--porcelain"])
    if result.stdout.strip():
        error_exit("Working directory is not clean. Please commit or stash changes first.")


def check_on_main() -> None:
    """Check that we're on the main branch."""
    result = run_cmd(["git", "branch", "--show-current"])
    branch = result.stdout.strip()
    if branch != "main":
        error_exit(f"Not on main branch (currently on '{branch}'). Please switch to main first.")


def check_synced_with_remote() -> None:
    """Check that local main is synced with remote."""
    print("Fetching from remote...")
    run_cmd(["git", "fetch", "origin"])

    local = run_cmd(["git", "rev-parse", "HEAD"]).stdout.strip()
    remote = run_cmd(["git", "rev-parse", "origin/main"]).stdout.strip()

    if local != remote:
        error_exit("Local main is not synced with origin/main. Please pull/push first.")


def get_current_version(cargo_toml: Path) -> str:
    """Extract current version from Cargo.toml [package] section."""
    with cargo_toml.open("rb") as f:
        data = tomllib.load(f)
    try:
        return data["package"]["version"]
    except KeyError:
        error_exit("Could not find version in [package] section of Cargo.toml")


def bump_patch_version(version: str) -> str:
    """Increment the patch version (X.Y.Z -> X.Y.Z+1)."""
    parts = version.split(".")
    if len(parts) != 3:
        error_exit(f"Invalid version format: {version}")
    parts[2] = str(int(parts[2]) + 1)
    return ".".join(parts)


def trigger_workflow(new_version: str) -> None:
    """Trigger the GitHub Actions workflow with the new version."""
    print("Triggering GitHub Actions workflow...")
    run_cmd([
        "gh", "workflow", "run", "build.yml",
        "--ref", "main",
        "-f", f"version={new_version}",
        "-f", "bump_version=true",
    ])


def main() -> None:
    # Find project root (where Cargo.toml is)
    script_dir = Path(__file__).parent
    project_root = script_dir.parent
    cargo_toml = project_root / "Cargo.toml"

    if not cargo_toml.exists():
        error_exit(f"Cargo.toml not found at {cargo_toml}")

    # Validate git state
    check_git_clean()
    check_on_main()
    check_synced_with_remote()

    # Get and bump version
    current_version = get_current_version(cargo_toml)
    new_version = bump_patch_version(current_version)

    print(f"Current version: {current_version}")
    print(f"New version: {new_version}")

    # Ask for confirmation
    response = input("\nProceed with release? [y/N] ").strip().lower()
    if response != "y":
        print("Aborted.")
        sys.exit(0)

    # Trigger workflow (workflow will bump version, commit, and build)
    trigger_workflow(new_version)

    print(f"\nDone! Workflow triggered for version {new_version}")
    print("The workflow will bump the version in Cargo.toml and build the release.")


if __name__ == "__main__":
    main()
