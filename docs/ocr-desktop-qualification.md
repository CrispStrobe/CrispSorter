# Desktop OCR execution qualification

The paired native workflow builds the exact pinned CrispEmbed revision, executes
the application’s Rust/native contracts, and then **links the Linux desktop
executable** with `desktop,ocr-workspace` and a committed dependency lockfile.
All paired Cargo commands use `--locked`. A `cargo check` result does not qualify
that final link or application startup. This profile includes the desktop UI,
native cloud-drive modules and mistralrs; it does not enable optional audio
runtimes, helper processes or translation models.

`scripts/desktop_ocr_startup_smoke.py` runs against the resulting binary in a
dedicated hosted Xvfb/DBus session. It requires a clean application/native source
checkout, records both immutable commits, hashes the executable and native
library, checks dynamic relocations with `ldd -r`, and uses `LD_BIND_NOW=1` for
execution. The timestamped generated license inventory is the only permitted
tracked source change; its output is included in the frontend manifest.

The first phase invokes `--help`, `--version` and JSON `version` without a display.
It checks the OCR command and exact package/target version. A deliberately
unsupported argument must fail with exit code 2. The GUI phase then launches the
same executable with new private XDG directories and serves the actual production
frontend build on the configured loopback URL. It requires a visible window owned
by that exact PID, the native startup message, successful requests for production
HTML and both JavaScript entry points, and a further ten seconds of process
liveness. An occupied frontend port is an error; another listener is never stopped.

The receipt contains the executable identity, resolved and actually mapped
CrispEmbed path/hash, production asset manifest, CLI outputs, GUI log, elapsed
time, RSS/PSS samples, process RSS high-water observation, child process names,
window ownership and a PNG capture. Only processes started by the helper are
terminated. Failure receipts and available logs are uploaded as well as successes.

These observations establish binary execution and bounded GUI startup only.
Fetching JavaScript does not prove that every application interaction works, and
the screenshot requires visual inspection. This gate does not download OCR
models, recognize documents, qualify a physical desktop, test hardware cameras,
or replace the native inference and export-reader gates. The helper’s eight local
rejection controls use synthetic evidence and do not constitute an app launch.

## Hosted invocation

The workflow installs GTK/WebKit, protobuf/SSL, Xvfb, DBus, xdotool, ImageMagick and
software Mesa rendering. It builds the production frontend and uses a two-job
Rust build with dependency caching. Cold full desktop code generation/linking
has a 120-minute job limit; the startup observation itself is bounded to 60 seconds.

After the paired library, frontend and desktop binary have been built:

```sh
export OCR_CRISPEMBED_REF=EXACT_40_CHARACTER_NATIVE_COMMIT
export LD_LIBRARY_PATH="$PWD/../CrispEmbed/build:$PWD/../CrispEmbed/build/ggml/src"
xvfb-run -a -s '-screen 0 1440x1000x24' dbus-run-session -- \
  python3 scripts/desktop_ocr_startup_smoke.py \
    --source . --native-source ../CrispEmbed \
    --binary target/debug/crispsorter \
    --native-library ../CrispEmbed/build/libcrispembed.so \
    --frontend build --output test-results/desktop-startup
```

Use a new output directory for each execution. This document defines the gate;
qualification status comes from its exact-source hosted receipt and inspected
screenshot, not from the existence of this script.
