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

## Cutting a release

```sh
# Update CHANGELOG.md, bump Cargo.toml + Cargo.lock, commit
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

# Signature + notarization of the darwin binary:
tar xzf fj-v0.1.4-darwin-aarch64.tar.gz
codesign --verify --strict --verbose=2 fj-v0.1.4-darwin-aarch64/fj
spctl --assess --type execute --verbose=2 fj-v0.1.4-darwin-aarch64/fj
# Expected: "accepted" "source=Notarized Developer ID"
```

## Homebrew tap

The release workflow attaches a rendered `fj.rb` as a release asset. The
homebrew tap repo at `rasterandstate/homebrew-tap` is currently updated by
hand. (Open issue: automate the bump via a tap-side workflow that polls
`/releases/latest` and PRs in the new formula.)
