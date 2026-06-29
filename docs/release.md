# Release process

Tagged pushes (`v*`) trigger `.forgejo/workflows/release.yml`. The workflow:

1. Builds three targets in parallel (`darwin-aarch64`, `darwin-x86_64`, `linux-x86_64`).
2. On the two macOS legs, signs the binary with the Raster & State Developer ID
   and submits it to Apple's notary service. Both happen in CI; no local laptop
   needed.
3. Packages each binary as a versioned `.tar.gz` and uploads to the Forgejo
   release.
4. Renders `fj.rb` (Homebrew formula) with the freshly-computed SHA256s and
   attaches it as a release asset for the tap workflow to pick up.

## Runner requirements

The release matrix needs self-hosted runners with these labels, **online at tag
time** (a missing label leaves the job in `waiting` indefinitely, that's what
`make release-preflight` checks):

- **`macos-15`**: the two darwin legs (build + sign + notarize). An
  Apple-silicon Mac; the x86_64 binary cross-compiles there.
- **`docker`**: the linux build and the `publish` job.

Two environment gotchas that have broken releases:

- **macOS runners run action scripts under bash 3.2.** Every `shell: bash` /
  composite-action step uses the runner's `bash`, and macOS ships 3.2, where
  `"${arr[@]}"` on an empty array under `set -u` aborts ("unbound variable") and
  `mapfile` doesn't exist. Either keep action scripts 3.2-safe, or `brew install
  bash` on the runner and put it ahead of `/bin/bash` on the runner process's
  PATH (then restart the runner).
- **The `publish` job runs in bare `alpine`,** which has no `node`. The JS
  actions it calls (`download-artifact`, `forgejo-release`) need `nodejs` in the
  `apk add` line.

## Cutting a release

```sh
# 1. Update CHANGELOG.md and bump Cargo.toml + Cargo.lock to the new version,
#    then commit. The Cargo.toml version MUST equal the tag minus its `v`: the
#    Homebrew formula's `test do` asserts `fj --version` matches the tag, so a
#    mismatch fails the release at the publish step.

# 2. Preflight: version-vs-tag, an online runner per required label, and that the
#    signing/notarization secrets are set. Seconds, vs one ~8-minute signed build
#    per discovery.
make release-preflight TAG=v0.1.4

# 3. Tag and push, only once preflight is green.
git tag -a v0.1.4 -m "v0.1.4"
git push origin v0.1.4
```

Then watch the workflow at `https://rasterhub.com/rasterstate/fj/actions`.

## Required Forgejo Actions secrets

Set these on the repo under **Settings → Actions → Secrets** before the next
tagged release. They are all org/repo-scoped, not workflow-scoped.

### Code signing

| Secret | What | How to generate |
| ------ | ---- | --------------- |
| `MACOS_CERT_P12_BASE64` | Developer ID Application certificate exported as `.p12`, then base64-encoded. | Keychain Access → My Certificates → right-click your Developer ID Application certificate → **Export…** → `.p12` with a password. Then `base64 -i cert.p12 \| pbcopy`. |
| `MACOS_CERT_PASSWORD` | The password you set when exporting the `.p12`. | (Set during the export step above.) |
| `MACOS_KEYCHAIN_PASSWORD` | Any random string. Used to lock the throwaway keychain the runner creates. | `openssl rand -hex 32` |

> **The `.p12` must be a full *identity*: the certificate AND its private key.**
> Export from Keychain Access's **My Certificates** view (not "Certificates"),
> selecting the identity row, the cert with a disclosure triangle (▶) that
> reveals a nested private key. If the `.p12` (Personal Information Exchange)
> format is greyed out in the export dialog, you've selected a cert with no key;
> pick the identity, or the private key itself, instead. A key-only (or
> cert-only) `.p12` imports without forming a signing identity, and the release
> dies at `setup signing keychain` with `SecItemCopyMatching: The specified item
> could not be found`. Verify before you base64-encode:
>
> ```sh
> openssl pkcs12 -in cert.p12 -passin pass:PW -nokeys         | grep -c CERTIFICATE    # >= 1
> openssl pkcs12 -in cert.p12 -passin pass:PW -nocerts -nodes | grep -c 'PRIVATE KEY'  # 1
> ```

