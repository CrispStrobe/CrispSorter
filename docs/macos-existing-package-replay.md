# Historical macOS package replay

The one-off `macos_validate_b739` selector, replay/reevaluation scripts, private
observation schema, pins and fixture copies have been removed. Use ordinary
macOS builds and current package verification for new work.

The original 0.15.1 packages associated with `b739a3c0` were unsigned and their
initial release verification failed during scanned-PDF OCR. The original log
omitted the rejection, so its cause remains unestablished. Later retained-package
checks observed missing models; after explicit provisioning of the permissive
DBNet/TrOCR pair, both package OCR controls produced the expected hOCR.

An earlier replay also failed a doctor-schema assumption and DMG detach. Later
checks corrected the doctor interpretation and observed successful normal
detach; they did not explain or relabel the earlier failure. These are historical
CLI fixture results, not signing/notarization, GUI, device, general OCR quality
or publication qualification. Original observations remain in Git history and
retained external artifacts; the replay commands are no longer supported.
