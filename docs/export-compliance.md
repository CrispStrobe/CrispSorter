# Export compliance — encryption in CrispSorter

Record of what cryptography CrispSorter actually ships, and the basis for the
export-compliance answers given to Apple. Written so that a request from App
Store Connect ("send us your export compliance documentation") is a two-minute
reply rather than a re-derivation.

**Not legal advice.** This is an engineering inventory plus the reading of the
regulations it was decided on. The exporter of record signs the declaration.

- **Decision (2026-07-31):** `usesNonExemptEncryption = false`, on the
  published-source basis below.
- **Where it is asserted:** `ASC_USES_NON_EXEMPT_ENCRYPTION: "false"` on both
  TestFlight steps in `.github/workflows/release.yml`, and
  `ITSAppUsesNonExemptEncryption = false` written into the iOS `Info.plist` by
  the same workflow. `scripts/testflight_distribute.py` refuses to run if the
  env var is unset — the value is a declaration, not a script default.
- **Answer to Apple's "what type of encryption algorithms does your app
  implement?":** *"Standard encryption algorithms instead of, or in addition
  to, using or accessing the encryption within Apple's operating system."*
  This is the truthful option — see the inventory. It is not the same question
  as whether the encryption is *exempt*.

## What actually ships

Verify with:

```sh
grep -nE '^(openssl|reqwest|keyring|md-5|sha1|sha2) ' src-tauri/Cargo.toml
grep -A2 '^name = "openssl-src"$\|^name = "rustls"$\|^name = "aes"$' Cargo.lock
cargo tree -p crispsorter --features desktop -i aes
```

| What | Provenance | Reaches App Store builds? |
|---|---|---|
| **OpenSSL 3.6.3**, statically linked (`openssl-src 300.6.1+3.6.3`) | `openssl = { version = "0.10", features = ["vendored"] }`, `src-tauri/Cargo.toml:68` — an **unconditional** `[dependencies]` entry, not target-gated. Vendored so Android/iOS cross-builds don't need a system libssl. | Yes, every platform |
| **rustls 0.23.40** | TLS backend on the mobile path | Yes |
| **AES-256 (AESV3 / PDF 2.0 V5)** and **RC4-128 (V2)** PDF encryption | `pdf_ops::encrypt_pdf`, via `lopdf` 0.38 → `aes` 0.8.4 (RustCrypto). File encryption key from `getrandom`. **Not feature-gated** — ships in every build, exposed in the GUI and as `crispsorter pdf encrypt`. | Yes |
| **AES-256 / RC4 PDF *decryption*** | `zpdf` (our fork), behind `pdf-zpdf` | macOS yes (the MAS build passes `--features desktop,pdf-zpdf`); iOS no |
| `keyring` 3 with `crypto-rust` | OS keychain access; RustCrypto only for the Linux secret-service backend | Yes (uses `apple-native` on Apple platforms) |
| `md-5`, `sha1`, `sha2` | Content addressing / integrity, not confidentiality | Yes |

Consequences worth being explicit about, because both were assumed wrong at
some point:

- **Removing PDF encryption would not change the declaration.** The vendored
  OpenSSL alone puts the app in "standard algorithms, not Apple's". Deleting
  `encrypt_pdf` would cost a shipped, independently verified feature and change
  no answer.
- **The cloud-drive integrations contribute nothing here.** Internxt and Filen
  are reached by invoking a user-installed Python CLI as a **subprocess**
  (`src-tauri/src/drives/{internxt,filen}.rs`); there is no Internxt/Filen
  crate, and the only bundled resources are `bin/*` (CrispEmbed + ggml
  dylibs). Their end-to-end encryption runs in a process we don't ship. Google
  Drive and OneDrive are plain HTTPS through `reqwest`.

## Basis for "exempt"

CrispSorter's own source is published in full under **AGPL-3.0-or-later** at
<https://github.com/CrispStrobe/CrispSorter> (`[workspace.package] license`,
`Cargo.toml`).

1. Publicly available encryption **source** code is not subject to the EAR, and
   **object code compiled from published source** is likewise not subject when
   the corresponding source is publicly available —
   [BIS: encryption items not subject to the EAR](https://www.bis.gov/learn-support/encryption-controls/encryption-items-not-subject-to-ear),
   [15 CFR 734.17](https://www.bis.gov/ear/title-15/subtitle-b/chapter-vii/subchapter-c/part-734/ss-73417-export-encryption-source-code),
   [15 CFR 742.15](https://www.ecfr.gov/current/title-15/subtitle-B/chapter-VII/subchapter-C/part-742/section-742.15).
2. The **email-notification requirement** that used to be the price of that
   route (§742.15(b) notification to BIS/NSA) was **eliminated** by BIS's final
   rule — [Baker McKenzie summary](https://sanctionsnews.bakermckenzie.com/bis-updates-reporting-requirements-relating-to-mass-market-encryption-items-and-publicly-available-software-and-also-updates-certain-classifications/),
   [Federal Register](https://www.federalregister.gov/documents/2011/01/07/2010-32803/publicly-available-mass-market-encryption-software-and-other-specified-publicly-available-encryption).
   So there is no annual self-classification report for this app.
3. This is a **different route** from ECCN **5D992.c** self-classification,
   which is what a closed-source app shipping the same AES-256 would need. Do
   not conflate them: if CrispSorter's source ever stops being published, the
   answer here changes.

Background reading:
[EFF on published encryption source code](https://www.eff.org/deeplinks/2019/08/us-export-controls-and-published-encryption-source-code-explained),
[Linux Foundation guide](https://www.linuxfoundation.org/resources/publications/understanding-us-export-controls-with-open-source-projects).

### Caveats deliberately left visible

- The EAR position does not automatically settle **Apple's** field. Apple's own
  guidance is that `ITSAppUsesNonExemptEncryption` is `false` when the app uses
  only encryption exempt from export-compliance *documentation*, and in some
  paths they ask for documentation and issue an
  [`ITSEncryptionExportComplianceCode`](https://developer.apple.com/documentation/bundleresources/information-property-list/itsencryptionexportcompliancecode)
  ([export compliance overview](https://www.developer.apple.com/help/app-store-connect/manage-app-information/overview-of-export-compliance)).
  If Apple issues a code, add it to the plist; this document is the supporting
  material for that request.
- An item is **not** publicly available merely because it *incorporates or
  calls* open source. The basis above is that *our* source is published — not
  that OpenSSL's is. Bundling OpenSSL neither helps nor hurts that claim.
- If a future build ever gates the source (a closed fork, a proprietary
  bundle), re-derive from scratch; 5D992.c self-classification would then
  apply and the CI value must flip.

## If you would rather narrow the surface

Not required by the decision above, recorded because it came up: gating
*encryption* out of App Store builds while keeping decryption would fit Apple's
exemption for functionality limited to decryption. It would **not** remove
OpenSSL from the binary, so it does not by itself change the "standard
algorithms" answer. No such gate exists today, by choice.