### Notarization

Notarization uses an **App Store Connect API key** (not an app-specific
password), because API keys work in CI without interactive auth.

| Secret | What | How to generate |
| ------ | ---- | --------------- |
| `APPLE_API_KEY_BASE64` | The `.p8` private key from App Store Connect, base64-encoded. | [appstoreconnect.apple.com → Users and Access → Integrations → App Store Connect API](https://appstoreconnect.apple.com/access/integrations/api), then **Generate API Key**. Download the `.p8` (one-shot, save it). `base64 -i AuthKey_*.p8 \| pbcopy`. |
| `APPLE_API_KEY_ID` | The 10-character Key ID shown next to the key. | Copy from the same page. |
| `APPLE_API_ISSUER` | Your Issuer ID (UUID format). | Top of the same page. |

The API key needs the **Developer** role (the minimum that grants notarytool
access). It does not need Admin.

## Local fallback (without CI)

If a tagged release goes out without CI for some reason, `scripts/sign.sh
--notarize` chains signing then notarization using your local keychain. See
the script header for keychain profile setup. This is the path the v0.1.2 /
v0.1.3 releases took.

## Verifying a release post-publish

```sh
# Tarball integrity:
curl -fsSL https://rasterhub.com/rasterstate/fj/releases/download/v0.1.4/SHA256SUMS | shasum -a 256 -c

# Signature of the darwin binary (Developer ID chain + secure timestamp):
tar xzf fj-v0.1.4-darwin-aarch64.tar.gz
codesign --verify --strict --verbose=2 fj-v0.1.4-darwin-aarch64/fj
codesign -dvv fj-v0.1.4-darwin-aarch64/fj 2>&1 | grep -E 'Authority|TeamIdentifier|Timestamp'
# Expected: Authority=Developer ID Application: Raster & State LLC (62Y3FRM8PD),
#           chained to Apple Root CA, a Timestamp, and TeamIdentifier=62Y3FRM8PD.
```

> **Don't use `spctl --assess` on the binary.** `fj` is a bare CLI executable,
> and `spctl --assess --type execute` reports `rejected (the code is valid but
> does not seem to be an app)` for it *even when it's correctly signed and
> notarized*, `spctl` only meaningfully assesses app bundles / installers.
> Notarization is confirmed by the release job's `notarytool submit --wait`
> returning **Accepted**; a standalone executable can't be stapled, so there is
> nothing to check offline. The signals above (`codesign --verify` passing, the
> Developer ID authority chain, and a secure timestamp) are what to confirm.

## Homebrew tap

The release workflow attaches a rendered `fj.rb` as a release asset.

The canonical tap is `rasterhub.com/rasterstate/homebrew-tap` (self-hosted);
`github.com/rasterandstate/homebrew-tap` is a downstream push-mirror, so the
install command does not depend on GitHub:

```sh
brew tap rasterandstate/tap https://rasterhub.com/rasterstate/homebrew-tap
brew install fj
```

The tap is updated by hand after each release: copy the rendered `fj.rb` asset
onto the canonical tap and push.

```sh
git clone https://rasterhub.com/rasterstate/homebrew-tap.git
curl -fsSL https://rasterhub.com/rasterstate/fj/releases/download/vX.Y.Z/fj.rb \
  -o homebrew-tap/Formula/fj.rb
cd homebrew-tap && git commit -am "fj X.Y.Z" && git push   # push-mirror carries it to GitHub
```

(Open issue: automate the bump via a tap-side Forgejo Action that polls
`/releases/latest` and commits the new formula.)
