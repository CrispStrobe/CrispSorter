# Desktop release signing

These helpers operate on the final, patched release output. They do not create,
replace, revoke, or upload certificates. An unsigned draft is a build artifact,
not a signed release. Public release automation must independently require the
signed and verified artifacts for both desktop platforms, and remove obsolete
unsigned or unpatched assets before publication.

## macOS Developer ID

A Mac runner needs Xcode command-line tools, network access to Apple's
notarization service, an installed **Developer ID Application** identity and
its private key, and an App Store Connect API key allowed to use notarytool.
The Apple Distribution identity used by App Store builds is a different
certificate and must not be replaced or revoked. An account certificate without
its private key cannot sign. Use an existing Developer ID identity only if its
private key can be recovered; obtaining an identity is outside these scripts.

After adding and patching native libraries, supply these environment variables:

- `DEVELOPER_ID_SHA1`: the 40-hex SHA-1 identity fingerprint reported by
  `security find-identity -v -p codesigning`.
- `APPLE_TEAM_ID`: the ten-character team ID.
- `APPLE_NOTARY_KEY_PATH`, `APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER_ID`:
  the path to the API `.p8` file and its key and issuer IDs. Keep the key out
  of the repository, protect it with restrictive permissions, and remove
  temporary keychain and key files after the job.

Run `scripts/sign_macos_release.sh path/to/CrispSorter.app path/to/output.dmg`.
It signs nested Mach-O code and the app with the hardened runtime and secure
timestamp, submits a ZIP of the app, requires an Accepted status, staples and
validates the app ticket, and assesses it with Gatekeeper. It then builds and
signs a new DMG, submits it separately, and staples and validates the DMG.
Create the downloadable app tarball **after** this succeeds, preserving the
stapled ticket. Do not modify the app or image after signing. If submission or
validation fails, the job fails; a previously existing DMG is not proof of
success and must not be published. Do not label a draft as signed or notarized
just because credentials were present.

The Developer ID app does not reuse the App Store sandbox entitlements: those
include App Store provisioning identity values and a sandbox that is not
appropriate for the current direct-download build. Before publishing, test the
signed, quarantined download on a clean Mac, including library loading,
sidecars, OCR, and filesystem access. Notarization alone does not prove the
application works.

Apple's [notarization requirements](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)
include hardened runtime and secure timestamps.

## Windows portable ZIP

Install a legitimate code-signing certificate and accessible private key in
the runner's `CurrentUser\My` certificate store using the provider's
instructions. Hardware-backed certificates may require the vendor's software
and an appropriately configured runner. The script never generates a
certificate. Set `WINDOWS_SIGNING_THUMBPRINT` to its 40-hex SHA-1 thumbprint
and `WINDOWS_TIMESTAMP_URL` to the certificate provider's HTTPS RFC 3161
endpoint. Run:

```powershell
./scripts/sign_windows_release.ps1 -Stage portable-stage
```

Run it after the staged `crispsorter.exe` is in place and before assembling
the ZIP. It signs that executable with SHA-256 and an RFC 3161 timestamp, then
requires SignTool Authenticode verification with timestamp and the expected
certificate. It rejects a self-signed leaf and treats SignTool warnings as
failures. The runner's trust store must be the standard, unmodified Windows
trust store; trusting an arbitrary locally installed root does not establish
public trust. The helper signs the main executable only; it does not claim that
third-party DLLs or sidecars have been signed by CrispSorter.

After packaging, extract the actual ZIP into a clean temporary directory and
run `./scripts/sign_windows_release.ps1 -Stage <extracted-directory> -VerifyOnly`
with the expected thumbprint. Test the downloaded ZIP on a clean Windows host.
Microsoft documents [SignTool verification](https://learn.microsoft.com/en-us/windows/win32/seccrypto/using-signtool-to-verify-a-file-signature)
and [timestamp and digest options](https://learn.microsoft.com/en-us/dotnet/framework/tools/signtool-exe).

## Current readiness

No Developer ID private key export or trusted Windows signing certificate was
available during implementation, so no production signature, notarization,
or clean-machine verification has been performed. Successful local syntax and
negative-path checks do not demonstrate that these platform workflows work
with real credentials. Run a draft build on each target platform with the real
identities before enabling publication.

## GitHub Actions credentials and gate

The direct-download workflow expects these repository secrets when credentials
are available:

- macOS: `MACOS_DEVELOPER_ID_P12_BASE64`, `MACOS_DEVELOPER_ID_P12_PASSWORD`,
  `MACOS_DEVELOPER_ID_SHA1`, plus the existing `APPSTORE_API_KEY_P8`,
  `APPSTORE_API_KEY_ID`, and `APPSTORE_API_ISSUER_ID` notarization credentials.
  The Developer ID P12 is separate from `DIST_CERT_P12_BASE64`; do not replace
  or revoke the retained App Store Distribution identity.
- Windows: `WINDOWS_SIGNING_PFX_BASE64`, `WINDOWS_SIGNING_PFX_PASSWORD`,
  `WINDOWS_SIGNING_THUMBPRINT`, and `WINDOWS_TIMESTAMP_URL`. The PFX import is
  intended for ephemeral hosted runners; hardware-backed certificate providers
  may need a dedicated runner and a different import mechanism.

If an identity is entirely absent, non-publishing runs create explicitly
`-unsigned` desktop drafts. Partially configured identity credentials fail the
job. Tag and publish runs require both identities and final-asset verification.
The workflow refuses to overwrite a public release, verifies the existing tag or draft target
commit, and checks the downloaded draft assets against the source-bound
receipts before publication. Each signed macOS app is assessed from both its
archive and mounted DMG; the Windows executable is verified from the extracted
ZIP. The `certificate_audit` selector also examines the retained P12 inside an
ephemeral macOS keychain and reports code-signing identities without changing
the Apple account or the retained secret.
