# Security

## Reporting a vulnerability

Email `ellery@familia.me`. PGP key below. Please don't open public issues for security-sensitive reports.

## Release signing key

The Linux tarball on each GitHub release is shipped with a detached GPG signature (`zerminal-linux-x86_64.tar.gz.asc`), signed with this key:

- **UserID**: `Zerminal <ellery@familia.me>`
- **Fingerprint**: `8C04 4786 1386 07EE BFB4  E04B 3762 B681 02EC 4A8A`

The same fingerprint is published in three places so a pipeline compromise that swaps one will be visibly inconsistent with the others:

1. This file (`SECURITY.md`) in `elleryfamilia/zerminal`.
2. The `README.md` in `elleryfamilia/homebrew-zerminal` (the Homebrew tap).
3. `keys.openpgp.org` (search for `ellery@familia.me`).

**If these three disagree, do not install.** Open an issue or email instead.

The Linux installer at `script/install.sh` pins this fingerprint in source and refuses to install if the released public key doesn't contain it. macOS builds are signed and notarized by Apple; the GPG fingerprint above does not apply to the DMG.

### Rotation policy

The signing key is rotated on compromise or on an annual schedule. Rotation is announced at least 30 days in advance via GitHub release notes; the new fingerprint is cross-published to all three locations above before activation.

## Verifying the Linux tarball manually

The installer does this automatically; these steps are for users who want to audit independently.

```sh
BASE="https://github.com/elleryfamilia/zerminal/releases/latest/download"
curl -fsSLO "${BASE}/zerminal-linux-x86_64.tar.gz"
curl -fsSLO "${BASE}/zerminal-linux-x86_64.tar.gz.asc"
curl -fsSLO "${BASE}/zerminal-signing-key.asc"

# Import key into an isolated keyring, then check fingerprint:
export GNUPGHOME="$(mktemp -d)"
gpg --batch --import zerminal-signing-key.asc
gpg --batch --with-colons --fingerprint | awk -F: '/^fpr:/ { print $10 }'
# Must contain: 8C044786138607EEBFB4E04B3762B68102EC4A8A

# Verify the tarball:
gpg --batch --trusted-key 8C044786138607EEBFB4E04B3762B68102EC4A8A \
    --verify zerminal-linux-x86_64.tar.gz.asc zerminal-linux-x86_64.tar.gz
```

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
| Linux | `x86_64` | GPG-signed tarball (`.tar.gz` + `.asc`) |

Other targets are not built. Build from source for unsupported platforms.
