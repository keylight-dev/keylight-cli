#!/usr/bin/env python3
"""Point the Homebrew formula at a new release.

The tap (keylight-dev/homebrew-tap) pins an exact version and a sha256 per
download. Nothing about that updates itself, so without this the tap keeps
serving whatever the last hand-edit left behind — which is worse than having no
tap at all, because `brew install keylight` would silently install a stale CLI.

Run from a keylight-cli checkout, against a checkout of the tap:

    scripts/update-homebrew-formula.py --version 0.1.1 --formula ../homebrew-tap/Formula/keylight.rb

Verify without writing anything:

    scripts/update-homebrew-formula.py --version 0.1.1 --formula ... --dry-run

The release workflow runs this after the binaries job, so the assets it
checksums are the ones that were just published.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
import urllib.error
import urllib.request

REPO = "keylight-dev/keylight-cli"
UA = "keylight-formula-updater"

# Every download the formula pins, keyed by the URL it appears under. The
# formula is the source of truth for which platforms exist; this map only needs
# to know how to build each URL for a given version.
#
# Adding a platform here is the SECOND half of the change, never the first.
# `rewrite` fails if an entry is never referenced by the formula, so a key added
# before the formula points at that URL breaks the release. x86_64-apple-darwin
# landed that way in the v0.1.1 tap PR: the formula's Intel-macOS branch moved
# off the crates.io source build and onto the binary in the same commit as the
# key here. Dropping that source build also dropped `depends_on "rust"`, which
# is what had been stopping test-bot from bottling Intel macOS at all.
DOWNLOADS = {
    "aarch64-apple-darwin": lambda v: (
        f"https://github.com/{REPO}/releases/download/v{v}/keylight-aarch64-apple-darwin"
    ),
    "x86_64-apple-darwin": lambda v: (
        f"https://github.com/{REPO}/releases/download/v{v}/keylight-x86_64-apple-darwin"
    ),
    "x86_64-unknown-linux-gnu": lambda v: (
        f"https://github.com/{REPO}/releases/download/v{v}/keylight-x86_64-unknown-linux-gnu"
    ),
    "crate": lambda v: (
        f"https://static.crates.io/crates/keylight-cli/keylight-cli-{v}.crate"
    ),
}


def sha256_of(url: str) -> str:
    """Download a URL and return its hex sha256.

    Streamed rather than read whole: the binaries are only ~4 MB today, but a
    checksum tool that assumes small inputs is a trap for whoever adds a
    universal binary later.
    """
    request = urllib.request.Request(url, headers={"User-Agent": UA})
    digest = hashlib.sha256()
    with urllib.request.urlopen(request) as response:
        for chunk in iter(lambda: response.read(1 << 16), b""):
            digest.update(chunk)
    return digest.hexdigest()


def rewrite(formula: str, version: str, checksums: dict[str, str]) -> str:
    """Return the formula with its version, URLs, and sha256 lines updated.

    Each sha256 is replaced by position — the one on the line following the URL
    it belongs to — rather than by matching the old digest. Matching old digests
    would silently do nothing when a release reuses one (identical rebuild), and
    "silently did nothing" is the failure this whole script exists to prevent.
    """
    # An explicit `version` is optional and currently absent: every URL carries
    # the version, so Homebrew scans it, and `brew audit` fails the tap's CI for
    # declaring it redundantly. Rewrite it when present rather than requiring it —
    # the URLs below are what actually move the formula to a new release.
    updated = re.sub(
        r'^(\s*version\s+)"[^"]+"', rf'\1"{version}"', formula, count=1, flags=re.MULTILINE
    )

    # Drop any `bottle do` block. Bottles are built per version and hosted under a
    # release named for it, so a block left behind from the previous version sends
    # `brew install` to a URL that does not exist and the install fails outright —
    # worse than having no bottle. Removing it makes the new version install from
    # source until a bottle PR runs against the tap, which is the documented cost
    # of bumping the formula automatically.
    # Consumes the blank line after the block too, or removal leaves two in a row
    # and `brew style` fails the tap's CI on the very commit meant to fix it.
    updated = re.sub(
        r'\n[ \t]*bottle do\n.*?\n[ \t]*end\n\n', '\n', updated, count=1, flags=re.DOTALL
    )

    replaced = set()

    def substitute(match: re.Match[str]) -> str:
        indent, old_url, sha_indent = match.group(1), match.group(2), match.group(3)
        for key, build_url in DOWNLOADS.items():
            new_url = build_url(version)
            # Compare on the filename shape, not the whole URL, so a version
            # bump in the old URL still matches the right download.
            if _same_download(old_url, new_url):
                replaced.add(key)
                return f'{indent}url "{new_url}"\n{sha_indent}sha256 "{checksums[key]}"'
        raise SystemExit(f"error: formula pins a URL this script does not know: {old_url}")

    updated = re.sub(
        r'^([ \t]*)url "([^"]+)"\n([ \t]*)sha256 "[0-9a-f]{64}"',
        substitute,
        updated,
        flags=re.MULTILINE,
    )

    missing = set(DOWNLOADS) - replaced
    if missing:
        raise SystemExit(f"error: formula never referenced: {', '.join(sorted(missing))}")

    return updated


def _same_download(old_url: str, new_url: str) -> bool:
    """Do two URLs refer to the same artifact, ignoring the version in them?"""
    strip = lambda u: re.sub(r"\d+\.\d+\.\d+", "", u)  # noqa: E731 - local, single use
    return strip(old_url) == strip(new_url)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True, help="release version, without a leading v")
    parser.add_argument("--formula", required=True, help="path to Formula/keylight.rb in the tap")
    parser.add_argument(
        "--dry-run", action="store_true", help="print the result instead of writing it"
    )
    args = parser.parse_args()

    version = args.version.lstrip("v")

    checksums = {}
    for key, build_url in DOWNLOADS.items():
        url = build_url(version)
        try:
            checksums[key] = sha256_of(url)
        except urllib.error.HTTPError as error:
            # A 404 here almost always means this ran before the release
            # finished publishing, so say that rather than just the status.
            print(f"error: {url} returned {error.code}", file=sys.stderr)
            print("       is the release published and are the assets uploaded?", file=sys.stderr)
            return 1
        print(f"{key}: {checksums[key]}")

    with open(args.formula, encoding="utf-8") as handle:
        original = handle.read()

    updated = rewrite(original, version, checksums)

    if args.dry_run:
        print(updated)
        return 0

    if updated == original:
        print(f"formula already matches {version}; nothing to write")
        return 0

    with open(args.formula, "w", encoding="utf-8") as handle:
        handle.write(updated)
    print(f"wrote {args.formula} for {version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
