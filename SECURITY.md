# Security

## Reporting a vulnerability

Email `ellery@familia.me`. PGP key below. Please don't open public issues for security-sensitive reports.

## Package signing key

Linux packages (`.deb` and `.rpm`) on each GitHub release are signed with this key:

- **UserID**: `Zerminal <ellery@familia.me>`
- **Fingerprint**: `8C04 4786 1386 07EE BFB4  E04B 3762 B681 02EC 4A8A`

The same fingerprint is published in three places so a pipeline compromise that swaps one will be visibly inconsistent with the others:

1. This file (`SECURITY.md`) in `elleryfamilia/zerminal`.
2. The `README.md` in `elleryfamilia/homebrew-zerminal` (the Homebrew tap).
3. `keys.openpgp.org` (search for `ellery@familia.me`).

**If these three disagree, do not install.** Open an issue or email instead.

### Rotation policy

The signing key is rotated on compromise or on an annual schedule. Rotation is announced at least 30 days in advance via GitHub release notes; the new fingerprint is cross-published to all three locations above before activation.

## Verifying packages manually

### `.rpm`

```sh
sudo rpm --import https://github.com/elleryfamilia/zerminal/releases/latest/download/zerminal-rpm-signing-key.asc
rpm -K zerminal-*.x86_64.rpm     # exits non-zero on signature failure
```

`dnf install` enforces signature verification when invoked with `--setopt=localpkg_gpgcheck=1`, which the install script does automatically.

### `.deb`

The `.deb` is signed with the same GPG key. Standalone `dpkg -i` does *not* verify the embedded signature by default; verify manually with `dpkg-sig`:

```sh
sudo apt-get install -y dpkg-sig
dpkg-sig --verify zerminal_*_amd64.deb
```

A real signed APT repository is on the roadmap; until then, trust on a one-shot install comes from HTTPS to GitHub plus optional manual `dpkg-sig` verification.

## `curl | sh` and the install script

The Linux installer at `https://github.com/elleryfamilia/zerminal/releases/latest/download/install.sh` is a documented `curl | sh` flow. If you'd rather audit before running:

```sh
curl -fsSLO https://github.com/elleryfamilia/zerminal/releases/latest/download/install.sh
less install.sh
sh install.sh
```

The script prints what it will do before each privileged action and respects `ZERMINAL_NONINTERACTIVE=1` for CI.

## Supported platforms

| Platform | Architecture | Status |
| --- | --- | --- |
| macOS | Apple Silicon (`aarch64`) | Signed, notarized |
| Linux | `x86_64` | GPG-signed `.deb`, `.rpm`, `PKGBUILD` |

Other targets are not built. Build from source for unsupported platforms.
