# Linux package checks and licensed OCR tests

Normal Linux package verification runs the final DEB outside the build tree,
checks version/doctor with the dynamic-loader cache inhibited, exercises the
OCR-worker protocol, and recognizes the committed scanned PDF through
DBNet/TrOCR. A clean runner explicitly provisions only the permissive `dbnet-det`
(Apache-2.0) and `trocr-printed` (MIT) pair through the application's normal model
licensing and hash checks. A restrictive or changed model plan fails.

Tag/publication builds also run the real `ocr_task_presets_live` test for quick,
multilingual, tables and difficult presets. The test retains its fixture hash,
production model resolution and known-word assertions. It requires no Git origin,
package receipt or JSON report before running. Its optional diagnostics cannot
turn a passing or failing OCR test into a different result. The workflow does not
grant model-license consent; normal resolver consent remains required.

The user's Qwen2.5-VL-3B acceptance applies to the separately selected internal
non-commercial smoke. It is not general commercial permission or qualification
of all presets. A permissive-model package result is not a substitute for the
real licensed-preset test or for other publication requirements.

## Historical existing-package observation

The one-off `record_existing_linux_package.py` recorder, observation pins and
its tests have been removed. It summarized existing checks of the original
0.15.1 DEB associated with `b739a3c0`; it neither built nor executed that package.
The separately observed version/doctor/worker and permissive DBNet/TrOCR fixture
results remain historical. They do not reconstruct the original missing release
receipt or qualify licensed presets. Use current final-package checks and normal
test retries for new work; the retired invocation is available in Git history.
