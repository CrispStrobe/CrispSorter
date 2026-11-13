# Historical Windows package replay

The one-off `windows_validate_b739` selector, retained-package replay script,
pins and fixture copies have been removed. Use ordinary Windows builds and
current package verification for new work.

The original-source 0.15.1 retry associated with `b739a3c0` produced an unsigned
portable ZIP. Version, doctor and worker controls passed, but the release's
scanned-PDF OCR check failed without retaining the rejection text. A later
retained-package replay observed missing models, explicitly provisioned the
permissive DBNet/TrOCR pair, and passed the expected hOCR fixture on retry.

That later fixture result does not explain or relabel the original release
failure, establish Authenticode signing, or qualify GUI/device behavior and
general OCR quality. Original observations remain in Git history and retained
external artifacts; the replay commands are no longer supported.
