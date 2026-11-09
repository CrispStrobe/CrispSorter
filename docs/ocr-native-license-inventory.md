# Native build license inventory

The first complete OCR mobile builds at `646dbc48` generated 19 frontend license
entries and zero backend entries: the optional `cargo-license` executable was
absent, and the existing generator continued after a warning. The successful
iOS build receipt records that limitation. Its native runtime notices are a
separate, explicitly verified set of resources.

The paired desktop and OCR mobile workflows now use Cargo's existing metadata
command with `--locked`, the exact target triple, no default features, and the
same explicit feature set as the app build. This uses dependency versions from
the committed `Cargo.lock` without installing another scanner. The metadata
snapshot records the source commit, lock and manifest hashes, Cargo version,
target, features, and the complete metadata digest.

The inventory starts at the actual `src-tauri/Cargo.toml` application package and
follows its resolved normal and build dependencies. Test-only and disconnected
workspace/target packages are excluded. CrispEmbed, its bindings, ORT, and its
bindings must all be present. Declared SPDX expressions and license alternatives
are retained exactly; an undeclared license remains `Unknown`.

Native builds pass the snapshot through `LICENSES_CARGO_METADATA`,
`LICENSES_CARGO_TARGET`, and `LICENSES_CARGO_FEATURES`, with `LICENSES_REQUIRE=1`.
The prebuild generator verifies its source/lock/target identity before using it.
A missing or empty backend inventory fails before compilation. After packaging,
the workflow verifies every generated backend entry against the same resolved
metadata and retains both files as evidence. Standalone browser fixture builds
keep the existing permissive behavior unless these variables are explicitly set.

The generated source receipt accepts both the earlier Frontend/Backend schema
and the explicit Frontend/Backend/Native schema. It verifies the count of each
source and the total; only `static/licenses.json` may be a changed tracked file.
This preserves historical receipts without allowing changed application code or
dependency locks to be described as generated inventory.

Five focused inventory controls cover graph traversal, incomplete native
bindings, source/lock/target changes, metadata tampering, and missing or modified
packaged entries. Separate Python and Node controls verify the Native count
schema. Actual dependency inventory counts remain a hosted build result, not a
claim derived from these structural fixtures. This is a dependency metadata
inventory; it does not replace the separately packaged native license texts or
determine licensing obligations beyond the upstream declarations.
