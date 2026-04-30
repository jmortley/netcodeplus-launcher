# Security Policy

## Trust Model

The NetcodePlus launcher uses an offline-signed manifest model so that
update integrity does not depend on TLS, on GitHub's hosting, or on any
CDN being uncompromised.

- A single Ed25519 root **public** key is compiled into the launcher
  binary at build time.
- The release manifest is signed offline (minisign / Ed25519) using the
  matching **private** key, which lives on a hardware key (e.g. YubiKey)
  and is never present on a build server.
- Every pak entry in the manifest carries a SHA-256 hash. The launcher
  refuses to install a pak whose downloaded bytes do not match.
- Manifests carry an `expires_at` timestamp. Stale signed manifests are
  rejected even if the signature itself is still cryptographically
  valid.
- Manifests carry a `min_launcher_version`. A launcher older than the
  required version refuses to apply the update and prompts the user to
  upgrade the launcher first.

### What this defends against

| Threat | Mitigation |
|---|---|
| GitHub account or organisation compromise | Manifest signing key is offline; an attacker who controls the host cannot forge a signed manifest. |
| MITM on the manifest or pak download | Manifest signature verified against the compiled-in public key before parsing JSON. |
| Replay of an old, signed manifest | `expires_at` is enforced before any install action. |
| Pak bytes swapped at the host | SHA-256 of the downloaded file is compared against the value in the signed manifest, before atomic replace. |
| Downgrade attack against the launcher | `min_launcher_version` is checked before applying an update. |

### What this does NOT defend against

- A malicious launcher binary itself. The trust root is the binary, so
  the launcher binary's own distribution must be protected by code
  signing (planned via [SignPath Foundation](https://signpath.org/)).
- Compromise of the offline signing key.
- Vulnerabilities in Unreal Tournament 4 itself, or in third-party paks
  that the launcher merely keeps up to date.

## Reporting a Vulnerability

Please report security issues **privately** to:

- Email: `<security-contact-tbd>`
- GPG fingerprint: `<TBD>`

Please do **not** open a public GitHub issue for security reports.

We aim to acknowledge reports within 72 hours and to ship a fix within
30 days for high-severity issues. Coordinated disclosure preferred.

## Supported Versions

Only the latest released version of the launcher receives security
fixes. Older versions are out of scope.
