#!/usr/bin/env python3
"""The one definition of what this project can call a release version.

A releasable version has to satisfy two independent grammars, so this checks
against both rather than against a summary of them:

  * Cargo's, because the version lives in Cargo.toml. Cargo implements SemVer
    2.0.0 through the `semver` crate and enforces it strictly.
  * The OCI distribution spec's tag grammar, because the release pushes a
    container image tagged from the version. This is the narrower of the two.

Run with no arguments to print the validated version from Cargo.toml, or with
one argument to validate that string instead. Either way a version that cannot
be released exits non-zero with the reason.

Plain python3 (3.11+ for tomllib) and no dependencies: this runs from
packaging/build-tarball.sh and from CI runners, neither of which has uv.
"""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

# SemVer 2.0.0, verbatim from semver.org. Verified against cargo itself, which
# rejects every one of these that a looser pattern would wave through:
#
#   01.2.3      leading zero in a numeric field
#   1.2         missing patch field
#   1.2.3.4     a fourth field
#   1.2.3-      empty pre-release identifier
#   1.2.3-01    leading zero in a numeric pre-release identifier
#   1.2.3-RC_1  underscore, which is not in [0-9A-Za-z-]
#
# `\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?` accepts four of those six, and the first
# thing that would notice is `cargo build` — long after the release workflow has
# created the tag and drafted the release.
SEMVER = re.compile(
    r"^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)"
    r"(?:-(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)"
    r"(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*)?"
    r"(?:\+(?P<build>[0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$"
)

# An OCI image tag. Applied to the tag the release actually constructs rather
# than to the version on its own, because that whole string is what a registry
# has to accept — including the `v` prefix and the arch suffix that eat into the
# 128-character budget.
OCI_TAG = re.compile(r"^[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}$")

# The widest tag built from the version: .github/workflows/release.yml pushes
# ghcr.io/<owner>/save_audio_stream:v<version>-<arch>.
IMAGE_TAG_TEMPLATE = "v{version}-amd64"


class InvalidVersion(ValueError):
    """A version Cargo or a container registry would refuse."""


def validate(version: str) -> str:
    """Return `version` unchanged, or raise InvalidVersion explaining why not."""
    match = SEMVER.match(version)
    if not match:
        raise InvalidVersion(
            f"{version!r} is not a SemVer 2.0.0 version, which is what Cargo requires"
        )

    # The one place the two grammars genuinely disagree. Cargo accepts build
    # metadata — `cargo build` is happy with 1.2.3+build.5 — but an OCI tag may
    # not contain '+' at all, and the failure would land in the `docker` job,
    # which runs after `release` has already published the tag and its assets.
    # Rejected rather than sanitized: a container tag that silently disagrees
    # with the git tag is worse than a release that refuses to start.
    if match.group("build"):
        raise InvalidVersion(
            f"{version!r} carries SemVer build metadata, which this release cannot "
            f"carry: a container image tag may not contain '+'"
        )

    tag = IMAGE_TAG_TEMPLATE.format(version=version)
    if not OCI_TAG.match(tag):
        raise InvalidVersion(
            f"{version!r} is valid SemVer, but the image tag built from it, {tag!r}, "
            f"is not: an OCI tag must match [a-zA-Z0-9_][a-zA-Z0-9._-]{{0,127}}"
        )

    return version


def version_from_manifest(cargo_toml: Path) -> str:
    """The declared version, straight from the manifest.

    A real TOML parse rather than grep/sed, so a reordered file or a dependency
    that happens to have a `version =` line cannot change the answer. `[package]`,
    not `[workspace.package]`: this is a single crate.
    """
    with cargo_toml.open("rb") as f:
        return tomllib.load(f)["package"]["version"]


def main(argv: list[str]) -> int:
    if len(argv) > 2:
        print("usage: release_version.py [VERSION]", file=sys.stderr)
        return 2

    if len(argv) == 2:
        version = argv[1]
    else:
        # Relative to this file, not the working directory, so callers do not
        # have to care where they run it from.
        version = version_from_manifest(Path(__file__).resolve().parent.parent / "Cargo.toml")

    try:
        print(validate(version))
    except InvalidVersion as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
