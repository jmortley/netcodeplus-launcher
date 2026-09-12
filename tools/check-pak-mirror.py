#!/usr/bin/env python
"""Verify the paks-328 mirror still agrees with the live signed manifest.

WHY THIS EXISTS
  A pak roll has two halves. The manifest re-pins each lane (what the LAUNCHER
  serves), and the paks-328 release is updated to match (what HUB ADMINS copy
  into Game.ini, and the fallback host if UTCC is unreachable). Nothing links
  them, so a roll that forgets the second half leaves a stale redirect block
  public with no alarm.

  That is not cosmetic. A hub redirect whose PackageChecksum does not match the
  pak a client already has causes the client to re-download the hub's copy over
  it — so a stale block actively downgrades players back onto old paks, fighting
  the launcher that just gave them the right ones. It happened on seq 79
  (2026-09-11): four lanes sat stale for ~24h and an admin pasted them.

WHAT IT CHECKS, per pak lane
  * the release body's PackageChecksum == the md5 embedded in the manifest URL
  * the release body's PackageURL      == the manifest's URL (host + path)
  * the attached .pak asset's size      == the manifest's size_bytes
  * every manifest lane appears in the body, and the body has no extra lanes

Exit code 0 = consistent, 1 = drift (or the manifest/release could not be read).

Usage:
    python tools/check-pak-mirror.py
    python tools/check-pak-mirror.py --json
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

MANIFEST_REPO = "jmortley/netcodeplus-launcher"
MANIFEST_TAG = "updates-latest"
MIRROR_REPO = "jmortley/NetcodePlusUT4"
MIRROR_TAG = "paks-328"
REDIRECT_RE = re.compile(
    r'PackageName="(?P<name>[^"]+)"'
    r'.*?PackageURL="(?P<url>[^"]+)"'
    r'.*?PackageChecksum="(?P<md5>[0-9a-f]{32})"'
)
URL_MD5_RE = re.compile(r"/redirect/\d+/([0-9a-f]{32})/")


def _gh(*args: str) -> str:
    done = subprocess.run(("gh",) + args, capture_output=True, text=True)
    if done.returncode != 0:
        raise SystemExit(f"gh {' '.join(args)} failed:\n{done.stderr.strip()}")
    return done.stdout


def live_manifest() -> dict:
    with tempfile.TemporaryDirectory() as tmp:
        _gh("release", "download", MANIFEST_TAG, "--repo", MANIFEST_REPO,
            "--pattern", "manifest.json", "--dir", tmp, "--clobber")
        return json.loads((Path(tmp) / "manifest.json").read_text(encoding="utf-8"))


def mirror_release() -> tuple[str, dict[str, int]]:
    payload = json.loads(_gh("release", "view", MIRROR_TAG, "--repo", MIRROR_REPO,
                             "--json", "body,assets"))
    return payload["body"], {a["name"]: a["size"] for a in payload["assets"]}


def audit() -> tuple[dict, list[dict]]:
    manifest = live_manifest()
    body, asset_sizes = mirror_release()
    advertised = {m.group("name"): m for m in REDIRECT_RE.finditer(body)}
    rows = []
    for lane, entry in manifest["channels"]["stable"]["paks"].items():
        filename = entry["pak_filename"]
        package = filename[:-4]
        want_md5 = URL_MD5_RE.search(entry["url"]).group(1)
        # The manifest stores a full URL; the redirect line stores it protocol-less.
        want_url = entry["url"].split("//", 1)[1]
        line = advertised.pop(package, None)
        problems = []
        if line is None:
            problems.append("missing from the redirect block")
        else:
            if line.group("md5") != want_md5:
                problems.append(f"body md5 {line.group('md5')} != manifest {want_md5}")
            if line.group("url") != want_url:
                problems.append(f"body url {line.group('url')} != manifest {want_url}")
        size = asset_sizes.get(filename)
        if size is None:
            problems.append("no .pak asset attached to the release")
        elif size != entry["size_bytes"]:
            problems.append(f"asset {size} bytes != manifest {entry['size_bytes']}")
        rows.append({"lane": lane, "version": entry["version"], "problems": problems})
    for orphan in advertised:
        rows.append({"lane": f"({orphan})", "version": "-",
                     "problems": ["in the redirect block but not in the manifest"]})
    return manifest, rows


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--json", action="store_true", help="machine-readable output")
    args = ap.parse_args()

    manifest, rows = audit()
    drift = [r for r in rows if r["problems"]]

    if args.json:
        print(json.dumps({"sequence": manifest["sequence"],
                          "generated_at": manifest["generated_at"],
                          "consistent": not drift, "lanes": rows}, indent=2))
        return 1 if drift else 0

    print(f"live manifest sequence {manifest['sequence']} generated {manifest['generated_at']}")
    print(f"mirror {MIRROR_REPO} {MIRROR_TAG}\n")
    for row in rows:
        status = "OK" if not row["problems"] else "DRIFT"
        print(f"  {status:5} {row['lane']:15} {row['version']}")
        for problem in row["problems"]:
            print(f"        - {problem}")
    if drift:
        print(f"\nDRIFT on {len(drift)} lane(s). Hub admins copying the redirect block are being handed "
              f"checksums that do not match what the launcher installs, which downgrades their players.")
        print("Fix: re-upload the changed .pak files to the release and regenerate its body from the "
              "live manifest, then re-run this check.")
        return 1
    print("\nCONSISTENT: every lane agrees on redirect checksum, redirect URL and asset size.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
