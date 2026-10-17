# Signing a release

The release workflow signs what it can, and publishes exactly what it always
did when it cannot: every signing step looks for its credentials first and
prints a `::notice::` instead of failing. A fork, a test tag, or a repository
that has never set these secrets builds and uploads unsigned binaries with no
configuration at all.

Linux needs nothing here — the tarball carries no signature, and the
`SHA256SUMS` file the publish job writes is what the updater checks.

## Windows

| Secret | What it is |
|---|---|
| `WINDOWS_CERT_PFX_B64` | A `.pfx` export of the code-signing certificate, base64-encoded (`base64 -w0 cert.pfx`, or `[Convert]::ToBase64String([IO.File]::ReadAllBytes('cert.pfx'))`). |
| `WINDOWS_CERT_PASSWORD` | The password that protects that `.pfx`. |

An OV/EV Authenticode certificate from any public CA works; there is no
hard-coded publisher name to update. The workflow signs `editor.exe` *before*
WiX builds the MSI (so the installer embeds the signed executable, not an
unsigned copy) and then signs each MSI, timestamping both against DigiCert's
RFC 3161 service so the signatures outlive the certificate.

EV certificates on hardware tokens cannot be exported as a `.pfx`; cloud
signing (Azure Trusted Signing, DigiCert KeyLocker) would need a different
step. Nothing here pretends to cover that.

## macOS

| Secret | What it is |
|---|---|
| `MACOS_CERT_P12_B64` | A `.p12` export of the *Developer ID Application* certificate and its private key, base64-encoded. |
| `MACOS_CERT_PASSWORD` | The password that protects that `.p12`. |
| `MACOS_SIGN_IDENTITY` | The identity `codesign -s` should use, e.g. `Developer ID Application: Your Name (TEAMID1234)`. |
| `APPLE_ID` | The Apple ID that submits the build for notarization. |
| `APPLE_TEAM_ID` | The ten-character team identifier that Apple ID belongs to. |
| `APPLE_APP_PASSWORD` | An app-specific password for that Apple ID (not the account password), from appleid.apple.com. |

With the certificate alone the bundle is signed but not notarized: it opens on
machines that trust the certificate, and Gatekeeper still warns elsewhere.
With the Apple ID secrets too, the workflow submits the signed bundle with
`notarytool --wait`, staples the ticket, validates it, and runs
`spctl --assess` — the same judgement Gatekeeper makes — before zipping the
artifact users download.

Signing requires the hardened runtime (`--options runtime`), which is not
optional for notarization. It is why the bundle is signed *after*
`packaging/macos/bundle.sh` fills the plist and copies the icon: signing a
bundle and then editing it invalidates the signature.

## Rotating or revoking

Replace the secret values in the repository settings; the next tag picks them
up. Nothing reads the certificate from the tree, and no workflow prints a
secret: the `.pfx`/`.p12` is only ever written to `$RUNNER_TEMP` and removed
(or, on macOS, imported into a throwaway keychain that is deleted at the end
of the step).

If a certificate leaks, revoke it with the CA first and set a fresh secret
second — rotating the secret alone leaves the old certificate valid for anyone
who already has it.
